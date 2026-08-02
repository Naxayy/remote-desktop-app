//! Backend de agent-ui: icono de bandeja + ventana de configuracion
//! del agent. Corre en la sesion del usuario logueado (lanzado por el
//! servicio via CreateProcessAsUser), asi que tiene acceso real al
//! escritorio para capturar pantalla.
//!
//! La logica de red/captura es la MISMA que la del `agent` en modo
//! consola (mismo core_engine por debajo) - la diferencia es que aca
//! esta envuelta en comandos Tauri para poder arrancar/parar/cambiar
//! configuracion desde la ventana, y hay un icono de bandeja que
//! muestra/oculta esa ventana.
//!
//! Nota: no se pudo compilar (Tauri necesita WebView, no disponible en
//! este entorno de desarrollo) - es zona de alto riesgo de necesitar
//! ajustes, igual que controller-ui al principio.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use core_engine::capture::ScreenCapturer;
use core_engine::crypto::{self, SessionCipher};
use core_engine::encode::VideoEncoder;
use core_engine::input::{InputInjector, MouseButton};
use core_engine::netproto::{self, ControlEvent, InputEvent, OwnedFileEvent};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::fs::File;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::thread;
use std::time::Duration;
use tauri::menu::{MenuBuilder, MenuItemBuilder};
use tauri::tray::{MouseButton as TrayMouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::{mpsc, Mutex};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;
use x25519_dalek::EphemeralSecret;

const QUALITY_INITIAL: u8 = 50;
const QUALITY_MIN: u8 = 20;
const QUALITY_MAX: u8 = 75;

// ---------------------------------------------------------------
// Configuracion: servidor + codigo, guardados en un archivo editable
// por el usuario (no hace falta ser administrador para escribir ahi,
// a diferencia de ProgramData).
// ---------------------------------------------------------------

#[derive(Serialize, Deserialize, Clone)]
struct AgentConfig {
    signaling_url: String,
    agent_code: String,
}

fn config_path() -> PathBuf {
    let dir = std::env::var("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
        .join("RemoteDesktopAppAgent");
    let _ = std::fs::create_dir_all(&dir);
    dir.join("config.json")
}

fn load_config() -> AgentConfig {
    if let Ok(text) = std::fs::read_to_string(config_path()) {
        if let Ok(cfg) = serde_json::from_str::<AgentConfig>(&text) {
            return cfg;
        }
    }
    // No hay config guardada todavia - usamos lo que haya dejado el
    // instalador como variables de entorno, o defaults razonables.
    AgentConfig {
        signaling_url: std::env::var("SIGNALING_URL")
            .unwrap_or_else(|_| "ws://127.0.0.1:8080".to_string()),
        agent_code: std::env::var("AGENT_CODE").unwrap_or_else(|_| "123456".to_string()),
    }
}

fn persist_config(cfg: &AgentConfig) -> std::io::Result<()> {
    let text = serde_json::to_string_pretty(cfg)?;
    std::fs::write(config_path(), text)
}

struct AppState {
    config: Mutex<AgentConfig>,
    connection_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

// ---------------------------------------------------------------
// Todo lo de aca abajo es prácticamente identico a agent/src/main.rs
// (captura, cifrado, input, archivos, P2P) - el mismo motor, nomas
// que ahora corre dentro de un comando Tauri en vez de un binario
// standalone.
// ---------------------------------------------------------------

#[derive(Default)]
struct CryptoState {
    my_secret: StdMutex<Option<EphemeralSecret>>,
    cipher: StdMutex<Option<Arc<SessionCipher>>>,
}

fn wrap_outgoing(crypto: &CryptoState, plaintext: Vec<u8>) -> Vec<u8> {
    let guard = crypto.cipher.lock().unwrap();
    match guard.as_ref() {
        Some(cipher) => {
            let (nonce, ciphertext) = cipher.encrypt(&plaintext);
            netproto::encode_encrypted(&nonce, &ciphertext)
        }
        None => plaintext,
    }
}

struct IncomingTransfer {
    file: File,
    path: PathBuf,
    received: u64,
    total: u64,
}

fn received_files_dir() -> PathBuf {
    let dir = PathBuf::from("C:\\ProgramData\\RemoteDesktopAppAgent\\received");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

fn handle_file_bytes(transfers: &mut HashMap<u32, IncomingTransfer>, data: &[u8]) {
    match netproto::decode_file(data) {
        Some(OwnedFileEvent::Offer { transfer_id, name, total_size }) => {
            let safe_name = std::path::Path::new(&name)
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "archivo_recibido".to_string());
            let dest = received_files_dir().join(&safe_name);
            if let Ok(file) = File::create(&dest) {
                tracing::info!("recibiendo archivo '{safe_name}' ({total_size} bytes)");
                transfers.insert(
                    transfer_id,
                    IncomingTransfer { file, path: dest, received: 0, total: total_size },
                );
            }
        }
        Some(OwnedFileEvent::Chunk { transfer_id, data }) => {
            if let Some(t) = transfers.get_mut(&transfer_id) {
                if t.file.write_all(&data).is_ok() {
                    t.received += data.len() as u64;
                }
            }
        }
        Some(OwnedFileEvent::Complete { transfer_id }) => {
            if let Some(t) = transfers.remove(&transfer_id) {
                tracing::info!("archivo recibido completo: {}", t.path.display());
            }
        }
        None => {}
    }
}

fn handle_input_bytes(injector: &InputInjector, data: &[u8]) {
    let Some(event) = netproto::decode_input(data) else { return };
    let result = match event {
        InputEvent::MouseMove { x, y } => injector.move_mouse_normalized(x as f64, y as f64),
        InputEvent::MouseButton { button, pressed } => {
            let button = match button {
                1 => MouseButton::Right,
                2 => MouseButton::Middle,
                _ => MouseButton::Left,
            };
            injector.mouse_button(button, pressed)
        }
        InputEvent::MouseWheel { delta } => injector.mouse_wheel(delta),
        InputEvent::Key { vk, pressed } => injector.key(vk, pressed),
    };
    if let Err(e) = result {
        tracing::warn!("no se pudo inyectar el evento de input: {e:#}");
    }
}

#[cfg(windows)]
fn trigger_restart(delay_secs: u8) {
    let _ = std::process::Command::new("shutdown")
        .args(["/r", "/t", &delay_secs.to_string()])
        .status();
}
#[cfg(not(windows))]
fn trigger_restart(_delay_secs: u8) {}

fn handle_control_bytes(out_tx: &mpsc::UnboundedSender<Message>, crypto: &CryptoState, data: &[u8]) {
    if let Some(ControlEvent::RestartRequest { delay_secs }) = netproto::decode_control(data) {
        trigger_restart(delay_secs);
        let ack = netproto::encode_control(ControlEvent::RestartAck);
        let _ = out_tx.send(Message::Binary(wrap_outgoing(crypto, ack)));
    }
}

fn generate_fallback_code() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    format!("{:06}", (nanos % 1_000_000) as u32)
}

async fn setup_p2p(
    out_tx: mpsc::UnboundedSender<Message>,
    injector: InputInjector,
    mut peer_candidate_rx: mpsc::UnboundedReceiver<std::net::SocketAddrV4>,
) {
    let socket = match core_engine::net::bind_local_socket().await {
        Ok(s) => s,
        Err(_) => return,
    };
    let stun_server = "stun.l.google.com:19302";
    let my_candidate = match core_engine::net::stun_discover(&socket, stun_server).await {
        Ok(std::net::SocketAddr::V4(v4)) => v4,
        _ => return,
    };
    let _ = out_tx.send(Message::Binary(netproto::encode_p2p_candidate(my_candidate)));

    let peer_candidate =
        match tokio::time::timeout(Duration::from_secs(5), peer_candidate_rx.recv()).await {
            Ok(Some(addr)) => addr,
            _ => return,
        };

    let peer_addr = std::net::SocketAddr::V4(peer_candidate);
    if let Some(confirmed_addr) = core_engine::net::punch_hole(&socket, peer_addr, 5).await {
        tracing::info!("P2P establecido con {confirmed_addr}");
        let mut buf = [0u8; 1500];
        loop {
            match socket.recv_from(&mut buf).await {
                Ok((len, from)) if from == confirmed_addr => {
                    if buf[..len].first() == Some(&core_engine::net::P2P_PING_MARKER) {
                        continue;
                    }
                    handle_input_bytes(&injector, &buf[..len]);
                }
                Ok(_) => continue,
                Err(_) => break,
            }
        }
    }
}

fn adjust_quality(quality: &AtomicU8, produced_fps: f32, received_fps: f32) {
    if produced_fps < 1.0 {
        return;
    }
    let ratio = received_fps / produced_fps;
    let current = quality.load(Ordering::Relaxed);
    if ratio < 0.7 {
        let new_quality = current.saturating_sub(5).max(QUALITY_MIN);
        if new_quality != current {
            quality.store(new_quality, Ordering::Relaxed);
        }
    } else if ratio > 0.9 && current < QUALITY_MAX {
        quality.store((current + 2).min(QUALITY_MAX), Ordering::Relaxed);
    }
}

fn spawn_capture_thread(
    latest: Arc<StdMutex<Option<Vec<u8>>>>,
    frame_id: Arc<AtomicU64>,
    paired: Arc<AtomicBool>,
    quality: Arc<AtomicU8>,
) {
    thread::spawn(move || loop {
        let run = || -> anyhow::Result<()> {
            let mut capturer = ScreenCapturer::new()?;
            loop {
                if !paired.load(Ordering::Relaxed) {
                    thread::sleep(Duration::from_millis(100));
                    continue;
                }
                let Some(frame) = capturer.next_frame()? else { continue };
                let encoder = VideoEncoder::new(quality.load(Ordering::Relaxed));
                let compressed = encoder.encode(&frame)?;
                *latest.lock().unwrap() = Some(compressed);
                frame_id.fetch_add(1, Ordering::Relaxed);
            }
        };
        if let Err(e) = run() {
            tracing::warn!("captura interrumpida ({e:#}), reintentando en 2s...");
            thread::sleep(Duration::from_secs(2));
        }
    });
}

/// Logica principal de conexion: se conecta, se registra, y maneja
/// todo mientras dure la sesion. Termina (vuelve) si la tarea se
/// cancela desde afuera (`.abort()`, al desconectar) o si el
/// WebSocket se cierra. Emite eventos "status" para que la ventana
/// muestre lo que esta pasando.
async fn run_connection(app: AppHandle, signaling_url: String, agent_code: String) {
    let _ = app.emit("status", format!("Conectando a {signaling_url}..."));

    let ws_stream = match connect_async(&signaling_url).await {
        Ok((s, _)) => s,
        Err(e) => {
            let _ = app.emit("status", format!("Error de conexion: {e}"));
            return;
        }
    };
    let (mut write, mut read) = ws_stream.split();

    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<Message>();
    tokio::spawn(async move {
        while let Some(msg) = out_rx.recv().await {
            if write.send(msg).await.is_err() {
                break;
            }
        }
    });

    let mut code = agent_code;
    if out_tx
        .send(Message::Text(json!({"type": "register_agent", "code": code}).to_string()))
        .is_err()
    {
        return;
    }

    let latest: Arc<StdMutex<Option<Vec<u8>>>> = Arc::new(StdMutex::new(None));
    let frame_id = Arc::new(AtomicU64::new(0));
    let paired = Arc::new(AtomicBool::new(false));
    let crypto = Arc::new(CryptoState::default());
    let quality = Arc::new(AtomicU8::new(QUALITY_INITIAL));
    let produced_fps = Arc::new(StdMutex::new(0f32));

    spawn_capture_thread(Arc::clone(&latest), Arc::clone(&frame_id), Arc::clone(&paired), Arc::clone(&quality));

    let input_injector = InputInjector::new();
    let mut incoming_transfers: HashMap<u32, IncomingTransfer> = HashMap::new();
    let (peer_candidate_tx, peer_candidate_rx) = mpsc::unbounded_channel::<std::net::SocketAddrV4>();
    let mut peer_candidate_rx = Some(peer_candidate_rx);
    let mut p2p_attempted = false;

    {
        let latest = Arc::clone(&latest);
        let frame_id = Arc::clone(&frame_id);
        let paired = Arc::clone(&paired);
        let out_tx = out_tx.clone();
        let crypto = Arc::clone(&crypto);
        let produced_fps = Arc::clone(&produced_fps);
        tokio::spawn(async move {
            let mut last_sent = 0u64;
            let mut frames_this_second = 0u32;
            let mut second_start = tokio::time::Instant::now();
            loop {
                tokio::time::sleep(Duration::from_millis(5)).await;
                if !paired.load(Ordering::Relaxed) {
                    continue;
                }
                let current = frame_id.load(Ordering::Relaxed);
                if current == last_sent {
                    continue;
                }
                last_sent = current;
                let frame = { latest.lock().unwrap().clone() };
                if let Some(frame) = frame {
                    let msg = wrap_outgoing(&crypto, netproto::encode_frame(&frame));
                    if out_tx.send(Message::Binary(msg)).is_ok() {
                        frames_this_second += 1;
                    }
                }
                if second_start.elapsed().as_secs_f32() >= 1.0 {
                    *produced_fps.lock().unwrap() = frames_this_second as f32 / second_start.elapsed().as_secs_f32();
                    frames_this_second = 0;
                    second_start = tokio::time::Instant::now();
                }
            }
        });
    }

    while let Some(msg) = read.next().await {
        let msg = match msg {
            Ok(m) => m,
            Err(_) => break,
        };

        match msg {
            Message::Text(text) => {
                let parsed: Value = match serde_json::from_str(&text) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                match parsed["type"].as_str() {
                    Some("registered") => {
                        let _ = app.emit("status", format!("Registrado con codigo {code} - esperando conexion..."));
                    }
                    Some("paired") => {
                        paired.store(true, Ordering::Relaxed);
                        let _ = app.emit("status", "Conectado - transmitiendo".to_string());

                        let (secret, public_bytes) = crypto::generate_keypair();
                        *crypto.my_secret.lock().unwrap() = Some(secret);
                        let _ = out_tx.send(Message::Binary(netproto::encode_key_exchange(&public_bytes)));

                        if !p2p_attempted {
                            p2p_attempted = true;
                            if let Some(rx) = peer_candidate_rx.take() {
                                tokio::spawn(setup_p2p(out_tx.clone(), input_injector, rx));
                            }
                        }
                    }
                    Some("peer_disconnected") => {
                        paired.store(false, Ordering::Relaxed);
                        let _ = app.emit("status", format!("Registrado con codigo {code} - esperando conexion..."));
                    }
                    Some("error") => {
                        let message = parsed["message"].as_str().unwrap_or("").to_string();
                        if message.contains("ya esta en uso") {
                            code = generate_fallback_code();
                            let _ = out_tx.send(Message::Text(json!({"type": "register_agent", "code": code}).to_string()));
                        } else {
                            let _ = app.emit("status", format!("Error: {message}"));
                        }
                    }
                    _ => {}
                }
            }
            Message::Binary(bytes) => match netproto::peek_kind(&bytes) {
                Some(netproto::KIND_KEY_EXCHANGE) => {
                    if let Some(peer_public) = netproto::decode_key_exchange(&bytes) {
                        if let Some(secret) = crypto.my_secret.lock().unwrap().take() {
                            let key = crypto::derive_session_key(secret, peer_public);
                            *crypto.cipher.lock().unwrap() = Some(Arc::new(SessionCipher::new(key)));
                        }
                    }
                }
                Some(kind) => {
                    let decrypted_owner;
                    let effective: &[u8] = if kind == netproto::KIND_ENCRYPTED {
                        let cipher_guard = crypto.cipher.lock().unwrap();
                        match (cipher_guard.as_ref(), netproto::decode_encrypted(&bytes)) {
                            (Some(cipher), Some((nonce, ciphertext))) => match cipher.decrypt(nonce, ciphertext) {
                                Ok(plain) => {
                                    decrypted_owner = plain;
                                    &decrypted_owner
                                }
                                Err(_) => continue,
                            },
                            _ => continue,
                        }
                    } else {
                        &bytes
                    };
                    match netproto::peek_kind(effective) {
                        Some(netproto::KIND_INPUT) => handle_input_bytes(&input_injector, effective),
                        Some(netproto::KIND_CONTROL) => handle_control_bytes(&out_tx, &crypto, effective),
                        Some(netproto::KIND_FILE) => handle_file_bytes(&mut incoming_transfers, effective),
                        Some(netproto::KIND_P2P_CANDIDATE) => {
                            if let Some(addr) = netproto::decode_p2p_candidate(effective) {
                                let _ = peer_candidate_tx.send(addr);
                            }
                        }
                        Some(netproto::KIND_STATS) => {
                            if let Some(received_fps) = netproto::decode_stats(effective) {
                                let produced = *produced_fps.lock().unwrap();
                                adjust_quality(&quality, produced, received_fps);
                            }
                        }
                        _ => {}
                    }
                }
                None => {}
            },
            Message::Close(_) => break,
            _ => {}
        }
    }

    let _ = app.emit("status", "Desconectado".to_string());
}

// ---------------------------------------------------------------
// Comandos Tauri para la ventana de configuracion
// ---------------------------------------------------------------

#[tauri::command]
async fn get_config(state: State<'_, AppState>) -> Result<AgentConfig, String> {
    Ok(state.config.lock().await.clone())
}

#[tauri::command]
async fn disconnect(state: State<'_, AppState>, app: AppHandle) -> Result<(), String> {
    if let Some(handle) = state.connection_task.lock().await.take() {
        handle.abort();
    }
    let _ = app.emit("status", "Desconectado (manual)".to_string());
    Ok(())
}

#[tauri::command]
async fn save_and_reconnect(
    state: State<'_, AppState>,
    app: AppHandle,
    signaling_url: String,
    agent_code: String,
) -> Result<(), String> {
    let cfg = AgentConfig { signaling_url: signaling_url.clone(), agent_code: agent_code.clone() };
    persist_config(&cfg).map_err(|e| e.to_string())?;
    *state.config.lock().await = cfg;

    if let Some(handle) = state.connection_task.lock().await.take() {
        handle.abort();
    }

    let app_for_task = app.clone();
    let handle = tokio::spawn(run_connection(app_for_task, signaling_url, agent_code));
    *state.connection_task.lock().await = Some(handle);

    Ok(())
}

fn main() {
    // Capturamos panics y los escribimos al log - sin esto, un panic
    // en una app sin consola (windows_subsystem = "windows") muere en
    // silencio total, sin dejar ningun rastro de que paso ni por que.
    std::panic::set_hook(Box::new(|info| {
        let log_dir = std::path::Path::new("C:\\ProgramData\\RemoteDesktopAppAgent");
        let _ = std::fs::create_dir_all(log_dir);
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(log_dir.join("agent-ui.log"))
        {
            use std::io::Write;
            let _ = writeln!(file, "PANIC: {info}");
        }
    }));

    let log_dir = std::path::Path::new("C:\\ProgramData\\RemoteDesktopAppAgent");
    let _ = std::fs::create_dir_all(log_dir);
    if let Ok(file) = std::fs::OpenOptions::new().create(true).append(true).open(log_dir.join("agent-ui.log")) {
        let _ = tracing_subscriber::fmt().with_writer(std::sync::Mutex::new(file)).with_ansi(false).try_init();
    }

    let initial_config = load_config();

    tauri::Builder::default()
        .manage(AppState {
            config: Mutex::new(initial_config.clone()),
            connection_task: Mutex::new(None),
        })
        .setup(move |app| {
            let app_handle = app.handle().clone();
            let cfg = initial_config.clone();
            tauri::async_runtime::spawn(async move {
                let state = app_handle.state::<AppState>();
                let app_for_task = app_handle.clone();
                let handle = tokio::spawn(run_connection(app_for_task, cfg.signaling_url, cfg.agent_code));
                *state.connection_task.lock().await = Some(handle);
            });

            let icon_bytes = include_bytes!("../icons/icon.png");
            let decoded = image::load_from_memory(icon_bytes)
                .expect("el icono embebido deberia ser un PNG valido")
                .into_rgba8();
            let (icon_width, icon_height) = (decoded.width(), decoded.height());
            let tray_icon = tauri::image::Image::new_owned(decoded.into_raw(), icon_width, icon_height);

            let show_item = MenuItemBuilder::with_id("show", "Configurar...").build(app)?;
            let quit_item = MenuItemBuilder::with_id("quit", "Salir").build(app)?;
            let menu = MenuBuilder::new(app).items(&[&show_item, &quit_item]).build()?;

            let _tray = TrayIconBuilder::new()
                .icon(tray_icon)
                .menu(&menu)
                .tooltip("Remote Desktop App - Agent")
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "show" => {
                        if let Some(window) = app.get_webview_window("main") {
                            let _ = window.show();
                            let _ = window.set_focus();
                        }
                    }
                    "quit" => app.exit(0),
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click { button: TrayMouseButton::Left, button_state: MouseButtonState::Up, .. } = event {
                        let app = tray.app_handle();
                        if let Some(window) = app.get_webview_window("main") {
                            let _ = window.show();
                            let _ = window.set_focus();
                        }
                    }
                })
                .build(app)?;

            Ok(())
        })
        .on_window_event(|window, event| {
            // Cerrar la ventana la oculta en vez de terminar la app -
            // el agent tiene que seguir corriendo en la bandeja.
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                let _ = window.hide();
                api.prevent_close();
            }
        })
        .invoke_handler(tauri::generate_handler![get_config, disconnect, save_and_reconnect])
        .run(tauri::generate_context!())
        .expect("error corriendo agent-ui");
}
