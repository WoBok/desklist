Unicode true

!include "MUI2.nsh"
!include "FileFunc.nsh"
!include "LogicLib.nsh"

!define APP      "DeskList"
!define APPNAME  "Desk List"
!define VER      "1.5.13"
!define EXE      "DeskList.exe"
!define REGUN    "Software\Microsoft\Windows\CurrentVersion\Uninstall\DeskList"
!define REGAPP   "Software\DeskList"

Name "${APPNAME} ${VER}"
BrandingText "${APPNAME} ${VER}"
OutFile "..\dist\DeskList-Setup-${VER}.exe"
InstallDir "$LOCALAPPDATA\Programs\${APP}"
InstallDirRegKey HKCU "${REGAPP}" "InstallDir"
RequestExecutionLevel user
SetCompressor /SOLID lzma

VIProductVersion "1.5.13.0"
VIAddVersionKey "ProductName"     "Desk List"
VIAddVersionKey "FileDescription" "Desk List 桌面待办清单"
VIAddVersionKey "FileVersion"     "${VER}"
VIAddVersionKey "ProductVersion"  "${VER}"
VIAddVersionKey "LegalCopyright"  "Desk List"

!define MUI_ICON   "..\src-tauri\icons\icon.ico"
!define MUI_UNICON "..\src-tauri\icons\icon.ico"
!define MUI_ABORTWARNING
!define MUI_FINISHPAGE_RUN "$INSTDIR\${EXE}"
!define MUI_FINISHPAGE_RUN_TEXT "启动 ${APPNAME}"

!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH

!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES

!insertmacro MUI_LANGUAGE "SimpChinese"

Function .onInit
  ; 已经在跑就先关掉，否则文件替换会失败
  nsExec::Exec 'taskkill /F /IM ${EXE}'
  Pop $0
FunctionEnd

Section "install"
  SetShellVarContext current
  SetOutPath "$INSTDIR"
  ; 老版本显示名是 DeskList，桌面/开始菜单会留下同名快捷方式，先清掉
  Delete "$DESKTOP\DeskList.lnk"
  Delete "$SMPROGRAMS\DeskList\DeskList.lnk"
  Delete "$SMPROGRAMS\DeskList\卸载 DeskList.lnk"
  RMDir  "$SMPROGRAMS\DeskList"
  File "..\dist\${EXE}"
  WriteUninstaller "$INSTDIR\Uninstall.exe"

  CreateShortCut "$DESKTOP\${APPNAME}.lnk" "$INSTDIR\${EXE}" "" "$INSTDIR\${EXE}" 0
  CreateDirectory "$SMPROGRAMS\${APPNAME}"
  CreateShortCut "$SMPROGRAMS\${APPNAME}\${APPNAME}.lnk" "$INSTDIR\${EXE}" "" "$INSTDIR\${EXE}" 0
  CreateShortCut "$SMPROGRAMS\${APPNAME}\卸载 ${APPNAME}.lnk" "$INSTDIR\Uninstall.exe"

  WriteRegStr HKCU "${REGAPP}" "InstallDir" "$INSTDIR"

  WriteRegStr   HKCU "${REGUN}" "DisplayName"     "${APPNAME}"
  WriteRegStr   HKCU "${REGUN}" "DisplayVersion"  "${VER}"
  WriteRegStr   HKCU "${REGUN}" "Publisher"       "Desk List"
  WriteRegStr   HKCU "${REGUN}" "DisplayIcon"     "$INSTDIR\${EXE},0"
  WriteRegStr   HKCU "${REGUN}" "UninstallString" '"$INSTDIR\Uninstall.exe"'
  WriteRegStr   HKCU "${REGUN}" "InstallLocation" "$INSTDIR"
  WriteRegDWORD HKCU "${REGUN}" "NoModify"        1
  WriteRegDWORD HKCU "${REGUN}" "NoRepair"        1

  ${GetSize} "$INSTDIR" "/S=0K" $0 $1 $2
  IntFmt $0 "0x%08X" $0
  WriteRegDWORD HKCU "${REGUN}" "EstimatedSize" "$0"
SectionEnd

Section "Uninstall"
  SetShellVarContext current
  nsExec::Exec 'taskkill /F /IM ${EXE}'

  Delete "$DESKTOP\${APPNAME}.lnk"
  Delete "$SMPROGRAMS\${APPNAME}\${APPNAME}.lnk"
  Delete "$SMPROGRAMS\${APPNAME}\卸载 ${APPNAME}.lnk"
  RMDir  "$SMPROGRAMS\${APPNAME}"

  Delete "$INSTDIR\${EXE}"
  Delete "$INSTDIR\Uninstall.exe"
  RMDir  "$INSTDIR"

  ; 开机自启、窗口位置、界面偏好
  DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "DeskList"
  DeleteRegKey HKCU "${REGAPP}"
  DeleteRegKey HKCU "${REGUN}"
  RMDir /r "$APPDATA\DeskList"
  RMDir /r "$LOCALAPPDATA\com.workbuddy.desklist"
SectionEnd
