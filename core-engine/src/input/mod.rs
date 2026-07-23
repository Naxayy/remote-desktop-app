//! Inyeccion de input remoto (mouse/teclado) via la API SendInput de
//! Windows - la misma que usa cualquier herramienta de automatizacion
//! o control remoto legitimo. Requiere permisos de administrador para
//! poder inyectar input en ventanas que corren con privilegios
//! elevados (por eso el agent debe correr como servicio con
//! privilegios de sistema).

#[cfg(windows)]
mod windows_impl;

#[cfg(windows)]
pub use windows_impl::InputInjector;

#[cfg(not(windows))]
pub struct InputInjector;

#[cfg(not(windows))]
impl InputInjector {
    pub fn new() -> Self {
        Self
    }

    pub fn move_mouse_normalized(&self, _nx: f64, _ny: f64) -> anyhow::Result<()> {
        anyhow::bail!("input remoto solo esta implementado para Windows por ahora")
    }

    pub fn mouse_button(&self, _button: MouseButton, _pressed: bool) -> anyhow::Result<()> {
        anyhow::bail!("input remoto solo esta implementado para Windows por ahora")
    }

    pub fn mouse_wheel(&self, _delta: i32) -> anyhow::Result<()> {
        anyhow::bail!("input remoto solo esta implementado para Windows por ahora")
    }

    pub fn key(&self, _vk_code: u16, _pressed: bool) -> anyhow::Result<()> {
        anyhow::bail!("input remoto solo esta implementado para Windows por ahora")
    }
}

/// Boton de mouse para eventos de click.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
}
