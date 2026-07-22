//! Ejemplo standalone: prueba el encoder/decoder JPEG sobre un frame
//! sintetico (un degrade de colores), sin depender de la captura real
//! ni de Windows. Corre en cualquier plataforma.
//!
//! Correr con:
//!   cargo run --example test_encode --release -p core_engine

use core_engine::capture::Frame;
use core_engine::encode::{VideoDecoder, VideoEncoder};

fn synthetic_frame(width: u32, height: u32) -> Frame {
    let mut data = vec![0u8; (width * height * 4) as usize];
    for y in 0..height {
        for x in 0..width {
            let i = ((y * width + x) * 4) as usize;
            data[i] = (x % 256) as u8; // B
            data[i + 1] = (y % 256) as u8; // G
            data[i + 2] = 128; // R
            data[i + 3] = 255; // A
        }
    }
    Frame { width, height, data }
}

fn main() -> anyhow::Result<()> {
    let frame = synthetic_frame(1920, 1080);
    let raw_size = frame.data.len();

    for quality in [30u8, 60, 85] {
        let encoder = VideoEncoder::new(quality);
        let compressed = encoder.encode(&frame)?;
        let decoded = VideoDecoder::decode(&compressed)?;

        let ratio = raw_size as f64 / compressed.len() as f64;
        println!(
            "calidad {:3} -> {:>8} bytes (raw: {:>9} bytes, compresion: {:.1}x) | decodificado: {}x{}",
            quality,
            compressed.len(),
            raw_size,
            ratio,
            decoded.width,
            decoded.height
        );
    }

    Ok(())
}
