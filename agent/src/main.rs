//! Agent: corre en la PC remota (la que se va a controlar).
//!
//! Flujo actual:
//! 1. Se conecta al signaling-server y se registra con un codigo.
//! 2. Cuando un controller se empareja (mensaje "paired"), arranca a
//!    capturar+comprimir pantalla y mandar cada frame por el relay,
//!    e inicia el handshake de cifrado end-to-end (Diffie-Hellman) y
//!    el intento de P2P para el input.
//! 3. Todo el trafico de aplicacion (video/input/archivos/control) se
//!    cifra automaticamente en cuanto el handshake de cifrado
//!    termina - si el peer no lo soporta (ej: una version vieja del
//!    controller), simplemente se sigue sin cifrar, sin romper nada.
//! 4. Si el controller se desconecta ("peer_disconnected"), pausa el
//!    streaming (sigue registrado, listo para que alguien se reconecte).

use anyhow::{Context, Result};
use core_engine::capture::ScreenCapturer;
use core_engine::crypto::{self, SessionCipher};
use core_engine::encode::VideoEncoder;
use core_engine::input::{InputInjector, MouseButton};
use core_engine::netproto::{self, ControlEvent, InputEvent, OwnedFileEvent};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::fs::File;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;
use x25519_dalek::EphemeralSecret;

/// Calidad JPEG de partida, y los limites entre los que se mueve el
/// ajuste automatico segun como esta llegando el video del otro lado.
const QUALITY_INITIAL: u8 = 50;
const QUALITY_MIN: u8 = 20;
const QUALITY_MAX: u8 = 75;

/// Estado del cifrado end-to-end compartido entre tareas: la clave
/// efimera propia (se consume al recibir la clave publica del peer) y
/// el cifrador ya derivado (una vez que el handshake termino).
#[derive(Default)]
struct CryptoState {
    my_secret: Mutex<Option<EphemeralSecret>>,
    cipher: Mutex<Option<Arc<SessionCipher>>>,
}

/// Envuelve un mensaje saliente: si el handshake de cifrado ya
/// termino, lo cifra; si no, lo manda tal cual (esto es lo que hace
/// que todo siga funcionando con un peer que no soporte cifrado).
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

fn handle_control_bytes(out_tx: &mpsc::UnboundedSender<Message>, crypto: &CryptoState, data: &[u8]) {
    match netproto::decode_control(data) {
        Some(ControlEvent::RestartRequest { delay_secs }) => {
            trigger_restart(delay_secs);
            let ack = netproto::encode_control(ControlEvent::RestartAck);
            let _ = out_tx.send(Message::Binary(wrap_outgoing(crypto, ack)));
        }
        Some(ControlEvent::RestartAck) => {
            // El agent no espera recibir esto, es el agent el que lo manda.
        }
        None => {}
    }
}

/// Intenta establecer un path P2P directo para el input (mejor
/// latencia que pasar por el relay). Es pura optimizacion: si algo
/// falla en cualquier paso, simplemente no hace nada mas y el input
/// sigue llegando por el relay de siempre, sin que el usuario note
/// nada raro.
async fn setup_p2p(
    out_tx: mpsc::UnboundedSender<Message>,
    injector: InputInjector,
    mut peer_candidate_rx: mpsc::UnboundedReceiver<std::net::SocketAddrV4>,
) {
    let socket = match core_engine::net::bind_local_socket().await {
        Ok(s) => s,
        Err(e) => {
            tracing::info!("P2P: no se pudo bindear el socket UDP, sigo con el relay ({e:#})");
            return;
        }
    };

    let stun_server = "stun.l.google.com:19302";
    let my_candidate = match core_engine::net::stun_discover(&socket, stun_server).await {
        Ok(std::net::SocketAddr::V4(v4)) => v4,
        Ok(_) => {
            tracing::info!("P2P: STUN devolvio una direccion IPv6, no soportado todavia");
            return;
        }
        Err(e) => {
            tracing::info!("P2P: STUN fallo, sigo con el relay ({e:#})");
            return;
        }
    };
    tracing::info!("P2P: mi candidato es {my_candidate}");
    // El candidato viaja siempre SIN cifrar (a proposito): si lo
    // envolvieramos con wrap_outgoing, podria salir cifrado apenas
    // nuestro propio handshake termina, mientras el peer todavia no
    // tiene la clave lista de su lado - el mensaje se perderia. No es
    // informacion sensible (solo IP:puerto), asi que no hace falta.
    let _ = out_tx.send(Message::Binary(netproto::encode_p2p_candidate(my_candidate)));

    let peer_candidate =
        match tokio::time::timeout(Duration::from_secs(5), peer_candidate_rx.recv()).await {
            Ok(Some(addr)) => addr,
            _ => {
                tracing::info!("P2P: no llego el candidato del controller a tiempo, sigo con el relay");
                return;
            }
        };
    tracing::info!("P2P: candidato del controller es {peer_candidate}");

    let peer_addr = std::net::SocketAddr::V4(peer_candidate);
    match core_engine::net::punch_hole(&socket, peer_addr, 5).await {
        Some(confirmed_addr) => {
            tracing::info!("P2P establecido con {confirmed_addr} - el input puede llegar directo ahora");
            let mut buf = [0u8; 1500];
            loop {
                match socket.recv_from(&mut buf).await {
                    Ok((len, from)) if from == confirmed_addr => {
                        if buf[..len].first() == Some(&core_engine::net::P2P_PING_MARKER) {
                            continue; // solo era un keepalive
                        }
                        handle_input_bytes(&injector, &buf[..len]);
                    }
                    Ok(_) => continue,
                    Err(e) => {
                        tracing::info!("P2P: socket cerrado ({e}), sigo solo con el relay");
                        break;
                    }
                }
            }
        }
        None => {
            tracing::info!("P2P: no se pudo abrir el path directo, sigo con el relay");
        }
    }
}

fn spawn_capture_thread(
    latest: Arc<Mutex<Option<Vec<u8>>>>,
    frame_id: Arc<AtomicU64>,
    paired: Arc<AtomicBool>,
    quality: Arc<AtomicU8>,
) {
    thread::spawn(move || {
        // DXGI Desktop Duplication puede fallar en varias situaciones
        // normales: la pantalla se bloquea, entra en suspension, o hay
        // una sesion de Escritorio Remoto encima. En vez de morir del
        // todo, reintentamos: recreamos el ScreenCapturer y seguimos.
        loop {
            let run = || -> Result<()> {
                let mut capturer = ScreenCapturer::new()?;
                loop {
                    if !paired.load(Ordering::Relaxed) {
                        thread::sleep(Duration::from_millis(100));
                        continue;
                    }
                    let Some(frame) = capturer.next_frame()? else {
                        continue;
                    };
                    // El encoder se recrea cada vez con la calidad
                    // actual (es solo un u8 adentro, no cuesta nada) -
                    // asi el ajuste automatico de calidad se aplica
                    // frame a frame sin tener que reiniciar la captura.
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
        }
    });
}

/// Ajusta la calidad JPEG segun los fps que el controller reporta
/// estar recibiendo de verdad, comparados con los que el agent esta
/// mandando. Si el controller recibe bastante menos de lo que se
/// manda (la red o el controller no dan abasto), bajamos calidad para
/// aliviar; si recibe casi todo, subimos de a poco para aprovechar el
/// ancho de banda disponible. Nunca sale de [QUALITY_MIN, QUALITY_MAX].
fn adjust_quality(quality: &AtomicU8, produced_fps: f32, received_fps: f32) {
    if produced_fps < 1.0 {
        return; // todavia no hay suficiente informacion para decidir
    }
    let ratio = received_fps / produced_fps;
    let current = quality.load(Ordering::Relaxed);

    if ratio < 0.7 {
        let new_quality = current.saturating_sub(5).max(QUALITY_MIN);
        if new_quality != current {
            quality.store(new_quality, Ordering::Relaxed);
            tracing::info!(
                "calidad bajada a {new_quality} (el controller recibe {received_fps:.1}/{produced_fps:.1} fps)"
            );
        }
    } else if ratio > 0.9 && current < QUALITY_MAX {
        let new_quality = (current + 2).min(QUALITY_MAX);
        quality.store(new_quality, Ordering::Relaxed);
        tracing::info!(
            "calidad subida a {new_quality} (el controller recibe {received_fps:.1}/{produced_fps:.1} fps)"
        );
    }
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
    let crypto = Arc::new(CryptoState::default());
    let quality = Arc::new(AtomicU8::new(QUALITY_INITIAL));
    let produced_fps = Arc::new(Mutex::new(0f32));

    spawn_capture_thread(
        Arc::clone(&latest),
        Arc::clone(&frame_id),
        Arc::clone(&paired),
        Arc::clone(&quality),
    );

    let input_injector = InputInjector::new();
    let mut incoming_transfers: HashMap<u32, IncomingTransfer> = HashMap::new();

    let (peer_candidate_tx, peer_candidate_rx) =
        mpsc::unbounded_channel::<std::net::SocketAddrV4>();
    let mut peer_candidate_rx = Some(peer_candidate_rx);
    let mut p2p_attempted = false;

    // Tarea que manda por la red el ultimo frame disponible, cifrado
    // si el handshake ya termino. Tambien lleva la cuenta de fps
    // producidos (cuantos frames por segundo esta MANDANDO), que se
    // compara contra lo que el controller reporta haber RECIBIDO para
    // decidir si subir o bajar la calidad.
    {
        let latest = Arc::clone(&latest);
        let frame_id = Arc::clone(&frame_id);
        let paired = Arc::clone(&paired);
        let out_tx = out_tx.clone();
        let crypto = Arc::clone(&crypto);
        let produced_fps = Arc::clone(&produced_fps);
        tokio::spawn(async move {
            let mut last_sent = 0u64;
            let mut sent_count = 0u64;
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
                    let msg_len = msg.len();
                    match out_tx.send(Message::Binary(msg)) {
                        Ok(()) => {
                            sent_count += 1;
                            frames_this_second += 1;
                            if sent_count == 1 || sent_count % 100 == 0 {
                                tracing::info!("frame #{sent_count} mandado ({msg_len} bytes)");
                            }
                        }
                        Err(e) => tracing::warn!("no se pudo encolar el frame para mandar: {e}"),
                    }
                }

                if second_start.elapsed().as_secs_f32() >= 1.0 {
                    let fps = frames_this_second as f32 / second_start.elapsed().as_secs_f32();
                    *produced_fps.lock().unwrap() = fps;
                    frames_this_second = 0;
                    second_start = tokio::time::Instant::now();
                }
            }
        });
    }

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

                        // Cifrado E2E: generamos nuestro par de claves
                        // efimero y lo mandamos. Si el peer no
                        // participa del handshake (version vieja del
                        // controller), no llega respuesta y seguimos
                        // sin cifrar - no rompe nada.
                        let (secret, public_bytes) = crypto::generate_keypair();
                        *crypto.my_secret.lock().unwrap() = Some(secret);
                        let _ = out_tx.send(Message::Binary(netproto::encode_key_exchange(&public_bytes)));

                        if !p2p_attempted {
                            p2p_attempted = true;
                            if let Some(rx) = peer_candidate_rx.take() {
                                let out_tx_p2p = out_tx.clone();
                                tokio::spawn(setup_p2p(out_tx_p2p, input_injector, rx));
                            }
                        }
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
            Message::Binary(bytes) => {
                // El intercambio de clave nunca viaja cifrado (es el
                // handshake mismo) - se procesa aparte.
                if netproto::peek_kind(&bytes) == Some(netproto::KIND_KEY_EXCHANGE) {
                    if let Some(peer_public) = netproto::decode_key_exchange(&bytes) {
                        let secret = crypto.my_secret.lock().unwrap().take();
                        if let Some(secret) = secret {
                            let key = crypto::derive_session_key(secret, peer_public);
                            *crypto.cipher.lock().unwrap() = Some(Arc::new(SessionCipher::new(key)));
                            tracing::info!("cifrado end-to-end establecido con el controller");
                        }
                    }
                    continue;
                }

                // Si viene cifrado, lo desciframos y seguimos con el
                // contenido real; si no, lo tratamos tal cual (permite
                // seguir hablando con un peer que no soporte cifrado).
                let decrypted_owner;
                let effective: &[u8] = if netproto::peek_kind(&bytes) == Some(netproto::KIND_ENCRYPTED) {
                    let cipher_guard = crypto.cipher.lock().unwrap();
                    match (cipher_guard.as_ref(), netproto::decode_encrypted(&bytes)) {
                        (Some(cipher), Some((nonce, ciphertext))) => match cipher.decrypt(nonce, ciphertext) {
                            Ok(plain) => {
                                decrypted_owner = plain;
                                &decrypted_owner
                            }
                            Err(e) => {
                                tracing::warn!("no se pudo descifrar un mensaje: {e}");
                                continue;
                            }
                        },
                        _ => {
                            tracing::warn!("mensaje cifrado recibido pero todavia no tengo la clave");
                            continue;
                        }
                    }
                } else {
                    &bytes
                };

                match netproto::peek_kind(effective) {
                    Some(netproto::KIND_INPUT) => handle_input_bytes(&input_injector, effective),
                    Some(netproto::KIND_CONTROL) => handle_control_bytes(&out_tx, &crypto, effective),
                    Some(netproto::KIND_FILE) => handle_file_bytes(&mut incoming_transfers, effective),
                    Some(netproto::KIND_STATS) => {
                        if let Some(received_fps) = netproto::decode_stats(effective) {
                            let produced = *produced_fps.lock().unwrap();
                            adjust_quality(&quality, produced, received_fps);
                        }
                    }
                    Some(netproto::KIND_P2P_CANDIDATE) => {
                        if let Some(addr) = netproto::decode_p2p_candidate(effective) {
                            let _ = peer_candidate_tx.send(addr);
                        }
                    }
                    _ => {}
                }
            }
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
        let _ = command;
        run_console_mode()
    }
}
