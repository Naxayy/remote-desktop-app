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
use base64::{engine::general_purpose::STANDARD, Engine as _};
use core_engine::capture::ScreenCapturer;
use core_engine::encode::VideoEncoder;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
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

fn spawn_capture_thread(
    latest: Arc<Mutex<Option<Vec<u8>>>>,
    frame_id: Arc<AtomicU64>,
    paired: Arc<AtomicBool>,
) {
    thread::spawn(move || {
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
            tracing::error!("hilo de captura termino con error: {e:#}");
        }
    });
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

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
                    let b64 = STANDARD.encode(&frame);
                    let msg = json!({"type": "relay", "payload": {"kind": "frame", "data": b64}});
                    let _ = out_tx.send(Message::Text(msg.to_string()));
                }
            }
        });
    }

    // Loop principal: procesa los mensajes de control del signaling
    // server (registro confirmado, emparejamiento, desconexion, etc).
    while let Some(msg) = read.next().await {
        let msg = match msg {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!("error leyendo del signaling server: {e}");
                break;
            }
        };
        let Message::Text(text) = msg else { continue };
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
            Some("relay") => {
                tracing::debug!("relay recibido del controller (input, sin implementar todavia)");
            }
            _ => {}
        }
    }

    Ok(())
}
