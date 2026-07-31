<#
Instalador del agent de Remote Desktop App.

Uso: correr como Administrador (clic derecho sobre el archivo >
"Ejecutar con PowerShell", o desde una terminal de administrador):

    .\install-agent.ps1

Tambien acepta los parametros directo, para instalar sin que te
pregunte nada (util para desplegar en varias PCs con el mismo script):

    .\install-agent.ps1 -SignalingUrl "wss://tu-dominio.com" -AgentCode "123456"

Este script y agent.exe tienen que estar en la MISMA carpeta.
#>

param(
    [string]$SignalingUrl = "",
    [string]$AgentCode = ""
)

$ErrorActionPreference = "Stop"

# Confirmar que se esta corriendo como administrador - hace falta
# para registrar el servicio de Windows.
$currentPrincipal = New-Object Security.Principal.WindowsPrincipal([Security.Principal.WindowsIdentity]::GetCurrent())
if (-not $currentPrincipal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    Write-Error "Este script necesita correr como Administrador. Cerra esta ventana, click derecho sobre install-agent.ps1 y elegi 'Ejecutar con PowerShell' desde una sesion de administrador."
    exit 1
}

$installDir = "$env:ProgramFiles\RemoteDesktopAppAgent"
$exeName = "agent.exe"
$sourceExe = Join-Path $PSScriptRoot $exeName

if (-not (Test-Path $sourceExe)) {
    Write-Error "No se encontro '$exeName' en la misma carpeta que este script ($PSScriptRoot). Copia agent.exe ahi antes de instalar."
    exit 1
}

if (-not $SignalingUrl) {
    $SignalingUrl = Read-Host "URL del signaling server (ej: wss://tu-dominio.com)"
}
if (-not $AgentCode) {
    $AgentCode = Read-Host "Codigo fijo para este equipo (ej: 123456)"
}

Write-Host ""
Write-Host "Instalando en $installDir..."
New-Item -ItemType Directory -Path $installDir -Force | Out-Null
Copy-Item -Path $sourceExe -Destination $installDir -Force
$agentExePath = Join-Path $installDir $exeName

Write-Host "Configurando variables de entorno del sistema..."
# OJO: "Machine" (no "User") - el servicio corre como LocalSystem y
# solo ve las variables de entorno a nivel sistema, no las de sesion
# de usuario.
[System.Environment]::SetEnvironmentVariable("SIGNALING_URL", $SignalingUrl, "Machine")
[System.Environment]::SetEnvironmentVariable("AGENT_CODE", $AgentCode, "Machine")

# Si ya habia una instalacion previa, la sacamos primero para evitar
# el error de "el servicio ya existe". Si NO habia instalacion previa
# (caso normal, primera vez), esto va a fallar - es esperado, por eso
# bajamos el ErrorActionPreference solo para esta linea.
$previousPreference = $ErrorActionPreference
$ErrorActionPreference = "Continue"
& $agentExePath uninstall 2>$null | Out-Null
$ErrorActionPreference = $previousPreference

Write-Host "Registrando el servicio de Windows..."
& $agentExePath install

Write-Host "Iniciando el servicio..."
Start-Service -Name "RemoteDesktopAppAgent"

Write-Host ""
Write-Host "========================================"
Write-Host " Instalacion completa"
Write-Host "========================================"
Write-Host " Codigo de conexion: $AgentCode"
Write-Host " El agent va a arrancar solo con Windows de ahora en mas,"
Write-Host " incluso antes de iniciar sesion."
Write-Host ""
Write-Host " Logs: C:\ProgramData\RemoteDesktopAppAgent\agent.log"
Write-Host "========================================"
