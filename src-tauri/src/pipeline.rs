//! Orquesta una lectura completa:
//! imagen → (LLM, streaming) segmentos → fórmulas a voz → fragmentos → TTS → reproductor.
//!
//! Dos tareas comparten el documento: el *productor* lo llena conforme llega la
//! transcripción y el *alimentador* sintetiza y encola cada fragmento en cuanto
//! está listo (con un fragmento de adelanto), así la voz empieza antes de que
//! el modelo termine.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use image::RgbaImage;
use serde::Serialize;
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;

use crate::capture;
use crate::config::{Engine, TtsConfig};
use crate::doc::{Chunker, Script, SegKind, Segment, StreamParser, Target};
use crate::lang::{self, Lang};
use crate::llm::claude::{self, ClaudeClient};
use crate::doc::mathparts::PartKind;
use crate::llm::math_speech::MathReading;
use crate::llm::{self, Delta, MathSpeaker};
use crate::player::{PlayItem, Player, PlayerEvent};
use crate::tts::{Mark, Tts};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Transcribing,
    Thinking,
    Reading,
    Paused,
    Stopped,
    Done,
    Error,
}

/// Eventos hacia la ventana del lector.
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum UiEvent {
    Reset,
    Segment { seg: Segment },
    /// `annotated` es la fórmula con `\htmlData{part=i}` en el fragmento de la frase `i`.
    MathSpoken {
        seg: usize,
        spoken: String,
        pieces: Vec<String>,
        annotated: Option<String>,
        kinds: Vec<PartKind>,
    },
    Highlight { seg: usize, word: Option<usize> },
    Status { status: Status, detail: String },
    Lang { lang: Lang, voice: String },
    Notice { msg: String },
}

pub trait Ui: Send + Sync + 'static {
    fn emit(&self, ev: UiEvent);
}

pub struct Deps {
    pub llm: Arc<ClaudeClient>,
    pub math: Arc<MathSpeaker>,
    pub tts: Arc<Tts>,
    pub player: Arc<Player>,
}

#[derive(Clone)]
pub struct Settings {
    pub engine: Engine,
    pub voices: TtsConfig,
    pub rate: f32,
    pub precise: bool,
    /// Razonamiento adaptativo en las dos etapas.
    pub adaptive: bool,
    pub default_lang: Lang,
    /// Dividir las fórmulas para resaltar el fragmento que se lee.
    pub math_parts: bool,
}

pub enum Input {
    Image(RgbaImage),
    Text(String),
}

/// Órdenes para una lectura en curso.
pub enum Ctrl {
    Player(PlayerEvent),
    Toggle,
    Stop,
    /// Mover ±n fragmentos (0 = repetir el actual).
    Seek(i32),
    /// Mover ±n palabras (o frases de fórmula) desde la que suena.
    SeekWords(i32),
    /// Leer desde una palabra (o frase de fórmula) de un segmento; `None` = desde su inicio.
    SeekSegment(usize, Option<usize>),
    SetRate(f32),
}

pub struct Reading {
    id: u64,
    cancel: CancellationToken,
    ctrl: mpsc::UnboundedSender<Ctrl>,
}

impl Reading {
    pub fn id(&self) -> u64 {
        self.id
    }

    pub fn send(&self, c: Ctrl) {
        let _ = self.ctrl.send(c);
    }

    pub fn cancel(&self) {
        self.cancel.cancel();
    }
}

impl Drop for Reading {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

#[derive(Default)]
struct Doc {
    segs: Vec<Segment>,
    spoken: HashMap<usize, MathReading>,
    chunks: Vec<Vec<usize>>,
    complete: bool,
    lang: Option<Lang>,
}

struct Shared {
    doc: Mutex<Doc>,
    version: watch::Sender<u64>,
}

impl Shared {
    fn bump(&self) {
        self.version.send_modify(|v| *v += 1);
    }
}

/// Arranca una lectura. Debe llamarse dentro de un runtime de tokio.
pub fn start(id: u64, deps: Deps, ui: Arc<dyn Ui>, settings: Settings, input: Input) -> Reading {
    let cancel = CancellationToken::new();
    let (ctrl_tx, ctrl_rx) = mpsc::unbounded_channel();
    let (version_tx, version_rx) = watch::channel(0u64);
    let shared = Arc::new(Shared { doc: Mutex::new(Doc::default()), version: version_tx });
    let deps = Arc::new(deps);
    let t0 = Instant::now();

    let producer = Producer {
        shared: shared.clone(),
        deps: deps.clone(),
        ui: ui.clone(),
        settings: settings.clone(),
        cancel: cancel.clone(),
        parser: StreamParser::new(),
        chunker: Chunker::default(),
        lang_text: String::new(),
        thinking: false,
    };
    tokio::spawn(producer.run(input));

    let feeder = Feeder {
        id,
        engine: settings.engine,
        shared,
        version: version_rx,
        deps,
        ui,
        settings,
        ctrl: ctrl_rx,
        cancel: cancel.clone(),
        t0,
        epoch: 0,
        next: 0,
        in_flight: VecDeque::new(),
        playing: None,
        last_started: None,
        last_target: None,
        start_offset: None,
        stopped: false,
        done: false,
        first_audio: false,
        lang_sent: false,
        last_status: None,
        cache: HashMap::new(),
    };
    tokio::spawn(feeder.run());

    Reading { id, cancel, ctrl: ctrl_tx }
}

// ---------------------------------------------------------------- productor

struct Producer {
    shared: Arc<Shared>,
    deps: Arc<Deps>,
    ui: Arc<dyn Ui>,
    settings: Settings,
    cancel: CancellationToken,
    parser: StreamParser,
    chunker: Chunker,
    lang_text: String,
    thinking: bool,
}

impl Producer {
    async fn run(mut self, input: Input) {
        match input {
            Input::Text(text) => {
                for seg in self.parser.push(&text) {
                    self.on_segment(seg);
                }
            }
            Input::Image(img) => self.transcribe(img).await,
        }
        if self.cancel.is_cancelled() {
            return;
        }
        self.finish();
        if self.shared.doc.lock().unwrap().segs.is_empty() {
            self.status(Status::Error, "No se encontró texto en la selección.");
        }
    }

    async fn transcribe(&mut self, img: RgbaImage) {
        self.status(Status::Transcribing, "");
        let (img, b64) = match tokio::task::spawn_blocking(move || {
            let b64 = capture::to_png_base64(&img);
            (img, b64)
        })
        .await
        {
            Ok((img, Ok(b64))) => (img, b64),
            Ok((_, Err(e))) => return self.status(Status::Error, &format!("Imagen inválida: {e:#}")),
            Err(e) => return self.status(Status::Error, &format!("Imagen inválida: {e}")),
        };

        let llm = self.deps.llm.clone();
        if !llm.has_key() {
            return self.ocr_fallback(img, "Falta la API key de Anthropic (Ajustes en la bandeja).").await;
        }
        let cancel = self.cancel.clone();
        let (precise, adaptive) = (self.settings.precise, self.settings.adaptive);
        let res = llm::transcribe::transcribe(&llm, &b64, precise, adaptive, &cancel, |d| match d {
            Delta::Reasoning => {
                if !self.thinking {
                    self.thinking = true;
                    self.status(Status::Thinking, "");
                }
            }
            Delta::Content(s) => {
                for seg in self.parser.push(s) {
                    self.on_segment(seg);
                }
            }
        })
        .await;

        let Err(e) = res else { return };
        if e.is::<llm::Cancelled>() {
            return;
        }
        let nothing_yet = self.shared.doc.lock().unwrap().segs.is_empty();
        if e.is::<claude::Refused>() {
            self.notice(&format!("{e}."));
        } else if nothing_yet && claude::is_unreachable(&e) {
            self.ocr_fallback(img, "Sin conexión con Anthropic.").await;
        } else if nothing_yet && e.is::<claude::ApiError>() {
            self.ocr_fallback(img, &format!("{e}.")).await;
        } else {
            self.notice(&format!("Error del modelo: {e:#}"));
        }
    }

    #[cfg(windows)]
    async fn ocr_fallback(&mut self, img: RgbaImage, reason: &str) {
        self.notice(&format!("{reason} Leo con el OCR de Windows (sin fórmulas)."));
        match crate::ocr::recognize(img).await {
            Ok(text) => {
                for seg in self.parser.push(&text) {
                    self.on_segment(seg);
                }
            }
            Err(e) => self.notice(&format!("El OCR de Windows falló: {e:#}")),
        }
    }

    #[cfg(not(windows))]
    async fn ocr_fallback(&mut self, _img: RgbaImage, reason: &str) {
        self.notice(reason);
    }

    fn on_segment(&mut self, seg: Segment) {
        if seg.kind != SegKind::Math {
            self.lang_text.push(' ');
            self.lang_text.push_str(&seg.display);
        }
        let default = self.settings.default_lang;
        let mut doc = self.shared.doc.lock().unwrap();
        if doc.lang.is_none() {
            doc.lang = lang::detect(&self.lang_text);
        }
        let closed = self.chunker.push(&seg);
        // Una fórmula o un fragmento listo ya no pueden esperar a saber el idioma.
        if doc.lang.is_none() && (seg.kind == SegKind::Math || !closed.is_empty()) {
            doc.lang = Some(lang::decide(&self.lang_text, default));
        }
        doc.chunks.extend(closed);
        doc.segs.push(seg.clone());
        let lang = doc.lang.unwrap_or(default);
        drop(doc);

        let math = (seg.kind == SegKind::Math).then(|| (seg.id, seg.display.clone()));
        self.ui.emit(UiEvent::Segment { seg });
        if let Some((id, latex)) = math {
            self.spawn_math(id, latex, lang);
        }
        self.shared.bump();
    }

    fn spawn_math(&self, id: usize, latex: String, lang: Lang) {
        let (shared, deps, ui, cancel) =
            (self.shared.clone(), self.deps.clone(), self.ui.clone(), self.cancel.clone());
        let (parts, adaptive) = (self.settings.math_parts, self.settings.adaptive);
        tokio::spawn(async move {
            let reading = tokio::select! {
                _ = cancel.cancelled() => return,
                r = deps.math.speak(&deps.llm, &latex, lang, parts, adaptive) => r,
            };
            ui.emit(UiEvent::MathSpoken {
                seg: id,
                spoken: reading.spoken(),
                pieces: reading.pieces.clone(),
                annotated: reading.annotated.clone(),
                kinds: reading.kinds.clone(),
            });
            shared.doc.lock().unwrap().spoken.insert(id, reading);
            shared.bump();
        });
    }

    fn finish(&mut self) {
        for seg in self.parser.finish() {
            self.on_segment(seg);
        }
        let mut doc = self.shared.doc.lock().unwrap();
        doc.chunks.extend(self.chunker.finish());
        if doc.lang.is_none() {
            doc.lang = Some(lang::decide(&self.lang_text, self.settings.default_lang));
        }
        doc.complete = true;
        drop(doc);
        self.shared.bump();
    }

    fn status(&self, status: Status, detail: &str) {
        self.ui.emit(UiEvent::Status { status, detail: detail.to_owned() });
    }

    fn notice(&self, msg: &str) {
        eprintln!("{msg}");
        self.ui.emit(UiEvent::Notice { msg: msg.to_owned() });
    }
}

// ------------------------------------------------------------ alimentador

struct Prepared {
    audio: Vec<u8>,
    marks: Vec<(u32, Target)>,
    end_ms: Option<u32>,
}

enum Ready {
    Chunk(Script),
    Wait,
    End,
}

/// Dónde empezar a leer un fragmento.
#[derive(Clone, Copy, Debug)]
enum StartAt {
    /// Número de pieza dentro del fragmento.
    Piece(usize),
    /// La pieza que resalta esta palabra o frase de fórmula.
    Target(Target),
}

/// Fragmentos encolados por delante del que suena.
const LOOKAHEAD: usize = 2;

struct Feeder {
    id: u64,
    engine: Engine,
    shared: Arc<Shared>,
    version: watch::Receiver<u64>,
    deps: Arc<Deps>,
    ui: Arc<dyn Ui>,
    settings: Settings,
    ctrl: mpsc::UnboundedReceiver<Ctrl>,
    cancel: CancellationToken,
    t0: Instant,
    /// Cambia en cada salto/detención para descartar eventos viejos del reproductor.
    epoch: u64,
    next: usize,
    in_flight: VecDeque<usize>,
    playing: Option<usize>,
    last_started: Option<usize>,
    /// Última palabra resaltada y su fragmento (para saltar por palabras).
    last_target: Option<(usize, Target)>,
    /// Empezar el fragmento indicado a partir de esa pieza.
    start_offset: Option<(usize, StartAt)>,
    stopped: bool,
    done: bool,
    first_audio: bool,
    lang_sent: bool,
    last_status: Option<Status>,
    /// (fragmento, pieza inicial, velocidad, motor) → audio preparado.
    cache: HashMap<(usize, usize, u32, Engine), Arc<Prepared>>,
}

impl Feeder {
    async fn run(mut self) {
        loop {
            if self.cancel.is_cancelled() {
                break;
            }
            while let Ok(c) = self.ctrl.try_recv() {
                self.on_ctrl(c);
            }
            if !self.stopped && self.in_flight.len() < LOOKAHEAD {
                match self.ready(self.next) {
                    Ready::Chunk(script) => {
                        let k = self.next;
                        self.next += 1;
                        let offset = match self.start_offset {
                            Some((chunk, start)) if chunk == k => {
                                self.start_offset = None;
                                match start {
                                    StartAt::Piece(i) => i,
                                    // Se resuelve aquí: al hacer clic la oración podía no estar lista.
                                    StartAt::Target(t) => script.index_of(t).unwrap_or(0),
                                }
                            }
                            _ => 0,
                        };
                        let script = if offset > 0 { script.tail(offset) } else { script };
                        if !script.is_empty() {
                            self.synth_and_enqueue(k, offset, script).await;
                        }
                        continue;
                    }
                    Ready::End if self.in_flight.is_empty() && !self.done && self.next > 0 => {
                        self.done = true;
                        self.stopped = true;
                        self.status(Status::Done, "");
                    }
                    _ => {}
                }
            }
            tokio::select! {
                _ = self.cancel.cancelled() => break,
                _ = self.version.changed() => {}
                c = self.ctrl.recv() => match c {
                    Some(c) => self.on_ctrl(c),
                    None => break,
                },
            }
        }
    }

    fn ready(&self, k: usize) -> Ready {
        let doc = self.shared.doc.lock().unwrap();
        let Some(chunk) = doc.chunks.get(k) else {
            return if doc.complete { Ready::End } else { Ready::Wait };
        };
        let segs: Vec<&Segment> = chunk.iter().map(|&i| &doc.segs[i]).collect();
        if segs
            .iter()
            .any(|s| s.kind == SegKind::Math && !doc.spoken.contains_key(&s.id))
        {
            return Ready::Wait;
        }
        Ready::Chunk(Script::build(segs, |id| doc.spoken.get(&id).map(|r| r.pieces.clone())))
    }

    async fn synth_and_enqueue(&mut self, k: usize, offset: usize, script: Script) {
        let lang = self.shared.doc.lock().unwrap().lang.unwrap_or(self.settings.default_lang);
        let prepared = loop {
            let key = (k, offset, self.settings.rate.to_bits(), self.engine);
            if let Some(p) = self.cache.get(&key) {
                break p.clone();
            }
            let voice = self.settings.voices.voice(self.engine, lang).to_owned();
            if !self.lang_sent {
                self.lang_sent = true;
                self.ui.emit(UiEvent::Lang { lang, voice: voice.clone() });
            }
            let tts = self.deps.tts.clone();
            let t_synth = Instant::now();
            let res = tokio::select! {
                _ = self.cancel.cancelled() => return,
                r = tts.synthesize(self.engine, &voice, lang, self.settings.rate, &script.text) => r,
            };
            match res {
                Ok(utt) => {
                    eprintln!(
                        "síntesis fragmento {k} ({:?}): {} ms, {} marcas",
                        self.engine,
                        t_synth.elapsed().as_millis(),
                        utt.marks.len()
                    );
                    let p = Arc::new(Prepared {
                        marks: to_targets(&script, &utt.marks),
                        end_ms: utt.end_ms,
                        audio: utt.audio,
                    });
                    self.cache.insert(key, p.clone());
                    break p;
                }
                Err(e) if self.engine == Engine::Edge => {
                    eprintln!("Edge TTS: {e:#}");
                    self.ui.emit(UiEvent::Notice {
                        msg: "Las voces de Edge no responden; continúo con las voces del sistema.".into(),
                    });
                    self.engine = Engine::System;
                    self.lang_sent = false;
                }
                Err(e) => {
                    self.stopped = true;
                    self.status(Status::Error, &format!("No se pudo generar la voz: {e:#}"));
                    return;
                }
            }
        };
        self.deps.player.enqueue(PlayItem {
            tag: self.tag(k),
            audio: prepared.audio.clone(),
            marks: prepared.marks.clone(),
            end_ms: prepared.end_ms,
        });
        self.in_flight.push_back(k);
    }

    fn on_ctrl(&mut self, c: Ctrl) {
        match c {
            Ctrl::Player(ev) => self.on_player(ev),
            Ctrl::Toggle => {
                if self.in_flight.is_empty() {
                    let from = if self.done { 0 } else { self.last_started.unwrap_or(0) };
                    self.restart(from);
                } else {
                    self.deps.player.toggle_pause();
                }
            }
            Ctrl::Stop => {
                self.halt();
                self.stopped = true;
                self.status(Status::Stopped, "");
            }
            Ctrl::Seek(delta) => {
                let n = self.shared.doc.lock().unwrap().chunks.len();
                if n == 0 {
                    return;
                }
                let base = self
                    .playing
                    .or(self.in_flight.front().copied())
                    .or(self.last_started)
                    .unwrap_or(0) as i64;
                let target = (base + i64::from(delta)).clamp(0, n as i64 - 1) as usize;
                self.restart(target);
            }
            Ctrl::SeekWords(delta) => self.seek_words(delta),
            Ctrl::SeekSegment(seg, word) => {
                let k = self.shared.doc.lock().unwrap().chunks.iter().position(|c| c.contains(&seg));
                if let Some(k) = k {
                    self.restart(k);
                    if word.is_some() {
                        self.start_offset = Some((k, StartAt::Target(Target { seg, word })));
                    }
                }
            }
            Ctrl::SetRate(rate) => {
                self.settings.rate = rate;
                if let Some(k) = self.playing {
                    self.restart(k);
                }
            }
        }
    }

    fn on_player(&mut self, ev: PlayerEvent) {
        match ev {
            PlayerEvent::Started { tag } => {
                let Some(k) = self.chunk_of_tag(tag) else { return };
                self.playing = Some(k);
                self.last_started = Some(k);
                if !self.first_audio {
                    self.first_audio = true;
                    let detail = format!("primer audio en {:.1} s", self.t0.elapsed().as_secs_f32());
                    eprintln!("{detail}");
                    self.status(Status::Reading, &detail);
                } else if self.last_status != Some(Status::Reading) {
                    self.status(Status::Reading, "");
                }
            }
            PlayerEvent::Highlight { tag, target } => {
                if let Some(k) = self.chunk_of_tag(tag) {
                    self.last_target = Some((k, target));
                    self.ui.emit(UiEvent::Highlight { seg: target.seg, word: target.word });
                }
            }
            PlayerEvent::Finished { tag } => {
                let Some(k) = self.chunk_of_tag(tag) else { return };
                self.in_flight.retain(|&c| c != k);
                if self.playing == Some(k) {
                    self.playing = None;
                }
            }
            PlayerEvent::Paused(paused) => {
                if !self.stopped {
                    self.status(if paused { Status::Paused } else { Status::Reading }, "");
                }
            }
            PlayerEvent::Error(msg) => self.ui.emit(UiEvent::Notice { msg }),
        }
    }

    fn halt(&mut self) {
        self.epoch += 1;
        self.deps.player.clear();
        self.in_flight.clear();
        self.playing = None;
    }

    fn restart(&mut self, k: usize) {
        self.halt();
        self.next = k;
        self.start_offset = None;
        self.stopped = false;
        self.done = false;
    }

    /// Salta `delta` piezas desde la última resaltada, cruzando a fragmentos vecinos si hace falta.
    fn seek_words(&mut self, delta: i32) {
        let n_chunks = self.shared.doc.lock().unwrap().chunks.len();
        let (mut chunk, mut pos) = match self.last_target {
            Some((k, target)) => match self.ready(k) {
                Ready::Chunk(script) => (k, script.index_of(target).unwrap_or(0) as i64 + i64::from(delta)),
                _ => (k, 0),
            },
            None => (self.last_started.unwrap_or(0), i64::from(delta.max(0))),
        };
        let len = |k: usize| match self.ready(k) {
            Ready::Chunk(script) => Some(script.len() as i64),
            _ => None,
        };
        loop {
            if pos < 0 {
                match chunk.checked_sub(1).and_then(|p| len(p).map(|l| (p, l))) {
                    Some((prev, l)) => {
                        chunk = prev;
                        pos += l;
                    }
                    None => {
                        pos = 0;
                        break;
                    }
                }
            } else {
                let Some(l) = len(chunk) else {
                    pos = 0;
                    break;
                };
                if pos < l {
                    break;
                }
                match (chunk + 1 < n_chunks).then(|| len(chunk + 1)).flatten() {
                    Some(_) => {
                        pos -= l;
                        chunk += 1;
                    }
                    None => {
                        pos = l - 1;
                        break;
                    }
                }
            }
        }
        self.restart(chunk);
        if pos > 0 {
            self.start_offset = Some((chunk, StartAt::Piece(pos as usize)));
        }
    }

    fn tag(&self, k: usize) -> u64 {
        (self.id << 40) | ((self.epoch & 0xF_FFFF) << 20) | (k as u64 & 0xF_FFFF)
    }

    fn chunk_of_tag(&self, tag: u64) -> Option<usize> {
        let current = (self.id << 20) | (self.epoch & 0xF_FFFF);
        (tag >> 20 == current).then_some((tag & 0xF_FFFF) as usize)
    }

    fn status(&mut self, status: Status, detail: &str) {
        self.last_status = Some(status);
        self.ui.emit(UiEvent::Status { status, detail: detail.to_owned() });
    }
}

/// Marcas del motor de voz → qué palabra/fórmula resaltar y cuándo.
fn to_targets(script: &Script, marks: &[Mark]) -> Vec<(u32, Target)> {
    let mut out: Vec<(u32, Target)> = script.first().map(|t| (0, t)).into_iter().collect();
    for m in marks {
        if let Some(t) = script.locate(m.byte) {
            if out.last().map(|(_, last)| *last) != Some(t) {
                out.push((m.t_ms, t));
            }
        }
    }
    out
}
