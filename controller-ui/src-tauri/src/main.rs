//! Backend Rust de la app controladora (Tauri). Reemplaza a dev_viewer:
//! misma logica de red (conectarse al signaling server, emparejarse,
//! mandar/recibir video-input-archivos-control via netproto), pero
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
use core_engine::netproto::{self, ControlEvent, FileEvent, InputEvent, OwnedFileEvent};
use futures_util::{SinkExt, StreamExt};
use serde::Serialize;
use serde_json::json;
use std::collections::HashMap;
use std::fs::File;
use std::io::Write;
use std::path::PathBuf;
use tauri::{AppHandle, Emitter, State};
use tokio::sync::{mpsc, Mutex};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;

/// Estado compartido: el canal para mandar mensajes por la conexion
/// activa (si hay una). Envuelto en Mutex porque varios comandos
/// Tauri pueden querer mandar cosas a la vez (input llega seguido).
#[derive(Default)]
struct AppState {
    out_tx: Mutex<Option<mpsc::UnboundedSender<Message>>>,
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

    *state.out_tx.lock().await = Some(out_tx);

    let app_handle = app.clone();
    tokio::spawn(async move {
        struct IncomingTransfer {
            file: File,
            path: PathBuf,
        }
        let mut incoming: HashMap<u32, IncomingTransfer> = HashMap::new();
        let received_dir = received_files_dir();

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
                Message::Binary(bytes) => match netproto::peek_kind(&bytes) {
                    Some(netproto::KIND_FRAME) => {
                        if let Some(jpeg) = netproto::decode_frame(&bytes) {
                            // Base64 aca no cuesta nada de ancho de banda de
                            // red - es una llamada local entre el backend
                            // Rust y el WebView, no viaja por internet.
                            let b64 = STANDARD.encode(jpeg);
                            let _ = app_handle.emit("video-frame", b64);
                        }
                    }
                    Some(netproto::KIND_CONTROL) => {
                        if let Some(ControlEvent::RestartAck) = netproto::decode_control(&bytes) {
                            let _ = app_handle.emit("restart-ack", ());
                        }
                    }
                    Some(netproto::KIND_FILE) => match netproto::decode_file(&bytes) {
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
                },
                Message::Close(_) => break,
                _ => {}
            }
        }
        let _ = app_handle.emit("peer-disconnected", ());
    });

    Ok(())
}

async fn send_bytes(state: &State<'_, AppState>, msg: Vec<u8>) -> Result<(), String> {
    let guard = state.out_tx.lock().await;
    match guard.as_ref() {
        Some(tx) => tx.send(Message::Binary(msg)).map_err(|e| e.to_string()),
        None => Err("no hay conexion activa".to_string()),
    }
}

#[tauri::command]
async fn send_mouse_move(state: State<'_, AppState>, x: f32, y: f32) -> Result<(), String> {
    send_bytes(&state, netproto::encode_input(InputEvent::MouseMove { x, y })).await
}

#[tauri::command]
async fn send_mouse_button(state: State<'_, AppState>, button: u8, pressed: bool) -> Result<(), String> {
    send_bytes(&state, netproto::encode_input(InputEvent::MouseButton { button, pressed })).await
}

#[tauri::command]
async fn send_mouse_wheel(state: State<'_, AppState>, delta: i32) -> Result<(), String> {
    send_bytes(&state, netproto::encode_input(InputEvent::MouseWheel { delta })).await
}

#[tauri::command]
async fn send_key(state: State<'_, AppState>, vk: u16, pressed: bool) -> Result<(), String> {
    send_bytes(&state, netproto::encode_input(InputEvent::Key { vk, pressed })).await
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
