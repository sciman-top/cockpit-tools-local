param(
  [string]$BaseUrl,
  [string]$ApiKey,
  [string]$Model = "gpt-5.4",
  [ValidateSet("single", "small_pool", "fallback_probe")]
  [string]$Stage = "single",
  [switch]$RunUpstreamSmoke,
  [switch]$RunCodexExecSmoke,
  [switch]$AcknowledgeLiveUpstreamRisk,
  [switch]$AcknowledgeExpandedLiveUpstreamRisk,
  [switch]$RequireQuotaFallback,
  [switch]$Expect429,
  [switch]$WriteReport,
  [switch]$StartEphemeralGateway,
  [int]$EphemeralGatewayReadyTimeoutSeconds = 60,
  [switch]$SkipEphemeralGatewayBuild,
  [switch]$TemporaryFallbackConfig,
  [switch]$AssertCodexCliConfigUntouched,
  [string]$CodexHome = (Join-Path $HOME ".codex"),
  [switch]$AppSafeIsolatedProbe,
  [switch]$AutoDrainFirstFreeAccountUntilFallback,
  [ValidateRange(1, 200)]
  [int]$AutoDrainMaxRequests = 30,
  [ValidateRange(0, 300)]
  [int]$AutoDrainRequestIntervalSeconds = 22,
  [switch]$AssertCodexAppProcessStable,
  [string[]]$CodexAppProcessNames = @("Codex"),
  [string]$DataRoot = (Join-Path $HOME ".antigravity_cockpit")
)

$ErrorActionPreference = "Stop"

$liveUpstreamRiskRequests = [ordered]@{
  runUpstreamSmoke = [bool]$RunUpstreamSmoke
  runCodexExecSmoke = [bool]$RunCodexExecSmoke
  autoDrainFirstFreeAccountUntilFallback = [bool]$AutoDrainFirstFreeAccountUntilFallback
}
$liveUpstreamRiskRequested = [bool](@($liveUpstreamRiskRequests.GetEnumerator() | Where-Object { $_.Value }).Count)
$expandedLiveUpstreamRiskReasons = @()
if ($AutoDrainFirstFreeAccountUntilFallback -and $AutoDrainMaxRequests -gt 30) {
  $expandedLiveUpstreamRiskReasons += "drain_max_requests_gt_30"
}
if ($AutoDrainFirstFreeAccountUntilFallback -and $AutoDrainRequestIntervalSeconds -lt 20) {
  $expandedLiveUpstreamRiskReasons += "drain_request_interval_lt_20s"
}

if ($liveUpstreamRiskRequested -and -not $AcknowledgeLiveUpstreamRisk) {
  [ordered]@{
    overall = "blocked"
    reason = "live_upstream_risk_ack_required"
    requiredSwitch = "-AcknowledgeLiveUpstreamRisk"
    requested = $liveUpstreamRiskRequests
    drainMaxRequests = $AutoDrainMaxRequests
    drainRequestIntervalSeconds = $AutoDrainRequestIntervalSeconds
  } | ConvertTo-Json -Depth 8
  exit 2
}

if ($expandedLiveUpstreamRiskReasons.Count -gt 0 -and -not $AcknowledgeExpandedLiveUpstreamRisk) {
  [ordered]@{
    overall = "blocked"
    reason = "expanded_live_upstream_risk_ack_required"
    requiredSwitch = "-AcknowledgeExpandedLiveUpstreamRisk"
    expandedReasons = @($expandedLiveUpstreamRiskReasons)
    drainMaxRequests = $AutoDrainMaxRequests
    drainRequestIntervalSeconds = $AutoDrainRequestIntervalSeconds
  } | ConvertTo-Json -Depth 8
  exit 2
}

$script:LiveDataRoot = $DataRoot
$script:ProbeDataRoot = $DataRoot
$script:AppSafeProbe = [ordered]@{
  requested = [bool]$AppSafeIsolatedProbe
  status = if ($AppSafeIsolatedProbe) { "pending" } else { "not_requested" }
}

function New-SmokeResult {
  param([string]$Name)
  [ordered]@{
    name = $Name
    status = "pending"
    evidence = [ordered]@{}
    reason = $null
  }
}

function Set-SmokePass {
  param([System.Collections.IDictionary]$Result, [hashtable]$Evidence = @{})
  $Result.status = "pass"
  foreach ($key in $Evidence.Keys) {
    $Result.evidence[$key] = $Evidence[$key]
  }
}

function Set-SmokeFail {
  param([System.Collections.IDictionary]$Result, [string]$Reason, [hashtable]$Evidence = @{})
  $Result.status = "fail"
  $Result.reason = $Reason
  foreach ($key in $Evidence.Keys) {
    $Result.evidence[$key] = $Evidence[$key]
  }
}

function Set-SmokeBlocked {
  param([System.Collections.IDictionary]$Result, [string]$Reason, [hashtable]$Evidence = @{})
  $Result.status = "blocked"
  $Result.reason = $Reason
  foreach ($key in $Evidence.Keys) {
    $Result.evidence[$key] = $Evidence[$key]
  }
}

function Set-SmokeSkipped {
  param([System.Collections.IDictionary]$Result, [string]$Reason, [hashtable]$Evidence = @{})
  $Result.status = "skipped"
  $Result.reason = $Reason
  foreach ($key in $Evidence.Keys) {
    $Result.evidence[$key] = $Evidence[$key]
  }
}

function Get-DataRoot {
  $script:ProbeDataRoot
}

function Get-LiveDataRoot {
  $script:LiveDataRoot
}

function Get-LocalAccessConfigPath {
  Join-Path (Get-DataRoot) "codex_local_access.json"
}

function Get-LiveLocalAccessConfigPath {
  Join-Path (Get-LiveDataRoot) "codex_local_access.json"
}

function Get-LiveCodexAccountsIndexPath {
  Join-Path (Get-LiveDataRoot) "codex_accounts.json"
}

function Get-LiveCodexAccountDetailPath {
  param([string]$AccountId)
  Join-Path (Join-Path (Get-LiveDataRoot) "codex_accounts") ("{0}.json" -f $AccountId)
}

function Get-StableHashPrefix {
  param([string]$Value)
  $bytes = [System.Text.Encoding]::UTF8.GetBytes($Value)
  $hash = [System.Security.Cryptography.SHA256]::HashData($bytes)
  $hex = [System.BitConverter]::ToString($hash).Replace("-", "").ToLowerInvariant()
  "sha256:{0}" -f $hex.Substring(0, 12)
}

function Test-TruthyJsonValue {
  param([object]$Value)
  if ($null -eq $Value) {
    return $false
  }
  if ($Value -is [bool]) {
    return [bool]$Value
  }
  $text = [string]$Value
  $text -match '^(?i:true|1|yes)$'
}

function Get-UniqueTrimmedStrings {
  param([object[]]$Values)
  $seen = @{}
  $result = @()
  foreach ($value in $Values) {
    if ($null -eq $value) {
      continue
    }
    $text = ([string]$value).Trim()
    if (-not $text -or $seen.ContainsKey($text)) {
      continue
    }
    $seen[$text] = $true
    $result += $text
  }
  $result
}

function Get-CodexCliGuardState {
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

function Compare-CodexCliGuardState {
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
    changedFiles = $changed
  }
}

function Test-CodexCliGuardUnchanged {
  param([System.Collections.IDictionary]$Comparison)
  $result = New-SmokeResult "codex_cli_config_auth_untouched"
  $evidence = @{
    unchanged = [bool]$Comparison.unchanged
    changedFiles = @($Comparison.changedFiles)
  }
  if ($Comparison.unchanged) {
    Set-SmokePass $result $evidence
  } else {
    Set-SmokeFail $result "当前 Codex CLI 的 config.toml/auth.json 在 probe 期间发生变化" $evidence
  }
  $result
}

function Get-CodexAppProcessGuardState {
  param([string[]]$ProcessNames)
  $items = @()
  foreach ($name in $ProcessNames) {
    $items += @(Get-Process -Name $name -ErrorAction SilentlyContinue | Where-Object { $_.ProcessName -ceq $name } | ForEach-Object {
      [ordered]@{
        processName = $_.ProcessName
        id = $_.Id
        startTime = try { $_.StartTime.ToString("o") } catch { $null }
      }
    })
  }

  [ordered]@{
    processNames = @($ProcessNames)
    processes = @($items | Sort-Object processName, id)
  }
}

function Compare-CodexAppProcessGuardState {
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

function Test-CodexAppProcessStable {
  param([System.Collections.IDictionary]$Comparison)
  $result = New-SmokeResult "codex_app_process_stable"
  $evidence = @{
    stable = [bool]$Comparison.stable
    beforeCount = [int]$Comparison.beforeCount
    afterCount = [int]$Comparison.afterCount
  }
  if ($Comparison.stable) {
    Set-SmokePass $result $evidence
  } else {
    Set-SmokeFail $result "Codex App 进程集合在 probe 期间发生变化" $evidence
  }
  $result
}

function Set-JsonProperty {
  param(
    [object]$Object,
    [string]$Name,
    [object]$Value
  )

  if ($Object.PSObject.Properties[$Name]) {
    $Object.$Name = $Value
  } else {
    Add-Member -InputObject $Object -NotePropertyName $Name -NotePropertyValue $Value
  }
}

function Initialize-AppSafeIsolatedProbeRoot {
  $sourcePath = Get-LiveLocalAccessConfigPath
  if (-not (Test-Path -LiteralPath $sourcePath)) {
    throw "无法创建 App-safe isolated probe：live codex_local_access.json 不存在"
  }

  $root = Join-Path ([System.IO.Path]::GetTempPath()) ("cockpit-hla-appsafe-{0}-{1}" -f $PID, (Get-Date -Format "yyyyMMddHHmmssfff"))
  New-Item -ItemType Directory -Force -Path $root | Out-Null
  $targetPath = Join-Path $root "codex_local_access.json"
  $config = Get-Content -LiteralPath $sourcePath -Raw | ConvertFrom-Json
  Set-JsonProperty $config "enabled" $false
  Set-JsonProperty $config "port" 0
  $config | ConvertTo-Json -Depth 20 | Set-Content -LiteralPath $targetPath -Encoding UTF8
  $script:ProbeDataRoot = $root

  [ordered]@{
    requested = $true
    status = "initialized"
    mode = "isolated_data_root"
    liveDataRoot = Get-LiveDataRoot
    probeDataRoot = $root
    sourceConfigPath = $sourcePath
    probeConfigPath = $targetPath
    port = 0
  }
}

function Get-LocalAccessConfig {
  $path = Get-LocalAccessConfigPath
  if (-not (Test-Path -LiteralPath $path)) {
    return $null
  }

  Get-Content -LiteralPath $path -Raw | ConvertFrom-Json
}

function Set-TemporaryFallbackProbeConfig {
  $path = Get-LocalAccessConfigPath
  if (-not (Test-Path -LiteralPath $path)) {
    throw "无法临时设置 fallback probe config：codex_local_access.json 不存在"
  }

  $config = Get-Content -LiteralPath $path -Raw | ConvertFrom-Json
  $accountCount = if ($config.accountIds) { @($config.accountIds).Count } else { 0 }
  if ($accountCount -lt 1) {
    throw "fallback_probe 需要至少 1 个手动配置的 API 服务号池账号，当前 accountCount=$accountCount；请先在 Cockpit API 服务号池中添加账号后再运行验收"
  }

  if ($null -eq $config.safetyConfig) {
    Set-JsonProperty $config "safetyConfig" ([pscustomobject]@{})
  }
  $safety = $config.safetyConfig
  if ($null -eq $safety.logging) {
    Set-JsonProperty $safety "logging" ([pscustomobject]@{})
  }

  Set-JsonProperty $config "enabled" $true
  Set-JsonProperty $config "updatedAt" ([DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds())
  Set-JsonProperty $safety "schemaVersion" 1
  Set-JsonProperty $safety "hardenedLocalMode" $true
  Set-JsonProperty $safety "maxConcurrentRequests" 1
  Set-JsonProperty $safety "maxRetries" 1
  Set-JsonProperty $safety "maxRetryAccounts" 2
  Set-JsonProperty $safety "fallbackMode" "disabled"
  Set-JsonProperty $safety.logging "redactSensitiveValues" $true
  Set-JsonProperty $safety.logging "includePromptResponse" $false
  Set-JsonProperty $safety.logging "includeRawUpstreamBody" $false

  $config | ConvertTo-Json -Depth 20 | Set-Content -LiteralPath $path -Encoding UTF8

  [ordered]@{
    requested = $true
    status = "applied"
    path = $path
    accountCount = $accountCount
    accountHashes = @(Get-AccountHashList @($config.accountIds))
    accountPoolSource = "existing_config"
    enabled = $true
    maxRetryAccounts = 2
    fallbackMode = "disabled"
    restoredBy = "Stop-EphemeralGateway"
  }
}

function Resolve-BaseUrl {
  param($Config)
  if ($BaseUrl) {
    return $BaseUrl.TrimEnd("/")
  }
  if ($null -eq $Config -or -not $Config.port) {
    return $null
  }
  "http://127.0.0.1:$($Config.port)/v1"
}

function Resolve-ApiKey {
  param($Config)
  if ($ApiKey) {
    return $ApiKey
  }
  if ($null -eq $Config -or -not $Config.apiKey) {
    return $null
  }
  $Config.apiKey
}

function Invoke-JsonRequest {
  param(
    [string]$Method,
    [string]$Uri,
    [hashtable]$Headers = @{},
    [object]$Body = $null,
    [int]$TimeoutSeconds = 30
  )

  $response = $null
  $errorBody = $null
  try {
    $params = @{
      Method = $Method
      Uri = $Uri
      Headers = $Headers
      TimeoutSec = $TimeoutSeconds
      SkipHttpErrorCheck = $true
    }
    if ($null -ne $Body) {
      $params.ContentType = "application/json"
      $params.Body = ($Body | ConvertTo-Json -Depth 12 -Compress)
    }
    $response = Invoke-WebRequest @params
  } catch {
    if ($_.Exception.Response) {
      $response = $_.Exception.Response
      try {
        $reader = [System.IO.StreamReader]::new($response.GetResponseStream())
        $errorBody = $reader.ReadToEnd()
      } catch {
        $errorBody = $null
      }
    } else {
      throw
    }
  }

  [ordered]@{
    statusCode = [int]$response.StatusCode
    retryAfter = $response.Headers["Retry-After"]
    contentType = $response.Headers["Content-Type"]
    body = if ($errorBody) { $errorBody } else { [string]$response.Content }
  }
}

function Get-JsonFileSummary {
  param([string]$Path)
  if (-not (Test-Path -LiteralPath $Path)) {
    return [ordered]@{ exists = $false }
  }
  $item = Get-Item -LiteralPath $Path
  $summary = [ordered]@{
    exists = $true
    length = $item.Length
    lastWriteTime = $item.LastWriteTime.ToString("o")
  }
  try {
    $json = Get-Content -LiteralPath $Path -Raw | ConvertFrom-Json
    $summary.schemaVersion = $json.schemaVersion
    $summary.accountCount = if ($json.accounts) { ($json.accounts.PSObject.Properties | Measure-Object).Count } else { 0 }
    $summary.modelCooldownCount = if ($json.modelCooldowns) { ($json.modelCooldowns.PSObject.Properties | Measure-Object).Count } else { 0 }
    $summary.stickyBindingCount = if ($json.stickyBindings) { ($json.stickyBindings.PSObject.Properties | Measure-Object).Count } else { 0 }
    $summary.lastGlobalError = if ($json.lastGlobalError) { $json.lastGlobalError.errorType } else { $null }
  } catch {
    $summary.parseError = $_.Exception.Message
  }
  $summary
}

function Get-AccountHashList {
  param([object[]]$AccountIds)
  $hashes = @()
  foreach ($accountId in @($AccountIds)) {
    $value = [string]$accountId
    if (-not $value.Trim()) {
      continue
    }
    $hashes += Get-StableHashPrefix $value
  }
  @($hashes)
}

function Update-NearestCooldownMs {
  param([System.Collections.IDictionary]$Summary, [object]$CooldownUntilMs, [int64]$NowMs)
  if ($null -eq $CooldownUntilMs) {
    return
  }
  $until = [int64]$CooldownUntilMs
  if ($until -le $NowMs) {
    return
  }
  if ($null -eq $Summary.nearestCooldownUntilMs -or $until -lt [int64]$Summary.nearestCooldownUntilMs) {
    $Summary.nearestCooldownUntilMs = $until
  }
}

function Get-ScopedHealthFileSummary {
  param($Config, [string]$Path)
  $accountIds = if ($Config -and $Config.accountIds) { @($Config.accountIds) } else { @() }
  $accountSet = @{}
  foreach ($accountId in $accountIds) {
    $value = [string]$accountId
    if ($value.Trim()) {
      $accountSet[$value] = $true
    }
  }

  $summary = [ordered]@{
    exists = (Test-Path -LiteralPath $Path)
    scope = "current_config_account_ids"
    accountCount = $accountIds.Count
    accountHashes = @(Get-AccountHashList $accountIds)
    registryAccountCount = 0
    registryModelCooldownCount = 0
    healthyCount = 0
    estimatedAvailableCount = 0
    coolingCount = 0
    exhaustedCount = 0
    authSuspectCount = 0
    manualRequiredCount = 0
    disabledCount = 0
    activeModelCooldownCount = 0
    nearestCooldownUntilMs = $null
    lastErrorType = $null
    lastStatus = $null
    lastRequestId = $null
    lastGlobalError = $null
  }
  if (-not $summary.exists) {
    return $summary
  }

  $item = Get-Item -LiteralPath $Path
  $summary.length = $item.Length
  $summary.lastWriteTime = $item.LastWriteTime.ToString("o")
  $nowMs = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()
  $lastErrorUpdatedAt = [int64]::MinValue
  $observed = @{}

  try {
    $json = Get-Content -LiteralPath $Path -Raw | ConvertFrom-Json
    $summary.schemaVersion = $json.schemaVersion
    $summary.registryAccountCount = if ($json.accounts) { ($json.accounts.PSObject.Properties | Measure-Object).Count } else { 0 }
    $summary.registryModelCooldownCount = if ($json.modelCooldowns) { ($json.modelCooldowns.PSObject.Properties | Measure-Object).Count } else { 0 }
    $summary.lastGlobalError = if ($json.lastGlobalError) { $json.lastGlobalError.errorType } else { $null }

    if ($json.accounts) {
      foreach ($prop in $json.accounts.PSObject.Properties) {
        $accountId = [string]$prop.Name
        if (-not $accountSet.ContainsKey($accountId)) {
          continue
        }
        $observed[$accountId] = $true
        $account = $prop.Value
        switch ([string]$account.status) {
          "healthy" { $summary.healthyCount++ }
          "estimated_available" { $summary.estimatedAvailableCount++ }
          "cooling_down" { $summary.coolingCount++ }
          "exhausted" { $summary.exhaustedCount++ }
          "auth_suspect" { $summary.authSuspectCount++ }
          "manual_required" { $summary.manualRequiredCount++ }
          "disabled" { $summary.disabledCount++ }
          default { $summary.healthyCount++ }
        }
        if ($account.manualRequired -and [string]$account.status -ne "manual_required") {
          $summary.manualRequiredCount++
        }
        Update-NearestCooldownMs $summary $account.cooldownUntilMs $nowMs
        Update-NearestCooldownMs $summary $account.estimatedResetAtMs $nowMs
        if ($account.lastErrorType -and [int64]$account.updatedAt -ge $lastErrorUpdatedAt) {
          $lastErrorUpdatedAt = [int64]$account.updatedAt
          $summary.lastErrorType = [string]$account.lastErrorType
          $summary.lastStatus = $account.lastStatus
          $summary.lastRequestId = $account.lastRequestId
        }
      }
    }

    foreach ($accountId in $accountIds) {
      $value = [string]$accountId
      if ($value.Trim() -and -not $observed.ContainsKey($value)) {
        $summary.healthyCount++
      }
    }

    if ($json.modelCooldowns) {
      foreach ($prop in $json.modelCooldowns.PSObject.Properties) {
        $cooldown = $prop.Value
        $accountId = [string]$cooldown.accountId
        if (-not $accountSet.ContainsKey($accountId)) {
          continue
        }
        if ([int64]$cooldown.cooldownUntilMs -gt $nowMs) {
          $summary.activeModelCooldownCount++
          Update-NearestCooldownMs $summary $cooldown.cooldownUntilMs $nowMs
        }
        if ($cooldown.lastErrorType -and [int64]$cooldown.updatedAt -ge $lastErrorUpdatedAt) {
          $lastErrorUpdatedAt = [int64]$cooldown.updatedAt
          $summary.lastErrorType = [string]$cooldown.lastErrorType
          $summary.lastStatus = $null
          $summary.lastRequestId = $cooldown.lastRequestId
        }
      }
    }
  } catch {
    $summary.parseError = $_.Exception.Message
  }

  $summary
}

function Get-AuditTailSummary {
  param([string]$Path, [int]$Tail = 120)
  if (-not (Test-Path -LiteralPath $Path)) {
    return [ordered]@{ exists = $false }
  }
  $item = Get-Item -LiteralPath $Path
  $events = @()
  Get-Content -LiteralPath $Path -Tail $Tail | ForEach-Object {
    try {
      $events += ($_ | ConvertFrom-Json)
    } catch {
    }
  }
  $attemptedAccountHashes = @(
    $events |
      ForEach-Object { $_.accountHash } |
      Where-Object { $_ -and [string]$_ -ne "-" } |
      Select-Object -Unique
  )
  [ordered]@{
    exists = $true
    length = $item.Length
    lastWriteTime = $item.LastWriteTime.ToString("o")
    phases = @($events | ForEach-Object { $_.phase } | Where-Object { $_ } | Select-Object -Unique)
    errorTypes = @($events | ForEach-Object { $_.errorType } | Where-Object { $_ } | Select-Object -Unique)
    statuses = @($events | ForEach-Object { $_.status } | Where-Object { $null -ne $_ } | Select-Object -Unique)
    attemptedAccountCount = $attemptedAccountHashes.Count
    attemptedAccountHashes = @($attemptedAccountHashes)
    hasSensitiveMarkers = [bool](@($events | ConvertTo-Json -Depth 8) -match '(authorization|cookie|token|api[_-]?key|sk-[A-Za-z0-9])')
  }
}

function Get-SmokeResultByName {
  param([object[]]$Results, [string]$Name)
  @($Results | Where-Object { $_.name -eq $Name } | Select-Object -First 1)
}

function Get-SmokeFallbackBlockedReason {
  param([object[]]$Results)
  $sameTask = Get-SmokeResultByName $Results "same_task_affinity_fallback_blocked"
  if ($sameTask -and $sameTask.status -eq "pass") {
    return $null
  }
  if ($sameTask -and $sameTask.reason) {
    return [string]$sameTask.reason
  }
  if (-not $RunUpstreamSmoke -and -not $RunCodexExecSmoke) {
    return "upstream_probe_not_requested"
  }
  if ($Stage -eq "single") {
    return "single_stage_account_pool"
  }
  $null
}

function Test-SmokeValidAccountHash {
  param([object]$Value)
  $text = [string]$Value
  $text -and $text -ne "-" -and $text -match '^sha256:'
}

function Test-SmokeUsageLimitEvent {
  param([object]$Event)
  [string]$Event.errorType -eq "usage_limit_reached" -or
    [string]$Event.errorType -eq "insufficient_quota" -or
    [string]$Event.detail.provider_code -eq "usage_limit_reached" -or
    [string]$Event.detail.provider_code -eq "insufficient_quota"
}

function New-SmokeRoutingReport {
  param($Config, [object[]]$Results, $AuditSummary)
  $accountIds = if ($Config -and $Config.accountIds) { @($Config.accountIds) } else { @() }
  $safety = if ($Config) { $Config.safetyConfig } else { $null }
  $rawMaxRetryAccounts = if ($safety -and $safety.maxRetryAccounts) {
    [int]$safety.maxRetryAccounts
  } else {
    1
  }
  $effectiveMaxRetryAccounts = [Math]::Max($rawMaxRetryAccounts, 2)
  [ordered]@{
    candidate_pool_count = $accountIds.Count
    effective_max_retry_accounts = $effectiveMaxRetryAccounts
    attempted_account_count = if ($AuditSummary -and $AuditSummary.exists) { [int]$AuditSummary.attemptedAccountCount } else { 0 }
    fallback_blocked_reason = Get-SmokeFallbackBlockedReason $Results
    stage = $Stage
    fallback_mode = if ($safety -and $safety.fallbackMode) { [string]$safety.fallbackMode } else { $null }
  }
}

function New-SmokePoolUnavailableReport {
  param($HealthSummary)
  $nowMs = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()
  $nearestRetryAfterMs = $null
  if ($HealthSummary -and $HealthSummary.nearestCooldownUntilMs) {
    $nearestRetryAfterMs = [Math]::Max(0, [int64]$HealthSummary.nearestCooldownUntilMs - $nowMs)
  }
  [ordered]@{
    nearest_retry_after_ms = $nearestRetryAfterMs
    blocking_status_counts = [ordered]@{
      cooling = if ($HealthSummary) { [int]$HealthSummary.coolingCount } else { 0 }
      exhausted = if ($HealthSummary) { [int]$HealthSummary.exhaustedCount } else { 0 }
      auth_suspect = if ($HealthSummary) { [int]$HealthSummary.authSuspectCount } else { 0 }
      manual_required = if ($HealthSummary) { [int]$HealthSummary.manualRequiredCount } else { 0 }
      disabled = if ($HealthSummary) { [int]$HealthSummary.disabledCount } else { 0 }
      model_cooldown = if ($HealthSummary) { [int]$HealthSummary.activeModelCooldownCount } else { 0 }
    }
  }
}

function Get-QuotaFallbackAuditEvidence {
  param([string]$Path)
  if (-not (Test-Path -LiteralPath $Path)) {
    return [ordered]@{
      auditPath = $Path
      auditExists = $false
      pass = $false
    }
  }

  $events = @()
  Get-Content -LiteralPath $Path -Tail 120 | ForEach-Object {
    try {
      $events += ($_ | ConvertFrom-Json)
    } catch {
    }
  }

  $has429 = [bool](@($events | Where-Object { $_.status -eq 429 }).Count)
  $has200 = [bool](@($events | Where-Object { $_.status -eq 200 }).Count)
  $hasModelCooldown = [bool](@($events | Where-Object { $_.phase -eq "model_cooldown_applied" }).Count)
  $hasFallbackSelected = [bool](@($events | Where-Object { $_.phase -eq "fallback_selected" }).Count)
  $hasFallbackBlocked = [bool](@($events | Where-Object { $_.phase -eq "fallback_blocked" }).Count)
  $hasHardAffinityFallbackBlocked = [bool](@($events | Where-Object { $_.phase -eq "fallback_blocked" -and $_.outcome -eq "hard_affinity" }).Count)
  $hasUsageLimit = [bool](@($events | Where-Object { $_.errorType -eq "usage_limit_reached" }).Count)
  $first429Index = -1
  $first200After429Index = -1
  $firstBlockedAccountIndex = -1
  $blockedAccountHashes = @()
  $blockedAccountRecords = @()
  $fallbackCycleCount = 0
  $sameTaskAffinityBlockTransitions = @()
  $healthyAccountHashesAfterFallback = @()
  for ($i = 0; $i -lt $events.Count; $i++) {
    $event = $events[$i]
    if ($first429Index -lt 0 -and $event.status -eq 429) {
      $first429Index = $i
    }
    if ($first429Index -ge 0 -and $i -gt $first429Index -and $event.status -eq 200) {
      $first200After429Index = $i
      break
    }
  }
  for ($i = 0; $i -lt $events.Count; $i++) {
    $event = $events[$i]
    $isBlockingAccountEvent = (Test-SmokeValidAccountHash $event.accountHash) -and (
      ($event.status -eq 429 -and (Test-SmokeUsageLimitEvent $event)) -or
      $event.phase -eq "model_cooldown_applied" -or
      $event.phase -eq "fallback_selected" -or
      $event.phase -eq "fallback_blocked"
    )
    if ($isBlockingAccountEvent) {
      if ($firstBlockedAccountIndex -lt 0) {
        $firstBlockedAccountIndex = $i
      }
      $blockedAccountHashes += $event.accountHash
      $blockedAccountRecords += [ordered]@{
        index = $i
        accountHash = $event.accountHash
        requestId = $event.requestId
        phase = $event.phase
      }
    }

    if ($event.phase -eq "fallback_selected") {
      $next200 = $null
      for ($j = $i + 1; $j -lt $events.Count; $j++) {
        $candidate = $events[$j]
        if ($candidate.requestId -ne $event.requestId) {
          continue
        }
        if ($candidate.phase -eq "listener") {
          break
        }
        if ($candidate.status -eq 200 -and (Test-SmokeValidAccountHash $candidate.accountHash)) {
          $next200 = $candidate
          break
        }
      }
      if ($next200 -and (Test-SmokeValidAccountHash $event.accountHash) -and $next200.accountHash -ne $event.accountHash) {
        $fallbackCycleCount++
        $healthyAccountHashesAfterFallback += $next200.accountHash
      }
    }
    if ($event.phase -eq "fallback_blocked" -and $event.outcome -eq "hard_affinity") {
      $localCompletion = $null
      $terminalCompletion = $null
      $terminal429 = $null
      for ($j = $i + 1; $j -lt $events.Count; $j++) {
        $candidate = $events[$j]
        if ($candidate.requestId -ne $event.requestId) {
          continue
        }
        if ($candidate.phase -eq "listener") {
          break
        }
        if (
          $candidate.status -eq 200 -and
          $candidate.errorType -eq "pool_unavailable" -and
          ($candidate.streamState -eq "completed" -or $candidate.outcome -eq "in_band_local_completion" -or ($candidate | ConvertTo-Json -Depth 10 -Compress) -match 'response\.completed')
        ) {
          $localCompletion = $candidate
          break
        }
        if (
          $candidate.phase -eq "stream_completed" -and
          $candidate.outcome -eq "completed" -and
          $candidate.errorType -ne "pool_unavailable"
        ) {
          $terminalCompletion = $candidate
          break
        }
        if (
          $candidate.phase -eq "final_response" -and
          [int]$candidate.status -eq 429
        ) {
          $terminal429 = $candidate
          break
        }
      }
      $sameTaskAffinityBlockTransitions += [ordered]@{
        requestId = $event.requestId
        blockedAccountHash = $event.accountHash
        localCompletionAccountHash = if ($localCompletion) { $localCompletion.accountHash } else { $null }
        terminalCompletionAccountHash = if ($terminalCompletion) { $terminalCompletion.accountHash } else { $null }
        completedLocally = [bool]($null -ne $localCompletion)
        terminalCompleted = [bool]($null -ne $terminalCompletion)
        terminal429 = [bool]($null -ne $terminal429)
      }
    }
  }
  $blockedAccountHashes = @($blockedAccountHashes | Sort-Object -Unique)

  $newRequestGroups = @{}
  for ($i = 0; $i -lt $events.Count; $i++) {
    $event = $events[$i]
    $requestId = [string]$event.requestId
    if (-not $requestId -or $requestId -eq "-") {
      continue
    }
    if (-not $newRequestGroups.ContainsKey($requestId)) {
      $newRequestGroups[$requestId] = [ordered]@{
        requestId = $requestId
        firstIndex = $i
        accountEvents = @()
      }
    }
    $group = $newRequestGroups[$requestId]
    if (Test-SmokeValidAccountHash $event.accountHash) {
      $serializedEvent = $event | ConvertTo-Json -Depth 10 -Compress
      $isLocalPoolUnavailable = $event.errorType -eq "pool_unavailable" -or $serializedEvent -match 'pool_unavailable|API 服务号池|API 服务账号均在冷却|可用账号均在冷却|本地接入集合暂无可用账号'
      $group.accountEvents += [ordered]@{
        accountHash = $event.accountHash
        status = $event.status
        isLocalPoolUnavailable = [bool]$isLocalPoolUnavailable
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
          blockedAccountHashes = @($blockedUsed)
          knownBlockedBeforeRequest = @($knownBlockedBeforeRequest)
        }
      } elseif ($healthyUsed.Count -gt 0) {
        $newRequestAvoidance += [ordered]@{
          requestId = $group.requestId
          knownBlockedBeforeRequest = @($knownBlockedBeforeRequest)
          healthyAccountHashes = @($healthyUsed)
        }
      }
    }
  }

  $currentPass = $has429 -and $has200 -and $hasUsageLimit -and $hasModelCooldown -and $hasFallbackSelected -and ($first200After429Index -gt $first429Index -and $first429Index -ge 0) -and $fallbackCycleCount -gt 0
  $newRequestAvoidancePass = $newRequestAvoidance.Count -gt 0 -and $newRequestBlockedReuse.Count -eq 0
  $completedSameTaskAffinityBlocks = @($sameTaskAffinityBlockTransitions | Where-Object { $_.completedLocally })
  $terminalCompletedSameTaskAffinityBlocks = @($sameTaskAffinityBlockTransitions | Where-Object { $_.terminalCompleted })
  $terminal429SameTaskAffinityBlocks = @($sameTaskAffinityBlockTransitions | Where-Object { $_.terminal429 })
  $distinctHealthyAccountHashesAfterBlock = @(
    $newRequestAvoidance |
      ForEach-Object { $_.healthyAccountHashes } |
      Sort-Object -Unique
  )
  $sameTaskPass = $has429 -and
    $hasUsageLimit -and
    $hasModelCooldown -and
    $hasHardAffinityFallbackBlocked -and
    $terminalCompletedSameTaskAffinityBlocks.Count -gt 0 -and
    $completedSameTaskAffinityBlocks.Count -eq 0 -and
    $terminal429SameTaskAffinityBlocks.Count -eq 0

  [ordered]@{
    auditPath = $Path
    auditExists = $true
    tailEventCount = $events.Count
    has429 = $has429
    has200 = $has200
    hasUsageLimitReached = $hasUsageLimit
    hasModelCooldownApplied = $hasModelCooldown
    hasFallbackSelected = $hasFallbackSelected
    hasFallbackBlocked = $hasFallbackBlocked
    hasHardAffinityFallbackBlocked = $hasHardAffinityFallbackBlocked
    legacyHas200After429 = ($first200After429Index -gt $first429Index -and $first429Index -ge 0)
    fallbackCycleCount = $fallbackCycleCount
    distinctHealthyAccountCountAfterFallback = @($healthyAccountHashesAfterFallback | Sort-Object -Unique).Count
    sameTaskAffinityFallbackBlockedCount = $sameTaskAffinityBlockTransitions.Count
    sameTaskAffinityLocalCompletionCount = $completedSameTaskAffinityBlocks.Count
    sameTaskAffinityTerminalCompletionCount = $terminalCompletedSameTaskAffinityBlocks.Count
    sameTaskAffinityUnrecoveredTerminal429Count = $terminal429SameTaskAffinityBlocks.Count
    sameTaskAffinityFallbackBlockedRequestIds = @($sameTaskAffinityBlockTransitions | ForEach-Object { $_.requestId } | Sort-Object -Unique)
    sameTaskAffinityTerminalCompletionRequestIds = @($terminalCompletedSameTaskAffinityBlocks | ForEach-Object { $_.requestId } | Sort-Object -Unique)
    sameTaskAffinityUnrecoveredTerminal429RequestIds = @($terminal429SameTaskAffinityBlocks | ForEach-Object { $_.requestId } | Sort-Object -Unique)
    sameTaskAffinityFallbackBlockedTransitions = @($sameTaskAffinityBlockTransitions)
    distinctHealthyAccountCountAfterBlock = $distinctHealthyAccountHashesAfterBlock.Count
    distinctHealthyAccountHashesAfterBlock = @($distinctHealthyAccountHashesAfterBlock)
    blockedAccountCount = $blockedAccountHashes.Count
    blockedAccountHashes = @($blockedAccountHashes)
    newRequestAvoidanceCount = $newRequestAvoidance.Count
    newRequestAvoidanceRequestIds = @($newRequestAvoidance | ForEach-Object { $_.requestId } | Sort-Object -Unique)
    newRequestBlockedReuseCount = $newRequestBlockedReuse.Count
    newRequestBlockedReuseRequestIds = @($newRequestBlockedReuse | ForEach-Object { $_.requestId } | Sort-Object -Unique)
    newRequestAvoidance = @($newRequestAvoidance)
    newRequestBlockedReuse = @($newRequestBlockedReuse)
    newRequestAvoidancePass = $newRequestAvoidancePass
    legacySameRequestFallbackPass = $currentPass
    pass = $sameTaskPass
  }
}

function Test-QuotaFallbackAudit {
  param([string]$Path)
  $result = New-SmokeResult "same_task_affinity_fallback_blocked"
  if (-not $RequireQuotaFallback) {
    Set-SmokeSkipped $result "未传入 -RequireQuotaFallback；不强制要求本次 run 出现同任务 hard-affinity 429 闭合"
    return $result
  }

  $evidence = Get-QuotaFallbackAuditEvidence $Path
  if (-not $evidence.auditExists) {
    Set-SmokeBlocked $result "缺少 audit 文件，无法证明额度耗尽后的同任务 hard-affinity 行为" $evidence
    return $result
  }

  if ($evidence.pass) {
    Set-SmokePass $result $evidence
    return $result
  }

  if ($evidence.sameTaskAffinityLocalCompletionCount -gt 0) {
    Set-SmokeFail $result "同任务 hard-affinity block 后被本地 pool_unavailable completed 闭合；这会让用户侧任务提前结束" $evidence
    return $result
  }

  if ($evidence.sameTaskAffinityUnrecoveredTerminal429Count -gt 0) {
    Set-SmokeFail $result "同任务 hard-affinity block 后仍以 final 429 结束" $evidence
    return $result
  }

  Set-SmokeBlocked $result "本次 run 未证明同任务额度耗尽后阻止切号并等到真实 upstream terminal completion" $evidence
  $result
}

function Test-NewRequestAvoidanceAudit {
  param([string]$Path)
  $result = New-SmokeResult "new_request_avoids_exhausted_account"
  if (-not $RequireQuotaFallback) {
    Set-SmokeSkipped $result "未传入 -RequireQuotaFallback；不强制要求观察后续新请求避开 exhausted/cooldown 账号"
    return $result
  }

  $evidence = Get-QuotaFallbackAuditEvidence $Path
  if (-not $evidence.auditExists) {
    Set-SmokeBlocked $result "缺少 audit 文件，无法证明后续新请求调度行为" $evidence
    return $result
  }

  if ($evidence.newRequestBlockedReuseCount -gt 0) {
    Set-SmokeFail $result "后续新请求仍命中过已 exhausted/cooldown 的账号" $evidence
    return $result
  }

  if ($evidence.newRequestAvoidancePass) {
    Set-SmokePass $result $evidence
    return $result
  }

  Set-SmokeBlocked $result "本次 run 未观察到后续新请求避开 exhausted/cooldown 账号" $evidence
  $result
}

function New-SmokeContinuitySummary {
  param($AuditEvidence, [object[]]$Results)
  $sameTask = Get-SmokeResultByName $Results "same_task_affinity_fallback_blocked"
  $newRequest = Get-SmokeResultByName $Results "new_request_avoids_exhausted_account"

  [ordered]@{
    sameTaskAffinityFallbackBlocked = [ordered]@{
      status = if ($sameTask) { [string]$sameTask.status } else { "missing" }
      reason = if ($sameTask) { $sameTask.reason } else { "same_task_affinity_fallback_blocked result missing" }
      evidence = [ordered]@{
        has429 = if ($AuditEvidence) { [bool]$AuditEvidence.has429 } else { $false }
        hasUsageLimitReached = if ($AuditEvidence) { [bool]$AuditEvidence.hasUsageLimitReached } else { $false }
        hasModelCooldownApplied = if ($AuditEvidence) { [bool]$AuditEvidence.hasModelCooldownApplied } else { $false }
        hasFallbackBlocked = if ($AuditEvidence) { [bool]$AuditEvidence.hasFallbackBlocked } else { $false }
        hasHardAffinityFallbackBlocked = if ($AuditEvidence) { [bool]$AuditEvidence.hasHardAffinityFallbackBlocked } else { $false }
        sameTaskAffinityFallbackBlockedCount = if ($AuditEvidence) { [int]$AuditEvidence.sameTaskAffinityFallbackBlockedCount } else { 0 }
        sameTaskAffinityLocalCompletionCount = if ($AuditEvidence) { [int]$AuditEvidence.sameTaskAffinityLocalCompletionCount } else { 0 }
        sameTaskAffinityTerminalCompletionCount = if ($AuditEvidence) { [int]$AuditEvidence.sameTaskAffinityTerminalCompletionCount } else { 0 }
        sameTaskAffinityUnrecoveredTerminal429Count = if ($AuditEvidence) { [int]$AuditEvidence.sameTaskAffinityUnrecoveredTerminal429Count } else { 0 }
        sameTaskAffinityFallbackBlockedRequestIds = if ($AuditEvidence) { @($AuditEvidence.sameTaskAffinityFallbackBlockedRequestIds) } else { @() }
        sameTaskAffinityTerminalCompletionRequestIds = if ($AuditEvidence) { @($AuditEvidence.sameTaskAffinityTerminalCompletionRequestIds) } else { @() }
        sameTaskAffinityUnrecoveredTerminal429RequestIds = if ($AuditEvidence) { @($AuditEvidence.sameTaskAffinityUnrecoveredTerminal429RequestIds) } else { @() }
      }
    }
    newRequestAvoidsExhaustedCooldown = [ordered]@{
      status = if ($newRequest) { [string]$newRequest.status } else { "missing" }
      reason = if ($newRequest) { $newRequest.reason } else { "new_request_avoids_exhausted_account result missing" }
      evidence = [ordered]@{
        blockedAccountCount = if ($AuditEvidence) { [int]$AuditEvidence.blockedAccountCount } else { 0 }
        newRequestAvoidanceCount = if ($AuditEvidence) { [int]$AuditEvidence.newRequestAvoidanceCount } else { 0 }
        newRequestAvoidanceRequestIds = if ($AuditEvidence) { @($AuditEvidence.newRequestAvoidanceRequestIds) } else { @() }
        newRequestBlockedReuseCount = if ($AuditEvidence) { [int]$AuditEvidence.newRequestBlockedReuseCount } else { 0 }
        newRequestBlockedReuseRequestIds = if ($AuditEvidence) { @($AuditEvidence.newRequestBlockedReuseRequestIds) } else { @() }
      }
    }
  }
}

function ConvertTo-RedactedText {
  param([string]$Text, [int]$MaxLength = 240)
  if (-not $Text) {
    return $null
  }
  $value = $Text
  $value = $value -replace 'agt_codex_[A-Za-z0-9]+', '[redacted-api-key]'
  $value = $value -replace 'sk-[A-Za-z0-9_-]+', '[redacted-api-key]'
  $value = $value -replace '[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}', '[redacted-email]'
  $value = $value -replace 'codex_[0-9a-fA-F]{32}', '[redacted-account-id]'
  if ($value.Length -gt $MaxLength) {
    return $value.Substring(0, $MaxLength) + "...[truncated]"
  }
  $value
}

function Get-SafeErrorSummary {
  param([string]$Body)
  if (-not $Body) {
    return $null
  }

  try {
    $json = $Body | ConvertFrom-Json
    $error = if ($json.error) { $json.error } else { $json }
    return [ordered]@{
      type = if ($error.type) { [string]$error.type } else { $null }
      code = if ($error.code) { [string]$error.code } else { $null }
      param = if ($error.param) { [string]$error.param } else { $null }
      message = ConvertTo-RedactedText ([string]$error.message)
    }
  } catch {
    return [ordered]@{
      type = $null
      code = $null
      param = $null
      message = ConvertTo-RedactedText $Body
    }
  }
}

function Test-LocalAccessConfigContract {
  param($Config, [string]$Stage)
  $result = New-SmokeResult "config_${Stage}_contract"
  if ($null -eq $Config) {
    Set-SmokeBlocked $result "codex_local_access.json 不存在"
    return $result
  }

  $accountCount = if ($Config.accountIds) { @($Config.accountIds).Count } else { 0 }
  $safety = $Config.safetyConfig
  $effectiveMaxRetryAccounts = [Math]::Max([int]$safety.maxRetryAccounts, 2)
  $evidence = @{
    enabled = [bool]$Config.enabled
    port = [int]$Config.port
    accountCount = $accountCount
    accountHashes = @(Get-AccountHashList @($Config.accountIds))
    hardenedLocalMode = [bool]$safety.hardenedLocalMode
    maxConcurrentRequests = [int]$safety.maxConcurrentRequests
    minRequestIntervalSeconds = [int]$safety.minRequestIntervalSeconds
    maxRetryAccounts = [int]$safety.maxRetryAccounts
    effectiveMaxRetryAccounts = [int]$effectiveMaxRetryAccounts
    fallbackMode = [string]$safety.fallbackMode
  }

  if (-not $safety.hardenedLocalMode -or $safety.maxConcurrentRequests -ne 1 -or $effectiveMaxRetryAccounts -lt 2) {
    if ($Stage -ne "fallback_probe" -or $effectiveMaxRetryAccounts -lt 2) {
      Set-SmokeFail $result "安全配置不是当前阶段允许的 hardened 合同" $evidence
      return $result
    }
  }

  if ($Stage -eq "single") {
    if ($accountCount -ne 1) {
      Set-SmokeBlocked $result "single 阶段要求 accountIds 恰好为 1" $evidence
      return $result
    }
    if ($effectiveMaxRetryAccounts -lt 2) {
      Set-SmokeBlocked $result "single 阶段要求有效 maxRetryAccounts >= 2；单账号池仍只会尝试 1 个账号" $evidence
      return $result
    }
  } elseif ($Stage -eq "small_pool") {
    if ($accountCount -lt 2 -or $accountCount -gt 3) {
      Set-SmokeBlocked $result "small_pool 阶段要求 accountIds 为 2 到 3 个" $evidence
      return $result
    }
    if ($effectiveMaxRetryAccounts -lt 2) {
      Set-SmokeBlocked $result "small_pool 阶段要求有效 maxRetryAccounts >= 2，以覆盖新 admission 的账号尝试上限" $evidence
      return $result
    }
  } elseif ($Stage -eq "fallback_probe") {
    if ($accountCount -lt 1) {
      Set-SmokeBlocked $result "fallback_probe 阶段要求 accountIds 至少 1 个" $evidence
      return $result
    }
    if ($effectiveMaxRetryAccounts -lt 2) {
      Set-SmokeBlocked $result "fallback_probe 阶段要求有效 maxRetryAccounts >= 2；fallbackMode 不阻断当前请求安全 failover" $evidence
      return $result
    }
    if (-not $RunUpstreamSmoke -and -not $RunCodexExecSmoke) {
      Set-SmokeBlocked $result "fallback_probe 需要显式 -RunUpstreamSmoke 或 -RunCodexExecSmoke 才能验证真实上游 fallback 边界" $evidence
      return $result
    }
  } else {
    Set-SmokeFail $result "未知 smoke stage: $Stage" $evidence
    return $result
  }

  Set-SmokePass $result $evidence
  $result
}

function Test-LoopbackAndModels {
  param([string]$ResolvedBaseUrl, [string]$ResolvedApiKey)
  $result = New-SmokeResult "loopback_models_endpoint"
  if (-not $ResolvedBaseUrl -or -not $ResolvedApiKey) {
    Set-SmokeBlocked $result "缺少 BaseUrl 或 API key"
    return $result
  }

  $uri = "$ResolvedBaseUrl/models"
  try {
    $response = Invoke-JsonRequest -Method "GET" -Uri $uri -Headers @{ Authorization = "Bearer $ResolvedApiKey" }
  } catch {
    Set-SmokeBlocked $result "无法连接 Cockpit API service" @{ baseUrl = $ResolvedBaseUrl; error = $_.Exception.Message }
    return $result
  }

  $evidence = @{
    uri = $uri
    statusCode = $response.statusCode
    contentType = $response.contentType
  }
  if ($response.statusCode -ne 200) {
    Set-SmokeFail $result "/v1/models 未返回 200" $evidence
    return $result
  }

  Set-SmokePass $result $evidence
  $result
}

function Test-InvalidKeyAuth {
  param([string]$ResolvedBaseUrl)
  $result = New-SmokeResult "invalid_key_auth_guard"
  if (-not $ResolvedBaseUrl) {
    Set-SmokeBlocked $result "缺少 BaseUrl"
    return $result
  }

  $uri = "$ResolvedBaseUrl/models"
  try {
    $response = Invoke-JsonRequest -Method "GET" -Uri $uri -Headers @{ Authorization = "Bearer invalid-local-smoke-key" }
  } catch {
    Set-SmokeBlocked $result "无法连接 Cockpit API service" @{ baseUrl = $ResolvedBaseUrl; error = $_.Exception.Message }
    return $result
  }

  $evidence = @{
    uri = $uri
    statusCode = $response.statusCode
    contentType = $response.contentType
  }
  if ($response.statusCode -ne 401) {
    Set-SmokeFail $result "错误 API key 未被 401 拦截" $evidence
    return $result
  }

  Set-SmokePass $result $evidence
  $result
}

function Invoke-UpstreamChatSmoke {
  param([string]$ResolvedBaseUrl, [string]$ResolvedApiKey, [string]$Model)
  $result = New-SmokeResult "single_account_upstream_chat"
  if ($AutoDrainFirstFreeAccountUntilFallback) {
    Set-SmokeSkipped $result "已启用 -AutoDrainFirstFreeAccountUntilFallback；由 quota_drain_until_hard_affinity_block 执行真实上游请求"
    return $result
  }
  if (-not $RunUpstreamSmoke) {
    Set-SmokeSkipped $result "未传入 -RunUpstreamSmoke；默认不消耗真实上游额度"
    return $result
  }
  if (-not $ResolvedBaseUrl -or -not $ResolvedApiKey) {
    Set-SmokeBlocked $result "缺少 BaseUrl 或 API key"
    return $result
  }

  $uri = "$ResolvedBaseUrl/chat/completions"
  $body = @{
    model = $Model
    stream = $false
    messages = @(
      @{ role = "user"; content = "Reply with exactly OK." }
    )
  }
  try {
    $response = Invoke-JsonRequest -Method "POST" -Uri $uri -Headers @{ Authorization = "Bearer $ResolvedApiKey" } -Body $body -TimeoutSeconds 120
  } catch {
    Set-SmokeFail $result "真实上游请求异常" @{ baseUrl = $ResolvedBaseUrl; error = $_.Exception.Message }
    return $result
  }

  $evidence = @{
    uri = $uri
    statusCode = $response.statusCode
    retryAfter = $response.retryAfter
    model = $Model
    bodyHasOK = ([string]$response.body -match '"OK"|OK')
  }
  $safeError = Get-SafeErrorSummary $response.body
  if ($safeError) {
    $evidence.error = $safeError
  }

  if ($response.statusCode -eq 200) {
    Set-SmokePass $result $evidence
    return $result
  }

  if ($Expect429 -and $response.statusCode -eq 429) {
    Set-SmokePass $result $evidence
    return $result
  }

  Set-SmokeFail $result "真实上游请求未按预期返回" $evidence
  $result
}

function Invoke-QuotaDrainUntilFallback {
  param([string]$ResolvedBaseUrl, [string]$ResolvedApiKey, [string]$Model, [string]$AuditPath)
  $result = New-SmokeResult "quota_drain_until_hard_affinity_block"
  if (-not $AutoDrainFirstFreeAccountUntilFallback) {
    Set-SmokeSkipped $result "未传入 -AutoDrainFirstFreeAccountUntilFallback；不主动消耗第一个 free 账号"
    return $result
  }
  if (-not $ResolvedBaseUrl -or -not $ResolvedApiKey) {
    Set-SmokeBlocked $result "缺少 BaseUrl 或 API key"
    return $result
  }

  $uri = "$ResolvedBaseUrl/chat/completions"
  $attempts = @()
  for ($i = 1; $i -le $AutoDrainMaxRequests; $i++) {
    $body = @{
      model = $Model
      stream = $false
      messages = @(
        @{ role = "user"; content = "Reply with exactly OK." }
      )
    }
    try {
      $response = Invoke-JsonRequest -Method "POST" -Uri $uri -Headers @{ Authorization = "Bearer $ResolvedApiKey" } -Body $body -TimeoutSeconds 120
      $safeError = Get-SafeErrorSummary $response.body
      $attempt = [ordered]@{
        attempt = $i
        statusCode = $response.statusCode
        retryAfter = $response.retryAfter
        bodyHasOK = ([string]$response.body -match '"OK"|OK')
      }
      if ($safeError) {
        $attempt.error = $safeError
      }
      $attempts += $attempt
    } catch {
      Set-SmokeFail $result "消耗型上游请求异常" @{
        uri = $uri
        attempt = $i
        maxRequests = $AutoDrainMaxRequests
        error = $_.Exception.Message
        attempts = $attempts
      }
      return $result
    }

    $auditEvidence = Get-QuotaFallbackAuditEvidence $AuditPath
    if ($auditEvidence.pass) {
      Set-SmokePass $result @{
        uri = $uri
        model = $Model
        requestCount = $i
        maxRequests = $AutoDrainMaxRequests
        requestIntervalSeconds = $AutoDrainRequestIntervalSeconds
        attempts = $attempts
        audit = $auditEvidence
      }
      return $result
    }

    if ($i -lt $AutoDrainMaxRequests -and $AutoDrainRequestIntervalSeconds -gt 0) {
      Start-Sleep -Seconds $AutoDrainRequestIntervalSeconds
    }
  }

  $finalAuditEvidence = Get-QuotaFallbackAuditEvidence $AuditPath
  Set-SmokeBlocked $result "第一个 free 账号在最大消耗请求数内未触发同任务 hard-affinity 429 闭合；为避免过度请求已停止" @{
    uri = $uri
    model = $Model
    requestCount = $AutoDrainMaxRequests
    maxRequests = $AutoDrainMaxRequests
    requestIntervalSeconds = $AutoDrainRequestIntervalSeconds
    attempts = $attempts
    audit = $finalAuditEvidence
  }
  $result
}

function Invoke-CodexExecSmoke {
  param([string]$ResolvedBaseUrl, [string]$ResolvedApiKey, [string]$Model)
  $result = New-SmokeResult "codex_exec_task_e2e"
  if (-not $RunCodexExecSmoke) {
    Set-SmokeSkipped $result "未传入 -RunCodexExecSmoke；默认不启动额外 Codex 任务进程"
    return $result
  }
  if (-not $ResolvedBaseUrl -or -not $ResolvedApiKey) {
    Set-SmokeBlocked $result "缺少 BaseUrl 或 API key"
    return $result
  }

  $tempDir = Join-Path ([System.IO.Path]::GetTempPath()) ("cockpit-codex-exec-e2e-{0}-{1}" -f $PID, (Get-Date -Format "yyyyMMddHHmmssfff"))
  $codexHome = Join-Path $tempDir "codex-home"
  $workspace = Join-Path $tempDir "workspace"
  New-Item -ItemType Directory -Force -Path $codexHome, $workspace | Out-Null
  $configPath = Join-Path $codexHome "config.toml"
  $stdoutPath = Join-Path $tempDir "codex-events.jsonl"
  $stderrPath = Join-Path $tempDir "codex-stderr.log"
  $finalPath = Join-Path $tempDir "codex-final.txt"
  $taskFile = Join-Path $workspace "cockpit-e2e-result.txt"
  $providerId = "cockpit_exec_e2e"
  $marker = "COCKPIT_API_SERVICE_E2E_OK"

  @"
model = "$Model"
model_provider = "$providerId"
approval_policy = "never"
sandbox_mode = "danger-full-access"

[model_providers.$providerId]
name = "Cockpit API Service E2E"
base_url = "$ResolvedBaseUrl"
wire_api = "responses"
env_key = "OPENAI_API_KEY"
"@ | Set-Content -LiteralPath $configPath -Encoding UTF8

  $prompt = @"
This is a Cockpit API service continuity probe running in an isolated temporary workspace.
Create a file named cockpit-e2e-result.txt in the current workspace.
The file content must be exactly:
$marker
After writing the file, reply with exactly:
CODEX_TASK_DONE
"@

  $oldCodexHome = [Environment]::GetEnvironmentVariable("CODEX_HOME", "Process")
  $oldOpenAiKey = [Environment]::GetEnvironmentVariable("OPENAI_API_KEY", "Process")
  [Environment]::SetEnvironmentVariable("CODEX_HOME", $codexHome, "Process")
  [Environment]::SetEnvironmentVariable("OPENAI_API_KEY", $ResolvedApiKey, "Process")
  try {
    $args = @(
      "exec",
      "--ephemeral",
      "--skip-git-repo-check",
      "--ignore-rules",
      "--dangerously-bypass-approvals-and-sandbox",
      "--json",
      "-m",
      $Model,
      "-C",
      $workspace,
      "--output-last-message",
      $finalPath,
      $prompt
    )
    & codex @args 1> $stdoutPath 2> $stderrPath
    $exitCode = $LASTEXITCODE
  } catch {
    $exitCode = 1
    $_.Exception.Message | Set-Content -LiteralPath $stderrPath -Encoding UTF8
  } finally {
    [Environment]::SetEnvironmentVariable("CODEX_HOME", $oldCodexHome, "Process")
    [Environment]::SetEnvironmentVariable("OPENAI_API_KEY", $oldOpenAiKey, "Process")
  }

  $finalMessage = if (Test-Path -LiteralPath $finalPath) {
    ConvertTo-RedactedText (Get-Content -LiteralPath $finalPath -Raw) 240
  } else {
    $null
  }
  $taskContent = if (Test-Path -LiteralPath $taskFile) {
    Get-Content -LiteralPath $taskFile -Raw
  } else {
    $null
  }
  $stderrPreview = if (Test-Path -LiteralPath $stderrPath) {
    ConvertTo-RedactedText (Get-Content -LiteralPath $stderrPath -Raw) 360
  } else {
    $null
  }

  $evidence = @{
    exitCode = $exitCode
    model = $Model
    providerId = $providerId
    baseUrl = $ResolvedBaseUrl
    tempDir = $tempDir
    codexHome = $codexHome
    workspace = $workspace
    stdoutPath = $stdoutPath
    stderrPath = $stderrPath
    finalMessagePath = $finalPath
    finalMessage = $finalMessage
    taskFile = $taskFile
    taskFileExists = (Test-Path -LiteralPath $taskFile)
    taskFileHasMarker = ($taskContent -and $taskContent.Trim() -eq $marker)
    sandboxMode = "danger-full-access"
    sandboxBypassForIsolatedWorkspace = $true
    sandboxBypassReason = "nested codex exec smoke runs only in a temp CODEX_HOME and temp workspace; workspace-write is read-only in codex-cli 0.131 nested probes"
  }

  if ($stderrPreview) {
    $evidence.stderrPreview = $stderrPreview
  }

  if ($exitCode -eq 0 -and $evidence.taskFileHasMarker) {
    Set-SmokePass $result $evidence
    return $result
  }

  Set-SmokeFail $result "codex exec 任务未完成或未写入预期文件" $evidence
  $result
}

function Get-EphemeralGatewayExePath {
  Join-Path (Get-Location) "target\debug\codex-local-access-gateway.exe"
}

function Get-NewestEphemeralGatewaySource {
  $roots = @(
    (Join-Path (Get-Location) "src-tauri\src"),
    (Join-Path (Get-Location) "crates")
  )
  $items = @()
  foreach ($root in $roots) {
    if (Test-Path -LiteralPath $root) {
      $items += @(Get-ChildItem -LiteralPath $root -Recurse -File -Include *.rs,*.toml -ErrorAction SilentlyContinue)
    }
  }
  foreach ($path in @(
      (Join-Path (Get-Location) "src-tauri\Cargo.toml"),
      (Join-Path (Get-Location) "src-tauri\Cargo.lock"),
      (Join-Path (Get-Location) "Cargo.toml"),
      (Join-Path (Get-Location) "Cargo.lock")
    )) {
    if (Test-Path -LiteralPath $path) {
      $items += Get-Item -LiteralPath $path
    }
  }

  @($items | Sort-Object LastWriteTimeUtc -Descending | Select-Object -First 1)
}

function Test-EphemeralGatewayExeFresh {
  param([string]$Exe)
  if (-not (Test-Path -LiteralPath $Exe)) {
    return $false
  }
  $newestSource = Get-NewestEphemeralGatewaySource
  if (-not $newestSource) {
    return $true
  }
  $exeItem = Get-Item -LiteralPath $Exe
  $exeItem.LastWriteTimeUtc -ge $newestSource.LastWriteTimeUtc
}

function Build-EphemeralGateway {
  $exe = Get-EphemeralGatewayExePath
  if ($SkipEphemeralGatewayBuild -and (Test-EphemeralGatewayExeFresh $exe)) {
    return $exe
  }
  if ($SkipEphemeralGatewayBuild -and (Test-Path -LiteralPath $exe)) {
    $newestSource = Get-NewestEphemeralGatewaySource
    $sourceInfo = if ($newestSource) {
      "{0} ({1})" -f $newestSource.FullName, $newestSource.LastWriteTime.ToString("o")
    } else {
      "unknown"
    }
    Write-Warning ("-SkipEphemeralGatewayBuild ignored because codex-local-access-gateway.exe is older than source; newest_source={0}; exe_time={1}" -f $sourceInfo, (Get-Item -LiteralPath $exe).LastWriteTime.ToString("o"))
  }

  $manifest = Join-Path (Get-Location) "src-tauri\Cargo.toml"
  $targetDir = Join-Path (Get-Location) "target"
  & cargo build --manifest-path $manifest --target-dir $targetDir --bin codex-local-access-gateway
  if ($LASTEXITCODE -ne 0) {
    throw "构建 ephemeral gateway runner 失败，exit_code=$LASTEXITCODE"
  }
  if (-not (Test-Path -LiteralPath $exe)) {
    throw "ephemeral gateway runner 构建后不存在: $exe"
  }
  $exe
}

function Wait-EphemeralGatewayReady {
  param(
    [System.Diagnostics.Process]$Process,
    [string]$StdoutPath,
    [string]$StderrPath,
    [int]$TimeoutSeconds
  )

  $deadline = (Get-Date).AddSeconds($TimeoutSeconds)
  $lastError = $null
  while ((Get-Date) -lt $deadline) {
    if ($Process.HasExited) {
      $stdout = if (Test-Path -LiteralPath $StdoutPath) { Get-Content -LiteralPath $StdoutPath -Raw } else { "" }
      $stderr = if (Test-Path -LiteralPath $StderrPath) { Get-Content -LiteralPath $StderrPath -Raw } else { "" }
      throw "ephemeral gateway runner 提前退出，exit_code=$($Process.ExitCode), stdout=$stdout, stderr=$stderr"
    }

    $config = Get-LocalAccessConfig
    $baseUrl = Resolve-BaseUrl $config
    $apiKey = Resolve-ApiKey $config
    if ($baseUrl -and $apiKey) {
      try {
        $response = Invoke-JsonRequest -Method "GET" -Uri "$baseUrl/models" -Headers @{ Authorization = "Bearer $apiKey" } -TimeoutSeconds 2
        if ($response.statusCode -eq 200) {
          return [ordered]@{
            baseUrl = $baseUrl
            statusCode = $response.statusCode
          }
        }
        $lastError = "statusCode=$($response.statusCode)"
      } catch {
        $lastError = $_.Exception.Message
      }
    }

    Start-Sleep -Milliseconds 500
  }

  throw "ephemeral gateway runner 未在 $TimeoutSeconds 秒内就绪: $lastError"
}

function Start-EphemeralGateway {
  $configPath = Get-LocalAccessConfigPath
  $tempDir = Join-Path ([System.IO.Path]::GetTempPath()) ("cockpit-hla-smoke-{0}-{1}" -f $PID, (Get-Date -Format "yyyyMMddHHmmssfff"))
  New-Item -ItemType Directory -Force -Path $tempDir | Out-Null
  $backupPath = Join-Path $tempDir "codex_local_access.original.json"
  $originalMissing = -not (Test-Path -LiteralPath $configPath)
  if (-not $originalMissing) {
    Copy-Item -LiteralPath $configPath -Destination $backupPath -Force
  }

  $temporaryFallbackApplied = $null
  if ($TemporaryFallbackConfig) {
    $temporaryFallbackApplied = Set-TemporaryFallbackProbeConfig
  }

  $exe = Build-EphemeralGateway
  $stdoutPath = Join-Path $tempDir "gateway.stdout.log"
  $stderrPath = Join-Path $tempDir "gateway.stderr.log"
  $previousDataRootEnv = [Environment]::GetEnvironmentVariable("COCKPIT_LOCAL_ACCESS_DATA_ROOT", "Process")
  [Environment]::SetEnvironmentVariable("COCKPIT_LOCAL_ACCESS_DATA_ROOT", (Get-DataRoot), "Process")
  try {
    $process = Start-Process -FilePath $exe -ArgumentList @("--serve") -WindowStyle Hidden -PassThru -RedirectStandardOutput $stdoutPath -RedirectStandardError $stderrPath
  } finally {
    [Environment]::SetEnvironmentVariable("COCKPIT_LOCAL_ACCESS_DATA_ROOT", $previousDataRootEnv, "Process")
  }
  $ready = Wait-EphemeralGatewayReady -Process $process -StdoutPath $stdoutPath -StderrPath $stderrPath -TimeoutSeconds $EphemeralGatewayReadyTimeoutSeconds

  [ordered]@{
    requested = $true
    status = "running"
    processId = $process.Id
    exe = $exe
    tempDir = $tempDir
    stdoutPath = $stdoutPath
    stderrPath = $stderrPath
    backupPath = if ($originalMissing) { $null } else { $backupPath }
    originalMissing = $originalMissing
    dataRoot = Get-DataRoot
    temporaryFallbackConfig = if ($temporaryFallbackApplied) { $temporaryFallbackApplied } else { [ordered]@{ requested = [bool]$TemporaryFallbackConfig; status = "not_applied" } }
    ready = $ready
    stopped = $false
    restoredConfig = $false
    removedConfigBackup = $false
  }
}

function Stop-EphemeralGateway {
  param([System.Collections.IDictionary]$Gateway)
  if ($null -eq $Gateway -or -not $Gateway.requested) {
    return
  }

  if ($Gateway.processId) {
    $process = Get-Process -Id $Gateway.processId -ErrorAction SilentlyContinue
    if ($process) {
      Stop-Process -Id $Gateway.processId -Force
      $Gateway.stopped = $true
    }
  }

  $configPath = Get-LocalAccessConfigPath
  if ($Gateway.originalMissing) {
    if (Test-Path -LiteralPath $configPath) {
      Remove-Item -LiteralPath $configPath -Force
    }
    $Gateway.restoredConfig = $true
  } elseif ($Gateway.backupPath -and (Test-Path -LiteralPath $Gateway.backupPath)) {
    Copy-Item -LiteralPath $Gateway.backupPath -Destination $configPath -Force
    $Gateway.restoredConfig = $true
    Remove-Item -LiteralPath $Gateway.backupPath -Force
    $Gateway.removedConfigBackup = $true
    $Gateway.backupPath = $null
  }
  $Gateway.status = "stopped"
}

$ephemeralGateway = [ordered]@{
  requested = [bool]$StartEphemeralGateway
  status = if ($StartEphemeralGateway) { "pending" } else { "not_requested" }
}
$temporaryFallbackProbe = [ordered]@{
  requested = [bool]$TemporaryFallbackConfig
  status = if ($TemporaryFallbackConfig) { "pending" } else { "not_requested" }
}
$results = @()
$healthSummary = $null
$auditSummary = $null
$resolvedBaseUrl = $null
$codexAppGuardBefore = $null
$codexAppGuardAfter = $null
$codexAppGuardComparison = $null
$codexCliGuardAfter = $null
$codexCliGuardComparison = $null

if ($AppSafeIsolatedProbe -and -not $StartEphemeralGateway) {
  throw "-AppSafeIsolatedProbe 只能与 -StartEphemeralGateway 一起使用"
}

if ($TemporaryFallbackConfig -and -not $StartEphemeralGateway) {
  throw "-TemporaryFallbackConfig 只能与 -StartEphemeralGateway 一起使用，避免修改 live Cockpit/Codex provider 配置"
}

if ($TemporaryFallbackConfig -and $Stage -ne "fallback_probe") {
  throw "-TemporaryFallbackConfig 只适用于 -Stage fallback_probe"
}

if ($AutoDrainFirstFreeAccountUntilFallback -and -not $RequireQuotaFallback) {
  throw "-AutoDrainFirstFreeAccountUntilFallback 需要同时传入 -RequireQuotaFallback"
}

if ($AppSafeIsolatedProbe) {
  $script:AppSafeProbe = Initialize-AppSafeIsolatedProbeRoot
}

$dataRoot = Get-DataRoot
$codexCliGuardBefore = Get-CodexCliGuardState $CodexHome
if ($AssertCodexAppProcessStable) {
  $codexAppGuardBefore = Get-CodexAppProcessGuardState $CodexAppProcessNames
}

try {
  if ($StartEphemeralGateway) {
    $ephemeralGateway = Start-EphemeralGateway
    if ($ephemeralGateway.temporaryFallbackConfig) {
      $temporaryFallbackProbe = $ephemeralGateway.temporaryFallbackConfig
    }
  }

  $config = Get-LocalAccessConfig
  $resolvedBaseUrl = Resolve-BaseUrl $config
  $resolvedApiKey = Resolve-ApiKey $config
  $healthPath = Join-Path $dataRoot "codex_local_access_health.json"
  $auditPath = Join-Path $dataRoot "codex_local_access_audit.jsonl"

  $results += Test-LocalAccessConfigContract $config $Stage
  $results += Test-LoopbackAndModels $resolvedBaseUrl $resolvedApiKey
  $results += Test-InvalidKeyAuth $resolvedBaseUrl
  $results += Invoke-UpstreamChatSmoke $resolvedBaseUrl $resolvedApiKey $Model
  $results += Invoke-QuotaDrainUntilFallback $resolvedBaseUrl $resolvedApiKey $Model $auditPath
  $results += Invoke-CodexExecSmoke $resolvedBaseUrl $resolvedApiKey $Model

  $healthRegistrySummary = Get-JsonFileSummary $healthPath
  $healthSummary = Get-ScopedHealthFileSummary $config $healthPath
  $auditSummary = Get-AuditTailSummary $auditPath
  $results += Test-QuotaFallbackAudit $auditPath
  $results += Test-NewRequestAvoidanceAudit $auditPath
} finally {
  if ($StartEphemeralGateway) {
    Stop-EphemeralGateway $ephemeralGateway
  }
}

$codexCliGuardAfter = Get-CodexCliGuardState $CodexHome
$codexCliGuardComparison = Compare-CodexCliGuardState $codexCliGuardBefore $codexCliGuardAfter
if ($AssertCodexCliConfigUntouched) {
  $results += Test-CodexCliGuardUnchanged $codexCliGuardComparison
}
if ($AssertCodexAppProcessStable) {
  $codexAppGuardAfter = Get-CodexAppProcessGuardState $CodexAppProcessNames
  $codexAppGuardComparison = Compare-CodexAppProcessGuardState $codexAppGuardBefore $codexAppGuardAfter
  $results += Test-CodexAppProcessStable $codexAppGuardComparison
}

$overall = if ($results | Where-Object { $_.status -eq "fail" }) {
  "fail"
} elseif ($results | Where-Object { $_.status -eq "blocked" }) {
  "blocked"
} else {
  "pass"
}
$quotaAuditEvidence = if ($auditPath) { Get-QuotaFallbackAuditEvidence $auditPath } else { $null }

$report = [ordered]@{
  schemaVersion = 1
  generatedAt = (Get-Date).ToString("o")
  overall = $overall
  mode = if ($RunCodexExecSmoke) { "codex_exec_smoke" } elseif ($RunUpstreamSmoke) { "upstream_smoke" } else { "preflight" }
  stage = $Stage
  baseUrl = $resolvedBaseUrl
  dataRoot = $dataRoot
  runUpstreamSmoke = [bool]$RunUpstreamSmoke
  runCodexExecSmoke = [bool]$RunCodexExecSmoke
  requireQuotaFallback = [bool]$RequireQuotaFallback
  autoDrainFirstFreeAccountUntilFallback = [bool]$AutoDrainFirstFreeAccountUntilFallback
  autoDrainMaxRequests = $AutoDrainMaxRequests
  autoDrainRequestIntervalSeconds = $AutoDrainRequestIntervalSeconds
  expect429 = [bool]$Expect429
  ephemeralGateway = $ephemeralGateway
  appSafeProbe = $script:AppSafeProbe
  temporaryFallbackConfig = $temporaryFallbackProbe
  codexCliGuard = [ordered]@{
    before = $codexCliGuardBefore
    after = $codexCliGuardAfter
    comparison = $codexCliGuardComparison
    asserted = [bool]$AssertCodexCliConfigUntouched
  }
  codexAppGuard = [ordered]@{
    before = $codexAppGuardBefore
    after = $codexAppGuardAfter
    comparison = $codexAppGuardComparison
    asserted = [bool]$AssertCodexAppProcessStable
  }
  routing = New-SmokeRoutingReport $config $results $auditSummary
  poolUnavailable = New-SmokePoolUnavailableReport $healthSummary
  continuitySummary = New-SmokeContinuitySummary $quotaAuditEvidence $results
  results = $results
  health = $healthSummary
  healthRegistry = $healthRegistrySummary
  audit = $auditSummary
  safetyNotes = @(
    "report redacts API key and account identity",
    "default mode does not call /v1/chat/completions",
    "staged rollout: single -> small_pool -> fallback_probe",
    "use -RunUpstreamSmoke only after API service is enabled and the current stage contract passes",
    "use -RunCodexExecSmoke for a task-level E2E probe through an isolated CODEX_HOME",
    "use -RequireQuotaFallback when the acceptance target is real quota-exhaustion continuity; the report blocks unless audit shows 429, model cooldown, hard-affinity fallback_blocked, and real upstream terminal completion; same-task local completed Responses closure is a failure",
    "use -StartEphemeralGateway to exercise the same gateway code path without switching live Codex provider",
    "use -TemporaryFallbackConfig only with -StartEphemeralGateway; the original codex_local_access.json is restored afterward",
    "use -AppSafeIsolatedProbe to keep probe local access config, health, audit, and port allocation in an isolated temp data root; the probe copies the existing API service account pool and does not auto-populate accounts",
    "use -AutoDrainFirstFreeAccountUntilFallback only for explicit quota-drain acceptance; it sends bounded low-rate real requests until audit proves same-task hard-affinity closure or max requests is reached",
    "this script records hashes for ~/.codex/config.toml and ~/.codex/auth.json but does not read or write their contents"
  )
}

if ($WriteReport) {
  $reportDir = Join-Path (Get-Location) "reports\local-hardened-api-smoke"
  New-Item -ItemType Directory -Force -Path $reportDir | Out-Null
  $stamp = Get-Date -Format "yyyyMMdd-HHmmss"
  $reportPath = Join-Path $reportDir "smoke-$stamp.json"
  $report | ConvertTo-Json -Depth 10 | Set-Content -LiteralPath $reportPath -Encoding UTF8
  $report.reportPath = $reportPath
}

$report | ConvertTo-Json -Depth 10
