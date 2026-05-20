Set-Location -LiteralPath 'D:\CODE\external\Cockpit-Tools-Local'
& '.\scripts\accept-local-hardened-api-continuity.ps1' -Model 'gpt-5.5' -AcknowledgeLiveUpstreamRisk
$ec = if ($null -ne $LASTEXITCODE) { $LASTEXITCODE } else { 0 }
Set-Content -LiteralPath 'D:\CODE\external\Cockpit-Tools-Local\reports\local-hardened-api-acceptance\20260520-215349\exit_code.txt' -Value $ec -Encoding ASCII
exit $ec
