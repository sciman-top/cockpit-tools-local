param(
  [Parameter(Mandatory=$true)][string]$RepoRoot,
  [Parameter(Mandatory=$true)][string]$ReportDir,
  [string]$LiveRoot = "C:\Users\sciman\.antigravity_cockpit",
  [string]$Model = "gpt-5.5",
  [int]$MaxDrainRequests = 30,
  [int]$DrainIntervalSeconds = 22,
  [int]$HardAffinityClientTimeoutSeconds = 45
)

$ErrorActionPreference = "Stop"
Set-Location -LiteralPath $RepoRoot

function Write-Log {
  param([string]$Message)
  $line = "[{0}] {1}" -f (Get-Date).ToString("o"), $Message
  $line | Tee-Object -FilePath (Join-Path $ReportDir "probe.log") -Append | Out-Null
}

function Set-JsonProperty {
  param([object]$Object, [string]$Name, [object]$Value)
  if ($null -eq $Object.PSObject.Properties[$Name]) {
    $Object | Add-Member -NotePropertyName $Name -NotePropertyValue $Value
  } else {
    $Object.$Name = $Value
  }
}

function Get-HashPrefix {
  param([string]$Value)
  $bytes = [System.Text.Encoding]::UTF8.GetBytes($Value)
  $hash = [System.Security.Cryptography.SHA256]::HashData($bytes)
  $hex = [System.BitConverter]::ToString($hash).Replace("-", "").ToLowerInvariant()
  "sha256:{0}" -f $hex.Substring(0, 12)
}

function ConvertTo-SafeErrorSummary {
  param([string]$Body)
  if ([string]::IsNullOrWhiteSpace($Body)) { return $null }
  try {
    $json = $Body | ConvertFrom-Json
    $err = $json.error
    if ($null -eq $err) { return $null }
    return [ordered]@{
      type = if ($err.type) { [string]$err.type } else { $null }
      code = if ($err.code) { [string]$err.code } else { $null }
      message = if ($err.message) { ([string]$err.message).Substring(0, [Math]::Min(220, ([string]$err.message).Length)) } else { $null }
    }
  } catch {
    return $null
  }
}

function Invoke-JsonHttp {
  param(
    [ValidateSet("GET", "POST")][string]$Method,
    [string]$Uri,
    [string]$ApiKey,
    [object]$Body,
    [hashtable]$ExtraHeaders = @{},
    [int]$TimeoutSeconds = 120
  )
  $client = [System.Net.Http.HttpClient]::new()
  $client.Timeout = [TimeSpan]::FromSeconds($TimeoutSeconds)
  try {
    $methodObj = if ($Method -eq "GET") { [System.Net.Http.HttpMethod]::Get } else { [System.Net.Http.HttpMethod]::Post }
    $request = [System.Net.Http.HttpRequestMessage]::new($methodObj, $Uri)
    if (-not [string]::IsNullOrWhiteSpace($ApiKey)) {
      [void]$request.Headers.TryAddWithoutValidation("Authorization", "Bearer $ApiKey")
    }
    foreach ($key in $ExtraHeaders.Keys) {
      [void]$request.Headers.TryAddWithoutValidation($key, [string]$ExtraHeaders[$key])
    }
    if ($null -ne $Body) {
      $json = $Body | ConvertTo-Json -Depth 20 -Compress
      $request.Content = [System.Net.Http.StringContent]::new($json, [System.Text.Encoding]::UTF8, "application/json")
    }
    $response = $client.SendAsync($request).GetAwaiter().GetResult()
    $text = $response.Content.ReadAsStringAsync().GetAwaiter().GetResult()
    return [ordered]@{
      statusCode = [int]$response.StatusCode
      body = $text
      timedOut = $false
      retryAfter = if ($response.Headers.RetryAfter) { [string]$response.Headers.RetryAfter } else { $null }
      error = ConvertTo-SafeErrorSummary $text
    }
  } catch [System.Threading.Tasks.TaskCanceledException] {
    return [ordered]@{ statusCode = $null; body = $null; timedOut = $true; retryAfter = $null; error = "timeout_or_cancelled" }
  } catch {
    return [ordered]@{ statusCode = $null; body = $null; timedOut = $false; retryAfter = $null; error = $_.Exception.Message }
  } finally {
    $client.Dispose()
  }
}

function Read-AuditEvents {
  param([string]$AuditPath, [int64]$SinceMs)
  if (-not (Test-Path -LiteralPath $AuditPath)) { return @() }
  $events = @()
  Get-Content -LiteralPath $AuditPath -Tail 800 | ForEach-Object {
    try {
      $event = $_ | ConvertFrom-Json
      if ([int64]$event.timestamp -ge $SinceMs) { $events += $event }
    } catch {}
  }
  return @($events)
}

function Wait-GatewayReady {
  param([string]$ConfigPath, [int]$TimeoutSeconds = 90)
  $deadline = (Get-Date).AddSeconds($TimeoutSeconds)
  $last = $null
  while ((Get-Date) -lt $deadline) {
    if (Test-Path -LiteralPath $ConfigPath) {
      $cfg = Get-Content -LiteralPath $ConfigPath -Raw | ConvertFrom-Json
      if ($cfg.port -and $cfg.apiKey) {
        $baseUrl = "http://127.0.0.1:$($cfg.port)/v1"
        $probe = Invoke-JsonHttp -Method GET -Uri "$baseUrl/models" -ApiKey $cfg.apiKey -Body $null -TimeoutSeconds 2
        if ($probe.statusCode -eq 200) {
          return [ordered]@{ baseUrl = $baseUrl; apiKey = $cfg.apiKey; port = $cfg.port }
        }
        $last = "models status=$($probe.statusCode) error=$($probe.error)"
      }
    }
    Start-Sleep -Milliseconds 500
  }
  throw "gateway not ready: $last"
}

function Get-LastRequestEvents {
  param([object[]]$Events, [string]$RequestId)
  @($Events | Where-Object { $_.requestId -eq $RequestId })
}

$gateway = $null
$monitor = $null
$stopSignal = Join-Path $ReportDir "monitor.stop"
$probeRoot = Join-Path ([System.IO.Path]::GetTempPath()) ("cockpit-hla-real-hard-affinity-{0}" -f (Get-Date -Format "yyyyMMddHHmmssfff"))
$runStartedMs = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()
$summaryPath = Join-Path $ReportDir "summary.json"
$gatewayStdout = Join-Path $ReportDir "gateway.stdout.log"
$gatewayStderr = Join-Path $ReportDir "gateway.stderr.log"
$monitorStdout = Join-Path $ReportDir "monitor.stdout.log"
$monitorStderr = Join-Path $ReportDir "monitor.stderr.log"

try {
  New-Item -ItemType Directory -Force -Path $probeRoot | Out-Null
  Copy-Item -LiteralPath (Join-Path $LiveRoot "codex_accounts.json") -Destination (Join-Path $probeRoot "codex_accounts.json") -Force
  Copy-Item -LiteralPath (Join-Path $LiveRoot "codex_accounts") -Destination (Join-Path $probeRoot "codex_accounts") -Recurse -Force

  $liveConfigPath = Join-Path $LiveRoot "codex_local_access.json"
  $config = Get-Content -LiteralPath $liveConfigPath -Raw | ConvertFrom-Json
  $index = Get-Content -LiteralPath (Join-Path $LiveRoot "codex_accounts.json") -Raw | ConvertFrom-Json
  $healthPath = Join-Path $LiveRoot "codex_local_access_health.json"
  $healthyFreeIds = @{}
  if (Test-Path -LiteralPath $healthPath) {
    $health = Get-Content -LiteralPath $healthPath -Raw | ConvertFrom-Json
    foreach ($prop in @($health.accounts.PSObject.Properties)) {
      $id = [string]$prop.Name
      $acct = @($index.accounts | Where-Object { $_.id -eq $id } | Select-Object -First 1)
      if ($acct -and $acct.plan_type -eq "free" -and $prop.Value.status -eq "healthy" -and (Test-Path -LiteralPath (Join-Path (Join-Path $LiveRoot "codex_accounts") ("$id.json")))) {
        $healthyFreeIds[$id] = $true
      }
    }
  }
  $selected = @($index.accounts |
    Where-Object { $healthyFreeIds.ContainsKey($_.id) } |
    Sort-Object @{ Expression = { if ($_.last_used) { [int64]$_.last_used } else { 0 } }; Descending = $true } |
    Select-Object -First 2)
  if ($selected.Count -lt 2) {
    $selected = @($index.accounts |
      Where-Object { $_.plan_type -eq "free" -and (Test-Path -LiteralPath (Join-Path (Join-Path $LiveRoot "codex_accounts") ("$($_.id).json"))) } |
      Sort-Object @{ Expression = { if ($_.last_used) { [int64]$_.last_used } else { 0 } }; Descending = $true } |
      Select-Object -First 2)
  }
  if ($selected.Count -lt 2) { throw "not enough usable free accounts for isolated real probe" }

  Set-JsonProperty $config "enabled" $false
  Set-JsonProperty $config "port" 0
  Set-JsonProperty $config "accountIds" @($selected | ForEach-Object { $_.id })
  Set-JsonProperty $config "updatedAt" ([DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds())
  if ([string]::IsNullOrWhiteSpace([string]$config.apiKey)) {
    Set-JsonProperty $config "apiKey" ([Guid]::NewGuid().ToString("N"))
  }
  if ($null -eq $config.safetyConfig) { Set-JsonProperty $config "safetyConfig" ([pscustomobject]@{}) }
  if ($null -eq $config.safetyConfig.logging) { Set-JsonProperty $config.safetyConfig "logging" ([pscustomobject]@{}) }
  Set-JsonProperty $config.safetyConfig "hardenedLocalMode" $true
  Set-JsonProperty $config.safetyConfig "maxConcurrentRequests" 1
  Set-JsonProperty $config.safetyConfig "maxRetries" 1
  Set-JsonProperty $config.safetyConfig "maxRetryAccounts" 2
  Set-JsonProperty $config.safetyConfig "fallbackMode" "disabled"
  Set-JsonProperty $config.safetyConfig.logging "redactSensitiveValues" $true
  Set-JsonProperty $config.safetyConfig.logging "includePromptResponse" $false
  Set-JsonProperty $config.safetyConfig.logging "includeRawUpstreamBody" $false
  $config | ConvertTo-Json -Depth 20 | Set-Content -LiteralPath (Join-Path $probeRoot "codex_local_access.json") -Encoding UTF8

  $selectedHashes = @($selected | ForEach-Object { Get-HashPrefix $_.id })
  $exe = Join-Path $RepoRoot "target\debug\codex-local-access-gateway.exe"
  if (-not (Test-Path -LiteralPath $exe)) { throw "gateway exe missing: $exe" }
  $exeHash = (Get-FileHash -LiteralPath $exe -Algorithm SHA256).Hash
  [ordered]@{
    schemaVersion = 1
    generatedAt = (Get-Date).ToString("o")
    reportDir = $ReportDir
    probeDataRoot = $probeRoot
    liveRoot = $LiveRoot
    model = $Model
    selectedAccountHashes = $selectedHashes
    gatewayExe = $exe
    gatewayExeSha256 = $exeHash
    maxDrainRequests = $MaxDrainRequests
    drainIntervalSeconds = $DrainIntervalSeconds
    hardAffinityClientTimeoutSeconds = $HardAffinityClientTimeoutSeconds
  } | ConvertTo-Json -Depth 6 | Set-Content -LiteralPath (Join-Path $ReportDir "meta.json") -Encoding UTF8

  Write-Log "prepared isolated data root; selected account hashes: $($selectedHashes -join ', ')"

  $monitorArgs = @(
    "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", (Join-Path $RepoRoot "scripts\monitor-live-codex-app-cockpit-acceptance.ps1"),
    "-DataRoot", $probeRoot,
    "-AuditPath", (Join-Path $probeRoot "codex_local_access_audit.jsonl"),
    "-DurationSeconds", "900",
    "-PollIntervalSeconds", "2",
    "-CheckpointPath", (Join-Path $ReportDir "monitor-checkpoint.json"),
    "-ExitCodeFile", (Join-Path $ReportDir "monitor-exit-code.txt"),
    "-StopSignalFile", $stopSignal,
    "-WriteReport",
    "-RequireQuotaFallback",
    "-RequireStreamCompletion",
    "-Quiet"
  )
  $monitor = Start-Process -FilePath "pwsh" -ArgumentList $monitorArgs -WindowStyle Hidden -PassThru -RedirectStandardOutput $monitorStdout -RedirectStandardError $monitorStderr
  Write-Log "sidecar monitor started pid=$($monitor.Id)"

  $oldDataRoot = [Environment]::GetEnvironmentVariable("COCKPIT_LOCAL_ACCESS_DATA_ROOT", "Process")
  [Environment]::SetEnvironmentVariable("COCKPIT_LOCAL_ACCESS_DATA_ROOT", $probeRoot, "Process")
  try {
    $gateway = Start-Process -FilePath $exe -ArgumentList @("--serve") -WindowStyle Hidden -PassThru -RedirectStandardOutput $gatewayStdout -RedirectStandardError $gatewayStderr
  } finally {
    [Environment]::SetEnvironmentVariable("COCKPIT_LOCAL_ACCESS_DATA_ROOT", $oldDataRoot, "Process")
  }
  Write-Log "gateway started pid=$($gateway.Id)"
  $ready = Wait-GatewayReady -ConfigPath (Join-Path $probeRoot "codex_local_access.json")
  Write-Log "gateway ready baseUrl=$($ready.baseUrl)"

  $auditPath = Join-Path $probeRoot "codex_local_access_audit.jsonl"
  $turnState = "cockpit-real-hard-affinity-$([Guid]::NewGuid().ToString('N'))"
  $turnAuditId = "x-codex-turn-state:$(Get-HashPrefix $turnState)"
  $responseBody = [ordered]@{ model = $Model; stream = $false; messages = @([ordered]@{ role = "user"; content = "Reply with exactly OK." }) }
  $headers = @{ "x-codex-turn-state" = $turnState }

  Write-Log "sending initial sticky chat-to-Responses request"
  $initial = Invoke-JsonHttp -Method POST -Uri "$($ready.baseUrl)/chat/completions" -ApiKey $ready.apiKey -Body $responseBody -ExtraHeaders $headers -TimeoutSeconds 180
  Write-Log "initial sticky chat-to-Responses status=$($initial.statusCode) timedOut=$($initial.timedOut)"
  Start-Sleep -Seconds 2
  $events = Read-AuditEvents -AuditPath $auditPath -SinceMs $runStartedMs
  $turnEvents = Get-LastRequestEvents -Events $events -RequestId $turnAuditId
  $firstAccountHash = @($turnEvents | Where-Object { $_.accountHash -and $_.accountHash -match '^sha256:' } | Select-Object -ExpandProperty accountHash -First 1)

  $drainAttempts = @()
  $quotaObserved = $false
  $quotaAccountHash = $null
  $quotaRetryAfterMs = $null
  if ($initial.statusCode -eq 200 -and $firstAccountHash) {
    for ($i = 1; $i -le $MaxDrainRequests; $i++) {
      Write-Log "drain attempt $i/$MaxDrainRequests"
      $chatBody = [ordered]@{ model = $Model; stream = $false; messages = @([ordered]@{ role = "user"; content = "Reply with exactly OK." }) }
      $chat = Invoke-JsonHttp -Method POST -Uri "$($ready.baseUrl)/chat/completions" -ApiKey $ready.apiKey -Body $chatBody -TimeoutSeconds 120
      $drainAttempts += [ordered]@{ attempt=$i; statusCode=$chat.statusCode; timedOut=$chat.timedOut; retryAfter=$chat.retryAfter; error=$chat.error; bodyHasOK=([string]$chat.body -match 'OK') }
      Start-Sleep -Seconds 1
      $events = Read-AuditEvents -AuditPath $auditPath -SinceMs $runStartedMs
      $quotaEvent = @($events | Where-Object { $_.status -eq 429 -and $_.errorType -eq "usage_limit_reached" -and $_.accountHash -eq $firstAccountHash } | Select-Object -Last 1)
      if ($quotaEvent) {
        $quotaObserved = $true
        $quotaAccountHash = [string]$quotaEvent.accountHash
        if ($quotaEvent.detail -and $quotaEvent.detail.retry_after_ms) { $quotaRetryAfterMs = [int64]$quotaEvent.detail.retry_after_ms }
        Write-Log "real usage_limit_reached observed on sticky account hash=$quotaAccountHash retryAfterMs=$quotaRetryAfterMs"
        break
      }
      if ($i -lt $MaxDrainRequests) { Start-Sleep -Seconds $DrainIntervalSeconds }
    }
  }

  $hardAffinityProbe = [ordered]@{ attempted = $false }
  if ($quotaObserved) {
    Write-Log "sending same-turn hard-affinity Responses request with client timeout ${HardAffinityClientTimeoutSeconds}s"
    $hardAffinityProbe.attempted = $true
    $hardAffinityProbe.startedAt = (Get-Date).ToString("o")
    $second = Invoke-JsonHttp -Method POST -Uri "$($ready.baseUrl)/chat/completions" -ApiKey $ready.apiKey -Body $responseBody -ExtraHeaders $headers -TimeoutSeconds $HardAffinityClientTimeoutSeconds
    $hardAffinityProbe.statusCode = $second.statusCode
    $hardAffinityProbe.timedOut = $second.timedOut
    $hardAffinityProbe.retryAfter = $second.retryAfter
    $hardAffinityProbe.error = $second.error
    $hardAffinityProbe.endedAt = (Get-Date).ToString("o")
    Write-Log "same-turn hard-affinity result status=$($second.statusCode) timedOut=$($second.timedOut)"
    Start-Sleep -Seconds 3
  } else {
    Write-Log "quota not observed within bounded drain; skipping same-turn hard-affinity wait probe"
  }

  $events = Read-AuditEvents -AuditPath $auditPath -SinceMs $runStartedMs
  $turnEvents = Get-LastRequestEvents -Events $events -RequestId $turnAuditId
  $requestTraceEvents = @($turnEvents | Where-Object { $_.phase -eq "request_trace" })
  $lastRequestTrace = $requestTraceEvents | Select-Object -Last 1
  $poolWaitEvents = @($turnEvents | Where-Object { $_.phase -eq "pool_wait" })
  $fallbackBlockedEvents = @($turnEvents | Where-Object { $_.phase -eq "fallback_blocked" -and $_.outcome -eq "hard_affinity" })
  $final429Events = @($turnEvents | Where-Object { $_.phase -eq "final_response" -and [int]$_.status -eq 429 })
  $streamCompletedEvents = @($turnEvents | Where-Object { $_.phase -eq "stream_completed" -and $_.outcome -eq "completed" })
  $usageLimitEvents = @($events | Where-Object { $_.status -eq 429 -and $_.errorType -eq "usage_limit_reached" })
  $fallbackSelectedEvents = @($events | Where-Object { $_.phase -eq "fallback_selected" })
  $newAccount200After429 = $false
  if ($quotaObserved) {
    $blockedHash = $quotaAccountHash
    $newAccount200After429 = [bool](@($events | Where-Object { $_.accountHash -and $_.accountHash -match '^sha256:' -and $_.accountHash -ne $blockedHash -and [int]$_.status -eq 200 }).Count)
  }

  $noImmediateGeneric429 = $false
  if ($hardAffinityProbe.attempted) {
    $noImmediateGeneric429 = [bool]($hardAffinityProbe.timedOut -and @($final429Events).Count -eq 0)
  }

  $overall = if ($initial.statusCode -ne 200 -or -not $firstAccountHash) {
    "fail"
  } elseif (-not $quotaObserved) {
    "blocked"
  } elseif ($noImmediateGeneric429 -and $lastRequestTrace -and $lastRequestTrace.detail.timeout_extended -eq "true") {
    "pass"
  } else {
    "fail"
  }

  [ordered]@{
    schemaVersion = 1
    generatedAt = (Get-Date).ToString("o")
    overall = $overall
    probeDataRoot = $probeRoot
    auditPath = $auditPath
    model = $Model
    selectedAccountHashes = $selectedHashes
    firstStickyAccountHash = $firstAccountHash
    initialStickyResponse = [ordered]@{ statusCode=$initial.statusCode; timedOut=$initial.timedOut; retryAfter=$initial.retryAfter; error=$initial.error }
    quotaObserved = $quotaObserved
    quotaAccountHash = $quotaAccountHash
    quotaRetryAfterMs = $quotaRetryAfterMs
    drainAttempts = $drainAttempts
    hardAffinityProbe = $hardAffinityProbe
    noImmediateGeneric429WithinClientWindow = $noImmediateGeneric429
    requestTraceCount = @($requestTraceEvents).Count
    lastRequestTrace = if ($lastRequestTrace) { $lastRequestTrace.detail } else { $null }
    poolWaitCount = @($poolWaitEvents).Count
    lastPoolWait = if (@($poolWaitEvents).Count) { (@($poolWaitEvents) | Select-Object -Last 1).detail } else { $null }
    hardAffinityFallbackBlockedCount = @($fallbackBlockedEvents).Count
    lastHardAffinityFallbackBlocked = if (@($fallbackBlockedEvents).Count) { (@($fallbackBlockedEvents) | Select-Object -Last 1).detail } else { $null }
    final429CountForTurn = @($final429Events).Count
    streamCompletedCountForTurn = @($streamCompletedEvents).Count
    usageLimitEventCount = @($usageLimitEvents).Count
    fallbackSelectedCount = @($fallbackSelectedEvents).Count
    newAccount200After429 = $newAccount200After429
    auditTailEventCount = @($events).Count
    monitor = [ordered]@{
      checkpointPath = Join-Path $ReportDir "monitor-checkpoint.json"
      exitCodePath = Join-Path $ReportDir "monitor-exit-code.txt"
      stdoutPath = $monitorStdout
      stderrPath = $monitorStderr
    }
  } | ConvertTo-Json -Depth 12 | Set-Content -LiteralPath $summaryPath -Encoding UTF8
  Write-Log "summary written: $summaryPath"
} catch {
  [ordered]@{
    schemaVersion = 1
    generatedAt = (Get-Date).ToString("o")
    overall = "fail"
    error = $_.Exception.Message
    probeDataRoot = $probeRoot
  } | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $summaryPath -Encoding UTF8
  Write-Log "probe failed: $($_.Exception.Message)"
  throw
} finally {
  if ($gateway -and -not $gateway.HasExited) {
    Write-Log "stopping gateway pid=$($gateway.Id)"
    Stop-Process -Id $gateway.Id -Force -ErrorAction SilentlyContinue
  }
  if ($monitor -and -not $monitor.HasExited) {
    "stop" | Set-Content -LiteralPath $stopSignal -Encoding ASCII
    $exited = $monitor.WaitForExit(20000)
    if (-not $exited -and -not $monitor.HasExited) {
      Write-Log "monitor did not exit after stop signal; stopping exact pid=$($monitor.Id)"
      Stop-Process -Id $monitor.Id -Force -ErrorAction SilentlyContinue
    }
  }
}


