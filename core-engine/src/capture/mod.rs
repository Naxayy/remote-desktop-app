//! Captura de pantalla.
//!
//! Proximo paso: implementar el backend de Windows usando DXGI
//! (Desktop Duplication API) para capturar frames con la menor
//! latencia posible, con fallback a GDI si DXGI no esta disponible.

pub struct ScreenCapturer;

impl ScreenCapturer {
    pub fn new() -> Self {
        todo!("implementar captura via DXGI (Windows)")
    }
}
