//! Backend Rust de la app controladora (Tauri). Misma logica de red
//! que el agent (conectarse al signaling server, emparejarse, cifrado
//! end-to-end, P2P, video/input/archivos/control via netproto), pero
//! expuesta como comandos Tauri para que el frontend (HTML/JS) la
//! use, y eventos para que el frontend reciba lo que llega de la red.
//!
//! Nota: este archivo se escribio sin poder compilarlo del todo
//! (Tauri necesita librerias de sistema de WebView que este entorno
//! de desarrollo no tiene) - es esperable algun ajuste de API al
//! compilarlo por primera vez, mismo patron que tuvimos con capture/
//! input/service.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use base64::{engine::general_purpose::STANDARD, Engine as _};
use core_engine::crypto::{self, SessionCipher};
use core_engine::netproto::{self, ControlEvent, FileEvent, InputEvent, OwnedFileEvent};
use futures_util::{SinkExt, StreamExt};
use serde::Serialize;
use serde_json::json;
use std::collections::HashMap;
use std::fs::File;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::net::UdpSocket;
use tokio::sync::{mpsc, Mutex};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;
use x25519_dalek::EphemeralSecret;

/// Estado del cifrado end-to-end: la clave efimera propia (se
/// consume al recibir la clave publica del agent) y el cifrador ya
/// derivado, una vez que el handshake termino.
#[derive(Default)]
struct CryptoState {
    my_secret: Mutex<Option<EphemeralSecret>>,
    cipher: Mutex<Option<Arc<SessionCipher>>>,
}

/// Envuelve un mensaje saliente: si el handshake de cifrado ya
/// termino, lo cifra; si no, lo manda tal cual (permite seguir
/// hablando con un agent que no soporte cifrado).
async fn wrap_outgoing(crypto: &CryptoState, plaintext: Vec<u8>) -> Vec<u8> {
    let guard = crypto.cipher.lock().await;
    match guard.as_ref() {
        Some(cipher) => {
            let (nonce, ciphertext) = cipher.encrypt(&plaintext);
            netproto::encode_encrypted(&nonce, &ciphertext)
        }
        None => plaintext,
    }
}

/// Estado compartido: el canal para mandar mensajes por la conexion
/// activa, el path P2P directo si se logro establecer, y el estado
/// de cifrado de la sesion actual.
#[derive(Default)]
struct AppState {
    out_tx: Mutex<Option<mpsc::UnboundedSender<Message>>>,
    p2p: Mutex<Option<(UdpSocket, std::net::SocketAddr)>>,
    crypto: CryptoState,
}

#[derive(Serialize, Clone)]
struct ErrorPayload {
    message: String,
}

#[derive(Serialize, Clone)]
struct FileIncomingPayload {
    name: String,
    size: u64,
}

fn received_files_dir() -> PathBuf {
    let dir = std::env::var("USERPROFILE")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
        .join("Downloads")
        .join("RemoteDesktopApp-Received");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// Conecta al signaling server y pide emparejarse con el codigo dado.
/// Arranca una tarea de fondo que traduce lo que llega por la red en
/// eventos Tauri (`video-frame`, `paired`, `peer-disconnected`, etc)
/// que el frontend escucha con `listen()`.
#[tauri::command]
async fn connect(
    app: AppHandle,
    state: State<'_, AppState>,
    url: String,
    code: String,
) -> Result<(), String> {
    let (ws_stream, _) = connect_async(&url).await.map_err(|e| e.to_string())?;
    let (mut write, mut read) = ws_stream.split();

    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<Message>();
    tokio::spawn(async move {
        while let Some(msg) = out_rx.recv().await {
            if write.send(msg).await.is_err() {
                break;
            }
        }
    });

    out_tx
        .send(Message::Text(
            json!({"type": "connect", "code": code}).to_string(),
        ))
        .map_err(|e| e.to_string())?;

    let out_tx_for_reader = out_tx.clone();
    *state.out_tx.lock().await = Some(out_tx);

    let app_handle = app.clone();
    let (peer_candidate_tx, peer_candidate_rx) =
        mpsc::unbounded_channel::<std::net::SocketAddrV4>();
    let mut peer_candidate_rx = Some(peer_candidate_rx);
    let out_tx = out_tx_for_reader;

    tokio::spawn(async move {
        struct IncomingTransfer {
            file: File,
            path: PathBuf,
        }
        let mut incoming: HashMap<u32, IncomingTransfer> = HashMap::new();
        let received_dir = received_files_dir();

        // Para el reporte de fps recibidos (calidad adaptativa): el
        // agent usa esto para saber si tiene que subir o bajar la
        // calidad JPEG.
        let mut frames_this_second = 0u32;
        let mut second_start = tokio::time::Instant::now();

        while let Some(msg) = read.next().await {
            let msg = match msg {
                Ok(m) => m,
                Err(_) => break,
            };

            match msg {
                Message::Text(text) => {
                    if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&text) {
                        match parsed["type"].as_str() {
                            Some("paired") => {
                                let _ = app_handle.emit("paired", ());

                                let app_state = app_handle.state::<AppState>();

                                // Cifrado E2E: generamos nuestro par de
                                // claves efimero y lo mandamos. Si el
                                // agent no participa del handshake
                                // (version vieja), no llega respuesta y
                                // seguimos sin cifrar - no rompe nada.
                                let (secret, public_bytes) = crypto::generate_keypair();
                                *app_state.crypto.my_secret.lock().await = Some(secret);
                                let _ = out_tx.send(Message::Binary(netproto::encode_key_exchange(&public_bytes)));

                                // Intentamos abrir un path P2P directo
                                // para el input (mejor latencia). Es
                                // una optimizacion - si falla en
                                // cualquier paso, seguimos mandando
                                // input por el relay sin que se note.
                                if let Some(rx) = peer_candidate_rx.take() {
                                    let app_handle_p2p = app_handle.clone();
                                    let out_tx_p2p = out_tx.clone();
                                    tokio::spawn(async move {
                                        setup_p2p(app_handle_p2p, out_tx_p2p, rx).await;
                                    });
                                }
                            }
                            Some("peer_disconnected") => {
                                let _ = app_handle.emit("peer-disconnected", ());
                            }
                            Some("error") => {
                                let message = parsed["message"]
                                    .as_str()
                                    .unwrap_or("error desconocido")
                                    .to_string();
                                let _ = app_handle.emit("connection-error", ErrorPayload { message });
                            }
                            _ => {}
                        }
                    }
                }
                Message::Binary(bytes) => {
                    tracing::info!("mensaje binario recibido: {} bytes, kind={:?}", bytes.len(), netproto::peek_kind(&bytes));
                    let app_state = app_handle.state::<AppState>();

                    // El intercambio de clave nunca viaja cifrado.
                    if netproto::peek_kind(&bytes) == Some(netproto::KIND_KEY_EXCHANGE) {
                        if let Some(peer_public) = netproto::decode_key_exchange(&bytes) {
                            let secret = app_state.crypto.my_secret.lock().await.take();
                            if let Some(secret) = secret {
                                let key = crypto::derive_session_key(secret, peer_public);
                                *app_state.crypto.cipher.lock().await = Some(Arc::new(SessionCipher::new(key)));
                                tracing::info!("cifrado end-to-end establecido con el agent");
                                let _ = app_handle.emit("e2e-established", ());
                            } else {
                                tracing::warn!("llego la clave del agent pero yo todavia no tenia mi propia clave generada");
                            }
                        } else {
                            tracing::warn!("mensaje de intercambio de clave con formato invalido");
                        }
                        continue;
                    }

                    // Si viene cifrado, lo desciframos primero.
                    let decrypted_owner;
                    let effective: &[u8] = if netproto::peek_kind(&bytes) == Some(netproto::KIND_ENCRYPTED) {
                        let cipher_guard = app_state.crypto.cipher.lock().await;
                        match (cipher_guard.as_ref(), netproto::decode_encrypted(&bytes)) {
                            (Some(cipher), Some((nonce, ciphertext))) => match cipher.decrypt(nonce, ciphertext) {
                                Ok(plain) => {
                                    decrypted_owner = plain;
                                    &decrypted_owner
                                }
                                Err(e) => {
                                    tracing::warn!("no se pudo descifrar un mensaje ({} bytes): {e}", bytes.len());
                                    continue;
                                }
                            },
                            (None, _) => {
                                tracing::warn!("llego un mensaje cifrado pero todavia no tengo la clave ({} bytes)", bytes.len());
                                continue;
                            }
                            (_, None) => {
                                tracing::warn!("mensaje cifrado con formato invalido ({} bytes)", bytes.len());
                                continue;
                            }
                        }
                    } else {
                        &bytes
                    };

                    tracing::info!("mensaje efectivo tras descifrar (si aplicaba): kind={:?}, {} bytes", netproto::peek_kind(effective), effective.len());

                    match netproto::peek_kind(effective) {
                        Some(netproto::KIND_FRAME) => {
                            if let Some(jpeg) = netproto::decode_frame(effective) {
                                // Base64 aca no cuesta ancho de banda de
                                // red - es una llamada local entre el
                                // backend Rust y el WebView.
                                let b64 = STANDARD.encode(jpeg);
                                let _ = app_handle.emit("video-frame", b64);
                                frames_this_second += 1;
                            }

                            if second_start.elapsed().as_secs_f32() >= 1.0 {
                                let fps = frames_this_second as f32 / second_start.elapsed().as_secs_f32();
                                let stats_msg = {
                                    let app_state = app_handle.state::<AppState>();
                                    wrap_outgoing(&app_state.crypto, netproto::encode_stats(fps)).await
                                };
                                let _ = out_tx.send(Message::Binary(stats_msg));
                                frames_this_second = 0;
                                second_start = tokio::time::Instant::now();
                            }
                        }
                        Some(netproto::KIND_CONTROL) => {
                            if let Some(ControlEvent::RestartAck) = netproto::decode_control(effective) {
                                let _ = app_handle.emit("restart-ack", ());
                            }
                        }
                        Some(netproto::KIND_P2P_CANDIDATE) => {
                            if let Some(addr) = netproto::decode_p2p_candidate(effective) {
                                let _ = peer_candidate_tx.send(addr);
                            }
                        }
                        Some(netproto::KIND_FILE) => match netproto::decode_file(effective) {
                            Some(OwnedFileEvent::Offer { transfer_id, name, total_size }) => {
                                let safe_name = std::path::Path::new(&name)
                                    .file_name()
                                    .map(|n| n.to_string_lossy().to_string())
                                    .unwrap_or_else(|| "archivo_recibido".to_string());
                                let dest = received_dir.join(&safe_name);
                                if let Ok(file) = File::create(&dest) {
                                    incoming.insert(transfer_id, IncomingTransfer { file, path: dest });
                                }
                                let _ = app_handle.emit(
                                    "file-incoming",
                                    FileIncomingPayload { name: safe_name, size: total_size },
                                );
                            }
                            Some(OwnedFileEvent::Chunk { transfer_id, data }) => {
                                if let Some(t) = incoming.get_mut(&transfer_id) {
                                    let _ = t.file.write_all(&data);
                                }
                            }
                            Some(OwnedFileEvent::Complete { transfer_id }) => {
                                if let Some(t) = incoming.remove(&transfer_id) {
                                    let _ = app_handle.emit(
                                        "file-received",
                                        t.path.to_string_lossy().to_string(),
                                    );
                                }
                            }
                            None => {}
                        },
                        _ => {}
                    }
                }
                Message::Close(_) => break,
                _ => {}
            }
        }
        let _ = app_handle.emit("peer-disconnected", ());
    });

    Ok(())
}

/// Intenta abrir un path P2P directo para el input (mejor latencia
/// que el relay). Optimizacion pura: si algo falla, no hace nada mas
/// y el input sigue viajando por el relay de siempre.
async fn setup_p2p(
    app_handle: AppHandle,
    out_tx: mpsc::UnboundedSender<Message>,
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
            tracing::info!("P2P: STUN devolvio IPv6, no soportado todavia");
            return;
        }
        Err(e) => {
            tracing::info!("P2P: STUN fallo, sigo con el relay ({e:#})");
            return;
        }
    };
    // El candidato viaja siempre SIN cifrar, por el mismo motivo que
    // en el agent: evita la carrera con el handshake de cifrado (no
    // es informacion sensible de todos modos).
    let _ = out_tx.send(Message::Binary(netproto::encode_p2p_candidate(my_candidate)));

    let peer_candidate =
        match tokio::time::timeout(std::time::Duration::from_secs(5), peer_candidate_rx.recv()).await {
            Ok(Some(addr)) => addr,
            _ => {
                tracing::info!("P2P: no llego el candidato del agent a tiempo, sigo con el relay");
                return;
            }
        };

    let peer_addr = std::net::SocketAddr::V4(peer_candidate);
    match core_engine::net::punch_hole(&socket, peer_addr, 5).await {
        Some(confirmed_addr) => {
            tracing::info!("P2P establecido con {confirmed_addr}");
            *app_handle.state::<AppState>().p2p.lock().await = Some((socket, confirmed_addr));
            let _ = app_handle.emit("p2p-established", ());
        }
        None => {
            tracing::info!("P2P: no se pudo abrir el path directo, sigo con el relay");
        }
    }
}

async fn send_bytes(state: &State<'_, AppState>, plaintext: Vec<u8>) -> Result<(), String> {
    let msg = wrap_outgoing(&state.crypto, plaintext).await;
    let guard = state.out_tx.lock().await;
    match guard.as_ref() {
        Some(tx) => tx.send(Message::Binary(msg)).map_err(|e| e.to_string()),
        None => Err("no hay conexion activa".to_string()),
    }
}

/// Como send_bytes, pero para eventos de input: si hay un path P2P
/// establecido, lo manda directo por UDP (mas rapido); si no, cae al
/// relay de siempre via WebSocket. El input tambien se cifra en
/// cualquiera de los dos caminos.
async fn send_input_bytes(state: &State<'_, AppState>, plaintext: Vec<u8>) -> Result<(), String> {
    let msg = wrap_outgoing(&state.crypto, plaintext).await;
    {
        let p2p_guard = state.p2p.lock().await;
        if let Some((socket, addr)) = p2p_guard.as_ref() {
            if socket.send_to(&msg, *addr).await.is_ok() {
                return Ok(());
            }
        }
    }
    let guard = state.out_tx.lock().await;
    match guard.as_ref() {
        Some(tx) => tx.send(Message::Binary(msg)).map_err(|e| e.to_string()),
        None => Err("no hay conexion activa".to_string()),
    }
}

#[tauri::command]
async fn send_mouse_move(state: State<'_, AppState>, x: f32, y: f32) -> Result<(), String> {
    send_input_bytes(&state, netproto::encode_input(InputEvent::MouseMove { x, y })).await
}

#[tauri::command]
async fn send_mouse_button(state: State<'_, AppState>, button: u8, pressed: bool) -> Result<(), String> {
    send_input_bytes(&state, netproto::encode_input(InputEvent::MouseButton { button, pressed })).await
}

#[tauri::command]
async fn send_mouse_wheel(state: State<'_, AppState>, delta: i32) -> Result<(), String> {
    send_input_bytes(&state, netproto::encode_input(InputEvent::MouseWheel { delta })).await
}

#[tauri::command]
async fn send_key(state: State<'_, AppState>, vk: u16, pressed: bool) -> Result<(), String> {
    send_input_bytes(&state, netproto::encode_input(InputEvent::Key { vk, pressed })).await
}

#[tauri::command]
async fn send_restart(state: State<'_, AppState>, delay_secs: u8) -> Result<(), String> {
    send_bytes(&state, netproto::encode_control(ControlEvent::RestartRequest { delay_secs })).await
}

#[tauri::command]
async fn send_file(state: State<'_, AppState>, path: String) -> Result<(), String> {
    let data = std::fs::read(&path).map_err(|e| e.to_string())?;
    let name = std::path::Path::new(&path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "archivo".to_string());
    let transfer_id: u32 = (std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_nanos()
        % u32::MAX as u128) as u32;
    let total_size = data.len() as u64;

    send_bytes(
        &state,
        netproto::encode_file(FileEvent::Offer { transfer_id, name: &name, total_size }),
    )
    .await?;

    for chunk in data.chunks(netproto::FILE_CHUNK_SIZE) {
        send_bytes(&state, netproto::encode_file(FileEvent::Chunk { transfer_id, data: chunk })).await?;
    }

    send_bytes(&state, netproto::encode_file(FileEvent::Complete { transfer_id })).await?;
    Ok(())
}

fn main() {
    tracing_subscriber::fmt::init();

    tauri::Builder::default()
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![
            connect,
            send_mouse_move,
            send_mouse_button,
            send_mouse_wheel,
            send_key,
            send_restart,
            send_file
        ])
        .run(tauri::generate_context!())
        .expect("error corriendo la app controller-ui");
}
