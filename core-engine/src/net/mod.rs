//! Transporte P2P (UDP) para eventos de input, con NAT traversal via
//! STUN + hole punching. Es una OPTIMIZACION: si no se puede
//! establecer el path directo (NAT simetrico, firewall restrictivo,
//! etc), el llamador simplemente sigue usando el relay del
//! signaling-server, que siempre funciona.
//!
//! Por ahora solo los eventos de INPUT (mouse/teclado, chiquitos y
//! sensibles a latencia) se mandan por este path cuando esta
//! disponible. Video y archivos siguen yendo por el relay - mandar
//! JPEGs grandes por UDP requeriria trocearlos con reensamblado y
//! manejo de perdida de paquetes, que queda como mejora futura.

use anyhow::{Context, Result};
use std::net::SocketAddr;
use std::time::Duration;
use tokio::net::UdpSocket;
use tokio::time::timeout;

/// Byte "magico" que identifica un datagrama de hole-punching (no es
/// parte del netproto normal - viaja directo por UDP, nunca por el
/// relay del signaling-server).
pub const P2P_PING_MARKER: u8 = 0xFE;

/// Crea el socket UDP local que vamos a usar tanto para STUN como
/// para el trafico P2P una vez establecido.
pub async fn bind_local_socket() -> Result<UdpSocket> {
    UdpSocket::bind("0.0.0.0:0")
        .await
        .context("no se pudo bindear el socket UDP local")
}

/// Le pregunta a un servidor STUN publico cual es nuestra IP:puerto
/// vista desde afuera (candidate "server-reflexive"). Implementacion
/// minima del protocolo STUN (RFC 5389) - solo el Binding Request/
/// Response, sin autenticacion ni atributos extra.
pub async fn stun_discover(socket: &UdpSocket, stun_server: &str) -> Result<SocketAddr> {
    const MAGIC_COOKIE: u32 = 0x2112_A442;

    let mut txn_id = [0u8; 12];
    fill_random(&mut txn_id);

    let mut request = Vec::with_capacity(20);
    request.extend_from_slice(&0x0001u16.to_be_bytes()); // Binding Request
    request.extend_from_slice(&0u16.to_be_bytes()); // longitud: sin atributos
    request.extend_from_slice(&MAGIC_COOKIE.to_be_bytes());
    request.extend_from_slice(&txn_id);

    socket
        .send_to(&request, stun_server)
        .await
        .context("no se pudo mandar el STUN Binding Request")?;

    let mut buf = [0u8; 512];
    let (len, _) = timeout(Duration::from_secs(3), socket.recv_from(&mut buf))
        .await
        .context("timeout esperando respuesta STUN")?
        .context("error recibiendo respuesta STUN")?;

    parse_stun_response(&buf[..len], &txn_id)
}

fn fill_random(buf: &mut [u8]) {
    // No hace falta que sea criptograficamente aleatorio - el
    // transaction ID de STUN solo necesita ser unico por request para
    // matchear la respuesta.
    use std::time::{SystemTime, UNIX_EPOCH};
    let seed = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    for (i, b) in buf.iter_mut().enumerate() {
        *b = ((seed >> (i % 16)) ^ (seed.wrapping_mul(2654435761 + i as u128))) as u8;
    }
}

/// Parsea una respuesta STUN y extrae la direccion publica del
/// atributo XOR-MAPPED-ADDRESS (o MAPPED-ADDRESS como fallback, que
/// usan algunos servidores viejos). Separado en su propia funcion
/// pura (sin red) para poder testearlo con bytes sinteticos.
fn parse_stun_response(data: &[u8], expected_txn: &[u8; 12]) -> Result<SocketAddr> {
    if data.len() < 20 {
        anyhow::bail!("respuesta STUN demasiado corta");
    }
    let msg_type = u16::from_be_bytes([data[0], data[1]]);
    if msg_type != 0x0101 {
        anyhow::bail!("respuesta STUN no es un Binding Success Response (type={msg_type:#06x})");
    }
    if &data[8..20] != expected_txn {
        anyhow::bail!("transaction ID de la respuesta STUN no coincide");
    }

    const MAGIC_COOKIE_BYTES: [u8; 4] = 0x2112_A442u32.to_be_bytes();

    let mut offset = 20;
    while offset + 4 <= data.len() {
        let attr_type = u16::from_be_bytes([data[offset], data[offset + 1]]);
        let attr_len = u16::from_be_bytes([data[offset + 2], data[offset + 3]]) as usize;
        let value_start = offset + 4;
        let value_end = value_start + attr_len;
        if value_end > data.len() {
            break;
        }
        let value = &data[value_start..value_end];

        // XOR-MAPPED-ADDRESS (0x0020 estandar; 0x8020 es una variante
        // que usan algunos servidores STUN viejos/vendor-specific).
        if (attr_type == 0x0020 || attr_type == 0x8020) && value.len() >= 8 && value[1] == 0x01 {
            let port = u16::from_be_bytes([value[2], value[3]])
                ^ u16::from_be_bytes([MAGIC_COOKIE_BYTES[0], MAGIC_COOKIE_BYTES[1]]);
            let ip = [
                value[4] ^ MAGIC_COOKIE_BYTES[0],
                value[5] ^ MAGIC_COOKIE_BYTES[1],
                value[6] ^ MAGIC_COOKIE_BYTES[2],
                value[7] ^ MAGIC_COOKIE_BYTES[3],
            ];
            return Ok(SocketAddr::from((ip, port)));
        }

        // MAPPED-ADDRESS (variante vieja, sin XOR).
        if attr_type == 0x0001 && value.len() >= 8 && value[1] == 0x01 {
            let port = u16::from_be_bytes([value[2], value[3]]);
            let ip = [value[4], value[5], value[6], value[7]];
            return Ok(SocketAddr::from((ip, port)));
        }

        // Los atributos STUN van alineados a multiplos de 4 bytes.
        offset = value_end + ((4 - (attr_len % 4)) % 4);
    }

    anyhow::bail!("no se encontro XOR-MAPPED-ADDRESS ni MAPPED-ADDRESS en la respuesta STUN")
}

/// Intenta abrir un path P2P directo hacia `peer_addr` mandando pings
/// repetidos y escuchando una respuesta. Da por exitoso el hole
/// punch apenas llega CUALQUIER datagrama de esa direccion (no hace
/// falta que sea el ping mismo - alcanza con que el paquete haya
/// logrado atravesar el NAT). Si no lo logra en `timeout_secs`,
/// devuelve None y el llamador debe seguir usando el relay.
pub async fn punch_hole(
    socket: &UdpSocket,
    peer_addr: SocketAddr,
    timeout_secs: u64,
) -> Option<SocketAddr> {
    let ping = [P2P_PING_MARKER];
    let deadline = tokio::time::Instant::now() + Duration::from_secs(timeout_secs);
    let mut buf = [0u8; 1500];

    while tokio::time::Instant::now() < deadline {
        let _ = socket.send_to(&ping, peer_addr).await;

        match timeout(Duration::from_millis(300), socket.recv_from(&mut buf)).await {
            Ok(Ok((_, from))) if from == peer_addr => return Some(from),
            _ => continue,
        }
    }
    None
}

pub struct Connection;

#[cfg(test)]
mod tests {
    use super::*;

    /// Arma un Binding Success Response sintetico (como lo mandaria un
    /// servidor STUN real) con un XOR-MAPPED-ADDRESS conocido, y
    /// confirma que lo parseamos bien - sin tocar la red.
    #[test]
    fn parse_stun_response_xor_mapped_address() {
        let txn_id = [1u8; 12];
        let magic: [u8; 4] = 0x2112_A442u32.to_be_bytes();

        // IP publica de ejemplo: 203.0.113.5, puerto 54321.
        let real_ip = [203, 0, 113, 5];
        let real_port: u16 = 54321;

        let xor_port = real_port ^ u16::from_be_bytes([magic[0], magic[1]]);
        let xor_ip = [
            real_ip[0] ^ magic[0],
            real_ip[1] ^ magic[1],
            real_ip[2] ^ magic[2],
            real_ip[3] ^ magic[3],
        ];

        let mut attr_value = Vec::new();
        attr_value.push(0x00); // reserved
        attr_value.push(0x01); // family: IPv4
        attr_value.extend_from_slice(&xor_port.to_be_bytes());
        attr_value.extend_from_slice(&xor_ip);

        let mut response = Vec::new();
        response.extend_from_slice(&0x0101u16.to_be_bytes()); // Binding Success Response
        response.extend_from_slice(&((attr_value.len() + 4) as u16).to_be_bytes());
        response.extend_from_slice(&magic);
        response.extend_from_slice(&txn_id);
        response.extend_from_slice(&0x0020u16.to_be_bytes()); // XOR-MAPPED-ADDRESS
        response.extend_from_slice(&(attr_value.len() as u16).to_be_bytes());
        response.extend_from_slice(&attr_value);

        let addr = parse_stun_response(&response, &txn_id).unwrap();
        assert_eq!(addr, SocketAddr::from((real_ip, real_port)));
    }

    #[test]
    fn parse_stun_response_rejects_wrong_transaction_id() {
        let response = vec![0x01, 0x01, 0, 0, 0x21, 0x12, 0xA4, 0x42].into_iter().chain([0u8; 12]).collect::<Vec<u8>>();
        let wrong_txn = [9u8; 12];
        assert!(parse_stun_response(&response, &wrong_txn).is_err());
    }

    #[test]
    fn parse_stun_response_rejects_short_buffer() {
        let short = vec![0u8; 5];
        assert!(parse_stun_response(&short, &[0u8; 12]).is_err());
    }
}
