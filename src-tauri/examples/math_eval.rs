//! Compara la lectura de fórmulas con y sin razonamiento adaptativo usando el código real.
//!
//!   cargo run --example math_eval -- [repeticiones] [--adaptativo] [--formulas f.txt] [--salida lecturas.json]
//!
//! `--formulas` reemplaza las fórmulas de prueba (una por línea); `--adaptativo` mide sólo la
//! configuración por defecto; `--salida` guarda cada lectura marcada para revisarla con KaTeX.

use std::time::Instant;

use lector_lib::doc::mathparts::PartKind;
use lector_lib::lang::Lang;
use lector_lib::llm::{ClaudeClient, MathSpeaker};
use lector_lib::secrets;
use serde_json::json;

const FORMULAS: &[&str] = &[
    r"s_\theta(v_1, v_2) = \frac{v_1^T v_2}{\|v_1\|_2 \|v_2\|_2} = 0.6682.",
    r"\|x - y\|_2 = \sqrt{(x_1 - y_1)^2 + (x_2 - y_2)^2} = \sqrt{0.5^2 + 0.5^2} = 0.7071",
    r"\int_0^{\infty} e^{-x^2}\,dx = \frac{\sqrt{\pi}}{2}",
    r"f'(x) = \lim_{h \to 0} \frac{f(x+h)-f(x)}{h}",
    r"\sum_{i=1}^{n} x_i^2 = \|x\|_2^2",
];

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let n: usize = args.first().and_then(|a| a.parse().ok()).unwrap_or(3);
    let value = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned();
    let custom = match value("--formulas") {
        Some(path) => std::fs::read_to_string(path)?,
        None => String::new(),
    };
    let custom: Vec<&str> = custom.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    let formulas = if custom.is_empty() { FORMULAS } else { &custom[..] };
    let modes: &[bool] = if args.iter().any(|a| a == "--adaptativo") { &[true] } else { &[false, true] };

    let llm = ClaudeClient::new("claude-haiku-5-5", secrets::get_api_key().map(|(k, _)| k));
    let mut saved = Vec::new();
    for &adaptive in modes {
        let (mut times, mut by_parts, mut lost, mut total) = (Vec::new(), 0, 0, 0);
        for f in formulas {
            for _ in 0..n {
                // Un lector nuevo en cada vuelta: sin caché.
                let speaker = MathSpeaker::default();
                let t0 = Instant::now();
                let r = speaker.speak(&llm, f, Lang::Es, true, adaptive).await;
                times.push(t0.elapsed().as_secs_f64());
                total += 1;
                by_parts += usize::from(r.annotated.is_some());
                lost += r.kinds.iter().filter(|k| **k == PartKind::Lost).count();
                saved.push(json!({"latex": f, "annotated": r.annotated, "kinds": r.kinds, "pieces": r.pieces}));
            }
        }
        times.sort_by(f64::total_cmp);
        let mean = times.iter().sum::<f64>() / times.len() as f64;
        println!(
            "{:<26} lecturas con resaltado por partes {by_parts}/{total} · frases sin marcar {lost} · tiempo medio {mean:.2} s · peor {:.2} s",
            if adaptive { "razonamiento adaptativo" } else { "sin razonamiento" },
            times.last().copied().unwrap_or(0.0)
        );
    }
    if let Some(path) = value("--salida") {
        std::fs::write(&path, serde_json::to_string_pretty(&saved)?)?;
        println!("{} lecturas guardadas en {path}", saved.len());
    }
    Ok(())
}
