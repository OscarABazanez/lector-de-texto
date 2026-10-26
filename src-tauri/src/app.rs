//! Integración con Tauri: bandeja, atajo global, ventanas y comandos de la interfaz.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Context;
use image::RgbaImage;
use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{
    AppHandle, Emitter, Manager, PhysicalPosition, PhysicalSize, RunEvent, WebviewUrl,
    WebviewWindowBuilder, WindowEvent, Wry,
};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};

use crate::capture::{self, FracRect};
use crate::config::{Config, Engine};
use crate::doc::Target;
use crate::lang::Lang;
use crate::llm::claude::{self, ClaudeClient};
use crate::llm::MathSpeaker;
use crate::pipeline::{self, Ctrl, Deps, Input, Reading, Settings, Ui, UiEvent};
use crate::player::{PlayItem, Player};
use crate::secrets::{self, KeySource};
use crate::tts::{Tts, VoiceInfo};

/// Captura en espera de que el usuario termine de seleccionar.
struct Selection {
    /// Identifica la captura en `shot://` (evita mostrar una anterior que quedó en caché).
    id: u64,
    /// La misma captura en JPEG: la ventana de selección la muestra congelada.
    jpeg: Arc<Vec<u8>>,
    image: RgbaImage,
    origin: (i32, i32),
    size: (u32, u32),
    scale: f64,
}

struct TrayItems {
    read: MenuItem<Wry>,
    system: CheckMenuItem<Wry>,
    edge: CheckMenuItem<Wry>,
    precise: CheckMenuItem<Wry>,
}

struct AppState {
    cfg: Mutex<Config>,
    llm: Mutex<Arc<ClaudeClient>>,
    math: Arc<MathSpeaker>,
    tts: Arc<Tts>,
    player: Arc<Player>,
    selection: Mutex<Option<Selection>>,
    reading: Mutex<Option<Reading>>,
    next_id: AtomicU64,
    tray: Mutex<Option<TrayItems>>,
}

struct TauriUi(AppHandle);

impl Ui for TauriUi {
    fn emit(&self, ev: UiEvent) {
        let _ = self.0.emit_to("reader", "reader:event", ev);
    }
}

pub fn run() {
    let cfg = Config::load();
    tauri::Builder::default()
        // La ventana de selección pide aquí la captura congelada: shot://localhost/<id>.
        .register_uri_scheme_protocol("shot", |ctx, request| {
            let want: Option<u64> = request.uri().path().trim_matches('/').parse().ok();
            let jpeg = ctx.app_handle().try_state::<AppState>().and_then(|st| {
                let sel = st.selection.lock().unwrap();
                sel.as_ref().filter(|s| Some(s.id) == want).map(|s| s.jpeg.clone())
            });
            let builder = tauri::http::Response::builder()
                .header("Cache-Control", "no-store")
                .header("Access-Control-Allow-Origin", "*");
            match jpeg {
                Some(bytes) => builder.header("Content-Type", "image/jpeg").body(bytes.as_ref().clone()),
                None => builder.status(404).body(Vec::new()),
            }
            .expect("respuesta HTTP")
        })
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            let _ = open_settings(app);
        }))
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, _shortcut, event| {
                    if event.state() == ShortcutState::Pressed {
                        begin_selection(app.clone());
                    }
                })
                .build(),
        )
        .setup(move |app| {
            let handle = app.handle().clone();
            let player = {
                let handle = handle.clone();
                Player::new(move |ev| {
                    if let Some(st) = handle.try_state::<AppState>() {
                        if let Some(r) = st.reading.lock().unwrap().as_ref() {
                            r.send(Ctrl::Player(ev));
                        }
                    }
                })
            };
            let llm = make_client(&cfg);
            app.manage(AppState {
                cfg: Mutex::new(cfg.clone()),
                llm: Mutex::new(llm.clone()),
                math: Arc::new(MathSpeaker::default()),
                tts: Arc::new(Tts::default()),
                player: Arc::new(player),
                selection: Mutex::new(None),
                reading: Mutex::new(None),
                next_id: AtomicU64::new(1),
                tray: Mutex::new(None),
            });
            // Las ventanas se crean aquí y no en tauri.conf.json: sus páginas llaman
            // comandos al cargar y el estado ya debe existir.
            create_windows(&handle, &cfg)?;
            build_tray(&handle, &cfg)?;
            if let Err(e) = app.global_shortcut().register(cfg.hotkey.as_str()) {
                eprintln!("no se pudo registrar el atajo {}: {e}", cfg.hotkey);
            }
            tauri::async_runtime::spawn(check_llm(handle, llm));
            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                if window.label() == "reader" {
                    api.prevent_close();
                    let _ = window.hide();
                    stop_reading(window.app_handle());
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            selection_done,
            selection_cancel,
            overlay_ready,
            ui_log,
            player_toggle,
            player_stop,
            player_seek,
            player_seek_words,
            player_seek_segment,
            set_rate,
            reader_close,
            reader_pin,
            get_config,
            save_config,
            list_voices,
            test_voice,
            llm_status,
            set_api_key,
            api_key_status,
        ])
        .build(tauri::generate_context!())
        .expect("error al iniciar Lector")
        .run(|_app, event| {
            // Sigue viva en la bandeja aunque no haya ventanas visibles.
            if let RunEvent::ExitRequested { code: None, api, .. } = event {
                api.prevent_exit();
            }
        });
}

/// Lector y overlay se crean ocultos al arrancar para que aparezcan al instante.
fn create_windows(app: &AppHandle, cfg: &Config) -> tauri::Result<()> {
    WebviewWindowBuilder::new(app, "reader", WebviewUrl::App("reader.html".into()))
        .title("Lector")
        .inner_size(560.0, 400.0)
        .min_inner_size(360.0, 220.0)
        .visible(false)
        .always_on_top(cfg.ui.always_on_top)
        .build()?;
    WebviewWindowBuilder::new(app, "overlay", WebviewUrl::App("overlay.html".into()))
        .title("Lector · selección")
        .inner_size(800.0, 600.0)
        .visible(false)
        .transparent(true)
        .decorations(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .resizable(false)
        .maximizable(false)
        .minimizable(false)
        .shadow(false)
        .focused(false)
        .build()?;
    Ok(())
}

// ------------------------------------------------------------------ bandeja

fn read_label(cfg: &Config) -> String {
    format!("Leer zona de pantalla ({})", cfg.hotkey)
}

fn build_tray(app: &AppHandle, cfg: &Config) -> tauri::Result<()> {
    let read = MenuItem::with_id(app, "read", read_label(cfg), true, None::<&str>)?;
    let system = CheckMenuItem::with_id(
        app,
        "engine_system",
        "Voces del sistema",
        true,
        cfg.tts.engine == Engine::System,
        None::<&str>,
    )?;
    let edge = CheckMenuItem::with_id(
        app,
        "engine_edge",
        "Voces neuronales de Edge (online)",
        true,
        cfg.tts.engine == Engine::Edge,
        None::<&str>,
    )?;
    let precise = CheckMenuItem::with_id(
        app,
        "precise",
        "Modo preciso (más lento)",
        true,
        cfg.llm.precise_mode,
        None::<&str>,
    )?;
    let settings = MenuItem::with_id(app, "settings", "Ajustes…", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Salir", true, None::<&str>)?;
    let menu = Menu::with_items(
        app,
        &[
            &read,
            &PredefinedMenuItem::separator(app)?,
            &system,
            &edge,
            &precise,
            &PredefinedMenuItem::separator(app)?,
            &settings,
            &quit,
        ],
    )?;
    let mut tray = TrayIconBuilder::with_id("main")
        .tooltip("Lector")
        .menu(&menu)
        // En macOS el icono de la barra de menús abre el menú, como las demás apps.
        .show_menu_on_left_click(cfg!(target_os = "macos"))
        .on_menu_event(|app, event| on_menu(app, event.id.as_ref()))
        .on_tray_icon_event(|tray, event| {
            if cfg!(target_os = "macos") {
                return;
            }
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                begin_selection(tray.app_handle().clone());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        tray = tray.icon(icon.clone());
    }
    tray.build(app)?;
    *app.state::<AppState>().tray.lock().unwrap() = Some(TrayItems { read, system, edge, precise });
    Ok(())
}

fn on_menu(app: &AppHandle, id: &str) {
    match id {
        "read" => begin_selection(app.clone()),
        "engine_system" => {
            update_config(app, |c| c.tts.engine = Engine::System);
        }
        "engine_edge" => {
            update_config(app, |c| c.tts.engine = Engine::Edge);
        }
        "precise" => {
            update_config(app, |c| c.llm.precise_mode = !c.llm.precise_mode);
        }
        "settings" => {
            if let Err(e) = open_settings(app) {
                eprintln!("ajustes: {e}");
            }
        }
        "quit" => {
            stop_reading(app);
            app.exit(0);
        }
        _ => {}
    }
}

fn sync_tray(app: &AppHandle, cfg: &Config) {
    if let Some(t) = app.state::<AppState>().tray.lock().unwrap().as_ref() {
        let _ = t.read.set_text(read_label(cfg));
        let _ = t.system.set_checked(cfg.tts.engine == Engine::System);
        let _ = t.edge.set_checked(cfg.tts.engine == Engine::Edge);
        let _ = t.precise.set_checked(cfg.llm.precise_mode);
    }
}

fn update_config(app: &AppHandle, f: impl FnOnce(&mut Config)) -> Config {
    let cfg = {
        let st = app.state::<AppState>();
        let mut c = st.cfg.lock().unwrap();
        f(&mut c);
        c.clone()
    };
    if let Err(e) = cfg.save() {
        eprintln!("no se pudo guardar la configuración: {e:#}");
    }
    sync_tray(app, &cfg);
    cfg
}

fn make_client(cfg: &Config) -> Arc<ClaudeClient> {
    Arc::new(ClaudeClient::new(&cfg.llm.model, secrets::get_api_key().map(|(k, _)| k)))
}

/// Revisa la key y el modelo y lo muestra en el tooltip de la bandeja.
async fn check_llm(app: AppHandle, llm: Arc<ClaudeClient>) {
    let tip = match llm.check().await {
        Ok(()) => format!("Lector · Claude listo ({})", llm.model()),
        Err(e) if claude::is_unreachable(&e) => "Lector · sin conexión con Anthropic".to_owned(),
        Err(e) => format!("Lector · {e}"),
    };
    eprintln!("{tip}");
    if let Some(tray) = app.tray_by_id("main") {
        let _ = tray.set_tooltip(Some(tip));
    }
}

fn open_settings(app: &AppHandle) -> tauri::Result<()> {
    if let Some(w) = app.get_webview_window("settings") {
        w.show()?;
        return w.set_focus();
    }
    WebviewWindowBuilder::new(app, "settings", WebviewUrl::App("settings.html".into()))
        .title("Ajustes · Lector")
        .inner_size(540.0, 680.0)
        .min_inner_size(420.0, 480.0)
        .build()?;
    Ok(())
}

// ------------------------------------------------------- selección y lectura

fn begin_selection(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        if let Err(e) = show_overlay(&app).await {
            eprintln!("selección: {e:#}");
            let hint = if cfg!(target_os = "macos") {
                " En macOS da permiso en Ajustes del Sistema → Privacidad y seguridad → Grabación de pantalla."
            } else {
                ""
            };
            if let Some(reader) = app.get_webview_window("reader") {
                let _ = reader.show();
            }
            let msg = format!("No se pudo capturar la pantalla: {e:#}.{hint}");
            let _ = app.emit_to("reader", "reader:event", UiEvent::Notice { msg });
        }
    });
}

async fn show_overlay(app: &AppHandle) -> anyhow::Result<()> {
    let overlay = app.get_webview_window("overlay").context("falta la ventana de selección")?;
    if overlay.is_visible()? {
        return Ok(());
    }
    stop_reading(app);
    if let Some(reader) = app.get_webview_window("reader") {
        if reader.is_visible()? {
            reader.hide()?;
            // Da tiempo a que Windows repinte lo que estaba debajo antes de capturar.
            tokio::time::sleep(Duration::from_millis(80)).await;
        }
    }

    // Mientras el usuario selecciona, se abren las conexiones con Anthropic y el motor de voz.
    {
        let st = app.state::<AppState>();
        let (tts, engine) = (st.tts.clone(), st.cfg.lock().unwrap().tts.engine);
        let llm = st.llm.lock().unwrap().clone();
        tauri::async_runtime::spawn(async move { tts.prepare(engine).await });
        tauri::async_runtime::spawn(async move { llm.warmup().await });
    }

    let cursor = app.cursor_position()?;
    let monitor = match app.monitor_from_point(cursor.x, cursor.y)? {
        Some(m) => m,
        None => app.primary_monitor()?.context("no hay monitores")?,
    };
    let (cx, cy) = (cursor.x as i32, cursor.y as i32);
    let t0 = Instant::now();
    let (image, jpeg) = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
        let image = capture::grab_monitor_at(cx, cy)?;
        let jpeg = capture::to_jpeg(&image)?;
        Ok((image, jpeg))
    })
    .await??;
    eprintln!(
        "captura {}x{} ({} KB en JPEG) en {} ms",
        image.width(),
        image.height(),
        jpeg.len() / 1024,
        t0.elapsed().as_millis()
    );

    let pos = *monitor.position();
    let size = *monitor.size();
    let id = app.state::<AppState>().next_id.fetch_add(1, Ordering::Relaxed);
    *app.state::<AppState>().selection.lock().unwrap() = Some(Selection {
        id,
        jpeg: Arc::new(jpeg),
        image,
        origin: (pos.x, pos.y),
        size: (size.width, size.height),
        scale: monitor.scale_factor(),
    });
    overlay.set_position(PhysicalPosition::new(pos.x, pos.y))?;
    overlay.set_size(PhysicalSize::new(size.width, size.height))?;
    // Repetido: al cambiar de monitor con otro DPI Windows puede reajustar la ventana.
    overlay.set_position(PhysicalPosition::new(pos.x, pos.y))?;
    let payload = serde_json::json!({
        "shot": id,
        "x": (cursor.x - f64::from(pos.x)) / f64::from(size.width.max(1)),
        "y": (cursor.y - f64::from(pos.y)) / f64::from(size.height.max(1)),
    });
    // La ventana carga la captura y llama a `overlay_ready`; se muestra entonces, ya congelada.
    // Si la imagen no carga a tiempo se muestra igual (transparente, sobre la pantalla en vivo).
    overlay.emit_to("overlay", "overlay:show", payload)?;
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_millis(1500)).await;
        let pending = app.state::<AppState>().selection.lock().unwrap().as_ref().is_some_and(|s| s.id == id);
        let hidden = app.get_webview_window("overlay").is_some_and(|o| !o.is_visible().unwrap_or(true));
        if pending && hidden {
            eprintln!("la captura congelada no cargó a tiempo; selección sobre la pantalla en vivo");
            reveal_overlay(&app);
        }
    });
    Ok(())
}

fn reveal_overlay(app: &AppHandle) {
    if let Some(overlay) = app.get_webview_window("overlay") {
        if !overlay.is_visible().unwrap_or(true) {
            let _ = overlay.show();
            let _ = overlay.set_always_on_top(true);
            let _ = overlay.set_focus();
        }
    }
}

/// Coloca el lector junto a la selección sin taparla, dentro del mismo monitor.
fn place_reader(app: &AppHandle, sel: &Selection, r: FracRect) {
    let Some(reader) = app.get_webview_window("reader") else { return };
    let (mw, mh) = (f64::from(sel.size.0), f64::from(sel.size.1));
    let size = reader.outer_size().unwrap_or(PhysicalSize::new(
        (560.0 * sel.scale) as u32,
        (400.0 * sel.scale) as u32,
    ));
    let (rw, rh) = (f64::from(size.width), f64::from(size.height));
    let (sx, sy, sw, sh) = (r.x * mw, r.y * mh, r.w * mw, r.h * mh);
    let gap = 12.0 * sel.scale;
    let (x, y) = if sx + sw + gap + rw <= mw {
        (sx + sw + gap, sy)
    } else if sx - gap - rw >= 0.0 {
        (sx - gap - rw, sy)
    } else if sy + sh + gap + rh <= mh {
        (sx, sy + sh + gap)
    } else {
        (sx, sy - gap - rh)
    };
    let x = x.clamp(0.0, (mw - rw).max(0.0));
    let y = y.clamp(0.0, (mh - rh).max(0.0));
    let _ = reader.set_position(PhysicalPosition::new(sel.origin.0 + x as i32, sel.origin.1 + y as i32));
    let _ = reader.show();
    let _ = reader.set_focus();
}

fn start_reading(app: &AppHandle, input: Input) {
    stop_reading(app);
    let st = app.state::<AppState>();
    let cfg = st.cfg.lock().unwrap().clone();
    let settings = Settings {
        engine: cfg.tts.engine,
        voices: cfg.tts.clone(),
        rate: cfg.tts.rate,
        precise: cfg.llm.precise_mode,
        adaptive: cfg.llm.adaptive,
        default_lang: cfg.ui.default_language,
        math_parts: cfg.ui.math_parts,
    };
    let ui: Arc<dyn Ui> = Arc::new(TauriUi(app.clone()));
    ui.emit(UiEvent::Reset);
    let deps = Deps {
        llm: st.llm.lock().unwrap().clone(),
        math: st.math.clone(),
        tts: st.tts.clone(),
        player: st.player.clone(),
    };
    let id = st.next_id.fetch_add(1, Ordering::Relaxed);
    let reading = pipeline::start(id, deps, ui, settings, input);
    *st.reading.lock().unwrap() = Some(reading);
}

fn stop_reading(app: &AppHandle) {
    let st = app.state::<AppState>();
    // Soltar la lectura la cancela (ver `Drop for Reading`).
    let old = st.reading.lock().unwrap().take();
    drop(old);
    st.player.clear();
}

fn with_reading(app: &AppHandle, c: Ctrl) {
    if let Some(r) = app.state::<AppState>().reading.lock().unwrap().as_ref() {
        r.send(c);
    }
}

// ----------------------------------------------------------------- comandos

#[tauri::command]
async fn selection_done(app: AppHandle, rect: FracRect) -> Result<(), String> {
    if let Some(o) = app.get_webview_window("overlay") {
        let _ = o.hide();
    }
    let sel = app.state::<AppState>().selection.lock().unwrap().take();
    let Some(sel) = sel else { return Ok(()) };
    let Some(img) = capture::crop(&sel.image, rect) else { return Ok(()) };
    eprintln!("selección {}x{}", img.width(), img.height());
    place_reader(&app, &sel, rect);
    if capture::is_blank(&img) {
        // No se manda a Claude: sólo leería «pantalla negra».
        stop_reading(&app);
        let ui = TauriUi(app.clone());
        ui.emit(UiEvent::Reset);
        ui.emit(UiEvent::Status {
            status: pipeline::Status::Error,
            detail: "La zona seleccionada salió negra. Si es un video, el sitio lo protege contra \
                     capturas de pantalla (DRM) y Windows no deja leerlo. Usa las diapositivas o la \
                     transcripción del curso (en Coursera: «Descargas» o «Transcripción» bajo el video)."
                .into(),
        });
        return Ok(());
    }
    start_reading(&app, Input::Image(img));
    Ok(())
}

#[tauri::command]
fn selection_cancel(app: AppHandle) {
    if let Some(o) = app.get_webview_window("overlay") {
        let _ = o.hide();
    }
    app.state::<AppState>().selection.lock().unwrap().take();
    eprintln!("selección cancelada");
}

/// Mensajes de diagnóstico de las ventanas (van al registro).
#[tauri::command]
fn ui_log(msg: String) {
    eprintln!("{msg}");
}

/// La ventana de selección ya tiene la captura congelada (`frozen`) o no pudo cargarla.
#[tauri::command]
fn overlay_ready(app: AppHandle, id: u64, frozen: bool) {
    let current = app.state::<AppState>().selection.lock().unwrap().as_ref().is_some_and(|s| s.id == id);
    if current {
        if !frozen {
            eprintln!("no se pudo mostrar la captura congelada; selección sobre la pantalla en vivo");
        }
        reveal_overlay(&app);
    }
}

#[tauri::command]
fn player_toggle(app: AppHandle) {
    with_reading(&app, Ctrl::Toggle);
}

#[tauri::command]
fn player_stop(app: AppHandle) {
    with_reading(&app, Ctrl::Stop);
}

#[tauri::command]
fn player_seek(app: AppHandle, delta: i32) {
    with_reading(&app, Ctrl::Seek(delta));
}

#[tauri::command]
fn player_seek_words(app: AppHandle, delta: i32) {
    with_reading(&app, Ctrl::SeekWords(delta));
}

#[tauri::command]
fn player_seek_segment(app: AppHandle, seg: usize, word: Option<usize>) {
    with_reading(&app, Ctrl::SeekSegment(seg, word));
}

#[tauri::command]
fn set_rate(app: AppHandle, rate: f32) {
    let rate = rate.clamp(0.5, 3.0);
    update_config(&app, |c| c.tts.rate = rate);
    with_reading(&app, Ctrl::SetRate(rate));
}

#[tauri::command]
fn reader_close(app: AppHandle) {
    if let Some(w) = app.get_webview_window("reader") {
        let _ = w.hide();
    }
    stop_reading(&app);
}

#[tauri::command]
fn reader_pin(app: AppHandle, on: bool) {
    if let Some(w) = app.get_webview_window("reader") {
        let _ = w.set_always_on_top(on);
    }
    update_config(&app, |c| c.ui.always_on_top = on);
}

#[tauri::command]
fn get_config(app: AppHandle) -> Config {
    app.state::<AppState>().cfg.lock().unwrap().clone()
}

#[tauri::command]
fn save_config(app: AppHandle, cfg: Config) -> Result<(), String> {
    let st = app.state::<AppState>();
    let old = st.cfg.lock().unwrap().clone();
    if cfg.hotkey != old.hotkey {
        let gs = app.global_shortcut();
        gs.register(cfg.hotkey.as_str())
            .map_err(|e| format!("El atajo «{}» no es válido o ya está en uso: {e}", cfg.hotkey))?;
        let _ = gs.unregister(old.hotkey.as_str());
    }
    if cfg.llm.model != old.llm.model {
        let llm = make_client(&cfg);
        *st.llm.lock().unwrap() = llm.clone();
        tauri::async_runtime::spawn(check_llm(app.clone(), llm));
    }
    if cfg.ui.always_on_top != old.ui.always_on_top {
        if let Some(w) = app.get_webview_window("reader") {
            let _ = w.set_always_on_top(cfg.ui.always_on_top);
        }
    }
    *st.cfg.lock().unwrap() = cfg.clone();
    cfg.save().map_err(|e| format!("No se pudo guardar: {e:#}"))?;
    sync_tray(&app, &cfg);
    Ok(())
}

#[tauri::command]
async fn list_voices(app: AppHandle, engine: Engine) -> Result<Vec<VoiceInfo>, String> {
    let tts = app.state::<AppState>().tts.clone();
    tts.voices(engine).await.map_err(|e| format!("{e:#}"))
}

#[tauri::command]
async fn test_voice(app: AppHandle, engine: Engine, voice: String, lang: Lang) -> Result<(), String> {
    let text = match lang {
        Lang::Es => "Hola, así suena esta voz. La raíz cuadrada de x es igual a dos.",
        Lang::En => "Hello, this is how this voice sounds. The square root of x equals two.",
    };
    stop_reading(&app);
    let (tts, player, rate) = {
        let st = app.state::<AppState>();
        let rate = st.cfg.lock().unwrap().tts.rate;
        (st.tts.clone(), st.player.clone(), rate)
    };
    let utt = tts
        .synthesize(engine, &voice, lang, rate, text)
        .await
        .map_err(|e| format!("{e:#}"))?;
    let marks: Vec<(u32, Target)> = Vec::new();
    player.enqueue(PlayItem { tag: 0, audio: utt.audio, marks, end_ms: utt.end_ms });
    Ok(())
}

#[tauri::command]
async fn llm_status(model: String) -> Result<String, String> {
    let llm = ClaudeClient::new(&model, secrets::get_api_key().map(|(k, _)| k));
    match llm.check().await {
        Ok(()) => Ok(format!("Conectado: {model} está disponible y acepta imágenes.")),
        Err(e) if claude::is_unreachable(&e) => Err("Sin conexión con Anthropic.".to_owned()),
        Err(e) => Err(e.to_string()),
    }
}

#[derive(serde::Serialize)]
struct KeyStatus {
    present: bool,
    source: Option<KeySource>,
    /// Últimos 4 caracteres, para reconocerla sin mostrarla.
    hint: String,
}

#[tauri::command]
fn api_key_status() -> KeyStatus {
    match secrets::get_api_key() {
        Some((key, source)) => KeyStatus {
            present: true,
            source: Some(source),
            hint: key.chars().rev().take(4).collect::<Vec<_>>().into_iter().rev().collect(),
        },
        None => KeyStatus { present: false, source: None, hint: String::new() },
    }
}

/// Guarda (o borra, si viene vacía) la key en el almacén de credenciales y reconecta.
#[tauri::command]
fn set_api_key(app: AppHandle, key: String) -> Result<KeyStatus, String> {
    let key = key.trim();
    if key.is_empty() {
        secrets::clear_api_key()
    } else {
        secrets::set_api_key(key)
    }
    .map_err(|e| format!("No se pudo guardar la API key: {e:#}"))?;
    let st = app.state::<AppState>();
    let llm = make_client(&st.cfg.lock().unwrap());
    *st.llm.lock().unwrap() = llm.clone();
    tauri::async_runtime::spawn(check_llm(app.clone(), llm));
    Ok(api_key_status())
}
