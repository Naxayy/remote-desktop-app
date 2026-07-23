//! Visor de desarrollo: hace de "controller" temporal mientras no
//! tenemos la UI real de Tauri lista. Se conecta al signaling server,
//! pide el codigo del agent, y muestra los frames que le van llegando
//! por el relay.
//!
//! Este archivo es una herramienta de prueba, no el controller final -
//! cuando controller-ui (Tauri) este listo, esta logica de red se
//! muda para alla.
//!
//! Correr con:
//!   cargo run --example dev_viewer -p agent --release

#[cfg(windows)]
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    use core_engine::encode::VideoDecoder;
    use futures_util::{SinkExt, StreamExt};
    use minifb::{Window, WindowOptions};
    use serde_json::{json, Value};
    use std::sync::{Arc, Mutex};
    use tokio_tungstenite::connect_async;
    use tokio_tungstenite::tungstenite::Message;

    let signaling_url =
        std::env::var("SIGNALING_URL").unwrap_or_else(|_| "ws://127.0.0.1:8080".to_string());
    let code = std::env::var("AGENT_CODE").unwrap_or_else(|_| "123456".to_string());

    println!("conectando a {signaling_url}...");
    let (ws_stream, _) = connect_async(&signaling_url).await?;
    let (mut write, mut read) = ws_stream.split();

    write
        .send(Message::Text(
            json!({"type": "connect", "code": code}).to_string(),
        ))
        .await?;
    println!("pidiendo conexion al codigo {code}...");

    let latest_jpeg: Arc<Mutex<Option<Vec<u8>>>> = Arc::new(Mutex::new(None));

    {
        let latest_jpeg = Arc::clone(&latest_jpeg);
        tokio::spawn(async move {
            while let Some(msg) = read.next().await {
                let Ok(Message::Text(text)) = msg else { continue };
                let Ok(parsed) = serde_json::from_str::<Value>(&text) else { continue };
                match parsed["type"].as_str() {
                    Some("paired") => println!("emparejado con el agent, esperando video..."),
                    Some("relay") => {
                        if parsed["payload"]["kind"] == "frame" {
                            if let Some(b64) = parsed["payload"]["data"].as_str() {
                                if let Ok(bytes) = STANDARD.decode(b64) {
                                    *latest_jpeg.lock().unwrap() = Some(bytes);
                                }
                            }
                        }
                    }
                    Some("error") => eprintln!("error del servidor: {}", parsed["message"]),
                    Some("peer_disconnected") => println!("el agent se desconecto"),
                    _ => {}
                }
            }
        });
    }

    let mut window: Option<Window> = None;
    let mut buffer: Vec<u32> = Vec::new();
    let mut current_size = (0usize, 0usize);
    let mut frames_shown = 0u64;
    let mut last_report = std::time::Instant::now();

    loop {
        let jpeg = { latest_jpeg.lock().unwrap().take() };
        if let Some(jpeg) = jpeg {
            match VideoDecoder::decode(&jpeg) {
                Ok(decoded) => {
                    let (w, h) = (decoded.width as usize, decoded.height as usize);
                    if current_size != (w, h) || window.is_none() {
                        current_size = (w, h);
                        buffer = vec![0u32; w * h];
                        window = Some(Window::new(
                            "dev_viewer - streaming remoto via relay",
                            w,
                            h,
                            WindowOptions::default(),
                        )?);
                    }
                    for (i, px) in decoded.data.chunks_exact(4).enumerate() {
                        let (b, g, r) = (px[0] as u32, px[1] as u32, px[2] as u32);
                        buffer[i] = (r << 16) | (g << 8) | b;
                    }
                    if let Some(win) = window.as_mut() {
                        win.update_with_buffer(&buffer, current_size.0, current_size.1)?;
                    }
                    frames_shown += 1;
                }
                Err(e) => eprintln!("frame corrupto, se descarta: {e}"),
            }
        } else if let Some(win) = window.as_mut() {
            win.update();
        }

        if last_report.elapsed().as_secs_f64() >= 1.0 {
            println!("{} fps recibidos", frames_shown);
            frames_shown = 0;
            last_report = std::time::Instant::now();
        }

        if let Some(win) = &window {
            if !win.is_open() {
                break;
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }

    Ok(())
}

#[cfg(not(windows))]
fn main() {
    eprintln!("este ejemplo solo corre en Windows (necesita minifb + captura remota real)");
}
