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
}

/// Boton de mouse para eventos de click.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
}
