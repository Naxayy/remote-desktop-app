//! Captura de pantalla via DXGI Desktop Duplication API (Windows).
//!
//! Como funciona a alto nivel:
//! 1. Creamos un dispositivo Direct3D11 sobre el adaptador de video.
//! 2. Tomamos el output (monitor) y pedimos su interfaz de "duplication",
//!    que es la API que usa Windows internamente para grabacion de
//!    pantalla / remote desktop (la misma que usan OBS, TeamViewer, etc).
//! 3. En loop, le pedimos el proximo frame (`AcquireNextFrame`): Windows
//!    nos entrega una textura de GPU con SOLO los pixeles que cambiaron
//!    desde el frame anterior (mas eficiente que capturar todo siempre).
//! 4. Copiamos esa textura a un buffer que la CPU pueda leer, y la
//!    devolvemos como bytes BGRA crudos, listos para mostrarse o
//!    pasarse al encoder de video.

#[cfg(windows)]
mod windows_impl;

#[cfg(windows)]
pub use windows_impl::ScreenCapturer;

#[cfg(not(windows))]
pub struct ScreenCapturer;

#[cfg(not(windows))]
impl ScreenCapturer {
    pub fn new() -> anyhow::Result<Self> {
        anyhow::bail!("la captura de pantalla solo esta implementada para Windows por ahora")
    }

    pub fn next_frame(&mut self) -> anyhow::Result<Frame> {
        anyhow::bail!("la captura de pantalla solo esta implementada para Windows por ahora")
    }
}

/// Un frame capturado: pixeles crudos en formato BGRA8 + dimensiones.
pub struct Frame {
    pub width: u32,
    pub height: u32,
    /// 4 bytes por pixel: B, G, R, A (en ese orden, es el formato nativo de DXGI).
    pub data: Vec<u8>,
}
