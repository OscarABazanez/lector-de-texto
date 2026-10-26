//! Prueba de punta a punta sin interfaz: transcribe una imagen, la lee en voz
//! alta e imprime cada evento con su tiempo.
//!
//!   cargo run --example pipeline -- ..\tests\fixtures\coseno.png
//!   cargo run --example pipeline -- --edge ..\tests\fixtures\coseno.png
//!   cargo run --example pipeline -- --text "Sea $v_1$ un vector." [--edge]
//!
//! Opciones: --edge / --system (motor de voz; --windows también vale), --precise, --model <id>, --text <markdown>,
//! --saltos (prueba los botones de ±2 palabras), --clic <seg>:<palabra> (simula un clic en una palabra), --sin-razonar (desactiva el razonamiento adaptativo).
//! La API key se toma del almacén de credenciales (Ajustes) o de `ANTHROPIC_API_KEY`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use lector_lib::config::{Config, Engine};
use lector_lib::llm::{ClaudeClient, MathSpeaker};
use lector_lib::pipeline::{self, Ctrl, Deps, Input, Reading, Settings, Status, Ui, UiEvent};
use lector_lib::player::Player;
use lector_lib::secrets;
use lector_lib::tts::Tts;
use tokio::sync::mpsc;

struct ConsoleUi {
    t0: Instant,
    done: mpsc::UnboundedSender<Status>,
    words: Mutex<HashMap<usize, (Vec<String>, String)>>,
    pieces: Mutex<HashMap<usize, Vec<String>>>,
}

impl Ui for ConsoleUi {
    fn emit(&self, ev: UiEvent) {
        let t = self.t0.elapsed().as_secs_f32();
        match ev {
            UiEvent::Reset => {}
            UiEvent::Segment { seg } => {
                println!(
                    "[{t:5.2}s] segmento #{} {:?}{}{}: {}",
                    seg.id,
                    seg.kind,
                    if seg.block { " bloque" } else { "" },
                    if seg.para { " ¶" } else { "" },
                    seg.display
                );
                self.words.lock().unwrap().insert(seg.id, (seg.words, seg.display));
            }
            UiEvent::MathSpoken { seg, spoken, pieces, annotated, kinds } => {
                println!("[{t:5.2}s] fórmula #{seg} → «{spoken}»");
                if pieces.len() > 1 {
                    println!("          {} frases · marcada: {}", pieces.len(), annotated.as_deref().unwrap_or("(sin marcas)"));
                    for (i, (say, kind)) in pieces.iter().zip(&kinds).enumerate() {
                        println!("          {i}. {kind:?}: {say}");
                    }
                }
                self.pieces.lock().unwrap().insert(seg, pieces);
            }
            UiEvent::Highlight { seg, word } => {
                let words = self.words.lock().unwrap();
                let pieces = self.pieces.lock().unwrap();
                let text = match (words.get(&seg), word) {
                    (Some((w, _)), Some(i)) if !w.is_empty() => w.get(i).cloned().unwrap_or_default(),
                    (Some(_), Some(i)) => format!(
                        "[fórmula, frase {i}] {}",
                        pieces.get(&seg).and_then(|p| p.get(i)).cloned().unwrap_or_default()
                    ),
                    (Some((_, display)), None) => format!("[fórmula] {display}"),
                    _ => String::new(),
                };
                println!("[{t:5.2}s]     ▶ {text}");
            }
            UiEvent::Status { status, detail } => {
                println!("[{t:5.2}s] estado: {status:?} {detail}");
                if matches!(status, Status::Done | Status::Error) {
                    let _ = self.done.send(status);
                }
            }
            UiEvent::Lang { lang, voice } => println!("[{t:5.2}s] idioma {lang:?} · voz {voice}"),
            UiEvent::Notice { msg } => println!("[{t:5.2}s] AVISO: {msg}"),
        }
    }
}

fn arg_value(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut cfg = Config::load();
    if args.iter().any(|a| a == "--edge") {
        cfg.tts.engine = Engine::Edge;
    }
    if args.iter().any(|a| a == "--windows" || a == "--system") {
        cfg.tts.engine = Engine::System;
    }
    if let Some(model) = arg_value(&args, "--model") {
        cfg.llm.model = model;
    }
    let precise = cfg.llm.precise_mode || args.iter().any(|a| a == "--precise");

    let input = match arg_value(&args, "--text") {
        Some(text) => Input::Text(text),
        None => {
            let path = args
                .iter()
                .rev()
                .find(|a| !a.starts_with("--"))
                .cloned()
                .unwrap_or_else(|| "../tests/fixtures/coseno.png".into());
            Input::Image(image::open(&path)?.to_rgba8())
        }
    };

    let reading: Arc<OnceLock<Reading>> = Arc::new(OnceLock::new());
    let player = {
        let reading = reading.clone();
        Player::new(move |ev| {
            if let Some(r) = reading.get() {
                r.send(Ctrl::Player(ev));
            }
        })
    };
    let (done_tx, mut done_rx) = mpsc::unbounded_channel();
    let ui = Arc::new(ConsoleUi { t0: Instant::now(), done: done_tx, words: Mutex::default(), pieces: Mutex::default() });
    let deps = Deps {
        llm: Arc::new(ClaudeClient::new(&cfg.llm.model, secrets::get_api_key().map(|(k, _)| k))),
        math: Arc::new(MathSpeaker::default()),
        tts: Arc::new(Tts::default()),
        player: Arc::new(player),
    };
    let settings = Settings {
        engine: cfg.tts.engine,
        voices: cfg.tts.clone(),
        rate: cfg.tts.rate,
        precise,
        adaptive: cfg.llm.adaptive && !args.iter().any(|a| a == "--sin-razonar"),
        default_lang: cfg.ui.default_language,
        math_parts: cfg.ui.math_parts,
    };
    println!(
        "modelo {} · key {} · motor {:?} · preciso {precise}",
        cfg.llm.model,
        if deps.llm.has_key() { "sí" } else { "NO" },
        cfg.tts.engine
    );
    // La app abre la conexión de voz al pulsar el atajo; aquí se imita.
    {
        let tts = deps.tts.clone();
        let engine = cfg.tts.engine;
        tokio::spawn(async move { tts.prepare(engine).await });
        let llm = deps.llm.clone();
        tokio::spawn(async move { llm.warmup().await });
    }
    let _ = reading.set(pipeline::start(1, deps, ui, settings, input));
    if let Some(spec) = arg_value(&args, "--clic") {
        // Simula un clic en una palabra: --clic <segmento>:<palabra>, a los 2 s.
        let (seg, word) = spec.split_once(':').unwrap_or((&spec, ""));
        let (seg, word): (usize, Option<usize>) = (seg.parse()?, word.parse().ok());
        let reading = reading.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(2)).await;
            println!("---------- clic en segmento {seg}, palabra {word:?}");
            if let Some(r) = reading.get() {
                r.send(Ctrl::SeekSegment(seg, word));
            }
        });
    }
    if args.iter().any(|a| a == "--saltos") {
        // Prueba de los botones de ±2 palabras.
        let reading = reading.clone();
        tokio::spawn(async move {
            for (wait, delta) in [(3, -2), (3, 2)] {
                tokio::time::sleep(Duration::from_secs(wait)).await;
                println!("---------- salto {delta:+} palabras");
                if let Some(r) = reading.get() {
                    r.send(Ctrl::SeekWords(delta));
                }
            }
        });
    }

    match tokio::time::timeout(Duration::from_secs(180), done_rx.recv()).await {
        Ok(Some(Status::Done)) => Ok(()),
        Ok(other) => anyhow::bail!("terminó con {other:?}"),
        Err(_) => anyhow::bail!("tiempo agotado"),
    }
}
