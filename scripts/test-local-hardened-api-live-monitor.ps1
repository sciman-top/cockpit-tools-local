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
    [ordered]@{ schemaVersion = 1; timestamp = 1; requestId = "turn:sha256:current"; phase = "listener"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "-"; outcome = "accepted"; detail = [ordered]@{ gateway_request_id = "gw-pass-1"; turn_lineage_id = "turn:sha256:current"; turn_lineage_source = "codex_turn_metadata_turn_id"; is_continuation = "false"; is_auto_compact_candidate = "false" } },
    [ordered]@{ schemaVersion = 1; timestamp = 1; requestId = "turn:sha256:current"; phase = "request_trace"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "-"; outcome = "prepared"; detail = [ordered]@{ gateway_request_id = "gw-pass-1"; turn_lineage_id = "turn:sha256:current"; hard_affinity_continuity = "false"; request_timeout_ms = "120000"; normal_request_timeout_ms = "120000"; is_continuation = "false"; is_auto_compact_candidate = "false" } },
    [ordered]@{ schemaVersion = 1; timestamp = 2; requestId = "turn:sha256:current"; phase = "upstream_forward"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:exhausted"; status = 429; outcome = "response_received"; detail = [ordered]@{ gateway_request_id = "gw-pass-1"; turn_lineage_id = "turn:sha256:current" } },
    [ordered]@{ schemaVersion = 1; timestamp = 3; requestId = "turn:sha256:current"; phase = "classifier"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:exhausted"; status = 429; errorType = "usage_limit_reached"; outcome = "failover"; detail = [ordered]@{ gateway_request_id = "gw-pass-1"; turn_lineage_id = "turn:sha256:current"; provider_code = "usage_limit_reached" } },
    [ordered]@{ schemaVersion = 1; timestamp = 3; requestId = "turn:sha256:current"; phase = "quota_classification"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:exhausted"; status = 429; errorType = "usage_limit_reached"; outcome = "classified"; detail = [ordered]@{ gateway_request_id = "gw-pass-1"; turn_lineage_id = "turn:sha256:current"; provider_code = "usage_limit_reached"; reset_hint_present = "true" } },
    [ordered]@{ schemaVersion = 1; timestamp = 4; requestId = "turn:sha256:current"; phase = "model_cooldown_applied"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:exhausted"; status = 429; errorType = "usage_limit_reached"; outcome = "recorded"; detail = [ordered]@{ gateway_request_id = "gw-pass-1"; turn_lineage_id = "turn:sha256:current" } },
    [ordered]@{ schemaVersion = 1; timestamp = 5; requestId = "turn:sha256:current"; phase = "fallback_blocked"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:exhausted"; status = 429; errorType = "usage_limit_reached"; outcome = "hard_affinity"; detail = [ordered]@{ gateway_request_id = "gw-pass-1"; turn_lineage_id = "turn:sha256:current" } },
    [ordered]@{ schemaVersion = 1; timestamp = 6; requestId = "turn:sha256:current"; phase = "stream_completed"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:exhausted"; outcome = "completed"; detail = [ordered]@{ gateway_request_id = "gw-pass-1"; turn_lineage_id = "turn:sha256:current"; upstream_response_id_hash = "response:sha256:pass1"; terminal_origin = "upstream_completed" } },
    [ordered]@{ schemaVersion = 1; timestamp = 7; requestId = "turn:sha256:next"; phase = "listener"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "-"; outcome = "accepted"; detail = [ordered]@{ gateway_request_id = "gw-pass-2"; turn_lineage_id = "turn:sha256:next"; turn_lineage_source = "codex_turn_metadata_turn_id"; is_continuation = "false"; is_auto_compact_candidate = "false" } },
    [ordered]@{ schemaVersion = 1; timestamp = 7; requestId = "turn:sha256:next"; phase = "request_trace"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "-"; outcome = "prepared"; detail = [ordered]@{ gateway_request_id = "gw-pass-2"; turn_lineage_id = "turn:sha256:next"; hard_affinity_continuity = "false"; request_timeout_ms = "120000"; normal_request_timeout_ms = "120000"; is_continuation = "false"; is_auto_compact_candidate = "false" } },
    [ordered]@{ schemaVersion = 1; timestamp = 7; requestId = "turn:sha256:next"; phase = "routing_decision"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:healthy"; status = 200; outcome = "selected"; detail = [ordered]@{ gateway_request_id = "gw-pass-2"; turn_lineage_id = "turn:sha256:next"; selected_reason = "fill_first_selected" } },
    [ordered]@{ schemaVersion = 1; timestamp = 8; requestId = "turn:sha256:next"; phase = "lease_granted"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:healthy"; outcome = "active"; detail = [ordered]@{ gateway_request_id = "gw-pass-2"; turn_lineage_id = "turn:sha256:next" } },
    [ordered]@{ schemaVersion = 1; timestamp = 9; requestId = "turn:sha256:next"; phase = "stream_write"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:healthy"; status = 200; streamState = "first_chunk_written"; outcome = "ok"; detail = [ordered]@{ gateway_request_id = "gw-pass-2"; turn_lineage_id = "turn:sha256:next" } },
    [ordered]@{ schemaVersion = 1; timestamp = 10; requestId = "turn:sha256:next"; phase = "stream_completed"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:healthy"; outcome = "completed"; detail = [ordered]@{ gateway_request_id = "gw-pass-2"; turn_lineage_id = "turn:sha256:next"; upstream_response_id_hash = "response:sha256:pass2"; terminal_origin = "upstream_completed" } }
  )

  $reportDir = Join-Path $tempRoot "reports"
  $passExitCodeFile = Join-Path $tempRoot "pass-exit-code.txt"
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
    -ExitCodeFile $passExitCodeFile `
    -Quiet 2>$null

  if ($LASTEXITCODE -ne 0) {
    throw "live monitor pass fixture failed with exit_code=$LASTEXITCODE"
  }
  Assert-True (Test-Path -LiteralPath $passExitCodeFile) "expected monitor to write exit code file"
  Assert-Equal ((Get-Content -LiteralPath $passExitCodeFile -Raw).Trim()) "0" "expected pass fixture exit code file to contain zero"
  $passSummary = Convert-JsonOutput $passOutput "pass fixture"
  Assert-Equal $passSummary.overall "pass" "expected pass fixture overall"
  Assert-True (Test-Path -LiteralPath $passSummary.reportPath) "expected written report path"
  Assert-True (Test-Path -LiteralPath $passSummary.checkpointPath) "expected live monitor checkpoint path"
  $passCheckpoint = Get-Content -LiteralPath $passSummary.checkpointPath -Raw | ConvertFrom-Json
  Assert-Equal $passCheckpoint.reportStatus "completed" "expected final checkpoint to mark completed status"
  Assert-Equal $passSummary.terminationReason "duration_or_zero" "expected duration-zero fixture termination reason"
  Assert-Equal (($passSummary.results | Where-Object name -eq "same_task_affinity_fallback_blocked").status) "pass" "expected same-task hard-affinity block pass"
  Assert-Equal (($passSummary.results | Where-Object name -eq "new_request_avoids_exhausted_account").status) "pass" "expected healthy-account fallback pass"
  Assert-Equal (($passSummary.results | Where-Object name -eq "accepted_stream_continuity").status) "pass" "expected stream continuity pass"
  Assert-Equal (($passSummary.results | Where-Object name -eq "retry_limit_regression_absent").status) "pass" "expected retry-limit absence pass"
  Assert-Equal (($passSummary.results | Where-Object name -eq "structured_behavior_trace_present").status) "pass" "expected structured behavior trace coverage pass"
  Assert-Equal $passSummary.continuitySummary.sameTaskAffinityFallbackBlocked.status "pass" "expected same-task continuity summary pass"
  Assert-Equal $passSummary.continuitySummary.newRequestAvoidsExhaustedCooldown.status "pass" "expected new-request avoidance summary pass"
  Assert-Equal $passSummary.audit.newRequestAvoidanceCount 1 "expected one new request to avoid the exhausted account"
  Assert-Equal $passSummary.audit.newRequestBlockedReuseCount 0 "expected no new request to reuse the exhausted account"
  Assert-Equal $passSummary.temporaryConfig.restored "not_applicable" "expected live monitor not to manage temp config"
  Assert-Equal (($passSummary.results | Where-Object name -eq "audit_lineage_fields_present").status) "pass" "expected lineage field coverage pass"
  $currentTimeline = $passSummary.audit.requestTimelines | Where-Object requestId -eq "turn:sha256:current"
  Assert-True ($null -ne $currentTimeline) "expected request timeline for exhausted current turn"
  Assert-Equal $currentTimeline.gatewayRequestIds[0] "gw-pass-1" "expected current timeline gateway id"
  Assert-Equal $currentTimeline.fallbackBlockedCount 1 "expected current timeline to expose hard-affinity block"
  Assert-Equal $currentTimeline.terminalUpstreamCompletionCount 1 "expected current timeline terminal completion"
  Assert-Equal $currentTimeline.classification "hard_affinity_completed_on_original_account" "expected current timeline classification"
  $nextTimeline = $passSummary.audit.requestTimelines | Where-Object requestId -eq "turn:sha256:next"
  Assert-True ($null -ne $nextTimeline) "expected request timeline for independent next turn"
  Assert-Equal $nextTimeline.accountHashes[0] "sha256:healthy" "expected independent turn healthy account"
  Assert-Equal $nextTimeline.classification "independent_request_completed" "expected independent timeline classification"

  $dataRootDisabledRuntime = Join-Path $tempRoot "data-disabled-runtime"
  $auditDisabledRuntime = Join-Path $dataRootDisabledRuntime "codex_local_access_audit.jsonl"
  Write-AuditLines $auditDisabledRuntime @(
    [ordered]@{ schemaVersion = 1; timestamp = 1; requestId = "runtime-ok-history"; phase = "listener"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "-"; outcome = "accepted"; detail = [ordered]@{ gateway_request_id = "gw-disabled-runtime" } },
    [ordered]@{ schemaVersion = 1; timestamp = 2; requestId = "runtime-ok-history"; phase = "stream_completed"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:healthy"; outcome = "completed"; detail = [ordered]@{ gateway_request_id = "gw-disabled-runtime"; turn_lineage_id = "turn:sha256:runtime" } }
  )
  [ordered]@{
    enabled = $false
    port = 45336
    apiKey = "agt_test_disabled_runtime"
    safetyConfig = [ordered]@{ requestTimeoutSeconds = 600 }
    updatedAt = 1779906656679
  } | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath (Join-Path $dataRootDisabledRuntime "codex_local_access.json") -Encoding UTF8
  [ordered]@{
    mode = "direct_projection"
    accountKind = "oauth"
    currentAccountId = "codex_direct_test"
    updatedAt = 1779906518392
  } | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath (Join-Path $dataRootDisabledRuntime "codex_runtime_mode.json") -Encoding UTF8

  $disabledRuntimeOutput = & pwsh -NoProfile -ExecutionPolicy Bypass -File $monitorScript `
    -DurationSeconds 0 `
    -DataRoot $dataRootDisabledRuntime `
    -CodexHome $codexHome `
    -CodexAppProcessNames "__cockpit_no_such_process__" `
    -IncludeExistingAudit `
    -RequireApiServiceRuntimeAvailable `
    -Quiet 2>$null

  Assert-True ($LASTEXITCODE -eq 1) "expected disabled runtime fixture exit code 1"
  $disabledRuntimeSummary = Convert-JsonOutput $disabledRuntimeOutput "disabled runtime fixture"
  Assert-Equal $disabledRuntimeSummary.overall "fail" "expected disabled runtime fixture overall fail"
  Assert-Equal (($disabledRuntimeSummary.results | Where-Object name -eq "api_service_runtime_available").status) "fail" "expected disabled API service runtime guard to fail"
  Assert-Equal $disabledRuntimeSummary.apiServiceRuntime.localAccess.enabled $false "expected disabled local access to be recorded"
  Assert-Equal $disabledRuntimeSummary.apiServiceRuntime.runtimeMode.mode "direct_projection" "expected direct projection mode to be recorded"

  $dataRootChatAdapter503 = Join-Path $tempRoot "data-chat-adapter-503"
  $auditChatAdapter503 = Join-Path $dataRootChatAdapter503 "codex_local_access_audit.jsonl"
  Write-AuditLines $auditChatAdapter503 @(
    [ordered]@{ schemaVersion = 1; timestamp = 1; requestId = "turn:sha256:chat"; phase = "listener"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "-"; outcome = "accepted"; detail = [ordered]@{ gateway_request_id = "gw-chat-503"; client_route = "/v1/chat/completions"; response_adapter = "chat_completions"; turn_lineage_id = "turn:sha256:chat" } },
    [ordered]@{ schemaVersion = 1; timestamp = 1; requestId = "turn:sha256:chat"; phase = "request_trace"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "-"; outcome = "prepared"; detail = [ordered]@{ gateway_request_id = "gw-chat-503"; client_route = "/v1/chat/completions"; response_adapter = "chat_completions"; hard_affinity_continuity = "false"; request_timeout_ms = "600000"; normal_request_timeout_ms = "600000"; turn_lineage_id = "turn:sha256:chat" } },
    [ordered]@{ schemaVersion = 1; timestamp = 2; requestId = "turn:sha256:chat"; phase = "final_response"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:chat"; status = 503; errorType = "pool_unavailable"; outcome = "error"; detail = [ordered]@{ gateway_request_id = "gw-chat-503"; message = "API 服务账号均在冷却中"; turn_lineage_id = "turn:sha256:chat" } }
  )
  $chatAdapter503Output = & pwsh -NoProfile -ExecutionPolicy Bypass -File $monitorScript `
    -DurationSeconds 0 `
    -DataRoot $dataRootChatAdapter503 `
    -CodexHome $codexHome `
    -CodexAppProcessNames "__cockpit_no_such_process__" `
    -IncludeExistingAudit `
    -Quiet 2>$null

  Assert-True ($LASTEXITCODE -eq 0 -or $LASTEXITCODE -eq 2) "expected chat adapter 503 fixture to avoid fail exit code"
  $chatAdapter503Summary = Convert-JsonOutput $chatAdapter503Output "chat adapter 503 fixture"
  Assert-Equal (($chatAdapter503Summary.results | Where-Object name -eq "responses_pool_unavailable_transport_503_absent").status) "pass" "chat/completions adapter 503 must not be classified as Codex-facing Responses transport 503"
  Assert-Equal $chatAdapter503Summary.audit.responsesTransport503PoolUnavailableCount 0 "expected chat adapter 503 to be excluded from Responses transport 503 count"

  $dataRootMissingFields = Join-Path $tempRoot "data-missing-fields"
  $auditMissingFields = Join-Path $dataRootMissingFields "codex_local_access_audit.jsonl"
  Write-AuditLines $auditMissingFields @(
    [ordered]@{ schemaVersion = 1; timestamp = 1; requestId = "req-current"; phase = "listener"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "-"; outcome = "accepted" },
    [ordered]@{ schemaVersion = 1; timestamp = 2; requestId = "req-current"; phase = "upstream_forward"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:old"; status = 200; outcome = "response_received" },
    [ordered]@{ schemaVersion = 1; timestamp = 3; requestId = "req-current"; phase = "stream_completed"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:old"; outcome = "completed" }
  )
  $missingFieldsOutput = & pwsh -NoProfile -ExecutionPolicy Bypass -File $monitorScript `
    -DurationSeconds 0 `
    -DataRoot $dataRootMissingFields `
    -CodexHome $codexHome `
    -CodexAppProcessNames "__cockpit_no_such_process__" `
    -IncludeExistingAudit `
    -Quiet 2>$null

  Assert-True ($LASTEXITCODE -eq 0) "expected missing-field fixture exit code 0 because field coverage warning must not dominate overall"
  $missingFieldsSummary = Convert-JsonOutput $missingFieldsOutput "missing-field fixture"
  Assert-Equal (($missingFieldsSummary.results | Where-Object name -eq "audit_lineage_fields_present").status) "warn" "missing lineage fields should warn about limited root-cause diagnostics"
  Assert-Equal $missingFieldsSummary.audit.auditFieldCoverage.gatewayRequestIdCount 0 "expected missing gateway_request_id coverage"

  $dataRootStopSignal = Join-Path $tempRoot "data-stop-signal"
  $auditStopSignal = Join-Path $dataRootStopSignal "codex_local_access_audit.jsonl"
  Write-AuditLines $auditStopSignal @(
    [ordered]@{ schemaVersion = 1; timestamp = 1; requestId = "req-stop"; phase = "listener"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "-"; outcome = "accepted"; detail = [ordered]@{ gateway_request_id = "gw-stop"; turn_lineage_id = "turn:sha256:stop" } },
    [ordered]@{ schemaVersion = 1; timestamp = 2; requestId = "req-stop"; phase = "lease_granted"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:healthy-stop"; outcome = "active"; detail = [ordered]@{ gateway_request_id = "gw-stop"; turn_lineage_id = "turn:sha256:stop"; lease_id = "73" } },
    [ordered]@{ schemaVersion = 1; timestamp = 3; requestId = "req-stop"; phase = "stream_completed"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:healthy-stop"; outcome = "completed"; detail = [ordered]@{ gateway_request_id = "gw-stop"; turn_lineage_id = "turn:sha256:stop"; lease_id = "73" } }
  )
  $stopReportDir = Join-Path $tempRoot "stop-reports"
  $stopSignalFile = Join-Path $tempRoot "stop.signal"
  "stop" | Set-Content -LiteralPath $stopSignalFile -Encoding ASCII
  $stopSignalOutput = & pwsh -NoProfile -ExecutionPolicy Bypass -File $monitorScript `
    -DurationSeconds 30 `
    -DataRoot $dataRootStopSignal `
    -CodexHome $codexHome `
    -CodexAppProcessNames "__cockpit_no_such_process__" `
    -IncludeExistingAudit `
    -WriteReport `
    -ReportDir $stopReportDir `
    -StopSignalFile $stopSignalFile `
    -Quiet 2>$null

  if ($LASTEXITCODE -ne 0) {
    throw "live monitor stop-signal fixture failed with exit_code=$LASTEXITCODE"
  }
  $stopSignalSummary = Convert-JsonOutput $stopSignalOutput "stop-signal fixture"
  Assert-Equal $stopSignalSummary.terminationReason "stop_signal_file" "expected stop signal to finalize report gracefully"
  Assert-True (Test-Path -LiteralPath $stopSignalSummary.reportPath) "expected stop-signal final report"
  Assert-True (Test-Path -LiteralPath $stopSignalSummary.checkpointPath) "expected stop-signal checkpoint"
  $stopTimeline = $stopSignalSummary.audit.requestTimelines | Where-Object requestId -eq "req-stop"
  Assert-Equal $stopTimeline.leaseIds[0] "73" "expected stop-signal timeline lease id"

  $dataRootSameTaskLocalCompletion = Join-Path $tempRoot "data-same-task-local-completion"
  $auditSameTaskLocalCompletion = Join-Path $dataRootSameTaskLocalCompletion "codex_local_access_audit.jsonl"
  Write-AuditLines $auditSameTaskLocalCompletion @(
    [ordered]@{ schemaVersion = 1; timestamp = 1; requestId = "req-current"; phase = "listener"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "-"; outcome = "accepted" },
    [ordered]@{ schemaVersion = 1; timestamp = 2; requestId = "req-current"; phase = "upstream_forward"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:exhausted"; status = 429; outcome = "response_received" },
    [ordered]@{ schemaVersion = 1; timestamp = 3; requestId = "req-current"; phase = "classifier"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:exhausted"; status = 429; errorType = "usage_limit_reached"; outcome = "failover"; detail = [ordered]@{ provider_code = "usage_limit_reached" } },
    [ordered]@{ schemaVersion = 1; timestamp = 4; requestId = "req-current"; phase = "model_cooldown_applied"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:exhausted"; status = 429; errorType = "usage_limit_reached"; outcome = "recorded" },
    [ordered]@{ schemaVersion = 1; timestamp = 5; requestId = "req-current"; phase = "fallback_blocked"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:exhausted"; status = 429; errorType = "usage_limit_reached"; outcome = "hard_affinity" },
    [ordered]@{ schemaVersion = 1; timestamp = 6; requestId = "req-current"; phase = "final_response"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:exhausted"; status = 200; errorType = "pool_unavailable"; streamState = "completed"; outcome = "in_band_local_completion" },
    [ordered]@{ schemaVersion = 1; timestamp = 7; requestId = "req-next"; phase = "listener"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "-"; outcome = "accepted" },
    [ordered]@{ schemaVersion = 1; timestamp = 8; requestId = "req-next"; phase = "lease_granted"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:healthy"; outcome = "active" },
    [ordered]@{ schemaVersion = 1; timestamp = 9; requestId = "req-next"; phase = "stream_write"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:healthy"; status = 200; streamState = "first_chunk_written"; outcome = "ok" },
    [ordered]@{ schemaVersion = 1; timestamp = 10; requestId = "req-next"; phase = "stream_completed"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:healthy"; outcome = "completed" }
  )
  $sameTaskLocalCompletionOutput = & pwsh -NoProfile -ExecutionPolicy Bypass -File $monitorScript `
    -DurationSeconds 0 `
    -DataRoot $dataRootSameTaskLocalCompletion `
    -CodexHome $codexHome `
    -CodexAppProcessNames "__cockpit_no_such_process__" `
    -IncludeExistingAudit `
    -RequireQuotaFallback `
    -RequireStreamCompletion `
    -RequireCliConfigUntouched `
    -RequireAppStable `
    -Quiet 2>$null

  Assert-True ($LASTEXITCODE -eq 1) "expected same-task local completion fixture exit code 1"
  $sameTaskLocalCompletionSummary = Convert-JsonOutput $sameTaskLocalCompletionOutput "same-task local completion fixture"
  Assert-Equal $sameTaskLocalCompletionSummary.overall "fail" "expected same-task local completion fixture overall fail"
  Assert-Equal (($sameTaskLocalCompletionSummary.results | Where-Object name -eq "same_task_affinity_fallback_blocked").status) "fail" "same-task local completion must fail hard-affinity guard"
  Assert-Equal (($sameTaskLocalCompletionSummary.results | Where-Object name -eq "responses_pool_unavailable_local_completion_explicit").status) "fail" "same-task local completion must fail local-completion guard"
  Assert-Equal $sameTaskLocalCompletionSummary.continuitySummary.sameTaskAffinityFallbackBlocked.status "fail" "expected same-task continuity summary fail"

  $dataRootStructuredQuotaFromBlock = Join-Path $tempRoot "data-structured-quota-from-block"
  $auditStructuredQuotaFromBlock = Join-Path $dataRootStructuredQuotaFromBlock "codex_local_access_audit.jsonl"
  Write-AuditLines $auditStructuredQuotaFromBlock @(
    [ordered]@{ schemaVersion = 1; timestamp = 1; requestId = "turn:sha256:observed"; phase = "listener"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "-"; outcome = "accepted"; detail = [ordered]@{ gateway_request_id = "gw-observed-1"; turn_lineage_id = "turn:sha256:observed"; turn_lineage_source = "codex_turn_state" } },
    [ordered]@{ schemaVersion = 1; timestamp = 2; requestId = "turn:sha256:observed"; phase = "request_trace"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "-"; outcome = "prepared"; detail = [ordered]@{ gateway_request_id = "gw-observed-1"; hard_affinity_continuity = "true"; hard_affinity_wait_limit_ms = "3000"; normal_request_timeout_ms = "600000"; request_timeout_ms = "600000"; timeout_extended = "false"; sticky_boundary = "codex_turn_state"; turn_lineage_id = "turn:sha256:observed"; turn_lineage_source = "codex_turn_state" } },
    [ordered]@{ schemaVersion = 1; timestamp = 3; requestId = "turn:sha256:observed"; phase = "upstream_forward"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:old"; status = 429; outcome = "response_received"; detail = [ordered]@{ gateway_request_id = "gw-observed-1"; turn_lineage_id = "turn:sha256:observed" } },
    [ordered]@{ schemaVersion = 1; timestamp = 4; requestId = "turn:sha256:observed"; phase = "classifier"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:old"; status = 429; errorType = "usage_limit_reached"; outcome = "failover"; detail = [ordered]@{ gateway_request_id = "gw-observed-1"; turn_lineage_id = "turn:sha256:observed"; provider_code = "usage_limit_reached"; retry_after_ms = "604293000" } },
    [ordered]@{ schemaVersion = 1; timestamp = 5; requestId = "turn:sha256:observed"; phase = "quota_classification"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:old"; status = 429; errorType = "usage_limit_reached"; outcome = "classified"; detail = [ordered]@{ gateway_request_id = "gw-observed-1"; turn_lineage_id = "turn:sha256:observed"; provider_code = "usage_limit_reached"; reset_hint_present = "true"; retry_after_ms = "604293000" } },
    [ordered]@{ schemaVersion = 1; timestamp = 6; requestId = "turn:sha256:observed"; phase = "model_cooldown_applied"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:old"; status = 429; errorType = "usage_limit_reached"; outcome = "recorded"; detail = [ordered]@{ gateway_request_id = "gw-observed-1"; turn_lineage_id = "turn:sha256:observed"; provider_code = "usage_limit_reached"; retry_after_ms = "604293000" } },
    [ordered]@{ schemaVersion = 1; timestamp = 7; requestId = "turn:sha256:observed"; phase = "fallback_blocked"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:old"; status = 429; errorType = "usage_limit_reached"; outcome = "hard_affinity"; detail = [ordered]@{ gateway_request_id = "gw-observed-1"; turn_lineage_id = "turn:sha256:observed"; provider_code = "usage_limit_reached"; retry_after_ms = "604293000"; hard_affinity_inline_retry_wait_limit_ms = "3000" } },
    [ordered]@{ schemaVersion = 1; timestamp = 8; requestId = "turn:sha256:observed"; phase = "final_response"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:old"; status = 429; errorType = "rate_limited"; outcome = "error"; detail = [ordered]@{ gateway_request_id = "gw-observed-1"; turn_lineage_id = "turn:sha256:observed"; retry_after_ms = "604293000"; message = "上游返回使用额度冷却，请稍后重试" } },
    [ordered]@{ schemaVersion = 1; timestamp = 9; requestId = "turn:sha256:next-observed"; phase = "listener"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "-"; outcome = "accepted"; detail = [ordered]@{ gateway_request_id = "gw-observed-2"; turn_lineage_id = "turn:sha256:next-observed" } },
    [ordered]@{ schemaVersion = 1; timestamp = 10; requestId = "turn:sha256:next-observed"; phase = "lease_granted"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:healthy"; outcome = "active"; detail = [ordered]@{ gateway_request_id = "gw-observed-2"; turn_lineage_id = "turn:sha256:next-observed" } },
    [ordered]@{ schemaVersion = 1; timestamp = 11; requestId = "turn:sha256:next-observed"; phase = "stream_terminal"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:healthy"; status = 200; outcome = "ok"; streamState = "finished"; detail = [ordered]@{ gateway_request_id = "gw-observed-2"; turn_lineage_id = "turn:sha256:next-observed"; response_completed_seen = "true" } }
  )
  $structuredQuotaFromBlockOutput = & pwsh -NoProfile -ExecutionPolicy Bypass -File $monitorScript `
    -DurationSeconds 0 `
    -DataRoot $dataRootStructuredQuotaFromBlock `
    -CodexHome $codexHome `
    -CodexAppProcessNames "__cockpit_no_such_process__" `
    -IncludeExistingAudit `
    -RequireQuotaFallback `
    -RequireStreamCompletion `
    -Quiet 2>$null

  if ($LASTEXITCODE -ne 0) {
    throw "live monitor structured quota terminal from block fixture failed with exit_code=$LASTEXITCODE"
  }
  $structuredQuotaFromBlockSummary = Convert-JsonOutput $structuredQuotaFromBlockOutput "structured quota terminal from block fixture"
  Assert-Equal $structuredQuotaFromBlockSummary.overall "pass" "expected structured quota terminal from block fixture overall"
  Assert-Equal $structuredQuotaFromBlockSummary.audit.sameTaskAffinityStructuredQuotaTerminal429Count 1 "expected hard-affinity quota terminal to be recovered from block classification"
  Assert-Equal $structuredQuotaFromBlockSummary.audit.sameTaskAffinityUnstructuredTerminal429Count 0 "expected no unstructured terminal 429 after block correlation"
  Assert-Equal (($structuredQuotaFromBlockSummary.results | Where-Object name -eq "same_task_affinity_fallback_blocked").status) "pass" "expected same-task quota terminal to pass"

  $dataRootClientAborted = Join-Path $tempRoot "data-client-aborted"
  $auditClientAborted = Join-Path $dataRootClientAborted "codex_local_access_audit.jsonl"
  Write-AuditLines $auditClientAborted @(
    [ordered]@{ schemaVersion = 1; timestamp = 1; requestId = "turn:sha256:abort"; phase = "listener"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "-"; outcome = "accepted"; detail = [ordered]@{ gateway_request_id = "gw-abort-1"; turn_lineage_id = "turn:sha256:abort"; turn_lineage_source = "codex_turn_metadata_turn_id" } },
    [ordered]@{ schemaVersion = 1; timestamp = 2; requestId = "turn:sha256:abort"; phase = "upstream_forward"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:old"; status = 200; outcome = "response_received"; detail = [ordered]@{ gateway_request_id = "gw-abort-1"; turn_lineage_id = "turn:sha256:abort"; upstream_response_id_hash = "response:sha256:abort1" } },
    [ordered]@{ schemaVersion = 1; timestamp = 3; requestId = "turn:sha256:abort"; phase = "lease_granted"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:old"; outcome = "active"; detail = [ordered]@{ gateway_request_id = "gw-abort-1"; turn_lineage_id = "turn:sha256:abort"; lease_id = "41" } },
    [ordered]@{ schemaVersion = 1; timestamp = 4; requestId = "turn:sha256:abort"; phase = "stream_write"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:old"; status = 200; streamState = "first_chunk_written"; outcome = "ok"; detail = [ordered]@{ gateway_request_id = "gw-abort-1"; turn_lineage_id = "turn:sha256:abort" } },
    [ordered]@{ schemaVersion = 1; timestamp = 5; requestId = "turn:sha256:abort"; phase = "client_aborted"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:old"; status = 200; outcome = "client_aborted"; detail = [ordered]@{ gateway_request_id = "gw-abort-1"; turn_lineage_id = "turn:sha256:abort"; terminal_origin = "client_aborted" } },
    [ordered]@{ schemaVersion = 1; timestamp = 6; requestId = "turn:sha256:abort"; phase = "lease_released"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:old"; outcome = "client_aborted"; detail = [ordered]@{ gateway_request_id = "gw-abort-1"; turn_lineage_id = "turn:sha256:abort"; lease_id = "41" } }
  )
  $clientAbortedOutput = & pwsh -NoProfile -ExecutionPolicy Bypass -File $monitorScript `
    -DurationSeconds 0 `
    -DataRoot $dataRootClientAborted `
    -CodexHome $codexHome `
    -CodexAppProcessNames "__cockpit_no_such_process__" `
    -IncludeExistingAudit `
    -Quiet 2>$null

  Assert-True ($LASTEXITCODE -eq 2) "expected client-aborted fixture exit code 2"
  $clientAbortedSummary = Convert-JsonOutput $clientAbortedOutput "client-aborted fixture"
  Assert-Equal $clientAbortedSummary.audit.clientAbortedStreamCount 1 "expected one client_aborted stream"
  Assert-Equal $clientAbortedSummary.audit.clientAbortedAfterFirstChunkCount 1 "expected client_aborted after first chunk classification"
  Assert-Equal (($clientAbortedSummary.results | Where-Object name -eq "client_aborted_streams_classified").status) "blocked" "client_aborted should be classified but not over-attributed"
  Assert-Equal (($clientAbortedSummary.results | Where-Object name -eq "audit_lineage_fields_present").status) "pass" "lineage fields should be considered present"

  $dataRootNoNewRequest = Join-Path $tempRoot "data-no-new-request"
  $auditNoNewRequest = Join-Path $dataRootNoNewRequest "codex_local_access_audit.jsonl"
  Write-AuditLines $auditNoNewRequest @(
    [ordered]@{ schemaVersion = 1; timestamp = 1; requestId = "req-current"; phase = "listener"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "-"; outcome = "accepted" },
    [ordered]@{ schemaVersion = 1; timestamp = 2; requestId = "req-current"; phase = "upstream_forward"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:exhausted"; status = 429; outcome = "response_received" },
    [ordered]@{ schemaVersion = 1; timestamp = 3; requestId = "req-current"; phase = "classifier"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:exhausted"; status = 429; errorType = "usage_limit_reached"; outcome = "failover"; detail = [ordered]@{ provider_code = "usage_limit_reached" } },
    [ordered]@{ schemaVersion = 1; timestamp = 4; requestId = "req-current"; phase = "model_cooldown_applied"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:exhausted"; status = 429; errorType = "usage_limit_reached"; outcome = "recorded" },
    [ordered]@{ schemaVersion = 1; timestamp = 5; requestId = "req-current"; phase = "fallback_blocked"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:exhausted"; status = 429; errorType = "usage_limit_reached"; outcome = "hard_affinity" }
  )
  $noNewRequestOutput = & pwsh -NoProfile -ExecutionPolicy Bypass -File $monitorScript `
    -DurationSeconds 0 `
    -DataRoot $dataRootNoNewRequest `
    -CodexHome $codexHome `
    -CodexAppProcessNames "__cockpit_no_such_process__" `
    -IncludeExistingAudit `
    -RequireQuotaFallback `
    -Quiet 2>$null

  Assert-True ($LASTEXITCODE -eq 2) "expected missing-new-request fixture exit code 2"
  $noNewRequestSummary = Convert-JsonOutput $noNewRequestOutput "missing-new-request fixture"
  Assert-Equal $noNewRequestSummary.overall "blocked" "expected missing-new-request fixture overall blocked"
  Assert-Equal (($noNewRequestSummary.results | Where-Object name -eq "same_task_affinity_fallback_blocked").status) "blocked" "expected unclosed same-task hard-affinity block"
  Assert-Equal (($noNewRequestSummary.results | Where-Object name -eq "new_request_avoids_exhausted_account").status) "blocked" "expected missing new request to block"
  Assert-Equal $noNewRequestSummary.continuitySummary.sameTaskAffinityFallbackBlocked.status "blocked" "expected unclosed same-task continuity summary blocked"
  Assert-Equal $noNewRequestSummary.continuitySummary.newRequestAvoidsExhaustedCooldown.status "blocked" "expected new request summary blocked"

  $dataRootNewRequestReuse = Join-Path $tempRoot "data-new-request-reuse"
  $auditNewRequestReuse = Join-Path $dataRootNewRequestReuse "codex_local_access_audit.jsonl"
  Write-AuditLines $auditNewRequestReuse @(
    [ordered]@{ schemaVersion = 1; timestamp = 1; requestId = "req-current"; phase = "listener"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "-"; outcome = "accepted" },
    [ordered]@{ schemaVersion = 1; timestamp = 2; requestId = "req-current"; phase = "upstream_forward"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:exhausted"; status = 429; outcome = "response_received" },
    [ordered]@{ schemaVersion = 1; timestamp = 3; requestId = "req-current"; phase = "classifier"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:exhausted"; status = 429; errorType = "usage_limit_reached"; outcome = "failover"; detail = [ordered]@{ provider_code = "usage_limit_reached" } },
    [ordered]@{ schemaVersion = 1; timestamp = 4; requestId = "req-current"; phase = "model_cooldown_applied"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:exhausted"; status = 429; errorType = "usage_limit_reached"; outcome = "recorded" },
    [ordered]@{ schemaVersion = 1; timestamp = 5; requestId = "req-current"; phase = "fallback_blocked"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:exhausted"; status = 429; errorType = "usage_limit_reached"; outcome = "hard_affinity" },
    [ordered]@{ schemaVersion = 1; timestamp = 7; requestId = "req-next"; phase = "listener"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "-"; outcome = "accepted" },
    [ordered]@{ schemaVersion = 1; timestamp = 8; requestId = "req-next"; phase = "upstream_forward"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:exhausted"; status = 200; outcome = "response_received" }
  )
  $newRequestReuseOutput = & pwsh -NoProfile -ExecutionPolicy Bypass -File $monitorScript `
    -DurationSeconds 0 `
    -DataRoot $dataRootNewRequestReuse `
    -CodexHome $codexHome `
    -CodexAppProcessNames "__cockpit_no_such_process__" `
    -IncludeExistingAudit `
    -RequireQuotaFallback `
    -Quiet 2>$null

  Assert-True ($LASTEXITCODE -eq 1) "expected new-request blocked-account reuse fixture exit code 1"
  $newRequestReuseSummary = Convert-JsonOutput $newRequestReuseOutput "new-request reuse fixture"
  Assert-Equal $newRequestReuseSummary.overall "fail" "expected new-request reuse fixture overall fail"
  Assert-Equal (($newRequestReuseSummary.results | Where-Object name -eq "new_request_avoids_exhausted_account").status) "fail" "expected new-request reuse to fail"
  Assert-Equal $newRequestReuseSummary.continuitySummary.newRequestAvoidsExhaustedCooldown.status "fail" "expected new-request reuse summary fail"
  Assert-Equal $newRequestReuseSummary.audit.newRequestBlockedReuseCount 1 "expected one blocked-account reuse by a new request"

  $dataRootTransientServerError = Join-Path $tempRoot "data-transient-server-error"
  $auditTransientServerError = Join-Path $dataRootTransientServerError "codex_local_access_audit.jsonl"
  Write-AuditLines $auditTransientServerError @(
    [ordered]@{ schemaVersion = 1; timestamp = 1; requestId = "req-transient"; phase = "listener"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "-"; outcome = "accepted"; detail = [ordered]@{ gateway_request_id = "gw-transient" } },
    [ordered]@{ schemaVersion = 1; timestamp = 2; requestId = "req-transient"; phase = "classifier"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:transient"; status = 500; errorType = "server_error"; outcome = "failover"; detail = [ordered]@{ gateway_request_id = "gw-transient" } },
    [ordered]@{ schemaVersion = 1; timestamp = 3; requestId = "req-transient"; phase = "fallback_selected"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:transient"; status = 500; errorType = "server_error"; outcome = "next_account"; detail = [ordered]@{ gateway_request_id = "gw-transient" } },
    [ordered]@{ schemaVersion = 1; timestamp = 4; requestId = "req-quota"; phase = "classifier"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:exhausted"; status = 429; errorType = "usage_limit_reached"; outcome = "failover"; detail = [ordered]@{ gateway_request_id = "gw-quota"; provider_code = "usage_limit_reached" } },
    [ordered]@{ schemaVersion = 1; timestamp = 5; requestId = "req-quota"; phase = "model_cooldown_applied"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:exhausted"; status = 429; errorType = "usage_limit_reached"; outcome = "recorded"; detail = [ordered]@{ gateway_request_id = "gw-quota" } },
    [ordered]@{ schemaVersion = 1; timestamp = 7; requestId = "req-next"; phase = "listener"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "-"; outcome = "accepted"; detail = [ordered]@{ gateway_request_id = "gw-next" } },
    [ordered]@{ schemaVersion = 1; timestamp = 8; requestId = "req-next"; phase = "routing_decision"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:transient"; status = 200; outcome = "selected"; detail = [ordered]@{ gateway_request_id = "gw-next"; selected_reason = "fill_first_selected" } },
    [ordered]@{ schemaVersion = 1; timestamp = 9; requestId = "req-next"; phase = "stream_completed"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:transient"; outcome = "completed"; detail = [ordered]@{ gateway_request_id = "gw-next"; terminal_origin = "upstream_completed" } }
  )
  $transientServerErrorOutput = & pwsh -NoProfile -ExecutionPolicy Bypass -File $monitorScript `
    -DurationSeconds 0 `
    -DataRoot $dataRootTransientServerError `
    -CodexHome $codexHome `
    -CodexAppProcessNames "__cockpit_no_such_process__" `
    -IncludeExistingAudit `
    -Quiet 2>$null

  if ($LASTEXITCODE -ne 0) {
    throw "transient server-error fixture failed with exit_code=$LASTEXITCODE"
  }
  $transientServerErrorSummary = Convert-JsonOutput $transientServerErrorOutput "transient server-error fixture"
  Assert-True ($transientServerErrorSummary.audit.blockedAccountHashes -contains "sha256:exhausted") "expected usage-limit account to be tracked as blocked"
  Assert-True ($transientServerErrorSummary.audit.blockedAccountHashes -notcontains "sha256:transient") "server_error fallback_selected must not mark a later-healthy account as exhausted"
  Assert-Equal $transientServerErrorSummary.audit.newRequestBlockedReuseCount 0 "expected later healthy reuse of the transient-error account not to be counted as blocked reuse"

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
    [ordered]@{ schemaVersion = 1; timestamp = 1; requestId = "req-a"; phase = "listener"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "-"; outcome = "accepted"; detail = [ordered]@{ gateway_request_id = "gw-multi-a"; turn_lineage_id = "turn:sha256:multi-a" } },
    [ordered]@{ schemaVersion = 1; timestamp = 1; requestId = "req-a"; phase = "request_trace"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "-"; outcome = "prepared"; detail = [ordered]@{ gateway_request_id = "gw-multi-a"; turn_lineage_id = "turn:sha256:multi-a"; hard_affinity_continuity = "false" } },
    [ordered]@{ schemaVersion = 1; timestamp = 2; requestId = "req-a"; phase = "upstream_forward"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:exhausted-a"; status = 429; outcome = "response_received" },
    [ordered]@{ schemaVersion = 1; timestamp = 3; requestId = "req-a"; phase = "classifier"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:exhausted-a"; status = 429; errorType = "usage_limit_reached"; outcome = "failover"; detail = [ordered]@{ provider_code = "usage_limit_reached" } },
    [ordered]@{ schemaVersion = 1; timestamp = 4; requestId = "req-a"; phase = "model_cooldown_applied"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:exhausted-a"; status = 429; errorType = "usage_limit_reached"; outcome = "recorded" },
    [ordered]@{ schemaVersion = 1; timestamp = 5; requestId = "req-a"; phase = "fallback_blocked"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:exhausted-a"; status = 429; errorType = "usage_limit_reached"; outcome = "hard_affinity"; detail = [ordered]@{ gateway_request_id = "gw-multi-a"; turn_lineage_id = "turn:sha256:multi-a" } },
    [ordered]@{ schemaVersion = 1; timestamp = 6; requestId = "req-a"; phase = "stream_completed"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:exhausted-a"; outcome = "completed"; detail = [ordered]@{ gateway_request_id = "gw-multi-a"; turn_lineage_id = "turn:sha256:multi-a"; upstream_response_id_hash = "response:sha256:multi-a" } },
    [ordered]@{ schemaVersion = 1; timestamp = 7; requestId = "req-a-next"; phase = "listener"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "-"; outcome = "accepted"; detail = [ordered]@{ gateway_request_id = "gw-multi-a-next"; turn_lineage_id = "turn:sha256:multi-a-next"; is_continuation = "false"; is_auto_compact_candidate = "false" } },
    [ordered]@{ schemaVersion = 1; timestamp = 8; requestId = "req-a-next"; phase = "lease_granted"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:healthy-a"; outcome = "active" },
    [ordered]@{ schemaVersion = 1; timestamp = 9; requestId = "req-a-next"; phase = "stream_write"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:healthy-a"; status = 200; streamState = "first_chunk_written"; outcome = "ok" },
    [ordered]@{ schemaVersion = 1; timestamp = 10; requestId = "req-a-next"; phase = "stream_completed"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:healthy-a"; outcome = "completed"; detail = [ordered]@{ gateway_request_id = "gw-multi-a-next"; turn_lineage_id = "turn:sha256:multi-a-next"; upstream_response_id_hash = "response:sha256:multi-a-next" } },
    [ordered]@{ schemaVersion = 1; timestamp = 11; requestId = "req-b"; phase = "listener"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "-"; outcome = "accepted"; detail = [ordered]@{ gateway_request_id = "gw-multi-b"; turn_lineage_id = "turn:sha256:multi-b" } },
    [ordered]@{ schemaVersion = 1; timestamp = 12; requestId = "req-b"; phase = "upstream_forward"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:exhausted-b"; status = 429; outcome = "response_received" },
    [ordered]@{ schemaVersion = 1; timestamp = 13; requestId = "req-b"; phase = "classifier"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:exhausted-b"; status = 429; errorType = "usage_limit_reached"; outcome = "failover"; detail = [ordered]@{ provider_code = "usage_limit_reached" } },
    [ordered]@{ schemaVersion = 1; timestamp = 14; requestId = "req-b"; phase = "model_cooldown_applied"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:exhausted-b"; status = 429; errorType = "usage_limit_reached"; outcome = "recorded" },
    [ordered]@{ schemaVersion = 1; timestamp = 15; requestId = "req-b"; phase = "fallback_blocked"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:exhausted-b"; status = 429; errorType = "usage_limit_reached"; outcome = "hard_affinity"; detail = [ordered]@{ gateway_request_id = "gw-multi-b"; turn_lineage_id = "turn:sha256:multi-b" } },
    [ordered]@{ schemaVersion = 1; timestamp = 16; requestId = "req-b"; phase = "stream_completed"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:exhausted-b"; outcome = "completed"; detail = [ordered]@{ gateway_request_id = "gw-multi-b"; turn_lineage_id = "turn:sha256:multi-b"; upstream_response_id_hash = "response:sha256:multi-b" } },
    [ordered]@{ schemaVersion = 1; timestamp = 17; requestId = "req-b-next"; phase = "listener"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "-"; outcome = "accepted"; detail = [ordered]@{ gateway_request_id = "gw-multi-b-next"; turn_lineage_id = "turn:sha256:multi-b-next"; is_continuation = "false"; is_auto_compact_candidate = "false" } },
    [ordered]@{ schemaVersion = 1; timestamp = 18; requestId = "req-b-next"; phase = "lease_granted"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:healthy-b"; outcome = "active" },
    [ordered]@{ schemaVersion = 1; timestamp = 19; requestId = "req-b-next"; phase = "stream_write"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:healthy-b"; status = 200; streamState = "first_chunk_written"; outcome = "ok" },
    [ordered]@{ schemaVersion = 1; timestamp = 20; requestId = "req-b-next"; phase = "stream_completed"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:healthy-b"; outcome = "completed"; detail = [ordered]@{ gateway_request_id = "gw-multi-b-next"; turn_lineage_id = "turn:sha256:multi-b-next"; upstream_response_id_hash = "response:sha256:multi-b-next" } }
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
  Assert-Equal $multiSummary.audit.sameTaskAffinityLocalCompletionCount 0 "same-task local completions must not be counted as success"
  Assert-Equal $multiSummary.audit.distinctHealthyAccountCountAfterBlock 2 "expected two healthy replacement accounts after blocks"
  Assert-Equal $multiSummary.audit.completedStreamCount 2 "expected two completed streams"
  Assert-Equal (($multiSummary.results | Where-Object name -eq "multi_account_fallback_observed").status) "pass" "expected multi-account fallback result pass"

  $dataRootMetadataFallback = Join-Path $tempRoot "data-metadata-fallback"
  $auditMetadataFallback = Join-Path $dataRootMetadataFallback "codex_local_access_audit.jsonl"
  Write-AuditLines $auditMetadataFallback @(
    [ordered]@{ schemaVersion = 1; timestamp = 1; requestId = "x-codex-turn-metadata.turn_id:sha256:meta"; phase = "listener"; route = "/v1/responses"; model = "gpt-5.4-mini"; accountHash = "-"; outcome = "accepted"; detail = [ordered]@{ gateway_request_id = "gw-meta"; request_id_source = "codex_turn_metadata_turn_id"; turn_lineage_id = "x-codex-turn-metadata.turn_id:sha256:meta"; turn_lineage_source = "codex_turn_metadata_turn_id" } },
    [ordered]@{ schemaVersion = 1; timestamp = 2; requestId = "x-codex-turn-metadata.turn_id:sha256:meta"; phase = "auth_projection"; route = "/v1/responses"; model = "gpt-5.4-mini"; accountHash = "sha256:old"; outcome = "prepared"; detail = [ordered]@{ gateway_request_id = "gw-meta"; request_id_source = "codex_turn_metadata_turn_id"; turn_lineage_id = "x-codex-turn-metadata.turn_id:sha256:meta"; turn_lineage_source = "codex_turn_metadata_turn_id" } },
    [ordered]@{ schemaVersion = 1; timestamp = 3; requestId = "x-codex-turn-metadata.turn_id:sha256:meta"; phase = "upstream_forward"; route = "/v1/responses"; model = "gpt-5.4-mini"; accountHash = "sha256:old"; status = 429; outcome = "response_received"; detail = [ordered]@{ gateway_request_id = "gw-meta"; request_id_source = "codex_turn_metadata_turn_id"; turn_lineage_id = "x-codex-turn-metadata.turn_id:sha256:meta"; turn_lineage_source = "codex_turn_metadata_turn_id" } },
    [ordered]@{ schemaVersion = 1; timestamp = 4; requestId = "x-codex-turn-metadata.turn_id:sha256:meta"; phase = "classifier"; route = "/v1/responses"; model = "gpt-5.4-mini"; accountHash = "sha256:old"; status = 429; errorType = "usage_limit_reached"; outcome = "failover"; detail = [ordered]@{ gateway_request_id = "gw-meta"; provider_code = "usage_limit_reached"; request_id_source = "codex_turn_metadata_turn_id"; turn_lineage_id = "x-codex-turn-metadata.turn_id:sha256:meta"; turn_lineage_source = "codex_turn_metadata_turn_id" } },
    [ordered]@{ schemaVersion = 1; timestamp = 5; requestId = "x-codex-turn-metadata.turn_id:sha256:meta"; phase = "model_cooldown_applied"; route = "/v1/responses"; model = "gpt-5.4-mini"; accountHash = "sha256:old"; status = 429; errorType = "usage_limit_reached"; outcome = "recorded"; detail = [ordered]@{ gateway_request_id = "gw-meta"; request_id_source = "codex_turn_metadata_turn_id"; turn_lineage_id = "x-codex-turn-metadata.turn_id:sha256:meta"; turn_lineage_source = "codex_turn_metadata_turn_id" } },
    [ordered]@{ schemaVersion = 1; timestamp = 6; requestId = "x-codex-turn-metadata.turn_id:sha256:meta"; phase = "fallback_selected"; route = "/v1/responses"; model = "gpt-5.4-mini"; accountHash = "sha256:old"; status = 429; errorType = "usage_limit_reached"; outcome = "next_account"; detail = [ordered]@{ gateway_request_id = "gw-meta"; request_id_source = "codex_turn_metadata_turn_id"; turn_lineage_id = "x-codex-turn-metadata.turn_id:sha256:meta"; turn_lineage_source = "codex_turn_metadata_turn_id" } },
    [ordered]@{ schemaVersion = 1; timestamp = 7; requestId = "x-codex-turn-metadata.turn_id:sha256:meta"; phase = "auth_projection"; route = "/v1/responses"; model = "gpt-5.4-mini"; accountHash = "sha256:new"; outcome = "prepared"; detail = [ordered]@{ gateway_request_id = "gw-meta"; request_id_source = "codex_turn_metadata_turn_id"; turn_lineage_id = "x-codex-turn-metadata.turn_id:sha256:meta"; turn_lineage_source = "codex_turn_metadata_turn_id" } },
    [ordered]@{ schemaVersion = 1; timestamp = 8; requestId = "x-codex-turn-metadata.turn_id:sha256:meta"; phase = "upstream_forward"; route = "/v1/responses"; model = "gpt-5.4-mini"; accountHash = "sha256:new"; status = 200; outcome = "response_received"; detail = [ordered]@{ gateway_request_id = "gw-meta"; request_id_source = "codex_turn_metadata_turn_id"; turn_lineage_id = "x-codex-turn-metadata.turn_id:sha256:meta"; turn_lineage_source = "codex_turn_metadata_turn_id" } },
    [ordered]@{ schemaVersion = 1; timestamp = 9; requestId = "x-codex-turn-metadata.turn_id:sha256:meta"; phase = "stream_completed"; route = "/v1/responses"; model = "gpt-5.4-mini"; accountHash = "sha256:new"; outcome = "completed"; detail = [ordered]@{ gateway_request_id = "gw-meta"; request_id_source = "codex_turn_metadata_turn_id"; turn_lineage_id = "x-codex-turn-metadata.turn_id:sha256:meta"; turn_lineage_source = "codex_turn_metadata_turn_id" } }
  )
  $metadataFallbackOutput = & pwsh -NoProfile -ExecutionPolicy Bypass -File $monitorScript `
    -DurationSeconds 0 `
    -DataRoot $dataRootMetadataFallback `
    -CodexHome $codexHome `
    -CodexAppProcessNames "__cockpit_no_such_process__" `
    -IncludeExistingAudit `
    -Quiet 2>$null

  if ($LASTEXITCODE -ne 0) {
    throw "metadata-only fallback fixture failed with exit_code=$LASTEXITCODE"
  }
  $metadataFallbackSummary = Convert-JsonOutput $metadataFallbackOutput "metadata-only fallback fixture"
  Assert-Equal $metadataFallbackSummary.overall "pass" "expected metadata-only fallback fixture overall pass"
  Assert-Equal $metadataFallbackSummary.audit.lineageAccountSwitchCount 1 "expected metadata-only account switch to remain observable"
  Assert-Equal $metadataFallbackSummary.audit.hardAffinityLineageAccountSwitchCount 0 "metadata-only fallback must not count as hard-affinity account switch"
  Assert-Equal $metadataFallbackSummary.audit.metadataOnlyLineageAccountSwitchCount 1 "expected metadata-only account switch classification"
  Assert-Equal (($metadataFallbackSummary.results | Where-Object name -eq "turn_lineage_account_switch_absent").status) "warn" "metadata-only account switch should warn, not fail"
  Assert-Equal $metadataFallbackSummary.continuitySummary.turnLineageAccountSwitchAbsent.status "warn" "metadata-only account switch should be a continuity warning"

  $dataRootLineageSwitch = Join-Path $tempRoot "data-lineage-switch"
  $auditLineageSwitch = Join-Path $dataRootLineageSwitch "codex_local_access_audit.jsonl"
  Write-AuditLines $auditLineageSwitch @(
    [ordered]@{ schemaVersion = 1; timestamp = 1; requestId = "turn:sha256:aaa"; phase = "listener"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "-"; outcome = "accepted"; detail = [ordered]@{ turn_lineage_id = "turn:sha256:aaa"; turn_lineage_source = "codex_turn_metadata_turn_id"; gateway_request_id = "gw-a1" } },
    [ordered]@{ schemaVersion = 1; timestamp = 2; requestId = "turn:sha256:aaa"; phase = "upstream_forward"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:old"; status = 200; outcome = "response_received"; detail = [ordered]@{ turn_lineage_id = "turn:sha256:aaa"; gateway_request_id = "gw-a1"; admission_attempt = "1" } },
    [ordered]@{ schemaVersion = 1; timestamp = 3; requestId = "turn:sha256:aaa"; phase = "lease_granted"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:old"; outcome = "active"; detail = [ordered]@{ turn_lineage_id = "turn:sha256:aaa"; gateway_request_id = "gw-a1"; lease_id = "11" } },
    [ordered]@{ schemaVersion = 1; timestamp = 4; requestId = "turn:sha256:aaa"; phase = "stream_completed"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:old"; outcome = "completed"; detail = [ordered]@{ turn_lineage_id = "turn:sha256:aaa"; gateway_request_id = "gw-a1"; upstream_response_id_hash = "response:sha256:r1"; terminal_origin = "upstream_completed"; lease_id = "11" } },
    [ordered]@{ schemaVersion = 1; timestamp = 5; requestId = "turn:sha256:aaa"; phase = "listener"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "-"; outcome = "accepted"; detail = [ordered]@{ turn_lineage_id = "turn:sha256:aaa"; turn_lineage_source = "codex_turn_metadata_turn_id"; gateway_request_id = "gw-a2"; previous_response_id_hash = "response:sha256:r1"; is_continuation = "true"; is_auto_compact_candidate = "true" } },
    [ordered]@{ schemaVersion = 1; timestamp = 6; requestId = "turn:sha256:aaa"; phase = "upstream_forward"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:new"; status = 200; outcome = "response_received"; detail = [ordered]@{ turn_lineage_id = "turn:sha256:aaa"; gateway_request_id = "gw-a2"; previous_response_id_hash = "response:sha256:r1"; is_continuation = "true"; is_auto_compact_candidate = "true"; admission_attempt = "1" } },
    [ordered]@{ schemaVersion = 1; timestamp = 7; requestId = "turn:sha256:aaa"; phase = "stream_completed"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:new"; outcome = "completed"; detail = [ordered]@{ turn_lineage_id = "turn:sha256:aaa"; gateway_request_id = "gw-a2"; terminal_origin = "upstream_completed" } }
  )
  $lineageSwitchOutput = & pwsh -NoProfile -ExecutionPolicy Bypass -File $monitorScript `
    -DurationSeconds 0 `
    -DataRoot $dataRootLineageSwitch `
    -CodexHome $codexHome `
    -CodexAppProcessNames "__cockpit_no_such_process__" `
    -IncludeExistingAudit `
    -Quiet 2>$null

  Assert-True ($LASTEXITCODE -eq 1) "expected same-turn lineage switch fixture exit code 1"
  $lineageSwitchSummary = Convert-JsonOutput $lineageSwitchOutput "lineage switch fixture"
  Assert-Equal $lineageSwitchSummary.overall "fail" "expected same-turn lineage switch fixture overall fail"
  Assert-Equal $lineageSwitchSummary.audit.lineageAccountSwitchCount 1 "expected one same-turn lineage account switch"
  Assert-Equal $lineageSwitchSummary.audit.continuationReroutedCount 1 "expected continuation reroute to be counted"
  Assert-Equal $lineageSwitchSummary.audit.autoCompactReroutedCount 1 "expected auto-compact candidate reroute to be counted"
  Assert-Equal (($lineageSwitchSummary.results | Where-Object name -eq "turn_lineage_account_switch_absent").status) "fail" "expected lineage switch guard to fail"
  Assert-Equal $lineageSwitchSummary.continuitySummary.turnLineageAccountSwitchAbsent.status "fail" "expected continuity summary to expose lineage switch"

  $dataRootCrossRequest = Join-Path $tempRoot "data-cross-request"
  $auditCrossRequest = Join-Path $dataRootCrossRequest "codex_local_access_audit.jsonl"
  Write-AuditLines $auditCrossRequest @(
    [ordered]@{ schemaVersion = 1; timestamp = 1; requestId = "req-a"; phase = "listener"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "-"; outcome = "accepted" },
    [ordered]@{ schemaVersion = 1; timestamp = 2; requestId = "req-a"; phase = "upstream_forward"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:exhausted-a"; status = 429; outcome = "response_received" },
    [ordered]@{ schemaVersion = 1; timestamp = 3; requestId = "req-a"; phase = "quota_classification"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:exhausted-a"; status = 429; errorType = "usage_limit_reached"; outcome = "failover"; detail = [ordered]@{ provider_code = "usage_limit_reached" } },
    [ordered]@{ schemaVersion = 1; timestamp = 4; requestId = "req-a"; phase = "model_cooldown_applied"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:exhausted-a"; status = 429; errorType = "usage_limit_reached"; outcome = "recorded" },
    [ordered]@{ schemaVersion = 1; timestamp = 5; requestId = "req-a"; phase = "fallback_blocked"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:exhausted-a"; status = 429; errorType = "usage_limit_reached"; outcome = "hard_affinity" },
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

  if ($LASTEXITCODE -ne 0) {
    throw "structured sticky terminal 429 fixture failed with exit_code=$LASTEXITCODE"
  }
  $crossRequestSummary = Convert-JsonOutput $crossRequestOutput "cross-request fallback fixture"
  Assert-Equal $crossRequestSummary.overall "pass" "expected protocol-preserving sticky terminal fixture overall pass"
  Assert-Equal $crossRequestSummary.audit.sameTaskAffinityLocalCompletionCount 0 "cross-request 200 must not count as same-task local completion"
  Assert-Equal $crossRequestSummary.audit.retryLimitErrorFound $false "structured hard-affinity final 429 must not count as retry-limit regression"
  Assert-Equal (($crossRequestSummary.results | Where-Object name -eq "same_task_affinity_fallback_blocked").status) "pass" "expected structured same-task hard-affinity 429 to preserve sticky contract"
  Assert-Equal (($crossRequestSummary.results | Where-Object name -eq "retry_limit_regression_absent").status) "pass" "expected structured sticky quota terminal not to fail retry-limit guard"

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
  Assert-Equal (($blockedSummary.results | Where-Object name -eq "same_task_affinity_fallback_blocked").status) "blocked" "expected missing same-task hard-affinity block"

  $dataRootRequestReuse = Join-Path $tempRoot "data-request-reuse"
  $auditRequestReuse = Join-Path $dataRootRequestReuse "codex_local_access_audit.jsonl"
  Write-AuditLines $auditRequestReuse @(
    [ordered]@{ schemaVersion = 1; timestamp = 1; requestId = "req-reused"; phase = "listener"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "-"; outcome = "accepted"; detail = [ordered]@{ gateway_request_id = "gw-reused-1"; turn_lineage_id = "turn:sha256:reused-1" } },
    [ordered]@{ schemaVersion = 1; timestamp = 1; requestId = "req-reused"; phase = "request_trace"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "-"; outcome = "prepared"; detail = [ordered]@{ gateway_request_id = "gw-reused-1"; turn_lineage_id = "turn:sha256:reused-1"; hard_affinity_continuity = "false" } },
    [ordered]@{ schemaVersion = 1; timestamp = 2; requestId = "req-reused"; phase = "upstream_forward"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:healthy"; status = 200; outcome = "response_received" },
    [ordered]@{ schemaVersion = 1; timestamp = 3; requestId = "req-reused"; phase = "lease_granted"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:healthy"; outcome = "active" },
    [ordered]@{ schemaVersion = 1; timestamp = 4; requestId = "req-reused"; phase = "stream_write"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:healthy"; status = 200; streamState = "first_chunk_written"; outcome = "ok" },
    [ordered]@{ schemaVersion = 1; timestamp = 5; requestId = "req-reused"; phase = "stream_completed"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:healthy"; outcome = "completed" },
    [ordered]@{ schemaVersion = 1; timestamp = 6; requestId = "req-reused"; phase = "lease_released"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:healthy"; outcome = "completed" },
    [ordered]@{ schemaVersion = 1; timestamp = 7; requestId = "req-reused"; phase = "listener"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "-"; outcome = "accepted"; detail = [ordered]@{ gateway_request_id = "gw-reused-2"; turn_lineage_id = "turn:sha256:reused-2" } },
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

  $dataRootAuditWindow = Join-Path $tempRoot "data-audit-window"
  $auditAuditWindow = Join-Path $dataRootAuditWindow "codex_local_access_audit.jsonl"
  Write-AuditLines $auditAuditWindow @(
    [ordered]@{ schemaVersion = 1; timestamp = 1; requestId = "old-503"; phase = "listener"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "-"; outcome = "accepted"; detail = [ordered]@{ gateway_request_id = "gw-window-old" } },
    [ordered]@{ schemaVersion = 1; timestamp = 2; requestId = "old-503"; phase = "final_response"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "-"; status = 503; errorType = "pool_unavailable"; outcome = "error"; detail = [ordered]@{ gateway_request_id = "gw-window-old"; message = "模型 gpt-5.5 的API 服务号池暂无可调度账号（冷却中 1 个）" } },
    [ordered]@{ schemaVersion = 1; timestamp = 100; requestId = "good-window"; phase = "listener"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "-"; outcome = "accepted"; detail = [ordered]@{ gateway_request_id = "gw-window-good"; turn_lineage_id = "turn:sha256:window" } },
    [ordered]@{ schemaVersion = 1; timestamp = 101; requestId = "good-window"; phase = "lease_granted"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:healthy"; outcome = "active"; detail = [ordered]@{ gateway_request_id = "gw-window-good"; turn_lineage_id = "turn:sha256:window"; lease_id = "91" } },
    [ordered]@{ schemaVersion = 1; timestamp = 102; requestId = "good-window"; phase = "stream_write"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:healthy"; status = 200; streamState = "first_chunk_written"; outcome = "ok"; detail = [ordered]@{ gateway_request_id = "gw-window-good"; turn_lineage_id = "turn:sha256:window" } },
    [ordered]@{ schemaVersion = 1; timestamp = 103; requestId = "good-window"; phase = "stream_terminal"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:healthy"; status = 200; streamState = "finished"; outcome = "ok"; detail = [ordered]@{ gateway_request_id = "gw-window-good"; turn_lineage_id = "turn:sha256:window"; response_completed_seen = "true" } }
  )
  $auditWindowOutput = & pwsh -NoProfile -ExecutionPolicy Bypass -File $monitorScript `
    -DurationSeconds 0 `
    -DataRoot $dataRootAuditWindow `
    -CodexHome $codexHome `
    -CodexAppProcessNames "__cockpit_no_such_process__" `
    -IncludeExistingAudit `
    -AuditSinceTimestampMs 100 `
    -FocusGatewayRequestIds "gw-window-good" `
    -RequireStreamCompletion `
    -RequiredCompletedStreams 1 `
    -Quiet 2>$null

  if ($LASTEXITCODE -ne 0) {
    throw "live monitor audit-window fixture failed with exit_code=$LASTEXITCODE"
  }
  $auditWindowSummary = Convert-JsonOutput $auditWindowOutput "audit window fixture"
  Assert-Equal $auditWindowSummary.overall "pass" "expected audit-window fixture overall pass"
  Assert-Equal $auditWindowSummary.audit.responsesTransport503PoolUnavailableCount 0 "old transport 503 outside the window must not pollute focused verdict"
  Assert-Equal $auditWindowSummary.audit.completedStreamCount 1 "expected focused gateway request to retain stream completion"
  Assert-Equal $auditWindowSummary.auditWindow.rawObservedEventCount 6 "expected raw observed events to include old history"
  Assert-Equal $auditWindowSummary.auditWindow.filteredEventCount 4 "expected only focused current-window events in summary"
  Assert-Equal $auditWindowSummary.auditWindow.droppedEventCount 2 "expected old history to be dropped by window filter"
  Assert-Equal $auditWindowSummary.auditWindow.sinceTimestampMs 100 "expected since timestamp in report metadata"
  Assert-Equal $auditWindowSummary.auditWindow.focusGatewayRequestIds[0] "gw-window-good" "expected focused gateway id in report metadata"

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
    [ordered]@{ schemaVersion = 1; timestamp = 1; requestId = "req-wait"; phase = "request_trace"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "-"; outcome = "prepared"; detail = [ordered]@{ gateway_request_id = "gw-wait"; turn_lineage_id = "turn:sha256:wait"; hard_affinity_continuity = "false" } },
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

  $dataRootStickyResetRecovered = Join-Path $tempRoot "data-sticky-reset-recovered"
  $auditStickyResetRecovered = Join-Path $dataRootStickyResetRecovered "codex_local_access_audit.jsonl"
  Write-AuditLines $auditStickyResetRecovered @(
    [ordered]@{ schemaVersion = 1; timestamp = 1; requestId = "turn:sha256:sticky-recovered"; phase = "listener"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "-"; outcome = "accepted"; detail = [ordered]@{ gateway_request_id = "gw-sticky-recovered" } },
    [ordered]@{ schemaVersion = 1; timestamp = 2; requestId = "turn:sha256:sticky-recovered"; phase = "request_trace"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "-"; outcome = "prepared"; detail = [ordered]@{ gateway_request_id = "gw-sticky-recovered"; normal_request_timeout_ms = "1000"; request_timeout_ms = "4000"; hard_affinity_wait_limit_ms = "3000"; timeout_extended = "true"; hard_affinity_continuity = "true"; sticky_boundary = "x_codex_turn_state"; turn_lineage_id = "turn:sha256:sticky-recovered"; turn_lineage_source = "codex_turn_state" } },
    [ordered]@{ schemaVersion = 1; timestamp = 3; requestId = "turn:sha256:sticky-recovered"; phase = "upstream_forward"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:old"; status = 429; errorType = "usage_limit_reached"; outcome = "response_received"; detail = [ordered]@{ gateway_request_id = "gw-sticky-recovered"; turn_lineage_id = "turn:sha256:sticky-recovered"; retry_after_ms = "1500" } },
    [ordered]@{ schemaVersion = 1; timestamp = 4; requestId = "turn:sha256:sticky-recovered"; phase = "fallback_blocked"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:old"; status = 429; errorType = "usage_limit_reached"; outcome = "hard_affinity"; detail = [ordered]@{ gateway_request_id = "gw-sticky-recovered"; turn_lineage_id = "turn:sha256:sticky-recovered"; retry_after_ms = "1500"; hard_affinity_inline_retry_wait_limit_ms = "3000" } },
    [ordered]@{ schemaVersion = 1; timestamp = 5; requestId = "turn:sha256:sticky-recovered"; phase = "pool_wait"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:old"; status = 429; errorType = "usage_limit_reached"; outcome = "sleeping"; detail = [ordered]@{ gateway_request_id = "gw-sticky-recovered"; turn_lineage_id = "turn:sha256:sticky-recovered"; reason = "hard_affinity_same_account_retry"; retry_after_ms = "1500"; inline_wait_limit_ms = "3000" } },
    [ordered]@{ schemaVersion = 1; timestamp = 6; requestId = "turn:sha256:sticky-recovered"; phase = "pool_wait"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:old"; status = 429; errorType = "usage_limit_reached"; outcome = "retrying"; detail = [ordered]@{ gateway_request_id = "gw-sticky-recovered"; turn_lineage_id = "turn:sha256:sticky-recovered"; reason = "hard_affinity_same_account_retry"; slept_ms = "1500" } },
    [ordered]@{ schemaVersion = 1; timestamp = 7; requestId = "turn:sha256:sticky-recovered"; phase = "upstream_forward"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:old"; status = 200; outcome = "response_received"; detail = [ordered]@{ gateway_request_id = "gw-sticky-recovered"; turn_lineage_id = "turn:sha256:sticky-recovered" } },
    [ordered]@{ schemaVersion = 1; timestamp = 8; requestId = "turn:sha256:sticky-recovered"; phase = "stream_completed"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:old"; outcome = "completed"; detail = [ordered]@{ gateway_request_id = "gw-sticky-recovered"; turn_lineage_id = "turn:sha256:sticky-recovered" } }
  )
  $stickyResetRecoveredOutput = & pwsh -NoProfile -ExecutionPolicy Bypass -File $monitorScript `
    -DurationSeconds 0 `
    -DataRoot $dataRootStickyResetRecovered `
    -CodexHome $codexHome `
    -CodexAppProcessNames "__cockpit_no_such_process__" `
    -IncludeExistingAudit `
    -RequireStreamCompletion `
    -RequiredCompletedStreams 1 `
    -Quiet 2>$null

  if ($LASTEXITCODE -ne 0) {
    throw "live monitor sticky reset recovered fixture failed with exit_code=$LASTEXITCODE"
  }
  $stickyResetRecoveredSummary = Convert-JsonOutput $stickyResetRecoveredOutput "sticky reset recovered fixture"
  Assert-Equal $stickyResetRecoveredSummary.overall "pass" "expected sticky reset recovered fixture overall"
  Assert-Equal $stickyResetRecoveredSummary.audit.stickyResetWaitRecoveredCount 1 "expected one sticky reset wait recovery"
  Assert-Equal $stickyResetRecoveredSummary.audit.stickyResetWaitKilledByLocalTimeoutCount 0 "expected no sticky reset wait timeout kill"
  Assert-Equal $stickyResetRecoveredSummary.audit.stickyResetWaitRecovered[0].hardAffinityWaitLimitMs 3000 "expected short hard-affinity wait limit in sticky reset evidence"
  Assert-Equal (($stickyResetRecoveredSummary.results | Where-Object name -eq "sticky_reset_wait_not_killed_by_local_timeout").status) "pass" "expected sticky reset recovered guard pass"

  $dataRootStickyResetOversized = Join-Path $tempRoot "data-sticky-reset-oversized"
  $auditStickyResetOversized = Join-Path $dataRootStickyResetOversized "codex_local_access_audit.jsonl"
  Write-AuditLines $auditStickyResetOversized @(
    [ordered]@{ schemaVersion = 1; timestamp = 1; requestId = "turn:sha256:sticky-oversized"; phase = "request_trace"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "-"; outcome = "prepared"; detail = [ordered]@{ gateway_request_id = "gw-sticky-oversized"; normal_request_timeout_ms = "600000"; request_timeout_ms = "604283000"; hard_affinity_wait_limit_ms = "604282000"; timeout_extended = "true"; hard_affinity_continuity = "true"; sticky_boundary = "x_codex_turn_state"; turn_lineage_id = "turn:sha256:sticky-oversized"; turn_lineage_source = "codex_turn_state" } },
    [ordered]@{ schemaVersion = 1; timestamp = 2; requestId = "turn:sha256:sticky-oversized"; phase = "upstream_forward"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:old"; status = 429; errorType = "usage_limit_reached"; outcome = "response_received"; detail = [ordered]@{ gateway_request_id = "gw-sticky-oversized"; turn_lineage_id = "turn:sha256:sticky-oversized"; retry_after_ms = "604282000" } },
    [ordered]@{ schemaVersion = 1; timestamp = 3; requestId = "turn:sha256:sticky-oversized"; phase = "fallback_blocked"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:old"; status = 429; errorType = "usage_limit_reached"; outcome = "hard_affinity"; detail = [ordered]@{ gateway_request_id = "gw-sticky-oversized"; turn_lineage_id = "turn:sha256:sticky-oversized"; retry_after_ms = "604282000"; hard_affinity_inline_retry_wait_limit_ms = "604282000" } },
    [ordered]@{ schemaVersion = 1; timestamp = 4; requestId = "turn:sha256:sticky-oversized"; phase = "pool_wait"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:old"; status = 429; errorType = "usage_limit_reached"; outcome = "sleeping"; detail = [ordered]@{ gateway_request_id = "gw-sticky-oversized"; turn_lineage_id = "turn:sha256:sticky-oversized"; reason = "hard_affinity_same_account_retry"; retry_after_ms = "604282000"; inline_wait_limit_ms = "604282000" } },
    [ordered]@{ schemaVersion = 1; timestamp = 5; requestId = "turn:sha256:sticky-oversized"; phase = "final_response"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:old"; status = 429; errorType = "usage_limit_reached"; outcome = "error"; detail = [ordered]@{ gateway_request_id = "gw-sticky-oversized"; turn_lineage_id = "turn:sha256:sticky-oversized" } }
  )
  $stickyResetOversizedOutput = & pwsh -NoProfile -ExecutionPolicy Bypass -File $monitorScript `
    -DurationSeconds 0 `
    -DataRoot $dataRootStickyResetOversized `
    -CodexHome $codexHome `
    -CodexAppProcessNames "__cockpit_no_such_process__" `
    -IncludeExistingAudit `
    -Quiet 2>$null

  Assert-True ($LASTEXITCODE -eq 1) "expected oversized sticky reset wait fixture exit code 1"
  $stickyResetOversizedSummary = Convert-JsonOutput $stickyResetOversizedOutput "oversized sticky reset wait fixture"
  Assert-Equal $stickyResetOversizedSummary.overall "fail" "expected oversized sticky reset wait fixture overall fail"
  Assert-Equal $stickyResetOversizedSummary.audit.stickyResetWaitExceededInlineBudgetCount 1 "expected oversized sticky wait budget to be flagged"
  Assert-Equal (($stickyResetOversizedSummary.results | Where-Object name -eq "sticky_reset_wait_not_killed_by_local_timeout").status) "fail" "expected oversized sticky wait guard fail"

  $dataRootStructuredTrace = Join-Path $tempRoot "data-structured-trace"
  $auditStructuredTrace = Join-Path $dataRootStructuredTrace "codex_local_access_audit.jsonl"
  Write-AuditLines $auditStructuredTrace @(
    [ordered]@{ schemaVersion = 1; timestamp = 1; requestId = "turn:sha256:trace"; phase = "listener"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "-"; outcome = "accepted"; detail = [ordered]@{ gateway_request_id = "gw-trace" } },
    [ordered]@{ schemaVersion = 1; timestamp = 2; requestId = "turn:sha256:trace"; phase = "request_trace"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "-"; outcome = "prepared"; detail = [ordered]@{ gateway_request_id = "gw-trace"; normal_request_timeout_ms = "1000"; request_timeout_ms = "4000"; hard_affinity_wait_limit_ms = "3000"; timeout_extended = "true"; hard_affinity_continuity = "true"; sticky_boundary = "x_codex_turn_state"; turn_lineage_id = "turn:sha256:trace"; turn_lineage_source = "codex_turn_state" } },
    [ordered]@{ schemaVersion = 1; timestamp = 3; requestId = "turn:sha256:trace"; phase = "quota_classification"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:old"; status = 429; errorType = "usage_limit_reached"; outcome = "exhausted"; detail = [ordered]@{ gateway_request_id = "gw-trace"; turn_lineage_id = "turn:sha256:trace"; retry_after_ms = "1500"; reset_source = "retry_after" } },
    [ordered]@{ schemaVersion = 1; timestamp = 4; requestId = "turn:sha256:trace"; phase = "routing_decision"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:old"; outcome = "selected"; detail = [ordered]@{ gateway_request_id = "gw-trace"; turn_lineage_id = "turn:sha256:trace"; hard_affinity_bound = "true" } },
    [ordered]@{ schemaVersion = 1; timestamp = 5; requestId = "turn:sha256:trace"; phase = "fallback_blocked"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:old"; status = 429; errorType = "usage_limit_reached"; outcome = "hard_affinity"; detail = [ordered]@{ gateway_request_id = "gw-trace"; turn_lineage_id = "turn:sha256:trace"; retry_after_ms = "1500"; hard_affinity_inline_retry_wait_limit_ms = "3000" } },
    [ordered]@{ schemaVersion = 1; timestamp = 6; requestId = "turn:sha256:trace"; phase = "pool_wait"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:old"; status = 429; errorType = "usage_limit_reached"; outcome = "sleeping"; detail = [ordered]@{ gateway_request_id = "gw-trace"; turn_lineage_id = "turn:sha256:trace"; reason = "hard_affinity_same_account_retry"; retry_after_ms = "1500"; inline_wait_limit_ms = "3000" } },
    [ordered]@{ schemaVersion = 1; timestamp = 7; requestId = "turn:sha256:trace"; phase = "pool_wait"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:old"; status = 429; errorType = "usage_limit_reached"; outcome = "retrying"; detail = [ordered]@{ gateway_request_id = "gw-trace"; turn_lineage_id = "turn:sha256:trace"; reason = "hard_affinity_same_account_retry"; slept_ms = "1500" } },
    [ordered]@{ schemaVersion = 1; timestamp = 8; requestId = "turn:sha256:trace"; phase = "stream_terminal"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:old"; outcome = "completed"; detail = [ordered]@{ gateway_request_id = "gw-trace"; turn_lineage_id = "turn:sha256:trace"; response_completed_seen = "true"; compaction_summary_seen = "true"; response_id_hash = "response:sha256:trace" } }
  )
  $structuredTraceOutput = & pwsh -NoProfile -ExecutionPolicy Bypass -File $monitorScript `
    -DurationSeconds 0 `
    -DataRoot $dataRootStructuredTrace `
    -CodexHome $codexHome `
    -CodexAppProcessNames "__cockpit_no_such_process__" `
    -IncludeExistingAudit `
    -RequireStreamCompletion `
    -RequiredCompletedStreams 1 `
    -Quiet 2>$null

  if ($LASTEXITCODE -ne 0) {
    throw "live monitor structured trace fixture failed with exit_code=$LASTEXITCODE"
  }
  $structuredTraceSummary = Convert-JsonOutput $structuredTraceOutput "structured trace fixture"
  Assert-Equal $structuredTraceSummary.overall "pass" "expected structured trace fixture overall"
  Assert-Equal $structuredTraceSummary.audit.requestTraceCount 1 "expected request_trace count"
  Assert-Equal $structuredTraceSummary.audit.quotaClassificationCount 1 "expected quota_classification count"
  Assert-Equal $structuredTraceSummary.audit.routingDecisionCount 1 "expected routing_decision count"
  Assert-Equal $structuredTraceSummary.audit.streamTerminalCount 1 "expected stream_terminal count"
  Assert-Equal $structuredTraceSummary.audit.streamTerminalResponseCompletedCount 1 "expected response.completed trace count"
  Assert-Equal $structuredTraceSummary.audit.streamTerminalCompactionSummaryCount 1 "expected compaction summary trace count"
  Assert-Equal $structuredTraceSummary.audit.completedStreamCount 1 "expected stream_terminal to count as completed stream"
  Assert-Equal (($structuredTraceSummary.results | Where-Object name -eq "structured_behavior_trace_present").status) "pass" "expected structured behavior trace coverage pass"

  $dataRootStickyResetTimeout = Join-Path $tempRoot "data-sticky-reset-timeout"
  $auditStickyResetTimeout = Join-Path $dataRootStickyResetTimeout "codex_local_access_audit.jsonl"
  Write-AuditLines $auditStickyResetTimeout @(
    [ordered]@{ schemaVersion = 1; timestamp = 1; requestId = "turn:sha256:sticky-timeout"; phase = "listener"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "-"; outcome = "accepted"; detail = [ordered]@{ gateway_request_id = "gw-sticky-timeout" } },
    [ordered]@{ schemaVersion = 1; timestamp = 2; requestId = "turn:sha256:sticky-timeout"; phase = "request_trace"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "-"; outcome = "prepared"; detail = [ordered]@{ gateway_request_id = "gw-sticky-timeout"; normal_request_timeout_ms = "1000"; request_timeout_ms = "1000"; hard_affinity_wait_limit_ms = "1000"; timeout_extended = "false"; hard_affinity_continuity = "true"; sticky_boundary = "x_codex_turn_state"; turn_lineage_id = "turn:sha256:sticky-timeout"; turn_lineage_source = "codex_turn_state" } },
    [ordered]@{ schemaVersion = 1; timestamp = 3; requestId = "turn:sha256:sticky-timeout"; phase = "upstream_forward"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:old"; status = 429; errorType = "usage_limit_reached"; outcome = "response_received"; detail = [ordered]@{ gateway_request_id = "gw-sticky-timeout"; turn_lineage_id = "turn:sha256:sticky-timeout"; retry_after_ms = "1500" } },
    [ordered]@{ schemaVersion = 1; timestamp = 4; requestId = "turn:sha256:sticky-timeout"; phase = "fallback_blocked"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:old"; status = 429; errorType = "usage_limit_reached"; outcome = "hard_affinity"; detail = [ordered]@{ gateway_request_id = "gw-sticky-timeout"; turn_lineage_id = "turn:sha256:sticky-timeout"; retry_after_ms = "1500"; hard_affinity_inline_retry_wait_limit_ms = "1000" } },
    [ordered]@{ schemaVersion = 1; timestamp = 5; requestId = "turn:sha256:sticky-timeout"; phase = "final_response"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:old"; status = 503; errorType = "pool_unavailable"; outcome = "error"; detail = [ordered]@{ gateway_request_id = "gw-sticky-timeout"; turn_lineage_id = "turn:sha256:sticky-timeout"; message = "本地接入请求超时，请稍后重试" } }
  )
  $stickyResetTimeoutOutput = & pwsh -NoProfile -ExecutionPolicy Bypass -File $monitorScript `
    -DurationSeconds 0 `
    -DataRoot $dataRootStickyResetTimeout `
    -CodexHome $codexHome `
    -CodexAppProcessNames "__cockpit_no_such_process__" `
    -IncludeExistingAudit `
    -Quiet 2>$null

  Assert-True ($LASTEXITCODE -eq 1) "expected sticky reset timeout fixture exit code 1"
  $stickyResetTimeoutSummary = Convert-JsonOutput $stickyResetTimeoutOutput "sticky reset timeout fixture"
  Assert-Equal $stickyResetTimeoutSummary.overall "fail" "expected sticky reset timeout fixture overall fail"
  Assert-Equal $stickyResetTimeoutSummary.audit.stickyResetWaitKilledByLocalTimeoutCount 1 "expected one sticky reset wait killed by local timeout"
  Assert-Equal (($stickyResetTimeoutSummary.results | Where-Object name -eq "sticky_reset_wait_not_killed_by_local_timeout").status) "fail" "expected sticky reset timeout guard fail"

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

  $dataRootUpstreamStreamError = Join-Path $tempRoot "data-upstream-stream-error"
  $auditUpstreamStreamError = Join-Path $dataRootUpstreamStreamError "codex_local_access_audit.jsonl"
  Write-AuditLines $auditUpstreamStreamError @(
    [ordered]@{ schemaVersion = 1; timestamp = 1; requestId = "turn:sha256:stream-error"; phase = "listener"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "-"; outcome = "accepted"; detail = [ordered]@{ gateway_request_id = "gw-stream-error"; turn_lineage_id = "turn:sha256:stream-error" } },
    [ordered]@{ schemaVersion = 1; timestamp = 2; requestId = "turn:sha256:stream-error"; phase = "lease_granted"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:active"; outcome = "active"; detail = [ordered]@{ gateway_request_id = "gw-stream-error"; turn_lineage_id = "turn:sha256:stream-error" } },
    [ordered]@{ schemaVersion = 1; timestamp = 3; requestId = "turn:sha256:stream-error"; phase = "stream_write"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:active"; status = 200; streamState = "first_chunk_written"; outcome = "ok"; detail = [ordered]@{ gateway_request_id = "gw-stream-error"; turn_lineage_id = "turn:sha256:stream-error" } },
    [ordered]@{ schemaVersion = 1; timestamp = 4; requestId = "turn:sha256:stream-error"; phase = "stream_write"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:active"; status = 200; errorType = "upstream_stream_error"; streamState = "upstream_error"; outcome = "error"; detail = [ordered]@{ gateway_request_id = "gw-stream-error"; turn_lineage_id = "turn:sha256:stream-error"; terminal_origin = "upstream_stream_error"; response_completed_seen = "false"; terminal_contract = "response_failed_sse" } },
    [ordered]@{ schemaVersion = 1; timestamp = 5; requestId = "turn:sha256:stream-error"; phase = "stream_terminal"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:active"; status = 200; errorType = "upstream_stream_error"; streamState = "upstream_error"; outcome = "error"; detail = [ordered]@{ gateway_request_id = "gw-stream-error"; turn_lineage_id = "turn:sha256:stream-error"; terminal_origin = "upstream_stream_error"; response_completed_seen = "false"; terminal_contract = "response_failed_sse" } },
    [ordered]@{ schemaVersion = 1; timestamp = 6; requestId = "turn:sha256:stream-error"; phase = "stream_error"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:active"; outcome = "error"; detail = [ordered]@{ gateway_request_id = "gw-stream-error"; turn_lineage_id = "turn:sha256:stream-error" } },
    [ordered]@{ schemaVersion = 1; timestamp = 7; requestId = "turn:sha256:stream-error"; phase = "lease_released"; route = "/v1/responses"; model = "gpt-5.5"; accountHash = "sha256:active"; outcome = "error"; detail = [ordered]@{ gateway_request_id = "gw-stream-error"; turn_lineage_id = "turn:sha256:stream-error" } }
  )

  $upstreamStreamErrorOutput = & pwsh -NoProfile -ExecutionPolicy Bypass -File $monitorScript `
    -DurationSeconds 0 `
    -DataRoot $dataRootUpstreamStreamError `
    -CodexHome $codexHome `
    -CodexAppProcessNames "__cockpit_no_such_process__" `
    -IncludeExistingAudit `
    -RequireStreamCompletion `
    -Quiet 2>$null

  Assert-True ($LASTEXITCODE -eq 1) "expected upstream stream error fixture exit code 1"
  $upstreamStreamErrorSummary = Convert-JsonOutput $upstreamStreamErrorOutput "upstream stream error fixture"
  Assert-Equal $upstreamStreamErrorSummary.overall "fail" "expected upstream stream error fixture overall fail"
  Assert-Equal $upstreamStreamErrorSummary.audit.upstreamStreamErrorCount 1 "expected upstream stream error to be counted"
  Assert-Equal $upstreamStreamErrorSummary.audit.terminalErrorStreamCount 1 "expected upstream stream error to be terminal"
  Assert-Equal (($upstreamStreamErrorSummary.results | Where-Object name -eq "accepted_stream_continuity").status) "fail" "expected upstream stream error to fail stream continuity"

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
