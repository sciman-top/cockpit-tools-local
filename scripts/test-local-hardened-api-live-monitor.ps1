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

function New-FakeCodexHome {
  param([string]$Root)
  New-Item -ItemType Directory -Force -Path $Root | Out-Null
  "model = 'direct-oauth'" | Set-Content -LiteralPath (Join-Path $Root "config.toml") -Encoding UTF8
  '{"mode":"direct-oauth"}' | Set-Content -LiteralPath (Join-Path $Root "auth.json") -Encoding UTF8
}

function Write-AuditLines {
  param(
    [string]$Path,
    [object[]]$Events
  )
  $dir = Split-Path -Parent $Path
  New-Item -ItemType Directory -Force -Path $dir | Out-Null
  $Events | ForEach-Object { $_ | ConvertTo-Json -Depth 10 -Compress } | Set-Content -LiteralPath $Path -Encoding UTF8
}

$monitorScript = Join-Path $PSScriptRoot "monitor-live-codex-app-cockpit-acceptance.ps1"
$tempRoot = Join-Path ([System.IO.Path]::GetTempPath()) ("cockpit-hla-live-monitor-test-{0}-{1}" -f $PID, (Get-Date -Format "yyyyMMddHHmmssfff"))
New-Item -ItemType Directory -Force -Path $tempRoot | Out-Null

try {
  $codexHome = Join-Path $tempRoot "codex-home"
  New-FakeCodexHome $codexHome

  $dataRootPass = Join-Path $tempRoot "data-pass"
  $auditPass = Join-Path $dataRootPass "codex_local_access_audit.jsonl"
  Write-AuditLines $auditPass @(
    [ordered]@{ schemaVersion = 1; timestamp = 1; requestId = "-"; phase = "listener"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "-"; outcome = "accepted" },
    [ordered]@{ schemaVersion = 1; timestamp = 2; requestId = "-"; phase = "upstream_forward"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:exhausted"; status = 429; outcome = "response_received" },
    [ordered]@{ schemaVersion = 1; timestamp = 3; requestId = "-"; phase = "classifier"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:exhausted"; status = 429; errorType = "usage_limit_reached"; outcome = "failover"; detail = [ordered]@{ provider_code = "usage_limit_reached" } },
    [ordered]@{ schemaVersion = 1; timestamp = 4; requestId = "-"; phase = "model_cooldown_applied"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:exhausted"; status = 429; errorType = "usage_limit_reached"; outcome = "recorded" },
    [ordered]@{ schemaVersion = 1; timestamp = 5; requestId = "-"; phase = "fallback_selected"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:exhausted"; status = 429; errorType = "usage_limit_reached"; outcome = "next_account" },
    [ordered]@{ schemaVersion = 1; timestamp = 6; requestId = "-"; phase = "upstream_forward"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:healthy"; status = 200; outcome = "response_received" },
    [ordered]@{ schemaVersion = 1; timestamp = 7; requestId = "req-1"; phase = "lease_granted"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:healthy"; outcome = "active" },
    [ordered]@{ schemaVersion = 1; timestamp = 8; requestId = "req-1"; phase = "stream_write"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:healthy"; status = 200; streamState = "first_chunk_written"; outcome = "ok" },
    [ordered]@{ schemaVersion = 1; timestamp = 9; requestId = "req-1"; phase = "stream_completed"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:healthy"; outcome = "completed" }
  )

  $reportDir = Join-Path $tempRoot "reports"
  $passOutput = & pwsh -NoProfile -ExecutionPolicy Bypass -File $monitorScript `
    -DurationSeconds 0 `
    -DataRoot $dataRootPass `
    -CodexHome $codexHome `
    -CodexAppProcessNames "__cockpit_no_such_process__" `
    -IncludeExistingAudit `
    -RequireQuotaFallback `
    -RequireStreamCompletion `
    -RequireCliConfigUntouched `
    -RequireAppStable `
    -WriteReport `
    -ReportDir $reportDir `
    -Quiet 2>$null

  if ($LASTEXITCODE -ne 0) {
    throw "live monitor pass fixture failed with exit_code=$LASTEXITCODE"
  }
  $passSummary = Convert-JsonOutput $passOutput "pass fixture"
  Assert-Equal $passSummary.overall "pass" "expected pass fixture overall"
  Assert-True (Test-Path -LiteralPath $passSummary.reportPath) "expected written report path"
  Assert-Equal (($passSummary.results | Where-Object name -eq "quota_fallback_audit_contract").status) "pass" "expected quota fallback pass"
  Assert-Equal (($passSummary.results | Where-Object name -eq "new_request_avoids_exhausted_account").status) "pass" "expected healthy-account fallback pass"
  Assert-Equal (($passSummary.results | Where-Object name -eq "accepted_stream_continuity").status) "pass" "expected stream continuity pass"
  Assert-Equal (($passSummary.results | Where-Object name -eq "retry_limit_regression_absent").status) "pass" "expected retry-limit absence pass"
  Assert-Equal $passSummary.temporaryConfig.restored "not_applicable" "expected live monitor not to manage temp config"

  $dataRootProcessFilter = Join-Path $tempRoot "data-process-filter"
  New-Item -ItemType Directory -Force -Path $dataRootProcessFilter | Out-Null
  $processFilterOutput = & pwsh -NoProfile -ExecutionPolicy Bypass -File $monitorScript `
    -DurationSeconds 0 `
    -DataRoot $dataRootProcessFilter `
    -CodexHome $codexHome `
    -CodexAppProcessNames "pwsh" `
    -CodexAppPathIncludePatterns "__cockpit_no_app_path__" `
    -RequireAppStable `
    -Quiet 2>$null

  if ($LASTEXITCODE -ne 0) {
    throw "live monitor process filter fixture failed with exit_code=$LASTEXITCODE"
  }
  $processFilterSummary = Convert-JsonOutput $processFilterOutput "process filter fixture"
  Assert-Equal $processFilterSummary.overall "pass" "expected process filter fixture overall"
  Assert-Equal $processFilterSummary.codexAppGuard.before.processes.Count 0 "expected non-App pwsh processes to be excluded before"
  Assert-Equal $processFilterSummary.codexAppGuard.after.processes.Count 0 "expected non-App pwsh processes to be excluded after"
  Assert-Equal (($processFilterSummary.results | Where-Object name -eq "codex_app_process_stable").status) "pass" "expected filtered App process guard pass"

  $dataRootMulti = Join-Path $tempRoot "data-multi-account"
  $auditMulti = Join-Path $dataRootMulti "codex_local_access_audit.jsonl"
  Write-AuditLines $auditMulti @(
    [ordered]@{ schemaVersion = 1; timestamp = 1; requestId = "req-a"; phase = "listener"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "-"; outcome = "accepted" },
    [ordered]@{ schemaVersion = 1; timestamp = 2; requestId = "req-a"; phase = "upstream_forward"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:exhausted-a"; status = 429; outcome = "response_received" },
    [ordered]@{ schemaVersion = 1; timestamp = 3; requestId = "req-a"; phase = "classifier"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:exhausted-a"; status = 429; errorType = "usage_limit_reached"; outcome = "failover"; detail = [ordered]@{ provider_code = "usage_limit_reached" } },
    [ordered]@{ schemaVersion = 1; timestamp = 4; requestId = "req-a"; phase = "model_cooldown_applied"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:exhausted-a"; status = 429; errorType = "usage_limit_reached"; outcome = "recorded" },
    [ordered]@{ schemaVersion = 1; timestamp = 5; requestId = "req-a"; phase = "fallback_selected"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:exhausted-a"; status = 429; errorType = "usage_limit_reached"; outcome = "next_account" },
    [ordered]@{ schemaVersion = 1; timestamp = 6; requestId = "req-a"; phase = "upstream_forward"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:healthy-a"; status = 200; outcome = "response_received" },
    [ordered]@{ schemaVersion = 1; timestamp = 7; requestId = "req-a"; phase = "lease_granted"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:healthy-a"; outcome = "active" },
    [ordered]@{ schemaVersion = 1; timestamp = 8; requestId = "req-a"; phase = "stream_write"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:healthy-a"; status = 200; streamState = "first_chunk_written"; outcome = "ok" },
    [ordered]@{ schemaVersion = 1; timestamp = 9; requestId = "req-a"; phase = "stream_completed"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:healthy-a"; outcome = "completed" },
    [ordered]@{ schemaVersion = 1; timestamp = 10; requestId = "req-b"; phase = "listener"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "-"; outcome = "accepted" },
    [ordered]@{ schemaVersion = 1; timestamp = 11; requestId = "req-b"; phase = "upstream_forward"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:exhausted-b"; status = 429; outcome = "response_received" },
    [ordered]@{ schemaVersion = 1; timestamp = 12; requestId = "req-b"; phase = "classifier"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:exhausted-b"; status = 429; errorType = "usage_limit_reached"; outcome = "failover"; detail = [ordered]@{ provider_code = "usage_limit_reached" } },
    [ordered]@{ schemaVersion = 1; timestamp = 13; requestId = "req-b"; phase = "model_cooldown_applied"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:exhausted-b"; status = 429; errorType = "usage_limit_reached"; outcome = "recorded" },
    [ordered]@{ schemaVersion = 1; timestamp = 14; requestId = "req-b"; phase = "fallback_selected"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:exhausted-b"; status = 429; errorType = "usage_limit_reached"; outcome = "next_account" },
    [ordered]@{ schemaVersion = 1; timestamp = 15; requestId = "req-b"; phase = "upstream_forward"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:healthy-b"; status = 200; outcome = "response_received" },
    [ordered]@{ schemaVersion = 1; timestamp = 16; requestId = "req-b"; phase = "lease_granted"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:healthy-b"; outcome = "active" },
    [ordered]@{ schemaVersion = 1; timestamp = 17; requestId = "req-b"; phase = "stream_write"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:healthy-b"; status = 200; streamState = "first_chunk_written"; outcome = "ok" },
    [ordered]@{ schemaVersion = 1; timestamp = 18; requestId = "req-b"; phase = "stream_completed"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:healthy-b"; outcome = "completed" }
  )
  $multiOutput = & pwsh -NoProfile -ExecutionPolicy Bypass -File $monitorScript `
    -DurationSeconds 0 `
    -DataRoot $dataRootMulti `
    -CodexHome $codexHome `
    -CodexAppProcessNames "__cockpit_no_such_process__" `
    -IncludeExistingAudit `
    -RequireQuotaFallback `
    -RequireStreamCompletion `
    -RequiredFallbackCycles 2 `
    -RequiredDistinctHealthyAccounts 2 `
    -RequiredCompletedStreams 2 `
    -Quiet 2>$null

  if ($LASTEXITCODE -ne 0) {
    throw "live monitor multi-account fixture failed with exit_code=$LASTEXITCODE"
  }
  $multiSummary = Convert-JsonOutput $multiOutput "multi-account fixture"
  Assert-Equal $multiSummary.overall "pass" "expected multi-account fixture overall"
  Assert-Equal $multiSummary.audit.fallbackCycleCount 2 "expected two fallback cycles"
  Assert-Equal $multiSummary.audit.distinctHealthyAccountCountAfterFallback 2 "expected two healthy fallback accounts"
  Assert-Equal $multiSummary.audit.completedStreamCount 2 "expected two completed streams"
  Assert-Equal (($multiSummary.results | Where-Object name -eq "multi_account_fallback_observed").status) "pass" "expected multi-account fallback result pass"

  $dataRootCrossRequest = Join-Path $tempRoot "data-cross-request"
  $auditCrossRequest = Join-Path $dataRootCrossRequest "codex_local_access_audit.jsonl"
  Write-AuditLines $auditCrossRequest @(
    [ordered]@{ schemaVersion = 1; timestamp = 1; requestId = "req-a"; phase = "listener"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "-"; outcome = "accepted" },
    [ordered]@{ schemaVersion = 1; timestamp = 2; requestId = "req-a"; phase = "upstream_forward"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:exhausted-a"; status = 429; outcome = "response_received" },
    [ordered]@{ schemaVersion = 1; timestamp = 3; requestId = "req-a"; phase = "classifier"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:exhausted-a"; status = 429; errorType = "usage_limit_reached"; outcome = "failover"; detail = [ordered]@{ provider_code = "usage_limit_reached" } },
    [ordered]@{ schemaVersion = 1; timestamp = 4; requestId = "req-a"; phase = "model_cooldown_applied"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:exhausted-a"; status = 429; errorType = "usage_limit_reached"; outcome = "recorded" },
    [ordered]@{ schemaVersion = 1; timestamp = 5; requestId = "req-a"; phase = "fallback_selected"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:exhausted-a"; status = 429; errorType = "usage_limit_reached"; outcome = "next_account" },
    [ordered]@{ schemaVersion = 1; timestamp = 6; requestId = "req-a"; phase = "final_response"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:exhausted-a"; status = 429; errorType = "usage_limit_reached"; outcome = "error" },
    [ordered]@{ schemaVersion = 1; timestamp = 7; requestId = "req-b"; phase = "listener"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "-"; outcome = "accepted" },
    [ordered]@{ schemaVersion = 1; timestamp = 8; requestId = "req-b"; phase = "upstream_forward"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:healthy-b"; status = 200; outcome = "response_received" },
    [ordered]@{ schemaVersion = 1; timestamp = 9; requestId = "req-b"; phase = "lease_granted"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:healthy-b"; outcome = "active" },
    [ordered]@{ schemaVersion = 1; timestamp = 10; requestId = "req-b"; phase = "stream_write"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:healthy-b"; status = 200; streamState = "first_chunk_written"; outcome = "ok" },
    [ordered]@{ schemaVersion = 1; timestamp = 11; requestId = "req-b"; phase = "stream_completed"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:healthy-b"; outcome = "completed" }
  )
  $crossRequestOutput = & pwsh -NoProfile -ExecutionPolicy Bypass -File $monitorScript `
    -DurationSeconds 0 `
    -DataRoot $dataRootCrossRequest `
    -CodexHome $codexHome `
    -CodexAppProcessNames "__cockpit_no_such_process__" `
    -IncludeExistingAudit `
    -RequireQuotaFallback `
    -RequireStreamCompletion `
    -Quiet 2>$null

  Assert-True ($LASTEXITCODE -eq 2) "expected cross-request fallback fixture exit code 2"
  $crossRequestSummary = Convert-JsonOutput $crossRequestOutput "cross-request fallback fixture"
  Assert-Equal $crossRequestSummary.overall "blocked" "expected cross-request fallback fixture overall blocked"
  Assert-Equal $crossRequestSummary.audit.fallbackCycleCount 0 "cross-request 200 must not count as same-request fallback cycle"
  Assert-Equal (($crossRequestSummary.results | Where-Object name -eq "quota_fallback_audit_contract").status) "blocked" "expected cross-request fallback to block quota fallback contract"

  $dataRootBlocked = Join-Path $tempRoot "data-blocked"
  $auditBlocked = Join-Path $dataRootBlocked "codex_local_access_audit.jsonl"
  Write-AuditLines $auditBlocked @(
    [ordered]@{ schemaVersion = 1; timestamp = 1; requestId = "req-2"; phase = "listener"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "-"; outcome = "accepted" },
    [ordered]@{ schemaVersion = 1; timestamp = 2; requestId = "req-2"; phase = "stream_completed"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:healthy"; outcome = "completed" }
  )
  $blockedOutput = & pwsh -NoProfile -ExecutionPolicy Bypass -File $monitorScript `
    -DurationSeconds 0 `
    -DataRoot $dataRootBlocked `
    -CodexHome $codexHome `
    -CodexAppProcessNames "__cockpit_no_such_process__" `
    -IncludeExistingAudit `
    -RequireQuotaFallback `
    -Quiet 2>$null

  Assert-True ($LASTEXITCODE -eq 2) "expected blocked fixture exit code 2"
  $blockedSummary = Convert-JsonOutput $blockedOutput "blocked fixture"
  Assert-Equal $blockedSummary.overall "blocked" "expected blocked fixture overall"
  Assert-Equal (($blockedSummary.results | Where-Object name -eq "quota_fallback_audit_contract").status) "blocked" "expected missing fallback blocked"

  $dataRootFail = Join-Path $tempRoot "data-fail"
  $auditFail = Join-Path $dataRootFail "codex_local_access_audit.jsonl"
  New-Item -ItemType Directory -Force -Path $dataRootFail | Out-Null
  @(
    '{"schemaVersion":1,"timestamp":1,"requestId":"req-3","phase":"listener","route":"/v1/responses","model":"gpt-5.5","accountHash":"-","outcome":"accepted"}',
    '{"schemaVersion":1,"timestamp":2,"requestId":"req-3","phase":"final_response","route":"/v1/responses","model":"gpt-5.5","accountHash":"sha256:exhausted","status":429,"errorType":"usage_limit_reached","outcome":"error","detail":{"message":"exceeded retry limit, last status: 429 Too Many Requests"}}'
  ) | Set-Content -LiteralPath $auditFail -Encoding UTF8

  $failOutput = & pwsh -NoProfile -ExecutionPolicy Bypass -File $monitorScript `
    -DurationSeconds 0 `
    -DataRoot $dataRootFail `
    -CodexHome $codexHome `
    -CodexAppProcessNames "__cockpit_no_such_process__" `
    -IncludeExistingAudit `
    -Quiet 2>$null

  Assert-True ($LASTEXITCODE -eq 1) "expected retry-limit fixture exit code 1"
  $failSummary = Convert-JsonOutput $failOutput "retry-limit fixture"
  Assert-Equal $failSummary.overall "fail" "expected retry-limit fixture overall fail"
  Assert-Equal (($failSummary.results | Where-Object name -eq "retry_limit_regression_absent").status) "fail" "expected retry-limit regression fail"

  "PASS local hardened API live monitor tests"
} finally {
  if (-not $KeepTemp -and (Test-Path -LiteralPath $tempRoot)) {
    Remove-Item -LiteralPath $tempRoot -Recurse -Force
  }
}
