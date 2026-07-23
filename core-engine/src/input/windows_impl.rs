//! Implementacion real de InputInjector para Windows via SendInput.
//!
//! Nota: igual que con capture, este archivo se escribio en un entorno
//! Linux sin poder compilarlo. Es esperable que la primera compilacion
//! en tu PC tire algun ajuste de nombres/firmas - mandame el error y lo
//! resolvemos como hicimos con la captura.

use super::MouseButton;
use anyhow::Result;
use windows::Win32::Foundation::POINT;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYEVENTF_KEYUP,
    KEYEVENTF_UNICODE, MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP,
    MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN,
    MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_WHEEL, MOUSEINPUT, VIRTUAL_KEY,
};
use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSCREEN, SM_CYSCREEN};

pub struct InputInjector {
    screen_width: i32,
    screen_height: i32,
}

impl InputInjector {
    pub fn new() -> Self {
        // Por ahora asumimos monitor primario, igual que la captura
        // (indice 0). Cuando soportemos multi-monitor hay que usar
        // SM_CXVIRTUALSCREEN / SM_CYVIRTUALSCREEN + el offset del
        // monitor correspondiente.
        let screen_width = unsafe { GetSystemMetrics(SM_CXSCREEN) };
        let screen_height = unsafe { GetSystemMetrics(SM_CYSCREEN) };
        Self {
            screen_width,
            screen_height,
        }
    }

    /// Mueve el mouse a una posicion absoluta en pixeles de pantalla
    /// (0,0 = esquina superior izquierda).
    pub fn move_mouse_absolute(&self, x: i32, y: i32) -> Result<()> {
        // SendInput con MOUSEEVENTF_ABSOLUTE espera coordenadas
        // normalizadas en el rango 0..65535, no pixeles crudos.
        let norm_x = (x as f64 / self.screen_width as f64 * 65535.0) as i32;
        let norm_y = (y as f64 / self.screen_height as f64 * 65535.0) as i32;

        let input = mouse_input(norm_x, norm_y, MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE, 0);
        send(&[input])
    }

    /// Mueve el mouse usando coordenadas normalizadas 0.0-1.0 (0,0 =
    /// esquina superior izquierda, 1,1 = esquina inferior derecha).
    /// Pensado para el controller, que no conoce la resolucion real
    /// de la pantalla remota - solo sabe donde esta el mouse dentro
    /// de la ventana que esta mostrando el video.
    pub fn move_mouse_normalized(&self, nx: f64, ny: f64) -> Result<()> {
        let x = (nx.clamp(0.0, 1.0) * self.screen_width as f64) as i32;
        let y = (ny.clamp(0.0, 1.0) * self.screen_height as f64) as i32;
        self.move_mouse_absolute(x, y)
    }

    pub fn mouse_button(&self, button: MouseButton, pressed: bool) -> Result<()> {
        let flag = match (button, pressed) {
            (MouseButton::Left, true) => MOUSEEVENTF_LEFTDOWN,
            (MouseButton::Left, false) => MOUSEEVENTF_LEFTUP,
            (MouseButton::Right, true) => MOUSEEVENTF_RIGHTDOWN,
            (MouseButton::Right, false) => MOUSEEVENTF_RIGHTUP,
            (MouseButton::Middle, true) => MOUSEEVENTF_MIDDLEDOWN,
            (MouseButton::Middle, false) => MOUSEEVENTF_MIDDLEUP,
        };
        let input = mouse_input(0, 0, flag, 0);
        send(&[input])
    }

    /// delta positivo = scroll hacia arriba, negativo = hacia abajo.
    /// 120 es "un click de rueda" segun la convencion de Windows.
    pub fn mouse_wheel(&self, delta: i32) -> Result<()> {
        let input = mouse_input(0, 0, MOUSEEVENTF_WHEEL, delta);
        send(&[input])
    }

    /// Presiona/suelta una tecla identificada por su Virtual-Key Code
    /// de Windows (ej: 0x41 = 'A', 0x0D = Enter). Util para teclas de
    /// control (flechas, modificadores, funcion, etc).
    pub fn key(&self, vk_code: u16, pressed: bool) -> Result<()> {
        let mut flags = Default::default();
        if !pressed {
            flags = KEYEVENTF_KEYUP;
        }
        let input = INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: VIRTUAL_KEY(vk_code),
                    wScan: 0,
                    dwFlags: flags,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        };
        send(&[input])
    }

    /// Escribe un caracter unicode directamente (sirve para texto en
    /// cualquier idioma/layout, sin depender del Virtual-Key Code).
    pub fn type_char(&self, ch: char) -> Result<()> {
        let mut buf = [0u16; 2];
        let units = ch.encode_utf16(&mut buf);
        for &unit in units.iter() {
            let down = unicode_key_input(unit, false);
            let up = unicode_key_input(unit, true);
            send(&[down, up])?;
        }
        Ok(())
    }
}

fn mouse_input(dx: i32, dy: i32, flags: windows::Win32::UI::Input::KeyboardAndMouse::MOUSE_EVENT_FLAGS, mouse_data: i32) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx,
                dy,
                mouseData: mouse_data as u32,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn unicode_key_input(utf16_unit: u16, key_up: bool) -> INPUT {
    let mut flags = KEYEVENTF_UNICODE;
    if key_up {
        flags |= KEYEVENTF_KEYUP;
    }
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(0),
                wScan: utf16_unit,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn send(inputs: &[INPUT]) -> Result<()> {
    let sent = unsafe { SendInput(inputs, std::mem::size_of::<INPUT>() as i32) };
    if sent as usize != inputs.len() {
        anyhow::bail!("SendInput solo proceso {sent} de {} eventos", inputs.len());
    }
    Ok(())
}

// nota: la posicion actual del mouse (para saber de donde partir en
// movimientos relativos) se puede leer con GetCursorPos si hace falta
// mas adelante:
#[allow(dead_code)]
fn current_cursor_pos() -> Result<(i32, i32)> {
    let mut point = POINT::default();
    unsafe { windows::Win32::UI::WindowsAndMessaging::GetCursorPos(&mut point)? };
    Ok((point.x, point.y))
}
