# Remote Desktop App

Software open source de escritorio remoto para servicio tecnico, pensado para
conectarse desde una PC a otra: visualizar el escritorio, interactuar a nivel
administrador, transferir archivos y reiniciar la maquina remota con
reconexion automatica.

## Estado del proyecto

En desarrollo activo. Version inicial (v1) apuntada a Windows.

## Arquitectura

| Componente          | Lenguaje / Stack   | Rol                                                              |
|----------------------|---------------------|-------------------------------------------------------------------|
| `core-engine`         | Rust                | Captura de pantalla, codificacion de video, input, cifrado       |
| `agent`               | Rust (Windows Service) | Corre en la PC remota, expone el core-engine sobre la red     |
| `controller-ui`       | Tauri (Rust + web)  | App desde la que te conectas a las PCs remotas                   |
| `signaling-server`    | Rust                | Coordina el handshake inicial entre controller y agent           |
| `relay-server`        | Rust                | Fallback tipo TURN cuando la conexion P2P directa no es posible  |

### Modelo de conexion

Conexion hibrida: intenta P2P directo (NAT hole punching) y, si no es
posible, cae automaticamente a relay. El `signaling-server` y el
`relay-server` estan pensados para correr self-hosteados (por ejemplo, en un
homelab con Docker detras de un Cloudflare Tunnel), y cualquiera puede
clonar el repo y levantar su propia infraestructura con `docker-compose.yml`.

### Autenticacion

Codigo de sesion temporal por defecto, con opcion de fijar un codigo
permanente para equipos de acceso frecuente.

### Persistencia tras reinicio

El `agent` se instala como servicio de Windows que arranca antes del login,
por lo que el controller puede reconectarse automaticamente despues de un
reinicio remoto, sin necesidad de que haya un usuario logueado.

## Estructura del repo

```
remote-desktop-app/
├── core-engine/        # Motor: captura, encode, input, red, cripto
├── agent/               # Servicio de Windows que corre en la PC remota
├── controller-ui/       # App Tauri desde la que te conectas
├── signaling-server/    # Servidor de señalizacion (handshake)
├── relay-server/         # Servidor de relay (fallback TURN-like)
├── docker/               # Dockerfiles de signaling-server y relay-server
└── docker-compose.yml    # Levanta signaling + relay para self-hosting
```

## Desarrollo local

```bash
# Compilar y chequear todos los crates de Rust
cargo check

# Levantar signaling-server y relay-server localmente (self-hosting)
docker compose up --build
```

## Roadmap

- [x] Esqueleto del workspace (Rust + Tauri)
- [ ] Captura de pantalla en Windows (DXGI)
- [ ] Codificacion/streaming de video
- [ ] Signaling server (handshake + auth por codigo)
- [ ] Conexion P2P con fallback a relay
- [ ] Inyeccion de input remoto
- [ ] Transferencia de archivos
- [ ] Reinicio remoto + reconexion automatica
- [ ] Servicio de Windows para el agent
- [ ] UI del controller

## Licencia

MIT - ver [LICENSE](./LICENSE).
