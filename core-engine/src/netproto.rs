//! Codificacion binaria compacta para lo que viaja por la red una vez
//! que agent y controller estan emparejados: frames de video y
//! eventos de input. Reemplaza el JSON+base64 que usabamos al
//! principio (mas facil de debuggear, pero ~33% mas pesado solo por
//! el base64, mas el overhead de parsear JSON en cada frame).
//!
//! Formato: el primer byte indica el tipo de mensaje ("kind").
//! Los mensajes de control (registro, emparejamiento, errores) siguen
//! viajando como texto JSON via el signaling-server - son poco
//! frecuentes, la legibilidad ahi vale mas que el ahorro de bytes.

pub const KIND_FRAME: u8 = 1;
pub const KIND_INPUT: u8 = 2;
pub const KIND_CONTROL: u8 = 3;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum InputEvent {
    /// Coordenadas normalizadas 0.0-1.0 (0,0 = esquina superior
    /// izquierda), para no depender de la resolucion real de la
    /// pantalla remota.
    MouseMove { x: f32, y: f32 },
    /// button: 0 = izquierdo, 1 = derecho, 2 = medio.
    MouseButton { button: u8, pressed: bool },
    MouseWheel { delta: i32 },
    /// vk = Virtual-Key Code de Windows.
    Key { vk: u16, pressed: bool },
}

/// Envuelve un JPEG ya comprimido en el formato de red. No hace falta
/// mandar ancho/alto por separado - ya vienen en la cabecera del JPEG.
pub fn encode_frame(jpeg: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(1 + jpeg.len());
    out.push(KIND_FRAME);
    out.extend_from_slice(jpeg);
    out
}

/// Si `data` es un mensaje de frame, devuelve los bytes JPEG (sin la
/// cabecera de 1 byte). None si es de otro tipo.
pub fn decode_frame(data: &[u8]) -> Option<&[u8]> {
    if data.first() == Some(&KIND_FRAME) {
        Some(&data[1..])
    } else {
        None
    }
}

pub fn encode_input(event: InputEvent) -> Vec<u8> {
    let mut out = Vec::with_capacity(10);
    out.push(KIND_INPUT);
    match event {
        InputEvent::MouseMove { x, y } => {
            out.push(1);
            out.extend_from_slice(&x.to_le_bytes());
            out.extend_from_slice(&y.to_le_bytes());
        }
        InputEvent::MouseButton { button, pressed } => {
            out.push(2);
            out.push(button);
            out.push(pressed as u8);
        }
        InputEvent::MouseWheel { delta } => {
            out.push(3);
            out.extend_from_slice(&delta.to_le_bytes());
        }
        InputEvent::Key { vk, pressed } => {
            out.push(4);
            out.extend_from_slice(&vk.to_le_bytes());
            out.push(pressed as u8);
        }
    }
    out
}

/// Si `data` es un mensaje de input valido, lo decodifica. None si es
/// de otro tipo o esta corrupto/incompleto.
pub fn decode_input(data: &[u8]) -> Option<InputEvent> {
    if data.first() != Some(&KIND_INPUT) {
        return None;
    }
    let body = data.get(1..)?;
    match *body.first()? {
        1 => Some(InputEvent::MouseMove {
            x: f32::from_le_bytes(body.get(1..5)?.try_into().ok()?),
            y: f32::from_le_bytes(body.get(5..9)?.try_into().ok()?),
        }),
        2 => Some(InputEvent::MouseButton {
            button: *body.get(1)?,
            pressed: *body.get(2)? != 0,
        }),
        3 => Some(InputEvent::MouseWheel {
            delta: i32::from_le_bytes(body.get(1..5)?.try_into().ok()?),
        }),
        4 => Some(InputEvent::Key {
            vk: u16::from_le_bytes(body.get(1..3)?.try_into().ok()?),
            pressed: *body.get(3)? != 0,
        }),
        _ => None,
    }
}

/// Comandos de control: cosas puntuales que no son ni video ni input
/// continuo, como pedir un reinicio remoto.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ControlEvent {
    /// El controller le pide al agent que reinicie la PC remota,
    /// dandole `delay_secs` segundos de margen antes de que pase
    /// (para que el agent pueda avisar/cerrar cosas prolijamente).
    RestartRequest { delay_secs: u8 },
    /// El agent confirma que recibio el pedido y va a reiniciar.
    RestartAck,
}

pub fn encode_control(event: ControlEvent) -> Vec<u8> {
    let mut out = Vec::with_capacity(3);
    out.push(KIND_CONTROL);
    match event {
        ControlEvent::RestartRequest { delay_secs } => {
            out.push(1);
            out.push(delay_secs);
        }
        ControlEvent::RestartAck => {
            out.push(2);
        }
    }
    out
}

pub fn decode_control(data: &[u8]) -> Option<ControlEvent> {
    if data.first() != Some(&KIND_CONTROL) {
        return None;
    }
    let body = data.get(1..)?;
    match *body.first()? {
        1 => Some(ControlEvent::RestartRequest {
            delay_secs: *body.get(1)?,
        }),
        2 => Some(ControlEvent::RestartAck),
        _ => None,
    }
}

/// El "kind" (primer byte) de un mensaje binario, util para decidir
/// a que decoder mandarlo sin tener que probar los tres.
pub fn peek_kind(data: &[u8]) -> Option<u8> {
    data.first().copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_roundtrip() {
        let jpeg = vec![0xFF, 0xD8, 0xAB, 0xCD, 0xFF, 0xD9];
        let encoded = encode_frame(&jpeg);
        assert_eq!(decode_frame(&encoded), Some(jpeg.as_slice()));
    }

    #[test]
    fn input_roundtrip_all_variants() {
        let events = [
            InputEvent::MouseMove { x: 0.5, y: 0.25 },
            InputEvent::MouseButton { button: 1, pressed: true },
            InputEvent::MouseWheel { delta: -120 },
            InputEvent::Key { vk: 65, pressed: false },
        ];
        for event in events {
            let encoded = encode_input(event);
            assert_eq!(decode_input(&encoded), Some(event));
        }
    }

    #[test]
    fn decode_rejects_wrong_kind() {
        let frame_bytes = encode_frame(&[1, 2, 3]);
        assert_eq!(decode_input(&frame_bytes), None);
    }

    #[test]
    fn control_roundtrip() {
        let events = [
            ControlEvent::RestartRequest { delay_secs: 10 },
            ControlEvent::RestartAck,
        ];
        for event in events {
            let encoded = encode_control(event);
            assert_eq!(decode_control(&encoded), Some(event));
        }
    }
}
