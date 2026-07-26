//! Agent: corre en la PC remota (la que se va a controlar).
//!
//! Flujo actual:
//! 1. Se conecta al signaling-server y se registra con un codigo.
//! 2. Cuando un controller se empareja (mensaje "paired"), arranca a
//!    capturar+comprimir pantalla y mandar cada frame por el relay.
//! 3. Si el controller se desconecta ("peer_disconnected"), pausa el
//!    streaming (sigue registrado, listo para que alguien se reconecte).
//!
//! Todavia pendiente: input remoto entrante (por ahora el agent solo
//! manda video, no procesa comandos de mouse/teclado del controller),
//! servicio de Windows, y migrar de relay-por-signaling a P2P real.

use anyhow::{Context, Result};
use core_engine::capture::ScreenCapturer;
use core_engine::encode::VideoEncoder;
use core_engine::input::{InputInjector, MouseButton};
use core_engine::netproto::{self, ControlEvent, InputEvent, OwnedFileEvent};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::fs::File;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;

/// Calidad JPEG de partida. Mas adelante esto deberia ajustarse en
/// vivo segun el ancho de banda disponible.
const QUALITY: u8 = 50;

/// Transferencia de archivo entrante en progreso.
struct IncomingTransfer {
    file: File,
    path: PathBuf,
    received: u64,
    total: u64,
}

/// Carpeta donde se guardan los archivos que llegan del controller.
/// Fija (no depende del usuario logueado) porque el agent puede correr
/// como servicio LocalSystem, sin un "Desktop" de usuario al que
/// escribir directamente.
fn received_files_dir() -> PathBuf {
    let dir = PathBuf::from("C:\\ProgramData\\RemoteDesktopAppAgent\\received");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

fn handle_file_bytes(transfers: &mut HashMap<u32, IncomingTransfer>, data: &[u8]) {
    match netproto::decode_file(data) {
        Some(OwnedFileEvent::Offer { transfer_id, name, total_size }) => {
            // Sanitizar: nos quedamos solo con el nombre de archivo,
            // sin separadores de path, para que el controller no
            // pueda escribir fuera de la carpeta de destino.
            let safe_name = std::path::Path::new(&name)
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "archivo_recibido".to_string());
            let dest = received_files_dir().join(&safe_name);
            match File::create(&dest) {
                Ok(file) => {
                    tracing::info!(
                        "recibiendo archivo '{safe_name}' ({total_size} bytes) -> {}",
                        dest.display()
                    );
                    transfers.insert(
                        transfer_id,
                        IncomingTransfer { file, path: dest, received: 0, total: total_size },
                    );
                }
                Err(e) => tracing::error!("no se pudo crear el archivo de destino: {e}"),
            }
        }
        Some(OwnedFileEvent::Chunk { transfer_id, data }) => {
            if let Some(t) = transfers.get_mut(&transfer_id) {
                if let Err(e) = t.file.write_all(&data) {
                    tracing::error!("error escribiendo el archivo recibido: {e}");
                    return;
                }
                t.received += data.len() as u64;
            }
        }
        Some(OwnedFileEvent::Complete { transfer_id }) => {
            if let Some(t) = transfers.remove(&transfer_id) {
                tracing::info!(
                    "archivo recibido completo: {} ({}/{} bytes)",
                    t.path.display(),
                    t.received,
                    t.total
                );
            }
        }
        None => {}
    }
}

fn handle_input_bytes(injector: &InputInjector, data: &[u8]) {
    let Some(event) = netproto::decode_input(data) else {
        return;
    };
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

/// Ejecuta un reinicio real de Windows via el comando `shutdown`, con
/// el margen que pidio el controller. Usamos el comando del sistema
/// en vez de una API directa porque ya se encarga de avisarle a las
/// demas apps abiertas y de dar el margen configurado.
#[cfg(windows)]
fn trigger_restart(delay_secs: u8) {
    tracing::warn!("reinicio remoto pedido - ejecutando en {delay_secs}s");
    let result = std::process::Command::new("shutdown")
        .args(["/r", "/t", &delay_secs.to_string()])
        .status();
    if let Err(e) = result {
        tracing::error!("no se pudo ejecutar el comando de reinicio: {e}");
    }
}

#[cfg(not(windows))]
fn trigger_restart(_delay_secs: u8) {
    tracing::warn!("reinicio remoto pedido, pero no esta implementado fuera de Windows");
}

fn handle_control_bytes(out_tx: &mpsc::UnboundedSender<Message>, data: &[u8]) {
    match netproto::decode_control(data) {
        Some(ControlEvent::RestartRequest { delay_secs }) => {
            trigger_restart(delay_secs);
            let ack = netproto::encode_control(ControlEvent::RestartAck);
            let _ = out_tx.send(Message::Binary(ack));
        }
        Some(ControlEvent::RestartAck) => {
            // El agent no espera recibir esto, es el agent el que lo manda.
        }
        None => {}
    }
}

fn spawn_capture_thread(
    latest: Arc<Mutex<Option<Vec<u8>>>>,
    frame_id: Arc<AtomicU64>,
    paired: Arc<AtomicBool>,
) {
    thread::spawn(move || {
        // DXGI Desktop Duplication puede fallar en varias situaciones
        // normales: la pantalla se bloquea, entra en suspension, o hay
        // una sesion de Escritorio Remoto encima. En vez de morir del
        // todo, reintentamos: recreamos el ScreenCapturer y seguimos.
        loop {
            let run = || -> Result<()> {
                let mut capturer = ScreenCapturer::new()?;
                let encoder = VideoEncoder::new(QUALITY);
                loop {
                    if !paired.load(Ordering::Relaxed) {
                        thread::sleep(Duration::from_millis(100));
                        continue;
                    }
                    let frame = capturer.next_frame()?;
                    let compressed = encoder.encode(&frame)?;
                    *latest.lock().unwrap() = Some(compressed);
                    frame_id.fetch_add(1, Ordering::Relaxed);
                }
            };
            if let Err(e) = run() {
                tracing::warn!("captura interrumpida ({e:#}), reintentando en 2s...");
                thread::sleep(Duration::from_secs(2));
            }
        }
    });
}

#[cfg(windows)]
mod service;

/// Logica real del agent: conectarse al signaling server, registrarse,
/// y una vez emparejado, mandar video + recibir input. Es una funcion
/// aparte (en vez de estar directo en `main`) porque tanto el modo
/// consola como el modo servicio de Windows necesitan poder llamarla,
/// cada uno armando su propio runtime de tokio.
pub async fn run_agent() -> Result<()> {
    let signaling_url =
        std::env::var("SIGNALING_URL").unwrap_or_else(|_| "ws://127.0.0.1:8080".to_string());
    let code = std::env::var("AGENT_CODE").unwrap_or_else(|_| "123456".to_string());

    tracing::info!("conectando a {signaling_url}...");
    let (ws_stream, _) = connect_async(&signaling_url)
        .await
        .context("no se pudo conectar al signaling server")?;
    let (mut write, mut read) = ws_stream.split();

    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<Message>();

    // Tarea que vuelca el canal de salida al socket real.
    tokio::spawn(async move {
        while let Some(msg) = out_rx.recv().await {
            if write.send(msg).await.is_err() {
                break;
            }
        }
    });

    out_tx.send(Message::Text(
        json!({"type": "register_agent", "code": code}).to_string(),
    ))?;

    let latest: Arc<Mutex<Option<Vec<u8>>>> = Arc::new(Mutex::new(None));
    let frame_id = Arc::new(AtomicU64::new(0));
    let paired = Arc::new(AtomicBool::new(false));

    spawn_capture_thread(Arc::clone(&latest), Arc::clone(&frame_id), Arc::clone(&paired));

    let input_injector = InputInjector::new();
    let mut incoming_transfers: HashMap<u32, IncomingTransfer> = HashMap::new();

    // Tarea que manda por la red el ultimo frame disponible. No manda
    // mas rapido de lo que hay frames nuevos (chequea cada 5ms, que da
    // margen de sobra hasta para 60fps).
    {
        let latest = Arc::clone(&latest);
        let frame_id = Arc::clone(&frame_id);
        let paired = Arc::clone(&paired);
        let out_tx = out_tx.clone();
        tokio::spawn(async move {
            let mut last_sent = 0u64;
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
                    let msg = netproto::encode_frame(&frame);
                    let _ = out_tx.send(Message::Binary(msg));
                }
            }
        });
    }

    // Loop principal: los mensajes de TEXTO son control del signaling
    // server (registro confirmado, emparejamiento, etc), los de
    // BINARIO son eventos de input que manda el controller una vez
    // emparejados.
    while let Some(msg) = read.next().await {
        let msg = match msg {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!("error leyendo del signaling server: {e}");
                break;
            }
        };

        match msg {
            Message::Text(text) => {
                let parsed: Value = match serde_json::from_str(&text) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                match parsed["type"].as_str() {
                    Some("registered") => {
                        tracing::info!("registrado con codigo {}", parsed["code"]);
                    }
                    Some("paired") => {
                        tracing::info!("controller conectado - arrancando streaming");
                        paired.store(true, Ordering::Relaxed);
                    }
                    Some("peer_disconnected") => {
                        tracing::info!("controller desconectado - pausando streaming");
                        paired.store(false, Ordering::Relaxed);
                    }
                    Some("error") => {
                        tracing::warn!("error del signaling server: {}", parsed["message"]);
                    }
                    _ => {}
                }
            }
            Message::Binary(bytes) => match netproto::peek_kind(&bytes) {
                Some(netproto::KIND_INPUT) => handle_input_bytes(&input_injector, &bytes),
                Some(netproto::KIND_CONTROL) => handle_control_bytes(&out_tx, &bytes),
                Some(netproto::KIND_FILE) => handle_file_bytes(&mut incoming_transfers, &bytes),
                _ => {}
            },
            Message::Close(_) => break,
            _ => {}
        }
    }

    Ok(())
}

/// Corre el agent en modo consola: arma su propio runtime de tokio y
/// bloquea hasta que `run_agent` termine. Es el modo que usamos para
/// desarrollo/testing (`cargo run --bin agent`), y tambien el
/// fallback si el binario se ejecuta sin ser lanzado por el Service
/// Control Manager.
fn run_console_mode() -> Result<()> {
    tracing_subscriber::fmt::init();
    let runtime = tokio::runtime::Runtime::new().context("no se pudo crear el runtime de tokio")?;
    runtime.block_on(run_agent())
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let command = args.get(1).map(|s| s.as_str());

    #[cfg(windows)]
    {
        match command {
            Some("install") => return service::install(),
            Some("uninstall") => return service::uninstall(),
            Some("console") => return run_console_mode(),
            _ => {
                // Sin argumentos: si el SCM nos esta lanzando como
                // servicio, esto atiende ese arranque y no vuelve
                // hasta que el servicio para. Si NO nos lanzo el SCM
                // (ej: doble click en el exe), start_dispatcher falla
                // enseguida y caemos al modo consola.
                if service::start_dispatcher().is_ok() {
                    return Ok(());
                }
                tracing::warn!(
                    "no se pudo arrancar como servicio (¿no te lanzo el SCM?), corriendo en modo consola"
                );
                return run_console_mode();
            }
        }
    }

    #[cfg(not(windows))]
    {
        let _ = command; // install/uninstall/service no aplican fuera de Windows
        run_console_mode()
    }
}
