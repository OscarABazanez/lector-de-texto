pub mod claude;
pub mod math_speech;
pub mod prompts;
pub mod transcribe;

pub use claude::ClaudeClient;
pub use math_speech::MathSpeaker;

/// Lo que llega por el stream de una respuesta.
pub enum Delta<'a> {
    /// El modelo está razonando (modo preciso).
    Reasoning,
    Content(&'a str),
}

/// Error que indica que la operación se canceló a propósito (nueva selección, Detener…).
#[derive(Debug)]
pub struct Cancelled;

impl std::fmt::Display for Cancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("cancelado")
    }
}

impl std::error::Error for Cancelled {}
