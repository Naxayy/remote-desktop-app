//! Simula un agent: se registra, y al emparejarse manda un mensaje
//! BINARIO de prueba y espera la respuesta binaria del controller.
//!
//! Correr con:
//!   cargo run --example test_agent -p signaling_server

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
    println!("[agent] registrandome con codigo fijo 123456...");

    while let Some(msg) = read.next().await {
        match msg? {
            Message::Text(text) => {
                println!("[agent] control recibido: {text}");
                let parsed: serde_json::Value = serde_json::from_str(&text)?;
                if parsed["type"] == "paired" {
                    println!("[agent] emparejado! esperando bytes binarios del controller...");
                }
            }
            Message::Binary(bytes) => {
                println!("[agent] binario recibido ({} bytes): {:?}", bytes.len(), bytes);
                let reply = b"hola controller, soy el agent (binario)".to_vec();
                write.send(Message::Binary(reply)).await?;
            }
            _ => {}
        }
    }

    Ok(())
}
