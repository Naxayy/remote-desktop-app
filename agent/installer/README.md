# Instalador del agent

Carpeta con todo lo necesario para instalar el agent en una PC remota
sin que esa persona tenga que tocar Rust, git, ni nada del código
fuente — solo necesita 2 archivos.

## Armar el paquete para distribuir

En tu PC (con el proyecto compilado):

```powershell
cd C:\Users\USUARIO\Desktop\remote-desktop-app
cargo build --release -p agent --bin agent

# Copiar el .exe a esta carpeta
Copy-Item target\release\agent.exe agent\installer\

# Comprimir todo en un zip para mandar
Compress-Archive -Path agent\installer\* -DestinationPath agent-installer.zip -Force
```

Eso te deja `agent-installer.zip` con 3 archivos adentro:
- `agent.exe`
- `install-agent.ps1`
- `uninstall-agent.ps1`

## Lo que tiene que hacer la persona que lo recibe

1. Descomprimir el zip en cualquier carpeta.
2. Click derecho sobre `install-agent.ps1` → **"Ejecutar con PowerShell"**
   (si Windows no da esa opción directo, abrir PowerShell como
   Administrador, `cd` hasta la carpeta, y correr `.\install-agent.ps1`).
3. Si Windows bloquea el script por política de ejecución, correr antes
   (una sola vez, como administrador):
   ```powershell
   Set-ExecutionPolicy RemoteSigned -Scope CurrentUser
   ```
4. El script va a pedir la URL del signaling server y el código de
   conexión - se los pasás vos de antemano.

Después de eso, el agent queda instalado como servicio de Windows,
arranca solo con la PC, y no hace falta tocar nada más.

## Desinstalar

Mismo procedimiento pero con `uninstall-agent.ps1`.
