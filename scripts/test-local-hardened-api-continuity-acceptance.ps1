param(
  [switch]$KeepTemp
)

$ErrorActionPreference = "Stop"

function Assert-True {
  param([bool]$Condition, [string]$Message)
  if (-not $Condition) {
    throw $Message
  }
}

function Assert-Equal {
  param([object]$Actual, [object]$Expected, [string]$Message)
  if ($Actual -ne $Expected) {
    throw "$Message; expected=[$Expected], actual=[$Actual]"
  }
}

$repoRoot = Split-Path -Parent $PSScriptRoot
$acceptScript = Join-Path $PSScriptRoot "accept-local-hardened-api-continuity.ps1"
$tempRoot = Join-Path ([System.IO.Path]::GetTempPath()) ("cockpit-hla-accept-test-{0}-{1}" -f $PID, (Get-Date -Format "yyyyMMddHHmmssfff"))
New-Item -ItemType Directory -Force -Path $tempRoot | Out-Null

try {
  $fakeSmoke = Join-Path $tempRoot "fake-smoke.ps1"
  $argsPath = Join-Path $tempRoot "smoke-args.json"
  $reportPath = Join-Path $tempRoot "fake-report.json"
  @"
param(
  [Parameter(ValueFromRemainingArguments = `$true)]
  [string[]]`$Remaining
)
`$Remaining | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath "$argsPath" -Encoding UTF8
`$drainRequested = `$Remaining -contains "-AutoDrainFirstFreeAccountUntilFallback"
`$report = [ordered]@{
  overall = "pass"
  reportPath = "$reportPath"
  results = @(
    [ordered]@{ name = "quota_fallback_audit_contract"; status = "pass"; evidence = [ordered]@{ has429 = `$true; has200After429 = `$true } },
    [ordered]@{ name = "quota_drain_until_fallback"; status = if (`$drainRequested) { "pass" } else { "skipped" }; evidence = [ordered]@{ requested = `$drainRequested } },
    [ordered]@{ name = "codex_exec_task_e2e"; status = "pass"; evidence = [ordered]@{ taskFileHasMarker = `$true } },
    [ordered]@{ name = "codex_cli_config_auth_untouched"; status = "pass"; evidence = [ordered]@{ unchanged = `$true } },
    [ordered]@{ name = "codex_app_process_stable"; status = "pass"; evidence = [ordered]@{ stable = `$true } }
  )
  autoDrainFirstFreeAccountUntilFallback = `$drainRequested
  temporaryFallbackConfig = [ordered]@{
    accountCount = 3
  }
}
`$report | ConvertTo-Json -Depth 10 | Set-Content -LiteralPath "$reportPath" -Encoding UTF8
`$report | ConvertTo-Json -Depth 10
"@ | Set-Content -LiteralPath $fakeSmoke -Encoding UTF8

  $blockedOutput = & pwsh -NoProfile -ExecutionPolicy Bypass -File $acceptScript `
    -SmokeScriptPath $fakeSmoke `
    -Model "gpt-5.5" `
    -SkipEphemeralGatewayBuild 2>$null

  Assert-True ($LASTEXITCODE -ne 0) "expected wrapper to block live upstream acceptance without acknowledgement"
  $blockedSummary = ($blockedOutput | Out-String) | ConvertFrom-Json
  Assert-Equal $blockedSummary.overall "blocked" "expected blocked summary without acknowledgement"
  Assert-Equal $blockedSummary.reason "live_upstream_risk_ack_required" "expected live upstream risk acknowledgement guard"
  Assert-Equal $blockedSummary.requiredSwitch "-AcknowledgeLiveUpstreamRisk" "expected required acknowledgement switch"
  Assert-True (-not (Test-Path -LiteralPath $argsPath)) "expected blocked wrapper not to invoke smoke script"

  $expandedBlockedOutput = & pwsh -NoProfile -ExecutionPolicy Bypass -File $acceptScript `
    -SmokeScriptPath $fakeSmoke `
    -Model "gpt-5.5" `
    -DrainFirstFreeAccountUntilFallback `
    -DrainMaxRequests 31 `
    -AcknowledgeLiveUpstreamRisk `
    -SkipEphemeralGatewayBuild 2>$null

  Assert-True ($LASTEXITCODE -ne 0) "expected wrapper to block expanded drain attempts without expanded acknowledgement"
  $expandedBlockedSummary = ($expandedBlockedOutput | Out-String) | ConvertFrom-Json
  Assert-Equal $expandedBlockedSummary.overall "blocked" "expected expanded blocked summary"
  Assert-Equal $expandedBlockedSummary.reason "expanded_live_upstream_risk_ack_required" "expected expanded live upstream risk acknowledgement guard"
  Assert-Equal $expandedBlockedSummary.requiredSwitch "-AcknowledgeExpandedLiveUpstreamRisk" "expected expanded acknowledgement switch"
  Assert-True (-not (Test-Path -LiteralPath $argsPath)) "expected expanded blocked wrapper not to invoke smoke script"

  $output = & pwsh -NoProfile -ExecutionPolicy Bypass -File $acceptScript `
    -SmokeScriptPath $fakeSmoke `
    -Model "gpt-5.5" `
    -AcknowledgeLiveUpstreamRisk `
    -SkipEphemeralGatewayBuild 2>$null

  if ($LASTEXITCODE -ne 0) {
    throw "acceptance wrapper failed with exit_code=$LASTEXITCODE"
  }
  $summary = ($output | Out-String) | ConvertFrom-Json
  Assert-Equal $summary.overall "pass" "expected pass summary"
  Assert-Equal $summary.quotaFallback "pass" "expected quota fallback pass"
  Assert-Equal $summary.codexExec "pass" "expected codex exec pass"
  Assert-Equal $summary.cliUntouched "pass" "expected CLI guard pass"
  Assert-Equal $summary.appStable "pass" "expected App guard pass"
  Assert-Equal $summary.liveUpstreamRiskAcknowledged $true "expected live upstream risk acknowledgement summary"
  Assert-Equal $summary.expandedLiveUpstreamRiskAcknowledged $false "expected expanded acknowledgement off by default"
  Assert-Equal $summary.drainRequested $false "expected drain off by default"
  Assert-Equal $summary.drainResult "skipped" "expected drain result skipped by default"
  Assert-Equal $summary.configuredAccountCount 3 "expected configured account count summary"

  $args = Get-Content -LiteralPath $argsPath -Raw | ConvertFrom-Json
  foreach ($requiredArg in @(
      "-Stage",
      "fallback_probe",
      "-StartEphemeralGateway",
      "-TemporaryFallbackConfig",
      "-AppSafeIsolatedProbe",
      "-AcknowledgeLiveUpstreamRisk",
      "-RunUpstreamSmoke",
      "-RunCodexExecSmoke",
      "-RequireQuotaFallback",
      "-AssertCodexCliConfigUntouched",
      "-AssertCodexAppProcessStable",
      "-WriteReport"
    )) {
    Assert-True ([bool](@($args | Where-Object { $_ -eq $requiredArg }).Count)) "expected smoke arg $requiredArg"
  }

  $drainOutput = & pwsh -NoProfile -ExecutionPolicy Bypass -File $acceptScript `
    -SmokeScriptPath $fakeSmoke `
    -Model "gpt-5.5" `
    -AcknowledgeLiveUpstreamRisk `
    -AcknowledgeExpandedLiveUpstreamRisk `
    -DrainFirstFreeAccountUntilFallback `
    -DrainMaxRequests 3 `
    -DrainRequestIntervalSeconds 0 `
    -SkipEphemeralGatewayBuild 2>$null

  if ($LASTEXITCODE -ne 0) {
    throw "drain acceptance wrapper failed with exit_code=$LASTEXITCODE"
  }
  $drainSummary = ($drainOutput | Out-String) | ConvertFrom-Json
  Assert-Equal $drainSummary.drainRequested $true "expected drain summary requested"
  Assert-Equal $drainSummary.drainResult "pass" "expected drain result pass"
  Assert-Equal $drainSummary.expandedLiveUpstreamRiskAcknowledged $true "expected drain expanded acknowledgement summary"
  $drainArgs = Get-Content -LiteralPath $argsPath -Raw | ConvertFrom-Json
  foreach ($requiredDrainArg in @(
      "-AcknowledgeExpandedLiveUpstreamRisk",
      "-AutoDrainFirstFreeAccountUntilFallback",
      "-AutoDrainMaxRequests",
      "3",
      "-AutoDrainRequestIntervalSeconds",
      "0"
    )) {
    Assert-True ([bool](@($drainArgs | Where-Object { $_ -eq $requiredDrainArg }).Count)) "expected drain smoke arg $requiredDrainArg"
  }

  "PASS local hardened API continuity acceptance wrapper tests"
} finally {
  if (-not $KeepTemp -and (Test-Path -LiteralPath $tempRoot)) {
    Remove-Item -LiteralPath $tempRoot -Recurse -Force
  }
}
