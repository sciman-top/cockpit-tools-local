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

function Convert-JsonOutput {
  param([object[]]$Output, [string]$Context)
  $text = ($Output | Out-String).Trim()
  if (-not $text) {
    throw "$Context did not emit JSON"
  }
  $text | ConvertFrom-Json
}

function Get-ResultByName {
  param([object]$Report, [string]$Name)
  $result = @($Report.results | Where-Object { $_.name -eq $Name } | Select-Object -First 1)
  if (-not $result) {
    throw "missing result $Name"
  }
  $result
}

function Write-AuditLines {
  param([string]$Path, [object[]]$Events)
  $parent = Split-Path -Parent $Path
  New-Item -ItemType Directory -Force -Path $parent | Out-Null
  $Events |
    ForEach-Object { $_ | ConvertTo-Json -Depth 10 -Compress } |
    Set-Content -LiteralPath $Path -Encoding UTF8
}

$repoRoot = Split-Path -Parent $PSScriptRoot
$acceptScript = Join-Path $PSScriptRoot "accept-local-hardened-api-continuity.ps1"
$smokeScript = Join-Path $PSScriptRoot "smoke-local-hardened-api.ps1"
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
    [ordered]@{ name = "same_task_affinity_fallback_blocked"; status = "pass"; evidence = [ordered]@{ has429 = `$true; sameTaskAffinityLocalCompletionCount = 1 } },
    [ordered]@{ name = "quota_drain_until_hard_affinity_block"; status = if (`$drainRequested) { "pass" } else { "skipped" }; evidence = [ordered]@{ requested = `$drainRequested } },
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
  Assert-Equal $summary.sameTaskAffinity "pass" "expected same-task affinity pass"
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

  $structuredTerminalRoot = Join-Path $tempRoot "structured-terminal-429-data"
  New-Item -ItemType Directory -Force -Path $structuredTerminalRoot | Out-Null
  [ordered]@{
    enabled = $true
    port = 1
    apiKey = "test-api-key"
    accountIds = @(
      "codex_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
      "codex_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
    )
    safetyConfig = [ordered]@{
      schemaVersion = 1
      hardenedLocalMode = $true
      maxConcurrentRequests = 1
      minRequestIntervalSeconds = 20
      maxRetryAccounts = 2
      fallbackMode = "disabled"
    }
  } | ConvertTo-Json -Depth 10 | Set-Content -LiteralPath (Join-Path $structuredTerminalRoot "codex_local_access.json") -Encoding UTF8
  Write-AuditLines (Join-Path $structuredTerminalRoot "codex_local_access_audit.jsonl") @(
    [ordered]@{ schemaVersion = 1; timestamp = 1; requestId = "req-a"; phase = "listener"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "-"; outcome = "accepted" },
    [ordered]@{ schemaVersion = 1; timestamp = 2; requestId = "req-a"; phase = "upstream_forward"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:exhausted-a"; status = 429; outcome = "response_received" },
    [ordered]@{ schemaVersion = 1; timestamp = 3; requestId = "req-a"; phase = "quota_classification"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:exhausted-a"; status = 429; errorType = "usage_limit_reached"; outcome = "failover"; detail = [ordered]@{ provider_code = "usage_limit_reached" } },
    [ordered]@{ schemaVersion = 1; timestamp = 4; requestId = "req-a"; phase = "model_cooldown_applied"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:exhausted-a"; status = 429; errorType = "usage_limit_reached"; outcome = "recorded" },
    [ordered]@{ schemaVersion = 1; timestamp = 5; requestId = "req-a"; phase = "fallback_blocked"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:exhausted-a"; status = 429; errorType = "usage_limit_reached"; outcome = "hard_affinity" },
    [ordered]@{ schemaVersion = 1; timestamp = 6; requestId = "req-a"; phase = "final_response"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:exhausted-a"; status = 429; errorType = "usage_limit_reached"; outcome = "error" },
    [ordered]@{ schemaVersion = 1; timestamp = 7; requestId = "req-b"; phase = "listener"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "-"; outcome = "accepted" },
    [ordered]@{ schemaVersion = 1; timestamp = 8; requestId = "req-b"; phase = "upstream_forward"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:healthy-b"; status = 200; outcome = "response_received" },
    [ordered]@{ schemaVersion = 1; timestamp = 9; requestId = "req-b"; phase = "stream_completed"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:healthy-b"; outcome = "completed" }
  )
  $structuredTerminalOutput = & pwsh -NoProfile -ExecutionPolicy Bypass -File $smokeScript `
    -Stage fallback_probe `
    -DataRoot $structuredTerminalRoot `
    -BaseUrl "http://127.0.0.1:1/v1" `
    -ApiKey "test-api-key" `
    -RequireQuotaFallback 2>$null

  $structuredTerminalReport = Convert-JsonOutput $structuredTerminalOutput "structured terminal 429 smoke fixture"
  $structuredTerminalSameTask = Get-ResultByName $structuredTerminalReport "same_task_affinity_fallback_blocked"
  Assert-Equal $structuredTerminalSameTask.status "pass" "structured usage_limit_reached terminal 429 should satisfy same-task hard-affinity guard"
  Assert-Equal $structuredTerminalSameTask.evidence.sameTaskAffinityStructuredQuotaTerminal429Count 1 "expected structured terminal 429 count"
  Assert-Equal $structuredTerminalSameTask.evidence.sameTaskAffinityUnstructuredTerminal429Count 0 "expected no unstructured terminal 429"
  Assert-Equal $structuredTerminalReport.continuitySummary.sameTaskAffinityFallbackBlocked.evidence.sameTaskAffinityStructuredQuotaTerminal429Count 1 "expected continuity summary to expose structured terminal 429 count"

  $unstructuredTerminalRoot = Join-Path $tempRoot "unstructured-terminal-429-data"
  New-Item -ItemType Directory -Force -Path $unstructuredTerminalRoot | Out-Null
  Copy-Item -LiteralPath (Join-Path $structuredTerminalRoot "codex_local_access.json") -Destination (Join-Path $unstructuredTerminalRoot "codex_local_access.json") -Force
  Write-AuditLines (Join-Path $unstructuredTerminalRoot "codex_local_access_audit.jsonl") @(
    [ordered]@{ schemaVersion = 1; timestamp = 1; requestId = "req-a"; phase = "quota_classification"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:exhausted-a"; status = 429; errorType = "usage_limit_reached"; outcome = "failover"; detail = [ordered]@{ provider_code = "usage_limit_reached" } },
    [ordered]@{ schemaVersion = 1; timestamp = 2; requestId = "req-a"; phase = "model_cooldown_applied"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:exhausted-a"; status = 429; errorType = "usage_limit_reached"; outcome = "recorded" },
    [ordered]@{ schemaVersion = 1; timestamp = 3; requestId = "req-a"; phase = "fallback_blocked"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:exhausted-a"; status = 429; errorType = "usage_limit_reached"; outcome = "hard_affinity" },
    [ordered]@{ schemaVersion = 1; timestamp = 4; requestId = "req-a"; phase = "final_response"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:exhausted-a"; status = 429; outcome = "error" }
  )
  $unstructuredTerminalOutput = & pwsh -NoProfile -ExecutionPolicy Bypass -File $smokeScript `
    -Stage fallback_probe `
    -DataRoot $unstructuredTerminalRoot `
    -BaseUrl "http://127.0.0.1:1/v1" `
    -ApiKey "test-api-key" `
    -RequireQuotaFallback 2>$null

  $unstructuredTerminalReport = Convert-JsonOutput $unstructuredTerminalOutput "unstructured terminal 429 smoke fixture"
  $unstructuredTerminalSameTask = Get-ResultByName $unstructuredTerminalReport "same_task_affinity_fallback_blocked"
  Assert-Equal $unstructuredTerminalSameTask.status "fail" "unstructured terminal 429 should still fail same-task hard-affinity guard"
  Assert-Equal $unstructuredTerminalSameTask.evidence.sameTaskAffinityUnstructuredTerminal429Count 1 "expected unstructured terminal 429 count"

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

  $singleAccountRoot = Join-Path $tempRoot "single-account-data"
  New-Item -ItemType Directory -Force -Path $singleAccountRoot | Out-Null
  [ordered]@{
    enabled = $true
    port = 1
    apiKey = "test-api-key"
    accountIds = @("codex_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
    safetyConfig = [ordered]@{
      schemaVersion = 1
      hardenedLocalMode = $true
      maxConcurrentRequests = 1
      minRequestIntervalSeconds = 20
      maxRetryAccounts = 2
      fallbackMode = "disabled"
    }
  } | ConvertTo-Json -Depth 10 | Set-Content -LiteralPath (Join-Path $singleAccountRoot "codex_local_access.json") -Encoding UTF8

  $contractOutput = & pwsh -NoProfile -ExecutionPolicy Bypass -File $smokeScript `
    -Stage fallback_probe `
    -DataRoot $singleAccountRoot `
    -BaseUrl "http://127.0.0.1:1/v1" `
    -ApiKey "test-api-key" `
    -RunUpstreamSmoke `
    -AcknowledgeLiveUpstreamRisk 2>$null

  $contractReport = Convert-JsonOutput $contractOutput "single-account fallback_probe contract"
  $contractResult = Get-ResultByName $contractReport "config_fallback_probe_contract"
  Assert-Equal $contractResult.status "pass" "fallback_probe config contract should allow a one-account API service pool"
  Assert-Equal $contractResult.evidence.accountCount 1 "expected one-account fallback_probe evidence"

  $largePoolRoot = Join-Path $tempRoot "large-pool-data"
  New-Item -ItemType Directory -Force -Path $largePoolRoot | Out-Null
  [ordered]@{
    enabled = $true
    port = 1
    apiKey = "test-api-key"
    accountIds = @(
      "codex_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
      "codex_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
      "codex_cccccccccccccccccccccccccccccccc",
      "codex_dddddddddddddddddddddddddddddddd",
      "codex_eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee",
      "codex_ffffffffffffffffffffffffffffffff",
      "codex_11111111111111111111111111111111",
      "codex_22222222222222222222222222222222",
      "codex_33333333333333333333333333333333",
      "codex_44444444444444444444444444444444",
      "codex_55555555555555555555555555555555",
      "codex_66666666666666666666666666666666",
      "codex_77777777777777777777777777777777"
    )
    safetyConfig = [ordered]@{
      schemaVersion = 1
      hardenedLocalMode = $true
      maxConcurrentRequests = 1
      minRequestIntervalSeconds = 20
      maxRetryAccounts = 2
      fallbackMode = "disabled"
    }
  } | ConvertTo-Json -Depth 10 | Set-Content -LiteralPath (Join-Path $largePoolRoot "codex_local_access.json") -Encoding UTF8

  $largePoolContractOutput = & pwsh -NoProfile -ExecutionPolicy Bypass -File $smokeScript `
    -Stage fallback_probe `
    -DataRoot $largePoolRoot `
    -BaseUrl "http://127.0.0.1:1/v1" `
    -ApiKey "test-api-key" `
    -RunUpstreamSmoke `
    -AcknowledgeLiveUpstreamRisk 2>$null

  $largePoolContractReport = Convert-JsonOutput $largePoolContractOutput "large-pool fallback_probe contract"
  $largePoolContractResult = Get-ResultByName $largePoolContractReport "config_fallback_probe_contract"
  Assert-Equal $largePoolContractResult.status "pass" "fallback_probe config contract should allow a fully configured API service pool"
  Assert-Equal $largePoolContractResult.evidence.accountCount 13 "expected large-pool fallback_probe evidence"

  "PASS local hardened API continuity acceptance wrapper tests"
} finally {
  if (-not $KeepTemp -and (Test-Path -LiteralPath $tempRoot)) {
    Remove-Item -LiteralPath $tempRoot -Recurse -Force
  }
}
