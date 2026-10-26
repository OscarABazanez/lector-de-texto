//! Cliente mínimo de la API de Claude (Messages) por HTTP directo: Rust no tiene SDK oficial.

use std::time::Duration;

use anyhow::{bail, Context, Result};
use futures_util::StreamExt;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use super::{Cancelled, Delta};

const API: &str = "https://api.anthropic.com/v1";
const VERSION: &str = "2023-06-01";

pub struct ClaudeClient {
    http: reqwest::Client,
    model: String,
    api_key: Option<String>,
}

/// Los filtros de seguridad del modelo rechazaron la petición (`stop_reason: "refusal"`).
#[derive(Debug)]
pub struct Refused;

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Claude se negó a procesar esta imagen")
    }
}

impl std::error::Error for Refused {}

#[derive(Debug)]
pub struct MissingKey;

impl std::fmt::Display for MissingKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Falta la API key de Anthropic; configúrala en Ajustes")
    }
}

impl std::error::Error for MissingKey {}

/// Error HTTP de la API con un mensaje pensado para el usuario.
#[derive(Debug)]
pub struct ApiError {
    pub status: u16,
    pub message: String,
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.status {
            401 | 403 => f.write_str("La API key de Anthropic es inválida o fue revocada; configúrala en Ajustes"),
            404 => write!(f, "La API de Anthropic no encontró el modelo: {}", self.message),
            429 => f.write_str("Se alcanzó el límite de uso de la API de Anthropic"),
            529 => f.write_str("Claude está saturado en este momento"),
            s => write!(f, "Error {s} de la API de Anthropic: {}", self.message),
        }
    }
}

impl std::error::Error for ApiError {}

fn api_error(status: u16, body: &str) -> ApiError {
    let message = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| v["error"]["message"].as_str().map(str::to_owned))
        .unwrap_or_else(|| body.chars().take(200).collect());
    ApiError { status, message }
}

/// Lo que le importa al lector de cada evento SSE.
#[derive(Debug, PartialEq)]
pub enum SseEvent {
    Text(String),
    Thinking,
    Stop(Option<String>),
    Error(String),
    Other,
}

pub fn parse_event(data: &str) -> SseEvent {
    let Ok(v) = serde_json::from_str::<Value>(data) else {
        return SseEvent::Other;
    };
    match v["type"].as_str() {
        Some("content_block_start") => match v["content_block"]["type"].as_str() {
            Some("thinking" | "redacted_thinking") => SseEvent::Thinking,
            _ => SseEvent::Other,
        },
        Some("content_block_delta") => match v["delta"]["type"].as_str() {
            Some("text_delta") => SseEvent::Text(v["delta"]["text"].as_str().unwrap_or_default().to_owned()),
            Some("thinking_delta") => SseEvent::Thinking,
            _ => SseEvent::Other,
        },
        Some("message_delta") => SseEvent::Stop(v["delta"]["stop_reason"].as_str().map(str::to_owned)),
        Some("error") => SseEvent::Error(
            v["error"]["message"].as_str().unwrap_or("error desconocido").to_owned(),
        ),
        _ => SseEvent::Other,
    }
}

impl ClaudeClient {
    pub fn new(model: &str, api_key: Option<String>) -> Self {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .pool_idle_timeout(Duration::from_secs(300))
            .build()
            .expect("cliente HTTP");
        Self { http, model: model.to_owned(), api_key }
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    pub fn has_key(&self) -> bool {
        self.api_key.is_some()
    }

    fn request(&self, method: reqwest::Method, path: &str) -> Result<reqwest::RequestBuilder> {
        let key = self.api_key.as_deref().ok_or(MissingKey)?;
        Ok(self
            .http
            .request(method, format!("{API}/{path}"))
            .header("x-api-key", key)
            .header("anthropic-version", VERSION))
    }

    /// `POST /messages`, con un reintento ante saturación o límite de uso.
    async fn send(&self, body: &Value) -> Result<reqwest::Response> {
        // Diagnóstico: LECTOR_DEBUG_BODY=<carpeta> guarda el JSON de cada petición.
        if let Ok(dir) = std::env::var("LECTOR_DEBUG_BODY") {
            let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
            let _ = std::fs::write(std::path::Path::new(&dir).join(format!("body-{n}.json")), body.to_string());
        }
        let mut retried = false;
        loop {
            let resp = self.request(reqwest::Method::POST, "messages")?.json(body).send().await?;
            let status = resp.status();
            if status.is_success() {
                return Ok(resp);
            }
            let retryable = matches!(status.as_u16(), 429 | 529) || status.is_server_error();
            let text = resp.text().await.unwrap_or_default();
            if retryable && !retried {
                retried = true;
                tokio::time::sleep(Duration::from_millis(600)).await;
                continue;
            }
            return Err(api_error(status.as_u16(), &text).into());
        }
    }

    /// Mensaje con `stream: true`; llama a `on` por cada fragmento de texto.
    pub async fn stream_message(
        &self,
        body: &Value,
        cancel: &CancellationToken,
        mut on: impl FnMut(Delta),
    ) -> Result<()> {
        let resp = tokio::select! {
            _ = cancel.cancelled() => return Err(Cancelled.into()),
            r = self.send(body) => r?,
        };
        let mut stream = resp.bytes_stream();
        let mut buf: Vec<u8> = Vec::new();
        loop {
            let next = tokio::select! {
                _ = cancel.cancelled() => return Err(Cancelled.into()),
                n = tokio::time::timeout(Duration::from_secs(60), stream.next()) => {
                    n.context("Claude dejó de responder")?
                }
            };
            let Some(chunk) = next else { break };
            buf.extend_from_slice(&chunk?);
            while let Some(nl) = buf.iter().position(|&b| b == b'\n') {
                let line: Vec<u8> = buf.drain(..=nl).collect();
                let line = String::from_utf8_lossy(&line);
                let Some(data) = line.trim().strip_prefix("data:") else { continue };
                match parse_event(data.trim()) {
                    SseEvent::Text(t) if !t.is_empty() => on(Delta::Content(&t)),
                    SseEvent::Thinking => on(Delta::Reasoning),
                    SseEvent::Stop(Some(reason)) if reason == "refusal" => return Err(Refused.into()),
                    SseEvent::Error(msg) => bail!("Claude: {msg}"),
                    _ => {}
                }
            }
        }
        Ok(())
    }

    /// Mensaje sin streaming; devuelve sólo los bloques de texto (puede haber bloques de thinking).
    pub async fn message(&self, body: &Value) -> Result<String> {
        let v: Value = self.send(body).await?.json().await?;
        if v["stop_reason"] == "refusal" {
            return Err(Refused.into());
        }
        Ok(v["content"]
            .as_array()
            .map(|blocks| {
                blocks
                    .iter()
                    .filter(|b| b["type"] == "text")
                    .filter_map(|b| b["text"].as_str())
                    .collect::<String>()
            })
            .unwrap_or_default())
    }

    /// Valida la key y que el modelo exista y acepte imágenes (`GET /models/{id}`, sin costo).
    pub async fn check(&self) -> Result<()> {
        let resp = self
            .request(reqwest::Method::GET, &format!("models/{}", self.model))?
            .timeout(Duration::from_secs(8))
            .send()
            .await?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(api_error(status.as_u16(), &text).into());
        }
        let v: Value = resp.json().await?;
        if v["capabilities"]["image_input"]["supported"] == false {
            bail!("El modelo {} no acepta imágenes", self.model);
        }
        Ok(())
    }

    /// Abre la conexión TLS por adelantado (se llama al pulsar el atajo).
    pub async fn warmup(&self) {
        if self.has_key() {
            let _ = self.check().await;
        }
    }
}

/// No hay conexión con la API (sin internet, DNS, tiempo agotado).
pub fn is_unreachable(err: &anyhow::Error) -> bool {
    err.chain().any(|e| {
        e.downcast_ref::<reqwest::Error>()
            .is_some_and(|r| r.is_connect() || r.is_timeout())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eventos_sse() {
        assert_eq!(
            parse_event(r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hola"}}"#),
            SseEvent::Text("Hola".into())
        );
        assert_eq!(
            parse_event(r#"{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}"#),
            SseEvent::Thinking
        );
        assert_eq!(
            parse_event(r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":""}}"#),
            SseEvent::Thinking
        );
        assert_eq!(
            parse_event(r#"{"type":"message_delta","delta":{"stop_reason":"refusal"},"usage":{"output_tokens":3}}"#),
            SseEvent::Stop(Some("refusal".into()))
        );
        assert_eq!(
            parse_event(r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#),
            SseEvent::Error("Overloaded".into())
        );
        assert_eq!(parse_event(r#"{"type":"ping"}"#), SseEvent::Other);
        assert_eq!(parse_event("no es json"), SseEvent::Other);
    }

    #[test]
    fn mensajes_de_error() {
        let e = api_error(401, r#"{"type":"error","error":{"type":"authentication_error","message":"invalid x-api-key"}}"#);
        assert!(e.to_string().contains("inválida"));
        let e = api_error(400, r#"{"type":"error","error":{"type":"invalid_request_error","message":"bad"}}"#);
        assert_eq!(e.to_string(), "Error 400 de la API de Anthropic: bad");
    }
}
