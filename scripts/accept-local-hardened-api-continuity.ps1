param(
  [string]$Model = "gpt-5.5",
  [int]$TimeoutSeconds = 900,
  [ValidateRange(2, 20)]
  [int]$MaxProbeQuotaRefreshAttempts = 2,
  [switch]$AcknowledgeLiveUpstreamRisk,
  [switch]$AcknowledgeExpandedLiveUpstreamRisk,
  [switch]$DrainFirstFreeAccountUntilFallback,
  [ValidateRange(1, 200)]
  [int]$DrainMaxRequests = 30,
  [ValidateRange(0, 300)]
  [int]$DrainRequestIntervalSeconds = 22,
  [switch]$SkipEphemeralGatewayBuild,
  [string]$SmokeScriptPath = (Join-Path $PSScriptRoot "smoke-local-hardened-api.ps1"),
  [string]$CodexHome = (Join-Path $HOME ".codex"),
  [string]$DataRoot = (Join-Path $HOME ".antigravity_cockpit")
)

$ErrorActionPreference = "Stop"

$expandedLiveUpstreamRiskReasons = @()
if ($MaxProbeQuotaRefreshAttempts -gt 2) {
  $expandedLiveUpstreamRiskReasons += "max_probe_quota_refresh_attempts_gt_2"
}
if ($DrainFirstFreeAccountUntilFallback -and $DrainMaxRequests -gt 30) {
  $expandedLiveUpstreamRiskReasons += "drain_max_requests_gt_30"
}
if ($DrainFirstFreeAccountUntilFallback -and $DrainRequestIntervalSeconds -lt 20) {
  $expandedLiveUpstreamRiskReasons += "drain_request_interval_lt_20s"
}

if (-not $AcknowledgeLiveUpstreamRisk) {
  [ordered]@{
    overall = "blocked"
    reason = "live_upstream_risk_ack_required"
    requiredSwitch = "-AcknowledgeLiveUpstreamRisk"
    liveActions = @(
      "RunUpstreamSmoke"
      "RunCodexExecSmoke"
      "AutoPopulateProbeAccountPool"
      "optional AutoDrainFirstFreeAccountUntilFallback"
    )
    maxProbeQuotaRefreshAttempts = $MaxProbeQuotaRefreshAttempts
    drainFirstFreeAccountUntilFallback = [bool]$DrainFirstFreeAccountUntilFallback
    drainMaxRequests = $DrainMaxRequests
    drainRequestIntervalSeconds = $DrainRequestIntervalSeconds
  } | ConvertTo-Json -Depth 8
  exit 2
}

if ($expandedLiveUpstreamRiskReasons.Count -gt 0 -and -not $AcknowledgeExpandedLiveUpstreamRisk) {
  [ordered]@{
    overall = "blocked"
    reason = "expanded_live_upstream_risk_ack_required"
    requiredSwitch = "-AcknowledgeExpandedLiveUpstreamRisk"
    expandedReasons = @($expandedLiveUpstreamRiskReasons)
    maxProbeQuotaRefreshAttempts = $MaxProbeQuotaRefreshAttempts
    drainMaxRequests = $DrainMaxRequests
    drainRequestIntervalSeconds = $DrainRequestIntervalSeconds
  } | ConvertTo-Json -Depth 8
  exit 2
}

function Get-DescendantProcessIds {
  param([int]$RootProcessId)
  $all = @(Get-CimInstance Win32_Process | Select-Object ProcessId, ParentProcessId)
  $pending = @($RootProcessId)
  $descendants = @()
  while ($pending.Count -gt 0) {
    $parent = $pending[0]
    $pending = @($pending | Select-Object -Skip 1)
    $children = @($all | Where-Object { $_.ParentProcessId -eq $parent } | ForEach-Object { [int]$_.ProcessId })
    foreach ($child in $children) {
      if ($descendants -notcontains $child) {
        $descendants += $child
        $pending += $child
      }
    }
  }
  $descendants
}

function Stop-OwnedProcessTree {
  param([int]$RootProcessId)
  $ids = @(Get-DescendantProcessIds $RootProcessId)
  [array]::Reverse($ids)
  foreach ($id in $ids) {
    $process = Get-Process -Id $id -ErrorAction SilentlyContinue
    if ($process) {
      Stop-Process -Id $id -Force
    }
  }
  $root = Get-Process -Id $RootProcessId -ErrorAction SilentlyContinue
  if ($root) {
    Stop-Process -Id $RootProcessId -Force
  }
}

function Get-ResultStatus {
  param([object]$Report, [string]$Name)
  $item = @($Report.results | Where-Object { $_.name -eq $Name } | Select-Object -First 1)
  if ($item) {
    return [string]$item.status
  }
  "missing"
}

function Get-SmokeJsonFromStdout {
  param([string]$Path)
  if (-not (Test-Path -LiteralPath $Path)) {
    throw "smoke stdout 不存在: $Path"
  }
  $text = Get-Content -LiteralPath $Path -Raw
  if (-not $text.Trim()) {
    throw "smoke stdout 为空"
  }
  try {
    return ($text | ConvertFrom-Json)
  } catch {
    $start = $text.IndexOf("{")
    $end = $text.LastIndexOf("}")
    if ($start -lt 0 -or $end -le $start) {
      throw "smoke stdout 未包含 JSON 报告"
    }
    return ($text.Substring($start, $end - $start + 1) | ConvertFrom-Json)
  }
}

if (-not (Test-Path -LiteralPath $SmokeScriptPath)) {
  throw "smoke 脚本不存在: $SmokeScriptPath"
}

$tempRoot = Join-Path ([System.IO.Path]::GetTempPath()) ("cockpit-hla-accept-{0}-{1}" -f $PID, (Get-Date -Format "yyyyMMddHHmmssfff"))
New-Item -ItemType Directory -Force -Path $tempRoot | Out-Null
$stdoutPath = Join-Path $tempRoot "smoke.stdout.json"
$stderrPath = Join-Path $tempRoot "smoke.stderr.log"

$smokeArgs = @(
  "-NoProfile",
  "-ExecutionPolicy",
  "Bypass",
  "-File",
  $SmokeScriptPath,
  "-Stage",
  "fallback_probe",
  "-Model",
  $Model,
  "-StartEphemeralGateway",
  "-TemporaryFallbackConfig",
  "-AppSafeIsolatedProbe",
  "-AutoPopulateProbeAccountPool",
  "-AutoPopulateProbeMaxRefreshAttempts",
  $MaxProbeQuotaRefreshAttempts,
  "-AcknowledgeLiveUpstreamRisk",
  "-RunUpstreamSmoke",
  "-RunCodexExecSmoke",
  "-RequireQuotaFallback",
  "-AssertCodexCliConfigUntouched",
  "-AssertCodexAppProcessStable",
  "-CodexHome",
  $CodexHome,
  "-DataRoot",
  $DataRoot,
  "-WriteReport"
)
if ($AcknowledgeExpandedLiveUpstreamRisk) {
  $smokeArgs += "-AcknowledgeExpandedLiveUpstreamRisk"
}
if ($SkipEphemeralGatewayBuild) {
  $smokeArgs += "-SkipEphemeralGatewayBuild"
}
if ($DrainFirstFreeAccountUntilFallback) {
  $smokeArgs += @(
    "-AutoDrainFirstFreeAccountUntilFallback",
    "-AutoDrainMaxRequests",
    $DrainMaxRequests,
    "-AutoDrainRequestIntervalSeconds",
    $DrainRequestIntervalSeconds
  )
}

$startedAt = Get-Date
$process = Start-Process `
  -FilePath "pwsh" `
  -ArgumentList $smokeArgs `
  -WindowStyle Hidden `
  -PassThru `
  -RedirectStandardOutput $stdoutPath `
  -RedirectStandardError $stderrPath

$completed = $process.WaitForExit($TimeoutSeconds * 1000)
if (-not $completed) {
  Stop-OwnedProcessTree $process.Id
  $stderrPreview = if (Test-Path -LiteralPath $stderrPath) {
    (Get-Content -LiteralPath $stderrPath -Raw)
  } else {
    ""
  }
  $summary = [ordered]@{
    overall = "timeout"
    timeoutSeconds = $TimeoutSeconds
    startedAt = $startedAt.ToString("o")
    tempRoot = $tempRoot
    stderrPreview = if ($stderrPreview.Length -gt 500) { $stderrPreview.Substring(0, 500) } else { $stderrPreview }
  }
  $summary | ConvertTo-Json -Depth 8
  exit 124
}

$exitCode = $process.ExitCode
if ($exitCode -ne 0) {
  $stderrPreview = if (Test-Path -LiteralPath $stderrPath) {
    (Get-Content -LiteralPath $stderrPath -Raw)
  } else {
    ""
  }
  $summary = [ordered]@{
    overall = "fail"
    exitCode = $exitCode
    tempRoot = $tempRoot
    stdoutPath = $stdoutPath
    stderrPath = $stderrPath
    stderrPreview = if ($stderrPreview.Length -gt 500) { $stderrPreview.Substring(0, 500) } else { $stderrPreview }
  }
  $summary | ConvertTo-Json -Depth 8
  exit $exitCode
}

$report = Get-SmokeJsonFromStdout $stdoutPath
$roles = @(
  $report.temporaryFallbackConfig.autoPopulateProbeAccountPool.selectedAccountRoles |
    ForEach-Object {
      [ordered]@{
        role = $_.role
        planType = $_.planType
        quotaKind = $_.quotaKind
        weeklyRemainingPercent = $_.weeklyRemainingPercent
      }
    }
)

$summary = [ordered]@{
  overall = [string]$report.overall
  reportPath = $report.reportPath
  model = $Model
  elapsedSeconds = [math]::Round(((Get-Date) - $startedAt).TotalSeconds, 1)
  liveUpstreamRiskAcknowledged = [bool]$AcknowledgeLiveUpstreamRisk
  expandedLiveUpstreamRiskAcknowledged = [bool]$AcknowledgeExpandedLiveUpstreamRisk
  selectionOrder = $report.temporaryFallbackConfig.autoPopulateProbeAccountPool.selectionOrder
  refreshAttemptCount = $report.temporaryFallbackConfig.autoPopulateProbeAccountPool.refreshAttemptCount
  maxRefreshAttempts = $report.temporaryFallbackConfig.autoPopulateProbeAccountPool.maxRefreshAttempts
  drainRequested = [bool]$report.autoDrainFirstFreeAccountUntilFallback
  drainRequired = [bool]$report.temporaryFallbackConfig.autoPopulateProbeAccountPool.drainRequired
  drainResult = Get-ResultStatus $report "quota_drain_until_fallback"
  selectedRoles = $roles
  quotaFallback = Get-ResultStatus $report "quota_fallback_audit_contract"
  codexExec = Get-ResultStatus $report "codex_exec_task_e2e"
  cliUntouched = Get-ResultStatus $report "codex_cli_config_auth_untouched"
  appStable = Get-ResultStatus $report "codex_app_process_stable"
  gatewayStopped = [bool]$report.ephemeralGateway.stopped
  liveConfigRestored = [bool]$report.ephemeralGateway.restoredConfig
  tempRoot = $tempRoot
}

$summary | ConvertTo-Json -Depth 8
if ($report.overall -ne "pass") {
  exit 1
}
