//! Voces neuronales de Microsoft Edge ("Read aloud"), vía `msedge-tts`.
//!
//! Es un servicio online no oficial: si falla, el pipeline cambia a las voces del sistema.

use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use msedge_tts::tts::client::tokio_runtime::connect_async;
use msedge_tts::tts::client::SynthesizedAudio;
use msedge_tts::tts::SpeechConfig;
use tokio::sync::{mpsc, oneshot, Mutex, OnceCell};

use super::{Mark, Utterance, VoiceInfo};

const FORMAT: &str = "audio-24khz-48kbitrate-mono-mp3";
/// Una conexión inactiva más tiempo que esto se reabre antes de usarla.
const MAX_IDLE: Duration = Duration::from_secs(60);

enum Job {
    Synth {
        text: String,
        config: SpeechConfig,
        reply: oneshot::Sender<Result<SynthesizedAudio>>,
    },
    /// Abrir (o renovar) la conexión sin sintetizar nada.
    Warm,
}

#[derive(Default)]
pub struct EdgeTts {
    worker: OnceCell<mpsc::Sender<Job>>,
    voices: Mutex<Option<Vec<VoiceInfo>>>,
}

impl EdgeTts {
    pub async fn synthesize(&self, voice: &str, rate: f32, text: &str) -> Result<Utterance> {
        let config = SpeechConfig {
            voice_name: long_voice_name(voice),
            audio_format: FORMAT.into(),
            pitch: 0,
            rate: ((rate - 1.0) * 100.0).round() as i32,
            volume: 0,
        };
        let (tx, rx) = oneshot::channel();
        self.worker()
            .await
            .send(Job::Synth { text: xml_escape(text), config, reply: tx })
            .await
            .map_err(|_| anyhow!("Edge TTS se detuvo"))?;
        let audio = rx.await.map_err(|_| anyhow!("Edge TTS no respondió"))??;
        if audio.audio_bytes.is_empty() {
            bail!("Edge TTS devolvió audio vacío");
        }
        let words = audio
            .audio_metadata
            .iter()
            .filter(|m| m.metadata_type.as_deref() == Some("WordBoundary"))
            .filter_map(|m| Some(((m.offset / 10_000) as u32, (m.duration / 10_000) as u32, m.text.clone()?)));
        let marks = align(text, words);
        let end_ms = marks.iter().map(|m| m.t_ms + m.dur_ms).max().filter(|&t| t > 0);
        Ok(Utterance { audio: audio.audio_bytes, marks, end_ms })
    }

    async fn worker(&self) -> &mpsc::Sender<Job> {
        self.worker.get_or_init(|| async { spawn_worker() }).await
    }

    pub async fn prepare(&self) {
        let _ = self.worker().await.send(Job::Warm).await;
    }

    pub async fn voices(&self) -> Result<Vec<VoiceInfo>> {
        let mut cache = self.voices.lock().await;
        if let Some(v) = cache.as_ref() {
            return Ok(v.clone());
        }
        let list = tokio::time::timeout(
            Duration::from_secs(10),
            msedge_tts::voice::tokio_runtime::get_voices_list_async(),
        )
        .await
        .context("tiempo agotado pidiendo las voces de Edge")??;
        let mut out: Vec<VoiceInfo> = list
            .into_iter()
            .filter_map(|v| {
                let locale = v.locale?;
                if !(locale.starts_with("es-") || locale.starts_with("en-")) {
                    return None;
                }
                let id = v.short_name?;
                let name = v.friendly_name.unwrap_or_else(|| id.clone());
                Some(VoiceInfo { id, name, locale })
            })
            .collect();
        out.sort_by(|a, b| a.locale.cmp(&b.locale).then(a.id.cmp(&b.id)));
        *cache = Some(out.clone());
        Ok(out)
    }
}

/// Mantiene una conexión WebSocket abierta y la reabre si el servidor la cerró.
fn spawn_worker() -> mpsc::Sender<Job> {
    let (tx, mut rx) = mpsc::channel::<Job>(8);
    tokio::spawn(async move {
        let mut client = None;
        let mut last_used = Instant::now();
        while let Some(job) = rx.recv().await {
            if last_used.elapsed() > MAX_IDLE {
                client = None;
            }
            let (text, config, reply) = match job {
                Job::Synth { text, config, reply } => (text, config, reply),
                Job::Warm => {
                    if client.is_none() {
                        let t0 = Instant::now();
                        if let Ok(Ok(c)) = tokio::time::timeout(Duration::from_secs(6), connect_async()).await {
                            eprintln!("Edge TTS conectado en {} ms", t0.elapsed().as_millis());
                            client = Some(c);
                            last_used = Instant::now();
                        }
                    }
                    continue;
                }
            };
            let mut result = Err(anyhow!("Edge TTS: sin intentos"));
            for _ in 0..2 {
                if client.is_none() {
                    match tokio::time::timeout(Duration::from_secs(6), connect_async()).await {
                        Ok(Ok(c)) => client = Some(c),
                        Ok(Err(e)) => {
                            result = Err(anyhow!(e).context("no se pudo conectar con Edge TTS"));
                            continue;
                        }
                        Err(_) => {
                            result = Err(anyhow!("tiempo agotado conectando con Edge TTS"));
                            continue;
                        }
                    }
                }
                let Some(c) = client.as_mut() else { continue };
                match tokio::time::timeout(Duration::from_secs(12), c.synthesize(&text, &config)).await {
                    Ok(Ok(audio)) => {
                        result = Ok(audio);
                        break;
                    }
                    Ok(Err(e)) => {
                        client = None;
                        result = Err(anyhow!(e).context("Edge TTS falló"));
                    }
                    Err(_) => {
                        client = None;
                        result = Err(anyhow!("tiempo agotado sintetizando con Edge TTS"));
                    }
                }
            }
            last_used = Instant::now();
            let _ = reply.send(result);
        }
    });
    tx
}

/// `es-MX-DaliaNeural` → `Microsoft Server Speech Text to Speech Voice (es-MX, DaliaNeural)`.
pub fn long_voice_name(short: &str) -> String {
    if short.starts_with("Microsoft Server Speech") {
        return short.to_owned();
    }
    let parts: Vec<&str> = short.splitn(3, '-').collect();
    let [lang, region, name] = parts[..] else {
        return short.to_owned();
    };
    let (region, name) = match name.find('-') {
        Some(i) => (format!("{region}-{}", &name[..i]), &name[i + 1..]),
        None => (region.to_owned(), name),
    };
    format!("Microsoft Server Speech Text to Speech Voice ({lang}-{region}, {name})")
}

/// El crate inserta el texto tal cual dentro del SSML.
pub fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(c),
        }
    }
    out
}

/// Edge informa cada palabra por su texto; se ubica en orden dentro del guion.
pub fn align(script: &str, words: impl IntoIterator<Item = (u32, u32, String)>) -> Vec<Mark> {
    let lower = script.to_lowercase();
    let same_len = lower.len() == script.len();
    let mut marks = Vec::new();
    let mut cursor = 0;
    for (t_ms, dur_ms, word) in words {
        let word = word.trim();
        if word.is_empty() {
            continue;
        }
        let found = script[cursor..]
            .find(word)
            .map(|i| (cursor + i, word.len()))
            .or_else(|| {
                let needle = word.to_lowercase();
                same_len
                    .then(|| lower[cursor..].find(&needle).map(|i| (cursor + i, needle.len())))
                    .flatten()
            });
        match found {
            Some((pos, len)) => {
                marks.push(Mark { t_ms, dur_ms, byte: pos });
                cursor = (pos + len).min(script.len());
            }
            None => marks.push(Mark { t_ms, dur_ms, byte: cursor }),
        }
    }
    marks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nombre_largo() {
        assert_eq!(
            long_voice_name("es-MX-DaliaNeural"),
            "Microsoft Server Speech Text to Speech Voice (es-MX, DaliaNeural)"
        );
        assert_eq!(
            long_voice_name("zh-CN-liaoning-XiaobeiNeural"),
            "Microsoft Server Speech Text to Speech Voice (zh-CN-liaoning, XiaobeiNeural)"
        );
    }

    #[test]
    fn escape() {
        assert_eq!(xml_escape("a < b & c"), "a &lt; b &amp; c");
    }

    #[test]
    fn alinea_palabras() {
        let script = "Hola mundo, hola de nuevo.";
        let words = [(0, "Hola"), (300, "mundo"), (650, "HOLA"), (900, "xyz"), (1000, "nuevo")]
            .map(|(t, w)| (t, 100, w.to_string()));
        let marks = align(script, words);
        let bytes: Vec<usize> = marks.iter().map(|m| m.byte).collect();
        assert_eq!(bytes, vec![0, 5, 12, 16, 20]);
    }
}
