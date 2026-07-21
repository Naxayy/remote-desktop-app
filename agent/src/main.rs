//! Agent: corre en la PC remota (la que se va a controlar).
//!
//! Responsabilidades:
//! - Instalarse y correr como servicio de Windows (arranca antes del login).
//! - Levantar el core_engine (captura + input + red).
//! - Generar/validar el codigo de sesion (temporal o fijo).
//! - Manejar el comando de reinicio remoto y reconectar solo despues del boot.
//!
//! Por ahora es un placeholder que solo loguea que arranco.

fn main() {
    tracing_subscriber::fmt::init();
    tracing::info!("agent iniciado (placeholder) - proximo paso: servicio de Windows + engine");
}
