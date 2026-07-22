//! Compresion de video para el streaming.
//!
//! Primera version: compresion JPEG por frame (estilo "Motion JPEG").
//! Es mas simple que un codec de video real (H264/VP9) porque cada
//! frame se comprime de forma independiente, sin prediccion entre
//! frames - a cambio de menos eficiencia de compresion. Sirve como
//! base funcional; el proximo paso natural es sumar deteccion de
//! regiones que cambiaron (dirty rects) para no re-codificar pantalla
//! completa en cada frame, y mas adelante migrar a H264 con
//! prediccion temporal si la calidad/ancho de banda lo requiere.

use crate::capture::Frame;
use anyhow::{Context, Result};
use jpeg_encoder::{ColorType as JpegColorType, Encoder as JpegEncoder};

pub struct VideoEncoder {
    /// Calidad JPEG, 1 (peor/mas chico) a 100 (mejor/mas pesado).
    quality: u8,
}

impl VideoEncoder {
    pub fn new(quality: u8) -> Self {
        Self {
            quality: quality.clamp(1, 100),
        }
    }

    /// Comprime un frame BGRA8 crudo a JPEG. El canal alpha se descarta
    /// (JPEG no lo soporta - no lo necesitamos para escritorio remoto).
    pub fn encode(&self, frame: &Frame) -> Result<Vec<u8>> {
        let mut rgb = Vec::with_capacity((frame.width * frame.height * 3) as usize);
        for px in frame.data.chunks_exact(4) {
            rgb.push(px[2]); // R
            rgb.push(px[1]); // G
            rgb.push(px[0]); // B
        }

        let mut out = Vec::new();
        let encoder = JpegEncoder::new(&mut out, self.quality);
        encoder
            .encode(&rgb, frame.width as u16, frame.height as u16, JpegColorType::Rgb)
            .context("fallo al codificar el frame a JPEG")?;
        Ok(out)
    }
}

pub struct VideoDecoder;

impl VideoDecoder {
    /// Decodifica bytes JPEG de vuelta a un Frame BGRA8. Sirve para
    /// probar el pipeline completo (encode -> decode) antes de meter
    /// la red real, y es lo que va a usar el controller-ui para
    /// mostrar los frames que le llegan del agent.
    pub fn decode(bytes: &[u8]) -> Result<Frame> {
        let img = image::load_from_memory_with_format(bytes, image::ImageFormat::Jpeg)
            .context("fallo al decodificar JPEG")?
            .to_rgb8();
        let (width, height) = img.dimensions();

        let mut bgra = Vec::with_capacity((width * height * 4) as usize);
        for px in img.pixels() {
            bgra.push(px[2]); // B
            bgra.push(px[1]); // G
            bgra.push(px[0]); // R
            bgra.push(255); // A
        }

        Ok(Frame {
            width,
            height,
            data: bgra,
        })
    }
}
