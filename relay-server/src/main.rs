//! Relay server: fallback tipo TURN cuando la conexion P2P directa
//! entre controller y agent no es posible (NAT simetrico, redes
//! corporativas cerradas, etc). Todo el trafico de la sesion pasa
//! por aca, cifrado end-to-end (el relay no puede leer el contenido).

fn main() {
    tracing_subscriber::fmt::init();
    tracing::info!("relay-server iniciado (placeholder) - proximo paso: forwarding de paquetes cifrados");
}
