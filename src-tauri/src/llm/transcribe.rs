//! Etapa 1: imagen → Markdown + LaTeX (en streaming).

use anyhow::Result;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use super::claude::ClaudeClient;
use super::{prompts, Delta};

/// Cuerpo de la petición. Claude Haiku 5.5 rechaza `temperature` distinto del valor por defecto.
/// El razonamiento cuenta dentro de `max_tokens`, por eso el límite sube cuando está activo.
pub fn body(model: &str, png_b64: &str, precise: bool, adaptive: bool) -> Value {
    let (thinking, effort, max_tokens) = if precise {
        ("adaptive", "medium", 16000)
    } else if adaptive {
        ("adaptive", "low", 8000)
    } else {
        // Medido con Haiku 5.5: sin razonamiento empieza en 0.6–1.2 s y acertó los subíndices de prueba.
        ("disabled", "low", 4096)
    };
    json!({
        "model": model,
        "stream": true,
        "max_tokens": max_tokens,
        "thinking": {"type": thinking},
        "output_config": {"effort": effort},
        "messages": [{
            "role": "user",
            "content": [
                {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": png_b64}},
                {"type": "text", "text": prompts::TRANSCRIBE},
            ],
        }],
    })
}

pub async fn transcribe(
    llm: &ClaudeClient,
    png_b64: &str,
    precise: bool,
    adaptive: bool,
    cancel: &CancellationToken,
    on: impl FnMut(Delta),
) -> Result<()> {
    llm.stream_message(&body(llm.model(), png_b64, precise, adaptive), cancel, on).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cuerpo_de_peticion() {
        let fast = body("claude-haiku-5-5", "AAAA", false, false);
        assert!(fast.get("temperature").is_none());
        assert_eq!(fast["thinking"]["type"], "disabled");
        assert_eq!(fast["messages"][0]["content"][0]["type"], "image");
        let adaptive = body("claude-haiku-5-5", "AAAA", false, true);
        assert_eq!(adaptive["thinking"]["type"], "adaptive");
        assert_eq!(adaptive["output_config"]["effort"], "low");
        let precise = body("claude-haiku-5-5", "AAAA", true, false);
        assert_eq!(precise["thinking"]["type"], "adaptive");
        assert_eq!(precise["output_config"]["effort"], "medium");
    }
}
