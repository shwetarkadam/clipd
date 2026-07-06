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

!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES

Page custom AutoStartPage AutoStartPageLeave

!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_WELCOME
!insertmacro MUI_UNPAGE_INSTFILES

!insertmacro MUI_LANGUAGE "English"

Var AutoStart

Function AutoStartPage
  !insertmacro MUI_HEADER_TEXT "Auto-Start" "Launch Clipd automatically when you log in?"
  nsDialogs::Create 1018
  Pop $0
  ${NSD_CreateCheckbox} 0 0 100% 12u "Start Clipd automatically on login"
  Pop $AutoStart
  ${NSD_SetState} $AutoStart ${BST_CHECKED}
  nsDialogs::Show
FunctionEnd

Function AutoStartPageLeave
  ${NSD_GetState} $AutoStart $0
  StrCmp $0 ${BST_CHECKED} 0 +3
    CreateShortCut "$SMSTARTUP\${PRODUCT_NAME}.lnk" "$INSTDIR\clipd-ui.exe"
  Goto +2
    Delete "$SMSTARTUP\${PRODUCT_NAME}.lnk"
FunctionEnd

Section "MainSection" SEC01
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
  CreateShortCut "$SMPROGRAMS\${PRODUCT_NAME}\${PRODUCT_NAME} GUI.lnk" "$INSTDIR\clipd-gui.exe"
  CreateShortCut "$SMPROGRAMS\${PRODUCT_NAME}\${PRODUCT_NAME} CLI.lnk" "$INSTDIR\clipd.exe"
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

  ; Auto-start
  CreateShortCut "$SMSTARTUP\${PRODUCT_NAME}.lnk" "$INSTDIR\clipd-ui.exe"
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
