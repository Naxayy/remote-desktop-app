//! Ejemplo standalone: abre una ventana y muestra el escritorio
//! capturado en vivo. No usa red todavia - es solo para validar que
//! la captura DXGI funciona antes de meterla en el agent real.
//!
//! Correr con:
//!   cargo run --example live_view --release

#[cfg(windows)]
fn main() -> anyhow::Result<()> {
    use core_engine::capture::ScreenCapturer;
    use minifb::{Window, WindowOptions};

    let mut capturer = ScreenCapturer::new()?;

    // Creamos la ventana recien despues del primer frame para saber
    // las dimensiones reales del monitor capturado.
    let first = capturer.next_frame()?;
    let (width, height) = (first.width as usize, first.height as usize);

    let mut window = Window::new(
        "Remote Desktop App - live capture (local test)",
        width,
        height,
        WindowOptions::default(),
    )?;
    window.set_target_fps(30);

    let mut buffer: Vec<u32> = vec![0; width * height];

    while window.is_open() {
        let frame = capturer.next_frame()?;

        // BGRA8 (4 bytes por pixel) -> u32 0x00RRGGBB que espera minifb.
        for (i, px) in frame.data.chunks_exact(4).enumerate() {
            let (b, g, r) = (px[0] as u32, px[1] as u32, px[2] as u32);
            buffer[i] = (r << 16) | (g << 8) | b;
        }

        window.update_with_buffer(&buffer, width, height)?;
    }

    Ok(())
}

#[cfg(not(windows))]
fn main() {
    eprintln!("este ejemplo solo corre en Windows (usa la captura via DXGI)");
}
