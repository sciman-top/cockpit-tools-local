param(
  [string]$DataRoot = "",
  [int]$AuditWindowMinutes = 10,
  [int]$TailLines = 5000,
  [switch]$SkipLocalModelsProbe
)

$ErrorActionPreference = "Stop"

function Add-Count {
  param(
    [hashtable]$Table,
    [string]$Key
  )
  if ([string]::IsNullOrWhiteSpace($Key)) { return }
  if ($Table.ContainsKey($Key)) {
    $Table[$Key] = [int]$Table[$Key] + 1
  } else {
    $Table[$Key] = 1
  }
}

function Get-DetailValue {
  param(
    [object]$Event,
    [string]$Name
  )
  if (-not $Event -or -not $Event.detail) { return $null }
  $prop = $Event.detail.PSObject.Properties[$Name]
  if ($prop) { return [string]$prop.Value }
  return $null
}

function Test-UpstreamLimitEvent {
  param([object]$Event)
  $errorType = ([string]$Event.errorType).ToLowerInvariant()
  return @(
    "usage_limit_reached",
    "upstream_rate_limit",
    "insufficient_quota",
    "model_capacity",
    "rate_limited"
  ) -contains $errorType
}

function Get-ProblemKind {
  param([object]$Event)
  $phase = ([string]$Event.phase).ToLowerInvariant()
  $errorType = ([string]$Event.errorType).ToLowerInvariant()
  if ($phase -eq "local_backpressure" -or $errorType -eq "local_backpressure") {
    return "local_backpressure"
  }
  if ($phase -eq "pool_wait" -or $errorType -eq "cockpit_pool_wait") {
    return "pool_wait"
  }
  if (Test-UpstreamLimitEvent $Event) {
    return "upstream_limit"
  }
  if ($phase.Contains("stream_error") -or $errorType.Contains("stream_error")) {
    return "stream_error"
  }
  return $null
}

function Read-AuditEvents {
  param(
    [string]$Root,
    [int64]$WindowStartMs,
    [int]$LineLimit
  )
  $events = @()
  foreach ($name in @("codex_local_access_audit.jsonl.1", "codex_local_access_audit.jsonl")) {
    $path = Join-Path $Root $name
    if (-not (Test-Path -LiteralPath $path)) { continue }
    foreach ($line in Get-Content -LiteralPath $path -Tail $LineLimit) {
      if ([string]::IsNullOrWhiteSpace($line)) { continue }
      try {
        $event = $line | ConvertFrom-Json
        if ($event.timestamp -and [int64]$event.timestamp -ge $WindowStartMs) {
          $events += $event
        }
      } catch {
        # Keep diagnostics best-effort; malformed historical lines should not block live triage.
      }
    }
  }
  return @($events)
}

function Read-RecentLeaseState {
  param(
    [string]$Root,
    [int]$LineLimit
  )
  $auditPath = Join-Path $Root "codex_local_access_audit.jsonl"
  $openLeases = @{}
  if (-not (Test-Path -LiteralPath $auditPath)) {
    return $openLeases
  }
  $events = @()
  foreach ($line in Get-Content -LiteralPath $auditPath -Tail $LineLimit) {
    if ([string]::IsNullOrWhiteSpace($line)) { continue }
    try { $events += ($line | ConvertFrom-Json) } catch {}
  }
  foreach ($event in ($events | Sort-Object { [int64]($_.timestamp ?? 0) })) {
    $leaseId = Get-DetailValue -Event $event -Name "lease_id"
    if (-not $leaseId) { continue }
    if ($event.phase -eq "lease_granted") {
      $openLeases[$leaseId] = [ordered]@{
        requestId = $event.requestId
        accountHash = $event.accountHash
        timestamp = $event.timestamp
        model = $event.model
      }
    } elseif ($event.phase -eq "lease_released") {
      $openLeases.Remove($leaseId)
    }
  }
  return $openLeases
}

function Get-PortState {
  param([int[]]$Ports)
  $result = @()
  foreach ($port in ($Ports | Where-Object { $_ -gt 0 } | Sort-Object -Unique)) {
    $connections = @(Get-NetTCPConnection -LocalPort $port -ErrorAction SilentlyContinue)
    $owners = @($connections | Group-Object State, OwningProcess | ForEach-Object {
      $parts = $_.Name -split ", "
      $ownerPid = if ($parts.Count -gt 1) { [int]$parts[1] } else { 0 }
      $process = if ($ownerPid) { Get-Process -Id $ownerPid -ErrorAction SilentlyContinue } else { $null }
      [ordered]@{
        state = $parts[0]
        ownerPid = $ownerPid
        process = if ($process) { $process.ProcessName } else { $null }
        count = $_.Count
      }
    })
    $result += [ordered]@{
      port = $port
      connectionCount = $connections.Count
      owners = $owners
    }
  }
  return @($result)
}

function Get-HealthSummary {
  param([string]$Root)
  $path = Join-Path $Root "codex_local_access_health.json"
  $summary = [ordered]@{
    exists = Test-Path -LiteralPath $path
    statusCounts = @{}
    lastErrorTypes = @{}
    nearestCooldownUntilMs = $null
    error = $null
  }
  if (-not $summary.exists) { return $summary }
  try {
    $health = Get-Content -LiteralPath $path -Raw | ConvertFrom-Json
    foreach ($prop in @($health.accounts.PSObject.Properties)) {
      $account = $prop.Value
      Add-Count -Table $summary.statusCounts -Key ([string]$account.status)
      Add-Count -Table $summary.lastErrorTypes -Key ([string]$account.lastErrorType)
      if ($account.cooldownUntilMs) {
        $value = [int64]$account.cooldownUntilMs
        if (-not $summary.nearestCooldownUntilMs -or $value -lt [int64]$summary.nearestCooldownUntilMs) {
          $summary.nearestCooldownUntilMs = $value
        }
      }
    }
  } catch {
    $summary.error = $_.Exception.Message
  }
  return $summary
}

function Invoke-LocalModelsProbe {
  param([object]$Collection)
  $probe = [ordered]@{ attempted = $false }
  if (-not $Collection -or -not $Collection.enabled -or -not $Collection.apiKey -or -not $Collection.port) {
    return $probe
  }
  $probe.attempted = $true
  try {
    $response = Invoke-WebRequest `
      -Uri ("http://127.0.0.1:{0}/v1/models" -f $Collection.port) `
      -Headers @{ Authorization = "Bearer $($Collection.apiKey)" } `
      -TimeoutSec 3 `
      -UseBasicParsing
    $body = $response.Content | ConvertFrom-Json
    $probe.statusCode = [int]$response.StatusCode
    $probe.modelCount = @($body.data).Count
  } catch {
    $probe.error = $_.Exception.Message
    if ($_.Exception.Response) {
      $probe.statusCode = [int]$_.Exception.Response.StatusCode
    }
  }
  return $probe
}

if ([string]::IsNullOrWhiteSpace($DataRoot)) {
  $DataRoot = if ($env:COCKPIT_LOCAL_ACCESS_DATA_ROOT -and $env:COCKPIT_LOCAL_ACCESS_DATA_ROOT.Trim()) {
    $env:COCKPIT_LOCAL_ACCESS_DATA_ROOT.Trim()
  } else {
    Join-Path $HOME ".antigravity_cockpit"
  }
}

$nowMs = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()
$windowMs = [int64][Math]::Max(1, $AuditWindowMinutes) * 60 * 1000
$windowStartMs = $nowMs - $windowMs
$collectionPath = Join-Path $DataRoot "codex_local_access.json"
$collection = $null
if (Test-Path -LiteralPath $collectionPath) {
  $collection = Get-Content -LiteralPath $collectionPath -Raw | ConvertFrom-Json
}

$events = Read-AuditEvents -Root $DataRoot -WindowStartMs $windowStartMs -LineLimit $TailLines
$phaseCounts = @{}
$errorCounts = @{}
foreach ($event in $events) {
  Add-Count -Table $phaseCounts -Key ([string]$event.phase)
  Add-Count -Table $errorCounts -Key ([string]$event.errorType)
}
$problemEvents = @($events | Where-Object { Get-ProblemKind $_ } | Sort-Object { [int64]$_.timestamp })
$lastProblem = if ($problemEvents.Count -gt 0) { $problemEvents[-1] } else { $null }
$openLeases = Read-RecentLeaseState -Root $DataRoot -LineLimit $TailLines
$localProbe = if ($SkipLocalModelsProbe) { [ordered]@{ attempted = $false; skipped = $true } } else { Invoke-LocalModelsProbe -Collection $collection }

$recommendation = [ordered]@{
  action = "observe"
  label = "继续观察"
  reason = "Cockpit 侧未发现明确阻塞信号"
}
if (-not $collection) {
  $recommendation = [ordered]@{
    action = "configure_local_access"
    label = "配置 API 服务"
    reason = "未找到 codex_local_access.json"
  }
} elseif ($collection.enabled -ne $true) {
  $recommendation = [ordered]@{
    action = "enable_local_access"
    label = "启用 API 服务"
    reason = "API 服务集合存在但未启用"
  }
} elseif ($localProbe.attempted -and $localProbe.statusCode -ne 200) {
  $recommendation = [ordered]@{
    action = "inspect_local_service"
    label = "检查本地 API service"
    reason = "本地 /v1/models 探针未返回 200"
  }
} elseif (@($events | Where-Object { $_.phase -eq "local_backpressure" -or $_.errorType -eq "local_backpressure" }).Count -gt 0 -and [int]$collection.safetyConfig.maxConcurrentRequests -eq 1 -and [int]$collection.safetyConfig.minRequestIntervalSeconds -ge 60) {
  $recommendation = [ordered]@{
    action = "apply_balanced_self_use"
    label = "AI 推荐：切换到自用均衡"
    reason = "近窗口出现 local_backpressure，且当前为 1 并发 + 60s 启动间隔；先降到 20s，不直接提高并发"
  }
} elseif (@($events | Where-Object { Test-UpstreamLimitEvent $_ }).Count -gt 0) {
  $recommendation = [ordered]@{
    action = "inspect_health_registry"
    label = "检查账号健康和 cooldown"
    reason = "近窗口出现上游限额或容量类错误"
  }
} elseif (@($events | Where-Object { ([string]$_.phase).Contains("stream_error") -or ([string]$_.errorType).Contains("stream_error") }).Count -gt 0) {
  $recommendation = [ordered]@{
    action = "inspect_stream_errors"
    label = "检查流式错误"
    reason = "近窗口出现 stream_error 信号"
  }
}

$report = [ordered]@{
  timestamp = (Get-Date).ToString("o")
  dataRoot = $DataRoot
  collection = if ($collection) {
    [ordered]@{
      exists = $true
      enabled = [bool]$collection.enabled
      port = [int]$collection.port
      routingStrategy = $collection.routingStrategy
      accountCount = @($collection.accountIds).Count
      safetyConfig = $collection.safetyConfig
    }
  } else {
    [ordered]@{ exists = $false }
  }
  localModelsProbe = $localProbe
  auditWindow = [ordered]@{
    minutes = [Math]::Max(1, $AuditWindowMinutes)
    eventCount = $events.Count
    requestCount = @($events | Where-Object phase -eq "listener").Count
    localBackpressureCount = @($events | Where-Object { $_.phase -eq "local_backpressure" -or $_.errorType -eq "local_backpressure" }).Count
    poolWaitCount = @($events | Where-Object { $_.phase -eq "pool_wait" -or $_.errorType -eq "cockpit_pool_wait" }).Count
    upstreamLimitCount = @($events | Where-Object { Test-UpstreamLimitEvent $_ }).Count
    streamErrorCount = @($events | Where-Object { ([string]$_.phase).Contains("stream_error") -or ([string]$_.errorType).Contains("stream_error") }).Count
    phaseCounts = $phaseCounts
    errorCounts = $errorCounts
    lastProblem = if ($lastProblem) {
      [ordered]@{
        timestamp = $lastProblem.timestamp
        phase = $lastProblem.phase
        errorType = $lastProblem.errorType
        kind = Get-ProblemKind $lastProblem
        requestId = $lastProblem.requestId
        accountHash = $lastProblem.accountHash
      }
    } else {
      $null
    }
  }
  inferredOpenLeases = [ordered]@{
    count = $openLeases.Count
    leases = @($openLeases.GetEnumerator() | ForEach-Object {
      [ordered]@{ leaseId = $_.Key; value = $_.Value }
    })
  }
  healthRegistry = Get-HealthSummary -Root $DataRoot
  ports = Get-PortState -Ports @(
    4000,
    2876,
    45335,
    45336,
    $(if ($collection -and $collection.port) { [int]$collection.port } else { 0 })
  )
  recommendation = $recommendation
}

$report | ConvertTo-Json -Depth 12
