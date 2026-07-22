//! Ejemplo standalone: captura + comprime (JPEG) + descomprime + muestra
//! en una ventana, todo en un mismo loop local. Todavia sin red - la
//! idea es medir cuanto pesaria cada frame si lo tuvieramos que mandar,
//! y confirmar que decode->buffer para la ventana funciona bien antes
//! de meter la capa de red real.
//!
//! Correr con:
//!   cargo run --example live_view_compressed --release -p core_engine

#[cfg(windows)]
fn main() -> anyhow::Result<()> {
    use core_engine::capture::ScreenCapturer;
    use core_engine::encode::{VideoDecoder, VideoEncoder};
    use minifb::{Window, WindowOptions};
    use std::time::Instant;

    // Calidad 50 como punto de partida razonable entre nitidez y peso.
    // Se puede ajustar y volver a correr para comparar.
    let quality = 50u8;
    let encoder = VideoEncoder::new(quality);

    let mut capturer = ScreenCapturer::new()?;
    let first = capturer.next_frame()?;
    let (width, height) = (first.width as usize, first.height as usize);

    let mut window = Window::new(
        &format!("live_view_compressed (calidad {quality}) - local, sin red"),
        width,
        height,
        WindowOptions::default(),
    )?;
    window.set_target_fps(30);

    let mut buffer: Vec<u32> = vec![0; width * height];

    // Contadores para el resumen que se imprime cada 1s: fps/bandwidth
    // generales, mas el tiempo promedio de cada etapa del pipeline
    // (para saber exactamente donde se esta yendo el tiempo).
    let mut frames_in_window = 0u32;
    let mut bytes_in_window = 0u64;
    let mut capture_ms_total = 0f64;
    let mut encode_ms_total = 0f64;
    let mut decode_ms_total = 0f64;
    let mut display_ms_total = 0f64;
    let mut window_start = Instant::now();

    while window.is_open() {
        let t0 = Instant::now();
        let raw_frame = capturer.next_frame()?;
        let t1 = Instant::now();

        let compressed = encoder.encode(&raw_frame)?;
        let t2 = Instant::now();

        let decoded = VideoDecoder::decode(&compressed)?;
        let t3 = Instant::now();

        for (i, px) in decoded.data.chunks_exact(4).enumerate() {
            let (b, g, r) = (px[0] as u32, px[1] as u32, px[2] as u32);
            buffer[i] = (r << 16) | (g << 8) | b;
        }
        window.update_with_buffer(&buffer, width, height)?;
        let t4 = Instant::now();

        capture_ms_total += (t1 - t0).as_secs_f64() * 1000.0;
        encode_ms_total += (t2 - t1).as_secs_f64() * 1000.0;
        decode_ms_total += (t3 - t2).as_secs_f64() * 1000.0;
        display_ms_total += (t4 - t3).as_secs_f64() * 1000.0;

        frames_in_window += 1;
        bytes_in_window += compressed.len() as u64;

        if window_start.elapsed().as_secs_f64() >= 1.0 {
            let n = frames_in_window as f64;
            let fps = n / window_start.elapsed().as_secs_f64();
            let mbps = (bytes_in_window as f64 * 8.0 / 1_000_000.0) / window_start.elapsed().as_secs_f64();
            let avg_frame_kb = (bytes_in_window as f64 / n) / 1024.0;
            println!(
                "{:.1} fps | {:.1} KB/frame | {:.2} Mbps || ms promedio -> captura: {:.1} | encode: {:.1} | decode: {:.1} | display: {:.1}",
                fps,
                avg_frame_kb,
                mbps,
                capture_ms_total / n,
                encode_ms_total / n,
                decode_ms_total / n,
                display_ms_total / n
            );
            frames_in_window = 0;
            bytes_in_window = 0;
            capture_ms_total = 0.0;
            encode_ms_total = 0.0;
            decode_ms_total = 0.0;
            display_ms_total = 0.0;
            window_start = Instant::now();
        }
    }

    Ok(())
}

#[cfg(not(windows))]
fn main() {
    eprintln!("este ejemplo solo corre en Windows (usa la captura via DXGI)");
}
