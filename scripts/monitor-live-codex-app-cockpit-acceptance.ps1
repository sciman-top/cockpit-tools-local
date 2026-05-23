param(
  [int]$DurationSeconds = 900,
  [ValidateRange(1, 60)]
  [int]$PollIntervalSeconds = 2,
  [string]$DataRoot = (Join-Path $HOME ".antigravity_cockpit"),
  [string]$CodexHome = (Join-Path $HOME ".codex"),
  [string]$AuditPath,
  [string]$ReportDir = (Join-Path (Get-Location) "reports\local-hardened-api-live-monitor"),
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

function Test-CodexResponsesRoute {
  param([System.Collections.IDictionary]$Event)
  $route = [string]$Event.route
  $route -eq "/v1/responses" -or $route -eq "/responses" -or $route.EndsWith("/responses")
}

function Get-AuditAcceptanceSummary {
  param([object[]]$Events)
  $parsedEvents = @($Events | Where-Object { $_.parsed })
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
    (Test-CodexResponsesRoute $_) -and $_.status -eq 200 -and $_.outcome -eq "in_band_synthetic"
  })
  $responsesFailedPoolUnavailableEvents = @($localPoolUnavailableEvents | Where-Object {
    (Test-CodexResponsesRoute $_) -and $_.status -eq 200 -and ($_.streamState -eq "failed" -or $_.outcome -eq "pool_unavailable_after_active_stream_drain" -or $_.rawLine -match 'response\.failed')
  })
  $responsesLocalCompletionPoolUnavailableEvents = @($localPoolUnavailableEvents | Where-Object {
    (Test-CodexResponsesRoute $_) -and $_.status -eq 200 -and ($_.streamState -eq "completed" -or $_.outcome -eq "in_band_local_completion" -or $_.rawLine -match 'response\.completed')
  })
  $responsesTransport503PoolUnavailableEvents = @($localPoolUnavailableEvents | Where-Object {
    (Test-CodexResponsesRoute $_) -and $_.status -eq 503 -and $_.outcome -ne "in_band_synthetic"
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
    $isBlockingAccountEvent = (Test-ValidAccountHash $event.accountHash) -and (
      ($event.status -eq 429 -and (Test-UsageLimitEvent $event)) -or
      $event.phase -eq "model_cooldown_applied" -or
      $event.phase -eq "fallback_selected" -or
      $event.phase -eq "fallback_blocked"
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
        if ($candidate.phase -eq "final_response" -and $candidate.status -eq 429 -and -not (Test-LocalPoolUnavailableEvent $candidate)) {
          $terminal429 = $candidate
          break
        }
      }

      if ($terminal429) {
        $unrecoveredFallback429Events += $terminal429
      }

      $sameTaskAffinityBlockTransitions += [ordered]@{
        requestId = $event.requestId
        blockedAccountHash = $event.accountHash
        blockedTimestamp = $event.timestamp
        localCompletionTimestamp = if ($localCompletion) { $localCompletion.timestamp } else { $null }
        localCompletionAccountHash = if ($localCompletion) { $localCompletion.accountHash } else { $null }
        terminal429Timestamp = if ($terminal429) { $terminal429.timestamp } else { $null }
        completedLocally = [bool]($null -ne $localCompletion)
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
      if ($event.phase -eq "stream_completed" -or ($event.phase -eq "lease_released" -and $event.outcome -eq "completed")) {
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
    $isStreamEvent = $event.phase -in @("lease_granted", "stream_write", "stream_completed", "lease_released")
    $group = $null
    if ($event.phase -eq "lease_granted") {
      $streamSequence++
      $group = [ordered]@{
        streamKey = "{0}#{1}" -f $requestId, $streamSequence
        requestId = $requestId
        firstTimestamp = $event.timestamp
        lastTimestamp = $event.timestamp
        eventCount = 0
        started = $false
        completed = $false
        terminalError = $false
        interruptedByCooldown = $false
        firstAccountHash = $event.accountHash
        lastAccountHash = $event.accountHash
        phases = @()
        statuses = @()
        accountHashes = @()
      }
      $streamGroups += $group
      $activeStreamGroups[$requestId] = $group
    } elseif ($isStreamEvent -and $activeStreamGroups.ContainsKey($requestId)) {
      $group = $activeStreamGroups[$requestId]
    } elseif ($isStreamEvent) {
      $streamSequence++
      $group = [ordered]@{
        streamKey = "{0}#{1}" -f $requestId, $streamSequence
        requestId = $requestId
        firstTimestamp = $event.timestamp
        lastTimestamp = $event.timestamp
        eventCount = 0
        started = $false
        completed = $false
        terminalError = $false
        interruptedByCooldown = $false
        firstAccountHash = $event.accountHash
        lastAccountHash = $event.accountHash
        phases = @()
        statuses = @()
        accountHashes = @()
      }
      $streamGroups += $group
      $activeStreamGroups[$requestId] = $group
    } elseif ($activeStreamGroups.ContainsKey($requestId)) {
      $group = $activeStreamGroups[$requestId]
    }
    if (-not $group) {
      continue
    }
    $group.eventCount++
    $group.lastTimestamp = $event.timestamp
    $group.lastAccountHash = $event.accountHash
    $group.phases += $event.phase
    if ($null -ne $event.status) {
      $group.statuses += $event.status
    }
    if (Test-ValidAccountHash $event.accountHash) {
      $group.accountHashes += $event.accountHash
    }
    if ($event.phase -eq "lease_granted" -or $event.phase -eq "stream_write") {
      $group.started = $true
    }
    if ($event.phase -eq "stream_completed" -or ($event.phase -eq "lease_released" -and $event.outcome -eq "completed") -or ($event.phase -eq "final_response" -and $event.status -eq 200)) {
      $group.completed = $true
    }
    if ($event.phase -eq "final_response" -and $event.status -ge 400 -and -not $group.completed) {
      $group.terminalError = $true
    }
    if ($group.started -and $event.phase -eq "model_cooldown_applied") {
      $group.interruptedByCooldown = $true
    }
    if (($event.phase -eq "stream_completed" -or $event.phase -eq "lease_released" -or $event.phase -eq "final_response") -and ($group.completed -or $group.terminalError)) {
      [void]$activeStreamGroups.Remove($requestId)
    }
  }

  $streams = @($streamGroups)
  $startedStreams = @($streams | Where-Object { $_.started })
  $completedStreams = @($startedStreams | Where-Object { $_.completed })
  $openStreams = @($startedStreams | Where-Object { -not $_.completed -and -not $_.terminalError })
  $interruptedStreams = @($startedStreams | Where-Object { $_.interruptedByCooldown })
  $completedFallbackTransitions = @($fallbackTransitions | Where-Object { $_.completed -and $_.differentAccount })
  $distinctHealthyAccountHashesAfterFallback = @($healthyAccountHashesAfterFallback | Sort-Object -Unique)
  $completedSameTaskAffinityBlocks = @($sameTaskAffinityBlockTransitions | Where-Object { $_.completedLocally })
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
      $_.phase -eq "stream_completed" -or
      ($_.phase -eq "lease_released" -and $_.outcome -eq "completed") -or
      $_.phase -eq "final_response" -or
      ($_.phase -eq "upstream_forward" -and $_.status -eq 200)
    }).Count
    if (-not $hasPoolWaitTerminal) {
      $openPoolWaitRequestIds += $poolWaitRequestId
    }
  }

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
    sameTaskAffinityFallbackBlockedRequestIds = @($sameTaskAffinityBlockTransitions | ForEach-Object { $_.requestId } | Sort-Object -Unique)
    sameTaskAffinityFallbackBlockedTransitions = @($sameTaskAffinityBlockTransitions)
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
    startedStreamCount = $startedStreams.Count
    completedStreamCount = $completedStreams.Count
    openStreamCount = $openStreams.Count
    interruptedStreamCount = $interruptedStreams.Count
    streamSummaries = @($streams | ForEach-Object {
      [ordered]@{
        streamKey = $_.streamKey
        requestId = $_.requestId
        firstTimestamp = $_.firstTimestamp
        lastTimestamp = $_.lastTimestamp
        eventCount = [int]$_.eventCount
        started = [bool]$_.started
        completed = [bool]$_.completed
        terminalError = [bool]$_.terminalError
        interruptedByCooldown = [bool]$_.interruptedByCooldown
        firstAccountHash = $_.firstAccountHash
        lastAccountHash = $_.lastAccountHash
        accountHashes = @($_.accountHashes | Select-Object -Unique)
        statuses = @($_.statuses | Select-Object -Unique)
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
    [System.Collections.IDictionary]$AppComparison
  )
  $results = @()

  $sameTask = New-MonitorResult "same_task_affinity_fallback_blocked"
  $sameTaskEvidence = @{
    has429 = [bool]$AuditSummary.has429
    hasUsageLimitReached = [bool]$AuditSummary.hasUsageLimitReached
    hasModelCooldownApplied = [bool]$AuditSummary.hasModelCooldownApplied
    hasFallbackBlocked = [bool]$AuditSummary.hasFallbackBlocked
    hasHardAffinityFallbackBlocked = [bool]$AuditSummary.hasHardAffinityFallbackBlocked
    sameTaskAffinityFallbackBlockedCount = [int]$AuditSummary.sameTaskAffinityFallbackBlockedCount
    sameTaskAffinityLocalCompletionCount = [int]$AuditSummary.sameTaskAffinityLocalCompletionCount
    sameTaskAffinityFallbackBlockedRequestIds = @($AuditSummary.sameTaskAffinityFallbackBlockedRequestIds)
    requiredFallbackCycles = [int]$RequiredFallbackCycles
    first429AccountHash = $AuditSummary.first429AccountHash
    localPoolUnavailableCount = [int]$AuditSummary.localPoolUnavailableCount
    responsesLocalCompletionPoolUnavailableCount = [int]$AuditSummary.responsesLocalCompletionPoolUnavailableCount
  }
  $sameTaskPass = $AuditSummary.has429 -and $AuditSummary.hasUsageLimitReached -and $AuditSummary.hasModelCooldownApplied -and $AuditSummary.sameTaskAffinityFallbackBlockedCount -ge $RequiredFallbackCycles -and $AuditSummary.sameTaskAffinityLocalCompletionCount -ge $RequiredFallbackCycles
  if ($sameTaskPass) {
    $results += Set-MonitorStatus $sameTask "pass" $null $sameTaskEvidence
  } elseif ($RequireQuotaFallback) {
    $results += Set-MonitorStatus $sameTask "blocked" "未在监测窗口内观察到同任务 429 -> cooldown -> fallback_blocked(hard_affinity) -> 本地 completed 闭合" $sameTaskEvidence
  } else {
    $results += Set-MonitorStatus $sameTask "skipped" "未要求 quota hard-affinity 必须出现；仅记录 audit 观察结果" $sameTaskEvidence
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
    requiredFallbackCycles = [int]$RequiredFallbackCycles
    distinctHealthyAccountCountAfterBlock = [int]$AuditSummary.distinctHealthyAccountCountAfterBlock
    requiredDistinctHealthyAccounts = [int]$RequiredDistinctHealthyAccounts
    distinctHealthyAccountHashesAfterBlock = @($AuditSummary.distinctHealthyAccountHashesAfterBlock)
  }
  if ($RequiredFallbackCycles -le 1 -and $RequiredDistinctHealthyAccounts -le 1) {
    $results += Set-MonitorStatus $multi "skipped" "未要求多账号 quota recovery 计数；仅记录多账号观察结果" $multiEvidence
  } elseif ($AuditSummary.sameTaskAffinityLocalCompletionCount -ge $RequiredFallbackCycles -and $AuditSummary.distinctHealthyAccountCountAfterBlock -ge $RequiredDistinctHealthyAccounts) {
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
  }
  if ($AuditSummary.interruptedStreamCount -gt 0) {
    $results += Set-MonitorStatus $stream "fail" "已开始的 stream 后续出现 model_cooldown_applied，中断边界异常" $streamEvidence
  } elseif ($AuditSummary.completedStreamCount -ge $RequiredCompletedStreams) {
    $results += Set-MonitorStatus $stream "pass" $null $streamEvidence
  } elseif ($RequireStreamCompletion) {
    $results += Set-MonitorStatus $stream "blocked" "未在监测窗口内观察到已接纳 stream 完成" $streamEvidence
  } else {
    $results += Set-MonitorStatus $stream "skipped" "未要求必须观察 stream 完成；仅记录 stream 状态" $streamEvidence
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
    $results += Set-MonitorStatus $retry "fail" "监测窗口内出现历史 retry-limit/429 错误文本或 fallback 后未恢复的 final 429" $retryEvidence
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
    responsesFailedPoolUnavailableCount = [int]$AuditSummary.responsesFailedPoolUnavailableCount
    responsesFailedPoolUnavailableRequestIds = @($AuditSummary.responsesFailedPoolUnavailableRequestIds)
    openPoolWaitCount = [int]$AuditSummary.openPoolWaitCount
    openPoolWaitRequestIds = @($AuditSummary.openPoolWaitRequestIds)
  }
  if ($AuditSummary.openPoolWaitCount -gt 0 -and $AuditSummary.responsesLocalCompletionPoolUnavailableCount -eq 0) {
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
    sameTaskAffinityFallbackBlockedRequestIds = @($AuditSummary.sameTaskAffinityFallbackBlockedRequestIds)
    requiredFallbackCycles = [int]$RequiredFallbackCycles
    retryLimitErrorFound = [bool]$AuditSummary.retryLimitErrorFound
    first429AccountHash = $AuditSummary.first429AccountHash
  }
  $currentStatus = "blocked"
  $currentReason = "未在监测窗口内观察到同任务 429 -> cooldown -> fallback_blocked(hard_affinity) -> 本地 completed 闭合"
  if ($AuditSummary.retryLimitErrorFound) {
    $currentStatus = "fail"
    $currentReason = "hard-affinity block 后仍出现 retry-limit/final 429"
  } elseif ($AuditSummary.has429 -and $AuditSummary.hasUsageLimitReached -and $AuditSummary.hasModelCooldownApplied -and $AuditSummary.sameTaskAffinityFallbackBlockedCount -ge $RequiredFallbackCycles -and $AuditSummary.sameTaskAffinityLocalCompletionCount -ge $RequiredFallbackCycles) {
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
  }
}

if (-not $AuditPath) {
  $AuditPath = Join-Path $DataRoot "codex_local_access_audit.jsonl"
}

$startedAt = Get-Date
$deadline = $startedAt.AddSeconds($DurationSeconds)
$offset = Get-InitialAuditOffset $AuditPath
$events = @()
$cliBefore = Get-FileGuardState $CodexHome
$appBefore = Get-CodexAppProcessState $CodexAppProcessNames $CodexAppPathIncludePatterns $CodexAppPathExcludePatterns

if (-not $Quiet) {
  Write-Host ("monitoring audit={0}" -f $AuditPath)
  Write-Host ("started_at={0}; duration_seconds={1}; include_existing_audit={2}" -f $startedAt.ToString("o"), $DurationSeconds, [bool]$IncludeExistingAudit)
}

do {
  $lines = Read-NewAuditLines -Path $AuditPath -Offset ([ref]$offset)
  if ($lines.Count -gt 0) {
    $events += @($lines | ForEach-Object { Convert-AuditLine $_ })
    if (-not $Quiet) {
      $summary = Get-AuditAcceptanceSummary $events
      Write-Host ("events={0}; has429={1}; fallbackBlocked={2}; sameTaskLocalCompletion={3}; newRequestAvoidance={4}; healthyAccountsAfterBlock={5}; streams={6}/{7}; retryLimit={8}; poolWait={9}; heartbeatPoolWait={10}; activeDrainWait={11}; openPoolWait={12}; poolUnavailable={13}; localCompletionPoolUnavailable={14}; failedPoolUnavailable={15}; responses503={16}; parkedPoolWait={17}; sseIdle={18}" -f $summary.eventCount, $summary.has429, $summary.sameTaskAffinityFallbackBlockedCount, $summary.sameTaskAffinityLocalCompletionCount, $summary.newRequestAvoidanceCount, $summary.distinctHealthyAccountCountAfterBlock, $summary.completedStreamCount, $summary.startedStreamCount, $summary.retryLimitErrorFound, $summary.poolWaitCount, $summary.heartbeatPoolWaitCount, $summary.activeDrainPoolWaitCount, $summary.openPoolWaitCount, $summary.localPoolUnavailableCount, $summary.responsesLocalCompletionPoolUnavailableCount, $summary.responsesFailedPoolUnavailableCount, $summary.responsesTransport503PoolUnavailableCount, $summary.parkedPoolWaitCount, $summary.sseIdleErrorCount)
    }
  }

  if ($StopWhenSatisfied) {
    $summaryNow = Get-AuditAcceptanceSummary $events
    $quotaSatisfied = (-not $RequireQuotaFallback) -or ($summaryNow.has429 -and $summaryNow.hasUsageLimitReached -and $summaryNow.hasModelCooldownApplied -and $summaryNow.sameTaskAffinityFallbackBlockedCount -ge $RequiredFallbackCycles -and $summaryNow.sameTaskAffinityLocalCompletionCount -ge $RequiredFallbackCycles -and $summaryNow.newRequestAvoidanceCount -gt 0 -and $summaryNow.distinctHealthyAccountCountAfterBlock -ge $RequiredDistinctHealthyAccounts)
    $streamSatisfied = (-not $RequireStreamCompletion) -or ($summaryNow.completedStreamCount -ge $RequiredCompletedStreams)
    $retrySatisfied = (-not $summaryNow.retryLimitErrorFound)
    $responses503Satisfied = (-not $summaryNow.responsesTransport503PoolUnavailableCount)
    $localCompletionSatisfied = (-not $summaryNow.openPoolWaitCount) -or $summaryNow.responsesLocalCompletionPoolUnavailableCount -gt 0
    $failedStreamSatisfied = (-not $summaryNow.responsesFailedPoolUnavailableCount)
    $legacySyntheticTerminalSatisfied = (-not $summaryNow.inBandSyntheticPoolUnavailableCount)
    $poolWaitProgressSatisfied = (-not $summaryNow.openPoolWaitCount)
    if ($quotaSatisfied -and $streamSatisfied -and $retrySatisfied -and $responses503Satisfied -and $localCompletionSatisfied -and $failedStreamSatisfied -and $legacySyntheticTerminalSatisfied -and $poolWaitProgressSatisfied) {
      break
    }
  }

  if ($DurationSeconds -le 0) {
    break
  }
  Start-Sleep -Seconds $PollIntervalSeconds
} while ((Get-Date) -lt $deadline)

$endedAt = Get-Date
$cliAfter = Get-FileGuardState $CodexHome
$appAfter = Get-CodexAppProcessState $CodexAppProcessNames $CodexAppPathIncludePatterns $CodexAppPathExcludePatterns
$cliComparison = Compare-FileGuardState $cliBefore $cliAfter
$appComparison = Compare-CodexAppProcessState $appBefore $appAfter
$auditSummary = Get-AuditAcceptanceSummary $events
$results = New-AcceptanceResults -AuditSummary $auditSummary -CliComparison $cliComparison -AppComparison $appComparison
$continuitySummary = New-ContinuitySummary $auditSummary

$overall = if ($results | Where-Object { $_.status -eq "fail" }) {
  "fail"
} elseif ($results | Where-Object { $_.status -eq "blocked" }) {
  "blocked"
} else {
  "pass"
}

$report = [ordered]@{
  schemaVersion = 1
  generatedAt = $endedAt.ToString("o")
  overall = $overall
  mode = "live_codex_app_monitor"
  startedAt = $startedAt.ToString("o")
  endedAt = $endedAt.ToString("o")
  elapsedSeconds = [math]::Round(($endedAt - $startedAt).TotalSeconds, 1)
  dataRoot = $DataRoot
  auditPath = $AuditPath
  includeExistingAudit = [bool]$IncludeExistingAudit
  requireQuotaFallback = [bool]$RequireQuotaFallback
  requireStreamCompletion = [bool]$RequireStreamCompletion
  requireCliConfigUntouched = [bool]$RequireCliConfigUntouched
  requireAppStable = [bool]$RequireAppStable
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
    before = $cliBefore
    after = $cliAfter
    comparison = $cliComparison
  }
  codexAppGuard = [ordered]@{
    before = $appBefore
    after = $appAfter
    comparison = $appComparison
  }
  audit = $auditSummary
  continuitySummary = $continuitySummary
  results = $results
  safetyNotes = @(
    "this live monitor is read-only",
    "it hashes ~/.codex/config.toml and ~/.codex/auth.json but does not read or write their contents",
    "it does not start, stop, restart, or kill Codex App, Codex CLI, or Cockpit services",
    "it does not switch providers or restore manual provider changes",
    "use the App-safe isolated acceptance script when temporary provider config must be created and restored automatically"
  )
}

if ($WriteReport) {
  New-Item -ItemType Directory -Force -Path $ReportDir | Out-Null
  $stamp = Get-Date -Format "yyyyMMdd-HHmmss"
  $reportPath = Join-Path $ReportDir ("live-monitor-{0}.json" -f $stamp)
  $report.reportPath = $reportPath
  $report | ConvertTo-Json -Depth 20 | Set-Content -LiteralPath $reportPath -Encoding UTF8
}

$report | ConvertTo-Json -Depth 20
if ($overall -eq "fail") {
  exit 1
}
if ($overall -eq "blocked") {
  exit 2
}
