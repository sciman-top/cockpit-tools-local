param(
  [string]$BaseUrl,
  [string]$ApiKey,
  [string]$Model = "gpt-5.4",
  [ValidateSet("single", "small_pool", "fallback_probe")]
  [string]$Stage = "single",
  [switch]$RunUpstreamSmoke,
  [switch]$Expect429,
  [switch]$WriteReport,
  [switch]$StartEphemeralGateway,
  [int]$EphemeralGatewayReadyTimeoutSeconds = 60,
  [switch]$SkipEphemeralGatewayBuild
)

$ErrorActionPreference = "Stop"

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
  Join-Path $HOME ".antigravity_cockpit"
}

function Get-LocalAccessConfigPath {
  Join-Path (Get-DataRoot) "codex_local_access.json"
}

function Get-LocalAccessConfig {
  $path = Get-LocalAccessConfigPath
  if (-not (Test-Path -LiteralPath $path)) {
    return $null
  }

  Get-Content -LiteralPath $path -Raw | ConvertFrom-Json
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

function Get-AuditTailSummary {
  param([string]$Path, [int]$Tail = 40)
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
    if (-not $RunUpstreamSmoke) {
      Set-SmokeBlocked $result "fallback_probe 需要显式 -RunUpstreamSmoke 才能验证真实上游 fallback 边界" $evidence
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

  $exe = Build-EphemeralGateway
  $stdoutPath = Join-Path $tempDir "gateway.stdout.log"
  $stderrPath = Join-Path $tempDir "gateway.stderr.log"
  $process = Start-Process -FilePath $exe -ArgumentList @("--serve") -WindowStyle Hidden -PassThru -RedirectStandardOutput $stdoutPath -RedirectStandardError $stderrPath
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
$results = @()
$healthSummary = $null
$auditSummary = $null
$resolvedBaseUrl = $null
$dataRoot = Get-DataRoot

try {
  if ($StartEphemeralGateway) {
    $ephemeralGateway = Start-EphemeralGateway
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

  $healthSummary = Get-JsonFileSummary $healthPath
  $auditSummary = Get-AuditTailSummary $auditPath
} finally {
  if ($StartEphemeralGateway) {
    Stop-EphemeralGateway $ephemeralGateway
  }
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
  mode = if ($RunUpstreamSmoke) { "upstream_smoke" } else { "preflight" }
  stage = $Stage
  baseUrl = $resolvedBaseUrl
  dataRoot = $dataRoot
  runUpstreamSmoke = [bool]$RunUpstreamSmoke
  expect429 = [bool]$Expect429
  ephemeralGateway = $ephemeralGateway
  results = $results
  health = $healthSummary
  audit = $auditSummary
  safetyNotes = @(
    "report redacts API key and account identity",
    "default mode does not call /v1/chat/completions",
    "staged rollout: single -> small_pool -> fallback_probe",
    "use -RunUpstreamSmoke only after API service is enabled and the current stage contract passes",
    "use -StartEphemeralGateway to exercise the same gateway code path without switching live Codex provider"
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
