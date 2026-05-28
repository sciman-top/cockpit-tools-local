param(
  [datetime]$IncidentTime = (Get-Date),
  [ValidateRange(1, 1440)]
  [int]$WindowMinutes = 15,
  [string]$CodexHome = (Join-Path $HOME ".codex"),
  [string]$DataRoot = (Join-Path $HOME ".antigravity_cockpit"),
  [string]$ReportDir = (Join-Path (Get-Location) "reports\local-hardened-api-reconnect-diagnostics"),
  [switch]$IncludeCodexSessionLogs,
  [switch]$WriteReport,
  [switch]$Quiet
)

$ErrorActionPreference = "Stop"

$SensitivePatterns = @(
  @{ Pattern = 'agt_codex_[A-Za-z0-9]+'; Replacement = 'agt_codex_[redacted]' },
  @{ Pattern = '(?i)(Bearer\s+)[A-Za-z0-9._~+/=-]+'; Replacement = '$1[redacted]' },
  @{ Pattern = '(?i)(api[_-]?key["'']?\s*[:=]\s*["'']?)[^"'',\s}]+'; Replacement = '$1[redacted]' },
  @{ Pattern = '(?i)(OPENAI_API_KEY\s*=\s*)[^\s]+'; Replacement = '$1[redacted]' }
)

$ReconnectKeywords = @(
  "Reconnecting",
  "stream disconnected before completion",
  "websocket closed by server",
  "response.completed",
  "response_completed_seen",
  "local_backpressure",
  "usage_limit_reached",
  "429",
  "401",
  "refresh_token_invalidated",
  "pool_unavailable",
  "fallback_blocked",
  "previous_response_id",
  "x-codex-turn-state"
)

function ConvertTo-UnixTimeMilliseconds {
  param([datetime]$Value)
  $offset = if ($Value.Kind -eq [DateTimeKind]::Unspecified) {
    [DateTimeOffset]::new($Value)
  } else {
    [DateTimeOffset]$Value
  }
  return $offset.ToUnixTimeMilliseconds()
}

function ConvertFrom-UnixTimeMilliseconds {
  param([long]$Value)
  return ([DateTimeOffset]::FromUnixTimeMilliseconds($Value)).LocalDateTime.ToString("o")
}

function Redact-SensitiveText {
  param([string]$Text)
  if ($null -eq $Text) {
    return $null
  }
  $value = $Text
  foreach ($entry in $SensitivePatterns) {
    $value = [regex]::Replace($value, $entry.Pattern, $entry.Replacement)
  }
  return $value
}

function Get-Sha256String {
  param([string]$Value)
  if ($null -eq $Value) {
    return $null
  }
  $sha = [System.Security.Cryptography.SHA256]::Create()
  try {
    $bytes = [System.Text.Encoding]::UTF8.GetBytes($Value)
    $hash = $sha.ComputeHash($bytes)
    return (($hash | ForEach-Object { $_.ToString("x2") }) -join "")
  } finally {
    $sha.Dispose()
  }
}

function Get-FileSnapshot {
  param(
    [string]$Path,
    [switch]$IncludeHash
  )
  if (-not (Test-Path -LiteralPath $Path)) {
    return [ordered]@{
      path = $Path
      exists = $false
      length = 0
      lastWriteTime = $null
      sha256 = $null
    }
  }

  $item = Get-Item -LiteralPath $Path
  $hash = $null
  if ($IncludeHash) {
    $hash = (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash
  }

  [ordered]@{
    path = $Path
    exists = $true
    length = $item.Length
    lastWriteTime = $item.LastWriteTime.ToString("o")
    sha256 = $hash
  }
}

function Get-TopLevelTomlValue {
  param(
    [string[]]$Lines,
    [string]$Key
  )
  foreach ($line in $Lines) {
    $trimmed = $line.Trim()
    if ($trimmed.StartsWith("[")) {
      break
    }
    if ($trimmed -match "^\s*$([regex]::Escape($Key))\s*=\s*`"([^`"]*)`"") {
      return $Matches[1]
    }
  }
  return $null
}

function Get-TomlSectionValues {
  param(
    [string[]]$Lines,
    [string]$SectionName
  )
  $values = [ordered]@{}
  $inSection = $false
  $sectionPattern = "^\s*\[$([regex]::Escape($SectionName))\]\s*$"
  foreach ($line in $Lines) {
    $trimmed = $line.Trim()
    if ($trimmed -match "^\s*\[.+\]\s*$") {
      $inSection = ($trimmed -match $sectionPattern)
      continue
    }
    if (-not $inSection -or $trimmed.StartsWith("#") -or [string]::IsNullOrWhiteSpace($trimmed)) {
      continue
    }
    if ($trimmed -match "^\s*([A-Za-z0-9_\.-]+)\s*=\s*`"([^`"]*)`"") {
      $values[$Matches[1]] = Redact-SensitiveText $Matches[2]
    } elseif ($trimmed -match "^\s*([A-Za-z0-9_\.-]+)\s*=\s*(.+?)\s*$") {
      $values[$Matches[1]] = Redact-SensitiveText $Matches[2]
    }
  }
  return $values
}

function Get-CodexProviderSnapshot {
  param([string]$Root)
  $configPath = Join-Path $Root "config.toml"
  $snapshot = Get-FileSnapshot -Path $configPath -IncludeHash
  if (-not $snapshot.exists) {
    return [ordered]@{
      config = $snapshot
      activeModelProvider = $null
      activeModel = $null
      provider = [ordered]@{}
    }
  }

  $lines = Get-Content -LiteralPath $configPath
  $providerId = Get-TopLevelTomlValue -Lines $lines -Key "model_provider"
  $model = Get-TopLevelTomlValue -Lines $lines -Key "model"
  $provider = if ($providerId) {
    Get-TomlSectionValues -Lines $lines -SectionName "model_providers.$providerId"
  } else {
    [ordered]@{}
  }

  [ordered]@{
    config = $snapshot
    activeModelProvider = $providerId
    activeModel = $model
    provider = $provider
  }
}

function Get-ListenerSnapshot {
  param([string]$BaseUrl)
  if ([string]::IsNullOrWhiteSpace($BaseUrl)) {
    return [ordered]@{
      baseUrl = $BaseUrl
      localPort = $null
      listeners = @()
    }
  }

  $match = [regex]::Match($BaseUrl, '^https?://(?:127\.0\.0\.1|localhost|\[::1\]):(?<port>\d+)(?:/|$)')
  if (-not $match.Success) {
    return [ordered]@{
      baseUrl = $BaseUrl
      localPort = $null
      listeners = @()
    }
  }

  $port = [int]$match.Groups["port"].Value
  $listeners = @()
  $connections = @(Get-NetTCPConnection -LocalPort $port -State Listen -ErrorAction SilentlyContinue)
  foreach ($connection in $connections) {
    $process = Get-Process -Id $connection.OwningProcess -ErrorAction SilentlyContinue
    $cim = Get-CimInstance Win32_Process -Filter "ProcessId=$($connection.OwningProcess)" -ErrorAction SilentlyContinue
    $listeners += [ordered]@{
      localAddress = $connection.LocalAddress
      localPort = $connection.LocalPort
      owningProcess = $connection.OwningProcess
      processName = if ($process) { $process.ProcessName } else { $null }
      path = if ($process) { $process.Path } else { $null }
      parentProcessId = if ($cim) { $cim.ParentProcessId } else { $null }
      creationDate = if ($cim -and $cim.CreationDate) { $cim.CreationDate.ToString("o") } else { $null }
    }
  }

  [ordered]@{
    baseUrl = $BaseUrl
    localPort = $port
    listeners = @($listeners)
  }
}

function Get-JsonFile {
  param([string]$Path)
  if (-not (Test-Path -LiteralPath $Path)) {
    return $null
  }
  try {
    return (Get-Content -LiteralPath $Path -Raw | ConvertFrom-Json)
  } catch {
    return $null
  }
}

function Get-LocalRuntimeSnapshot {
  param([string]$Root)
  $runtimeModePath = Join-Path $Root "codex_runtime_mode.json"
  $localAccessPath = Join-Path $Root "codex_local_access.json"
  $healthPath = Join-Path $Root "codex_local_access_health.json"
  $auditPath = Join-Path $Root "codex_local_access_audit.jsonl"

  $runtimeMode = Get-JsonFile -Path $runtimeModePath
  $localAccess = Get-JsonFile -Path $localAccessPath

  $accountHashes = @()
  if ($localAccess -and $localAccess.accountIds) {
    $accountHashes = @($localAccess.accountIds | ForEach-Object { "sha256:$((Get-Sha256String $_).Substring(0, 12))" })
  }

  [ordered]@{
    root = $Root
    files = [ordered]@{
      runtimeMode = Get-FileSnapshot -Path $runtimeModePath -IncludeHash
      localAccess = Get-FileSnapshot -Path $localAccessPath -IncludeHash
      health = Get-FileSnapshot -Path $healthPath -IncludeHash
      audit = Get-FileSnapshot -Path $auditPath
    }
    runtimeMode = if ($runtimeMode) {
      [ordered]@{
        mode = $runtimeMode.mode
        accountKind = $runtimeMode.accountKind
        currentAccountHash = if ($runtimeMode.currentAccountId) { "sha256:$((Get-Sha256String $runtimeMode.currentAccountId).Substring(0, 12))" } else { $null }
        updatedAt = $runtimeMode.updatedAt
      }
    } else {
      $null
    }
    localAccess = if ($localAccess) {
      [ordered]@{
        enabled = $localAccess.enabled
        port = $localAccess.port
        routingStrategy = $localAccess.routingStrategy
        restrictFreeAccounts = $localAccess.restrictFreeAccounts
        followCurrentAccount = $localAccess.followCurrentAccount
        accountCount = @($localAccess.accountIds).Count
        accountHashes = @($accountHashes)
        apiKeyPresent = -not [string]::IsNullOrWhiteSpace($localAccess.apiKey)
        safetyConfig = if ($localAccess.safetyConfig) {
          [ordered]@{
            hardenedLocalMode = $localAccess.safetyConfig.hardenedLocalMode
            maxConcurrentRequests = $localAccess.safetyConfig.maxConcurrentRequests
            minRequestIntervalSeconds = $localAccess.safetyConfig.minRequestIntervalSeconds
            maxQueueWaitSeconds = $localAccess.safetyConfig.maxQueueWaitSeconds
            requestTimeoutSeconds = $localAccess.safetyConfig.requestTimeoutSeconds
            maxRetries = $localAccess.safetyConfig.maxRetries
            maxRetryAccounts = $localAccess.safetyConfig.maxRetryAccounts
            fallbackMode = $localAccess.safetyConfig.fallbackMode
          }
        } else {
          $null
        }
        updatedAt = $localAccess.updatedAt
      }
    } else {
      $null
    }
  }
}

function Add-Count {
  param(
    [hashtable]$Table,
    [string]$Key
  )
  $safeKey = if ([string]::IsNullOrWhiteSpace($Key)) { "(empty)" } else { $Key }
  if (-not $Table.ContainsKey($safeKey)) {
    $Table[$safeKey] = 0
  }
  $Table[$safeKey] += 1
}

function Convert-CountTable {
  param([hashtable]$Table)
  $result = [ordered]@{}
  foreach ($key in @($Table.Keys | Sort-Object)) {
    $result[[string]$key] = $Table[$key]
  }
  return $result
}

function Get-AuditSnapshot {
  param(
    [string]$Path,
    [long]$StartMs,
    [long]$EndMs,
    [int]$MaxTailLines = 5000
  )
  $file = Get-FileSnapshot -Path $Path
  if (-not $file.exists) {
    return [ordered]@{
      file = $file
      window = [ordered]@{ startMs = $StartMs; endMs = $EndMs }
      totalWindowEvents = 0
      counts = [ordered]@{}
      recentEvents = @()
      keywordHits = @()
    }
  }

  $phaseCounts = @{}
  $outcomeCounts = @{}
  $statusCounts = @{}
  $streamStateCounts = @{}
  $responseCompletedSeenCounts = @{}
  $recent = New-Object System.Collections.Generic.List[object]
  $keywordHits = New-Object System.Collections.Generic.List[object]
  $total = 0

  $lines = @(Get-Content -LiteralPath $Path -Tail $MaxTailLines)
  foreach ($line in $lines) {
    if ([string]::IsNullOrWhiteSpace($line)) {
      continue
    }
    try {
      $event = $line | ConvertFrom-Json
    } catch {
      continue
    }
    if ($null -eq $event.timestamp) {
      continue
    }
    $timestamp = [long]$event.timestamp
    if ($timestamp -lt $StartMs -or $timestamp -gt $EndMs) {
      continue
    }

    $total += 1
    Add-Count -Table $phaseCounts -Key ([string]$event.phase)
    Add-Count -Table $outcomeCounts -Key ([string]$event.outcome)
    if ($null -ne $event.status) {
      Add-Count -Table $statusCounts -Key ([string]$event.status)
    }
    if ($null -ne $event.streamState) {
      Add-Count -Table $streamStateCounts -Key ([string]$event.streamState)
    }
    if ($event.detail -and $null -ne $event.detail.response_completed_seen) {
      Add-Count -Table $responseCompletedSeenCounts -Key ([string]$event.detail.response_completed_seen)
    }

    $summary = [ordered]@{
      timestamp = $timestamp
      timestampLocal = ConvertFrom-UnixTimeMilliseconds -Value $timestamp
      requestId = $event.requestId
      phase = $event.phase
      route = $event.route
      model = $event.model
      accountHash = $event.accountHash
      status = $event.status
      streamState = $event.streamState
      outcome = $event.outcome
      gatewayRequestId = if ($event.detail) { $event.detail.gateway_request_id } else { $null }
      requestIdSource = if ($event.detail) { $event.detail.request_id_source } else { $null }
      responseCompletedSeen = if ($event.detail) { $event.detail.response_completed_seen } else { $null }
      isContinuation = if ($event.detail) { $event.detail.is_continuation } else { $null }
      errorCode = if ($event.detail) { $event.detail.error_code } else { $null }
    }

    $eventText = Redact-SensitiveText ($line)
    $matched = @($ReconnectKeywords | Where-Object { $eventText -like "*$_*" })
    if ($matched.Count -gt 0) {
      $keywordHits.Add([ordered]@{
        timestamp = $timestamp
        timestampLocal = $summary.timestampLocal
        phase = $event.phase
        outcome = $event.outcome
        status = $event.status
        streamState = $event.streamState
        matchedKeywords = @($matched)
        gatewayRequestId = $summary.gatewayRequestId
      }) | Out-Null
    }

    $recent.Add($summary) | Out-Null
    while ($recent.Count -gt 40) {
      $recent.RemoveAt(0)
    }
  }

  $recentEvents = @($recent.ToArray())
  $keywordEvents = @($keywordHits.ToArray() | Select-Object -Last 80)

  [ordered]@{
    file = $file
    window = [ordered]@{ startMs = $StartMs; endMs = $EndMs }
    totalWindowEvents = $total
    counts = [ordered]@{
      phase = Convert-CountTable -Table $phaseCounts
      outcome = Convert-CountTable -Table $outcomeCounts
      status = Convert-CountTable -Table $statusCounts
      streamState = Convert-CountTable -Table $streamStateCounts
      responseCompletedSeen = Convert-CountTable -Table $responseCompletedSeenCounts
    }
    recentEvents = $recentEvents
    keywordHits = $keywordEvents
  }
}

function Get-CodexSessionLogHits {
  param(
    [string]$Root,
    [datetime]$Since,
    [datetime]$Until = (Get-Date),
    [int]$MaxFiles = 30
  )
  if (-not (Test-Path -LiteralPath $Root)) {
    return [ordered]@{
      root = $Root
      searchedFiles = 0
      hits = @()
    }
  }

  $candidateFiles = New-Object System.Collections.Generic.List[object]
  $sessionsRoot = Join-Path $Root "sessions"
  if (Test-Path -LiteralPath $sessionsRoot) {
    $date = $Since.Date
    while ($date -le $Until.Date) {
      $dayRoot = Join-Path $sessionsRoot ("{0}\{1}\{2}" -f $date.ToString("yyyy"), $date.ToString("MM"), $date.ToString("dd"))
      if (Test-Path -LiteralPath $dayRoot) {
        @(Get-ChildItem -LiteralPath $dayRoot -File -ErrorAction SilentlyContinue) |
          Where-Object {
            $_.LastWriteTime -ge $Since -and
            $_.LastWriteTime -le $Until.AddMinutes(5) -and
            $_.Length -lt 10485760 -and
            ($_.Extension -in @(".jsonl", ".log", ".txt"))
          } |
          ForEach-Object { $candidateFiles.Add($_) | Out-Null }
      }
      $date = $date.AddDays(1)
    }
  }

  foreach ($child in @("log", "logs")) {
    $searchRoot = Join-Path $Root $child
    if (Test-Path -LiteralPath $searchRoot) {
      @(Get-ChildItem -LiteralPath $searchRoot -Recurse -File -ErrorAction SilentlyContinue) |
        Where-Object {
          $_.LastWriteTime -ge $Since -and
          $_.LastWriteTime -le $Until.AddMinutes(5) -and
          $_.Length -lt 10485760 -and
          ($_.Extension -in @(".jsonl", ".log", ".txt"))
        } |
        ForEach-Object { $candidateFiles.Add($_) | Out-Null }
    }
  }

  @(Get-ChildItem -LiteralPath $Root -File -ErrorAction SilentlyContinue) |
    Where-Object {
      $_.LastWriteTime -ge $Since -and
      $_.LastWriteTime -le $Until.AddMinutes(5) -and
      $_.Length -lt 10485760 -and
      ($_.Extension -in @(".jsonl", ".log", ".txt"))
    } |
    ForEach-Object { $candidateFiles.Add($_) | Out-Null }

  $files = @($candidateFiles.ToArray() | Sort-Object LastWriteTime -Descending | Select-Object -First $MaxFiles)

  $hits = New-Object System.Collections.Generic.List[object]
  foreach ($file in $files) {
    $fileHits = @(Select-String -LiteralPath $file.FullName -Pattern $ReconnectKeywords -SimpleMatch -ErrorAction SilentlyContinue)
    if ($fileHits.Count -eq 0) {
      continue
    }
    $counts = @{}
    foreach ($hit in $fileHits) {
      foreach ($keyword in $ReconnectKeywords) {
        if ($hit.Line -like "*$keyword*") {
          Add-Count -Table $counts -Key $keyword
        }
      }
    }
    $hits.Add([ordered]@{
      path = $file.FullName
      lastWriteTime = $file.LastWriteTime.ToString("o")
      length = $file.Length
      totalHits = $fileHits.Count
      keywordCounts = $counts
      sampleLineNumbers = @($fileHits | Select-Object -First 20 | ForEach-Object { $_.LineNumber })
    }) | Out-Null
  }

  [ordered]@{
    root = $Root
    searchedFiles = $files.Count
    hits = @($hits)
  }
}

function Get-CodexAppProcessSnapshot {
  $processes = @(Get-Process -Name "Codex" -ErrorAction SilentlyContinue | ForEach-Object {
    $path = $null
    $startTime = $null
    try {
      $path = $_.Path
    } catch {
      $path = $null
    }
    try {
      $startTime = $_.StartTime.ToString("o")
    } catch {
      $startTime = $null
    }

    [ordered]@{
      id = $_.Id
      processName = $_.ProcessName
      path = $path
      startTime = $startTime
    }
  })

  [ordered]@{
    processes = @($processes | Sort-Object id)
  }
}

$incidentEnd = $IncidentTime.AddMinutes($WindowMinutes)
$incidentStart = $IncidentTime.AddMinutes(-1 * $WindowMinutes)
$startMs = ConvertTo-UnixTimeMilliseconds -Value $incidentStart
$endMs = ConvertTo-UnixTimeMilliseconds -Value $incidentEnd

$providerSnapshot = Get-CodexProviderSnapshot -Root $CodexHome
$listenerSnapshot = Get-ListenerSnapshot -BaseUrl $providerSnapshot.provider["base_url"]
$runtimeSnapshot = Get-LocalRuntimeSnapshot -Root $DataRoot
$auditSnapshot = Get-AuditSnapshot -Path (Join-Path $DataRoot "codex_local_access_audit.jsonl") -StartMs $startMs -EndMs $endMs
$sessionHits = if ($IncludeCodexSessionLogs) {
  Get-CodexSessionLogHits -Root $CodexHome -Since $incidentStart -Until $incidentEnd
} else {
  [ordered]@{
    root = $CodexHome
    searchedFiles = 0
    skipped = $true
    reason = "pass -IncludeCodexSessionLogs to scan recent Codex session files"
    hits = @()
  }
}

$listenerProcessNames = @($listenerSnapshot.listeners | ForEach-Object { $_.processName } | Where-Object { $_ })
$isLocalCockpitProvider = (
  $providerSnapshot.activeModelProvider -eq "codex_local_access" -or
  ($providerSnapshot.provider["base_url"] -match '^https?://(?:127\.0\.0\.1|localhost|\[::1\]):')
)
$isCockpitListener = @($listenerProcessNames | Where-Object { $_ -like "cockpit-tools*" }).Count -gt 0

$report = [ordered]@{
  schemaVersion = 1
  generatedAt = (Get-Date).ToString("o")
  incident = [ordered]@{
    incidentTime = $IncidentTime.ToString("o")
    windowMinutes = $WindowMinutes
    startTime = $incidentStart.ToString("o")
    endTime = $incidentEnd.ToString("o")
  }
  codex = [ordered]@{
    home = $CodexHome
    provider = $providerSnapshot
    appProcesses = Get-CodexAppProcessSnapshot
    sessionLogHits = $sessionHits
  }
  cockpit = [ordered]@{
    dataRoot = $DataRoot
    listener = $listenerSnapshot
    runtime = $runtimeSnapshot
    audit = $auditSnapshot
  }
  assessment = [ordered]@{
    currentPathParticipates = ($isLocalCockpitProvider -and $isCockpitListener)
    isLocalCockpitProvider = $isLocalCockpitProvider
    isCockpitListener = $isCockpitListener
    evidence = @(
      if ($isLocalCockpitProvider) { "active provider or base_url points to localhost/local codex access" }
      if ($isCockpitListener) { "localhost provider port is owned by cockpit-tools" }
      if ($auditSnapshot.totalWindowEvents -gt 0) { "local access audit has events in the incident window" }
      if ($sessionHits.hits.Count -gt 0) { "Codex session logs contain reconnect/stream keywords in the incident window" }
    )
    nextStep = if ($isLocalCockpitProvider -and $isCockpitListener) {
      "Treat Cockpit local API service as an active candidate; compare audit terminal events with Codex reconnect log timestamps before changing code."
    } else {
      "Treat as upstream/client connection candidate unless another local Cockpit listener is identified."
    }
  }
}

$json = $report | ConvertTo-Json -Depth 12

if ($WriteReport) {
  New-Item -ItemType Directory -Force -Path $ReportDir | Out-Null
  $stamp = Get-Date -Format "yyyyMMdd-HHmmss"
  $path = Join-Path $ReportDir "reconnect-diagnostics-$stamp.json"
  Set-Content -LiteralPath $path -Encoding UTF8 -Value $json
  if (-not $Quiet) {
    Write-Output $path
  }
} else {
  $json
}
