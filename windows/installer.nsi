; SPDX-License-Identifier: GPL-3.0-or-later
Unicode true
!include "MUI2.nsh"
!include "LogicLib.nsh"
!include "x64.nsh"
!include "WinVer.nsh"
!ifndef BINARY_DIR
  !define BINARY_DIR "target/x86_64-pc-windows-msvc/release"
!endif
!ifndef OUTPUT_FILE
  !define OUTPUT_FILE "dist/flowmux-windows-0.10.1-dev-x64-setup.exe"
!endif
Name "flowmux (Windows development)"
OutFile "${OUTPUT_FILE}"
InstallDir "$LOCALAPPDATA\Programs\flowmux-windows"
InstallDirRegKey HKCU "Software\flowmux\Windows" "InstallDir"
RequestExecutionLevel user
SetCompressor /SOLID lzma
!define MUI_ABORTWARNING
!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_LICENSE "../LICENSE"
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "English"
!insertmacro MUI_LANGUAGE "Korean"

Function .onInit
  ${IfNot} ${RunningX64}
    MessageBox MB_ICONSTOP "This build requires 64-bit Windows."
    Abort
  ${EndIf}
  ${IfNot} ${AtLeastWin10}
    MessageBox MB_ICONSTOP "flowmux requires Windows 10 version 1809 or newer."
    Abort
  ${EndIf}
  SetRegView 64
  ReadRegStr $0 HKLM "SOFTWARE\Microsoft\Windows NT\CurrentVersion" "CurrentBuildNumber"
  ${If} $0 < 17763
    MessageBox MB_ICONSTOP "flowmux requires Windows build 17763 or newer for ConPTY."
    Abort
  ${EndIf}
  SetShellVarContext current
FunctionEnd

Section "flowmux" Main
  SetOutPath "$INSTDIR"
  ; NSIS prompts if an existing executable is in use. Never kill a running user session.
  File "${BINARY_DIR}/flowmux.exe"
  File "${BINARY_DIR}/flowmuxctl.exe"
  File /oname=flowmux.com "${BINARY_DIR}/flowmux-command.exe"
  File "${BINARY_DIR}/conpty.dll"
  SetOutPath "$INSTDIR\x64"
  File "${BINARY_DIR}/x64/OpenConsole.exe"
  SetOutPath "$INSTDIR"
  File /oname=LICENSE "../LICENSE"
  File "assets/THIRD_PARTY.txt"
  File "assets/THIRD_PARTY_RUST.txt"
  File "scripts/configure-path.ps1"
  File "IMPLEMENTATION.md"
  File "acceptance.json"
  nsExec::ExecToStack '"$INSTDIR\flowmuxctl.exe" doctor'
  Pop $0
  Pop $1
  ${If} $0 != 0
    InitPluginsDir
    SetOutPath "$PLUGINSDIR"
    File "scripts/install-webview2.ps1"
    ExecWait '"$SYSDIR\WindowsPowerShell\v1.0\powershell.exe" -NoProfile -ExecutionPolicy Bypass -File "$PLUGINSDIR\install-webview2.ps1"' $0
    nsExec::ExecToStack '"$INSTDIR\flowmuxctl.exe" doctor'
    Pop $0
    Pop $1
    ${If} $0 != 0
      MessageBox MB_ICONSTOP "WebView2 could not be installed. Connect to the internet, install Microsoft Edge WebView2 Runtime, then run this installer again."
      SetErrorLevel 1
      Abort
    ${EndIf}
  ${EndIf}
  SetOutPath "$INSTDIR"
  WriteUninstaller "$INSTDIR\Uninstall.exe"
  ExecWait '"$SYSDIR\WindowsPowerShell\v1.0\powershell.exe" -NoProfile -ExecutionPolicy Bypass -File "$INSTDIR\configure-path.ps1" -Action Add -Directory "$INSTDIR"' $0
  SendMessage 0xffff 0x001A 0 "STR:Environment" /TIMEOUT=1000
  ${If} $0 != 0
    MessageBox MB_ICONEXCLAMATION "flowmux was installed, but your user PATH could not be updated."
  ${EndIf}
  CreateDirectory "$SMPROGRAMS\flowmux (Windows)"
  CreateShortcut "$SMPROGRAMS\flowmux (Windows)\flowmux.lnk" "$INSTDIR\flowmux.exe"
  CreateShortcut "$SMPROGRAMS\flowmux (Windows)\Uninstall.lnk" "$INSTDIR\Uninstall.exe"
  WriteRegStr HKCU "Software\flowmux\Windows" "InstallDir" "$INSTDIR"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\flowmux-windows" "DisplayName" "flowmux (Windows development)"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\flowmux-windows" "DisplayVersion" "0.10.1-dev"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\flowmux-windows" "UninstallString" '"$INSTDIR\Uninstall.exe"'
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\flowmux-windows" "InstallLocation" "$INSTDIR"
  WriteRegDWORD HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\flowmux-windows" "NoModify" 1
  WriteRegDWORD HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\flowmux-windows" "NoRepair" 1
SectionEnd

Section "Uninstall"
  SetShellVarContext current
  SetRegView 64
  ExecWait '"$SYSDIR\WindowsPowerShell\v1.0\powershell.exe" -NoProfile -ExecutionPolicy Bypass -File "$INSTDIR\configure-path.ps1" -Action Remove -Directory "$INSTDIR"' $0
  SendMessage 0xffff 0x001A 0 "STR:Environment" /TIMEOUT=1000
  Delete "$SMPROGRAMS\flowmux (Windows)\flowmux.lnk"
  Delete "$SMPROGRAMS\flowmux (Windows)\Uninstall.lnk"
  RMDir "$SMPROGRAMS\flowmux (Windows)"
  DeleteRegKey HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\flowmux-windows"
  DeleteRegKey HKCU "Software\flowmux\Windows"
  ; Remove only installed payloads. User profiles, workspace files and settings remain.
  Delete "$INSTDIR\flowmux.exe"
  Delete "$INSTDIR\flowmuxctl.exe"
  Delete "$INSTDIR\flowmux.com"
  Delete "$INSTDIR\conpty.dll"
  Delete "$INSTDIR\x64\OpenConsole.exe"
  RMDir "$INSTDIR\x64"
  Delete "$INSTDIR\LICENSE"
  Delete "$INSTDIR\THIRD_PARTY.txt"
  Delete "$INSTDIR\THIRD_PARTY_RUST.txt"
  Delete "$INSTDIR\configure-path.ps1"
  Delete "$INSTDIR\IMPLEMENTATION.md"
  Delete "$INSTDIR\acceptance.json"
  Delete "$INSTDIR\Uninstall.exe"
  RMDir "$INSTDIR"
SectionEnd
