//! Etapa 2: LaTeX → cómo lo diría un profesor, con caché y paralelismo limitado.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use serde::Deserialize;
use serde_json::json;
use tokio::sync::Semaphore;

use super::claude::ClaudeClient;
use super::prompts;
use crate::doc::mathparts::{self, PartKind};
use crate::lang::Lang;

/// Cada fórmula tarda ~1 s en la API; en paralelo, 6 terminan en ~1 s (medido).
const PARALLEL: usize = 6;
const CACHE_MAX: usize = 1000;

/// Lectura de una fórmula en frases. Si `annotated` existe, la frase `i` con
/// `kinds[i] == Part` corresponde al fragmento marcado con `\htmlData{part=i}`.
#[derive(Clone, Debug, PartialEq)]
pub struct MathReading {
    pub pieces: Vec<String>,
    pub annotated: Option<String>,
    pub kinds: Vec<PartKind>,
    /// Frases sin marcar más señales de contenido omitido (ver `Annotation::omissions`).
    problems: usize,
}

impl MathReading {
    fn single(say: String) -> Self {
        Self { pieces: vec![say], annotated: None, kinds: vec![PartKind::Link], problems: 0 }
    }

    pub fn spoken(&self) -> String {
        self.pieces.join(" ")
    }
}

pub struct MathSpeaker {
    sem: Semaphore,
    cache: Mutex<HashMap<(Lang, bool, bool, String), MathReading>>,
}

impl Default for MathSpeaker {
    fn default() -> Self {
        Self {
            sem: Semaphore::new(PARALLEL),
            cache: Mutex::new(HashMap::new()),
        }
    }
}

impl MathSpeaker {
    /// Nunca falla: si el LLM no responde usa una conversión local aproximada.
    /// Con `parts`, divide las fórmulas no triviales para resaltar cada fragmento al leerlo.
    /// Con `adaptive`, el modelo razona lo que considere necesario antes de responder.
    pub async fn speak(&self, llm: &ClaudeClient, latex: &str, lang: Lang, parts: bool, adaptive: bool) -> MathReading {
        let latex = latex.trim();
        let parts = parts && mathparts::needs_parts(latex);
        let key = (lang, parts, adaptive, latex.to_owned());
        if let Some(r) = self.cache.lock().unwrap().get(&key) {
            return r.clone();
        }
        let _permit = self.sem.acquire().await;
        let reading = match parts {
            true => match speak_parts(llm, latex, lang, adaptive).await {
                Some(r) => r,
                None => speak_plain(llm, latex, lang, adaptive).await,
            },
            false => speak_plain(llm, latex, lang, adaptive).await,
        };
        let mut cache = self.cache.lock().unwrap();
        if cache.len() >= CACHE_MAX {
            cache.clear();
        }
        cache.insert(key, reading.clone());
        reading
    }
}

/// Lectura de una sola pieza (sin resaltado por partes).
/// `thinking` y `max_tokens` según el modo: el razonamiento cuenta dentro del límite.
fn thinking(adaptive: bool, max_tokens: u32) -> (serde_json::Value, u32) {
    if adaptive {
        (json!({"type": "adaptive"}), max_tokens * 4)
    } else {
        (json!({"type": "disabled"}), max_tokens)
    }
}

async fn speak_plain(llm: &ClaudeClient, latex: &str, lang: Lang, adaptive: bool) -> MathReading {
    let (thinking, max_tokens) = thinking(adaptive, 1024);
    let body = json!({
        "model": llm.model(),
        "max_tokens": max_tokens,
        "thinking": thinking,
        "output_config": {"effort": "low"},
        "system": prompts::math(lang),
        "messages": [{"role": "user", "content": latex}],
    });
    let spoken = match tokio::time::timeout(Duration::from_secs(30), llm.message(&body)).await {
        Ok(Ok(s)) => clean(&s),
        Ok(Err(e)) => {
            eprintln!("etapa 2 falló para {latex:?}: {e:#}");
            String::new()
        }
        Err(_) => String::new(),
    };
    MathReading::single(if spoken.is_empty() || looks_like_latex(&spoken) {
        fallback(latex, lang)
    } else {
        spoken
    })
}

#[derive(Deserialize)]
struct RawPieces {
    pieces: Vec<RawPiece>,
}

#[derive(Deserialize)]
struct RawPiece {
    say: String,
    tex: String,
}

/// Lectura en frases. Como cada respuesta del modelo sale distinta, si alguna frase no se
/// pudo marcar o la lectura parece omitir algo, se pide otra vez y se queda la mejor.
/// `None`: usar la lectura simple (una lectura completa vale más que el resaltado).
async fn speak_parts(llm: &ClaudeClient, latex: &str, lang: Lang, adaptive: bool) -> Option<MathReading> {
    let mut best: Option<MathReading> = None;
    for _ in 0..2 {
        let Some(reading) = request_parts(llm, latex, lang, adaptive).await else { continue };
        let better = best.as_ref().map_or(true, |b| {
            (reading.annotated.is_some(), std::cmp::Reverse(reading.problems))
                > (b.annotated.is_some(), std::cmp::Reverse(b.problems))
        });
        if better {
            best = Some(reading);
        }
        if best.as_ref().is_some_and(|b| b.annotated.is_some() && b.problems == 0) {
            break;
        }
    }
    // Una lectura que omite contenido se descarta; una frase sin marcar sólo pierde su resaltado.
    best.filter(|b| b.annotated.is_some() && b.problems == b.kinds.iter().filter(|k| **k == PartKind::Lost).count())
}

/// Una petición con *structured outputs*. `None` si la respuesta no sirve.
async fn request_parts(llm: &ClaudeClient, latex: &str, lang: Lang, adaptive: bool) -> Option<MathReading> {
    let (thinking, max_tokens) = thinking(adaptive, 2048);
    let body = json!({
        "model": llm.model(),
        "max_tokens": max_tokens,
        "thinking": thinking,
        "output_config": {
            "effort": "low",
            "format": {"type": "json_schema", "schema": prompts::pieces_schema()},
        },
        "system": prompts::math_parts(lang),
        "messages": [{"role": "user", "content": latex}],
    });
    let text = match tokio::time::timeout(Duration::from_secs(40), llm.message(&body)).await {
        Ok(Ok(t)) => t,
        Ok(Err(e)) => {
            eprintln!("etapa 2 por partes falló para {latex:?}: {e:#}");
            return None;
        }
        Err(_) => return None,
    };
    let raw: RawPieces = serde_json::from_str(&text).ok()?;
    let pieces: Vec<(String, String)> = raw
        .pieces
        .into_iter()
        .map(|p| (clean(&p.say), p.tex))
        .filter(|(say, _)| !say.is_empty())
        .collect();
    if pieces.is_empty() || pieces.iter().any(|(say, _)| looks_like_latex(say)) {
        return None;
    }
    let (says, texs): (Vec<String>, Vec<String>) = pieces.into_iter().unzip();
    let texs: Vec<&str> = texs.iter().map(String::as_str).collect();
    let annotation = mathparts::annotate(latex, &texs);
    let omissions = annotation.omissions(&says);
    if omissions > 0 {
        eprintln!("etapa 2: la lectura de {latex:?} parece omitir contenido; se reintenta");
        for (say, tex) in says.iter().zip(&texs) {
            eprintln!("    {tex:?} → {say}");
        }
    }
    Some(MathReading {
        problems: annotation.lost() + omissions,
        pieces: says,
        annotated: annotation.latex,
        kinds: annotation.kinds,
    })
}

fn clean(s: &str) -> String {
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    s.trim_matches(|c: char| matches!(c, '"' | '\'' | '“' | '”' | '«' | '»' | '`'))
        .trim()
        .to_owned()
}

fn looks_like_latex(s: &str) -> bool {
    s.contains('\\') || s.contains('$') || s.contains('{')
}

/// Conversión local aproximada para cuando el LLM no está disponible.
pub fn fallback(latex: &str, lang: Lang) -> String {
    let es = lang == Lang::Es;
    let words: &[(&str, &str, &str)] = &[
        ("\\frac", " fracción ", " fraction "),
        ("\\sqrt", " raíz de ", " square root of "),
        ("\\sum", " suma ", " sum "),
        ("\\int", " integral ", " integral "),
        ("\\cdot", " por ", " times "),
        ("\\times", " por ", " times "),
        ("\\leq", " menor o igual que ", " less than or equal to "),
        ("\\geq", " mayor o igual que ", " greater than or equal to "),
        ("\\neq", " distinto de ", " not equal to "),
        ("\\infty", " infinito ", " infinity "),
        ("\\|", " norma ", " norm "),
        ("\\alpha", " alfa ", " alpha "),
        ("\\beta", " beta ", " beta "),
        ("\\gamma", " gamma ", " gamma "),
        ("\\delta", " delta ", " delta "),
        ("\\theta", " theta ", " theta "),
        ("\\lambda", " lambda ", " lambda "),
        ("\\mu", " mu ", " mu "),
        ("\\pi", " pi ", " pi "),
        ("\\sigma", " sigma ", " sigma "),
        ("_", " sub ", " sub "),
        ("^", " a la ", " to the "),
        ("=", " igual a ", " equals "),
        ("+", " más ", " plus "),
        ("-", " menos ", " minus "),
        ("<", " menor que ", " less than "),
        (">", " mayor que ", " greater than "),
    ];
    let mut s = latex.to_owned();
    for (k, e, n) in words {
        s = s.replace(k, if es { e } else { n });
    }
    let s: String = s
        .chars()
        .map(|c| if matches!(c, '\\' | '{' | '}' | '$') { ' ' } else { c })
        .collect();
    let s = s.replace("mathbf", "").replace("mathrm", "").replace("left", "").replace("right", "");
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limpia_respuesta() {
        assert_eq!(clean("  \"v sub uno\"\n"), "v sub uno");
    }

    #[test]
    fn respaldo_local() {
        assert_eq!(fallback("v_1 + v_2", Lang::Es), "v sub 1 más v sub 2");
        assert!(looks_like_latex("\\frac{a}{b}"));
    }
}
