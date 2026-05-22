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

  Assert-True ($LASTEXITCODE -eq 1) "expected cross-request fallback fixture exit code 1"
  $crossRequestSummary = Convert-JsonOutput $crossRequestOutput "cross-request fallback fixture"
  Assert-Equal $crossRequestSummary.overall "fail" "expected cross-request fallback fixture overall fail"
  Assert-Equal $crossRequestSummary.audit.fallbackCycleCount 0 "cross-request 200 must not count as same-request fallback cycle"
  Assert-Equal $crossRequestSummary.audit.retryLimitErrorFound $true "fallback-selected final 429 must count as retry-limit regression"
  Assert-Equal (($crossRequestSummary.results | Where-Object name -eq "quota_fallback_audit_contract").status) "blocked" "expected cross-request fallback to block quota fallback contract"
  Assert-Equal (($crossRequestSummary.results | Where-Object name -eq "retry_limit_regression_absent").status) "fail" "expected unrecovered fallback 429 to fail retry-limit regression"

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

  $dataRootRequestReuse = Join-Path $tempRoot "data-request-reuse"
  $auditRequestReuse = Join-Path $dataRootRequestReuse "codex_local_access_audit.jsonl"
  Write-AuditLines $auditRequestReuse @(
    [ordered]@{ schemaVersion = 1; timestamp = 1; requestId = "req-reused"; phase = "listener"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "-"; outcome = "accepted" },
    [ordered]@{ schemaVersion = 1; timestamp = 2; requestId = "req-reused"; phase = "upstream_forward"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:healthy"; status = 200; outcome = "response_received" },
    [ordered]@{ schemaVersion = 1; timestamp = 3; requestId = "req-reused"; phase = "lease_granted"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:healthy"; outcome = "active" },
    [ordered]@{ schemaVersion = 1; timestamp = 4; requestId = "req-reused"; phase = "stream_write"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:healthy"; status = 200; streamState = "first_chunk_written"; outcome = "ok" },
    [ordered]@{ schemaVersion = 1; timestamp = 5; requestId = "req-reused"; phase = "stream_completed"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:healthy"; outcome = "completed" },
    [ordered]@{ schemaVersion = 1; timestamp = 6; requestId = "req-reused"; phase = "lease_released"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:healthy"; outcome = "completed" },
    [ordered]@{ schemaVersion = 1; timestamp = 7; requestId = "req-reused"; phase = "listener"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "-"; outcome = "accepted" },
    [ordered]@{ schemaVersion = 1; timestamp = 8; requestId = "req-reused"; phase = "final_response"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "-"; status = 200; errorType = "pool_unavailable"; streamState = "completed"; outcome = "in_band_local_completion"; detail = [ordered]@{ message = "模型 gpt-5.5 的API 服务号池账号额度均已耗尽，请 1 小时后重试" } }
  )
  $requestReuseOutput = & pwsh -NoProfile -ExecutionPolicy Bypass -File $monitorScript `
    -DurationSeconds 0 `
    -DataRoot $dataRootRequestReuse `
    -CodexHome $codexHome `
    -CodexAppProcessNames "__cockpit_no_such_process__" `
    -IncludeExistingAudit `
    -RequireStreamCompletion `
    -RequiredCompletedStreams 1 `
    -Quiet 2>$null

  if ($LASTEXITCODE -ne 0) {
    throw "live monitor local completion pool_unavailable fixture failed with exit_code=$LASTEXITCODE"
  }
  $requestReuseSummary = Convert-JsonOutput $requestReuseOutput "request reuse fixture"
  Assert-Equal $requestReuseSummary.overall "pass" "expected local completion pool_unavailable fixture overall pass"
  Assert-Equal $requestReuseSummary.audit.startedStreamCount 1 "expected one real stream instance for reused request id"
  Assert-Equal $requestReuseSummary.audit.completedStreamCount 1 "expected completed stream to remain completed"
  Assert-Equal $requestReuseSummary.audit.retryLimitErrorFound $false "local pool unavailable must not be reported as retry-limit"
  Assert-Equal $requestReuseSummary.audit.localPoolUnavailableCount 1 "expected local pool unavailable to be tracked separately"
  Assert-Equal $requestReuseSummary.audit.inBandSyntheticPoolUnavailableCount 0 "local pool unavailable must not use legacy in-band synthetic outcome"
  Assert-Equal $requestReuseSummary.audit.responsesLocalCompletionPoolUnavailableCount 1 "expected Codex-facing pool unavailable to be tracked as local completion"
  Assert-Equal $requestReuseSummary.audit.responsesFailedPoolUnavailableCount 0 "local completion must not be counted as response.failed"
  Assert-Equal $requestReuseSummary.audit.responsesTransport503PoolUnavailableCount 0 "Codex-facing pool unavailable must not be transport 503"
  Assert-Equal (($requestReuseSummary.results | Where-Object name -eq "accepted_stream_continuity").status) "pass" "expected reused request stream continuity pass"
  Assert-Equal (($requestReuseSummary.results | Where-Object name -eq "retry_limit_regression_absent").status) "pass" "expected local pool unavailable not to fail retry-limit regression"
  Assert-Equal (($requestReuseSummary.results | Where-Object name -eq "responses_pool_unavailable_transport_503_absent").status) "pass" "expected in-band pool unavailable not to fail transport 503 regression"
  Assert-Equal (($requestReuseSummary.results | Where-Object name -eq "responses_pool_unavailable_local_completion_explicit").status) "pass" "expected local completion pool_unavailable to satisfy terminal guard"
  Assert-Equal (($requestReuseSummary.results | Where-Object name -eq "responses_pool_unavailable_failed_stream_absent").status) "pass" "expected local completion pool_unavailable not to fail response.failed guard"
  Assert-Equal (($requestReuseSummary.results | Where-Object name -eq "responses_pool_unavailable_legacy_synthetic_completion_absent").status) "pass" "expected local completion pool_unavailable not to use legacy synthetic outcome"

  $dataRootResponses503 = Join-Path $tempRoot "data-responses-503"
  $auditResponses503 = Join-Path $dataRootResponses503 "codex_local_access_audit.jsonl"
  Write-AuditLines $auditResponses503 @(
    [ordered]@{ schemaVersion = 1; timestamp = 1; requestId = "req-503"; phase = "listener"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "-"; outcome = "accepted" },
    [ordered]@{ schemaVersion = 1; timestamp = 2; requestId = "req-503"; phase = "final_response"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "-"; status = 503; errorType = "pool_unavailable"; outcome = "error"; detail = [ordered]@{ message = "模型 gpt-5.5 的API 服务号池暂无可调度账号（冷却中 1 个）；请刷新配额、恢复账号或调整号池后重试" } }
  )
  $responses503Output = & pwsh -NoProfile -ExecutionPolicy Bypass -File $monitorScript `
    -DurationSeconds 0 `
    -DataRoot $dataRootResponses503 `
    -CodexHome $codexHome `
    -CodexAppProcessNames "__cockpit_no_such_process__" `
    -IncludeExistingAudit `
    -Quiet 2>$null

  Assert-True ($LASTEXITCODE -eq 1) "expected Codex-facing responses 503 fixture exit code 1"
  $responses503Summary = Convert-JsonOutput $responses503Output "responses 503 fixture"
  Assert-Equal $responses503Summary.overall "fail" "expected Codex-facing responses transport 503 fixture overall fail"
  Assert-Equal $responses503Summary.audit.responsesTransport503PoolUnavailableCount 1 "expected Codex-facing transport 503 to be counted"
  Assert-Equal (($responses503Summary.results | Where-Object name -eq "responses_pool_unavailable_transport_503_absent").status) "fail" "expected Codex-facing transport 503 regression guard to fail"

  $dataRootJsonCompleted = Join-Path $tempRoot "data-json-completed"
  $auditJsonCompleted = Join-Path $dataRootJsonCompleted "codex_local_access_audit.jsonl"
  Write-AuditLines $auditJsonCompleted @(
    [ordered]@{ schemaVersion = 1; timestamp = 1; requestId = "req-json-completed"; phase = "listener"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "-"; outcome = "accepted" },
    [ordered]@{ schemaVersion = 1; timestamp = 2; requestId = "req-json-completed"; phase = "final_response"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "-"; status = 200; errorType = "pool_unavailable"; streamState = "json_completed"; outcome = "in_band_json_local_completion"; detail = [ordered]@{ message = "模型 gpt-5.5 的API 服务号池暂无可调度账号（冷却中 1 个）" } }
  )
  $jsonCompletedOutput = & pwsh -NoProfile -ExecutionPolicy Bypass -File $monitorScript `
    -DurationSeconds 0 `
    -DataRoot $dataRootJsonCompleted `
    -CodexHome $codexHome `
    -CodexAppProcessNames "__cockpit_no_such_process__" `
    -IncludeExistingAudit `
    -Quiet 2>$null

  if ($LASTEXITCODE -ne 0) {
    throw "live monitor non-stream JSON completed fixture failed with exit_code=$LASTEXITCODE"
  }
  $jsonCompletedSummary = Convert-JsonOutput $jsonCompletedOutput "non-stream JSON completed fixture"
  Assert-Equal $jsonCompletedSummary.overall "pass" "expected non-stream JSON completed fixture overall pass"
  Assert-Equal $jsonCompletedSummary.audit.responsesFailedPoolUnavailableCount 0 "non-stream completed JSON must not be counted as streaming response.failed"
  Assert-Equal (($jsonCompletedSummary.results | Where-Object name -eq "responses_pool_unavailable_local_completion_explicit").status) "pass" "expected non-stream JSON completed not to require streaming local completion"

  $dataRootPoolWait = Join-Path $tempRoot "data-pool-wait"
  $auditPoolWait = Join-Path $dataRootPoolWait "codex_local_access_audit.jsonl"
  Write-AuditLines $auditPoolWait @(
    [ordered]@{ schemaVersion = 1; timestamp = 1; requestId = "req-wait"; phase = "listener"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "-"; outcome = "accepted" },
    [ordered]@{ schemaVersion = 1; timestamp = 2; requestId = "req-wait"; phase = "pool_wait"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "-"; status = 503; errorType = "pool_unavailable"; outcome = "sleeping"; detail = [ordered]@{ retry_after_ms = "2000"; message = "模型 gpt-5.5 的API 服务号池账号额度均已耗尽，请 2 秒后重试" } },
    [ordered]@{ schemaVersion = 1; timestamp = 3; requestId = "req-wait"; phase = "pool_wait"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "-"; status = 503; errorType = "pool_unavailable"; outcome = "retrying"; detail = [ordered]@{ slept_ms = "2000" } },
    [ordered]@{ schemaVersion = 1; timestamp = 4; requestId = "req-wait"; phase = "upstream_forward"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:recovered"; status = 200; outcome = "response_received" },
    [ordered]@{ schemaVersion = 1; timestamp = 5; requestId = "req-wait"; phase = "lease_granted"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:recovered"; outcome = "active" },
    [ordered]@{ schemaVersion = 1; timestamp = 6; requestId = "req-wait"; phase = "stream_write"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:recovered"; status = 200; streamState = "first_chunk_written"; outcome = "ok" },
    [ordered]@{ schemaVersion = 1; timestamp = 7; requestId = "req-wait"; phase = "stream_completed"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:recovered"; outcome = "completed" }
  )
  $poolWaitOutput = & pwsh -NoProfile -ExecutionPolicy Bypass -File $monitorScript `
    -DurationSeconds 0 `
    -DataRoot $dataRootPoolWait `
    -CodexHome $codexHome `
    -CodexAppProcessNames "__cockpit_no_such_process__" `
    -IncludeExistingAudit `
    -RequireStreamCompletion `
    -RequiredCompletedStreams 1 `
    -Quiet 2>$null

  if ($LASTEXITCODE -ne 0) {
    throw "live monitor pool-wait fixture failed with exit_code=$LASTEXITCODE"
  }
  $poolWaitSummary = Convert-JsonOutput $poolWaitOutput "pool-wait fixture"
  Assert-Equal $poolWaitSummary.overall "pass" "expected pool-wait fixture overall"
  Assert-Equal $poolWaitSummary.audit.poolWaitCount 2 "expected two pool_wait audit events"
  Assert-Equal $poolWaitSummary.audit.poolWaitSleepingCount 1 "expected one pool_wait sleeping event"
  Assert-Equal $poolWaitSummary.audit.poolWaitRetryingCount 1 "expected one pool_wait retrying event"
  Assert-Equal $poolWaitSummary.audit.heartbeatPoolWaitCount 0 "short recovered pool_wait must not use SSE heartbeat"
  Assert-Equal $poolWaitSummary.audit.openPoolWaitCount 0 "recovered pool_wait must not remain open"
  Assert-Equal $poolWaitSummary.audit.localPoolUnavailableCount 0 "pool_wait must not be counted as final pool_unavailable"
  Assert-Equal $poolWaitSummary.audit.completedStreamCount 1 "expected recovered stream completion after pool_wait"
  Assert-Equal (($poolWaitSummary.results | Where-Object name -eq "sse_idle_pool_wait_regression_absent").status) "pass" "expected closed pool_wait not to fail SSE idle guard"
  Assert-Equal (($poolWaitSummary.results | Where-Object name -eq "pool_wait_reaches_terminal_or_recovery").status) "pass" "expected recovered pool_wait to satisfy terminal progress guard"
  Assert-Equal (($poolWaitSummary.results | Where-Object name -eq "responses_pool_unavailable_legacy_synthetic_completion_absent").status) "pass" "expected recovered pool_wait not to fail legacy synthetic completion guard"

  $dataRootActiveDrain = Join-Path $tempRoot "data-active-drain"
  $auditActiveDrain = Join-Path $dataRootActiveDrain "codex_local_access_audit.jsonl"
  Write-AuditLines $auditActiveDrain @(
    [ordered]@{ schemaVersion = 1; timestamp = 1; requestId = "req-drain"; phase = "listener"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "-"; outcome = "accepted" },
    [ordered]@{ schemaVersion = 1; timestamp = 2; requestId = "req-drain"; phase = "pool_wait"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "-"; status = 200; errorType = "pool_unavailable"; streamState = "admission_blocked"; outcome = "active_streams_draining"; detail = [ordered]@{ active_streams = "1"; message = "模型 gpt-5.5 的API 服务号池账号额度均已耗尽，请 1 小时后重试" } },
    [ordered]@{ schemaVersion = 1; timestamp = 3; requestId = "req-drain"; phase = "pool_wait"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "-"; status = 200; errorType = "pool_unavailable"; streamState = "admission_blocked"; outcome = "active_streams_drained"; detail = [ordered]@{ active_streams = "0" } },
    [ordered]@{ schemaVersion = 1; timestamp = 4; requestId = "req-drain"; phase = "final_response"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "-"; status = 200; errorType = "pool_unavailable"; streamState = "failed"; outcome = "pool_unavailable_after_active_stream_drain"; detail = [ordered]@{ original_status = "503"; message = "模型 gpt-5.5 的API 服务号池账号额度均已耗尽，请 1 小时后重试" } }
  )
  $activeDrainOutput = & pwsh -NoProfile -ExecutionPolicy Bypass -File $monitorScript `
    -DurationSeconds 0 `
    -DataRoot $dataRootActiveDrain `
    -CodexHome $codexHome `
    -CodexAppProcessNames "__cockpit_no_such_process__" `
    -IncludeExistingAudit `
    -Quiet 2>$null

  Assert-True ($LASTEXITCODE -eq 1) "expected response.failed pool_unavailable fixture exit code 1"
  $activeDrainSummary = Convert-JsonOutput $activeDrainOutput "active-drain fixture"
  Assert-Equal $activeDrainSummary.overall "fail" "expected response.failed pool_unavailable fixture overall fail"
  Assert-Equal $activeDrainSummary.audit.activeDrainPoolWaitCount 2 "expected active drain pool_wait events"
  Assert-Equal $activeDrainSummary.audit.openPoolWaitCount 0 "active drain final response must close pool_wait"
  Assert-Equal $activeDrainSummary.audit.responsesFailedPoolUnavailableCount 1 "expected failed in-band terminal pool_unavailable to be counted"
  Assert-Equal (($activeDrainSummary.results | Where-Object name -eq "pool_wait_reaches_terminal_or_recovery").status) "pass" "expected active-drain terminal to satisfy progress guard"
  Assert-Equal (($activeDrainSummary.results | Where-Object name -eq "responses_pool_unavailable_failed_stream_absent").status) "fail" "expected response.failed pool_unavailable to fail fatal stream guard"

  $dataRootOpenPoolWait = Join-Path $tempRoot "data-open-pool-wait"
  $auditOpenPoolWait = Join-Path $dataRootOpenPoolWait "codex_local_access_audit.jsonl"
  Write-AuditLines $auditOpenPoolWait @(
    [ordered]@{ schemaVersion = 1; timestamp = 1; requestId = "req-open"; phase = "listener"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "-"; outcome = "accepted" },
    [ordered]@{ schemaVersion = 1; timestamp = 2; requestId = "req-open"; phase = "pool_wait"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "-"; status = 200; errorType = "pool_unavailable"; streamState = "heartbeat"; outcome = "sleeping"; detail = [ordered]@{ retry_after_ms = "3600000"; message = "模型 gpt-5.5 的API 服务号池账号额度均已耗尽，请 1 小时后重试" } }
  )
  $openPoolWaitOutput = & pwsh -NoProfile -ExecutionPolicy Bypass -File $monitorScript `
    -DurationSeconds 0 `
    -DataRoot $dataRootOpenPoolWait `
    -CodexHome $codexHome `
    -CodexAppProcessNames "__cockpit_no_such_process__" `
    -IncludeExistingAudit `
    -Quiet 2>$null

  Assert-True ($LASTEXITCODE -eq 1) "expected open pool_wait fixture exit code 1"
  $openPoolWaitSummary = Convert-JsonOutput $openPoolWaitOutput "open pool_wait fixture"
  Assert-Equal $openPoolWaitSummary.overall "fail" "expected open heartbeat pool_wait fixture overall fail"
  Assert-Equal $openPoolWaitSummary.audit.openPoolWaitCount 1 "expected open pool_wait to be counted"
  Assert-Equal (($openPoolWaitSummary.results | Where-Object name -eq "sse_idle_pool_wait_regression_absent").status) "fail" "expected heartbeat open pool_wait to fail SSE idle guard"
  Assert-Equal (($openPoolWaitSummary.results | Where-Object name -eq "pool_wait_reaches_terminal_or_recovery").status) "fail" "expected open pool_wait to fail progress guard"
  Assert-Equal (($openPoolWaitSummary.results | Where-Object name -eq "responses_pool_unavailable_local_completion_explicit").status) "fail" "expected open pool_wait without local completion to fail terminal guard"

  $dataRootPoolParked = Join-Path $tempRoot "data-pool-parked"
  $auditPoolParked = Join-Path $dataRootPoolParked "codex_local_access_audit.jsonl"
  Write-AuditLines $auditPoolParked @(
    [ordered]@{ schemaVersion = 1; timestamp = 1; requestId = "req-parked"; phase = "listener"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "-"; outcome = "accepted" },
    [ordered]@{ schemaVersion = 1; timestamp = 2; requestId = "req-parked"; phase = "pool_wait"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "-"; status = 200; streamState = "headers_written"; outcome = "parked"; detail = [ordered]@{ reason = "pool_unavailable_stream_park" } },
    [ordered]@{ schemaVersion = 1; timestamp = 3; requestId = "req-parked"; phase = "pool_wait"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "-"; status = 503; errorType = "pool_unavailable"; outcome = "sleeping"; detail = [ordered]@{ retry_after_ms = "603000000"; message = "模型 gpt-5.5 的API 服务号池暂无可调度账号（冷却中 2 个）；请刷新配额、恢复账号或调整号池后重试" } },
    [ordered]@{ schemaVersion = 1; timestamp = 4; requestId = "req-parked"; phase = "pool_wait"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "-"; status = 503; errorType = "pool_unavailable"; outcome = "retrying"; detail = [ordered]@{ slept_ms = "15000" } }
  )
  $poolParkedOutput = & pwsh -NoProfile -ExecutionPolicy Bypass -File $monitorScript `
    -DurationSeconds 0 `
    -DataRoot $dataRootPoolParked `
    -CodexHome $codexHome `
    -CodexAppProcessNames "__cockpit_no_such_process__" `
    -IncludeExistingAudit `
    -Quiet 2>$null

  Assert-True ($LASTEXITCODE -eq 1) "expected pool-parked fixture exit code 1"
  $poolParkedSummary = Convert-JsonOutput $poolParkedOutput "pool-parked fixture"
  Assert-Equal $poolParkedSummary.overall "fail" "expected pool-parked fixture overall fail"
  Assert-Equal $poolParkedSummary.audit.poolWaitCount 3 "expected three pool_wait parked events"
  Assert-Equal $poolParkedSummary.audit.localPoolUnavailableCount 0 "parked pool_wait must not be counted as final pool_unavailable"
  Assert-Equal $poolParkedSummary.audit.retryLimitErrorFound $false "parked pool_wait must not be retry-limit"
  Assert-Equal $poolParkedSummary.audit.parkedPoolWaitCount 1 "expected parked pool_wait regression to be counted"
  Assert-Equal (($poolParkedSummary.results | Where-Object name -eq "sse_idle_pool_wait_regression_absent").status) "fail" "expected parked pool_wait to fail SSE idle regression guard"

  $dataRootSseIdle = Join-Path $tempRoot "data-sse-idle"
  $auditSseIdle = Join-Path $dataRootSseIdle "codex_local_access_audit.jsonl"
  New-Item -ItemType Directory -Force -Path $dataRootSseIdle | Out-Null
  @(
    '{"schemaVersion":1,"timestamp":1,"requestId":"req-idle","phase":"listener","route":"/v1/responses","model":"gpt-5.5","accountHash":"-","outcome":"accepted"}',
    'stream disconnected before completion: Cockpit API Service pool_unavailable: 模型 gpt-5.5 的API 服务号池暂无可调度账号（冷却中 1 个）；请刷新配额、恢复账号或调整号池后重试。'
  ) | Set-Content -LiteralPath $auditSseIdle -Encoding UTF8

  $sseIdleOutput = & pwsh -NoProfile -ExecutionPolicy Bypass -File $monitorScript `
    -DurationSeconds 0 `
    -DataRoot $dataRootSseIdle `
    -CodexHome $codexHome `
    -CodexAppProcessNames "__cockpit_no_such_process__" `
    -IncludeExistingAudit `
    -Quiet 2>$null

  Assert-True ($LASTEXITCODE -eq 1) "expected SSE idle fixture exit code 1"
  $sseIdleSummary = Convert-JsonOutput $sseIdleOutput "SSE disconnect fixture"
  Assert-Equal $sseIdleSummary.overall "fail" "expected SSE disconnect fixture overall fail"
  Assert-Equal $sseIdleSummary.audit.sseIdleErrorCount 1 "expected stream disconnected pool_unavailable text to be counted"
  Assert-Equal (($sseIdleSummary.results | Where-Object name -eq "sse_idle_pool_wait_regression_absent").status) "fail" "expected stream disconnected pool_unavailable text to fail regression guard"

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
