param(
  [int]$DurationSeconds = 900,
  [ValidateRange(1, 60)]
  [int]$PollIntervalSeconds = 2,
  [string]$DataRoot = (Join-Path $HOME ".antigravity_cockpit"),
  [string]$CodexHome = (Join-Path $HOME ".codex"),
  [string]$AuditPath,
  [string]$ReportDir = (Join-Path (Get-Location) "reports\local-hardened-api-live-monitor"),
  [string]$ExitCodeFile,
  [string]$CheckpointPath,
  [ValidateRange(1, 300)]
  [int]$CheckpointIntervalSeconds = 10,
  [string[]]$CodexAppProcessNames = @("Codex"),
  [string[]]$CodexAppPathIncludePatterns = @(
    "*\WindowsApps\OpenAI.Codex_*\app\Codex.exe",
    "*\OpenAI.Codex_*\app\Codex.exe",
    "*/Codex.app/Contents/MacOS/Codex"
  ),
  [string[]]$CodexAppPathExcludePatterns = @(
    "*\node_modules\@openai\codex\*",
    "*\@openai\codex\*",
    "*\vendor\*\codex\codex.exe"
  ),
  [switch]$IncludeExistingAudit,
  [long]$AuditSinceTimestampMs = 0,
  [long]$AuditUntilTimestampMs = 0,
  [string[]]$FocusGatewayRequestIds = @(),
  [switch]$RequireQuotaFallback,
  [switch]$RequireStreamCompletion,
  [switch]$RequireCliConfigUntouched,
  [switch]$RequireAppStable,
  [ValidateRange(1, 100)]
  [int]$RequiredFallbackCycles = 1,
  [ValidateRange(1, 100)]
  [int]$RequiredDistinctHealthyAccounts = 1,
  [ValidateRange(1, 100)]
  [int]$RequiredCompletedStreams = 1,
  [switch]$RequireApiServiceRuntimeAvailable,
  [string]$StopSignalFile,
  [switch]$StopWhenSatisfied,
  [switch]$WriteReport,
  [switch]$Quiet
)

$ErrorActionPreference = "Stop"

function New-MonitorResult {
  param([string]$Name)
  [ordered]@{
    name = $Name
    status = "pending"
    reason = $null
    evidence = [ordered]@{}
  }
}

function Set-MonitorStatus {
  param(
    [System.Collections.IDictionary]$Result,
    [string]$Status,
    [string]$Reason,
    [hashtable]$Evidence = @{}
  )
  $Result.status = $Status
  $Result.reason = $Reason
  foreach ($key in $Evidence.Keys) {
    $Result.evidence[$key] = $Evidence[$key]
  }
  $Result
}

function Write-MonitorExitCode {
  param(
    [string]$Path,
    [int]$Code
  )
  if ([string]::IsNullOrWhiteSpace($Path)) {
    return
  }
  $dir = Split-Path -Parent $Path
  if (-not [string]::IsNullOrWhiteSpace($dir)) {
    New-Item -ItemType Directory -Force -Path $dir | Out-Null
  }
  Set-Content -LiteralPath $Path -Encoding ASCII -Value ([string]$Code)
}

function Get-FileGuardState {
  param([string]$Root)
  $files = [ordered]@{}
  foreach ($name in @("config.toml", "auth.json")) {
    $path = Join-Path $Root $name
    if (Test-Path -LiteralPath $path) {
      $item = Get-Item -LiteralPath $path
      $hash = Get-FileHash -LiteralPath $path -Algorithm SHA256
      $files[$name] = [ordered]@{
        exists = $true
        length = $item.Length
        lastWriteTime = $item.LastWriteTime.ToString("o")
        sha256 = $hash.Hash
      }
    } else {
      $files[$name] = [ordered]@{
        exists = $false
        length = 0
        lastWriteTime = $null
        sha256 = $null
      }
    }
  }

  [ordered]@{
    root = $Root
    files = $files
  }
}

function Compare-FileGuardState {
  param(
    [System.Collections.IDictionary]$Before,
    [System.Collections.IDictionary]$After
  )
  $changed = @()
  foreach ($name in @("config.toml", "auth.json")) {
    $beforeFile = $Before.files[$name]
    $afterFile = $After.files[$name]
    if ($beforeFile.exists -ne $afterFile.exists -or $beforeFile.sha256 -ne $afterFile.sha256) {
      $changed += $name
    }
  }

  [ordered]@{
    unchanged = ($changed.Count -eq 0)
    changedFiles = @($changed)
  }
}

function Test-PathMatchesAnyPattern {
  param(
    [string]$Path,
    [string[]]$Patterns
  )
  if (-not $Path) {
    return $false
  }
  foreach ($pattern in $Patterns) {
    if ($Path -like $pattern) {
      return $true
    }
  }
  return $false
}

function Test-CodexAppProcessPath {
  param(
    [string]$Path,
    [string[]]$IncludePatterns,
    [string[]]$ExcludePatterns
  )
  if (Test-PathMatchesAnyPattern -Path $Path -Patterns $ExcludePatterns) {
    return $false
  }
  if ($IncludePatterns.Count -eq 0) {
    return $true
  }
  return (Test-PathMatchesAnyPattern -Path $Path -Patterns $IncludePatterns)
}

function Get-CodexAppProcessState {
  param(
    [string[]]$ProcessNames,
    [string[]]$PathIncludePatterns,
    [string[]]$PathExcludePatterns
  )
  $items = @()
  foreach ($name in $ProcessNames) {
    $items += @(Get-Process -Name $name -ErrorAction SilentlyContinue | ForEach-Object {
      $path = try { $_.Path } catch { $null }
      if (Test-CodexAppProcessPath -Path $path -IncludePatterns $PathIncludePatterns -ExcludePatterns $PathExcludePatterns) {
        $startTime = try { $_.StartTime.ToString("o") } catch { $null }
        [ordered]@{
          processName = $_.ProcessName
          id = $_.Id
          startTime = $startTime
          path = $path
        }
      }
    })
  }

  [ordered]@{
    processNames = @($ProcessNames)
    pathIncludePatterns = @($PathIncludePatterns)
    pathExcludePatterns = @($PathExcludePatterns)
    processes = @($items | Sort-Object processName, id)
  }
}

function Compare-CodexAppProcessState {
  param(
    [System.Collections.IDictionary]$Before,
    [System.Collections.IDictionary]$After
  )
  $beforeKeys = @($Before.processes | ForEach-Object { "$($_.processName):$($_.id):$($_.startTime)" } | Sort-Object)
  $afterKeys = @($After.processes | ForEach-Object { "$($_.processName):$($_.id):$($_.startTime)" } | Sort-Object)
  $stable = ($beforeKeys.Count -eq $afterKeys.Count)
  if ($stable) {
    for ($i = 0; $i -lt $beforeKeys.Count; $i++) {
      if ($beforeKeys[$i] -ne $afterKeys[$i]) {
        $stable = $false
        break
      }
    }
  }

  [ordered]@{
    stable = $stable
    beforeCount = $beforeKeys.Count
    afterCount = $afterKeys.Count
  }
}

function Read-MonitorJsonFile {
  param([string]$Path)
  if (-not (Test-Path -LiteralPath $Path)) {
    return [ordered]@{
      path = $Path
      exists = $false
      parseError = $null
      value = $null
    }
  }

  try {
    return [ordered]@{
      path = $Path
      exists = $true
      parseError = $null
      value = (Get-Content -LiteralPath $Path -Raw | ConvertFrom-Json)
    }
  } catch {
    return [ordered]@{
      path = $Path
      exists = $true
      parseError = $_.Exception.Message
      value = $null
    }
  }
}

function Get-ObjectPropertyValue {
  param(
    [object]$Object,
    [string]$Name
  )
  if ($null -eq $Object) {
    return $null
  }
  $property = $Object.PSObject.Properties[$Name]
  if ($null -eq $property) {
    return $null
  }
  $property.Value
}

function Get-LocalTcpListenerState {
  param([Nullable[int]]$Port)
  $listeners = @()
  $errorMessage = $null
  $command = Get-Command Get-NetTCPConnection -ErrorAction SilentlyContinue
  if ($null -eq $command) {
    return [ordered]@{
      commandAvailable = $false
      checked = $false
      error = "Get-NetTCPConnection unavailable"
      listenerCount = 0
      listening = $false
      listeners = @()
    }
  }
  if ($null -eq $Port) {
    return [ordered]@{
      commandAvailable = $true
      checked = $false
      error = "port missing"
      listenerCount = 0
      listening = $false
      listeners = @()
    }
  }

  try {
    $listeners = @(Get-NetTCPConnection -LocalPort ([int]$Port) -State Listen -ErrorAction SilentlyContinue | ForEach-Object {
      $processInfo = Get-Process -Id $_.OwningProcess -ErrorAction SilentlyContinue
      [ordered]@{
        localAddress = $_.LocalAddress
        localPort = $_.LocalPort
        owningProcess = $_.OwningProcess
        processName = if ($processInfo) { $processInfo.ProcessName } else { $null }
        path = if ($processInfo) { try { $processInfo.Path } catch { $null } } else { $null }
      }
    })
  } catch {
    $errorMessage = $_.Exception.Message
  }

  [ordered]@{
    commandAvailable = $true
    checked = $true
    error = $errorMessage
    listenerCount = $listeners.Count
    listening = ($listeners.Count -gt 0)
    listeners = @($listeners)
  }
}

function Get-ApiServiceRuntimeState {
  $localAccessPath = Join-Path $DataRoot "codex_local_access.json"
  $runtimeModePath = Join-Path $DataRoot "codex_runtime_mode.json"
  $serverPath = Join-Path $DataRoot "server.json"
  $localAccessFile = Read-MonitorJsonFile $localAccessPath
  $runtimeModeFile = Read-MonitorJsonFile $runtimeModePath
  $serverFile = Read-MonitorJsonFile $serverPath

  $localAccess = $localAccessFile.value
  $runtimeMode = $runtimeModeFile.value
  $server = $serverFile.value
  $enabled = Get-ObjectPropertyValue $localAccess "enabled"
  $rawPort = Get-ObjectPropertyValue $localAccess "port"
  $port = $null
  if ($null -ne $rawPort) {
    try {
      $port = [int]$rawPort
    } catch {
      $port = $null
    }
  }
  $listenerState = Get-LocalTcpListenerState $port
  $reason = "available"
  if (-not $localAccessFile.exists) {
    $reason = "local_access_config_missing"
  } elseif ($localAccessFile.parseError) {
    $reason = "local_access_config_parse_error"
  } elseif ($enabled -ne $true) {
    $reason = "local_access_disabled"
  } elseif ($null -eq $port) {
    $reason = "local_access_port_missing"
  } elseif (-not $listenerState.commandAvailable) {
    $reason = "listener_check_unavailable"
  } elseif (-not $listenerState.listening) {
    $reason = "port_not_listening"
  }

  [ordered]@{
    available = ($reason -eq "available")
    reason = $reason
    apiBaseUrl = if ($null -ne $port) { "http://127.0.0.1:$port/v1" } else { $null }
    localAccess = [ordered]@{
      path = $localAccessPath
      exists = [bool]$localAccessFile.exists
      parseError = $localAccessFile.parseError
      enabled = $enabled
      port = $port
      updatedAt = Get-ObjectPropertyValue $localAccess "updatedAt"
      fallbackMode = Get-ObjectPropertyValue (Get-ObjectPropertyValue $localAccess "safetyConfig") "fallbackMode"
      accountCount = @((Get-ObjectPropertyValue $localAccess "accountIds")).Count
    }
    runtimeMode = [ordered]@{
      path = $runtimeModePath
      exists = [bool]$runtimeModeFile.exists
      parseError = $runtimeModeFile.parseError
      mode = Get-ObjectPropertyValue $runtimeMode "mode"
      accountKind = Get-ObjectPropertyValue $runtimeMode "accountKind"
      updatedAt = Get-ObjectPropertyValue $runtimeMode "updatedAt"
    }
    server = [ordered]@{
      path = $serverPath
      exists = [bool]$serverFile.exists
      parseError = $serverFile.parseError
      pid = Get-ObjectPropertyValue $server "pid"
      wsPort = Get-ObjectPropertyValue $server "ws_port"
      version = Get-ObjectPropertyValue $server "version"
      startedAt = Get-ObjectPropertyValue $server "started_at"
    }
    listener = $listenerState
  }
}

function Get-InitialAuditOffset {
  param([string]$Path)
  if (-not (Test-Path -LiteralPath $Path)) {
    return [int64]0
  }
  if ($IncludeExistingAudit) {
    return [int64]0
  }
  return [int64](Get-Item -LiteralPath $Path).Length
}

function Read-NewAuditLines {
  param(
    [string]$Path,
    [ref]$Offset
  )
  if (-not (Test-Path -LiteralPath $Path)) {
    return @()
  }

  $stream = [System.IO.File]::Open($Path, [System.IO.FileMode]::Open, [System.IO.FileAccess]::Read, [System.IO.FileShare]::ReadWrite)
  try {
    if ($Offset.Value -gt $stream.Length) {
      $Offset.Value = [int64]0
    }
    if ($Offset.Value -eq $stream.Length) {
      return @()
    }

    [void]$stream.Seek($Offset.Value, [System.IO.SeekOrigin]::Begin)
    $byteCount = [int]($stream.Length - $Offset.Value)
    $bytes = New-Object byte[] $byteCount
    $read = $stream.Read($bytes, 0, $byteCount)
    if ($read -le 0) {
      return @()
    }

    $text = [System.Text.Encoding]::UTF8.GetString($bytes, 0, $read)
    $lastNewline = $text.LastIndexOf("`n")
    if ($lastNewline -lt 0) {
      return @()
    }

    $completeText = $text.Substring(0, $lastNewline + 1)
    $Offset.Value = $Offset.Value + [System.Text.Encoding]::UTF8.GetByteCount($completeText)
    @($completeText -split "`r?`n" | Where-Object { $_.Trim() })
  } finally {
    $stream.Dispose()
  }
}

function Convert-AuditLine {
  param([string]$Line)
  try {
    $event = $Line | ConvertFrom-Json
    return [ordered]@{
      rawLine = $Line
      parsed = $true
      timestamp = $event.timestamp
      requestId = [string]$event.requestId
      phase = [string]$event.phase
      route = [string]$event.route
      model = [string]$event.model
      accountHash = [string]$event.accountHash
      status = if ($null -ne $event.status) { [int]$event.status } else { $null }
      errorType = if ($null -ne $event.errorType) { [string]$event.errorType } else { $null }
      outcome = if ($null -ne $event.outcome) { [string]$event.outcome } else { $null }
      streamState = if ($null -ne $event.streamState) { [string]$event.streamState } else { $null }
      detail = $event.detail
    }
  } catch {
    return [ordered]@{
      rawLine = $Line
      parsed = $false
      parseError = $_.Exception.Message
    }
  }
}

function Test-UsageLimitEvent {
  param([System.Collections.IDictionary]$Event)
  if ($Event.errorType -eq "usage_limit_reached") {
    return $true
  }
  $detail = $Event.detail
  if ($detail -and $detail.PSObject.Properties["provider_code"] -and [string]$detail.provider_code -eq "usage_limit_reached") {
    return $true
  }
  $false
}

function Test-ValidAccountHash {
  param([string]$AccountHash)
  -not [string]::IsNullOrWhiteSpace($AccountHash) -and $AccountHash -ne "-"
}

function Test-LocalPoolUnavailableEvent {
  param([System.Collections.IDictionary]$Event)
  if ($Event.errorType -eq "pool_unavailable") {
    return $true
  }
  if ($Event.rawLine -match 'pool_unavailable|API 服务号池|API 服务账号均在冷却|可用账号均在冷却|本地接入集合暂无可用账号') {
    return $true
  }
  $detail = $Event.detail
  if ($detail -and $detail.PSObject.Properties["message"]) {
    $message = [string]$detail.message
    if ($message -match 'API 服务号池|API 服务账号均在冷却|可用账号均在冷却|本地接入集合暂无可用账号') {
      return $true
    }
  }
  $false
}

function Test-RouteIsCodexResponses {
  param([string]$Route)
  $route = [string]$Route
  $route -eq "/v1/responses" -or $route -eq "/responses" -or $route.EndsWith("/responses")
}

function Test-CodexResponsesRoute {
  param([System.Collections.IDictionary]$Event)
  Test-RouteIsCodexResponses ([string]$Event.route)
}

function Test-CodexFacingResponsesRoute {
  param(
    [System.Collections.IDictionary]$Event,
    [hashtable]$GatewayClientRoutes
  )
  if (-not (Test-CodexResponsesRoute $Event)) {
    return $false
  }

  $gatewayRequestId = Get-AuditGatewayRequestId $Event
  if ($gatewayRequestId -and $GatewayClientRoutes.ContainsKey($gatewayRequestId)) {
    return Test-RouteIsCodexResponses ([string]$GatewayClientRoutes[$gatewayRequestId])
  }

  $true
}

function Test-AuditStreamTerminalCompleted {
  param([System.Collections.IDictionary]$Event)
  if ($Event.phase -eq "stream_completed") {
    return $true
  }
  if ($Event.phase -eq "lease_released" -and $Event.outcome -eq "completed") {
    return $true
  }
  if ($Event.phase -ne "stream_terminal") {
    return $false
  }
  if ($Event.outcome -eq "completed" -or $Event.streamState -eq "completed") {
    return $true
  }
  Get-AuditDetailBool -Event $Event -Name "response_completed_seen"
}

function Get-AuditDetailValue {
  param(
    [System.Collections.IDictionary]$Event,
    [string]$Name
  )
  $detail = $Event.detail
  if (-not $detail) {
    return $null
  }
  if ($detail -is [System.Collections.IDictionary] -and $detail.Contains($Name)) {
    return [string]$detail[$Name]
  }
  $property = $detail.PSObject.Properties[$Name]
  if ($property -and $null -ne $property.Value) {
    return [string]$property.Value
  }
  $null
}

function Get-AuditDetailBool {
  param(
    [System.Collections.IDictionary]$Event,
    [string]$Name
  )
  $value = Get-AuditDetailValue -Event $Event -Name $Name
  if ([string]::IsNullOrWhiteSpace($value)) {
    return $false
  }
  $value -match '^(?i:true|1|yes)$'
}

function Get-AuditDetailInt64 {
  param(
    [System.Collections.IDictionary]$Event,
    [string]$Name
  )
  $value = Get-AuditDetailValue -Event $Event -Name $Name
  if ([string]::IsNullOrWhiteSpace($value)) {
    return $null
  }
  $parsed = 0L
  if ([int64]::TryParse($value, [ref]$parsed)) {
    return $parsed
  }
  $null
}

function Get-AuditDetailFirstValue {
  param(
    [System.Collections.IDictionary]$Event,
    [string[]]$Names
  )
  foreach ($name in $Names) {
    $value = Get-AuditDetailValue -Event $Event -Name $name
    if (-not [string]::IsNullOrWhiteSpace($value) -and $value -ne "-") {
      return $value
    }
  }
  $null
}

function Select-DistinctAuditDetailValues {
  param(
    [object[]]$Events,
    [string[]]$Names,
    [int]$Limit = 20
  )
  $values = @()
  foreach ($event in $Events) {
    $value = Get-AuditDetailFirstValue -Event $event -Names $Names
    if (-not [string]::IsNullOrWhiteSpace($value) -and $value -ne "-") {
      $values += $value
    }
  }
  @($values | Sort-Object -Unique | Select-Object -First $Limit)
}

function New-QuotaMetadataCoverage {
  param([object[]]$ParsedEvents)

  $fieldAliases = [ordered]@{
    plan_type = @("plan_type", "planType")
    provider_plan_type = @("provider_plan_type", "providerPlanType")
    reset_at = @("reset_at", "resets_at", "resetAt")
    reset_after_seconds = @("reset_after_seconds", "resets_in_seconds", "resetAfterSeconds")
    retry_after_ms = @("retry_after_ms", "retryAfterMs")
    active_limit = @("active_limit", "activeLimit")
    rate_limit_reached_type = @("rate_limit_reached_type", "rateLimitReachedType")
    promo_message_present = @("promo_message_present", "promoMessagePresent")
  }
  $fieldEventCounts = [ordered]@{}
  $presentFieldNames = @()
  foreach ($fieldName in $fieldAliases.Keys) {
    $count = @($ParsedEvents | Where-Object {
      $value = Get-AuditDetailFirstValue -Event $_ -Names $fieldAliases[$fieldName]
      -not [string]::IsNullOrWhiteSpace($value) -and $value -ne "-"
    }).Count
    $fieldEventCounts[$fieldName] = [int]$count
    if ($count -gt 0) {
      $presentFieldNames += $fieldName
    }
  }

  $metadataEvents = @($ParsedEvents | Where-Object {
    $hasMetadata = $false
    foreach ($fieldName in $fieldAliases.Keys) {
      $value = Get-AuditDetailFirstValue -Event $_ -Names $fieldAliases[$fieldName]
      if (-not [string]::IsNullOrWhiteSpace($value) -and $value -ne "-") {
        $hasMetadata = $true
        break
      }
    }
    $hasMetadata
  })

  [ordered]@{
    metadataEventCount = [int]$metadataEvents.Count
    fieldEventCounts = $fieldEventCounts
    presentFieldNames = @($presentFieldNames)
    missingFieldNames = @($fieldAliases.Keys | Where-Object { $_ -notin $presentFieldNames })
    planTypes = @(Select-DistinctAuditDetailValues -Events $ParsedEvents -Names $fieldAliases["plan_type"])
    providerPlanTypes = @(Select-DistinctAuditDetailValues -Events $ParsedEvents -Names $fieldAliases["provider_plan_type"])
    resetAtValues = @(Select-DistinctAuditDetailValues -Events $ParsedEvents -Names $fieldAliases["reset_at"])
    resetAfterSecondsValues = @(Select-DistinctAuditDetailValues -Events $ParsedEvents -Names $fieldAliases["reset_after_seconds"])
    retryAfterMsValues = @(Select-DistinctAuditDetailValues -Events $ParsedEvents -Names $fieldAliases["retry_after_ms"])
    activeLimits = @(Select-DistinctAuditDetailValues -Events $ParsedEvents -Names $fieldAliases["active_limit"])
    rateLimitReachedTypes = @(Select-DistinctAuditDetailValues -Events $ParsedEvents -Names $fieldAliases["rate_limit_reached_type"])
    promoMessagePresentCount = [int]$fieldEventCounts["promo_message_present"]
    hasPlanMetadata = (($fieldEventCounts["plan_type"] + $fieldEventCounts["provider_plan_type"]) -gt 0)
    hasResetMetadata = (($fieldEventCounts["reset_at"] + $fieldEventCounts["reset_after_seconds"] + $fieldEventCounts["retry_after_ms"]) -gt 0)
    hasLimitMetadata = (($fieldEventCounts["active_limit"] + $fieldEventCounts["rate_limit_reached_type"]) -gt 0)
  }
}

function Get-AuditEventTimestampMs {
  param([System.Collections.IDictionary]$Event)
  if ($null -eq $Event.timestamp) {
    return $null
  }
  try {
    return [int64]$Event.timestamp
  } catch {
    return $null
  }
}

function Get-AuditLeaseId {
  param([System.Collections.IDictionary]$Event)
  foreach ($name in @("lease_id", "leaseId")) {
    $value = Get-AuditDetailValue -Event $Event -Name $name
    if (-not [string]::IsNullOrWhiteSpace($value) -and $value -ne "-") {
      return $value
    }
  }
  $null
}

function Get-AuditAdmissionAttempt {
  param([System.Collections.IDictionary]$Event)
  foreach ($name in @("admission_attempt", "admissionAttempt", "attempt")) {
    $value = Get-AuditDetailValue -Event $Event -Name $name
    if (-not [string]::IsNullOrWhiteSpace($value) -and $value -ne "-") {
      return $value
    }
  }
  $null
}

function Get-AuditLineageId {
  param([System.Collections.IDictionary]$Event)
  $explicit = Get-AuditDetailValue -Event $Event -Name "turn_lineage_id"
  if (-not [string]::IsNullOrWhiteSpace($explicit) -and $explicit -ne "-") {
    return $explicit
  }

  $requestId = [string]$Event.requestId
  $source = Get-AuditDetailValue -Event $Event -Name "request_id_source"
  if ($source -in @("codex_turn_state", "codex_turn_metadata", "codex_turn_metadata_turn_id")) {
    if (-not [string]::IsNullOrWhiteSpace($requestId) -and $requestId -ne "-") {
      return $requestId
    }
  }
  if ($requestId -match '^(x-codex-turn-state|x-codex-turn-metadata|x-codex-turn-metadata\.turn_id|turn):sha256:') {
    return $requestId
  }
  $null
}

function Get-AuditGatewayRequestId {
  param([System.Collections.IDictionary]$Event)
  $gatewayRequestId = Get-AuditDetailValue -Event $Event -Name "gateway_request_id"
  if (-not [string]::IsNullOrWhiteSpace($gatewayRequestId) -and $gatewayRequestId -ne "-") {
    return $gatewayRequestId
  }
  $requestId = [string]$Event.requestId
  if (-not [string]::IsNullOrWhiteSpace($requestId) -and $requestId -ne "-") {
    return $requestId
  }
  $null
}

function Test-AuditEventInWindow {
  param([System.Collections.IDictionary]$Event)
  if ($AuditSinceTimestampMs -gt 0 -or $AuditUntilTimestampMs -gt 0) {
    if ($null -eq $Event.timestamp) {
      return $false
    }
    try {
      $eventTimestamp = [int64]$Event.timestamp
    } catch {
      return $false
    }
    if ($AuditSinceTimestampMs -gt 0 -and $eventTimestamp -lt $AuditSinceTimestampMs) {
      return $false
    }
    if ($AuditUntilTimestampMs -gt 0 -and $eventTimestamp -gt $AuditUntilTimestampMs) {
      return $false
    }
  }

  $focusIds = @($FocusGatewayRequestIds | Where-Object { -not [string]::IsNullOrWhiteSpace($_) })
  if ($focusIds.Count -gt 0) {
    $gatewayRequestId = Get-AuditGatewayRequestId $Event
    if ([string]::IsNullOrWhiteSpace($gatewayRequestId) -or $gatewayRequestId -notin $focusIds) {
      return $false
    }
  }

  $true
}

function Select-AuditWindowEvents {
  param([object[]]$Events)
  @($Events | Where-Object { Test-AuditEventInWindow $_ })
}

function Get-AuditPreviousResponseIdHash {
  param([System.Collections.IDictionary]$Event)
  foreach ($name in @("previous_response_id_hash", "previousResponseIdHash")) {
    $value = Get-AuditDetailValue -Event $Event -Name $name
    if (-not [string]::IsNullOrWhiteSpace($value) -and $value -ne "-") {
      return $value
    }
  }
  $null
}

function Get-AuditUpstreamResponseIdHash {
  param([System.Collections.IDictionary]$Event)
  foreach ($name in @("upstream_response_id_hash", "response_id_hash", "upstreamResponseIdHash", "responseIdHash")) {
    $value = Get-AuditDetailValue -Event $Event -Name $name
    if (-not [string]::IsNullOrWhiteSpace($value) -and $value -ne "-") {
      return $value
    }
  }
  $null
}

function Get-AuditLineageSource {
  param([System.Collections.IDictionary]$Event)
  foreach ($name in @("turn_lineage_source", "request_id_source")) {
    $value = Get-AuditDetailValue -Event $Event -Name $name
    if (-not [string]::IsNullOrWhiteSpace($value) -and $value -ne "-") {
      return $value
    }
  }

  $requestId = [string]$Event.requestId
  if ($requestId -match '^x-codex-turn-state:sha256:') {
    return "codex_turn_state"
  }
  if ($requestId -match '^x-codex-turn-metadata\.turn_id:sha256:') {
    return "codex_turn_metadata_turn_id"
  }
  if ($requestId -match '^x-codex-turn-metadata:sha256:') {
    return "codex_turn_metadata"
  }
  if (-not [string]::IsNullOrWhiteSpace((Get-AuditPreviousResponseIdHash $Event))) {
    return "previous_response_id"
  }

  $null
}

function Test-HardAffinityLineageSource {
  param([string]$Source)
  $Source -in @("codex_turn_state", "previous_response_id")
}

function Test-MetadataLineageSource {
  param([string]$Source)
  $Source -in @("codex_turn_metadata", "codex_turn_metadata_turn_id")
}

function New-RequestTimelines {
  param([object[]]$ParsedEvents)

  $groups = [ordered]@{}
  foreach ($event in $ParsedEvents) {
    $requestId = [string]$event.requestId
    if ([string]::IsNullOrWhiteSpace($requestId)) {
      $requestId = "-"
    }
    $gatewayRequestId = Get-AuditGatewayRequestId $event
    $timelineKey = if (-not [string]::IsNullOrWhiteSpace($gatewayRequestId)) {
      "gateway:{0}" -f $gatewayRequestId
    } else {
      "request:{0}" -f $requestId
    }
    if (-not $groups.Contains($timelineKey)) {
      $groups[$timelineKey] = [ordered]@{
        timelineKey = $timelineKey
        requestId = $requestId
        firstTimestamp = $event.timestamp
        lastTimestamp = $event.timestamp
        eventCount = 0
        gatewayRequestIds = @()
        leaseIds = @()
        accountHashes = @()
        phases = @()
        statuses = @()
        admissionAttempts = @()
        turnLineageIds = @()
        turnLineageSources = @()
        previousResponseIdHashes = @()
        upstreamResponseIdHashes = @()
        terminalOrigins = @()
        hasContinuation = $false
        hasAutoCompactCandidate = $false
        has429 = $false
        usageLimitReachedCount = 0
        modelCooldownCount = 0
        fallbackSelectedCount = 0
        fallbackBlockedCount = 0
        hardAffinityBlockedCount = 0
        upstreamForward200Count = 0
        upstreamForward429Count = 0
        final200Count = 0
        final429Count = 0
        terminalUpstreamCompletionCount = 0
        localCompletionCount = 0
        clientAbortedCount = 0
        poolWaitCount = 0
        streamWriteCount = 0
        streamCompletedCount = 0
        leaseGrantedCount = 0
        leaseReleasedCount = 0
        keyEvents = @()
      }
    }

    $group = $groups[$timelineKey]
    $group.eventCount++
    $group.lastTimestamp = $event.timestamp
    if ($requestId -ne "-" -and $group.requestId -eq "-") {
      $group.requestId = $requestId
    }
    if ($gatewayRequestId) {
      $group.gatewayRequestIds += $gatewayRequestId
    }
    $leaseId = Get-AuditLeaseId $event
    if ($leaseId) {
      $group.leaseIds += $leaseId
    }
    if (Test-ValidAccountHash $event.accountHash) {
      $group.accountHashes += $event.accountHash
    }
    if ($event.phase) {
      $group.phases += $event.phase
    }
    if ($null -ne $event.status) {
      $group.statuses += [string]$event.status
      if ($event.status -eq 429) {
        $group.has429 = $true
      }
    }
    $admissionAttempt = Get-AuditAdmissionAttempt $event
    if ($admissionAttempt) {
      $group.admissionAttempts += $admissionAttempt
    }
    $lineageId = Get-AuditLineageId $event
    if ($lineageId) {
      $group.turnLineageIds += $lineageId
    }
    $lineageSource = Get-AuditLineageSource $event
    if ($lineageSource) {
      $group.turnLineageSources += $lineageSource
    }
    $previousResponseIdHash = Get-AuditPreviousResponseIdHash $event
    if ($previousResponseIdHash) {
      $group.previousResponseIdHashes += $previousResponseIdHash
    }
    $upstreamResponseIdHash = Get-AuditUpstreamResponseIdHash $event
    if ($upstreamResponseIdHash) {
      $group.upstreamResponseIdHashes += $upstreamResponseIdHash
    }
    $terminalOrigin = Get-AuditDetailValue -Event $event -Name "terminal_origin"
    if ($terminalOrigin) {
      $group.terminalOrigins += $terminalOrigin
    }
    if (Get-AuditDetailBool -Event $event -Name "is_continuation") {
      $group.hasContinuation = $true
    }
    if (Get-AuditDetailBool -Event $event -Name "is_auto_compact_candidate") {
      $group.hasAutoCompactCandidate = $true
    }
    if (Test-UsageLimitEvent $event) {
      $group.usageLimitReachedCount++
    }
    switch ($event.phase) {
      "model_cooldown_applied" { $group.modelCooldownCount++ }
      "fallback_selected" { $group.fallbackSelectedCount++ }
      "fallback_blocked" {
        $group.fallbackBlockedCount++
        if ($event.outcome -eq "hard_affinity") {
          $group.hardAffinityBlockedCount++
        }
      }
      "pool_wait" { $group.poolWaitCount++ }
      "stream_write" { $group.streamWriteCount++ }
      "stream_completed" {
        $group.streamCompletedCount++
        $group.terminalUpstreamCompletionCount++
      }
      "stream_terminal" {
        if (Test-AuditStreamTerminalCompleted $event) {
          $group.streamCompletedCount++
          $group.terminalUpstreamCompletionCount++
        }
      }
      "client_aborted" { $group.clientAbortedCount++ }
      "lease_granted" { $group.leaseGrantedCount++ }
      "lease_released" {
        $group.leaseReleasedCount++
        if ($event.outcome -eq "completed") {
          $group.terminalUpstreamCompletionCount++
        }
        if ($event.outcome -eq "client_aborted") {
          $group.clientAbortedCount++
        }
      }
      "upstream_forward" {
        if ($event.status -eq 200) {
          $group.upstreamForward200Count++
        } elseif ($event.status -eq 429) {
          $group.upstreamForward429Count++
        }
      }
      "final_response" {
        if ($event.status -eq 200) {
          $group.final200Count++
          if (Test-LocalPoolUnavailableEvent $event) {
            $group.localCompletionCount++
          } else {
            $group.terminalUpstreamCompletionCount++
          }
        } elseif ($event.status -eq 429) {
          $group.final429Count++
        }
      }
    }

    $group.keyEvents += [ordered]@{
      timestamp = $event.timestamp
      phase = $event.phase
      accountHash = $event.accountHash
      status = $event.status
      errorType = $event.errorType
      outcome = $event.outcome
      streamState = $event.streamState
      gatewayRequestId = $gatewayRequestId
      leaseId = $leaseId
      admissionAttempt = $admissionAttempt
      turnLineageId = $lineageId
      previousResponseIdHash = $previousResponseIdHash
      upstreamResponseIdHash = $upstreamResponseIdHash
      terminalOrigin = $terminalOrigin
    }
  }

  @($groups.Values | ForEach-Object {
    $accountHashes = @($_.accountHashes | Where-Object { Test-ValidAccountHash $_ } | Select-Object -Unique)
    $classification = "observed"
    if ($_.hardAffinityBlockedCount -gt 0 -and $_.final429Count -gt 0) {
      $classification = "hard_affinity_terminal_429"
    } elseif ($_.hardAffinityBlockedCount -gt 0 -and $_.localCompletionCount -gt 0) {
      $classification = "hard_affinity_local_completion"
    } elseif ($_.hardAffinityBlockedCount -gt 0 -and $_.terminalUpstreamCompletionCount -gt 0) {
      $classification = "hard_affinity_completed_on_original_account"
    } elseif ($_.hasContinuation -and $accountHashes.Count -gt 1) {
      $classification = "continuation_account_switch"
    } elseif ($_.clientAbortedCount -gt 0 -and $_.streamWriteCount -gt 0) {
      $classification = "client_aborted_after_stream_started"
    } elseif ($_.clientAbortedCount -gt 0) {
      $classification = "client_aborted_before_stream_body"
    } elseif ($_.poolWaitCount -gt 0 -and $_.terminalUpstreamCompletionCount -gt 0) {
      $classification = "pool_wait_recovered_completed"
    } elseif ($_.localCompletionCount -gt 0) {
      $classification = "pool_unavailable_local_completion"
    } elseif ($_.final429Count -gt 0) {
      $classification = "terminal_429"
    } elseif ($_.terminalUpstreamCompletionCount -gt 0 -and ($_.upstreamForward200Count -gt 0 -or $_.leaseGrantedCount -gt 0 -or $_.streamWriteCount -gt 0)) {
      $classification = "independent_request_completed"
    }

    [ordered]@{
      timelineKey = $_.timelineKey
      requestId = $_.requestId
      firstTimestamp = $_.firstTimestamp
      lastTimestamp = $_.lastTimestamp
      eventCount = [int]$_.eventCount
      classification = $classification
      gatewayRequestIds = @($_.gatewayRequestIds | Select-Object -Unique)
      leaseIds = @($_.leaseIds | Select-Object -Unique)
      accountHashes = @($accountHashes)
      phases = @($_.phases | Select-Object -Unique)
      statuses = @($_.statuses | Select-Object -Unique)
      admissionAttempts = @($_.admissionAttempts | Select-Object -Unique)
      turnLineageIds = @($_.turnLineageIds | Select-Object -Unique)
      turnLineageSources = @($_.turnLineageSources | Select-Object -Unique)
      previousResponseIdHashes = @($_.previousResponseIdHashes | Select-Object -Unique)
      upstreamResponseIdHashes = @($_.upstreamResponseIdHashes | Select-Object -Unique)
      terminalOrigins = @($_.terminalOrigins | Select-Object -Unique)
      hasContinuation = [bool]$_.hasContinuation
      hasAutoCompactCandidate = [bool]$_.hasAutoCompactCandidate
      has429 = [bool]$_.has429
      usageLimitReachedCount = [int]$_.usageLimitReachedCount
      modelCooldownCount = [int]$_.modelCooldownCount
      fallbackSelectedCount = [int]$_.fallbackSelectedCount
      fallbackBlockedCount = [int]$_.fallbackBlockedCount
      hardAffinityBlockedCount = [int]$_.hardAffinityBlockedCount
      upstreamForward200Count = [int]$_.upstreamForward200Count
      upstreamForward429Count = [int]$_.upstreamForward429Count
      final200Count = [int]$_.final200Count
      final429Count = [int]$_.final429Count
      terminalUpstreamCompletionCount = [int]$_.terminalUpstreamCompletionCount
      localCompletionCount = [int]$_.localCompletionCount
      clientAbortedCount = [int]$_.clientAbortedCount
      poolWaitCount = [int]$_.poolWaitCount
      streamWriteCount = [int]$_.streamWriteCount
      streamCompletedCount = [int]$_.streamCompletedCount
      leaseGrantedCount = [int]$_.leaseGrantedCount
      leaseReleasedCount = [int]$_.leaseReleasedCount
      keyEvents = @($_.keyEvents)
    }
  } | Sort-Object firstTimestamp, timelineKey)
}

function Format-AuditRealtimeEventLine {
  param([System.Collections.IDictionary]$Event)
  if (-not $Event.parsed) {
    return "event=parse_error"
  }
  $gatewayRequestId = Get-AuditGatewayRequestId $Event
  $leaseId = Get-AuditLeaseId $Event
  $admissionAttempt = Get-AuditAdmissionAttempt $Event
  $parts = @(
    "event={0}" -f $Event.phase,
    "requestId={0}" -f $Event.requestId,
    "gatewayRequestId={0}" -f $(if ($gatewayRequestId) { $gatewayRequestId } else { "-" }),
    "leaseId={0}" -f $(if ($leaseId) { $leaseId } else { "-" }),
    "accountHash={0}" -f $Event.accountHash,
    "status={0}" -f $(if ($null -ne $Event.status) { $Event.status } else { "-" }),
    "errorType={0}" -f $(if ($Event.errorType) { $Event.errorType } else { "-" }),
    "outcome={0}" -f $(if ($Event.outcome) { $Event.outcome } else { "-" }),
    "streamState={0}" -f $(if ($Event.streamState) { $Event.streamState } else { "-" }),
    "admissionAttempt={0}" -f $(if ($admissionAttempt) { $admissionAttempt } else { "-" })
  )
  $parts -join "; "
}

function Get-AuditAcceptanceSummary {
  param([object[]]$Events)
  $parsedEvents = @($Events | Where-Object { $_.parsed })
  $gatewayClientRoutes = @{}
  foreach ($event in $parsedEvents) {
    $gatewayRequestId = Get-AuditGatewayRequestId $event
    if (-not $gatewayRequestId) {
      continue
    }
    $clientRoute = Get-AuditDetailValue -Event $event -Name "client_route"
    if ($clientRoute) {
      $gatewayClientRoutes[$gatewayRequestId] = $clientRoute
    }
  }
  $retryLimitEvents = @($Events | Where-Object {
    ($_.rawLine -match 'exceeded retry limit|last status:\s*429|429 Too Many Requests') -and -not (Test-LocalPoolUnavailableEvent $_)
  })
  $poolWaitEvents = @($parsedEvents | Where-Object { $_.phase -eq "pool_wait" })
  $parkedPoolWaitEvents = @($poolWaitEvents | Where-Object {
    $_.outcome -eq "parked" -or $_.rawLine -match 'pool_unavailable_stream_park'
  })
  $heartbeatPoolWaitEvents = @($poolWaitEvents | Where-Object {
    $_.streamState -eq "heartbeat" -or $_.rawLine -match 'cockpit_pool_wait'
  })
  $activeDrainPoolWaitEvents = @($poolWaitEvents | Where-Object {
    $_.streamState -eq "admission_blocked" -or $_.outcome -match '^active_stream'
  })
  $sseIdleEvents = @($Events | Where-Object {
    $_.rawLine -match 'stream disconnected before completion:\s*idle timeout waiting for SSE|idle timeout waiting for SSE|stream disconnected before completion:\s*Cockpit API Service pool_unavailable'
  })
  $localPoolUnavailableEvents = @($parsedEvents | Where-Object { (Test-LocalPoolUnavailableEvent $_) -and $_.phase -ne "pool_wait" })
  $inBandSyntheticPoolUnavailableEvents = @($localPoolUnavailableEvents | Where-Object {
    (Test-CodexFacingResponsesRoute $_ $gatewayClientRoutes) -and $_.status -eq 200 -and $_.outcome -eq "in_band_synthetic"
  })
  $responsesFailedPoolUnavailableEvents = @($localPoolUnavailableEvents | Where-Object {
    (Test-CodexFacingResponsesRoute $_ $gatewayClientRoutes) -and $_.status -eq 200 -and ($_.streamState -eq "failed" -or $_.outcome -eq "pool_unavailable_after_active_stream_drain" -or $_.rawLine -match 'response\.failed')
  })
  $responsesLocalCompletionPoolUnavailableEvents = @($localPoolUnavailableEvents | Where-Object {
    (Test-CodexFacingResponsesRoute $_ $gatewayClientRoutes) -and $_.status -eq 200 -and ($_.streamState -eq "completed" -or $_.outcome -eq "in_band_local_completion" -or $_.rawLine -match 'response\.completed')
  })
  $responsesTransport503PoolUnavailableEvents = @($localPoolUnavailableEvents | Where-Object {
    (Test-CodexFacingResponsesRoute $_ $gatewayClientRoutes) -and $_.status -eq 503 -and $_.outcome -ne "in_band_synthetic"
  })
  $responsesTransport503TextEvents = @($Events | Where-Object {
    $_.rawLine -match 'unexpected status 503 Service Unavailable' -and ($_.rawLine -match '/v1/responses|/responses|pool_unavailable|API 服务号池')
  })

  $first429Index = -1
  $firstFallbackIndex = -1
  $first429AccountHash = $null
  $firstFallbackAccountHash = $null
  $firstBlockedAccountIndex = -1
  $firstBlockedAccountHash = $null
  $blockedAccountHashes = @()
  $blockedAccountRecords = @()
  for ($i = 0; $i -lt $parsedEvents.Count; $i++) {
    $event = $parsedEvents[$i]
    if ($first429Index -lt 0 -and $event.status -eq 429) {
      $first429Index = $i
      $first429AccountHash = $event.accountHash
    }
    if ($firstFallbackIndex -lt 0 -and $event.phase -eq "fallback_selected") {
      $firstFallbackIndex = $i
      $firstFallbackAccountHash = $event.accountHash
    }
    $isUsageLimitEvent = Test-UsageLimitEvent $event
    $isBlockingAccountEvent = (Test-ValidAccountHash $event.accountHash) -and (
      ($event.status -eq 429 -and $isUsageLimitEvent) -or
      $event.phase -eq "model_cooldown_applied" -or
      (($event.phase -eq "fallback_selected" -or $event.phase -eq "fallback_blocked") -and $isUsageLimitEvent)
    )
    if ($isBlockingAccountEvent) {
      if ($firstBlockedAccountIndex -lt 0) {
        $firstBlockedAccountIndex = $i
        $firstBlockedAccountHash = $event.accountHash
      }
      $blockedAccountHashes += $event.accountHash
      $blockedAccountRecords += [ordered]@{
        index = $i
        accountHash = $event.accountHash
        requestId = $event.requestId
        phase = $event.phase
      }
    }
  }
  $blockedAccountHashes = @($blockedAccountHashes | Sort-Object -Unique)

  $hasDifferentAccount200AfterFallback = $false
  $fallbackTransitions = @()
  $sameTaskAffinityBlockTransitions = @()
  $healthyAccountHashesAfterFallback = @()
  $unrecoveredFallback429Events = @()
  for ($i = 0; $i -lt $parsedEvents.Count; $i++) {
    $event = $parsedEvents[$i]
    if ($firstFallbackIndex -ge 0 -and $i -gt $firstFallbackIndex -and $event.status -eq 200) {
      if ($event.accountHash -and $event.accountHash -ne "-" -and $event.accountHash -ne $firstFallbackAccountHash) {
        $hasDifferentAccount200AfterFallback = $true
      }
    }

    if ($event.phase -eq "fallback_selected") {
      $next200 = $null
      $terminal429 = $null
      for ($j = $i + 1; $j -lt $parsedEvents.Count; $j++) {
        $candidate = $parsedEvents[$j]
        if ($candidate.requestId -ne $event.requestId) {
          continue
        }
        if ($candidate.phase -eq "listener") {
          break
        }
        if ($candidate.status -eq 200 -and (Test-ValidAccountHash $candidate.accountHash)) {
          $next200 = $candidate
          break
        }
        if ($candidate.phase -eq "final_response" -and $candidate.status -eq 429 -and -not (Test-LocalPoolUnavailableEvent $candidate)) {
          $terminal429 = $candidate
          break
        }
      }

      $hasDifferent = $false
      if ($next200 -and (Test-ValidAccountHash $event.accountHash)) {
        $hasDifferent = $next200.accountHash -ne $event.accountHash
      }
      if ($hasDifferent) {
        $healthyAccountHashesAfterFallback += $next200.accountHash
      }
      if (-not $next200 -and $terminal429) {
        $unrecoveredFallback429Events += $terminal429
      }

      $fallbackTransitions += [ordered]@{
        fallbackRequestId = $event.requestId
        fallbackAccountHash = $event.accountHash
        fallbackTimestamp = $event.timestamp
        next200RequestId = if ($next200) { $next200.requestId } else { $null }
        next200AccountHash = if ($next200) { $next200.accountHash } else { $null }
        next200Timestamp = if ($next200) { $next200.timestamp } else { $null }
        next200Phase = if ($next200) { $next200.phase } else { $null }
        terminal429Timestamp = if ($terminal429) { $terminal429.timestamp } else { $null }
        terminal429AccountHash = if ($terminal429) { $terminal429.accountHash } else { $null }
        sameRequest = [bool]($next200 -and $next200.requestId -eq $event.requestId)
        completed = [bool]($null -ne $next200)
        differentAccount = [bool]$hasDifferent
      }
    }

    if ($event.phase -eq "fallback_blocked" -and $event.outcome -eq "hard_affinity") {
      $localCompletion = $null
      $terminalCompletion = $null
      $terminal429 = $null
      for ($j = $i + 1; $j -lt $parsedEvents.Count; $j++) {
        $candidate = $parsedEvents[$j]
        if ($candidate.requestId -ne $event.requestId) {
          continue
        }
        if ($candidate.phase -eq "listener") {
          break
        }
        if (
          (Test-LocalPoolUnavailableEvent $candidate) -and
          (Test-CodexResponsesRoute $candidate) -and
          $candidate.status -eq 200 -and
          ($candidate.streamState -eq "completed" -or $candidate.outcome -eq "in_band_local_completion" -or $candidate.rawLine -match 'response\.completed')
        ) {
          $localCompletion = $candidate
          break
        }
        if (
          (Test-AuditStreamTerminalCompleted $candidate) -or
          ($candidate.phase -eq "final_response" -and $candidate.status -eq 200 -and -not (Test-LocalPoolUnavailableEvent $candidate))
        ) {
          $terminalCompletion = $candidate
          break
        }
        if ($candidate.phase -eq "final_response" -and $candidate.status -eq 429 -and -not (Test-LocalPoolUnavailableEvent $candidate)) {
          $terminal429 = $candidate
          break
        }
      }

      $structuredQuotaTerminal429 = [bool](
        $null -ne $terminal429 -and
        (
          (Test-UsageLimitEvent $terminal429) -or
          (Test-UsageLimitEvent $event)
        )
      )
      $structuredQuotaTerminalSource = $null
      if ($structuredQuotaTerminal429) {
        $structuredQuotaTerminalSource = if (Test-UsageLimitEvent $terminal429) {
          "final_response"
        } else {
          "fallback_blocked"
        }
      }

      $sameTaskAffinityBlockTransitions += [ordered]@{
        requestId = $event.requestId
        blockedAccountHash = $event.accountHash
        blockedTimestamp = $event.timestamp
        localCompletionTimestamp = if ($localCompletion) { $localCompletion.timestamp } else { $null }
        localCompletionAccountHash = if ($localCompletion) { $localCompletion.accountHash } else { $null }
        terminalCompletionTimestamp = if ($terminalCompletion) { $terminalCompletion.timestamp } else { $null }
        terminalCompletionAccountHash = if ($terminalCompletion) { $terminalCompletion.accountHash } else { $null }
        terminal429Timestamp = if ($terminal429) { $terminal429.timestamp } else { $null }
        completedLocally = [bool]($null -ne $localCompletion)
        completedByUpstream = [bool]($null -ne $terminalCompletion)
        unrecoveredTerminal429 = [bool]($null -ne $terminal429)
        structuredQuotaTerminal429 = [bool]$structuredQuotaTerminal429
        structuredQuotaTerminalSource = $structuredQuotaTerminalSource
      }
    }
  }

  $newRequestGroups = @{}
  for ($i = 0; $i -lt $parsedEvents.Count; $i++) {
    $event = $parsedEvents[$i]
    $requestId = $event.requestId
    if (-not $requestId -or $requestId -eq "-") {
      continue
    }
    if (-not $newRequestGroups.ContainsKey($requestId)) {
      $newRequestGroups[$requestId] = [ordered]@{
        requestId = $requestId
        firstIndex = $i
        firstTimestamp = $event.timestamp
        accountEvents = @()
      }
    }
    $group = $newRequestGroups[$requestId]
    if (Test-ValidAccountHash $event.accountHash) {
      $group.accountEvents += [ordered]@{
        accountHash = $event.accountHash
        status = $event.status
        isLocalPoolUnavailable = [bool](Test-LocalPoolUnavailableEvent $event)
      }
    }
  }

  $newRequestAvoidance = @()
  $newRequestBlockedReuse = @()
  if ($firstBlockedAccountIndex -ge 0) {
    foreach ($group in $newRequestGroups.Values) {
      if ($group.firstIndex -le $firstBlockedAccountIndex) {
        continue
      }
      $knownBlockedBeforeRequest = @(
        $blockedAccountRecords |
          Where-Object { $_.index -lt $group.firstIndex } |
          ForEach-Object { $_.accountHash } |
          Sort-Object -Unique
      )
      $blockedUsed = @(
        $group.accountEvents |
          Where-Object { $knownBlockedBeforeRequest -contains $_.accountHash } |
          ForEach-Object { $_.accountHash } |
          Sort-Object -Unique
      )
      $healthyUsed = @(
        $group.accountEvents |
          Where-Object { $_.status -eq 200 -and -not $_.isLocalPoolUnavailable -and $knownBlockedBeforeRequest -notcontains $_.accountHash } |
          ForEach-Object { $_.accountHash } |
          Sort-Object -Unique
      )
      if ($blockedUsed.Count -gt 0) {
        $newRequestBlockedReuse += [ordered]@{
          requestId = $group.requestId
          firstTimestamp = $group.firstTimestamp
          blockedAccountHashes = @($blockedUsed)
          knownBlockedBeforeRequest = @($knownBlockedBeforeRequest)
          accountHashes = @($group.accountEvents | ForEach-Object { $_.accountHash } | Sort-Object -Unique)
        }
      } elseif ($healthyUsed.Count -gt 0) {
        $newRequestAvoidance += [ordered]@{
          requestId = $group.requestId
          firstTimestamp = $group.firstTimestamp
          knownBlockedBeforeRequest = @($knownBlockedBeforeRequest)
          healthyAccountHashes = @($healthyUsed)
        }
      }
    }
  }

  $streamGroups = @()
  $activeStreamGroups = @{}
  $accountGroups = @{}
  $streamSequence = 0
  $hardAffinityContinuityRequestIds = @(
    $parsedEvents |
      Where-Object { $_.phase -eq "request_trace" -and (Get-AuditDetailBool -Event $_ -Name "hard_affinity_continuity") } |
      ForEach-Object { $_.requestId } |
      Where-Object { -not [string]::IsNullOrWhiteSpace($_) -and $_ -ne "-" } |
      Sort-Object -Unique
  )
  foreach ($event in $parsedEvents) {
    $requestId = $event.requestId
    if (Test-ValidAccountHash $event.accountHash) {
      if (-not $accountGroups.ContainsKey($event.accountHash)) {
        $accountGroups[$event.accountHash] = [ordered]@{
          accountHash = $event.accountHash
          eventCount = 0
          status200Count = 0
          status429Count = 0
          fallbackSelectedCount = 0
          modelCooldownCount = 0
          completedStreamCount = 0
          requestIds = @()
          phases = @()
          firstTimestamp = $event.timestamp
          lastTimestamp = $event.timestamp
        }
      }
      $account = $accountGroups[$event.accountHash]
      $account.eventCount++
      $account.lastTimestamp = $event.timestamp
      if ($event.status -eq 200) {
        $account.status200Count++
      }
      if ($event.status -eq 429) {
        $account.status429Count++
      }
      if ($event.phase -eq "fallback_selected") {
        $account.fallbackSelectedCount++
      }
      if ($event.phase -eq "model_cooldown_applied") {
        $account.modelCooldownCount++
      }
      if (Test-AuditStreamTerminalCompleted $event) {
        $account.completedStreamCount++
      }
      if ($requestId -and $requestId -ne "-") {
        $account.requestIds += $requestId
      }
      $account.phases += $event.phase
    }

    if (-not $requestId -or $requestId -eq "-") {
      continue
    }
    $gatewayRequestId = Get-AuditGatewayRequestId $event
    $streamGroupKey = if (-not [string]::IsNullOrWhiteSpace($gatewayRequestId)) {
      $gatewayRequestId
    } else {
      $requestId
    }
    $isStreamTerminalEvent = $event.phase -in @("stream_completed", "stream_terminal")
    $isStreamEvent =
      $event.phase -in @("lease_granted", "stream_write", "lease_released") -or
      ($isStreamTerminalEvent -and ($activeStreamGroups.ContainsKey($streamGroupKey) -or $hardAffinityContinuityRequestIds -contains $requestId))
    $group = $null
    if ($event.phase -eq "lease_granted") {
      $streamSequence++
      $group = [ordered]@{
        streamKey = "{0}#{1}" -f $streamGroupKey, $streamSequence
        streamGroupKey = $streamGroupKey
        requestId = $requestId
        gatewayRequestId = $gatewayRequestId
        firstTimestamp = $event.timestamp
        lastTimestamp = $event.timestamp
        eventCount = 0
        started = $false
        completed = $false
        terminalError = $false
        interruptedByCooldown = $false
        upstreamStreamError = $false
        firstStartedTimestamp = $null
        terminalTimestamp = $null
        firstAccountHash = $event.accountHash
        lastAccountHash = $event.accountHash
        phases = @()
        streamStates = @()
        statuses = @()
        accountHashes = @()
      }
      $streamGroups += $group
      $activeStreamGroups[$streamGroupKey] = $group
    } elseif ($isStreamEvent -and $activeStreamGroups.ContainsKey($streamGroupKey)) {
      $group = $activeStreamGroups[$streamGroupKey]
    } elseif ($isStreamEvent) {
      $streamSequence++
      $group = [ordered]@{
        streamKey = "{0}#{1}" -f $streamGroupKey, $streamSequence
        streamGroupKey = $streamGroupKey
        requestId = $requestId
        gatewayRequestId = $gatewayRequestId
        firstTimestamp = $event.timestamp
        lastTimestamp = $event.timestamp
        eventCount = 0
        started = $false
        completed = $false
        terminalError = $false
        interruptedByCooldown = $false
        upstreamStreamError = $false
        firstStartedTimestamp = $null
        terminalTimestamp = $null
        firstAccountHash = $event.accountHash
        lastAccountHash = $event.accountHash
        phases = @()
        streamStates = @()
        statuses = @()
        accountHashes = @()
      }
      $streamGroups += $group
      $activeStreamGroups[$streamGroupKey] = $group
    } elseif ($activeStreamGroups.ContainsKey($streamGroupKey)) {
      $group = $activeStreamGroups[$streamGroupKey]
    }
    if (-not $group) {
      continue
    }
    $group.eventCount++
    $group.lastTimestamp = $event.timestamp
    $group.lastAccountHash = $event.accountHash
    $group.phases += $event.phase
    if (-not [string]::IsNullOrWhiteSpace($event.streamState)) {
      $group.streamStates += $event.streamState
    }
    if ($null -ne $event.status) {
      $group.statuses += $event.status
    }
    if (Test-ValidAccountHash $event.accountHash) {
      $group.accountHashes += $event.accountHash
    }
    if ($event.phase -eq "lease_granted" -or $event.phase -eq "stream_write" -or $isStreamTerminalEvent) {
      $group.started = $true
      if ($null -eq $group.firstStartedTimestamp) {
        $group.firstStartedTimestamp = Get-AuditEventTimestampMs $event
      }
    }
    if ((Test-AuditStreamTerminalCompleted $event) -or ($event.phase -eq "final_response" -and $event.status -eq 200)) {
      $group.completed = $true
      if ($null -eq $group.terminalTimestamp) {
        $group.terminalTimestamp = Get-AuditEventTimestampMs $event
      }
    }
    if ($event.phase -eq "final_response" -and $event.status -ge 400 -and -not $group.completed) {
      $group.terminalError = $true
      if ($null -eq $group.terminalTimestamp) {
        $group.terminalTimestamp = Get-AuditEventTimestampMs $event
      }
    }
    if (
      $event.phase -eq "stream_error" -or
      ($event.phase -eq "stream_write" -and $event.streamState -eq "upstream_error") -or
      ($event.phase -eq "stream_terminal" -and ($event.outcome -eq "error" -or $event.streamState -eq "upstream_error"))
    ) {
      $group.terminalError = $true
      $group.upstreamStreamError = $true
      if ($null -eq $group.terminalTimestamp) {
        $group.terminalTimestamp = Get-AuditEventTimestampMs $event
      }
    }
    if ($group.started -and $event.phase -eq "model_cooldown_applied") {
      $group.interruptedByCooldown = $true
    }
    if (($isStreamTerminalEvent -or $event.phase -eq "lease_released" -or $event.phase -eq "final_response") -and ($group.completed -or $group.terminalError)) {
      [void]$activeStreamGroups.Remove($streamGroupKey)
    }
  }

  $streams = @($streamGroups)
  $startedStreams = @($streams | Where-Object { $_.started })
  $completedStreams = @($startedStreams | Where-Object { $_.completed })
  $openStreams = @($startedStreams | Where-Object { -not $_.completed -and -not $_.terminalError })
  $interruptedStreams = @($startedStreams | Where-Object { $_.interruptedByCooldown })
  $terminalErrorStreams = @($startedStreams | Where-Object { $_.terminalError })
  $upstreamStreamErrorStreams = @($startedStreams | Where-Object { $_.upstreamStreamError -or ($_.streamStates -contains "upstream_error") -or ($_.phases -contains "stream_error") })
  $clientAbortedStreams = @($startedStreams | Where-Object { $_.phases -contains "client_aborted" })
  $clientAbortedBeforeFirstChunk = @($clientAbortedStreams | Where-Object { $_.phases -notcontains "stream_write" })
  $clientAbortedAfterFirstChunk = @($clientAbortedStreams | Where-Object { $_.phases -contains "stream_write" })

  $accountExhaustionRecords = @{}
  foreach ($event in $parsedEvents) {
    if (-not (Test-ValidAccountHash $event.accountHash)) {
      continue
    }
    if (-not (Test-UsageLimitEvent $event)) {
      continue
    }
    $eventTimestamp = Get-AuditEventTimestampMs $event
    if ($null -eq $eventTimestamp) {
      continue
    }
    if (-not $accountExhaustionRecords.ContainsKey($event.accountHash)) {
      $accountExhaustionRecords[$event.accountHash] = [ordered]@{
        accountHash = $event.accountHash
        firstExhaustionTimestamp = $eventTimestamp
        firstExhaustionRequestId = $event.requestId
        firstExhaustionGatewayRequestId = Get-AuditGatewayRequestId $event
        firstExhaustionPhase = $event.phase
        firstExhaustionStatus = $event.status
      }
    }
  }

  $accountExhaustionContinuitySummaries = @()
  foreach ($accountHash in @($accountExhaustionRecords.Keys | Sort-Object)) {
    $exhaustion = $accountExhaustionRecords[$accountHash]
    $exhaustedAt = [int64]$exhaustion.firstExhaustionTimestamp
    $inFlightStreams = @()
    foreach ($stream in $startedStreams) {
      $streamAccountHashes = @($stream.accountHashes | Where-Object { Test-ValidAccountHash $_ } | Select-Object -Unique)
      if ($streamAccountHashes -notcontains $accountHash) {
        continue
      }
      $startedAt = if ($null -ne $stream.firstStartedTimestamp) { [int64]$stream.firstStartedTimestamp } elseif ($null -ne $stream.firstTimestamp) { [int64]$stream.firstTimestamp } else { $null }
      if ($null -eq $startedAt -or $startedAt -gt $exhaustedAt) {
        continue
      }
      $terminalAt = if ($null -ne $stream.terminalTimestamp) { [int64]$stream.terminalTimestamp } elseif (($stream.completed -or $stream.terminalError) -and $null -ne $stream.lastTimestamp) { [int64]$stream.lastTimestamp } else { $null }
      if ($null -ne $terminalAt -and $terminalAt -lt $exhaustedAt) {
        continue
      }
      $clientAborted = [bool]($stream.phases -contains "client_aborted")
      $terminalError = [bool]($stream.terminalError -or $stream.upstreamStreamError)
      $completed = [bool]$stream.completed
      $open = [bool](-not $completed -and -not $terminalError -and -not $clientAborted)
      $inFlightStreams += [ordered]@{
        streamKey = $stream.streamKey
        streamGroupKey = $stream.streamGroupKey
        gatewayRequestId = $stream.gatewayRequestId
        requestId = $stream.requestId
        startedTimestamp = $startedAt
        terminalTimestamp = $terminalAt
        completed = $completed
        terminalError = $terminalError
        upstreamStreamError = [bool]$stream.upstreamStreamError
        clientAborted = $clientAborted
        interruptedByCooldown = [bool]$stream.interruptedByCooldown
        openAfterExhaustion = $open
        accountHashes = @($streamAccountHashes)
        phases = @($stream.phases | Select-Object -Unique)
        streamStates = @($stream.streamStates | Select-Object -Unique)
        statuses = @($stream.statuses | Select-Object -Unique)
      }
    }
    $completedAfterExhaustion = @($inFlightStreams | Where-Object { $_.completed -and -not $_.terminalError -and -not $_.clientAborted })
    $terminalErrorAfterExhaustion = @($inFlightStreams | Where-Object { $_.terminalError })
    $clientAbortedAfterExhaustion = @($inFlightStreams | Where-Object { $_.clientAborted })
    $interruptedAfterExhaustion = @($inFlightStreams | Where-Object { $_.interruptedByCooldown })
    $openAfterExhaustion = @($inFlightStreams | Where-Object { $_.openAfterExhaustion })
    $accountExhaustionContinuitySummaries += [ordered]@{
      accountHash = $accountHash
      firstExhaustionTimestamp = $exhaustion.firstExhaustionTimestamp
      firstExhaustionRequestId = $exhaustion.firstExhaustionRequestId
      firstExhaustionGatewayRequestId = $exhaustion.firstExhaustionGatewayRequestId
      firstExhaustionPhase = $exhaustion.firstExhaustionPhase
      firstExhaustionStatus = $exhaustion.firstExhaustionStatus
      inFlightAtExhaustionCount = $inFlightStreams.Count
      completedAfterExhaustionCount = $completedAfterExhaustion.Count
      terminalErrorAfterExhaustionCount = $terminalErrorAfterExhaustion.Count
      clientAbortedAfterExhaustionCount = $clientAbortedAfterExhaustion.Count
      interruptedByCooldownAfterExhaustionCount = $interruptedAfterExhaustion.Count
      openAfterExhaustionCount = $openAfterExhaustion.Count
      allInFlightTerminal = [bool]($inFlightStreams.Count -gt 0 -and $openAfterExhaustion.Count -eq 0)
      allInFlightCompleted = [bool]($inFlightStreams.Count -gt 0 -and $completedAfterExhaustion.Count -eq $inFlightStreams.Count)
      inFlightStreams = @($inFlightStreams)
    }
  }
  $totalInFlightAtAccountExhaustion = [int]($accountExhaustionContinuitySummaries | ForEach-Object { [int]$_.inFlightAtExhaustionCount } | Measure-Object -Sum).Sum
  $totalCompletedAfterAccountExhaustion = [int]($accountExhaustionContinuitySummaries | ForEach-Object { [int]$_.completedAfterExhaustionCount } | Measure-Object -Sum).Sum
  $totalTerminalErrorAfterAccountExhaustion = [int]($accountExhaustionContinuitySummaries | ForEach-Object { [int]$_.terminalErrorAfterExhaustionCount } | Measure-Object -Sum).Sum
  $totalClientAbortedAfterAccountExhaustion = [int]($accountExhaustionContinuitySummaries | ForEach-Object { [int]$_.clientAbortedAfterExhaustionCount } | Measure-Object -Sum).Sum
  $totalInterruptedAfterAccountExhaustion = [int]($accountExhaustionContinuitySummaries | ForEach-Object { [int]$_.interruptedByCooldownAfterExhaustionCount } | Measure-Object -Sum).Sum
  $totalOpenAfterAccountExhaustion = [int]($accountExhaustionContinuitySummaries | ForEach-Object { [int]$_.openAfterExhaustionCount } | Measure-Object -Sum).Sum

  $unclosedHardAffinityBlocks = @($sameTaskAffinityBlockTransitions | Where-Object { -not $_.completedLocally -and -not $_.completedByUpstream -and -not $_.unrecoveredTerminal429 })
  $stickyResetWaitRequests = @()
  $stickyResetWaitRecovered = @()
  $stickyResetWaitKilledByLocalTimeout = @()
  $stickyResetWaitExceededInlineBudget = @()
  $maxExpectedStickyResetInlineWaitMs = 3000
  foreach ($traceEvent in @($parsedEvents | Where-Object { $_.phase -eq "request_trace" -and (Get-AuditDetailBool -Event $_ -Name "hard_affinity_continuity") })) {
    $requestId = [string]$traceEvent.requestId
    $gatewayRequestId = Get-AuditGatewayRequestId $traceEvent
    $normalRequestTimeoutMs = Get-AuditDetailInt64 -Event $traceEvent -Name "normal_request_timeout_ms"
    $requestTimeoutMs = Get-AuditDetailInt64 -Event $traceEvent -Name "request_timeout_ms"
    $hardAffinityWaitLimitMs = Get-AuditDetailInt64 -Event $traceEvent -Name "hard_affinity_wait_limit_ms"
    $explicitTimeoutExtended = Get-AuditDetailValue -Event $traceEvent -Name "timeout_extended"
    $timeoutExtended = if ($null -ne $explicitTimeoutExtended) {
      Get-AuditDetailBool -Event $traceEvent -Name "timeout_extended"
    } else {
      [bool]($null -ne $normalRequestTimeoutMs -and $null -ne $requestTimeoutMs -and $requestTimeoutMs -gt $normalRequestTimeoutMs)
    }
    $sameEvents = @($parsedEvents | Where-Object {
      $sameRequest = (-not [string]::IsNullOrWhiteSpace($requestId)) -and $_.requestId -eq $requestId
      $sameGateway = $false
      if (-not [string]::IsNullOrWhiteSpace($gatewayRequestId)) {
        $sameGateway = (Get-AuditGatewayRequestId $_) -eq $gatewayRequestId
      }
      $sameRequest -or $sameGateway
    })
    $hardAffinityBlock = @($sameEvents | Where-Object { $_.phase -eq "fallback_blocked" -and $_.outcome -eq "hard_affinity" } | Select-Object -First 1)
    $sameAccountRetryWait = @($sameEvents | Where-Object {
      $_.phase -eq "pool_wait" -and
      (Get-AuditDetailValue -Event $_ -Name "reason") -eq "hard_affinity_same_account_retry"
    } | Select-Object -First 1)
    $retryAfterMs = $null
    $inlineWaitLimitMs = $null
    foreach ($candidate in @($sameAccountRetryWait + $hardAffinityBlock)) {
      if ($null -eq $retryAfterMs) {
        foreach ($name in @("retry_after_ms", "retryAfterMs")) {
          $value = Get-AuditDetailInt64 -Event $candidate -Name $name
          if ($null -ne $value) {
            $retryAfterMs = $value
            break
          }
        }
      }
      if ($null -eq $inlineWaitLimitMs) {
        foreach ($name in @("inline_wait_limit_ms", "hard_affinity_wait_limit_ms", "hard_affinity_inline_retry_wait_limit_ms", "max_inline_wait_ms", "max_hard_affinity_inline_retry_wait_ms")) {
          $value = Get-AuditDetailInt64 -Event $candidate -Name $name
          if ($null -ne $value) {
            $inlineWaitLimitMs = $value
            break
          }
        }
      }
    }
    if ($null -eq $inlineWaitLimitMs -and $null -ne $hardAffinityWaitLimitMs) {
      $inlineWaitLimitMs = $hardAffinityWaitLimitMs
    }
    $resetWaitExceedsNormalTimeout = [bool](
      $null -ne $retryAfterMs -and
      $null -ne $normalRequestTimeoutMs -and
      $retryAfterMs -gt $normalRequestTimeoutMs
    )
    $requestTimeoutCoversResetWait = [bool](
      $null -ne $retryAfterMs -and
      $null -ne $requestTimeoutMs -and
      $requestTimeoutMs -gt $retryAfterMs
    )
    $terminalSuccess = @($sameEvents | Where-Object {
      (Test-AuditStreamTerminalCompleted $_) -or
      ($_.phase -eq "final_response" -and $_.status -eq 200 -and -not (Test-LocalPoolUnavailableEvent $_))
    } | Select-Object -First 1)
    $localTimeoutTerminal = @($sameEvents | Where-Object {
      $_.phase -eq "final_response" -and
      $_.status -ge 400 -and
      (
        (Test-LocalPoolUnavailableEvent $_) -or
        ($_.rawLine -match '请求超时|request timeout|timed out')
      )
    } | Select-Object -First 1)
    $record = [ordered]@{
      requestId = $requestId
      gatewayRequestId = $gatewayRequestId
      timestamp = $traceEvent.timestamp
      normalRequestTimeoutMs = $normalRequestTimeoutMs
      requestTimeoutMs = $requestTimeoutMs
      timeoutExtended = [bool]$timeoutExtended
      retryAfterMs = $retryAfterMs
      inlineWaitLimitMs = $inlineWaitLimitMs
      hardAffinityWaitLimitMs = $hardAffinityWaitLimitMs
      resetWaitExceedsNormalTimeout = [bool]$resetWaitExceedsNormalTimeout
      requestTimeoutCoversResetWait = [bool]$requestTimeoutCoversResetWait
      stickyBoundary = Get-AuditDetailValue -Event $traceEvent -Name "sticky_boundary"
      turnLineageId = Get-AuditLineageId $traceEvent
      hasHardAffinityBlock = [bool]($hardAffinityBlock.Count -gt 0)
      hasSameAccountRetryWait = [bool]($sameAccountRetryWait.Count -gt 0)
      terminalSuccess = [bool]($terminalSuccess.Count -gt 0)
      localTimeoutTerminal = [bool]($localTimeoutTerminal.Count -gt 0)
      terminalPhase = if ($terminalSuccess.Count -gt 0) { $terminalSuccess[0].phase } elseif ($localTimeoutTerminal.Count -gt 0) { $localTimeoutTerminal[0].phase } else { $null }
      terminalStatus = if ($terminalSuccess.Count -gt 0) { $terminalSuccess[0].status } elseif ($localTimeoutTerminal.Count -gt 0) { $localTimeoutTerminal[0].status } else { $null }
    }
    $exceededInlineBudget = [bool](
      ($null -ne $record.inlineWaitLimitMs -and $record.inlineWaitLimitMs -gt $maxExpectedStickyResetInlineWaitMs) -or
      ($null -ne $record.hardAffinityWaitLimitMs -and $record.hardAffinityWaitLimitMs -gt $maxExpectedStickyResetInlineWaitMs) -or
      ($record.hasSameAccountRetryWait -and $null -ne $record.retryAfterMs -and $record.retryAfterMs -gt $maxExpectedStickyResetInlineWaitMs)
    )
    if ($record.hasSameAccountRetryWait -or $record.localTimeoutTerminal -or $exceededInlineBudget) {
      $stickyResetWaitRequests += $record
    }
    if ($exceededInlineBudget) {
      $stickyResetWaitExceededInlineBudget += $record
    }
    if ($record.hasHardAffinityBlock -and $record.terminalSuccess -and ($record.timeoutExtended -or $record.requestTimeoutCoversResetWait)) {
      $stickyResetWaitRecovered += $record
    } elseif ($record.hasHardAffinityBlock -and $record.localTimeoutTerminal -and (-not $record.timeoutExtended -or ($null -ne $record.retryAfterMs -and $null -ne $record.requestTimeoutMs -and $record.retryAfterMs -ge $record.requestTimeoutMs))) {
      $stickyResetWaitKilledByLocalTimeout += $record
    }
  }
  $responseAccountBindings = @{}
  foreach ($event in $parsedEvents) {
    if (-not (Test-ValidAccountHash $event.accountHash)) {
      continue
    }
    $responseIdHash = Get-AuditUpstreamResponseIdHash $event
    if (-not [string]::IsNullOrWhiteSpace($responseIdHash)) {
      $responseAccountBindings[$responseIdHash] = $event.accountHash
    }
  }

  $lineageGroups = @{}
  foreach ($event in $parsedEvents) {
    $lineageId = Get-AuditLineageId $event
    if ([string]::IsNullOrWhiteSpace($lineageId)) {
      continue
    }
    if (-not $lineageGroups.ContainsKey($lineageId)) {
      $lineageGroups[$lineageId] = [ordered]@{
        lineageId = $lineageId
        firstTimestamp = $event.timestamp
        lastTimestamp = $event.timestamp
        eventCount = 0
        requestIds = @()
        gatewayRequestIds = @()
        lineageSources = @()
        accountHashes = @()
        accountTransitions = @()
        hardAffinityAccountTransitions = @()
        metadataAccountTransitions = @()
        previousResponseIdHashes = @()
        upstreamResponseIdHashes = @()
        continuationReroutes = @()
        autoCompactReroutes = @()
        localCompletionAfterHardAffinity = @()
        terminalOrigins = @()
      }
    }
    $lineage = $lineageGroups[$lineageId]
    $lineage.eventCount++
    $lineage.lastTimestamp = $event.timestamp
    $lineageSource = Get-AuditLineageSource $event
    if (-not [string]::IsNullOrWhiteSpace($lineageSource)) {
      $lineage.lineageSources += $lineageSource
    }
    if ($event.requestId -and $event.requestId -ne "-") {
      $lineage.requestIds += $event.requestId
    }
    $gatewayRequestId = Get-AuditGatewayRequestId $event
    if ($gatewayRequestId) {
      $lineage.gatewayRequestIds += $gatewayRequestId
    }
    if (Test-ValidAccountHash $event.accountHash) {
      $lastAccount = if ($lineage.accountHashes.Count -gt 0) { $lineage.accountHashes[-1] } else { $null }
      if ($lastAccount -and $lastAccount -ne $event.accountHash) {
        $previousResponseIdHashForTransition = Get-AuditPreviousResponseIdHash $event
        $isContinuationTransition = [bool](Get-AuditDetailBool -Event $event -Name "is_continuation")
        $isHardAffinityTransition = [bool](
          (Test-HardAffinityLineageSource $lineageSource) -or
          $isContinuationTransition -or
          (-not [string]::IsNullOrWhiteSpace($previousResponseIdHashForTransition))
        )
        $transition = [ordered]@{
          timestamp = $event.timestamp
          fromAccountHash = $lastAccount
          toAccountHash = $event.accountHash
          requestId = $event.requestId
          gatewayRequestId = $gatewayRequestId
          phase = $event.phase
          lineageSource = $lineageSource
          isContinuation = $isContinuationTransition
          isAutoCompactCandidate = [bool](Get-AuditDetailBool -Event $event -Name "is_auto_compact_candidate")
          previousResponseIdHash = $previousResponseIdHashForTransition
          isHardAffinityLineage = $isHardAffinityTransition
        }
        $lineage.accountTransitions += $transition
        if ($isHardAffinityTransition) {
          $lineage.hardAffinityAccountTransitions += $transition
        } elseif (Test-MetadataLineageSource $lineageSource) {
          $lineage.metadataAccountTransitions += $transition
        }
      }
      $lineage.accountHashes += $event.accountHash
    }
    $previousResponseIdHash = Get-AuditPreviousResponseIdHash $event
    if ($previousResponseIdHash) {
      $lineage.previousResponseIdHashes += $previousResponseIdHash
      if ($responseAccountBindings.ContainsKey($previousResponseIdHash) -and (Test-ValidAccountHash $event.accountHash)) {
        $boundAccountHash = $responseAccountBindings[$previousResponseIdHash]
        if ($boundAccountHash -ne $event.accountHash) {
          $reroute = [ordered]@{
            timestamp = $event.timestamp
            requestId = $event.requestId
            gatewayRequestId = $gatewayRequestId
            previousResponseIdHash = $previousResponseIdHash
            expectedAccountHash = $boundAccountHash
            actualAccountHash = $event.accountHash
            phase = $event.phase
            isAutoCompactCandidate = [bool](Get-AuditDetailBool -Event $event -Name "is_auto_compact_candidate")
          }
          $lineage.continuationReroutes += $reroute
          if ($reroute.isAutoCompactCandidate) {
            $lineage.autoCompactReroutes += $reroute
          }
        }
      }
    }
    $upstreamResponseIdHash = Get-AuditUpstreamResponseIdHash $event
    if ($upstreamResponseIdHash) {
      $lineage.upstreamResponseIdHashes += $upstreamResponseIdHash
    }
    $terminalOrigin = Get-AuditDetailValue -Event $event -Name "terminal_origin"
    if (-not [string]::IsNullOrWhiteSpace($terminalOrigin)) {
      $lineage.terminalOrigins += $terminalOrigin
    }
    if ($event.phase -eq "final_response" -and (Test-LocalPoolUnavailableEvent $event)) {
      $blocked = @($sameTaskAffinityBlockTransitions | Where-Object { $_.requestId -eq $event.requestId })
      if ($blocked.Count -gt 0) {
        $lineage.localCompletionAfterHardAffinity += [ordered]@{
          timestamp = $event.timestamp
          requestId = $event.requestId
          gatewayRequestId = $gatewayRequestId
          accountHash = $event.accountHash
          streamState = $event.streamState
          outcome = $event.outcome
        }
      }
    }
  }

  $lineageSummaries = @($lineageGroups.Values | ForEach-Object {
    $uniqueAccounts = @($_.accountHashes | Where-Object { Test-ValidAccountHash $_ } | Select-Object -Unique)
    [ordered]@{
      lineageId = $_.lineageId
      firstTimestamp = $_.firstTimestamp
      lastTimestamp = $_.lastTimestamp
      eventCount = [int]$_.eventCount
      requestIds = @($_.requestIds | Select-Object -Unique)
      gatewayRequestIds = @($_.gatewayRequestIds | Select-Object -Unique)
      lineageSources = @($_.lineageSources | Select-Object -Unique)
      accountHashes = @($uniqueAccounts)
      accountSwitchCount = [math]::Max(0, $uniqueAccounts.Count - 1)
      accountTransitions = @($_.accountTransitions)
      hardAffinityAccountSwitchCount = @($_.hardAffinityAccountTransitions).Count
      hardAffinityAccountTransitions = @($_.hardAffinityAccountTransitions)
      metadataAccountSwitchCount = @($_.metadataAccountTransitions).Count
      metadataAccountTransitions = @($_.metadataAccountTransitions)
      previousResponseIdHashes = @($_.previousResponseIdHashes | Select-Object -Unique)
      upstreamResponseIdHashes = @($_.upstreamResponseIdHashes | Select-Object -Unique)
      continuationReroutes = @($_.continuationReroutes)
      autoCompactReroutes = @($_.autoCompactReroutes)
      localCompletionAfterHardAffinity = @($_.localCompletionAfterHardAffinity)
      terminalOrigins = @($_.terminalOrigins | Select-Object -Unique)
    }
  } | Sort-Object lineageId)
  $lineageAccountSwitches = @($lineageSummaries | Where-Object { $_.accountSwitchCount -gt 0 })
  $hardAffinityLineageAccountSwitches = @($lineageSummaries | Where-Object { $_.hardAffinityAccountSwitchCount -gt 0 })
  $metadataOnlyLineageAccountSwitches = @($lineageSummaries | Where-Object {
    $_.metadataAccountSwitchCount -gt 0 -and
    $_.hardAffinityAccountSwitchCount -eq 0 -and
    @($_.continuationReroutes).Count -eq 0
  })
  $continuationReroutes = @($lineageSummaries | ForEach-Object { $_.continuationReroutes })
  $autoCompactReroutes = @($lineageSummaries | ForEach-Object { $_.autoCompactReroutes })
  $lineageLocalCompletionsAfterHardAffinity = @($lineageSummaries | ForEach-Object { $_.localCompletionAfterHardAffinity })
  $detailEvents = @($parsedEvents | Where-Object { $_.detail })
  $quotaMetadataCoverage = New-QuotaMetadataCoverage $parsedEvents
  $requestTraceEvents = @($parsedEvents | Where-Object { $_.phase -eq "request_trace" })
  $hardAffinityContinuityRequestTraceEvents = @($requestTraceEvents | Where-Object { Get-AuditDetailBool -Event $_ -Name "hard_affinity_continuity" })
  $routingDecisionEvents = @($parsedEvents | Where-Object { $_.phase -eq "routing_decision" })
  $quotaClassificationEvents = @($parsedEvents | Where-Object { $_.phase -eq "quota_classification" })
  $streamTerminalEvents = @($parsedEvents | Where-Object { $_.phase -eq "stream_terminal" })
  $streamTerminalResponseCompletedEvents = @($streamTerminalEvents | Where-Object { Get-AuditDetailBool -Event $_ -Name "response_completed_seen" })
  $streamTerminalCompactionSummaryEvents = @($streamTerminalEvents | Where-Object { Get-AuditDetailBool -Event $_ -Name "compaction_summary_seen" })
  $eventsWithGatewayRequestId = @($parsedEvents | Where-Object { -not [string]::IsNullOrWhiteSpace((Get-AuditDetailValue -Event $_ -Name "gateway_request_id")) })
  $eventsWithTurnLineageId = @($parsedEvents | Where-Object { -not [string]::IsNullOrWhiteSpace((Get-AuditLineageId $_)) })
  $eventsWithExplicitTurnLineageId = @($parsedEvents | Where-Object { -not [string]::IsNullOrWhiteSpace((Get-AuditDetailValue -Event $_ -Name "turn_lineage_id")) })
  $eventsWithPreviousResponseIdHash = @($parsedEvents | Where-Object { -not [string]::IsNullOrWhiteSpace((Get-AuditPreviousResponseIdHash $_)) })
  $eventsWithUpstreamResponseIdHash = @($parsedEvents | Where-Object { -not [string]::IsNullOrWhiteSpace((Get-AuditUpstreamResponseIdHash $_)) })
  $eventsWithContinuationFlag = @($parsedEvents | Where-Object { $null -ne (Get-AuditDetailValue -Event $_ -Name "is_continuation") })
  $eventsWithAutoCompactFlag = @($parsedEvents | Where-Object { $null -ne (Get-AuditDetailValue -Event $_ -Name "is_auto_compact_candidate") })
  $autoCompactCandidateEvents = @($parsedEvents | Where-Object { Get-AuditDetailBool -Event $_ -Name "is_auto_compact_candidate" })
  $lineageRequiredFieldNames = @("gateway_request_id", "turn_lineage_id", "previous_response_id_hash", "upstream_response_id_hash", "is_continuation", "is_auto_compact_candidate")
  $lineagePresentFieldNames = @()
  if ($eventsWithGatewayRequestId.Count -gt 0) { $lineagePresentFieldNames += "gateway_request_id" }
  if ($eventsWithTurnLineageId.Count -gt 0) { $lineagePresentFieldNames += "turn_lineage_id" }
  if ($eventsWithPreviousResponseIdHash.Count -gt 0) { $lineagePresentFieldNames += "previous_response_id_hash" }
  if ($eventsWithUpstreamResponseIdHash.Count -gt 0) { $lineagePresentFieldNames += "upstream_response_id_hash" }
  if ($eventsWithContinuationFlag.Count -gt 0) { $lineagePresentFieldNames += "is_continuation" }
  if ($eventsWithAutoCompactFlag.Count -gt 0) { $lineagePresentFieldNames += "is_auto_compact_candidate" }
  $lineageMissingFieldNames = @($lineageRequiredFieldNames | Where-Object { $_ -notin $lineagePresentFieldNames })
  $completedFallbackTransitions = @($fallbackTransitions | Where-Object { $_.completed -and $_.differentAccount })
  $distinctHealthyAccountHashesAfterFallback = @($healthyAccountHashesAfterFallback | Sort-Object -Unique)
  $completedSameTaskAffinityBlocks = @($sameTaskAffinityBlockTransitions | Where-Object { $_.completedLocally })
  $upstreamCompletedSameTaskAffinityBlocks = @($sameTaskAffinityBlockTransitions | Where-Object { $_.completedByUpstream })
  $unrecoveredHardAffinityTerminal429Blocks = @($sameTaskAffinityBlockTransitions | Where-Object { $_.unrecoveredTerminal429 })
  $structuredHardAffinityTerminal429Blocks = @($sameTaskAffinityBlockTransitions | Where-Object { $_.structuredQuotaTerminal429 })
  $unstructuredHardAffinityTerminal429Blocks = @($sameTaskAffinityBlockTransitions | Where-Object { $_.unrecoveredTerminal429 -and -not $_.structuredQuotaTerminal429 })
  $distinctHealthyAccountHashesAfterBlock = @(
    $newRequestAvoidance |
      ForEach-Object { $_.healthyAccountHashes } |
      Sort-Object -Unique
  )
  $retryLimitErrorCount = [int]$retryLimitEvents.Count + [int]$unrecoveredFallback429Events.Count
  $openPoolWaitRequestIds = @()
  foreach ($poolWaitRequestId in @($poolWaitEvents | ForEach-Object { $_.requestId } | Where-Object { $_ -and $_ -ne "-" } | Sort-Object -Unique)) {
    $requestEvents = @($parsedEvents | Where-Object { $_.requestId -eq $poolWaitRequestId })
    $hasPoolWaitTerminal = [bool]@($requestEvents | Where-Object {
      (Test-AuditStreamTerminalCompleted $_) -or
      $_.phase -eq "final_response" -or
      ($_.phase -eq "upstream_forward" -and $_.status -eq 200)
    }).Count
    if (-not $hasPoolWaitTerminal) {
      $openPoolWaitRequestIds += $poolWaitRequestId
    }
  }
  $requestTimelines = New-RequestTimelines $parsedEvents

  [ordered]@{
    eventCount = $Events.Count
    parsedEventCount = $parsedEvents.Count
    parseErrorCount = @($Events | Where-Object { -not $_.parsed }).Count
    has429 = [bool]@($parsedEvents | Where-Object { $_.status -eq 429 }).Count
    hasUsageLimitReached = [bool]@($parsedEvents | Where-Object { Test-UsageLimitEvent $_ }).Count
    hasModelCooldownApplied = [bool]@($parsedEvents | Where-Object { $_.phase -eq "model_cooldown_applied" }).Count
    hasFallbackSelected = [bool]@($parsedEvents | Where-Object { $_.phase -eq "fallback_selected" }).Count
    hasFallbackBlocked = [bool]@($parsedEvents | Where-Object { $_.phase -eq "fallback_blocked" }).Count
    hasHardAffinityFallbackBlocked = [bool]@($parsedEvents | Where-Object { $_.phase -eq "fallback_blocked" -and $_.outcome -eq "hard_affinity" }).Count
    hasDifferentAccount200AfterFallback = [bool]$hasDifferentAccount200AfterFallback
    fallbackSelectedCount = @($parsedEvents | Where-Object { $_.phase -eq "fallback_selected" }).Count
    fallbackBlockedCount = @($parsedEvents | Where-Object { $_.phase -eq "fallback_blocked" }).Count
    fallbackCycleCount = $completedFallbackTransitions.Count
    distinctHealthyAccountCountAfterFallback = $distinctHealthyAccountHashesAfterFallback.Count
    distinctHealthyAccountHashesAfterFallback = @($distinctHealthyAccountHashesAfterFallback)
    fallbackTransitions = @($fallbackTransitions)
    sameTaskAffinityFallbackBlockedCount = $sameTaskAffinityBlockTransitions.Count
    sameTaskAffinityLocalCompletionCount = $completedSameTaskAffinityBlocks.Count
    sameTaskAffinityTerminalCompletionCount = $upstreamCompletedSameTaskAffinityBlocks.Count
    sameTaskAffinityTerminalCompletionRequestIds = @($upstreamCompletedSameTaskAffinityBlocks | ForEach-Object { $_.requestId } | Sort-Object -Unique)
    sameTaskAffinityUnrecoveredTerminal429Count = $unrecoveredHardAffinityTerminal429Blocks.Count
    sameTaskAffinityUnrecoveredTerminal429RequestIds = @($unrecoveredHardAffinityTerminal429Blocks | ForEach-Object { $_.requestId } | Sort-Object -Unique)
    sameTaskAffinityStructuredQuotaTerminal429Count = $structuredHardAffinityTerminal429Blocks.Count
    sameTaskAffinityStructuredQuotaTerminal429RequestIds = @($structuredHardAffinityTerminal429Blocks | ForEach-Object { $_.requestId } | Sort-Object -Unique)
    sameTaskAffinityUnstructuredTerminal429Count = $unstructuredHardAffinityTerminal429Blocks.Count
    sameTaskAffinityUnstructuredTerminal429RequestIds = @($unstructuredHardAffinityTerminal429Blocks | ForEach-Object { $_.requestId } | Sort-Object -Unique)
    sameTaskAffinityUnclosedBlockCount = $unclosedHardAffinityBlocks.Count
    sameTaskAffinityUnclosedBlockRequestIds = @($unclosedHardAffinityBlocks | ForEach-Object { $_.requestId } | Sort-Object -Unique)
    sameTaskAffinityFallbackBlockedRequestIds = @($sameTaskAffinityBlockTransitions | ForEach-Object { $_.requestId } | Sort-Object -Unique)
    sameTaskAffinityFallbackBlockedTransitions = @($sameTaskAffinityBlockTransitions)
    requestTraceCount = $requestTraceEvents.Count
    hardAffinityContinuityRequestTraceCount = $hardAffinityContinuityRequestTraceEvents.Count
    routingDecisionCount = $routingDecisionEvents.Count
    quotaClassificationCount = $quotaClassificationEvents.Count
    streamTerminalCount = $streamTerminalEvents.Count
    streamTerminalResponseCompletedCount = $streamTerminalResponseCompletedEvents.Count
    streamTerminalCompactionSummaryCount = $streamTerminalCompactionSummaryEvents.Count
    behaviorTraceCoverage = [ordered]@{
      requestTraceCount = $requestTraceEvents.Count
      hardAffinityContinuityRequestTraceCount = $hardAffinityContinuityRequestTraceEvents.Count
      routingDecisionCount = $routingDecisionEvents.Count
      quotaClassificationCount = $quotaClassificationEvents.Count
      streamTerminalCount = $streamTerminalEvents.Count
      streamTerminalResponseCompletedCount = $streamTerminalResponseCompletedEvents.Count
      streamTerminalCompactionSummaryCount = $streamTerminalCompactionSummaryEvents.Count
      hasStructuredBehaviorTrace = [bool]($requestTraceEvents.Count -gt 0 -or $routingDecisionEvents.Count -gt 0 -or $quotaClassificationEvents.Count -gt 0 -or $streamTerminalEvents.Count -gt 0)
    }
    stickyResetWaitRequestCount = $stickyResetWaitRequests.Count
    stickyResetWaitRecoveredCount = $stickyResetWaitRecovered.Count
    stickyResetWaitKilledByLocalTimeoutCount = $stickyResetWaitKilledByLocalTimeout.Count
    stickyResetWaitExceededInlineBudgetCount = $stickyResetWaitExceededInlineBudget.Count
    stickyResetWaitRequestIds = @($stickyResetWaitRequests | ForEach-Object { $_.requestId } | Sort-Object -Unique)
    stickyResetWaitRecoveredRequestIds = @($stickyResetWaitRecovered | ForEach-Object { $_.requestId } | Sort-Object -Unique)
    stickyResetWaitKilledByLocalTimeoutRequestIds = @($stickyResetWaitKilledByLocalTimeout | ForEach-Object { $_.requestId } | Sort-Object -Unique)
    stickyResetWaitExceededInlineBudgetRequestIds = @($stickyResetWaitExceededInlineBudget | ForEach-Object { $_.requestId } | Sort-Object -Unique)
    stickyResetWaitRequests = @($stickyResetWaitRequests)
    stickyResetWaitRecovered = @($stickyResetWaitRecovered)
    stickyResetWaitKilledByLocalTimeout = @($stickyResetWaitKilledByLocalTimeout)
    stickyResetWaitExceededInlineBudget = @($stickyResetWaitExceededInlineBudget)
    distinctHealthyAccountCountAfterBlock = $distinctHealthyAccountHashesAfterBlock.Count
    distinctHealthyAccountHashesAfterBlock = @($distinctHealthyAccountHashesAfterBlock)
    first429AccountHash = $first429AccountHash
    firstFallbackAccountHash = $firstFallbackAccountHash
    firstBlockedAccountHash = $firstBlockedAccountHash
    blockedAccountCount = $blockedAccountHashes.Count
    blockedAccountHashes = @($blockedAccountHashes)
    newRequestAvoidanceCount = $newRequestAvoidance.Count
    newRequestAvoidanceRequestIds = @($newRequestAvoidance | ForEach-Object { $_.requestId } | Sort-Object -Unique)
    newRequestAvoidance = @($newRequestAvoidance)
    newRequestBlockedReuseCount = $newRequestBlockedReuse.Count
    newRequestBlockedReuseRequestIds = @($newRequestBlockedReuse | ForEach-Object { $_.requestId } | Sort-Object -Unique)
    newRequestBlockedReuse = @($newRequestBlockedReuse)
    retryLimitErrorFound = [bool]$retryLimitErrorCount
    retryLimitErrorCount = $retryLimitErrorCount
    retryLimitTextMatchCount = $retryLimitEvents.Count
    unrecoveredFallback429Count = $unrecoveredFallback429Events.Count
    unrecoveredFallback429RequestIds = @($unrecoveredFallback429Events | ForEach-Object { $_.requestId } | Sort-Object -Unique)
    poolWaitCount = $poolWaitEvents.Count
    poolWaitSleepingCount = @($poolWaitEvents | Where-Object { $_.outcome -eq "sleeping" }).Count
    poolWaitRetryingCount = @($poolWaitEvents | Where-Object { $_.outcome -eq "retrying" }).Count
    poolWaitRequestIds = @($poolWaitEvents | ForEach-Object { $_.requestId } | Sort-Object -Unique)
    heartbeatPoolWaitCount = $heartbeatPoolWaitEvents.Count
    heartbeatPoolWaitRequestIds = @($heartbeatPoolWaitEvents | ForEach-Object { $_.requestId } | Sort-Object -Unique)
    activeDrainPoolWaitCount = $activeDrainPoolWaitEvents.Count
    activeDrainPoolWaitRequestIds = @($activeDrainPoolWaitEvents | ForEach-Object { $_.requestId } | Sort-Object -Unique)
    openPoolWaitCount = $openPoolWaitRequestIds.Count
    openPoolWaitRequestIds = @($openPoolWaitRequestIds)
    parkedPoolWaitCount = $parkedPoolWaitEvents.Count
    parkedPoolWaitRequestIds = @($parkedPoolWaitEvents | ForEach-Object { $_.requestId } | Sort-Object -Unique)
    sseIdleErrorCount = $sseIdleEvents.Count
    sseIdleRequestIds = @($sseIdleEvents | ForEach-Object { $_.requestId } | Where-Object { $_ } | Sort-Object -Unique)
    localPoolUnavailableCount = $localPoolUnavailableEvents.Count
    localPoolUnavailableRequestIds = @($localPoolUnavailableEvents | ForEach-Object { $_.requestId } | Sort-Object -Unique)
    inBandSyntheticPoolUnavailableCount = $inBandSyntheticPoolUnavailableEvents.Count
    inBandSyntheticPoolUnavailableRequestIds = @($inBandSyntheticPoolUnavailableEvents | ForEach-Object { $_.requestId } | Sort-Object -Unique)
    responsesFailedPoolUnavailableCount = $responsesFailedPoolUnavailableEvents.Count
    responsesFailedPoolUnavailableRequestIds = @($responsesFailedPoolUnavailableEvents | ForEach-Object { $_.requestId } | Sort-Object -Unique)
    responsesLocalCompletionPoolUnavailableCount = $responsesLocalCompletionPoolUnavailableEvents.Count
    responsesLocalCompletionPoolUnavailableRequestIds = @($responsesLocalCompletionPoolUnavailableEvents | ForEach-Object { $_.requestId } | Sort-Object -Unique)
    responsesTransport503PoolUnavailableCount = ([int]$responsesTransport503PoolUnavailableEvents.Count + [int]$responsesTransport503TextEvents.Count)
    responsesTransport503PoolUnavailableAuditCount = $responsesTransport503PoolUnavailableEvents.Count
    responsesTransport503PoolUnavailableTextCount = $responsesTransport503TextEvents.Count
    responsesTransport503PoolUnavailableRequestIds = @($responsesTransport503PoolUnavailableEvents | ForEach-Object { $_.requestId } | Sort-Object -Unique)
    lineageCount = $lineageSummaries.Count
    lineageAccountSwitchCount = $lineageAccountSwitches.Count
    lineageAccountSwitches = @($lineageAccountSwitches)
    hardAffinityLineageAccountSwitchCount = $hardAffinityLineageAccountSwitches.Count
    hardAffinityLineageAccountSwitches = @($hardAffinityLineageAccountSwitches)
    metadataOnlyLineageAccountSwitchCount = $metadataOnlyLineageAccountSwitches.Count
    metadataOnlyLineageAccountSwitches = @($metadataOnlyLineageAccountSwitches)
    continuationReroutedCount = $continuationReroutes.Count
    continuationReroutes = @($continuationReroutes)
    autoCompactReroutedCount = $autoCompactReroutes.Count
    autoCompactReroutes = @($autoCompactReroutes)
    localCompletionAfterHardAffinityLineageCount = $lineageLocalCompletionsAfterHardAffinity.Count
    localCompletionAfterHardAffinityLineages = @($lineageLocalCompletionsAfterHardAffinity)
    lineageSummaries = @($lineageSummaries)
    auditFieldCoverage = [ordered]@{
      parsedEventCount = $parsedEvents.Count
      detailEventCount = $detailEvents.Count
      gatewayRequestIdCount = $eventsWithGatewayRequestId.Count
      turnLineageIdCount = $eventsWithTurnLineageId.Count
      explicitTurnLineageIdCount = $eventsWithExplicitTurnLineageId.Count
      previousResponseIdHashCount = $eventsWithPreviousResponseIdHash.Count
      upstreamResponseIdHashCount = $eventsWithUpstreamResponseIdHash.Count
      continuationFlagCount = $eventsWithContinuationFlag.Count
      autoCompactFlagCount = $eventsWithAutoCompactFlag.Count
      autoCompactCandidateCount = $autoCompactCandidateEvents.Count
      lineageDiagnosticReady = ($parsedEvents.Count -gt 0 -and $eventsWithGatewayRequestId.Count -gt 0 -and $eventsWithTurnLineageId.Count -gt 0)
      continuationDiagnosticReady = ($eventsWithPreviousResponseIdHash.Count -gt 0 -and $eventsWithUpstreamResponseIdHash.Count -gt 0)
      autoCompactDiagnosticReady = ($eventsWithAutoCompactFlag.Count -gt 0)
      presentFieldNames = @($lineagePresentFieldNames)
      missingFieldNames = @($lineageMissingFieldNames)
    }
    quotaMetadataCoverage = $quotaMetadataCoverage
    requestTimelineCount = $requestTimelines.Count
    requestTimelines = @($requestTimelines)
    startedStreamCount = $startedStreams.Count
    completedStreamCount = $completedStreams.Count
    openStreamCount = $openStreams.Count
    interruptedStreamCount = $interruptedStreams.Count
    terminalErrorStreamCount = $terminalErrorStreams.Count
    upstreamStreamErrorCount = $upstreamStreamErrorStreams.Count
    accountExhaustionContinuityCount = $accountExhaustionContinuitySummaries.Count
    inFlightAtAccountExhaustionCount = $totalInFlightAtAccountExhaustion
    completedAfterAccountExhaustionCount = $totalCompletedAfterAccountExhaustion
    terminalErrorAfterAccountExhaustionCount = $totalTerminalErrorAfterAccountExhaustion
    clientAbortedAfterAccountExhaustionCount = $totalClientAbortedAfterAccountExhaustion
    interruptedAfterAccountExhaustionCount = $totalInterruptedAfterAccountExhaustion
    openAfterAccountExhaustionCount = $totalOpenAfterAccountExhaustion
    accountExhaustionContinuitySummaries = @($accountExhaustionContinuitySummaries)
    upstreamStreamErrorSummaries = @($upstreamStreamErrorStreams | ForEach-Object {
      [ordered]@{
        streamKey = $_.streamKey
        streamGroupKey = $_.streamGroupKey
        gatewayRequestId = $_.gatewayRequestId
        requestId = $_.requestId
        firstTimestamp = $_.firstTimestamp
        lastTimestamp = $_.lastTimestamp
        completed = [bool]$_.completed
        terminalError = [bool]$_.terminalError
        firstAccountHash = $_.firstAccountHash
        lastAccountHash = $_.lastAccountHash
        phases = @($_.phases | Select-Object -Unique)
        streamStates = @($_.streamStates | Select-Object -Unique)
        statuses = @($_.statuses | Select-Object -Unique)
      }
    })
    clientAbortedStreamCount = $clientAbortedStreams.Count
    clientAbortedBeforeFirstChunkCount = $clientAbortedBeforeFirstChunk.Count
    clientAbortedAfterFirstChunkCount = $clientAbortedAfterFirstChunk.Count
    clientAbortedStreamSummaries = @($clientAbortedStreams | ForEach-Object {
      [ordered]@{
        streamKey = $_.streamKey
        streamGroupKey = $_.streamGroupKey
        gatewayRequestId = $_.gatewayRequestId
        requestId = $_.requestId
        firstTimestamp = $_.firstTimestamp
        lastTimestamp = $_.lastTimestamp
        started = [bool]$_.started
        completed = [bool]$_.completed
        terminalError = [bool]$_.terminalError
        firstAccountHash = $_.firstAccountHash
        lastAccountHash = $_.lastAccountHash
        phases = @($_.phases | Select-Object -Unique)
        statuses = @($_.statuses | Select-Object -Unique)
        observedAfterFirstChunk = [bool]($_.phases -contains "stream_write")
        attribution = if ($_.phases -contains "stream_write") { "client_or_app_aborted_after_stream_started" } else { "client_or_app_aborted_before_stream_body" }
      }
    })
    streamSummaries = @($streams | ForEach-Object {
      [ordered]@{
        streamKey = $_.streamKey
        streamGroupKey = $_.streamGroupKey
        gatewayRequestId = $_.gatewayRequestId
        requestId = $_.requestId
        firstTimestamp = $_.firstTimestamp
        lastTimestamp = $_.lastTimestamp
        firstStartedTimestamp = $_.firstStartedTimestamp
        terminalTimestamp = $_.terminalTimestamp
        eventCount = [int]$_.eventCount
        started = [bool]$_.started
        completed = [bool]$_.completed
        terminalError = [bool]$_.terminalError
        interruptedByCooldown = [bool]$_.interruptedByCooldown
        firstAccountHash = $_.firstAccountHash
        lastAccountHash = $_.lastAccountHash
        accountHashes = @($_.accountHashes | Select-Object -Unique)
        statuses = @($_.statuses | Select-Object -Unique)
        streamStates = @($_.streamStates | Select-Object -Unique)
        phases = @($_.phases | Select-Object -Unique)
      }
    })
    accountSummaries = @($accountGroups.Values | ForEach-Object {
      [ordered]@{
        accountHash = $_.accountHash
        firstTimestamp = $_.firstTimestamp
        lastTimestamp = $_.lastTimestamp
        eventCount = [int]$_.eventCount
        status200Count = [int]$_.status200Count
        status429Count = [int]$_.status429Count
        fallbackSelectedCount = [int]$_.fallbackSelectedCount
        modelCooldownCount = [int]$_.modelCooldownCount
        completedStreamCount = [int]$_.completedStreamCount
        requestIds = @($_.requestIds | Select-Object -Unique)
        phases = @($_.phases | Select-Object -Unique)
      }
    } | Sort-Object accountHash)
  }
}

function New-AcceptanceResults {
  param(
    [System.Collections.IDictionary]$AuditSummary,
    [System.Collections.IDictionary]$CliComparison,
    [System.Collections.IDictionary]$AppComparison,
    [System.Collections.IDictionary]$ApiServiceRuntime
  )
  $results = @()

  $runtime = New-MonitorResult "api_service_runtime_available"
  $runtimeEvidence = @{
    required = [bool]$RequireApiServiceRuntimeAvailable
    available = [bool]$ApiServiceRuntime.available
    reason = $ApiServiceRuntime.reason
    apiBaseUrl = $ApiServiceRuntime.apiBaseUrl
    localAccess = $ApiServiceRuntime.localAccess
    runtimeMode = $ApiServiceRuntime.runtimeMode
    server = $ApiServiceRuntime.server
    listener = $ApiServiceRuntime.listener
  }
  if ($ApiServiceRuntime.available) {
    $results += Set-MonitorStatus $runtime "pass" $null $runtimeEvidence
  } elseif ($RequireApiServiceRuntimeAvailable) {
    $results += Set-MonitorStatus $runtime "fail" "API 服务 runtime 不可用；Codex 会把本地 gateway 断开表现为 error sending request，而不是结构化 quota 终态" $runtimeEvidence
  } elseif ($ApiServiceRuntime.localAccess.exists) {
    $results += Set-MonitorStatus $runtime "warn" "API 服务 runtime 当前不可用；本次未要求端口在线，仅记录运行态差异" $runtimeEvidence
  } else {
    $results += Set-MonitorStatus $runtime "skipped" "未找到 local access 配置；本次未要求验证 API 服务 runtime" $runtimeEvidence
  }

  $sameTask = New-MonitorResult "same_task_affinity_fallback_blocked"
  $sameTaskEvidence = @{
    has429 = [bool]$AuditSummary.has429
    hasUsageLimitReached = [bool]$AuditSummary.hasUsageLimitReached
    hasModelCooldownApplied = [bool]$AuditSummary.hasModelCooldownApplied
    hasFallbackBlocked = [bool]$AuditSummary.hasFallbackBlocked
    hasHardAffinityFallbackBlocked = [bool]$AuditSummary.hasHardAffinityFallbackBlocked
    sameTaskAffinityFallbackBlockedCount = [int]$AuditSummary.sameTaskAffinityFallbackBlockedCount
    sameTaskAffinityLocalCompletionCount = [int]$AuditSummary.sameTaskAffinityLocalCompletionCount
    sameTaskAffinityTerminalCompletionCount = [int]$AuditSummary.sameTaskAffinityTerminalCompletionCount
    sameTaskAffinityUnclosedBlockCount = [int]$AuditSummary.sameTaskAffinityUnclosedBlockCount
    sameTaskAffinityUnrecoveredTerminal429Count = [int]$AuditSummary.sameTaskAffinityUnrecoveredTerminal429Count
    sameTaskAffinityStructuredQuotaTerminal429Count = [int]$AuditSummary.sameTaskAffinityStructuredQuotaTerminal429Count
    sameTaskAffinityUnstructuredTerminal429Count = [int]$AuditSummary.sameTaskAffinityUnstructuredTerminal429Count
    sameTaskAffinityFallbackBlockedRequestIds = @($AuditSummary.sameTaskAffinityFallbackBlockedRequestIds)
    sameTaskAffinityTerminalCompletionRequestIds = @($AuditSummary.sameTaskAffinityTerminalCompletionRequestIds)
    sameTaskAffinityUnclosedBlockRequestIds = @($AuditSummary.sameTaskAffinityUnclosedBlockRequestIds)
    sameTaskAffinityUnrecoveredTerminal429RequestIds = @($AuditSummary.sameTaskAffinityUnrecoveredTerminal429RequestIds)
    requiredFallbackCycles = [int]$RequiredFallbackCycles
    first429AccountHash = $AuditSummary.first429AccountHash
    localPoolUnavailableCount = [int]$AuditSummary.localPoolUnavailableCount
    responsesLocalCompletionPoolUnavailableCount = [int]$AuditSummary.responsesLocalCompletionPoolUnavailableCount
  }
  $sameTaskObserved = $AuditSummary.has429 -and $AuditSummary.hasUsageLimitReached -and $AuditSummary.hasModelCooldownApplied -and $AuditSummary.sameTaskAffinityFallbackBlockedCount -ge $RequiredFallbackCycles
  if ($AuditSummary.sameTaskAffinityLocalCompletionCount -gt 0) {
    $results += Set-MonitorStatus $sameTask "fail" "同任务 hard-affinity block 后被本地 pool_unavailable completed 闭合；这会让用户侧任务提前结束" $sameTaskEvidence
  } elseif ($AuditSummary.sameTaskAffinityUnstructuredTerminal429Count -gt 0) {
    $results += Set-MonitorStatus $sameTask "fail" "同任务 hard-affinity block 后以非结构化 final 429 结束，无法证明是协议保持的额度终止" $sameTaskEvidence
  } elseif ($AuditSummary.sameTaskAffinityUnclosedBlockCount -gt 0) {
    $results += Set-MonitorStatus $sameTask "blocked" "同任务 hard-affinity block 已出现，但监测窗口内没有观察到 terminal/local completion，无法确认是否继续到完成" $sameTaskEvidence
  } elseif ($sameTaskObserved -and ($AuditSummary.sameTaskAffinityTerminalCompletionCount + $AuditSummary.sameTaskAffinityStructuredQuotaTerminal429Count) -ge $RequiredFallbackCycles) {
    $results += Set-MonitorStatus $sameTask "pass" $null $sameTaskEvidence
  } elseif ($sameTaskObserved) {
    $results += Set-MonitorStatus $sameTask "blocked" "同任务 hard-affinity block 已出现，但观察到的真实 upstream terminal completion 不足" $sameTaskEvidence
  } elseif ($RequireQuotaFallback) {
    $results += Set-MonitorStatus $sameTask "blocked" "未在监测窗口内观察到同任务 429 -> cooldown -> fallback_blocked(hard_affinity)" $sameTaskEvidence
  } else {
    $results += Set-MonitorStatus $sameTask "skipped" "未要求 quota hard-affinity 必须出现；仅记录 audit 观察结果" $sameTaskEvidence
  }

  $stickyReset = New-MonitorResult "sticky_reset_wait_not_killed_by_local_timeout"
  $stickyResetEvidence = @{
    stickyResetWaitRequestCount = [int]$AuditSummary.stickyResetWaitRequestCount
    stickyResetWaitRecoveredCount = [int]$AuditSummary.stickyResetWaitRecoveredCount
    stickyResetWaitKilledByLocalTimeoutCount = [int]$AuditSummary.stickyResetWaitKilledByLocalTimeoutCount
    stickyResetWaitExceededInlineBudgetCount = [int]$AuditSummary.stickyResetWaitExceededInlineBudgetCount
    stickyResetWaitRequestIds = @($AuditSummary.stickyResetWaitRequestIds)
    stickyResetWaitRecoveredRequestIds = @($AuditSummary.stickyResetWaitRecoveredRequestIds)
    stickyResetWaitKilledByLocalTimeoutRequestIds = @($AuditSummary.stickyResetWaitKilledByLocalTimeoutRequestIds)
    stickyResetWaitExceededInlineBudgetRequestIds = @($AuditSummary.stickyResetWaitExceededInlineBudgetRequestIds)
    stickyResetWaitRequests = @($AuditSummary.stickyResetWaitRequests)
  }
  if ($AuditSummary.stickyResetWaitExceededInlineBudgetCount -gt 0) {
    $results += Set-MonitorStatus $stickyReset "fail" "hard-affinity continuation 出现超过 3 秒的同账号内联等待预算；长 reset 应快速返回额度终态而不是挂起请求" $stickyResetEvidence
  } elseif ($AuditSummary.stickyResetWaitKilledByLocalTimeoutCount -gt 0) {
    $results += Set-MonitorStatus $stickyReset "fail" "hard-affinity continuation 已触发，但 request timeout 没有扩展到 cooldown 等待窗口，旧任务被本地超时提前打断" $stickyResetEvidence
  } elseif ($AuditSummary.stickyResetWaitRecoveredCount -gt 0) {
    $results += Set-MonitorStatus $stickyReset "pass" $null $stickyResetEvidence
  } elseif ($AuditSummary.stickyResetWaitRequestCount -gt 0) {
    $results += Set-MonitorStatus $stickyReset "blocked" "已观察到 hard-affinity continuation request_trace，但监测窗口内没有看到恢复完成或明确本地超时终止" $stickyResetEvidence
  } else {
    $results += Set-MonitorStatus $stickyReset "skipped" "未观察到 hard-affinity continuation request_trace；旧 audit 可能尚未包含该结构化追踪字段" $stickyResetEvidence
  }

  $behaviorTrace = New-MonitorResult "structured_behavior_trace_present"
  $behaviorTraceEvidence = @{
    behaviorTraceCoverage = $AuditSummary.behaviorTraceCoverage
    requestTraceCount = [int]$AuditSummary.requestTraceCount
    hardAffinityContinuityRequestTraceCount = [int]$AuditSummary.hardAffinityContinuityRequestTraceCount
    routingDecisionCount = [int]$AuditSummary.routingDecisionCount
    quotaClassificationCount = [int]$AuditSummary.quotaClassificationCount
    streamTerminalCount = [int]$AuditSummary.streamTerminalCount
    streamTerminalResponseCompletedCount = [int]$AuditSummary.streamTerminalResponseCompletedCount
    streamTerminalCompactionSummaryCount = [int]$AuditSummary.streamTerminalCompactionSummaryCount
  }
  if ($AuditSummary.eventCount -eq 0) {
    $results += Set-MonitorStatus $behaviorTrace "skipped" "监测窗口内没有 audit 事件，无法评估结构化行为追踪覆盖率" $behaviorTraceEvidence
  } elseif ($AuditSummary.behaviorTraceCoverage.hasStructuredBehaviorTrace) {
    $results += Set-MonitorStatus $behaviorTrace "pass" $null $behaviorTraceEvidence
  } elseif ($RequireQuotaFallback -or $RequireStreamCompletion) {
    $results += Set-MonitorStatus $behaviorTrace "blocked" "当前 audit 仍是旧事件模型，缺少 request_trace/routing_decision/quota_classification/stream_terminal；请确认实跑使用的是当前源码构建出的 gateway，而不是过期 target/debug 二进制" $behaviorTraceEvidence
  } else {
    $results += Set-MonitorStatus $behaviorTrace "warn" "当前 audit 仍是旧事件模型，缺少 request_trace/routing_decision/quota_classification/stream_terminal 结构化行为追踪" $behaviorTraceEvidence
  }

  $quotaMetadata = New-MonitorResult "quota_metadata_fields_present"
  $quotaMetadataEvidence = @{
    quotaMetadataCoverage = $AuditSummary.quotaMetadataCoverage
  }
  if ($AuditSummary.eventCount -eq 0) {
    $results += Set-MonitorStatus $quotaMetadata "skipped" "监测窗口内没有 audit 事件，无法评估 plan/quota 元数据覆盖率" $quotaMetadataEvidence
  } elseif ($AuditSummary.hasUsageLimitReached -and -not ($AuditSummary.quotaMetadataCoverage.hasPlanMetadata -or $AuditSummary.quotaMetadataCoverage.hasResetMetadata -or $AuditSummary.quotaMetadataCoverage.hasLimitMetadata)) {
    $results += Set-MonitorStatus $quotaMetadata "warn" "已观察到 usage_limit_reached，但 audit 缺少 plan_type/reset/active_limit 等元数据；Free/Plus 差异定位证据不足" $quotaMetadataEvidence
  } else {
    $results += Set-MonitorStatus $quotaMetadata "pass" $null $quotaMetadataEvidence
  }

  $newRequest = New-MonitorResult "new_request_avoids_exhausted_account"
  $newRequestEvidence = @{
    blockedAccountCount = [int]$AuditSummary.blockedAccountCount
    blockedAccountHashes = @($AuditSummary.blockedAccountHashes)
    newRequestAvoidanceCount = [int]$AuditSummary.newRequestAvoidanceCount
    newRequestAvoidanceRequestIds = @($AuditSummary.newRequestAvoidanceRequestIds)
    newRequestBlockedReuseCount = [int]$AuditSummary.newRequestBlockedReuseCount
    newRequestBlockedReuseRequestIds = @($AuditSummary.newRequestBlockedReuseRequestIds)
  }
  if ($AuditSummary.newRequestBlockedReuseCount -gt 0) {
    $results += Set-MonitorStatus $newRequest "fail" "监测窗口内后续新请求仍命中过已 exhausted/cooldown 的账号" $newRequestEvidence
  } elseif ($AuditSummary.newRequestAvoidanceCount -gt 0) {
    $results += Set-MonitorStatus $newRequest "pass" $null $newRequestEvidence
  } elseif ($RequireQuotaFallback) {
    $results += Set-MonitorStatus $newRequest "blocked" "未观察到后续新请求避开 exhausted/cooldown 账号" $newRequestEvidence
  } else {
    $results += Set-MonitorStatus $newRequest "skipped" "未要求观察新请求避开 exhausted/cooldown 账号" $newRequestEvidence
  }

  $multi = New-MonitorResult "multi_account_fallback_observed"
  $multiEvidence = @{
    sameTaskAffinityFallbackBlockedCount = [int]$AuditSummary.sameTaskAffinityFallbackBlockedCount
    sameTaskAffinityLocalCompletionCount = [int]$AuditSummary.sameTaskAffinityLocalCompletionCount
    sameTaskAffinityTerminalCompletionCount = [int]$AuditSummary.sameTaskAffinityTerminalCompletionCount
    requiredFallbackCycles = [int]$RequiredFallbackCycles
    distinctHealthyAccountCountAfterBlock = [int]$AuditSummary.distinctHealthyAccountCountAfterBlock
    requiredDistinctHealthyAccounts = [int]$RequiredDistinctHealthyAccounts
    distinctHealthyAccountHashesAfterBlock = @($AuditSummary.distinctHealthyAccountHashesAfterBlock)
  }
  if ($RequiredFallbackCycles -le 1 -and $RequiredDistinctHealthyAccounts -le 1) {
    $results += Set-MonitorStatus $multi "skipped" "未要求多账号 quota recovery 计数；仅记录多账号观察结果" $multiEvidence
  } elseif ($AuditSummary.sameTaskAffinityLocalCompletionCount -gt 0) {
    $results += Set-MonitorStatus $multi "fail" "观察到同任务 hard-affinity block 被本地 completed 闭合；多账号接管只能发生在后续 independent request" $multiEvidence
  } elseif ($AuditSummary.sameTaskAffinityTerminalCompletionCount -ge $RequiredFallbackCycles -and $AuditSummary.distinctHealthyAccountCountAfterBlock -ge $RequiredDistinctHealthyAccounts) {
    $results += Set-MonitorStatus $multi "pass" $null $multiEvidence
  } else {
    $results += Set-MonitorStatus $multi "blocked" "未观察到足够的同任务 hard-affinity block 与后续健康账号接管" $multiEvidence
  }

  $stream = New-MonitorResult "accepted_stream_continuity"
  $streamEvidence = @{
    startedStreamCount = [int]$AuditSummary.startedStreamCount
    completedStreamCount = [int]$AuditSummary.completedStreamCount
    requiredCompletedStreams = [int]$RequiredCompletedStreams
    openStreamCount = [int]$AuditSummary.openStreamCount
    interruptedStreamCount = [int]$AuditSummary.interruptedStreamCount
    terminalErrorStreamCount = [int]$AuditSummary.terminalErrorStreamCount
    upstreamStreamErrorCount = [int]$AuditSummary.upstreamStreamErrorCount
    upstreamStreamErrorSummaries = @($AuditSummary.upstreamStreamErrorSummaries)
    clientAbortedStreamCount = [int]$AuditSummary.clientAbortedStreamCount
    clientAbortedBeforeFirstChunkCount = [int]$AuditSummary.clientAbortedBeforeFirstChunkCount
    clientAbortedAfterFirstChunkCount = [int]$AuditSummary.clientAbortedAfterFirstChunkCount
    clientAbortedStreamSummaries = @($AuditSummary.clientAbortedStreamSummaries)
  }
  if ($AuditSummary.interruptedStreamCount -gt 0) {
    $results += Set-MonitorStatus $stream "fail" "已开始的 stream 后续出现 model_cooldown_applied，中断边界异常" $streamEvidence
  } elseif ($AuditSummary.upstreamStreamErrorCount -gt 0) {
    $results += Set-MonitorStatus $stream "fail" "已开始的 stream 出现 upstream_error/stream_error；这会让 Codex turn 停在最后一个可见工具动作，必须以明确 terminal event 闭合并纳入故障统计" $streamEvidence
  } elseif ($AuditSummary.completedStreamCount -ge $RequiredCompletedStreams) {
    $results += Set-MonitorStatus $stream "pass" $null $streamEvidence
  } elseif ($RequireStreamCompletion) {
    $results += Set-MonitorStatus $stream "blocked" "未在监测窗口内观察到已接纳 stream 完成" $streamEvidence
  } else {
    $results += Set-MonitorStatus $stream "skipped" "未要求必须观察 stream 完成；仅记录 stream 状态" $streamEvidence
  }

  $accountExhaustionStream = New-MonitorResult "in_flight_streams_survive_account_exhaustion"
  $accountExhaustionStreamEvidence = @{
    accountExhaustionContinuityCount = [int]$AuditSummary.accountExhaustionContinuityCount
    inFlightAtAccountExhaustionCount = [int]$AuditSummary.inFlightAtAccountExhaustionCount
    completedAfterAccountExhaustionCount = [int]$AuditSummary.completedAfterAccountExhaustionCount
    terminalErrorAfterAccountExhaustionCount = [int]$AuditSummary.terminalErrorAfterAccountExhaustionCount
    clientAbortedAfterAccountExhaustionCount = [int]$AuditSummary.clientAbortedAfterAccountExhaustionCount
    interruptedAfterAccountExhaustionCount = [int]$AuditSummary.interruptedAfterAccountExhaustionCount
    openAfterAccountExhaustionCount = [int]$AuditSummary.openAfterAccountExhaustionCount
    accountExhaustionContinuitySummaries = @($AuditSummary.accountExhaustionContinuitySummaries)
  }
  if ($AuditSummary.terminalErrorAfterAccountExhaustionCount -gt 0 -or $AuditSummary.interruptedAfterAccountExhaustionCount -gt 0) {
    $results += Set-MonitorStatus $accountExhaustionStream "fail" "账号首次耗尽时已经开始的 stream 出现 terminal error 或 cooldown interrupt" $accountExhaustionStreamEvidence
  } elseif ($AuditSummary.clientAbortedAfterAccountExhaustionCount -gt 0) {
    $results += Set-MonitorStatus $accountExhaustionStream "blocked" "账号首次耗尽时已经开始的 stream 后续被 client_aborted；仅凭 audit 不能归因为服务端连续性失败" $accountExhaustionStreamEvidence
  } elseif ($AuditSummary.openAfterAccountExhaustionCount -gt 0) {
    $results += Set-MonitorStatus $accountExhaustionStream "blocked" "账号首次耗尽时已经开始的 stream 在监测窗口结束时仍未观察到 terminal event" $accountExhaustionStreamEvidence
  } elseif ($AuditSummary.inFlightAtAccountExhaustionCount -gt 0 -and $AuditSummary.completedAfterAccountExhaustionCount -eq $AuditSummary.inFlightAtAccountExhaustionCount) {
    $results += Set-MonitorStatus $accountExhaustionStream "pass" $null $accountExhaustionStreamEvidence
  } elseif ($AuditSummary.accountExhaustionContinuityCount -gt 0) {
    $results += Set-MonitorStatus $accountExhaustionStream "skipped" "已观察到账号耗尽，但耗尽瞬间没有已开始且未结束的 stream" $accountExhaustionStreamEvidence
  } else {
    $results += Set-MonitorStatus $accountExhaustionStream "skipped" "未观察到账号 usage_limit_reached 耗尽事件" $accountExhaustionStreamEvidence
  }

  $retry = New-MonitorResult "retry_limit_regression_absent"
  $retryEvidence = @{
    retryLimitErrorFound = [bool]$AuditSummary.retryLimitErrorFound
    retryLimitErrorCount = [int]$AuditSummary.retryLimitErrorCount
    retryLimitTextMatchCount = [int]$AuditSummary.retryLimitTextMatchCount
    unrecoveredFallback429Count = [int]$AuditSummary.unrecoveredFallback429Count
    unrecoveredFallback429RequestIds = @($AuditSummary.unrecoveredFallback429RequestIds)
    localPoolUnavailableCount = [int]$AuditSummary.localPoolUnavailableCount
    localPoolUnavailableRequestIds = @($AuditSummary.localPoolUnavailableRequestIds)
  }
  if ($AuditSummary.retryLimitErrorFound) {
    $results += Set-MonitorStatus $retry "fail" "监测窗口内出现历史 retry-limit 错误文本，或可切号 fallback 后仍以 final 429 结束" $retryEvidence
  } else {
    $results += Set-MonitorStatus $retry "pass" $null $retryEvidence
  }

  $sseIdle = New-MonitorResult "sse_idle_pool_wait_regression_absent"
  $sseIdleEvidence = @{
    parkedPoolWaitCount = [int]$AuditSummary.parkedPoolWaitCount
    parkedPoolWaitRequestIds = @($AuditSummary.parkedPoolWaitRequestIds)
    heartbeatPoolWaitCount = [int]$AuditSummary.heartbeatPoolWaitCount
    heartbeatPoolWaitRequestIds = @($AuditSummary.heartbeatPoolWaitRequestIds)
    activeDrainPoolWaitCount = [int]$AuditSummary.activeDrainPoolWaitCount
    activeDrainPoolWaitRequestIds = @($AuditSummary.activeDrainPoolWaitRequestIds)
    sseIdleErrorCount = [int]$AuditSummary.sseIdleErrorCount
    sseIdleRequestIds = @($AuditSummary.sseIdleRequestIds)
  }
  if ($AuditSummary.parkedPoolWaitCount -gt 0 -or $AuditSummary.heartbeatPoolWaitCount -gt 0 -or $AuditSummary.sseIdleErrorCount -gt 0) {
    $results += Set-MonitorStatus $sseIdle "fail" "监测窗口内出现 heartbeat/parked pool_wait 或 SSE idle timeout；streaming /v1/responses 不能以保活但无 terminal event 的方式静默挂起" $sseIdleEvidence
  } else {
    $results += Set-MonitorStatus $sseIdle "pass" $null $sseIdleEvidence
  }

  $poolWaitProgress = New-MonitorResult "pool_wait_reaches_terminal_or_recovery"
  $poolWaitProgressEvidence = @{
    poolWaitCount = [int]$AuditSummary.poolWaitCount
    heartbeatPoolWaitCount = [int]$AuditSummary.heartbeatPoolWaitCount
    activeDrainPoolWaitCount = [int]$AuditSummary.activeDrainPoolWaitCount
    openPoolWaitCount = [int]$AuditSummary.openPoolWaitCount
    openPoolWaitRequestIds = @($AuditSummary.openPoolWaitRequestIds)
    responsesFailedPoolUnavailableCount = [int]$AuditSummary.responsesFailedPoolUnavailableCount
    responsesFailedPoolUnavailableRequestIds = @($AuditSummary.responsesFailedPoolUnavailableRequestIds)
    responsesLocalCompletionPoolUnavailableCount = [int]$AuditSummary.responsesLocalCompletionPoolUnavailableCount
    responsesLocalCompletionPoolUnavailableRequestIds = @($AuditSummary.responsesLocalCompletionPoolUnavailableRequestIds)
  }
  if ($AuditSummary.openPoolWaitCount -gt 0) {
    $results += Set-MonitorStatus $poolWaitProgress "fail" "监测窗口内存在 open pool_wait；全池不可用必须在请求预算内恢复或以本地 completed Responses 响应闭合，不能无 terminal event 停滞" $poolWaitProgressEvidence
  } else {
    $results += Set-MonitorStatus $poolWaitProgress "pass" $null $poolWaitProgressEvidence
  }

  $responses503 = New-MonitorResult "responses_pool_unavailable_transport_503_absent"
  $responses503Evidence = @{
    responsesTransport503PoolUnavailableCount = [int]$AuditSummary.responsesTransport503PoolUnavailableCount
    responsesTransport503PoolUnavailableAuditCount = [int]$AuditSummary.responsesTransport503PoolUnavailableAuditCount
    responsesTransport503PoolUnavailableTextCount = [int]$AuditSummary.responsesTransport503PoolUnavailableTextCount
    responsesTransport503PoolUnavailableRequestIds = @($AuditSummary.responsesTransport503PoolUnavailableRequestIds)
    inBandSyntheticPoolUnavailableCount = [int]$AuditSummary.inBandSyntheticPoolUnavailableCount
    inBandSyntheticPoolUnavailableRequestIds = @($AuditSummary.inBandSyntheticPoolUnavailableRequestIds)
    responsesFailedPoolUnavailableCount = [int]$AuditSummary.responsesFailedPoolUnavailableCount
    responsesFailedPoolUnavailableRequestIds = @($AuditSummary.responsesFailedPoolUnavailableRequestIds)
    responsesLocalCompletionPoolUnavailableCount = [int]$AuditSummary.responsesLocalCompletionPoolUnavailableCount
    responsesLocalCompletionPoolUnavailableRequestIds = @($AuditSummary.responsesLocalCompletionPoolUnavailableRequestIds)
  }
  if ($AuditSummary.responsesTransport503PoolUnavailableCount -gt 0) {
    $results += Set-MonitorStatus $responses503 "fail" "监测窗口内 Codex-facing /v1/responses 暴露了 transport 503/pool_unavailable；请求应返回 200 completed Responses 本地响应，不能让 Codex CLI/App 看到 transport 503" $responses503Evidence
  } else {
    $results += Set-MonitorStatus $responses503 "pass" $null $responses503Evidence
  }

  $localCompletion = New-MonitorResult "responses_pool_unavailable_local_completion_explicit"
  $localCompletionEvidence = @{
    responsesLocalCompletionPoolUnavailableCount = [int]$AuditSummary.responsesLocalCompletionPoolUnavailableCount
    responsesLocalCompletionPoolUnavailableRequestIds = @($AuditSummary.responsesLocalCompletionPoolUnavailableRequestIds)
    sameTaskAffinityLocalCompletionCount = [int]$AuditSummary.sameTaskAffinityLocalCompletionCount
    sameTaskAffinityFallbackBlockedRequestIds = @($AuditSummary.sameTaskAffinityFallbackBlockedRequestIds)
    responsesFailedPoolUnavailableCount = [int]$AuditSummary.responsesFailedPoolUnavailableCount
    responsesFailedPoolUnavailableRequestIds = @($AuditSummary.responsesFailedPoolUnavailableRequestIds)
    openPoolWaitCount = [int]$AuditSummary.openPoolWaitCount
    openPoolWaitRequestIds = @($AuditSummary.openPoolWaitRequestIds)
  }
  if ($AuditSummary.sameTaskAffinityLocalCompletionCount -gt 0) {
    $results += Set-MonitorStatus $localCompletion "fail" "同任务 hard-affinity block 不能用本地 completed Responses 伪装成成功闭合" $localCompletionEvidence
  } elseif ($AuditSummary.openPoolWaitCount -gt 0 -and $AuditSummary.responsesLocalCompletionPoolUnavailableCount -eq 0) {
    $results += Set-MonitorStatus $localCompletion "fail" "监测窗口内存在 open pool_wait 且没有本地 completed Responses 终止；这会让 Codex turn 表面不断线但实际停滞" $localCompletionEvidence
  } elseif ($AuditSummary.responsesLocalCompletionPoolUnavailableCount -gt 0) {
    $results += Set-MonitorStatus $localCompletion "pass" "监测窗口内 Codex-facing /v1/responses 以本地 completed Responses 明确闭合 pool_unavailable，避免 response.failed/503/SSE idle" $localCompletionEvidence
  } else {
    $results += Set-MonitorStatus $localCompletion "pass" $null $localCompletionEvidence
  }

  $failedStream = New-MonitorResult "responses_pool_unavailable_failed_stream_absent"
  $failedStreamEvidence = @{
    responsesFailedPoolUnavailableCount = [int]$AuditSummary.responsesFailedPoolUnavailableCount
    responsesFailedPoolUnavailableRequestIds = @($AuditSummary.responsesFailedPoolUnavailableRequestIds)
  }
  if ($AuditSummary.responsesFailedPoolUnavailableCount -gt 0) {
    $results += Set-MonitorStatus $failedStream "fail" "监测窗口内 Codex-facing /v1/responses 仍以 response.failed/pool_unavailable 结束；Codex 会把它视为 fatal stream failure" $failedStreamEvidence
  } else {
    $results += Set-MonitorStatus $failedStream "pass" $null $failedStreamEvidence
  }

  $legacySyntheticTerminal = New-MonitorResult "responses_pool_unavailable_legacy_synthetic_completion_absent"
  $legacySyntheticTerminalEvidence = @{
    inBandSyntheticPoolUnavailableCount = [int]$AuditSummary.inBandSyntheticPoolUnavailableCount
    inBandSyntheticPoolUnavailableRequestIds = @($AuditSummary.inBandSyntheticPoolUnavailableRequestIds)
    heartbeatPoolWaitCount = [int]$AuditSummary.heartbeatPoolWaitCount
    heartbeatPoolWaitRequestIds = @($AuditSummary.heartbeatPoolWaitRequestIds)
  }
  if ($AuditSummary.inBandSyntheticPoolUnavailableCount -gt 0) {
    $results += Set-MonitorStatus $legacySyntheticTerminal "fail" "监测窗口内 Codex-facing /v1/responses 仍使用旧 outcome=in_band_synthetic；应改为可观测的 in_band_local_completion，并保持完整 completed Responses 事件序列" $legacySyntheticTerminalEvidence
  } else {
    $results += Set-MonitorStatus $legacySyntheticTerminal "pass" $null $legacySyntheticTerminalEvidence
  }

  $lineageSwitch = New-MonitorResult "turn_lineage_account_switch_absent"
  $lineageSwitchEvidence = @{
    lineageAccountSwitchCount = [int]$AuditSummary.lineageAccountSwitchCount
    hardAffinityLineageAccountSwitchCount = [int]$AuditSummary.hardAffinityLineageAccountSwitchCount
    metadataOnlyLineageAccountSwitchCount = [int]$AuditSummary.metadataOnlyLineageAccountSwitchCount
    continuationReroutedCount = [int]$AuditSummary.continuationReroutedCount
    autoCompactReroutedCount = [int]$AuditSummary.autoCompactReroutedCount
    lineageAccountSwitches = @($AuditSummary.lineageAccountSwitches)
    hardAffinityLineageAccountSwitches = @($AuditSummary.hardAffinityLineageAccountSwitches)
    metadataOnlyLineageAccountSwitches = @($AuditSummary.metadataOnlyLineageAccountSwitches)
    continuationReroutes = @($AuditSummary.continuationReroutes)
    autoCompactReroutes = @($AuditSummary.autoCompactReroutes)
  }
  if ($AuditSummary.hardAffinityLineageAccountSwitchCount -gt 0 -or $AuditSummary.continuationReroutedCount -gt 0) {
    $results += Set-MonitorStatus $lineageSwitch "fail" "同一 sticky turn/response lineage 在监测窗口内命中过多个账号；x-codex-turn-state/previous_response_id 必须保持同账号恢复边界" $lineageSwitchEvidence
  } elseif ($AuditSummary.metadataOnlyLineageAccountSwitchCount -gt 0) {
    $results += Set-MonitorStatus $lineageSwitch "warn" "观察到 metadata-only lineage fallback 使用了多个账号；这会消耗账号池，但 x-codex-turn-metadata 不是 hard-affinity token" $lineageSwitchEvidence
  } else {
    $results += Set-MonitorStatus $lineageSwitch "pass" $null $lineageSwitchEvidence
  }

  $fieldCoverage = New-MonitorResult "audit_lineage_fields_present"
  $fieldCoverageEvidence = @{
    auditFieldCoverage = $AuditSummary.auditFieldCoverage
  }
  if ($AuditSummary.eventCount -eq 0) {
    $results += Set-MonitorStatus $fieldCoverage "skipped" "监测窗口内没有 audit 事件，无法评估 lineage 字段覆盖率" $fieldCoverageEvidence
  } elseif (-not $AuditSummary.auditFieldCoverage.lineageDiagnosticReady) {
    $results += Set-MonitorStatus $fieldCoverage "warn" "audit 缺少 gateway_request_id/turn_lineage_id 等字段，无法准确定位 continuation/auto-compact/恢复失败根因" $fieldCoverageEvidence
  } else {
    $results += Set-MonitorStatus $fieldCoverage "pass" $null $fieldCoverageEvidence
  }

  $clientAbort = New-MonitorResult "client_aborted_streams_classified"
  $clientAbortEvidence = @{
    clientAbortedStreamCount = [int]$AuditSummary.clientAbortedStreamCount
    clientAbortedBeforeFirstChunkCount = [int]$AuditSummary.clientAbortedBeforeFirstChunkCount
    clientAbortedAfterFirstChunkCount = [int]$AuditSummary.clientAbortedAfterFirstChunkCount
    clientAbortedStreamSummaries = @($AuditSummary.clientAbortedStreamSummaries)
  }
  if ($AuditSummary.clientAbortedStreamCount -gt 0) {
    $results += Set-MonitorStatus $clientAbort "blocked" "观察到 client_aborted；已分类到 report，但仅凭 audit 不能归因为客户端主动中断、恢复失败或服务端异常" $clientAbortEvidence
  } else {
    $results += Set-MonitorStatus $clientAbort "pass" $null $clientAbortEvidence
  }

  $cli = New-MonitorResult "codex_cli_config_auth_untouched"
  $cliEvidence = @{
    unchanged = [bool]$CliComparison.unchanged
    changedFiles = @($CliComparison.changedFiles)
  }
  if ($CliComparison.unchanged) {
    $results += Set-MonitorStatus $cli "pass" $null $cliEvidence
  } elseif ($RequireCliConfigUntouched) {
    $results += Set-MonitorStatus $cli "fail" "当前 Codex CLI 的 config.toml/auth.json 在监测期间发生变化" $cliEvidence
  } else {
    $results += Set-MonitorStatus $cli "skipped" "未要求 CLI config/auth 必须不变；仅记录变化" $cliEvidence
  }

  $app = New-MonitorResult "codex_app_process_stable"
  $appEvidence = @{
    stable = [bool]$AppComparison.stable
    beforeCount = [int]$AppComparison.beforeCount
    afterCount = [int]$AppComparison.afterCount
  }
  if ($AppComparison.stable) {
    $results += Set-MonitorStatus $app "pass" $null $appEvidence
  } elseif ($RequireAppStable) {
    $results += Set-MonitorStatus $app "fail" "Codex App 进程集合在监测期间发生变化" $appEvidence
  } else {
    $results += Set-MonitorStatus $app "skipped" "未要求 App 进程集合必须稳定；仅记录变化" $appEvidence
  }

  $results
}

function New-ContinuitySummary {
  param([System.Collections.IDictionary]$AuditSummary)

  $currentEvidence = [ordered]@{
    has429 = [bool]$AuditSummary.has429
    hasUsageLimitReached = [bool]$AuditSummary.hasUsageLimitReached
    hasModelCooldownApplied = [bool]$AuditSummary.hasModelCooldownApplied
    hasFallbackBlocked = [bool]$AuditSummary.hasFallbackBlocked
    hasHardAffinityFallbackBlocked = [bool]$AuditSummary.hasHardAffinityFallbackBlocked
    sameTaskAffinityFallbackBlockedCount = [int]$AuditSummary.sameTaskAffinityFallbackBlockedCount
    sameTaskAffinityLocalCompletionCount = [int]$AuditSummary.sameTaskAffinityLocalCompletionCount
    sameTaskAffinityTerminalCompletionCount = [int]$AuditSummary.sameTaskAffinityTerminalCompletionCount
    sameTaskAffinityUnclosedBlockCount = [int]$AuditSummary.sameTaskAffinityUnclosedBlockCount
    sameTaskAffinityUnrecoveredTerminal429Count = [int]$AuditSummary.sameTaskAffinityUnrecoveredTerminal429Count
    sameTaskAffinityStructuredQuotaTerminal429Count = [int]$AuditSummary.sameTaskAffinityStructuredQuotaTerminal429Count
    sameTaskAffinityUnstructuredTerminal429Count = [int]$AuditSummary.sameTaskAffinityUnstructuredTerminal429Count
    sameTaskAffinityFallbackBlockedRequestIds = @($AuditSummary.sameTaskAffinityFallbackBlockedRequestIds)
    sameTaskAffinityTerminalCompletionRequestIds = @($AuditSummary.sameTaskAffinityTerminalCompletionRequestIds)
    sameTaskAffinityUnclosedBlockRequestIds = @($AuditSummary.sameTaskAffinityUnclosedBlockRequestIds)
    sameTaskAffinityUnrecoveredTerminal429RequestIds = @($AuditSummary.sameTaskAffinityUnrecoveredTerminal429RequestIds)
    requiredFallbackCycles = [int]$RequiredFallbackCycles
    retryLimitErrorFound = [bool]$AuditSummary.retryLimitErrorFound
    first429AccountHash = $AuditSummary.first429AccountHash
  }
  $currentStatus = "blocked"
  $currentReason = "未在监测窗口内观察到同任务 429 -> cooldown -> fallback_blocked(hard_affinity)"
  if ($AuditSummary.sameTaskAffinityLocalCompletionCount -gt 0) {
    $currentStatus = "fail"
    $currentReason = "hard-affinity block 后被本地 pool_unavailable completed 闭合"
  } elseif ($AuditSummary.sameTaskAffinityUnstructuredTerminal429Count -gt 0) {
    $currentStatus = "fail"
    $currentReason = "hard-affinity block 后以非结构化 final 429 结束"
  } elseif ($AuditSummary.sameTaskAffinityUnclosedBlockCount -gt 0) {
    $currentStatus = "blocked"
    $currentReason = "hard-affinity block 已出现但监测窗口内没有 terminal/local completion"
  } elseif ($AuditSummary.retryLimitErrorFound) {
    $currentStatus = "fail"
    $currentReason = "hard-affinity block 后仍出现 retry-limit 或可切号 fallback 的 final 429"
  } elseif ($AuditSummary.has429 -and $AuditSummary.hasUsageLimitReached -and $AuditSummary.hasModelCooldownApplied -and ($AuditSummary.sameTaskAffinityTerminalCompletionCount + $AuditSummary.sameTaskAffinityStructuredQuotaTerminal429Count) -ge $RequiredFallbackCycles) {
    $currentStatus = "pass"
    $currentReason = $null
  }

  $newRequestEvidence = [ordered]@{
    blockedAccountCount = [int]$AuditSummary.blockedAccountCount
    blockedAccountHashes = @($AuditSummary.blockedAccountHashes)
    newRequestAvoidanceCount = [int]$AuditSummary.newRequestAvoidanceCount
    newRequestAvoidanceRequestIds = @($AuditSummary.newRequestAvoidanceRequestIds)
    newRequestBlockedReuseCount = [int]$AuditSummary.newRequestBlockedReuseCount
    newRequestBlockedReuseRequestIds = @($AuditSummary.newRequestBlockedReuseRequestIds)
  }
  $newRequestStatus = "blocked"
  $newRequestReason = "未观察到后续新请求避开 exhausted/cooldown 账号"
  if ($AuditSummary.newRequestBlockedReuseCount -gt 0) {
    $newRequestStatus = "fail"
    $newRequestReason = "后续新请求仍命中过已 exhausted/cooldown 的账号"
  } elseif ($AuditSummary.newRequestAvoidanceCount -gt 0) {
    $newRequestStatus = "pass"
    $newRequestReason = $null
  } elseif (-not $RequireQuotaFallback) {
    $newRequestStatus = "skipped"
    $newRequestReason = "未要求观察新请求避开 exhausted/cooldown 账号"
  }

  $lineageEvidence = [ordered]@{
    lineageCount = [int]$AuditSummary.lineageCount
    lineageAccountSwitchCount = [int]$AuditSummary.lineageAccountSwitchCount
    hardAffinityLineageAccountSwitchCount = [int]$AuditSummary.hardAffinityLineageAccountSwitchCount
    metadataOnlyLineageAccountSwitchCount = [int]$AuditSummary.metadataOnlyLineageAccountSwitchCount
    continuationReroutedCount = [int]$AuditSummary.continuationReroutedCount
    autoCompactReroutedCount = [int]$AuditSummary.autoCompactReroutedCount
    lineageAccountSwitches = @($AuditSummary.lineageAccountSwitches)
    hardAffinityLineageAccountSwitches = @($AuditSummary.hardAffinityLineageAccountSwitches)
    metadataOnlyLineageAccountSwitches = @($AuditSummary.metadataOnlyLineageAccountSwitches)
    continuationReroutes = @($AuditSummary.continuationReroutes)
    autoCompactReroutes = @($AuditSummary.autoCompactReroutes)
  }
  $lineageStatus = "pass"
  $lineageReason = $null
  if ($AuditSummary.hardAffinityLineageAccountSwitchCount -gt 0 -or $AuditSummary.continuationReroutedCount -gt 0) {
    $lineageStatus = "fail"
    $lineageReason = "同一 sticky turn/response lineage 使用了多个账号"
  } elseif ($AuditSummary.metadataOnlyLineageAccountSwitchCount -gt 0) {
    $lineageStatus = "warn"
    $lineageReason = "metadata-only lineage fallback 使用了多个账号；不是 hard-affinity 违规"
  } elseif ($AuditSummary.lineageCount -eq 0) {
    $lineageStatus = "blocked"
    $lineageReason = "未观察到 turn lineage 字段；只能给出 request 级结论"
  }

  [ordered]@{
    sameTaskAffinityFallbackBlocked = [ordered]@{
      status = $currentStatus
      reason = $currentReason
      evidence = $currentEvidence
    }
    newRequestAvoidsExhaustedCooldown = [ordered]@{
      status = $newRequestStatus
      reason = $newRequestReason
      evidence = $newRequestEvidence
    }
    turnLineageAccountSwitchAbsent = [ordered]@{
      status = $lineageStatus
      reason = $lineageReason
      evidence = $lineageEvidence
    }
    accountExhaustionInFlightContinuity = [ordered]@{
      status = if ($AuditSummary.terminalErrorAfterAccountExhaustionCount -gt 0 -or $AuditSummary.interruptedAfterAccountExhaustionCount -gt 0) { "fail" } elseif ($AuditSummary.clientAbortedAfterAccountExhaustionCount -gt 0 -or $AuditSummary.openAfterAccountExhaustionCount -gt 0) { "blocked" } elseif ($AuditSummary.inFlightAtAccountExhaustionCount -gt 0 -and $AuditSummary.completedAfterAccountExhaustionCount -eq $AuditSummary.inFlightAtAccountExhaustionCount) { "pass" } elseif ($AuditSummary.accountExhaustionContinuityCount -gt 0) { "skipped" } else { "skipped" }
      reason = if ($AuditSummary.terminalErrorAfterAccountExhaustionCount -gt 0 -or $AuditSummary.interruptedAfterAccountExhaustionCount -gt 0) { "in-flight stream hit terminal error or cooldown interrupt after account exhaustion" } elseif ($AuditSummary.clientAbortedAfterAccountExhaustionCount -gt 0) { "in-flight stream was client_aborted after account exhaustion" } elseif ($AuditSummary.openAfterAccountExhaustionCount -gt 0) { "in-flight stream remained open at the end of the monitor window" } elseif ($AuditSummary.accountExhaustionContinuityCount -gt 0 -and $AuditSummary.inFlightAtAccountExhaustionCount -eq 0) { "account exhaustion observed without in-flight streams at that instant" } else { $null }
      evidence = [ordered]@{
        accountExhaustionContinuityCount = [int]$AuditSummary.accountExhaustionContinuityCount
        inFlightAtAccountExhaustionCount = [int]$AuditSummary.inFlightAtAccountExhaustionCount
        completedAfterAccountExhaustionCount = [int]$AuditSummary.completedAfterAccountExhaustionCount
        terminalErrorAfterAccountExhaustionCount = [int]$AuditSummary.terminalErrorAfterAccountExhaustionCount
        clientAbortedAfterAccountExhaustionCount = [int]$AuditSummary.clientAbortedAfterAccountExhaustionCount
        interruptedAfterAccountExhaustionCount = [int]$AuditSummary.interruptedAfterAccountExhaustionCount
        openAfterAccountExhaustionCount = [int]$AuditSummary.openAfterAccountExhaustionCount
        accountExhaustionContinuitySummaries = @($AuditSummary.accountExhaustionContinuitySummaries)
      }
    }
    stickyResetWaitNotKilledByLocalTimeout = [ordered]@{
      status = if ($AuditSummary.stickyResetWaitExceededInlineBudgetCount -gt 0) { "fail" } elseif ($AuditSummary.stickyResetWaitKilledByLocalTimeoutCount -gt 0) { "fail" } elseif ($AuditSummary.stickyResetWaitRecoveredCount -gt 0) { "pass" } elseif ($AuditSummary.stickyResetWaitRequestCount -gt 0) { "blocked" } else { "skipped" }
      reason = if ($AuditSummary.stickyResetWaitExceededInlineBudgetCount -gt 0) { "hard-affinity continuation requested an inline wait beyond the 3 second cap" } elseif ($AuditSummary.stickyResetWaitKilledByLocalTimeoutCount -gt 0) { "hard-affinity continuation reset wait hit local timeout before recovery" } elseif ($AuditSummary.stickyResetWaitRequestCount -gt 0 -and $AuditSummary.stickyResetWaitRecoveredCount -eq 0) { "hard-affinity continuation observed without terminal recovery in this window" } else { $null }
      evidence = [ordered]@{
        requestCount = [int]$AuditSummary.stickyResetWaitRequestCount
        recoveredCount = [int]$AuditSummary.stickyResetWaitRecoveredCount
        killedByLocalTimeoutCount = [int]$AuditSummary.stickyResetWaitKilledByLocalTimeoutCount
        exceededInlineBudgetCount = [int]$AuditSummary.stickyResetWaitExceededInlineBudgetCount
        requests = @($AuditSummary.stickyResetWaitRequests)
      }
    }
  }
}

function New-QuotaContinuityVerdict {
  param([System.Collections.IDictionary]$AuditSummary)

  $evidence = [ordered]@{
    has429 = [bool]$AuditSummary.has429
    hasUsageLimitReached = [bool]$AuditSummary.hasUsageLimitReached
    hasModelCooldownApplied = [bool]$AuditSummary.hasModelCooldownApplied
    fallbackSelectedCount = [int]$AuditSummary.fallbackSelectedCount
    sameTaskAffinityFallbackBlockedCount = [int]$AuditSummary.sameTaskAffinityFallbackBlockedCount
    sameTaskAffinityTerminalCompletionCount = [int]$AuditSummary.sameTaskAffinityTerminalCompletionCount
    sameTaskAffinityStructuredQuotaTerminal429Count = [int]$AuditSummary.sameTaskAffinityStructuredQuotaTerminal429Count
    sameTaskAffinityLocalCompletionCount = [int]$AuditSummary.sameTaskAffinityLocalCompletionCount
    sameTaskAffinityUnstructuredTerminal429Count = [int]$AuditSummary.sameTaskAffinityUnstructuredTerminal429Count
    newRequestAvoidanceCount = [int]$AuditSummary.newRequestAvoidanceCount
    newRequestBlockedReuseCount = [int]$AuditSummary.newRequestBlockedReuseCount
    upstreamStreamErrorCount = [int]$AuditSummary.upstreamStreamErrorCount
    quotaMetadataCoverage = $AuditSummary.quotaMetadataCoverage
  }
  $status = "not_observed"
  $reason = "监测窗口内未观察到完整 quota exhaustion 链路"
  $plusLike = $false
  $terminalQuota = $false

  if ($AuditSummary.upstreamStreamErrorCount -gt 0) {
    $status = "fail"
    $reason = "已接纳 stream 出现 upstream_stream_error；无法证明 Plus-like in-flight continuation"
  } elseif ($AuditSummary.sameTaskAffinityLocalCompletionCount -gt 0) {
    $status = "fail"
    $reason = "同任务 hard-affinity block 被本地 completed 响应闭合，不是上游真实持续完成"
  } elseif ($AuditSummary.sameTaskAffinityUnstructuredTerminal429Count -gt 0 -or $AuditSummary.retryLimitErrorFound) {
    $status = "fail"
    $reason = "同任务 hard-affinity block 后出现非结构化 terminal 429/retry-limit"
  } elseif ($AuditSummary.sameTaskAffinityStructuredQuotaTerminal429Count -gt 0) {
    $status = "terminal_quota"
    $terminalQuota = $true
    $reason = "同任务以结构化 usage_limit_reached terminal 429 闭合；这是明确配额终态，不是 Plus-like 持续完成"
  } elseif ($AuditSummary.has429 -and $AuditSummary.hasUsageLimitReached -and $AuditSummary.hasModelCooldownApplied -and $AuditSummary.sameTaskAffinityTerminalCompletionCount -ge $RequiredFallbackCycles -and $AuditSummary.newRequestAvoidanceCount -gt 0) {
    $status = "plus_like"
    $plusLike = $true
    $reason = $null
  } elseif ($AuditSummary.has429 -and $AuditSummary.hasUsageLimitReached -and $AuditSummary.hasModelCooldownApplied) {
    $status = "blocked"
    $reason = "已观察到 quota/cooldown，但尚未同时证明同任务完成与新请求避开 exhausted 账号"
  }

  [ordered]@{
    status = $status
    reason = $reason
    plusLike = [bool]$plusLike
    terminalQuota = [bool]$terminalQuota
    evidence = $evidence
  }
}

function New-HostDriftVerdict {
  param(
    [System.Collections.IDictionary]$CliComparison,
    [System.Collections.IDictionary]$AppComparison
  )

  $cliUnchanged = [bool]$CliComparison.unchanged
  $appStable = [bool]$AppComparison.stable
  $status = "pass"
  $reason = $null
  if (-not $cliUnchanged -and -not $appStable) {
    $status = "fail"
    $reason = "Codex CLI config/auth 文件和 Codex App 进程集合均发生变化"
  } elseif (-not $cliUnchanged) {
    $status = "fail"
    $reason = "Codex CLI config/auth 文件发生变化"
  } elseif (-not $appStable) {
    $status = "fail"
    $reason = "Codex App 进程集合发生变化"
  }

  [ordered]@{
    status = $status
    reason = $reason
    evidence = [ordered]@{
      cliConfigUnchanged = $cliUnchanged
      cliChangedFiles = @($CliComparison.changedFiles)
      codexAppStable = $appStable
      codexAppBeforeCount = [int]$AppComparison.beforeCount
      codexAppAfterCount = [int]$AppComparison.afterCount
    }
  }
}

function New-CriticalSignals {
  param(
    [System.Collections.IDictionary]$ApiServiceRuntime,
    [datetime]$GeneratedAt
  )
  $signals = @()
  if ($null -eq $ApiServiceRuntime) {
    return @($signals)
  }

  $localAccess = $ApiServiceRuntime.localAccess
  $runtimeMode = $ApiServiceRuntime.runtimeMode
  $listener = $ApiServiceRuntime.listener
  $runtimeModeName = if ($runtimeMode) { $runtimeMode.mode } else { $null }
  $localAccessEnabled = if ($localAccess) { $localAccess.enabled } else { $null }
  $initialRuntime = $script:InitialApiServiceRuntime
  $initialRuntimeMode = if ($initialRuntime) { $initialRuntime.runtimeMode } else { $null }
  $initialLocalAccess = if ($initialRuntime) { $initialRuntime.localAccess } else { $null }
  $initialExpectedApiServiceRuntime = (
    ($initialRuntime -and $initialRuntime.available) -or
    ($initialRuntimeMode -and $initialRuntimeMode.mode -eq "cockpit_api_service") -or
    ($initialLocalAccess -and $initialLocalAccess.enabled -eq $true)
  )
  $expectedApiServiceRuntime = (
    [bool]$RequireApiServiceRuntimeAvailable -or
    [bool]$initialExpectedApiServiceRuntime -or
    $runtimeModeName -eq "cockpit_api_service" -or
    $localAccessEnabled -eq $true
  )

  if (-not $ApiServiceRuntime.available -and ($expectedApiServiceRuntime -or ($localAccess -and $localAccess.exists))) {
    $signals += [ordered]@{
      name = "api_service_runtime_unavailable"
      severity = if ($expectedApiServiceRuntime) { "critical" } else { "warning" }
      reason = $ApiServiceRuntime.reason
      generatedAt = $GeneratedAt.ToString("o")
      required = [bool]$RequireApiServiceRuntimeAvailable
      expectedRuntime = [bool]$expectedApiServiceRuntime
      apiBaseUrl = $ApiServiceRuntime.apiBaseUrl
      localAccess = $localAccess
      runtimeMode = $runtimeMode
      initial = [ordered]@{
        available = if ($initialRuntime) { [bool]$initialRuntime.available } else { $null }
        reason = if ($initialRuntime) { $initialRuntime.reason } else { $null }
        localAccess = $initialLocalAccess
        runtimeMode = $initialRuntimeMode
      }
      listenerCount = if ($listener) { [int]$listener.listenerCount } else { 0 }
      server = $ApiServiceRuntime.server
    }
  }

  @($signals)
}

function New-MonitorReport {
  param(
    [datetime]$StartedAt,
    [datetime]$EndedAt,
    [string]$ReportStatus,
    [string]$TerminationReason,
    [object[]]$Events,
    [int]$RawObservedEventCount = 0,
    [int]$WindowDroppedEventCount = 0,
    [System.Collections.IDictionary]$CliBefore,
    [System.Collections.IDictionary]$CliAfter,
    [System.Collections.IDictionary]$AppBefore,
    [System.Collections.IDictionary]$AppAfter,
    [string]$ReportPath,
    [string]$CheckpointPath
  )

  $cliComparison = Compare-FileGuardState $CliBefore $CliAfter
  $appComparison = Compare-CodexAppProcessState $AppBefore $AppAfter
  $apiServiceRuntime = Get-ApiServiceRuntimeState
  $auditSummary = Get-AuditAcceptanceSummary $Events
  $results = New-AcceptanceResults -AuditSummary $auditSummary -CliComparison $cliComparison -AppComparison $appComparison -ApiServiceRuntime $apiServiceRuntime
  $continuitySummary = New-ContinuitySummary $auditSummary
  $quotaContinuityVerdict = New-QuotaContinuityVerdict $auditSummary
  $hostDriftVerdict = New-HostDriftVerdict -CliComparison $cliComparison -AppComparison $appComparison
  $criticalSignals = New-CriticalSignals -ApiServiceRuntime $apiServiceRuntime -GeneratedAt $EndedAt
  $overall = if ($results | Where-Object { $_.status -eq "fail" }) {
    "fail"
  } elseif ($results | Where-Object { $_.status -eq "blocked" }) {
    "blocked"
  } else {
    "pass"
  }

  [ordered]@{
    schemaVersion = 1
    generatedAt = $EndedAt.ToString("o")
    overall = $overall
    reportStatus = $ReportStatus
    terminationReason = $TerminationReason
    mode = "live_codex_app_monitor"
    startedAt = $StartedAt.ToString("o")
    endedAt = $EndedAt.ToString("o")
    elapsedSeconds = [math]::Round(($EndedAt - $StartedAt).TotalSeconds, 1)
    dataRoot = $DataRoot
    auditPath = $AuditPath
    auditWindow = [ordered]@{
      sinceTimestampMs = if ($AuditSinceTimestampMs -gt 0) { [int64]$AuditSinceTimestampMs } else { $null }
      untilTimestampMs = if ($AuditUntilTimestampMs -gt 0) { [int64]$AuditUntilTimestampMs } else { $null }
      focusGatewayRequestIds = @($FocusGatewayRequestIds | Where-Object { -not [string]::IsNullOrWhiteSpace($_) })
      rawObservedEventCount = [int]$RawObservedEventCount
      filteredEventCount = [int]$Events.Count
      droppedEventCount = [int]$WindowDroppedEventCount
    }
    exitCodeFile = $ExitCodeFile
    checkpointPath = $CheckpointPath
    reportPath = $ReportPath
    includeExistingAudit = [bool]$IncludeExistingAudit
    requireQuotaFallback = [bool]$RequireQuotaFallback
    requireStreamCompletion = [bool]$RequireStreamCompletion
    requireCliConfigUntouched = [bool]$RequireCliConfigUntouched
    requireAppStable = [bool]$RequireAppStable
    requireApiServiceRuntimeAvailable = [bool]$RequireApiServiceRuntimeAvailable
    requiredFallbackCycles = [int]$RequiredFallbackCycles
    requiredDistinctHealthyAccounts = [int]$RequiredDistinctHealthyAccounts
    requiredCompletedStreams = [int]$RequiredCompletedStreams
    retryLimitRegressionCheck = "always_on"
    responsesPoolUnavailableTransport503RegressionCheck = "always_on"
    responsesPoolUnavailableFailedStreamRegressionCheck = "always_on"
    responsesPoolUnavailableSyntheticCompletionRegressionCheck = "always_on"
    poolWaitTerminalProgressRegressionCheck = "always_on"
    temporaryConfig = [ordered]@{
      managedByThisScript = $false
      restored = "not_applicable"
      reason = "live monitor is read-only; provider switching/restoration is performed outside this script"
    }
    codexCliGuard = [ordered]@{
      before = $CliBefore
      after = $CliAfter
      comparison = $cliComparison
    }
    codexAppGuard = [ordered]@{
      before = $AppBefore
      after = $AppAfter
      comparison = $appComparison
    }
    apiServiceRuntime = $apiServiceRuntime
    apiServiceRuntimeInitial = $script:InitialApiServiceRuntime
    criticalSignals = @($criticalSignals)
    audit = $auditSummary
    continuitySummary = $continuitySummary
    quotaContinuityVerdict = $quotaContinuityVerdict
    hostDriftVerdict = $hostDriftVerdict
    results = $results
    safetyNotes = @(
      "this live monitor is read-only",
      "it hashes ~/.codex/config.toml and ~/.codex/auth.json but does not read or write their contents",
      "it does not start, stop, restart, or kill Codex App, Codex CLI, or Cockpit services",
      "it does not switch providers or restore manual provider changes",
      "use the App-safe isolated acceptance script when temporary provider config must be created and restored automatically"
    )
  }
}

function Write-MonitorJsonFile {
  param(
    [System.Collections.IDictionary]$Report,
    [string]$Path
  )
  if ([string]::IsNullOrWhiteSpace($Path)) {
    return
  }
  $dir = Split-Path -Parent $Path
  if (-not [string]::IsNullOrWhiteSpace($dir)) {
    New-Item -ItemType Directory -Force -Path $dir | Out-Null
  }
  $Report | ConvertTo-Json -Depth 24 | Set-Content -LiteralPath $Path -Encoding UTF8
}

if (-not $AuditPath) {
  $AuditPath = Join-Path $DataRoot "codex_local_access_audit.jsonl"
}

$startedAt = Get-Date
$deadline = $startedAt.AddSeconds($DurationSeconds)
$offset = Get-InitialAuditOffset $AuditPath
$events = @()
$rawObservedEventCount = 0
$windowDroppedEventCount = 0
$cliBefore = Get-FileGuardState $CodexHome
$appBefore = Get-CodexAppProcessState $CodexAppProcessNames $CodexAppPathIncludePatterns $CodexAppPathExcludePatterns
$script:InitialApiServiceRuntime = Get-ApiServiceRuntimeState
$terminationReason = "deadline"
$lastCheckpointAt = [datetime]::MinValue
if ($WriteReport) {
  New-Item -ItemType Directory -Force -Path $ReportDir | Out-Null
  if ([string]::IsNullOrWhiteSpace($CheckpointPath)) {
    $CheckpointPath = Join-Path $ReportDir "live-monitor-checkpoint.json"
  }
}

if (-not $Quiet) {
  Write-Host ("monitoring audit={0}" -f $AuditPath)
  Write-Host ("started_at={0}; duration_seconds={1}; include_existing_audit={2}" -f $startedAt.ToString("o"), $DurationSeconds, [bool]$IncludeExistingAudit)
  if ($AuditSinceTimestampMs -gt 0 -or $AuditUntilTimestampMs -gt 0 -or @($FocusGatewayRequestIds).Count -gt 0) {
    Write-Host ("audit_window since_ms={0}; until_ms={1}; focus_gateway_request_ids={2}" -f $AuditSinceTimestampMs, $AuditUntilTimestampMs, (@($FocusGatewayRequestIds) -join ","))
  }
}

do {
  $lines = Read-NewAuditLines -Path $AuditPath -Offset ([ref]$offset)
  $wroteEventsThisPoll = $false
  if ($lines.Count -gt 0) {
    $newEvents = @($lines | ForEach-Object { Convert-AuditLine $_ })
    $rawObservedEventCount += $newEvents.Count
    $windowEvents = Select-AuditWindowEvents $newEvents
    $windowDroppedEventCount += [math]::Max(0, $newEvents.Count - $windowEvents.Count)
    $events += $windowEvents
    $wroteEventsThisPoll = $windowEvents.Count -gt 0
    if (-not $Quiet) {
      foreach ($event in $windowEvents) {
        Write-Host (Format-AuditRealtimeEventLine $event)
      }
      if ($newEvents.Count -ne $windowEvents.Count) {
        Write-Host ("audit_window dropped={0}; kept={1}; raw={2}" -f ($newEvents.Count - $windowEvents.Count), $windowEvents.Count, $newEvents.Count)
      }
      $summary = Get-AuditAcceptanceSummary $events
      Write-Host ("events={0}; has429={1}; fallbackBlocked={2}; sameTaskLocalCompletion={3}; newRequestAvoidance={4}; healthyAccountsAfterBlock={5}; streams={6}/{7}; retryLimit={8}; poolWait={9}; heartbeatPoolWait={10}; activeDrainWait={11}; openPoolWait={12}; poolUnavailable={13}; localCompletionPoolUnavailable={14}; failedPoolUnavailable={15}; responses503={16}; parkedPoolWait={17}; sseIdle={18}; lineageSwitch={19}; continuationReroute={20}; autoCompactReroute={21}; behaviorTrace={22}; stickyResetRecovered={23}; stickyResetTimeoutKill={24}" -f $summary.eventCount, $summary.has429, $summary.sameTaskAffinityFallbackBlockedCount, $summary.sameTaskAffinityLocalCompletionCount, $summary.newRequestAvoidanceCount, $summary.distinctHealthyAccountCountAfterBlock, $summary.completedStreamCount, $summary.startedStreamCount, $summary.retryLimitErrorFound, $summary.poolWaitCount, $summary.heartbeatPoolWaitCount, $summary.activeDrainPoolWaitCount, $summary.openPoolWaitCount, $summary.localPoolUnavailableCount, $summary.responsesLocalCompletionPoolUnavailableCount, $summary.responsesFailedPoolUnavailableCount, $summary.responsesTransport503PoolUnavailableCount, $summary.parkedPoolWaitCount, $summary.sseIdleErrorCount, $summary.lineageAccountSwitchCount, $summary.continuationReroutedCount, $summary.autoCompactReroutedCount, $summary.behaviorTraceCoverage.hasStructuredBehaviorTrace, $summary.stickyResetWaitRecoveredCount, $summary.stickyResetWaitKilledByLocalTimeoutCount)
    }
  }

  if ($WriteReport) {
    $nowForCheckpoint = Get-Date
    if ($wroteEventsThisPoll -or ($nowForCheckpoint - $lastCheckpointAt).TotalSeconds -ge $CheckpointIntervalSeconds) {
      $cliCurrent = Get-FileGuardState $CodexHome
      $appCurrent = Get-CodexAppProcessState $CodexAppProcessNames $CodexAppPathIncludePatterns $CodexAppPathExcludePatterns
      $checkpoint = New-MonitorReport `
        -StartedAt $startedAt `
        -EndedAt $nowForCheckpoint `
        -ReportStatus "running" `
        -TerminationReason "running" `
        -Events $events `
        -RawObservedEventCount $rawObservedEventCount `
        -WindowDroppedEventCount $windowDroppedEventCount `
        -CliBefore $cliBefore `
        -CliAfter $cliCurrent `
        -AppBefore $appBefore `
        -AppAfter $appCurrent `
        -ReportPath $null `
        -CheckpointPath $CheckpointPath
      Write-MonitorJsonFile -Report $checkpoint -Path $CheckpointPath
      if (-not $Quiet) {
        Write-Host ("checkpoint apiAvailable={0}; apiReason={1}; runtimeMode={2}; localAccessEnabled={3}; listenerCount={4}; criticalSignals={5}" -f $checkpoint.apiServiceRuntime.available, $checkpoint.apiServiceRuntime.reason, $checkpoint.apiServiceRuntime.runtimeMode.mode, $checkpoint.apiServiceRuntime.localAccess.enabled, $checkpoint.apiServiceRuntime.listener.listenerCount, @($checkpoint.criticalSignals).Count)
      }
      $lastCheckpointAt = $nowForCheckpoint
    }
  }

  if ($StopWhenSatisfied) {
    $summaryNow = Get-AuditAcceptanceSummary $events
    $quotaSatisfied = (-not $RequireQuotaFallback) -or ($summaryNow.has429 -and $summaryNow.hasUsageLimitReached -and $summaryNow.hasModelCooldownApplied -and ($summaryNow.sameTaskAffinityTerminalCompletionCount + $summaryNow.sameTaskAffinityStructuredQuotaTerminal429Count) -ge $RequiredFallbackCycles -and $summaryNow.sameTaskAffinityLocalCompletionCount -eq 0 -and $summaryNow.newRequestAvoidanceCount -gt 0 -and $summaryNow.distinctHealthyAccountCountAfterBlock -ge $RequiredDistinctHealthyAccounts)
    $streamSatisfied = (-not $RequireStreamCompletion) -or ($summaryNow.completedStreamCount -ge $RequiredCompletedStreams)
    $retrySatisfied = (-not $summaryNow.retryLimitErrorFound)
    $responses503Satisfied = (-not $summaryNow.responsesTransport503PoolUnavailableCount)
    $localCompletionSatisfied = (-not $summaryNow.openPoolWaitCount) -or $summaryNow.responsesLocalCompletionPoolUnavailableCount -gt 0
    $failedStreamSatisfied = (-not $summaryNow.responsesFailedPoolUnavailableCount)
    $legacySyntheticTerminalSatisfied = (-not $summaryNow.inBandSyntheticPoolUnavailableCount)
    $poolWaitProgressSatisfied = (-not $summaryNow.openPoolWaitCount)
    $stickyResetSatisfied = (-not $summaryNow.stickyResetWaitKilledByLocalTimeoutCount) -and (-not $summaryNow.stickyResetWaitExceededInlineBudgetCount)
    $runtimeSatisfied = (-not $RequireApiServiceRuntimeAvailable) -or (Get-ApiServiceRuntimeState).available
    if ($quotaSatisfied -and $streamSatisfied -and $retrySatisfied -and $responses503Satisfied -and $localCompletionSatisfied -and $failedStreamSatisfied -and $legacySyntheticTerminalSatisfied -and $poolWaitProgressSatisfied -and $stickyResetSatisfied -and $runtimeSatisfied) {
      $terminationReason = "stop_when_satisfied"
      break
    }
  }

  if ($DurationSeconds -le 0) {
    $terminationReason = "duration_or_zero"
    break
  }
  if ($StopSignalFile -and (Test-Path -LiteralPath $StopSignalFile)) {
    $terminationReason = "stop_signal_file"
    if (-not $Quiet) {
      Write-Host ("stop_signal_file_detected={0}" -f $StopSignalFile)
    }
    break
  }
  Start-Sleep -Seconds $PollIntervalSeconds
} while ((Get-Date) -lt $deadline)

$endedAt = Get-Date
$cliAfter = Get-FileGuardState $CodexHome
$appAfter = Get-CodexAppProcessState $CodexAppProcessNames $CodexAppPathIncludePatterns $CodexAppPathExcludePatterns
$reportPath = $null
if ($WriteReport) {
  New-Item -ItemType Directory -Force -Path $ReportDir | Out-Null
  $stamp = Get-Date -Format "yyyyMMdd-HHmmss"
  $reportPath = Join-Path $ReportDir ("live-monitor-{0}.json" -f $stamp)
}

$report = New-MonitorReport `
  -StartedAt $startedAt `
  -EndedAt $endedAt `
  -ReportStatus "completed" `
  -TerminationReason $terminationReason `
  -Events $events `
  -RawObservedEventCount $rawObservedEventCount `
  -WindowDroppedEventCount $windowDroppedEventCount `
  -CliBefore $cliBefore `
  -CliAfter $cliAfter `
  -AppBefore $appBefore `
  -AppAfter $appAfter `
  -ReportPath $reportPath `
  -CheckpointPath $CheckpointPath

if ($WriteReport) {
  Write-MonitorJsonFile -Report $report -Path $reportPath
  Write-MonitorJsonFile -Report $report -Path $CheckpointPath
}

$report | ConvertTo-Json -Depth 24
$overall = $report.overall
$exitCode = if ($overall -eq "fail") {
  1
} elseif ($overall -eq "blocked") {
  2
} else {
  0
}
Write-MonitorExitCode -Path $ExitCodeFile -Code $exitCode
exit $exitCode
