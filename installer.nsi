; devkit 用户级安装包：devkit.exe + 自带运行工具（MinGit / Win32-OpenSSH）。
; 编译：makensis installer.nsi（NSIS 3.x，Unicode）
; 输入：dist\devkit-win64\（由 package.ps1 组装）；输出：dist\devkit-win64-setup-<版本>.exe

Unicode true
!include "MUI2.nsh"
!include "FileFunc.nsh"

!define APP_NAME "devkit"
!define APP_VERSION "0.1.0"
!define UNINST_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\devkit"

Name "devkit"
OutFile "dist\devkit-win64-setup-${APP_VERSION}.exe"
InstallDir "$LOCALAPPDATA\Programs\devkit"
InstallDirRegKey HKCU "Software\devkit" "InstallDir"
RequestExecutionLevel user
ShowInstDetails show
ShowUnInstDetails show
SetCompressor /SOLID lzma

!define MUI_ABORTWARNING
!define MUI_FINISHPAGE_RUN "$INSTDIR\devkit.exe"
!define MUI_FINISHPAGE_RUN_TEXT "立即运行 devkit"

!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "SimpChinese"

Section "Install"
  SetOutPath "$INSTDIR"
  File /r "dist\devkit-win64\*.*"

  ; 快捷方式：开始菜单 + 桌面
  CreateDirectory "$SMPROGRAMS\devkit"
  CreateShortcut "$SMPROGRAMS\devkit\devkit.lnk" "$INSTDIR\devkit.exe"
  CreateShortcut "$DESKTOP\devkit.lnk" "$INSTDIR\devkit.exe"

  ; 卸载器 + 用户级卸载注册
  WriteUninstaller "$INSTDIR\uninstall.exe"
  WriteRegStr HKCU "Software\devkit" "InstallDir" "$INSTDIR"
  WriteRegStr HKCU "${UNINST_KEY}" "DisplayName" "devkit"
  WriteRegStr HKCU "${UNINST_KEY}" "DisplayVersion" "${APP_VERSION}"
  WriteRegStr HKCU "${UNINST_KEY}" "Publisher" "LightningAndWind"
  WriteRegStr HKCU "${UNINST_KEY}" "DisplayIcon" "$INSTDIR\devkit.exe"
  WriteRegStr HKCU "${UNINST_KEY}" "UninstallString" "$INSTDIR\uninstall.exe"
  WriteRegStr HKCU "${UNINST_KEY}" "QuietUninstallString" "$INSTDIR\uninstall.exe /S"
  WriteRegDWORD HKCU "${UNINST_KEY}" "NoModify" 1
  WriteRegDWORD HKCU "${UNINST_KEY}" "NoRepair" 1
  ${GetSize} "$INSTDIR" "/S=0K" $0 $1 $2
  IntFmt $0 "0x%08X" $0
  WriteRegDWORD HKCU "${UNINST_KEY}" "EstimatedSize" "$0"
SectionEnd

Section "Uninstall"
  RMDir /r "$INSTDIR"
  Delete "$SMPROGRAMS\devkit\devkit.lnk"
  RMDir "$SMPROGRAMS\devkit"
  Delete "$DESKTOP\devkit.lnk"
  DeleteRegKey HKCU "${UNINST_KEY}"
  DeleteRegKey HKCU "Software\devkit"
SectionEnd
