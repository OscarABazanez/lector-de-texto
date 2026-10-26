//! Motores de voz: las voces del sistema (WinRT en Windows, `say` en macOS) y Edge
//! (voces neuronales online).
//!
//! Todos devuelven el audio completo de un fragmento y las marcas de tiempo de
//! cada palabra (posición en bytes dentro del texto de entrada).

pub mod edge;
pub mod say;
pub mod wav;
#[cfg(windows)]
pub mod winrt;

use anyhow::Result;
use serde::Serialize;

use crate::config::Engine;
use crate::lang::Lang;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mark {
    pub t_ms: u32,
    pub dur_ms: u32,
    /// Posición (bytes) del inicio de la palabra en el texto de entrada.
    pub byte: usize,
}

pub struct Utterance {
    /// WAV (WinRT) o MP3 (Edge); el reproductor detecta el formato.
    pub audio: Vec<u8>,
    pub marks: Vec<Mark>,
    /// Milisegundo en que termina la voz; lo que sigue es silencio que se puede cortar.
    pub end_ms: Option<u32>,
}

#[derive(Clone, Debug, Serialize)]
pub struct VoiceInfo {
    pub id: String,
    pub name: String,
    pub locale: String,
}

#[derive(Default)]
pub struct Tts {
    edge: edge::EdgeTts,
}

impl Tts {
    pub async fn synthesize(
        &self,
        engine: Engine,
        voice: &str,
        lang: Lang,
        rate: f32,
        text: &str,
    ) -> Result<Utterance> {
        match engine {
            Engine::Edge => self.edge.synthesize(voice, rate, text).await,
            Engine::System => system_synth(voice, lang, rate, text).await,
        }
    }

    /// Prepara el motor por adelantado (p. ej. abrir la conexión con Edge).
    pub async fn prepare(&self, engine: Engine) {
        if engine == Engine::Edge {
            self.edge.prepare().await;
        }
    }

    pub async fn voices(&self, engine: Engine) -> Result<Vec<VoiceInfo>> {
        match engine {
            Engine::Edge => self.edge.voices().await,
            Engine::System => system_voices().await,
        }
    }
}

#[cfg(windows)]
async fn system_synth(voice: &str, lang: Lang, rate: f32, text: &str) -> Result<Utterance> {
    winrt::synthesize(voice.to_owned(), lang, rate, text.to_owned()).await
}

#[cfg(windows)]
async fn system_voices() -> Result<Vec<VoiceInfo>> {
    winrt::voices().await
}

#[cfg(target_os = "macos")]
async fn system_synth(voice: &str, lang: Lang, rate: f32, text: &str) -> Result<Utterance> {
    say::synthesize(voice, lang, rate, text).await
}

#[cfg(target_os = "macos")]
async fn system_voices() -> Result<Vec<VoiceInfo>> {
    say::voices().await
}

#[cfg(not(any(windows, target_os = "macos")))]
async fn system_synth(_: &str, _: Lang, _: f32, _: &str) -> Result<Utterance> {
    anyhow::bail!("no hay voces del sistema en esta plataforma; usa las voces de Edge")
}

#[cfg(not(any(windows, target_os = "macos")))]
async fn system_voices() -> Result<Vec<VoiceInfo>> {
    Ok(Vec::new())
}
