; FER-474 (IDX-S1): FerrumIndexer service in the NSIS installer (Tauri `installerHooks`).
;
; The installer is per-machine (elevated), so these hooks run as an administrator.
; Security rules (review M1/I1, 2026-10-08):
; - Stop and delete of the service use Windows' own tools from System32 (net.exe stop, sc.exe
;   delete), never an executable from the install folder: an old copy there could have been
;   replaced if the folder was user-writable.
; - ferrum-indexer.exe is executed (install, purge) only when $INSTDIR is under Program Files
;   and contains no "\..": elsewhere the folder may be writable by users, and whatever sits there
;   would run elevated. Outside Program Files the service is not installed and Ferrum's
;   Settings > Indexer says why.
; - The mode is applied only when no valid config exists yet (`install --mode-if-new`), also for
;   `/INDEXERMODE=high_performance|shared`. Silent/passive installs default to `shared`.
; - A failing service step never aborts the Ferrum install.
; The "Delete the application data" checkbox of the uninstaller also removes the indexer files
; in C:\ProgramData\Ferrum.

; $R9 = 1 when $INSTDIR is under Program Files (64 or 32) and has no "\..", else 0.
!macro FER474_INSTDIR_OK
  Push $R8
  Push $R7
  StrCpy $R9 0
  StrLen $R7 "$PROGRAMFILES64\"
  StrCpy $R8 "$INSTDIR" $R7
  ${If} $R8 == "$PROGRAMFILES64\"
    StrCpy $R9 1
  ${EndIf}
  StrLen $R7 "$PROGRAMFILES\"
  StrCpy $R8 "$INSTDIR" $R7
  ${If} $R8 == "$PROGRAMFILES\"
    StrCpy $R9 1
  ${EndIf}
  ; Reject any "\.." segment.
  StrCpy $R7 0
  ${Do}
    StrCpy $R8 "$INSTDIR" 3 $R7
    ${If} $R8 == ""
      ${Break}
    ${EndIf}
    ${If} $R8 == "\.."
      StrCpy $R9 0
      ${Break}
    ${EndIf}
    IntOp $R7 $R7 + 1
  ${Loop}
  Pop $R7
  Pop $R8
!macroend

; Stop the service with System32's net.exe (waits until stopped; no-op if absent).
!macro FER474_STOP_SERVICE
  DetailPrint "FerrumIndexer: stopping service"
  nsExec::Exec '"$SYSDIR\net.exe" stop FerrumIndexer'
  Pop $0
!macroend

!macro NSIS_HOOK_PREINSTALL
  ; Upgrade/reinstall over a running service: release ferrum-indexer.exe before files are copied.
  !insertmacro FER474_STOP_SERVICE
!macroend

!macro NSIS_HOOK_POSTINSTALL
  Push $0
  Push $1
  Push $2
  Push $R9
  !insertmacro FER474_INSTDIR_OK
  ${If} $R9 <> 1
    DetailPrint "FerrumIndexer: not installed, $INSTDIR is outside Program Files."
  ${Else}
    StrCpy $2 ""
    ; Explicit choice on the command line (validated again by the exe); applied only when no
    ; valid config exists yet, like the dialog.
    ClearErrors
    ${GetOptions} $CMDLINE "/INDEXERMODE=" $1
    ${If} $1 == "high_performance"
      StrCpy $2 "install --mode-if-new high_performance"
    ${ElseIf} $1 == "shared"
      StrCpy $2 "install --mode-if-new shared"
    ${Else}
      ; Ask only for a fresh configuration.
      nsExec::Exec '"$INSTDIR\ferrum-indexer.exe" has-config'
      Pop $0
      ${If} $0 == "0"
        StrCpy $2 "install"
      ${ElseIf} ${Silent}
      ${OrIf} $PassiveMode = 1
        StrCpy $2 "install --mode-if-new shared"
      ${Else}
        MessageBox MB_YESNO|MB_ICONQUESTION "Ferrum Indexer$\r$\n$\r$\nIs this PC used only by you?$\r$\n$\r$\nYes = High performance (recommended for a personal PC): all files are indexed without per-user filtering, including file names in other accounts.$\r$\n$\r$\nNo = Shared: results are filtered by each user's access rights, and other users' profiles are not indexed.$\r$\n$\r$\nYou can change this later in Ferrum > Settings > Indexer." /SD IDNO IDYES fer474_hp
        StrCpy $2 "install --mode-if-new shared"
        Goto fer474_run
        fer474_hp:
        StrCpy $2 "install --mode-if-new high_performance"
      ${EndIf}
    ${EndIf}

    fer474_run:
    DetailPrint "FerrumIndexer: $2"
    nsExec::ExecToLog '"$INSTDIR\ferrum-indexer.exe" $2'
    Pop $0
    ${If} $0 != "0"
      DetailPrint "FerrumIndexer: service setup failed (exit $0). Ferrum works without it."
    ${EndIf}
  ${EndIf}
  Pop $R9
  Pop $2
  Pop $1
  Pop $0
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  ; Stop + delete the service with System32 tools before its exe is deleted.
  !insertmacro FER474_STOP_SERVICE
  DetailPrint "FerrumIndexer: removing service"
  nsExec::Exec '"$SYSDIR\sc.exe" delete FerrumIndexer'
  Pop $0
  ; Data is purged only on a real uninstall with "Delete the application data" checked, and only
  ; by the exe from a Program Files install.
  ${If} $DeleteAppDataCheckboxState = 1
  ${AndIf} $UpdateMode <> 1
    Push $R9
    !insertmacro FER474_INSTDIR_OK
    ${If} $R9 = 1
    ${AndIf} ${FileExists} "$INSTDIR\ferrum-indexer.exe"
      DetailPrint "FerrumIndexer: removing its data"
      nsExec::ExecToLog '"$INSTDIR\ferrum-indexer.exe" uninstall --purge-data'
      Pop $0
    ${EndIf}
    Pop $R9
  ${EndIf}
!macroend
