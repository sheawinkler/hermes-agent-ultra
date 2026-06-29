!macro NSIS_HOOK_POSTINSTALL
  DetailPrint "Installing Terra Hermes HTTP service..."
  nsExec::ExecToLog 'powershell.exe -ExecutionPolicy Bypass -File "$INSTDIR\installers\windows\install-service.ps1" -BinaryPath "$INSTDIR\hermes-http.exe"'
  Pop $0
  ${If} $0 != 0
    DetailPrint "Warning: hermes-http service install returned $0"
  ${EndIf}
!macroend

!macro NSIS_HOOK_POSTUNINSTALL
  DetailPrint "Removing Terra Hermes HTTP service..."
  nsExec::ExecToLog 'powershell.exe -ExecutionPolicy Bypass -File "$INSTDIR\installers\windows\uninstall-service.ps1"'
  Pop $0
!macroend
