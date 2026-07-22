//! Signaling server: coordina el handshake entre controller y agent.
//!
//! Responsabilidades actuales:
//! - Registrar agentes conectados bajo un codigo (temporal random o
//!   fijo, segun lo que mande el agent).
//! - Emparejar un controller con el agent que tiene el codigo pedido.
//! - Reenviar mensajes de aplicacion entre las dos puntas ya
//!   emparejadas (por ahora generico via ClientMessage::Relay -
//!   mas adelante esto va a llevar SDP/ICE para negociar P2P, o
//!   directamente los frames de video/input mientras no tengamos
//!   conexion P2P real).
//!
//! Pensado para correr en el homelab de Nicolas (Docker + Cloudflare
//! Tunnel) y ser self-hosteable por cualquiera que clone el repo.

mod protocol;

use anyhow::Result;
use futures_util::{SinkExt, StreamExt};
use protocol::{ClientMessage, ServerMessage};
use rand::Rng;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;
use uuid::Uuid;

#[derive(Default)]
struct AppState {
    /// codigo -> id de conexion del agent que lo registro, esperando
    /// a que un controller se conecte.
    pending_agents: Mutex<HashMap<String, Uuid>>,
    /// id de conexion -> canal para mandarle mensajes a esa conexion.
    connections: Mutex<HashMap<Uuid, mpsc::UnboundedSender<Message>>>,
    /// id de conexion -> id de conexion del peer, una vez emparejados.
    /// Se guarda en ambos sentidos para que el relay sea simetrico.
    pairs: Mutex<HashMap<Uuid, Uuid>>,
}

fn generate_code() -> String {
    let n: u32 = rand::thread_rng().gen_range(0..1_000_000);
    format!("{n:06}")
}

fn send_to(state: &AppState, target: Uuid, msg: ServerMessage) {
    let text = match serde_json::to_string(&msg) {
        Ok(t) => t,
        Err(e) => {
            tracing::error!("no se pudo serializar ServerMessage: {e}");
            return;
        }
    };
    if let Some(sender) = state.connections.lock().unwrap().get(&target) {
        let _ = sender.send(Message::Text(text));
    }
}

async fn handle_connection(stream: TcpStream, state: Arc<AppState>) -> Result<()> {
    let ws_stream = tokio_tungstenite::accept_async(stream).await?;
    let (mut write, mut read) = ws_stream.split();

    let conn_id = Uuid::new_v4();
    let (tx, mut rx) = mpsc::unbounded_channel::<Message>();
    state.connections.lock().unwrap().insert(conn_id, tx);

    // Tarea que vuelca el canal hacia el socket real. Separarlo asi
    // permite que cualquier otra tarea (el handler de OTRA conexion,
    // al hacer relay) le mande mensajes a esta conexion sin pelearse
    // por el mismo `write` mutable.
    tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            if write.send(msg).await.is_err() {
                break;
            }
        }
    });

    let mut registered_code: Option<String> = None;

    while let Some(msg) = read.next().await {
        let msg = match msg {
            Ok(m) => m,
            Err(_) => break,
        };

        let text = match msg {
            Message::Text(t) => t,
            Message::Close(_) => break,
            _ => continue,
        };

        match serde_json::from_str::<ClientMessage>(&text) {
            Ok(ClientMessage::RegisterAgent { code }) => {
                let code = code.unwrap_or_else(generate_code);
                state
                    .pending_agents
                    .lock()
                    .unwrap()
                    .insert(code.clone(), conn_id);
                registered_code = Some(code.clone());
                tracing::info!("agent {conn_id} registrado con codigo {code}");
                send_to(&state, conn_id, ServerMessage::Registered { code });
            }

            Ok(ClientMessage::Connect { code }) => {
                let agent_id = state.pending_agents.lock().unwrap().remove(&code);
                match agent_id {
                    Some(agent_id) => {
                        state.pairs.lock().unwrap().insert(conn_id, agent_id);
                        state.pairs.lock().unwrap().insert(agent_id, conn_id);
                        tracing::info!("emparejados: controller {conn_id} <-> agent {agent_id}");
                        send_to(&state, conn_id, ServerMessage::Paired);
                        send_to(&state, agent_id, ServerMessage::Paired);
                    }
                    None => {
                        send_to(
                            &state,
                            conn_id,
                            ServerMessage::Error {
                                message: format!("codigo '{code}' no encontrado"),
                            },
                        );
                    }
                }
            }

            Ok(ClientMessage::Relay { payload }) => {
                let peer = state.pairs.lock().unwrap().get(&conn_id).copied();
                match peer {
                    Some(peer_id) => send_to(&state, peer_id, ServerMessage::Relay { payload }),
                    None => send_to(
                        &state,
                        conn_id,
                        ServerMessage::Error {
                            message: "todavia no estas emparejado con nadie".into(),
                        },
                    ),
                }
            }

            Err(e) => {
                send_to(
                    &state,
                    conn_id,
                    ServerMessage::Error {
                        message: format!("mensaje invalido: {e}"),
                    },
                );
            }
        }
    }

    // Limpieza al desconectarse: sacarlo de todos lados y avisarle al
    // peer (si tenia uno) que se quedo solo.
    state.connections.lock().unwrap().remove(&conn_id);
    if let Some(code) = registered_code {
        state.pending_agents.lock().unwrap().remove(&code);
    }
    let peer = state.pairs.lock().unwrap().remove(&conn_id);
    if let Some(peer_id) = peer {
        state.pairs.lock().unwrap().remove(&peer_id);
        send_to(&state, peer_id, ServerMessage::PeerDisconnected);
    }
    tracing::info!("conexion {conn_id} cerrada");

    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let addr = std::env::var("SIGNALING_ADDR").unwrap_or_else(|_| "0.0.0.0:8080".to_string());
    let listener = TcpListener::bind(&addr).await?;
    tracing::info!("signaling-server escuchando en {addr}");

    let state = Arc::new(AppState::default());

    loop {
        let (stream, peer_addr) = listener.accept().await?;
        let state = Arc::clone(&state);
        tokio::spawn(async move {
            if let Err(e) = handle_connection(stream, state).await {
                tracing::warn!("conexion desde {peer_addr} termino con error: {e}");
            }
        });
    }
}
