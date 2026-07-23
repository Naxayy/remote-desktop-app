//! Visor de desarrollo: hace de "controller" temporal mientras no
//! tenemos la UI real de Tauri lista. Se conecta al signaling server,
//! pide el codigo del agent, muestra los frames que le van llegando, y
//! ahora tambien manda el mouse/teclado de esta ventana como input
//! remoto para que el agent lo inyecte.
//!
//! Este archivo es una herramienta de prueba, no el controller final -
//! cuando controller-ui (Tauri) este listo, esta logica de red se
//! muda para alla.
//!
//! Correr con:
//!   cargo run --example dev_viewer -p agent --release

#[cfg(windows)]
fn key_to_vk(key: minifb::Key) -> Option<u16> {
    use minifb::Key;
    Some(match key {
        Key::A => 0x41, Key::B => 0x42, Key::C => 0x43, Key::D => 0x44,
        Key::E => 0x45, Key::F => 0x46, Key::G => 0x47, Key::H => 0x48,
        Key::I => 0x49, Key::J => 0x4A, Key::K => 0x4B, Key::L => 0x4C,
        Key::M => 0x4D, Key::N => 0x4E, Key::O => 0x4F, Key::P => 0x50,
        Key::Q => 0x51, Key::R => 0x52, Key::S => 0x53, Key::T => 0x54,
        Key::U => 0x55, Key::V => 0x56, Key::W => 0x57, Key::X => 0x58,
        Key::Y => 0x59, Key::Z => 0x5A,
        Key::Key0 => 0x30, Key::Key1 => 0x31, Key::Key2 => 0x32, Key::Key3 => 0x33,
        Key::Key4 => 0x34, Key::Key5 => 0x35, Key::Key6 => 0x36, Key::Key7 => 0x37,
        Key::Key8 => 0x38, Key::Key9 => 0x39,
        Key::Space => 0x20,
        Key::Enter => 0x0D,
        Key::Backspace => 0x08,
        Key::Tab => 0x09,
        Key::Escape => 0x1B,
        Key::Left => 0x25,
        Key::Up => 0x26,
        Key::Right => 0x27,
        Key::Down => 0x28,
        Key::Delete => 0x2E,
        Key::Home => 0x24,
        Key::End => 0x23,
        Key::PageUp => 0x21,
        Key::PageDown => 0x22,
        Key::LeftShift => 0xA0,
        Key::RightShift => 0xA1,
        Key::LeftCtrl => 0xA2,
        Key::RightCtrl => 0xA3,
        Key::LeftAlt => 0xA4,
        Key::RightAlt => 0xA5,
        Key::CapsLock => 0x14,
        Key::F1 => 0x70, Key::F2 => 0x71, Key::F3 => 0x72, Key::F4 => 0x73,
        Key::F5 => 0x74, Key::F6 => 0x75, Key::F7 => 0x76, Key::F8 => 0x77,
        Key::F9 => 0x78, Key::F10 => 0x79, Key::F11 => 0x7A, Key::F12 => 0x7B,
        _ => return None,
    })
}

#[cfg(windows)]
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    use core_engine::encode::VideoDecoder;
    use futures_util::{SinkExt, StreamExt};
    use minifb::{Key, MouseButton as MinifbMouseButton, MouseMode, Window, WindowOptions};
    use serde_json::{json, Value};
    use std::collections::HashSet;
    use std::sync::{Arc, Mutex};
    use tokio::sync::mpsc;
    use tokio_tungstenite::connect_async;
    use tokio_tungstenite::tungstenite::Message;

    let signaling_url =
        std::env::var("SIGNALING_URL").unwrap_or_else(|_| "ws://127.0.0.1:8080".to_string());
    let code = std::env::var("AGENT_CODE").unwrap_or_else(|_| "123456".to_string());

    println!("conectando a {signaling_url}...");
    let (ws_stream, _) = connect_async(&signaling_url).await?;
    let (mut write, mut read) = ws_stream.split();

    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<Message>();
    tokio::spawn(async move {
        while let Some(msg) = out_rx.recv().await {
            if write.send(msg).await.is_err() {
                break;
            }
        }
    });

    out_tx.send(Message::Text(
        json!({"type": "connect", "code": code}).to_string(),
    ))?;
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

    // Estado previo de input, para mandar solo los CAMBIOS (evita
    // saturar la red mandando "sigue apretado" 200 veces por segundo).
    let mut prev_mouse_pos: Option<(f32, f32)> = None;
    let mut prev_buttons = [false, false, false]; // left, right, middle
    let mut prev_keys: HashSet<Key> = HashSet::new();

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

        // --- Capturar y mandar input de esta ventana ---
        if let Some(win) = &window {
            if current_size.0 > 0 && current_size.1 > 0 {
                if let Some((mx, my)) = win.get_mouse_pos(MouseMode::Clamp) {
                    let nx = (mx / current_size.0 as f32) as f64;
                    let ny = (my / current_size.1 as f32) as f64;
                    if prev_mouse_pos != Some((mx, my)) {
                        prev_mouse_pos = Some((mx, my));
                        let msg = json!({"type": "relay", "payload": {"kind": "input", "event": {"type": "mouse_move", "x": nx, "y": ny}}});
                        let _ = out_tx.send(Message::Text(msg.to_string()));
                    }
                }

                let buttons = [
                    win.get_mouse_down(MinifbMouseButton::Left),
                    win.get_mouse_down(MinifbMouseButton::Right),
                    win.get_mouse_down(MinifbMouseButton::Middle),
                ];
                let names = ["left", "right", "middle"];
                for i in 0..3 {
                    if buttons[i] != prev_buttons[i] {
                        let msg = json!({"type": "relay", "payload": {"kind": "input", "event": {"type": "mouse_button", "button": names[i], "pressed": buttons[i]}}});
                        let _ = out_tx.send(Message::Text(msg.to_string()));
                    }
                }
                prev_buttons = buttons;
            }

            let current_keys: HashSet<Key> = win.get_keys().into_iter().collect();
            for key in current_keys.difference(&prev_keys) {
                if let Some(vk) = key_to_vk(*key) {
                    let msg = json!({"type": "relay", "payload": {"kind": "input", "event": {"type": "key", "vk": vk, "pressed": true}}});
                    let _ = out_tx.send(Message::Text(msg.to_string()));
                }
            }
            for key in prev_keys.difference(&current_keys) {
                if let Some(vk) = key_to_vk(*key) {
                    let msg = json!({"type": "relay", "payload": {"kind": "input", "event": {"type": "key", "vk": vk, "pressed": false}}});
                    let _ = out_tx.send(Message::Text(msg.to_string()));
                }
            }
            prev_keys = current_keys;
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
