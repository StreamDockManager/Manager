; Optional default-configuration cleanup for the pinned Tauri NSIS template.
; Destructive work is performed by the Rust helper, not by RMDir /r.
!ifndef SDM_UNINSTALL_INCLUDED
!define SDM_UNINSTALL_INCLUDED
Var ManagerDeleteDataAllowed
Var ManagerApplicationRemovalFailed

!macro NSIS_HOOK_PREUNINSTALL
  Call un.ManagerCanDeleteData
  Pop $ManagerDeleteDataAllowed
  ${If} $ManagerDeleteDataAllowed <> 1
    StrCpy $DeleteAppDataCheckboxState 0
  ${EndIf}
  ${If} $DeleteAppDataCheckboxState = 1
    SetShellVarContext current
    MessageBox MB_YESNO|MB_ICONEXCLAMATION|MB_DEFBUTTON2 "Permanently delete the default Stream Dock Manager data?$\r$\n$\r$\n$APPDATA\streamdockmanager\Stream Dock Manager\config$\r$\n$\r$\nThis removes profiles, settings, installed plugins, managed media, logs, history, recovery backups and private runtimes. The separately installed CLI shares this data.$\r$\n$\r$\nAll files under this path are removed. Custom --config folders, exports and linked media OUTSIDE this path are kept, as are OS credentials and WebView caches.$\r$\n$\r$\nQuit Manager and all CLI commands first. This cannot be undone. Choose No to uninstall the app and keep this data." /SD IDNO IDYES +2
      StrCpy $DeleteAppDataCheckboxState 0
  ${EndIf}
!macroend

!macro NSIS_HOOK_POSTUNINSTALL
  ; Eligibility was established before removing $INSTDIR. Do not resolve an
  ; already-removed installation directory or recreate it after cleanup.
  ${If} $ManagerDeleteDataAllowed = 1
  ${AndIf} $DeleteAppDataCheckboxState = 1
    ${If} $ManagerApplicationRemovalFailed = 1
    ${OrIf} ${FileExists} "$INSTDIR\${MAINBINARYNAME}.exe"
      DetailPrint "Application removal failed; default configuration was kept."
      MessageBox MB_OK|MB_ICONEXCLAMATION "Application removal did not complete, so the default configuration was kept. Quit Manager and retry uninstalling.$\r$\n$\r$\n$APPDATA\streamdockmanager\Stream Dock Manager\config" /SD IDOK
      SetErrorLevel 1
    ${Else}
      ; Extract the exact packaged executable, not a mutable installed copy. Its
      ; private helper mode runs before any Tauri/plugin/configuration startup.
      InitPluginsDir
      SetOutPath "$PLUGINSDIR"
      ClearErrors
      File /oname=manager-data-cleanup.exe "${MAINBINARYSRCPATH}"
      ${If} ${Errors}
        StrCpy $1 "The cleanup helper could not be prepared. Configuration was kept."
        Call un.ManagerCleanupFailed
      ${Else}
        StrCpy $0 -1
        ClearErrors
        ExecWait '"$PLUGINSDIR\manager-data-cleanup.exe" --uninstall-default-data "$PLUGINSDIR\manager-data-cleanup-result.txt"' $0
        ${If} ${Errors}
          StrCpy $0 -1
        ${EndIf}
        ${If} $0 != 0
          StrCpy $1 "The cleanup helper could not finish. Data may remain; close Manager and CLI commands and check the folder."
          ClearErrors
          FileOpen $2 "$PLUGINSDIR\manager-data-cleanup-result.txt" r
          ${IfNot} ${Errors}
            FileReadUTF16LE $2 $1
            FileClose $2
          ${EndIf}
          Call un.ManagerCleanupFailed
        ${Else}
          DetailPrint "Default Manager/CLI configuration removed. Files outside the default folder and OS credentials were kept."
        ${EndIf}
      ${EndIf}
      SetOutPath "$TEMP"
    ${EndIf}
  ${EndIf}
!macroend

!macro SDM_UNINSTALL_FUNCTIONS
Function un.ManagerCanDeleteData
  ${If} ${Silent}
  ${OrIf} $PassiveMode = 1
  ${OrIf} $UpdateMode = 1
    Push 0
    Return
  ${EndIf}
  ; Older Tauri installers use _?= to run the previous uninstaller in place.
  ; NSIS removes that switch from $CMDLINE. Its normal direct uninstall instead
  ; runs a temporary copy. Refuse cleanup in-place, including legacy callers.
  ClearErrors
  GetFullPathName /SHORT $0 "$EXEDIR"
  GetFullPathName /SHORT $1 "$INSTDIR"
  ${If} ${Errors}
  ${OrIf} $0 == ""
  ${OrIf} $1 == ""
  ${OrIf} $0 == $1
    Push 0
  ${Else}
    Push 1
  ${EndIf}
FunctionEnd

Function un.ManagerCleanupFailed
  DetailPrint "$1"
  SetDetailsView show
  MessageBox MB_OK|MB_ICONEXCLAMATION "Uninstall finished, but configuration cleanup did not complete.$\r$\n$\r$\n$1$\r$\n$\r$\nCheck: $APPDATA\streamdockmanager\Stream Dock Manager\config$\r$\n$\r$\nSome files may already have been removed. Files outside the default folder and OS credentials were not removed." /SD IDOK
  SetErrorLevel 1
FunctionEnd
!macroend

!endif
