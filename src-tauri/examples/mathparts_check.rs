//! Diagnóstico: aplica `mathparts::annotate` a lecturas guardadas (JSON: [{latex, texs}])
//! y escribe el resultado, para compararlo con el prototipo.
//!
//!   cargo run --example mathparts_check -- lecturas.json salida.json

use lector_lib::doc::mathparts;
use serde_json::{json, Value};

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [input, output] = &args[..] else { anyhow::bail!("uso: mathparts_check <entrada.json> <salida.json>") };
    let cases: Vec<Value> = serde_json::from_str(&std::fs::read_to_string(input)?)?;
    let results: Vec<Value> = cases
        .iter()
        .map(|c| {
            let latex = c["latex"].as_str().unwrap_or_default();
            let texs: Vec<&str> = c["texs"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
            let a = mathparts::annotate(latex, &texs);
            json!({"latex": latex, "annotated": a.latex, "kinds": a.kinds})
        })
        .collect();
    std::fs::write(output, serde_json::to_string(&results)?)?;
    println!("{} lecturas procesadas", results.len());
    Ok(())
}
