//! Simula un segundo agent que intenta registrarse con el MISMO
//! codigo que otro ya conectado - para probar que el servidor lo
//! rechaza en vez de pisar el registro anterior.
//!
//! Correr con (con test_agent ya corriendo con el codigo 123456):
//!   cargo run --example test_agent_dup -p signaling_server

use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let (ws_stream, _) = connect_async("ws://127.0.0.1:8080").await?;
    let (mut write, mut read) = ws_stream.split();

    let register = json!({"type": "register_agent", "code": "123456"});
    write.send(Message::Text(register.to_string())).await?;
    println!("[agent duplicado] intentando registrarme con codigo 123456 (ya deberia estar en uso)...");

    if let Some(msg) = read.next().await {
        let Message::Text(text) = msg? else { return Ok(()) };
        println!("[agent duplicado] recibido: {text}");
        if text.contains("\"error\"") {
            println!("[agent duplicado] CORRECTO: el servidor rechazo el codigo duplicado.");
        } else {
            println!("[agent duplicado] MAL: el servidor deberia haber rechazado esto.");
        }
    }

    Ok(())
}
