; Instalador del agent de Remote Desktop App.
;
; Pantalla de configuracion con dos opciones:
;   - Recomendada: servidor fijo (el homelab) + codigo alfanumerico
;     generado al azar en el momento de instalar.
;   - Personalizada: el usuario escribe su propio servidor y codigo.
;
; Compilar con:
;   makensis agent-installer.nsi
;
; Necesita NSIS instalado (gratis): https://nsis.sourceforge.io/Download
; Y el agent.exe compilado en la misma carpeta que este script.

!include "MUI2.nsh"
!include "WinMessages.nsh"
!include "nsDialogs.nsh"
!include "LogicLib.nsh"

; Servidor por defecto de la opcion "recomendada". Cambialo aca si tu
; signaling server cambia de dominio.
!define DEFAULT_SIGNALING_URL "wss://remote-desktop.skyguard.com.ar"

Name "Remote Desktop App - Agent"
OutFile "RemoteDesktopAppAgent-Setup.exe"
InstallDir "$PROGRAMFILES64\RemoteDesktopAppAgent"
InstallDirRegKey HKLM "Software\RemoteDesktopAppAgent" "InstallDir"
RequestExecutionLevel admin
Unicode true

!define MUI_ABORTWARNING
!define MUI_ICON "..\..\controller-ui\src-tauri\icons\icon.ico"
!define MUI_UNICON "..\..\controller-ui\src-tauri\icons\icon.ico"

Var Dialog
Var RadioDefault
Var RadioCustom
Var TextServerCustom
Var TextCodeCustom
Var LabelCodeDefault
Var FinalSignalingUrl
Var FinalAgentCode
Var GeneratedCode

!insertmacro MUI_PAGE_WELCOME
Page custom ConfigPageCreate ConfigPageLeave
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH

!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES

!insertmacro MUI_LANGUAGE "Spanish"

; Genera un codigo alfanumerico de 6 caracteres (hex de un GUID nuevo -
; no hace falta que sea criptograficamente random, solo que no se
; repita entre instalaciones).
Function GenerateRandomCode
    System::Call "ole32::CoCreateGuid(g .r0)"
    System::Call "ole32::StringFromGUID2(g r0, t .r1, i 40)"
    StrCpy $0 $1 6 1
    Push $0
FunctionEnd

Function .onInit
    Call GenerateRandomCode
    Pop $GeneratedCode
FunctionEnd

Function ConfigPageCreate
    nsDialogs::Create 1018
    Pop $Dialog
    ${If} $Dialog == error
        Abort
    ${EndIf}

    ${NSD_CreateLabel} 0 0 100% 24u "Como queres configurar este equipo?"
    Pop $0

    ${NSD_CreateRadioButton} 10u 24u 90% 12u "Usar configuracion recomendada"
    Pop $RadioDefault
    ${NSD_SetState} $RadioDefault ${BST_CHECKED}
    ${NSD_OnClick} $RadioDefault OnConfigModeChange

    ${NSD_CreateLabel} 20u 38u 90% 12u "Servidor: ${DEFAULT_SIGNALING_URL}"
    Pop $0

    ${NSD_CreateLabel} 20u 50u 90% 12u "Codigo: $GeneratedCode"
    Pop $LabelCodeDefault

    ${NSD_CreateRadioButton} 10u 68u 90% 12u "Configuracion personalizada"
    Pop $RadioCustom
    ${NSD_OnClick} $RadioCustom OnConfigModeChange

    ${NSD_CreateLabel} 20u 84u 25% 12u "Servidor:"
    Pop $0
    ${NSD_CreateText} 90u 82u 55% 12u "${DEFAULT_SIGNALING_URL}"
    Pop $TextServerCustom
    EnableWindow $TextServerCustom 0

    ${NSD_CreateLabel} 20u 100u 25% 12u "Codigo:"
    Pop $0
    ${NSD_CreateText} 90u 98u 55% 12u ""
    Pop $TextCodeCustom
    EnableWindow $TextCodeCustom 0

    nsDialogs::Show
FunctionEnd

Function OnConfigModeChange
    ${NSD_GetState} $RadioCustom $0
    ${If} $0 == ${BST_CHECKED}
        EnableWindow $TextServerCustom 1
        EnableWindow $TextCodeCustom 1
    ${Else}
        EnableWindow $TextServerCustom 0
        EnableWindow $TextCodeCustom 0
    ${EndIf}
FunctionEnd

Function ConfigPageLeave
    ${NSD_GetState} $RadioCustom $0
    ${If} $0 == ${BST_CHECKED}
        ${NSD_GetText} $TextServerCustom $FinalSignalingUrl
        ${NSD_GetText} $TextCodeCustom $FinalAgentCode
        ${If} $FinalSignalingUrl == ""
        ${OrIf} $FinalAgentCode == ""
            MessageBox MB_OK "Completa el servidor y el codigo, o elegi la configuracion recomendada."
            Abort
        ${EndIf}
    ${Else}
        StrCpy $FinalSignalingUrl "${DEFAULT_SIGNALING_URL}"
        StrCpy $FinalAgentCode $GeneratedCode
    ${EndIf}
FunctionEnd

Section "Instalar" SecInstall
    SetOutPath "$INSTDIR"
    File "agent.exe"
    File "agent-ui.exe"

    WriteRegStr HKLM "Software\RemoteDesktopAppAgent" "InstallDir" "$INSTDIR"

    DetailPrint "Configurando variables de entorno del sistema..."
    WriteRegExpandStr HKLM "SYSTEM\CurrentControlSet\Control\Session Manager\Environment" "SIGNALING_URL" "$FinalSignalingUrl"
    WriteRegExpandStr HKLM "SYSTEM\CurrentControlSet\Control\Session Manager\Environment" "AGENT_CODE" "$FinalAgentCode"
    SendMessage ${HWND_BROADCAST} ${WM_SETTINGCHANGE} 0 "STR:Environment" /TIMEOUT=5000

    DetailPrint "Quitando una instalacion anterior del servicio, si habia..."
    nsExec::ExecToLog '"$INSTDIR\agent.exe" uninstall'

    DetailPrint "Registrando el servicio de Windows..."
    nsExec::ExecToLog '"$INSTDIR\agent.exe" install'

    DetailPrint "Iniciando el servicio..."
    nsExec::ExecToLog 'net start RemoteDesktopAppAgent'

    WriteUninstaller "$INSTDIR\uninstall.exe"

    WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\RemoteDesktopAppAgent" "DisplayName" "Remote Desktop App - Agent"
    WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\RemoteDesktopAppAgent" "UninstallString" "$\"$INSTDIR\uninstall.exe$\""
    WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\RemoteDesktopAppAgent" "InstallLocation" "$\"$INSTDIR$\""
    WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\RemoteDesktopAppAgent" "Publisher" "Nicolas Carmona"
    WriteRegDWORD HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\RemoteDesktopAppAgent" "NoModify" 1
    WriteRegDWORD HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\RemoteDesktopAppAgent" "NoRepair" 1

    MessageBox MB_OK "Instalacion completa.$\r$\nCodigo de conexion: $FinalAgentCode"
SectionEnd

Section "Uninstall"
    DetailPrint "Deteniendo y quitando el servicio..."
    nsExec::ExecToLog '"$INSTDIR\agent.exe" uninstall'

    Delete "$INSTDIR\agent.exe"
    Delete "$INSTDIR\agent-ui.exe"
    Delete "$INSTDIR\uninstall.exe"
    RMDir "$INSTDIR"

    DeleteRegValue HKLM "SYSTEM\CurrentControlSet\Control\Session Manager\Environment" "SIGNALING_URL"
    DeleteRegValue HKLM "SYSTEM\CurrentControlSet\Control\Session Manager\Environment" "AGENT_CODE"
    SendMessage ${HWND_BROADCAST} ${WM_SETTINGCHANGE} 0 "STR:Environment" /TIMEOUT=5000

    DeleteRegKey HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\RemoteDesktopAppAgent"
    DeleteRegKey HKLM "Software\RemoteDesktopAppAgent"
SectionEnd
