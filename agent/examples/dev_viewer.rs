//! Visor de desarrollo: hace de "controller" temporal mientras no
//! tenemos la UI real de Tauri lista. Se conecta al signaling server,
//! pide el codigo del agent, muestra los frames que le van llegando, y
//! manda el mouse/teclado de esta ventana como input remoto.
//!
//! Tambien soporta pedir un reinicio remoto (escribi "restart" + Enter
//! en esta consola) y se reconecta solo si la conexion se corta - por
//! ejemplo, justo lo que pasa cuando la PC remota se reinicia: el
//! agent (como servicio de Windows, AutoStart) vuelve a registrarse
//! con el mismo codigo apenas Windows termina de arrancar, y este
//! visor reintenta conectarse cada pocos segundos hasta encontrarlo
//! de nuevo.
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
#[derive(Debug, Clone)]
enum ConsoleCommand {
    Restart,
    SendFile(String),
}

#[cfg(windows)]
async fn send_file(out_tx: &tokio::sync::mpsc::UnboundedSender<tokio_tungstenite::tungstenite::Message>, path: &str) {
    use core_engine::netproto::{self, FileEvent};
    use tokio_tungstenite::tungstenite::Message;

    let path = std::path::Path::new(path.trim());
    let data = match std::fs::read(path) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("no se pudo leer '{}': {e}", path.display());
            return;
        }
    };
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "archivo".to_string());
    let transfer_id: u32 = rand_transfer_id();
    let total_size = data.len() as u64;

    println!("mandando '{name}' ({total_size} bytes)...");
    let offer = netproto::encode_file(FileEvent::Offer { transfer_id, name: &name, total_size });
    let _ = out_tx.send(Message::Binary(offer));

    for chunk in data.chunks(netproto::FILE_CHUNK_SIZE) {
        let msg = netproto::encode_file(FileEvent::Chunk { transfer_id, data: chunk });
        if out_tx.send(Message::Binary(msg)).is_err() {
            return;
        }
    }

    let complete = netproto::encode_file(FileEvent::Complete { transfer_id });
    let _ = out_tx.send(Message::Binary(complete));
    println!("'{name}' mandado completo.");
}

#[cfg(windows)]
fn rand_transfer_id() -> u32 {
    use std::time::{SystemTime, UNIX_EPOCH};
    // No hace falta criptograficamente aleatorio, solo que no se
    // repita entre transferencias concurrentes (que por ahora no
    // soportamos de todos modos - una a la vez).
    (SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos() % u32::MAX as u128) as u32
}


#[cfg(windows)]
async fn run_session(
    signaling_url: &str,
    code: &str,
    commands_rx: &mut tokio::sync::mpsc::UnboundedReceiver<ConsoleCommand>,
) -> anyhow::Result<()> {
    use core_engine::encode::VideoDecoder;
    use core_engine::netproto::{self, ControlEvent, InputEvent};
    use futures_util::{SinkExt, StreamExt};
    use minifb::{Key, MouseButton as MinifbMouseButton, MouseMode, Window, WindowOptions};
    use serde_json::{json, Value};
    use std::collections::HashSet;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use tokio::sync::mpsc;
    use tokio_tungstenite::connect_async;
    use tokio_tungstenite::tungstenite::Message;

    let (ws_stream, _) = connect_async(signaling_url).await?;
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
    let session_alive = Arc::new(AtomicBool::new(true));

    {
        let latest_jpeg = Arc::clone(&latest_jpeg);
        let session_alive = Arc::clone(&session_alive);
        tokio::spawn(async move {
            use std::collections::HashMap;
            use std::fs::File;
            use std::io::Write;
            use core_engine::netproto::OwnedFileEvent;

            struct IncomingTransfer {
                file: File,
                path: std::path::PathBuf,
            }
            let mut incoming_transfers: HashMap<u32, IncomingTransfer> = HashMap::new();
            let received_dir = std::env::var("USERPROFILE")
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|_| std::path::PathBuf::from("."))
                .join("Downloads")
                .join("RemoteDesktopApp-Received");
            let _ = std::fs::create_dir_all(&received_dir);

            while let Some(msg) = read.next().await {
                match msg {
                    Ok(Message::Text(text)) => {
                        let Ok(parsed) = serde_json::from_str::<Value>(&text) else { continue };
                        match parsed["type"].as_str() {
                            Some("paired") => println!("emparejado con el agent, esperando video..."),
                            Some("error") => eprintln!("error del servidor: {}", parsed["message"]),
                            Some("peer_disconnected") => {
                                println!("el agent se desconecto");
                                session_alive.store(false, Ordering::Relaxed);
                                break;
                            }
                            _ => {}
                        }
                    }
                    Ok(Message::Binary(bytes)) => match netproto::peek_kind(&bytes) {
                        Some(netproto::KIND_FRAME) => {
                            if let Some(jpeg) = netproto::decode_frame(&bytes) {
                                *latest_jpeg.lock().unwrap() = Some(jpeg.to_vec());
                            }
                        }
                        Some(netproto::KIND_CONTROL) => {
                            if let Some(ControlEvent::RestartAck) = netproto::decode_control(&bytes) {
                                println!("el agent confirmo el reinicio, va a desconectarse...");
                            }
                        }
                        Some(netproto::KIND_FILE) => match netproto::decode_file(&bytes) {
                            Some(OwnedFileEvent::Offer { transfer_id, name, total_size }) => {
                                let safe_name = std::path::Path::new(&name)
                                    .file_name()
                                    .map(|n| n.to_string_lossy().to_string())
                                    .unwrap_or_else(|| "archivo_recibido".to_string());
                                let dest = received_dir.join(&safe_name);
                                println!("recibiendo '{safe_name}' ({total_size} bytes)...");
                                if let Ok(file) = File::create(&dest) {
                                    incoming_transfers
                                        .insert(transfer_id, IncomingTransfer { file, path: dest });
                                }
                            }
                            Some(OwnedFileEvent::Chunk { transfer_id, data }) => {
                                if let Some(t) = incoming_transfers.get_mut(&transfer_id) {
                                    let _ = t.file.write_all(&data);
                                }
                            }
                            Some(OwnedFileEvent::Complete { transfer_id }) => {
                                if let Some(t) = incoming_transfers.remove(&transfer_id) {
                                    println!("archivo recibido: {}", t.path.display());
                                }
                            }
                            None => {}
                        },
                        _ => {}
                    },
                    Ok(Message::Close(_)) | Err(_) => {
                        session_alive.store(false, Ordering::Relaxed);
                        break;
                    }
                    _ => {}
                }
            }
            session_alive.store(false, Ordering::Relaxed);
        });
    }

    let mut window: Option<Window> = None;
    let mut buffer: Vec<u32> = Vec::new();
    let mut current_size = (0usize, 0usize);
    let mut frames_shown = 0u64;
    let mut last_report = std::time::Instant::now();

    let mut prev_mouse_pos: Option<(f32, f32)> = None;
    let mut prev_buttons = [false, false, false];
    let mut prev_keys: HashSet<Key> = HashSet::new();

    while session_alive.load(Ordering::Relaxed) {
        while let Ok(command) = commands_rx.try_recv() {
            match command {
                ConsoleCommand::Restart => {
                    println!("mandando pedido de reinicio remoto...");
                    let msg = netproto::encode_control(ControlEvent::RestartRequest { delay_secs: 5 });
                    let _ = out_tx.send(Message::Binary(msg));
                }
                ConsoleCommand::SendFile(path) => {
                    send_file(&out_tx, &path).await;
                }
            }
        }

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

        if let Some(win) = &window {
            let (win_w, win_h) = win.get_size();
            if current_size.0 > 0 && current_size.1 > 0 && win_w > 0 && win_h > 0 {
                if let Some((mx, my)) = win.get_mouse_pos(MouseMode::Clamp) {
                    let nx = mx / current_size.0 as f32;
                    let ny = my / current_size.1 as f32;
                    if prev_mouse_pos != Some((mx, my)) {
                        prev_mouse_pos = Some((mx, my));
                        let msg = netproto::encode_input(InputEvent::MouseMove { x: nx, y: ny });
                        let _ = out_tx.send(Message::Binary(msg));
                    }
                }

                let buttons = [
                    win.get_mouse_down(MinifbMouseButton::Left),
                    win.get_mouse_down(MinifbMouseButton::Right),
                    win.get_mouse_down(MinifbMouseButton::Middle),
                ];
                for i in 0..3 {
                    if buttons[i] != prev_buttons[i] {
                        let msg = netproto::encode_input(InputEvent::MouseButton {
                            button: i as u8,
                            pressed: buttons[i],
                        });
                        let _ = out_tx.send(Message::Binary(msg));
                    }
                }
                prev_buttons = buttons;
            }

            let current_keys: HashSet<Key> = win.get_keys().into_iter().collect();
            for key in current_keys.difference(&prev_keys) {
                if let Some(vk) = key_to_vk(*key) {
                    let msg = netproto::encode_input(InputEvent::Key { vk, pressed: true });
                    let _ = out_tx.send(Message::Binary(msg));
                }
            }
            for key in prev_keys.difference(&current_keys) {
                if let Some(vk) = key_to_vk(*key) {
                    let msg = netproto::encode_input(InputEvent::Key { vk, pressed: false });
                    let _ = out_tx.send(Message::Binary(msg));
                }
            }
            prev_keys = current_keys;
        }

        if last_report.elapsed().as_secs_f64() >= 1.0 {
            if frames_shown > 0 {
                println!("{} fps recibidos", frames_shown);
            }
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

#[cfg(windows)]
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    use tokio::io::{AsyncBufReadExt, BufReader};

    let signaling_url =
        std::env::var("SIGNALING_URL").unwrap_or_else(|_| "ws://127.0.0.1:8080".to_string());
    let code = std::env::var("AGENT_CODE").unwrap_or_else(|_| "123456".to_string());

    let (commands_tx, mut commands_rx) = tokio::sync::mpsc::unbounded_channel::<ConsoleCommand>();

    // Consola: comandos disponibles en cualquier momento:
    //   restart          -> reinicia la PC remota
    //   send <ruta>      -> manda un archivo a la PC remota
    tokio::spawn(async move {
        println!("comandos: 'restart' (reiniciar la PC remota), 'send <ruta>' (mandar un archivo)");
        let mut lines = BufReader::new(tokio::io::stdin()).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let line = line.trim();
            if line.eq_ignore_ascii_case("restart") {
                let _ = commands_tx.send(ConsoleCommand::Restart);
            } else if let Some(path) = line.strip_prefix("send ") {
                let _ = commands_tx.send(ConsoleCommand::SendFile(path.to_string()));
            } else if !line.is_empty() {
                println!("comando no reconocido: '{line}'");
            }
        }
    });

    loop {
        println!("conectando a {signaling_url}...");
        match run_session(&signaling_url, &code, &mut commands_rx).await {
            Ok(()) => println!("sesion terminada."),
            Err(e) => eprintln!("error de conexion: {e:#}"),
        }
        println!("reintentando en 3s...");
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    }
}

#[cfg(not(windows))]
fn main() {
    eprintln!("este ejemplo solo corre en Windows (necesita minifb + captura remota real)");
}
