//! Voz del sistema en macOS mediante el comando `say`. Sin una voz elegida en Ajustes usa la
//! del sistema (Ajustes del Sistema → Accesibilidad → Contenido leído).
//!
//! `say` no informa cuándo empieza cada palabra: las marcas se estiman repartiendo la duración
//! real de la voz (medida en el WAV) según la longitud de cada palabra y las pausas de la
//! puntuación. El análisis y la estimación se prueban en cualquier plataforma.

#![cfg_attr(not(target_os = "macos"), allow(dead_code))]

use super::{Mark, VoiceInfo};

/// Velocidad normal de `say`, en palabras por minuto.
const BASE_WPM: f32 = 180.0;

/// Voces instaladas según `say -v '?'`, p. ej.
/// `Paulina             es_MX    # Hola, me llamo Paulina y soy una voz mexicana.`
/// `Eddy (Español (México)) es_MX # ¡Hola! Me llamo Eddy.`
pub fn parse_voices(out: &str) -> Vec<VoiceInfo> {
    out.lines()
        .filter_map(|line| {
            let (left, _) = line.split_once('#')?;
            let (name, locale) = left.trim_end().rsplit_once(char::is_whitespace)?;
            let (name, locale) = (name.trim(), locale.trim());
            let looks_like_locale = locale.len() >= 4
                && locale.contains(['_', '-'])
                && locale.chars().next()?.is_ascii_lowercase();
            if name.is_empty() || !looks_like_locale {
                return None;
            }
            Some(VoiceInfo { id: name.to_owned(), name: name.to_owned(), locale: locale.replace('_', "-") })
        })
        .collect()
}

/// Primer idioma de `defaults read -g AppleLanguages`, p. ej. `es-MX`.
pub fn parse_system_language(out: &str) -> Option<String> {
    out.split(|c: char| matches!(c, '"' | '(' | ')' | ',') || c.is_whitespace())
        .find(|t| t.len() >= 2 && t.chars().next().is_some_and(|c| c.is_ascii_lowercase()))
        .map(str::to_owned)
}

/// Marcas por palabra estimadas entre `start_ms` y `end_ms`.
pub fn estimate_marks(text: &str, start_ms: u32, end_ms: u32) -> Vec<Mark> {
    let words: Vec<(usize, &str)> = text
        .split_whitespace()
        .map(|w| (w.as_ptr() as usize - text.as_ptr() as usize, w))
        .collect();
    // Peso ≈ caracteres que se pronuncian, más la pausa que deja la puntuación al final.
    let weight = |w: &str| {
        let chars = w.chars().filter(|c| c.is_alphanumeric()).count().max(1) as f64;
        let pause = match w.chars().last() {
            Some('.' | '?' | '!' | ';' | ':') => 6.0,
            Some(',') => 3.0,
            _ => 0.0,
        };
        chars + 1.5 + pause
    };
    let total: f64 = words.iter().map(|(_, w)| weight(w)).sum();
    let span = f64::from(end_ms.saturating_sub(start_ms));
    let mut acc = 0.0;
    words
        .iter()
        .map(|&(byte, w)| {
            let t_ms = start_ms + (span * acc / total) as u32;
            let dur_ms = (span * weight(w) / total) as u32;
            acc += weight(w);
            Mark { t_ms, dur_ms, byte }
        })
        .collect()
}

// Se compila en todas las plataformas para que el compilador revise el código de macOS,
// pero sólo se usa allí.
mod imp {
    use std::process::Command;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::OnceLock;

    use anyhow::{bail, Context, Result};

    use super::{estimate_marks, parse_system_language, parse_voices, BASE_WPM};
    use crate::lang::Lang;
    use crate::tts::{wav, Utterance, VoiceInfo};

    fn installed_voices() -> &'static [VoiceInfo] {
        static VOICES: OnceLock<Vec<VoiceInfo>> = OnceLock::new();
        VOICES.get_or_init(|| {
            Command::new("say")
                .args(["-v", "?"])
                .output()
                .map(|o| parse_voices(&String::from_utf8_lossy(&o.stdout)))
                .unwrap_or_default()
        })
    }

    fn system_language() -> Option<&'static str> {
        static LANG: OnceLock<Option<String>> = OnceLock::new();
        LANG.get_or_init(|| {
            let out = Command::new("defaults").args(["read", "-g", "AppleLanguages"]).output().ok()?;
            parse_system_language(&String::from_utf8_lossy(&out.stdout))
        })
        .as_deref()
    }

    /// Sin voz elegida: la del sistema si el texto está en el idioma del sistema; si no, la
    /// primera voz instalada de ese idioma.
    fn resolve_voice(voice: &str, lang: Lang) -> Option<String> {
        if !voice.trim().is_empty() {
            return Some(voice.trim().to_owned());
        }
        if system_language().is_none_or(|l| l.starts_with(lang.code())) {
            return None;
        }
        installed_voices()
            .iter()
            .find(|v| v.locale.to_lowercase().starts_with(lang.code()))
            .map(|v| v.id.clone())
    }

    pub async fn synthesize(voice: &str, lang: Lang, rate: f32, text: &str) -> Result<Utterance> {
        let voice = resolve_voice(voice, lang);
        let text = text.to_owned();
        let wpm = (BASE_WPM * rate.clamp(0.5, 3.0)).round() as u32;
        tokio::task::spawn_blocking(move || synth_blocking(voice.as_deref(), wpm, &text)).await?
    }

    fn synth_blocking(voice: Option<&str>, wpm: u32, text: &str) -> Result<Utterance> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let id = format!("{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed));
        let dir = std::env::temp_dir();
        let (txt, out) = (dir.join(format!("lector-{id}.txt")), dir.join(format!("lector-{id}.wav")));
        std::fs::write(&txt, text)?;
        let mut cmd = Command::new("say");
        if let Some(v) = voice {
            cmd.args(["-v", v]);
        }
        let status = cmd
            .args(["-r", &wpm.to_string(), "--file-format=WAVE", "--data-format=LEI16@22050", "-o"])
            .arg(&out)
            .arg("-f")
            .arg(&txt)
            .status()
            .context("no se pudo ejecutar `say`");
        let _ = std::fs::remove_file(&txt);
        if !status?.success() {
            bail!("`say` no pudo generar la voz");
        }
        let audio = std::fs::read(&out)?;
        let _ = std::fs::remove_file(&out);
        let (start, end) = wav::speech_bounds_ms(&audio).context("`say` devolvió audio vacío")?;
        Ok(Utterance { marks: estimate_marks(text, start, end), end_ms: Some(end), audio })
    }

    pub async fn voices() -> Result<Vec<VoiceInfo>> {
        Ok(tokio::task::spawn_blocking(|| installed_voices().to_vec()).await?)
    }
}

#[cfg(target_os = "macos")]
pub use imp::{synthesize, voices};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lista_de_voces() {
        let out = "Alex                en_US    # Most people recognize me by my voice.\n\
                   Paulina             es_MX    # Hola, me llamo Paulina y soy una voz mexicana.\n\
                   Eddy (Español (México)) es_MX    # ¡Hola! Me llamo Eddy.\n\
                   línea sin formato\n";
        let voices = parse_voices(out);
        let got: Vec<(&str, &str)> = voices.iter().map(|v| (v.id.as_str(), v.locale.as_str())).collect();
        assert_eq!(got, [("Alex", "en-US"), ("Paulina", "es-MX"), ("Eddy (Español (México))", "es-MX")]);
    }

    #[test]
    fn idioma_del_sistema() {
        assert_eq!(parse_system_language("(\n    \"es-MX\",\n    \"en-MX\"\n)\n").as_deref(), Some("es-MX"));
        assert_eq!(parse_system_language("(\n    en\n)").as_deref(), Some("en"));
        assert_eq!(parse_system_language(""), None);
    }

    #[test]
    fn marcas_estimadas() {
        let text = "Hola mundo, adiós.";
        let marks = estimate_marks(text, 100, 1100);
        let bytes: Vec<usize> = marks.iter().map(|m| m.byte).collect();
        assert_eq!(bytes, [0, 5, 12]);
        assert_eq!(marks[0].t_ms, 100);
        // En orden, dentro del rango, y la coma deja más espacio después de «mundo,».
        assert!(marks.windows(2).all(|w| w[0].t_ms < w[1].t_ms));
        assert!(marks[2].t_ms + marks[2].dur_ms <= 1100);
        assert!(marks[1].dur_ms > marks[0].dur_ms);
    }
}
