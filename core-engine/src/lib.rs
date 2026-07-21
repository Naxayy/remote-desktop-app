//! core_engine: motor central del sistema de escritorio remoto.
//!
//! Modulos:
//! - `capture`: captura de pantalla (DXGI en Windows).
//! - `encode`:  codificacion/decodificacion de video para el streaming.
//! - `input`:   inyeccion de eventos de mouse/teclado en la maquina remota.
//! - `net`:     transporte de red (P2P directo + fallback a relay).
//! - `crypto`:  cifrado end-to-end de la sesion.
//!
//! Cada modulo se implementa de forma incremental. Por ahora son stubs.

pub mod capture;
pub mod encode;
pub mod input;
pub mod net;
pub mod crypto;
