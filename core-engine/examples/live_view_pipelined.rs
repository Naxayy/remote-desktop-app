//! Ejemplo standalone: como live_view_compressed, pero con captura+encode
//! corriendo en un hilo separado del decode+render. Esto simula lo que
//! va a pasar en produccion: el agent (captura+encode) y el controller
//! (decode+render) corren en maquinas distintas, en paralelo - el fps
//! final queda limitado por la etapa MAS LENTA, no por la suma de todas
//! como en el loop secuencial de live_view_compressed.
//!
//! Usamos un "ultimo frame disponible" compartido (Mutex) en vez de una
//! cola: si el consumidor (render) es mas lento que el productor
//! (encode), se descartan frames viejos y siempre se muestra el mas
//! reciente - es el comportamiento correcto para video en vivo (no
//! queremos ir "atrasando" el stream).
//!
//! Correr con:
//!   cargo run --example live_view_pipelined --release -p core_engine

#[cfg(windows)]
fn main() -> anyhow::Result<()> {
    use core_engine::capture::ScreenCapturer;
    use core_engine::encode::{VideoDecoder, VideoEncoder};
    use minifb::{Window, WindowOptions};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::Instant;

    let quality = 50u8;

    // Slot compartido: el productor pisa el contenido con cada frame
    // nuevo, el consumidor lee el ultimo disponible. frame_id sirve
    // para que el consumidor sepa si ya vio este frame o no.
    let latest: Arc<Mutex<Option<Vec<u8>>>> = Arc::new(Mutex::new(None));
    let frame_id = Arc::new(AtomicU64::new(0));

    // --- Hilo productor: captura + encode (simula el "agent") ---
    {
        let latest = Arc::clone(&latest);
        let frame_id = Arc::clone(&frame_id);
        thread::spawn(move || -> anyhow::Result<()> {
            let mut capturer = ScreenCapturer::new()?;
            let encoder = VideoEncoder::new(quality);

            let mut frames_in_window = 0u32;
            let mut window_start = Instant::now();

            loop {
                let raw = capturer.next_frame()?;
                let compressed = encoder.encode(&raw)?;

                *latest.lock().unwrap() = Some(compressed);
                frame_id.fetch_add(1, Ordering::Relaxed);

                frames_in_window += 1;
                if window_start.elapsed().as_secs_f64() >= 1.0 {
                    let fps = frames_in_window as f64 / window_start.elapsed().as_secs_f64();
                    println!("  [productor: captura+encode] {:.1} fps", fps);
                    frames_in_window = 0;
                    window_start = Instant::now();
                }
            }
        });
    }

    // --- Hilo/loop principal: decode + render (simula el "controller") ---
    // La ventana tiene que vivir en el hilo principal.
    let mut window = Window::new(
        &format!("live_view_pipelined (calidad {quality}) - 2 hilos, local sin red"),
        1280,
        720,
        WindowOptions::default(),
    )?;
    window.set_target_fps(60); // no limitamos artificialmente a 30, para ver el techo real

    let mut buffer: Vec<u32> = Vec::new();
    let mut last_seen_id = 0u64;
    let mut frames_in_window = 0u32;
    let mut window_start = Instant::now();
    let mut current_size = (0usize, 0usize);

    while window.is_open() {
        let current_id = frame_id.load(Ordering::Relaxed);
        if current_id == last_seen_id {
            // Todavia no hay frame nuevo, no tiene sentido decodificar
            // el mismo de vuelta.
            std::thread::sleep(std::time::Duration::from_millis(1));
            continue;
        }
        last_seen_id = current_id;

        let compressed = { latest.lock().unwrap().clone() };
        let Some(compressed) = compressed else { continue };

        let decoded = VideoDecoder::decode(&compressed)?;
        let (w, h) = (decoded.width as usize, decoded.height as usize);

        if current_size != (w, h) {
            current_size = (w, h);
            buffer = vec![0u32; w * h];
            window = Window::new(
                &format!("live_view_pipelined (calidad {quality}) - 2 hilos, local sin red"),
                w,
                h,
                WindowOptions::default(),
            )?;
            window.set_target_fps(60);
        }

        for (i, px) in decoded.data.chunks_exact(4).enumerate() {
            let (b, g, r) = (px[0] as u32, px[1] as u32, px[2] as u32);
            buffer[i] = (r << 16) | (g << 8) | b;
        }
        window.update_with_buffer(&buffer, w, h)?;

        frames_in_window += 1;
        if window_start.elapsed().as_secs_f64() >= 1.0 {
            let fps = frames_in_window as f64 / window_start.elapsed().as_secs_f64();
            println!("[consumidor: decode+render] {:.1} fps  <-- este es el fps real que verias", fps);
            frames_in_window = 0;
            window_start = Instant::now();
        }
    }

    Ok(())
}

#[cfg(not(windows))]
fn main() {
    eprintln!("este ejemplo solo corre en Windows (usa la captura via DXGI)");
}
