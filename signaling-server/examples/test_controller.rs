//! Simula un controller: se conecta al codigo 123456, y al emparejarse
//! manda un mensaje BINARIO de prueba.
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
        match msg? {
            Message::Text(text) => {
                println!("[controller] control recibido: {text}");
                let parsed: serde_json::Value = serde_json::from_str(&text)?;
                if parsed["type"] == "paired" {
                    println!("[controller] emparejado! mandando bytes binarios de prueba...");
                    let hello = b"hola agent, soy el controller (binario)".to_vec();
                    write.send(Message::Binary(hello)).await?;
                }
            }
            Message::Binary(bytes) => {
                println!("[controller] binario recibido ({} bytes): {:?}", bytes.len(), bytes);
                println!("[controller] round-trip binario completo, listo.");
                break;
            }
            _ => {}
        }
    }

    Ok(())
}
