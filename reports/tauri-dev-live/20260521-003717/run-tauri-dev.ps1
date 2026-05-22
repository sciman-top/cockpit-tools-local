$ErrorActionPreference = 'Continue'
Set-Location -LiteralPath 'D:\CODE\external\Cockpit-Tools-Local'
try {
  & npm run tauri dev 1> 'D:\CODE\external\Cockpit-Tools-Local\reports\tauri-dev-live\20260521-003717\stdout.log' 2> 'D:\CODE\external\Cockpit-Tools-Local\reports\tauri-dev-live\20260521-003717\stderr.log'
  $code = $LASTEXITCODE
} catch {
  $code = 999
  $_.Exception.ToString() | Add-Content -LiteralPath 'D:\CODE\external\Cockpit-Tools-Local\reports\tauri-dev-live\20260521-003717\stderr.log'
}
Set-Content -LiteralPath 'D:\CODE\external\Cockpit-Tools-Local\reports\tauri-dev-live\20260521-003717\exit_code.txt' -Value ($code.ToString()) -Encoding UTF8
