//! Simula un agent: se conecta al signaling server, se registra con
//! un codigo fijo, y espera mensajes de relay (los imprime y
//! responde con un eco).
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
        let Message::Text(text) = msg? else { continue };
        println!("[agent] recibido: {text}");

        let parsed: serde_json::Value = serde_json::from_str(&text)?;
        if parsed["type"] == "paired" {
            println!("[agent] emparejado con un controller!");
        }
        if parsed["type"] == "relay" {
            println!("[agent] mensaje del controller: {:?}", parsed["payload"]);
            let reply = json!({"type": "relay", "payload": {"from": "agent", "text": "hola controller, soy el agent"}});
            write.send(Message::Text(reply.to_string())).await?;
        }
    }

    Ok(())
}
