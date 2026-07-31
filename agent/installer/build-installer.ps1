<#
Arma el instalador final del agent (RemoteDesktopAppAgent-Setup.exe).

A diferencia de versiones anteriores, ya NO hace falta pasarle la URL
ni el codigo por parametro - eso ahora lo elige la persona que instala,
en una pantalla propia del instalador:
  - "Configuracion recomendada": usa el servidor fijo (definido adentro
    del .nsi) + un codigo alfanumerico generado al azar en el momento.
  - "Configuracion personalizada": la persona escribe su propio
    servidor y codigo.

Uso:
    .\build-installer.ps1

Requiere NSIS instalado: https://nsis.sourceforge.io/Download
(el instalador de NSIS agrega "makensis" al PATH solo)
#>

$ErrorActionPreference = "Stop"
$repoRoot = Resolve-Path (Join-Path $PSScriptRoot "..\..")

Write-Host "Compilando agent.exe (release)..."
Push-Location $repoRoot
cargo build --release -p agent --bin agent
Pop-Location

Write-Host "Copiando el binario a la carpeta del instalador..."
Copy-Item (Join-Path $repoRoot "target\release\agent.exe") $PSScriptRoot -Force

$makensis = Get-Command makensis -ErrorAction SilentlyContinue
if (-not $makensis) {
    Write-Error "No se encontro 'makensis' en el PATH. Instala NSIS desde https://nsis.sourceforge.io/Download y volve a intentar."
    exit 1
}

Write-Host "Compilando el instalador con NSIS..."
Push-Location $PSScriptRoot
makensis agent-installer.nsi
Pop-Location

Write-Host ""
Write-Host "========================================"
Write-Host " Listo: $PSScriptRoot\RemoteDesktopAppAgent-Setup.exe"
Write-Host "========================================"
Write-Host " Mandale ese unico archivo .exe al cliente."
Write-Host " Al abrirlo va a poder elegir entre la configuracion"
Write-Host " recomendada (servidor fijo + codigo al azar) o poner"
Write-Host " su propio servidor/codigo."
