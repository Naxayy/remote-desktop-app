# Instalador del agent

Dos formas de instalar el agent en una PC remota, sin que esa persona
toque Rust ni código fuente.

Desde esta versión, el agent tiene un **ícono en la bandeja del
sistema**: el cliente puede hacer click ahí para abrir una ventana
chica donde ve/edita el servidor y el código de conexión, y puede
desconectar manualmente. Eso lo maneja un segundo ejecutable,
`agent-ui.exe`, que el servicio lanza solo en la sesión del usuario
logueado - por eso ahora se empaquetan los dos `.exe` juntos.

## Opción A (recomendada para clientes): instalador `.exe` con NSIS

Un único `.exe` que, al ejecutarlo, le muestra al cliente una pantalla
para elegir:
- **Configuración recomendada**: usa tu servidor fijo (`wss://remote-desktop.skyguard.com.ar`,
  definido dentro del `.nsi` — cambialo ahí si tu dominio cambia) +
  un código alfanumérico generado al azar en el momento de instalar.
- **Configuración personalizada**: la persona escribe su propio
  servidor y código.

**Requisito único (una sola vez, en tu PC):** instalar NSIS (gratis):
https://nsis.sourceforge.io/Download — el instalador agrega `makensis`
al PATH solo.

**Armar el instalador:**
```powershell
cd agent\installer
.\build-installer.ps1
```

Esto te deja `RemoteDesktopAppAgent-Setup.exe` en esa misma carpeta —
ese único archivo es todo lo que le mandás al cliente. Al ejecutarlo:
- Pide permiso de administrador (UAC)
- Muestra la pantalla de configuración (recomendada / personalizada)
- Instala el agent en `Program Files`
- Configura la URL/código elegidos automáticamente
- Registra e inicia el servicio de Windows
- Muestra el código de conexión al terminar (para que se lo pases al
  cliente si usó la opción recomendada con código al azar)
- Queda con entrada en "Agregar o quitar programas" para desinstalar
  fácil

## Opción B (para vos, o gente técnica): script de PowerShell

Más manual, pero no necesita NSIS instalado - útil para pruebas
rápidas o si preferís pasarle la URL/código a mano en el momento:

```powershell
cargo build --release -p agent --bin agent
cargo build --release -p agent-ui --bin agent-ui
Copy-Item ..\..\target\release\agent.exe .
Copy-Item ..\..\target\release\agent-ui.exe .
```

Y le mandás `agent.exe` + `agent-ui.exe` + `install-agent.ps1` +
`uninstall-agent.ps1` (los 4 archivos de esta carpeta) a quien lo vaya
a instalar. Instrucciones
para esa persona:

1. Descomprimir en cualquier carpeta.
2. Click derecho sobre `install-agent.ps1` → **"Ejecutar con PowerShell"**
   (como administrador).
3. Si Windows bloquea el script, correr antes (una vez, como admin):
   ```powershell
   Set-ExecutionPolicy RemoteSigned -Scope CurrentUser
   ```
4. El script pide la URL y el código por consola.

## Desinstalar

- Instalador `.exe`: desde "Agregar o quitar programas" de Windows.
- Script de PowerShell: correr `uninstall-agent.ps1` como administrador.
