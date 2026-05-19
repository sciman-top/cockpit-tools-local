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
  [switch]$AutoPopulateProbeAccountPool,
  [ValidateRange(2, 3)]
  [int]$AutoPopulateProbeAccountCount = 2,
  [ValidateRange(2, 20)]
  [int]$AutoPopulateProbeMaxRefreshAttempts = 2,
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
  autoPopulateProbeAccountPool = [bool]$AutoPopulateProbeAccountPool
  autoDrainFirstFreeAccountUntilFallback = [bool]$AutoDrainFirstFreeAccountUntilFallback
}
$liveUpstreamRiskRequested = [bool](@($liveUpstreamRiskRequests.GetEnumerator() | Where-Object { $_.Value }).Count)
$expandedLiveUpstreamRiskReasons = @()
if ($AutoPopulateProbeAccountPool -and $AutoPopulateProbeMaxRefreshAttempts -gt 2) {
  $expandedLiveUpstreamRiskReasons += "auto_populate_refresh_attempts_gt_2"
}
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
    maxRefreshAttempts = $AutoPopulateProbeMaxRefreshAttempts
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
    maxRefreshAttempts = $AutoPopulateProbeMaxRefreshAttempts
    drainMaxRequests = $AutoDrainMaxRequests
    drainRequestIntervalSeconds = $AutoDrainRequestIntervalSeconds
  } | ConvertTo-Json -Depth 8
  exit 2
}

. (Join-Path $PSScriptRoot "lib\local-hardened-api-probe-account-pool.ps1")
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

function Resolve-ProbeOAuthAccountPool {
  param(
    [object[]]$ExistingAccountIds,
    [int]$RequiredCount
  )

  Resolve-CockpitProbeOAuthAccountPool `
    -ExistingAccountIds $ExistingAccountIds `
    -RequiredCount $RequiredCount `
    -DataRoot (Get-LiveDataRoot) `
    -RefreshQuotaScript ${function:Invoke-CodexWhamUsageForProbeSelection} `
    -MaxRefreshAttempts $AutoPopulateProbeMaxRefreshAttempts `
    -AllowFirstAccountDrain:$AutoDrainFirstFreeAccountUntilFallback
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
    $items += @(Get-Process -Name $name -ErrorAction SilentlyContinue | ForEach-Object {
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
  $autoPopulatePool = [ordered]@{
    requested = [bool]$AutoPopulateProbeAccountPool
    status = if ($AutoPopulateProbeAccountPool) { "pending" } else { "not_requested" }
  }
  if ($AutoPopulateProbeAccountPool) {
    $autoPopulatePool = Resolve-ProbeOAuthAccountPool `
      -ExistingAccountIds @($config.accountIds) `
      -RequiredCount $AutoPopulateProbeAccountCount
    Set-JsonProperty $config "accountIds" @($autoPopulatePool.accountIds)
    if ($autoPopulatePool.Contains("accountIds")) {
      $autoPopulatePool.Remove("accountIds")
    }
  }

  $accountCount = if ($config.accountIds) { @($config.accountIds).Count } else { 0 }
  if ($accountCount -lt 2) {
    throw "fallback_probe 需要至少 2 个 accountIds，当前 accountCount=$accountCount；App-safe 验收可加 -AutoPopulateProbeAccountPool 从现有 OAuth 账号仓库自动补齐临时号池"
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
  Set-JsonProperty $safety "fallbackMode" "next_request_only"
  Set-JsonProperty $safety.logging "redactSensitiveValues" $true
  Set-JsonProperty $safety.logging "includePromptResponse" $false
  Set-JsonProperty $safety.logging "includeRawUpstreamBody" $false

  $config | ConvertTo-Json -Depth 20 | Set-Content -LiteralPath $path -Encoding UTF8

  [ordered]@{
    requested = $true
    status = "applied"
    path = $path
    accountCount = $accountCount
    autoPopulateProbeAccountPool = $autoPopulatePool
    enabled = $true
    maxRetryAccounts = 2
    fallbackMode = "next_request_only"
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

function Invoke-CodexWhamUsageForProbeSelection {
  param([object]$Account)

  $tokens = $Account.tokens
  $accessToken = if ($tokens -and $tokens.access_token) { ([string]$tokens.access_token).Trim() } else { "" }
  if (-not $accessToken) {
    return [ordered]@{
      status = "error"
      source = "wham_usage_refresh"
      error = "access_token_missing"
    }
  }

  $headers = @{
    Authorization = "Bearer $accessToken"
    Accept = "application/json"
  }
  $chatgptAccountId = if ($Account.account_id) { ([string]$Account.account_id).Trim() } else { "" }
  if ($chatgptAccountId) {
    $headers["ChatGPT-Account-Id"] = $chatgptAccountId
  }

  $uri = "https://chatgpt.com/backend-api/wham/usage"
  try {
    $response = Invoke-WebRequest -Method "GET" -Uri $uri -Headers $headers -TimeoutSec 30 -SkipHttpErrorCheck
  } catch {
    return [ordered]@{
      status = "error"
      source = "wham_usage_refresh"
      error = $_.Exception.Message
    }
  }

  $statusCode = [int]$response.StatusCode
  $body = [string]$response.Content
  if ($statusCode -lt 200 -or $statusCode -ge 300) {
    $detailCode = $null
    try {
      $errorJson = $body | ConvertFrom-Json
      if ($errorJson.detail -and $errorJson.detail.code) {
        $detailCode = [string]$errorJson.detail.code
      } elseif ($errorJson.error -and $errorJson.error.code) {
        $detailCode = [string]$errorJson.error.code
      }
    } catch {
    }
    return [ordered]@{
      status = "error"
      source = "wham_usage_refresh"
      statusCode = $statusCode
      detailCode = $detailCode
      bodyLength = $body.Length
    }
  }

  try {
    $usage = $body | ConvertFrom-Json
  } catch {
    return [ordered]@{
      status = "error"
      source = "wham_usage_refresh"
      statusCode = $statusCode
      error = "usage_json_parse_failed"
      bodyLength = $body.Length
    }
  }

  [ordered]@{
    status = "ok"
    source = "wham_usage_refresh"
    statusCode = $statusCode
    usage = $usage
    planType = if ($usage.plan_type) { [string]$usage.plan_type } else { $null }
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
  [ordered]@{
    exists = $true
    length = $item.Length
    lastWriteTime = $item.LastWriteTime.ToString("o")
    phases = @($events | ForEach-Object { $_.phase } | Where-Object { $_ } | Select-Object -Unique)
    errorTypes = @($events | ForEach-Object { $_.errorType } | Where-Object { $_ } | Select-Object -Unique)
    statuses = @($events | ForEach-Object { $_.status } | Where-Object { $null -ne $_ } | Select-Object -Unique)
    hasSensitiveMarkers = [bool](@($events | ConvertTo-Json -Depth 8) -match '(authorization|cookie|token|api[_-]?key|sk-[A-Za-z0-9])')
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
  $hasUsageLimit = [bool](@($events | Where-Object { $_.errorType -eq "usage_limit_reached" }).Count)
  $first429Index = -1
  $first200After429Index = -1
  for ($i = 0; $i -lt $events.Count; $i++) {
    if ($first429Index -lt 0 -and $events[$i].status -eq 429) {
      $first429Index = $i
    }
    if ($first429Index -ge 0 -and $i -gt $first429Index -and $events[$i].status -eq 200) {
      $first200After429Index = $i
      break
    }
  }

  [ordered]@{
    auditPath = $Path
    auditExists = $true
    tailEventCount = $events.Count
    has429 = $has429
    has200 = $has200
    hasUsageLimitReached = $hasUsageLimit
    hasModelCooldownApplied = $hasModelCooldown
    hasFallbackSelected = $hasFallbackSelected
    has200After429 = ($first200After429Index -gt $first429Index -and $first429Index -ge 0)
    pass = ($has429 -and $has200 -and $hasUsageLimit -and $hasModelCooldown -and $hasFallbackSelected -and ($first200After429Index -gt $first429Index -and $first429Index -ge 0))
  }
}

function Test-QuotaFallbackAudit {
  param([string]$Path)
  $result = New-SmokeResult "quota_fallback_audit_contract"
  if (-not $RequireQuotaFallback) {
    Set-SmokeSkipped $result "未传入 -RequireQuotaFallback；不强制要求本次 run 出现真实 429->fallback->200"
    return $result
  }

  $evidence = Get-QuotaFallbackAuditEvidence $Path
  if (-not $evidence.auditExists) {
    Set-SmokeBlocked $result "缺少 audit 文件，无法证明额度耗尽后的 fallback" $evidence
    return $result
  }

  if ($evidence.pass) {
    Set-SmokePass $result $evidence
    return $result
  }

  Set-SmokeBlocked $result "本次 run 未证明真实额度耗尽后 fallback；需要选入第一个会返回 429 的 OAuth 账号和第二个会返回 200 的 OAuth 账号" $evidence
  $result
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
  $evidence = @{
    enabled = [bool]$Config.enabled
    port = [int]$Config.port
    accountCount = $accountCount
    hardenedLocalMode = [bool]$safety.hardenedLocalMode
    maxConcurrentRequests = [int]$safety.maxConcurrentRequests
    minRequestIntervalSeconds = [int]$safety.minRequestIntervalSeconds
    maxRetryAccounts = [int]$safety.maxRetryAccounts
    fallbackMode = [string]$safety.fallbackMode
  }

  if (-not $safety.hardenedLocalMode -or $safety.maxConcurrentRequests -ne 1 -or $safety.maxRetryAccounts -ne 1) {
    if ($Stage -ne "fallback_probe" -or $safety.maxRetryAccounts -ne 2) {
      Set-SmokeFail $result "安全配置不是当前阶段允许的 hardened 合同" $evidence
      return $result
    }
  }

  if ($Stage -eq "single") {
    if ($accountCount -ne 1) {
      Set-SmokeBlocked $result "single 阶段要求 accountIds 恰好为 1" $evidence
      return $result
    }
    if ($safety.maxRetryAccounts -ne 1) {
      Set-SmokeBlocked $result "single 阶段要求 maxRetryAccounts = 1" $evidence
      return $result
    }
  } elseif ($Stage -eq "small_pool") {
    if ($accountCount -lt 2 -or $accountCount -gt 3) {
      Set-SmokeBlocked $result "small_pool 阶段要求 accountIds 为 2 到 3 个" $evidence
      return $result
    }
    if ($safety.maxRetryAccounts -ne 1) {
      Set-SmokeBlocked $result "small_pool 阶段仍要求 maxRetryAccounts = 1，只验证 selector/sticky 不乱轮换" $evidence
      return $result
    }
  } elseif ($Stage -eq "fallback_probe") {
    if ($accountCount -lt 2 -or $accountCount -gt 3) {
      Set-SmokeBlocked $result "fallback_probe 阶段要求 accountIds 为 2 到 3 个" $evidence
      return $result
    }
    if ($safety.maxRetryAccounts -ne 2 -or $safety.fallbackMode -ne "next_request_only") {
      Set-SmokeBlocked $result "fallback_probe 阶段要求 maxRetryAccounts = 2 且 fallbackMode = next_request_only" $evidence
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
    Set-SmokeSkipped $result "已启用 -AutoDrainFirstFreeAccountUntilFallback；由 quota_drain_until_fallback 执行真实上游请求"
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
  $result = New-SmokeResult "quota_drain_until_fallback"
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
  Set-SmokeBlocked $result "第一个 free 账号在最大消耗请求数内未触发 429->fallback->200；为避免过度请求已停止" @{
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

function Build-EphemeralGateway {
  $exe = Get-EphemeralGatewayExePath
  if ($SkipEphemeralGatewayBuild -and (Test-Path -LiteralPath $exe)) {
    return $exe
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

if ($AutoPopulateProbeAccountPool -and -not $AppSafeIsolatedProbe) {
  throw "-AutoPopulateProbeAccountPool 只能与 -AppSafeIsolatedProbe 一起使用，避免改动 live API service 号池"
}

if ($AutoPopulateProbeAccountPool -and -not $TemporaryFallbackConfig) {
  throw "-AutoPopulateProbeAccountPool 只能与 -TemporaryFallbackConfig 一起使用"
}

if ($AutoDrainFirstFreeAccountUntilFallback -and -not $AutoPopulateProbeAccountPool) {
  throw "-AutoDrainFirstFreeAccountUntilFallback 只能与 -AutoPopulateProbeAccountPool 一起使用"
}

if ($AutoDrainFirstFreeAccountUntilFallback -and -not $RequireQuotaFallback) {
  throw "-AutoDrainFirstFreeAccountUntilFallback 需要同时传入 -RequireQuotaFallback"
}

if ($AutoPopulateProbeAccountPool -and $AutoPopulateProbeAccountCount -ne 2) {
  throw "-AutoPopulateProbeAccountPool 的 fallback continuity 验收号池必须恰好 2 个账号：第一个 exhausted free OAuth，第二个 available free OAuth"
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

  $healthSummary = Get-JsonFileSummary $healthPath
  $auditSummary = Get-AuditTailSummary $auditPath
  $results += Test-QuotaFallbackAudit $auditPath
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
  autoPopulateProbeAccountPool = [bool]$AutoPopulateProbeAccountPool
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
  results = $results
  health = $healthSummary
  audit = $auditSummary
  safetyNotes = @(
    "report redacts API key and account identity",
    "default mode does not call /v1/chat/completions",
    "staged rollout: single -> small_pool -> fallback_probe",
    "use -RunUpstreamSmoke only after API service is enabled and the current stage contract passes",
    "use -RunCodexExecSmoke for a task-level E2E probe through an isolated CODEX_HOME",
    "use -RequireQuotaFallback when the acceptance target is real quota-exhaustion continuity; the report blocks unless audit shows 429, model cooldown, fallback selection, and 200",
    "use -StartEphemeralGateway to exercise the same gateway code path without switching live Codex provider",
    "use -TemporaryFallbackConfig only with -StartEphemeralGateway; the original codex_local_access.json is restored afterward",
    "use -AppSafeIsolatedProbe to keep probe local access config, health, audit, and port allocation in an isolated temp data root",
    "use -AutoPopulateProbeAccountPool only with -AppSafeIsolatedProbe; it refreshes wham/usage for OAuth candidates and writes exactly two free weekly OAuth account IDs only to the isolated probe config",
    "AutoPopulateProbeAccountPool defaults to max 2 live wham/usage refreshes; raise -AutoPopulateProbeMaxRefreshAttempts only for an explicit wider scan",
    "use -AutoDrainFirstFreeAccountUntilFallback only for explicit quota-drain acceptance; it sends bounded low-rate real requests until audit proves 429->fallback->200 or max requests is reached",
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
