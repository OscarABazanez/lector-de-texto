//! Diagnóstico: imprime las marcas por palabra que devuelve cada motor de voz.
//!
//!   cargo run --example tts_marks -- "Similitud del coseno."

use lector_lib::config::{Engine, TtsConfig};
use lector_lib::lang::Lang;
use lector_lib::tts::Tts;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let text = std::env::args().nth(1).unwrap_or_else(|| "Similitud del coseno.".into());
    let voices = TtsConfig::default();
    let tts = Tts::default();
    for engine in [Engine::System, Engine::Edge] {
        let utt = tts.synthesize(engine, voices.voice(engine, Lang::Es), Lang::Es, 1.0, &text).await?;
        println!("{engine:?}: {} bytes de audio, fin de voz {:?} ms", utt.audio.len(), utt.end_ms);
        for m in &utt.marks {
            let word: String = text[m.byte..].split_whitespace().next().unwrap_or("").into();
            println!("  {:>6} ms  dur {:>5} ms  {word}", m.t_ms, m.dur_ms);
        }
    }
    Ok(())
}
