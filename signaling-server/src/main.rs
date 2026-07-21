//! Signaling server: coordina el handshake entre controller y agent.
//!
//! Responsabilidades:
//! - Registrar agentes conectados (ID + clave publica).
//! - Autenticar sesiones por codigo temporal o codigo fijo.
//! - Intercambiar info de NAT/IP entre las dos puntas para intentar P2P.
//! - Si el P2P falla, indicarles a ambas puntas que usen el relay-server.
//!
//! Pensado para correr en el homelab de Nicolas (Docker + Cloudflare Tunnel).

fn main() {
    tracing_subscriber::fmt::init();
    tracing::info!("signaling-server iniciado (placeholder) - proximo paso: servidor websocket");
}
