//! Simula un controller: se conecta al signaling server, pide
//! conectarse al agent con codigo 123456, y manda un mensaje de
//! prueba una vez emparejado.
//!
//! Correr con (con test_agent ya corriendo en otra terminal):
//!   cargo run --example test_controller -p signaling_server

use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let (ws_stream, _) = connect_async("ws://127.0.0.1:8080").await?;
    let (mut write, mut read) = ws_stream.split();

    let connect = json!({"type": "connect", "code": "123456"});
    write.send(Message::Text(connect.to_string())).await?;
    println!("[controller] pidiendo conexion al codigo 123456...");

    while let Some(msg) = read.next().await {
        let Message::Text(text) = msg? else { continue };
        println!("[controller] recibido: {text}");

        let parsed: serde_json::Value = serde_json::from_str(&text)?;
        if parsed["type"] == "paired" {
            println!("[controller] emparejado! mandando mensaje de prueba...");
            let hello = json!({"type": "relay", "payload": {"from": "controller", "text": "hola agent, soy el controller"}});
            write.send(Message::Text(hello.to_string())).await?;
        }
        if parsed["type"] == "relay" {
            println!("[controller] respuesta del agent: {:?}", parsed["payload"]);
            println!("[controller] round-trip completo, listo.");
            break;
        }
    }

    Ok(())
}
