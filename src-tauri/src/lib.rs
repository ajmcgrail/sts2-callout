mod audio;
mod data;
mod matcher;
mod settings;
pub mod stt;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use futures_util::StreamExt;
use parking_lot::{Mutex, RwLock};
use serde::Serialize;
use tauri::http::{Response, StatusCode};
use tauri::{AppHandle, Emitter, Manager, State, WindowEvent};

use data::{Card, CachedImage, GameData, ImageKind, Relic};
use matcher::{ItemRef, Kind, Match, Matcher};
use settings::Settings;

pub struct AppState {
    pub settings: Mutex<Settings>,
    pub data: RwLock<Option<Arc<GameData>>>,
    pub matcher: RwLock<Option<Arc<Matcher>>>,
    listen: Mutex<Listen>,
    listen_gen: AtomicU64,
    loading_data: AtomicBool,
    prefetch_gen: AtomicU64,
    client: reqwest::Client,
    cache_dir: PathBuf,
    models_dir: PathBuf,
    settings_path: PathBuf,
}

impl AppState {
    fn rebuild_matcher(&self) {
        let data = self.data.read().clone();
        let m = data.map(|d| Arc::new(Matcher::new(&d, &self.settings.lock())));
        *self.matcher.write() = m;
    }
}

/// Listening state. Owned by the backend; the UI only renders `listen-state` events.
enum Listen {
    Idle,
    /// Downloading / loading models. Setting `cancel` aborts the start.
    Starting { cancel: Arc<AtomicBool> },
    Listening { gen: u64, listener: stt::Listener },
}

#[derive(Clone, Serialize)]
struct ListenState {
    phase: &'static str,
    label: Option<String>,
    /// Why we went back to idle, if it wasn't the user's doing.
    error: Option<String>,
}

impl Listen {
    fn view(&self, error: Option<String>) -> ListenState {
        let (phase, label) = match self {
            Listen::Idle => ("idle", None),
            Listen::Starting { .. } => ("starting", None),
            Listen::Listening { listener, .. } => ("listening", Some(listener.device_label.clone())),
        };
        ListenState { phase, label, error }
    }
}

fn emit_listen(app: &AppHandle, view: ListenState) {
    let _ = app.emit("listen-state", view);
}

/// Called by the speech thread when it ends. Only resets the state if it is
/// still the current listener (a stopped listener can finish after a restart).
pub(crate) fn listener_exited(app: &AppHandle, gen: u64, error: Option<String>) {
    let state = app.state::<AppState>();
    let mut listen = state.listen.lock();
    if matches!(&*listen, Listen::Listening { gen: g, .. } if *g == gen) {
        *listen = Listen::Idle;
        let view = listen.view(error);
        drop(listen);
        emit_listen(app, view);
    }
}

type CmdResult<T> = Result<T, String>;

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

#[derive(Clone, Serialize)]
struct ShowPayload<'a> {
    kind: Kind,
    id: &'a str,
    heard: &'a str,
    card: Option<&'a Card>,
    relic: Option<&'a Relic>,
    display_secs: f32,
    max_onscreen: usize,
    scale: f32,
}

/// Send an item to the overlay. Returns false if it isn't in the data.
pub(crate) fn emit_show(app: &AppHandle, item: &ItemRef, heard: &str) -> bool {
    let state = app.state::<AppState>();
    let Some(data) = state.data.read().clone() else { return false };
    let s = state.settings.lock().clone();
    let (card, relic) = match item.kind {
        Kind::Card => (data.card(&item.id), None),
        Kind::Relic => (None, data.relic(&item.id)),
    };
    if card.is_none() && relic.is_none() {
        return false;
    }
    let payload = ShowPayload {
        kind: item.kind,
        id: &item.id,
        heard,
        card,
        relic,
        display_secs: s.display_secs,
        max_onscreen: s.max_onscreen.max(1),
        scale: s.overlay_scale,
    };
    app.emit("show-item", payload).is_ok()
}

#[derive(Serialize)]
struct Status {
    data_loaded: bool,
    cards: usize,
    relics: usize,
    fetched_at: u64,
    source: String,
    listen: ListenState,
    cache_dir: String,
    model_present: bool,
    models: Vec<stt::ModelInfo>,
}

#[tauri::command]
fn get_status(state: State<'_, AppState>) -> Status {
    let data = state.data.read().clone();
    let model = state.settings.lock().model.clone();
    let listen = state.listen.lock().view(None);
    Status {
        data_loaded: data.is_some(),
        cards: data.as_ref().map_or(0, |d| d.cards.len()),
        relics: data.as_ref().map_or(0, |d| d.relics.len()),
        fetched_at: data.as_ref().map_or(0, |d| d.meta.fetched_at),
        source: data.as_ref().map_or_else(String::new, |d| d.meta.source.clone()),
        listen,
        cache_dir: state.cache_dir.display().to_string(),
        model_present: stt::model_path(&state.models_dir, &model).exists(),
        models: stt::MODELS.to_vec(),
    }
}

#[tauri::command]
async fn list_devices() -> CmdResult<Vec<audio::DeviceInfo>> {
    tauri::async_runtime::spawn_blocking(audio::list_devices).await.map_err(err)
}

#[tauri::command]
fn get_settings(state: State<'_, AppState>) -> Settings {
    state.settings.lock().clone()
}

#[tauri::command]
fn save_settings(app: AppHandle, state: State<'_, AppState>, settings: Settings) -> CmdResult<Settings> {
    let settings = settings.sanitized();
    settings.save(&state.settings_path).map_err(err)?;
    let old = std::mem::replace(&mut *state.settings.lock(), settings.clone());
    state.rebuild_matcher();
    place_overlay(&app);
    let restart_needed = old.model != settings.model || old.device != settings.device || old.use_vad != settings.use_vad;
    if restart_needed && !matches!(&*state.listen.lock(), Listen::Idle) {
        let _ = app.emit("status", "Stop and start listening again to apply the new audio/model settings");
    }
    Ok(settings)
}

#[tauri::command]
async fn refresh_data(app: AppHandle) -> CmdResult<usize> {
    load_data(&app, true).await.map_err(err)
}

#[tauri::command]
async fn start_listening(app: AppHandle, state: State<'_, AppState>) -> CmdResult<String> {
    let cancel = {
        let mut listen = state.listen.lock();
        match &*listen {
            Listen::Listening { listener, .. } => return Ok(listener.device_label.clone()),
            Listen::Starting { .. } => return Err("already starting".into()),
            Listen::Idle => {}
        }
        let cancel = Arc::new(AtomicBool::new(false));
        *listen = Listen::Starting { cancel: cancel.clone() };
        cancel
    };
    emit_listen(&app, ListenState { phase: "starting", label: None, error: None });

    let gen = state.listen_gen.fetch_add(1, Ordering::SeqCst) + 1;
    let result = start_inner(&app, &state, gen, &cancel).await;

    let mut listen = state.listen.lock();
    // stop_listening already moved us to Idle if the user cancelled.
    let still_ours = matches!(&*listen, Listen::Starting { cancel: c } if Arc::ptr_eq(c, &cancel));
    match result {
        Ok(listener) if still_ours && !cancel.load(Ordering::SeqCst) => {
            let label = listener.device_label.clone();
            *listen = Listen::Listening { gen, listener };
            let view = listen.view(None);
            drop(listen);
            emit_listen(&app, view);
            Ok(label)
        }
        Ok(listener) => {
            listener.stop();
            Err("cancelled".into())
        }
        Err(e) => {
            let msg = format!("{e:#}");
            if still_ours {
                *listen = Listen::Idle;
                let view = listen.view(Some(msg.clone()));
                drop(listen);
                emit_listen(&app, view);
            }
            Err(msg)
        }
    }
}

async fn start_inner(app: &AppHandle, state: &AppState, gen: u64, cancel: &AtomicBool) -> anyhow::Result<stt::Listener> {
    let s = state.settings.lock().clone();
    let model = stt::model_path(&state.models_dir, &s.model);
    if !model.exists() {
        let _ = app.emit("status", format!("Downloading speech model {} (one time)…", s.model));
        download_model(app, state, &stt::model_url(&s.model), &model, cancel).await?;
    }
    let vad = stt::vad_path(&state.models_dir);
    if s.use_vad && !vad.exists() {
        // Small (~1 MB); without it recognition falls back to fixed chunks.
        if let Err(e) = download_model(app, state, &stt::vad_url(), &vad, cancel).await {
            log::warn!("VAD model download failed: {e:#}");
        }
    }
    if cancel.load(Ordering::SeqCst) {
        anyhow::bail!("cancelled");
    }
    let vad = (s.use_vad && vad.exists()).then_some(vad);
    let app2 = app.clone();
    let listener = tauri::async_runtime::spawn_blocking(move || {
        stt::spawn(app2, gen, &model, vad.as_deref(), s.device)
    })
    .await??;
    Ok(listener)
}

async fn download_model(
    app: &AppHandle,
    state: &AppState,
    url: &str,
    dest: &std::path::Path,
    cancel: &AtomicBool,
) -> anyhow::Result<()> {
    let app2 = app.clone();
    let mut last = 0u64;
    data::download_file(&state.client, url, dest, cancel, move |done, total| {
        if done - last > 2_000_000 || Some(done) == total {
            last = done;
            let _ = app2.emit("model-progress", (done, total));
        }
    })
    .await
}

/// Stops listening, or cancels a start that is still downloading/loading.
#[tauri::command]
fn stop_listening(app: AppHandle, state: State<'_, AppState>) {
    let previous = std::mem::replace(&mut *state.listen.lock(), Listen::Idle);
    match previous {
        Listen::Starting { cancel } => cancel.store(true, Ordering::SeqCst),
        Listen::Listening { listener, .. } => listener.stop(),
        Listen::Idle => {}
    }
    emit_listen(&app, ListenState { phase: "idle", label: None, error: None });
}

/// Run the matcher on typed text, as if it had been heard.
#[tauri::command]
fn test_text(app: AppHandle, state: State<'_, AppState>, text: String) -> Vec<Match> {
    let Some(m) = state.matcher.read().clone() else { return Vec::new() };
    let found = m.find(&text);
    for f in &found {
        emit_show(&app, &f.item, &f.heard);
    }
    found
}

#[derive(Serialize)]
struct SearchHit {
    kind: Kind,
    id: String,
    name: String,
    detail: String,
}

#[tauri::command]
fn search(state: State<'_, AppState>, query: String) -> Vec<SearchHit> {
    let Some(data) = state.data.read().clone() else { return Vec::new() };
    let q = matcher::normalize(&query);
    if q.is_empty() {
        return Vec::new();
    }
    let cards = data.cards.iter().filter(|c| matcher::normalize(&c.name).contains(&q)).map(|c| SearchHit {
        kind: Kind::Card,
        id: c.id.clone(),
        name: c.name.clone(),
        detail: format!("{} {} · {}", c.rarity, c.card_type, c.color),
    });
    let relics = data.relics.iter().filter(|r| matcher::normalize(&r.name).contains(&q)).map(|r| SearchHit {
        kind: Kind::Relic,
        id: r.id.clone(),
        name: r.name.clone(),
        detail: format!("{} · {}", r.rarity, r.pool),
    });
    cards.chain(relics).take(40).collect()
}

#[tauri::command]
fn show_item(app: AppHandle, kind: Kind, id: String) -> bool {
    emit_show(&app, &ItemRef { kind, id }, "")
}

async fn load_data(app: &AppHandle, force: bool) -> anyhow::Result<usize> {
    let state = app.state::<AppState>();
    if state.loading_data.swap(true, Ordering::SeqCst) {
        anyhow::bail!("game data is already being downloaded");
    }
    let result = load_data_inner(app, &state, force).await;
    state.loading_data.store(false, Ordering::SeqCst);
    result
}

async fn load_data_inner(app: &AppHandle, state: &AppState, force: bool) -> anyhow::Result<usize> {
    let _ = app.emit("status", if force { "Re-downloading game data…" } else { "Loading game data…" });
    let data = Arc::new(data::load_or_fetch(&state.client, &state.cache_dir, force).await?);
    let n = data.cards.len() + data.relics.len();
    *state.data.write() = Some(data.clone());
    state.rebuild_matcher();
    let _ = app.emit("data-ready", n);
    tauri::async_runtime::spawn(prefetch_images(app.clone(), data));
    Ok(n)
}

/// Warm the image cache in the background so every popup is instant and
/// nothing is downloaded more than once.
async fn prefetch_images(app: AppHandle, data: Arc<GameData>) {
    let state = app.state::<AppState>();
    let gen = state.prefetch_gen.fetch_add(1, Ordering::SeqCst) + 1;
    let all = data.all_images();
    let total = all.len();
    let client = state.client.clone();
    let cache = state.cache_dir.clone();
    let mut done = 0usize;
    let mut failed = 0usize;
    let mut results = futures_util::stream::iter(all)
        .map(|(kind, id)| {
            let (client, cache, data) = (client.clone(), cache.clone(), data.clone());
            async move { data::get_image(&client, &cache, &data, kind, &id).await }
        })
        .buffer_unordered(4);
    while let Some(r) = results.next().await {
        if state.prefetch_gen.load(Ordering::SeqCst) != gen {
            return; // superseded by a data refresh
        }
        done += 1;
        if r.is_err() {
            failed += 1;
        }
        if done.is_multiple_of(25) || done == total {
            let _ = app.emit("prefetch", (done, total, failed));
        }
    }
}

async fn serve_image(app: &AppHandle, path: &str) -> Response<Vec<u8>> {
    let respond = |code: StatusCode, mime: &str, body: Vec<u8>| {
        Response::builder()
            .status(code)
            .header("Content-Type", mime)
            .header("Access-Control-Allow-Origin", "*")
            .header("Cache-Control", if code == StatusCode::OK { "max-age=31536000" } else { "no-store" })
            .body(body)
            .unwrap()
    };
    let mut parts = path.trim_start_matches('/').splitn(2, '/');
    let (Some(kind), Some(id)) = (parts.next().and_then(ImageKind::parse), parts.next()) else {
        return respond(StatusCode::BAD_REQUEST, "text/plain", b"bad path".to_vec());
    };
    let state = app.state::<AppState>();
    let Some(data) = state.data.read().clone() else {
        return respond(StatusCode::SERVICE_UNAVAILABLE, "text/plain", b"no data".to_vec());
    };
    match data::get_image(&state.client, &state.cache_dir, &data, kind, id).await {
        Ok(CachedImage::Found(p)) => match tokio::fs::read(&p).await {
            Ok(bytes) => respond(StatusCode::OK, data::mime_for(&p), bytes),
            Err(e) => respond(StatusCode::INTERNAL_SERVER_ERROR, "text/plain", e.to_string().into_bytes()),
        },
        Ok(CachedImage::Missing) => respond(StatusCode::NOT_FOUND, "text/plain", b"missing".to_vec()),
        Err(e) => respond(StatusCode::BAD_GATEWAY, "text/plain", e.to_string().into_bytes()),
    }
}

/// Size the click-through overlay to a column on one side of the main screen.
fn place_overlay(app: &AppHandle) {
    let Some(win) = app.get_webview_window("overlay") else { return };
    let s = app.state::<AppState>().settings.lock().clone();
    let monitor = win.primary_monitor().ok().flatten().or_else(|| win.current_monitor().ok().flatten());
    if let Some(m) = monitor {
        let sf = m.scale_factor();
        let (mw, mh) = (m.size().width as f64, m.size().height as f64);
        let width = (700.0 * s.overlay_scale as f64 * sf).min(mw);
        let height = mh - 80.0 * sf;
        let x = if s.overlay_side == "left" { m.position().x as f64 + 20.0 * sf } else { m.position().x as f64 + mw - width - 20.0 * sf };
        let _ = win.set_size(tauri::PhysicalSize::new(width, height));
        let _ = win.set_position(tauri::PhysicalPosition::new(x, m.position().y as f64 + 40.0 * sf));
    }
    let _ = win.set_ignore_cursor_events(true);
    let _ = win.set_always_on_top(true);
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info,whisper_rs=warn")).init();

    tauri::Builder::default()
        .register_asynchronous_uri_scheme_protocol("stsimg", |ctx, request, responder| {
            let app = ctx.app_handle().clone();
            let path = request.uri().path().to_string();
            tauri::async_runtime::spawn(async move {
                responder.respond(serve_image(&app, &path).await);
            });
        })
        .setup(|app| {
            let paths = app.path();
            let cache_dir = paths.app_local_data_dir()?;
            let settings_path = paths.app_config_dir()?.join("settings.json");
            let models_dir = cache_dir.join("models");
            std::fs::create_dir_all(&cache_dir)?;
            log::info!("cache directory: {}", cache_dir.display());
            app.manage(AppState {
                settings: Mutex::new(Settings::load(&settings_path)),
                data: RwLock::new(None),
                matcher: RwLock::new(None),
                listen: Mutex::new(Listen::Idle),
                listen_gen: AtomicU64::new(0),
                loading_data: AtomicBool::new(false),
                prefetch_gen: AtomicU64::new(0),
                client: data::http_client(),
                cache_dir,
                models_dir,
                settings_path,
            });
            place_overlay(app.handle());

            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                if let Err(e) = load_data(&handle, false).await {
                    log::error!("loading data: {e:#}");
                    let _ = handle.emit("status", format!("Could not load game data: {e:#}"));
                }
            });
            Ok(())
        })
        .on_window_event(|window, event| {
            // Closing the control panel quits the whole app (overlay included).
            if window.label() == "main" && matches!(event, WindowEvent::CloseRequested { .. }) {
                window.app_handle().exit(0);
            }
        })
        .invoke_handler(tauri::generate_handler![
            get_status,
            list_devices,
            get_settings,
            save_settings,
            refresh_data,
            start_listening,
            stop_listening,
            test_text,
            search,
            show_item,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
