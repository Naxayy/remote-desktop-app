//! Protocolo de mensajes entre agent/controller y el signaling server.
//! Se serializa como JSON sobre WebSocket.

use serde::{Deserialize, Serialize};

/// Mensajes que un cliente (agent o controller) le manda al servidor.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    /// El agent se registra para poder recibir conexiones. Si `code`
    /// es None, el servidor genera un codigo temporal random. Si
    /// viene un `code`, se usa como codigo fijo (decision de producto:
    /// codigo temporal por defecto, con opcion de fijar uno
    /// permanente para equipos de acceso frecuente).
    RegisterAgent { code: Option<String> },

    /// El controller pide conectarse al agent que tiene ese codigo.
    Connect { code: String },

    /// Una vez emparejados, cualquier mensaje de aplicacion (futuro:
    /// oferta/respuesta SDP, candidatos ICE, o mientras no tengamos
    /// P2P real, los frames de video/input directamente) se manda
    /// asi y el servidor lo reenvia tal cual a la otra punta.
    Relay { payload: serde_json::Value },
}

/// Mensajes que el servidor le manda de vuelta a un cliente.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    /// Confirma el registro del agent y le informa el codigo final
    /// (el que mando, o el generado si no mando ninguno).
    Registered { code: String },

    /// Le avisa al agent que un controller se quiere conectar, y al
    /// controller que la conexion se establecio.
    Paired,

    /// La otra punta se desconecto - la sesion ya no es valida.
    PeerDisconnected,

    /// Reenvio de un mensaje de aplicacion desde la otra punta.
    Relay { payload: serde_json::Value },

    /// Algo salio mal (codigo no encontrado, ya emparejado, etc).
    Error { message: String },
}
