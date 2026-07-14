; Clipd NSIS Installer
; Produces Clipd-Setup.exe — a proper Windows installer with:
;   - Install to %LOCALAPPDATA%\Clipd (no admin needed)
;   - Start Menu shortcuts
;   - Desktop shortcut (optional)
;   - Add to PATH
;   - Auto-start via Startup folder (optional)
;   - Uninstaller (appears in Add/Remove Programs)
;
; Build:  makensis /DVERSION=0.1.1 clipd-installer.nsi

!define PRODUCT_NAME "Clipd"
!define PRODUCT_PUBLISHER "Shweta Kadam"
!define PRODUCT_WEB_SITE "https://github.com/shwetarkadam/clipd"
!define PRODUCT_UNINST_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\${PRODUCT_NAME}"

!ifndef VERSION
  !define VERSION "0.1.1"
!endif

Name "${PRODUCT_NAME} ${VERSION}"
OutFile "Clipd-Setup-${VERSION}.exe"
InstallDir "$LOCALAPPDATA\${PRODUCT_NAME}"
RequestExecutionLevel user

!include "MUI2.nsh"
!include "FileFunc.nsh"

!define MUI_ABORTWARNING
!define MUI_ICON "${NSISDIR}\Contrib\Graphics\Icons\modern-install.ico"
!define MUI_UNICON "${NSISDIR}\Contrib\Graphics\Icons\modern-uninstall.ico"
!define MUI_FINISHPAGE_RUN "$INSTDIR\clipd-ui.exe"
!define MUI_FINISHPAGE_RUN_TEXT "Launch Clipd (GUI)"

!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_DIRECTORY
Page custom AutoStartPage AutoStartPageLeave
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_WELCOME
!insertmacro MUI_UNPAGE_INSTFILES

!insertmacro MUI_LANGUAGE "English"

Var AutoStartCheckbox
Var TerminalCheckbox
Var AutoStartChoice
Var TerminalChoice

Function .onInit
  ; GUI is the product default. Terminal/TUI access is an explicit opt-in.
  StrCpy $AutoStartChoice ${BST_CHECKED}
  StrCpy $TerminalChoice ${BST_UNCHECKED}
FunctionEnd

Function AutoStartPage
  !insertmacro MUI_HEADER_TEXT "Startup & optional tools" "Clipd opens as a GUI by default."
  nsDialogs::Create 1018
  Pop $0
  ${NSD_CreateCheckbox} 0 0 100% 12u "Start Clipd automatically on login"
  Pop $AutoStartCheckbox
  ${NSD_SetState} $AutoStartCheckbox $AutoStartChoice
  ${NSD_CreateCheckbox} 0 24u 100% 12u "Add optional Developer Terminal / TUI shortcut"
  Pop $TerminalCheckbox
  ${NSD_SetState} $TerminalCheckbox $TerminalChoice
  ${NSD_CreateLabel} 0 48u 100% 28u "The terminal is never opened automatically. You can enable Developer Terminal mode later from the Clipd tray menu."
  Pop $0
  nsDialogs::Show
FunctionEnd

Function AutoStartPageLeave
  ${NSD_GetState} $AutoStartCheckbox $AutoStartChoice
  ${NSD_GetState} $TerminalCheckbox $TerminalChoice
FunctionEnd

Section "MainSection" SEC01
  ; Upgrades must not leave the old tray/GUI/daemon alive and then launch a
  ; second copy. taskkill is run through nsExec, so no terminal is displayed.
  nsExec::Exec '"$SYSDIR\taskkill.exe" /F /IM clipd-ui.exe'
  Pop $0
  nsExec::Exec '"$SYSDIR\taskkill.exe" /F /IM clipd-gui.exe'
  Pop $0
  nsExec::Exec '"$SYSDIR\taskkill.exe" /F /IM clipd.exe'
  Pop $0
  nsExec::Exec '"$SYSDIR\taskkill.exe" /F /IM clipd-overlay.exe'
  Pop $0
  nsExec::Exec '"$SYSDIR\taskkill.exe" /F /IM clipd-picker.exe'
  Pop $0
  Sleep 300

  SetOutPath "$INSTDIR"
  SetOverwrite on

  File "clipd.exe"
  File "clipd-ui.exe"
  File "clipd-gui.exe"
  File "clipd-mcp.exe"
  File "clipd-overlay.exe"
  File "clipd-picker.exe"

  CreateDirectory "$SMPROGRAMS\${PRODUCT_NAME}"
  CreateShortCut "$SMPROGRAMS\${PRODUCT_NAME}\${PRODUCT_NAME}.lnk" "$INSTDIR\clipd-ui.exe"
  Delete "$SMPROGRAMS\${PRODUCT_NAME}\${PRODUCT_NAME} GUI.lnk"
  Delete "$SMPROGRAMS\${PRODUCT_NAME}\${PRODUCT_NAME} CLI.lnk"
  CreateShortCut "$SMPROGRAMS\${PRODUCT_NAME}\Uninstall ${PRODUCT_NAME}.lnk" "$INSTDIR\uninstall.exe"

  CreateShortCut "$DESKTOP\${PRODUCT_NAME}.lnk" "$INSTDIR\clipd-ui.exe"
SectionEnd

Section -Post
  WriteUninstaller "$INSTDIR\uninstall.exe"
  WriteRegStr HKCU "${PRODUCT_UNINST_KEY}" "DisplayName" "${PRODUCT_NAME}"
  WriteRegStr HKCU "${PRODUCT_UNINST_KEY}" "DisplayVersion" "${VERSION}"
  WriteRegStr HKCU "${PRODUCT_UNINST_KEY}" "Publisher" "${PRODUCT_PUBLISHER}"
  WriteRegStr HKCU "${PRODUCT_UNINST_KEY}" "URLInfoAbout" "${PRODUCT_WEB_SITE}"
  WriteRegStr HKCU "${PRODUCT_UNINST_KEY}" "UninstallString" "$INSTDIR\uninstall.exe"
  WriteRegStr HKCU "${PRODUCT_UNINST_KEY}" "QuietUninstallString" "$INSTDIR\uninstall.exe /S"

  ; PATH is not modified here: the EnVar plugin isn't in stock NSIS (breaks CI
  ; compile). CLI users can run install.ps1 or add %LOCALAPPDATA%\Clipd to PATH.

  ; Respect installer choices. GUI/tray is always the normal launch path;
  ; terminal mode is present only when the user explicitly asks for it.
  StrCmp $AutoStartChoice ${BST_CHECKED} 0 NoAutoStart
    CreateShortCut "$SMSTARTUP\${PRODUCT_NAME}.lnk" "$INSTDIR\clipd-ui.exe"
    Goto AutoStartDone
  NoAutoStart:
    Delete "$SMSTARTUP\${PRODUCT_NAME}.lnk"
  AutoStartDone:

  StrCmp $TerminalChoice ${BST_CHECKED} 0 NoTerminalShortcut
    CreateShortCut "$SMPROGRAMS\${PRODUCT_NAME}\${PRODUCT_NAME} Developer Terminal (Optional).lnk" "$SYSDIR\cmd.exe" '/K ""$INSTDIR\clipd.exe" search"' "$INSTDIR\clipd.exe" 0
    Goto TerminalShortcutDone
  NoTerminalShortcut:
    Delete "$SMPROGRAMS\${PRODUCT_NAME}\${PRODUCT_NAME} Developer Terminal (Optional).lnk"
  TerminalShortcutDone:
SectionEnd

Section Uninstall
  Delete "$INSTDIR\clipd.exe"
  Delete "$INSTDIR\clipd-ui.exe"
  Delete "$INSTDIR\clipd-gui.exe"
  Delete "$INSTDIR\clipd-mcp.exe"
  Delete "$INSTDIR\clipd-overlay.exe"
  Delete "$INSTDIR\clipd-picker.exe"
  Delete "$INSTDIR\uninstall.exe"

  RMDir "$INSTDIR"

  Delete "$SMPROGRAMS\${PRODUCT_NAME}\*.lnk"
  RMDir "$SMPROGRAMS\${PRODUCT_NAME}"
  Delete "$DESKTOP\${PRODUCT_NAME}.lnk"
  Delete "$SMSTARTUP\${PRODUCT_NAME}.lnk"

  DeleteRegKey HKCU "${PRODUCT_UNINST_KEY}"
SectionEnd
