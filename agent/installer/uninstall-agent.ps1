<#
Desinstalador del agent de Remote Desktop App.
Correr como Administrador.
#>

$ErrorActionPreference = "Continue"

$currentPrincipal = New-Object Security.Principal.WindowsPrincipal([Security.Principal.WindowsIdentity]::GetCurrent())
if (-not $currentPrincipal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    Write-Error "Este script necesita correr como Administrador."
    exit 1
}

$installDir = "$env:ProgramFiles\RemoteDesktopAppAgent"
$agentExePath = Join-Path $installDir "agent.exe"

if (Test-Path $agentExePath) {
    Write-Host "Deteniendo y quitando el servicio..."
    & $agentExePath uninstall
} else {
    Write-Host "No se encontro una instalacion en $installDir - intentando limpiar el servicio de todos modos."
    Stop-Service -Name "RemoteDesktopAppAgent" -ErrorAction SilentlyContinue
    sc.exe delete "RemoteDesktopAppAgent" | Out-Null
}

Write-Host "Borrando archivos..."
Remove-Item -Path $installDir -Recurse -Force -ErrorAction SilentlyContinue

Write-Host "Limpiando variables de entorno..."
[System.Environment]::SetEnvironmentVariable("SIGNALING_URL", $null, "Machine")
[System.Environment]::SetEnvironmentVariable("AGENT_CODE", $null, "Machine")

Write-Host ""
Write-Host "Agent desinstalado. Los archivos recibidos en C:\ProgramData\RemoteDesktopAppAgent\received"
Write-Host "y los logs se dejaron sin tocar por si los queres revisar - borralos a mano si haces falta."
