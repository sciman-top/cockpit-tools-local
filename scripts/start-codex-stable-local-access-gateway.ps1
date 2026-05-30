param(
  [string]$RepoRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot "..")).Path,
  [string]$ReportDir = "",
  [string]$DataRoot = "",
  [int]$ReadyTimeoutSeconds = 30,
  [switch]$SkipBuild,
  [switch]$ForceBuild,
  [switch]$AllowConfigEnable,
  [switch]$CheckOnly,
  [switch]$Quiet
)

$ErrorActionPreference = "Stop"

function Get-NowIso {
  return (Get-Date).ToString("o")
}

function Write-JsonFile {
  param(
    [Parameter(Mandatory = $true)][string]$Path,
    [Parameter(Mandatory = $true)]$Value
  )

  $parent = Split-Path -Parent $Path
  if ($parent) {
    New-Item -ItemType Directory -Force -Path $parent | Out-Null
  }
  $Value | ConvertTo-Json -Depth 12 | Set-Content -LiteralPath $Path -Encoding UTF8
}

function Write-Event {
  param([Parameter(Mandatory = $true)][hashtable]$Event)

  $Event.timestamp = Get-NowIso
  $json = $Event | ConvertTo-Json -Depth 10 -Compress
  Add-Content -LiteralPath $script:EventLogPath -Value $json -Encoding UTF8
  if (-not $Quiet) {
    Write-Host $json
  }
}

function Resolve-DataRoot {
  if (-not [string]::IsNullOrWhiteSpace($DataRoot)) {
    return [System.IO.Path]::GetFullPath($DataRoot)
  }
  $envRoot = [Environment]::GetEnvironmentVariable("COCKPIT_LOCAL_ACCESS_DATA_ROOT", "Process")
  if (-not [string]::IsNullOrWhiteSpace($envRoot)) {
    return [System.IO.Path]::GetFullPath($envRoot)
  }
  return [System.IO.Path]::GetFullPath((Join-Path $HOME ".antigravity_cockpit"))
}

function Get-LocalAccessConfigPath {
  Join-Path $script:ResolvedDataRoot "codex_local_access.json"
}

function Get-LocalAccessConfig {
  $path = Get-LocalAccessConfigPath
  if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
    return $null
  }
  Get-Content -LiteralPath $path -Raw | ConvertFrom-Json
}

function Resolve-BaseUrl {
  param($Config)
  if ($null -eq $Config -or -not $Config.port) {
    return $null
  }
  return "http://127.0.0.1:$($Config.port)/v1"
}

function Resolve-ApiKey {
  param($Config)
  if ($null -eq $Config -or -not $Config.apiKey) {
    return $null
  }
  return [string]$Config.apiKey
}

function Invoke-LocalAccessHealth {
  param(
    [Parameter(Mandatory = $true)][string]$BaseUrl,
    [Parameter(Mandatory = $true)][string]$ApiKey,
    [int]$TimeoutSeconds = 2
  )

  try {
    $response = Invoke-WebRequest `
      -Method GET `
      -Uri "$BaseUrl/models" `
      -Headers @{ Authorization = "Bearer $ApiKey" } `
      -TimeoutSec $TimeoutSeconds `
      -SkipHttpErrorCheck
    return [ordered]@{
      ok = ([int]$response.StatusCode -ge 200 -and [int]$response.StatusCode -lt 300)
      statusCode = [int]$response.StatusCode
      error = $null
    }
  } catch {
    return [ordered]@{
      ok = $false
      statusCode = $null
      error = $_.Exception.Message
    }
  }
}

function Get-PortOwner {
  param([int]$Port)

  try {
    $connections = @(Get-NetTCPConnection -LocalAddress 127.0.0.1 -LocalPort $Port -State Listen -ErrorAction Stop)
  } catch {
    return @()
  }

  foreach ($connection in $connections) {
    $process = Get-Process -Id ([int]$connection.OwningProcess) -ErrorAction SilentlyContinue
    [pscustomobject][ordered]@{
      processId = [int]$connection.OwningProcess
      processName = if ($process) { $process.ProcessName } else { $null }
      path = if ($process) { $process.Path } else { $null }
    }
  }
}

function Get-GatewayExePath {
  Join-Path $RepoRoot "target\debug\codex-local-access-gateway.exe"
}

function Get-NewestGatewaySource {
  $roots = @(
    (Join-Path $RepoRoot "src-tauri\src"),
    (Join-Path $RepoRoot "crates")
  )
  $items = @()
  foreach ($root in $roots) {
    if (Test-Path -LiteralPath $root) {
      $items += @(Get-ChildItem -LiteralPath $root -Recurse -File -Include *.rs,*.toml -ErrorAction SilentlyContinue)
    }
  }
  foreach ($path in @(
      (Join-Path $RepoRoot "src-tauri\Cargo.toml"),
      (Join-Path $RepoRoot "src-tauri\Cargo.lock"),
      (Join-Path $RepoRoot "Cargo.toml"),
      (Join-Path $RepoRoot "Cargo.lock")
    )) {
    if (Test-Path -LiteralPath $path) {
      $items += Get-Item -LiteralPath $path
    }
  }
  @($items | Sort-Object LastWriteTimeUtc -Descending | Select-Object -First 1)
}

function Test-GatewayExeFresh {
  param([string]$Exe)
  if (-not (Test-Path -LiteralPath $Exe -PathType Leaf)) {
    return $false
  }
  $newestSource = Get-NewestGatewaySource
  if (-not $newestSource) {
    return $true
  }
  (Get-Item -LiteralPath $Exe).LastWriteTimeUtc -ge $newestSource.LastWriteTimeUtc
}

function Build-Gateway {
  $exe = Get-GatewayExePath
  if (-not $ForceBuild -and $SkipBuild -and (Test-GatewayExeFresh $exe)) {
    return $exe
  }
  if (-not $ForceBuild -and (Test-GatewayExeFresh $exe)) {
    return $exe
  }

  $manifest = Join-Path $RepoRoot "src-tauri\Cargo.toml"
  $targetDir = Join-Path $RepoRoot "target"
  Write-Event @{ event = "gateway_build_started"; command = "cargo build --manifest-path src-tauri/Cargo.toml --target-dir target --bin codex-local-access-gateway" }
  & cargo build --manifest-path $manifest --target-dir $targetDir --bin codex-local-access-gateway
  if ($LASTEXITCODE -ne 0) {
    throw "codex-local-access-gateway build failed: exit_code=$LASTEXITCODE"
  }
  if (-not (Test-Path -LiteralPath $exe -PathType Leaf)) {
    throw "gateway executable was not created: $exe"
  }
  Write-Event @{ event = "gateway_build_completed"; exe = $exe }
  return $exe
}

function Wait-GatewayReady {
  param(
    [Parameter(Mandatory = $true)][System.Diagnostics.Process]$Process,
    [Parameter(Mandatory = $true)][string]$BaseUrl,
    [Parameter(Mandatory = $true)][string]$ApiKey,
    [Parameter(Mandatory = $true)][string]$StdoutPath,
    [Parameter(Mandatory = $true)][string]$StderrPath
  )

  $deadline = (Get-Date).AddSeconds($ReadyTimeoutSeconds)
  $lastHealth = $null
  while ((Get-Date) -lt $deadline) {
    if ($Process.HasExited) {
      $stdout = if (Test-Path -LiteralPath $StdoutPath) { Get-Content -LiteralPath $StdoutPath -Raw } else { "" }
      $stderr = if (Test-Path -LiteralPath $StderrPath) { Get-Content -LiteralPath $StderrPath -Raw } else { "" }
      throw "gateway exited before ready: exit_code=$($Process.ExitCode), stdout=$stdout, stderr=$stderr"
    }

    $lastHealth = Invoke-LocalAccessHealth -BaseUrl $BaseUrl -ApiKey $ApiKey -TimeoutSeconds 2
    if ($lastHealth.ok) {
      return $lastHealth
    }
    Start-Sleep -Milliseconds 500
  }

  throw "gateway did not become ready in $ReadyTimeoutSeconds seconds: $($lastHealth | ConvertTo-Json -Compress)"
}

$RepoRoot = [System.IO.Path]::GetFullPath($RepoRoot)
if (-not (Test-Path -LiteralPath $RepoRoot -PathType Container)) {
  throw "RepoRoot does not exist: $RepoRoot"
}

if ([string]::IsNullOrWhiteSpace($ReportDir)) {
  $stamp = Get-Date -Format "yyyyMMdd-HHmmss"
  $ReportDir = Join-Path $RepoRoot "reports\codex-stable-local-access-gateway\$stamp"
}
$ReportDir = [System.IO.Path]::GetFullPath($ReportDir)
New-Item -ItemType Directory -Force -Path $ReportDir | Out-Null

$script:EventLogPath = Join-Path $ReportDir "gateway-events.jsonl"
$summaryPath = Join-Path $ReportDir "gateway-summary.json"
$startInfoPath = Join-Path $ReportDir "gateway-start-info.json"
$pidPath = Join-Path $ReportDir "gateway.pid.txt"
$commandPath = Join-Path $ReportDir "gateway-command.txt"
$stdoutPath = Join-Path $ReportDir "gateway.stdout.log"
$stderrPath = Join-Path $ReportDir "gateway.stderr.log"

$script:ResolvedDataRoot = Resolve-DataRoot
New-Item -ItemType Directory -Force -Path $script:ResolvedDataRoot | Out-Null

$config = Get-LocalAccessConfig
$baseUrl = Resolve-BaseUrl $config
$apiKey = Resolve-ApiKey $config
$port = if ($config -and $config.port) { [int]$config.port } else { $null }
$owners = if ($port) { @(Get-PortOwner -Port $port) } else { @() }

$startInfo = [ordered]@{
  startedAt = Get-NowIso
  pid = $PID
  repoRoot = $RepoRoot
  reportDir = $ReportDir
  dataRoot = $script:ResolvedDataRoot
  configPath = Get-LocalAccessConfigPath
  checkOnly = [bool]$CheckOnly
  allowConfigEnable = [bool]$AllowConfigEnable
  readyTimeoutSeconds = $ReadyTimeoutSeconds
}
Write-JsonFile -Path $startInfoPath -Value $startInfo
Write-Event @{ event = "stable_gateway_probe_started"; repoRoot = $RepoRoot; dataRoot = $script:ResolvedDataRoot; configPath = Get-LocalAccessConfigPath }

if ($null -eq $config) {
  $summary = [ordered]@{
    status = "blocked"
    reason = "codex_local_access.json not found"
    configPath = Get-LocalAccessConfigPath
    reportDir = $ReportDir
  }
  Write-JsonFile -Path $summaryPath -Value $summary
  Write-Event @{ event = "stable_gateway_blocked"; reason = $summary.reason }
  exit 2
}

if (-not $baseUrl -or -not $apiKey -or -not $port) {
  $summary = [ordered]@{
    status = "blocked"
    reason = "local access config is missing port or apiKey"
    configPath = Get-LocalAccessConfigPath
    reportDir = $ReportDir
  }
  Write-JsonFile -Path $summaryPath -Value $summary
  Write-Event @{ event = "stable_gateway_blocked"; reason = $summary.reason }
  exit 2
}

if ($config.enabled -ne $true -and -not $AllowConfigEnable) {
  $summary = [ordered]@{
    status = "blocked"
    reason = "local access config is disabled; rerun with -AllowConfigEnable if this mutation is intended"
    baseUrl = $baseUrl
    port = $port
    reportDir = $ReportDir
  }
  Write-JsonFile -Path $summaryPath -Value $summary
  Write-Event @{ event = "stable_gateway_blocked"; reason = $summary.reason; port = $port }
  exit 2
}

$initialHealth = Invoke-LocalAccessHealth -BaseUrl $baseUrl -ApiKey $apiKey -TimeoutSeconds 2
if ($initialHealth.ok) {
  $summary = [ordered]@{
    status = "already_running"
    baseUrl = $baseUrl
    port = $port
    owners = $owners
    health = $initialHealth
    reportDir = $ReportDir
  }
  Write-JsonFile -Path $summaryPath -Value $summary
  Write-Event @{ event = "stable_gateway_already_running"; baseUrl = $baseUrl; port = $port; ownerCount = $owners.Count }
  exit 0
}

if ($owners.Count -gt 0) {
  $summary = [ordered]@{
    status = "blocked"
    reason = "configured port is already occupied but health probe failed"
    baseUrl = $baseUrl
    port = $port
    owners = $owners
    health = $initialHealth
    reportDir = $ReportDir
  }
  Write-JsonFile -Path $summaryPath -Value $summary
  Write-Event @{ event = "stable_gateway_blocked_port_occupied"; port = $port; health = $initialHealth }
  exit 3
}

if ($CheckOnly) {
  $summary = [ordered]@{
    status = "not_running"
    baseUrl = $baseUrl
    port = $port
    health = $initialHealth
    reportDir = $ReportDir
  }
  Write-JsonFile -Path $summaryPath -Value $summary
  Write-Event @{ event = "stable_gateway_check_only_not_running"; port = $port; health = $initialHealth }
  exit 4
}

$exe = Build-Gateway
$commandText = "`"$exe`" --serve"
Set-Content -LiteralPath $commandPath -Value $commandText -Encoding UTF8

$previousDataRootEnv = [Environment]::GetEnvironmentVariable("COCKPIT_LOCAL_ACCESS_DATA_ROOT", "Process")
[Environment]::SetEnvironmentVariable("COCKPIT_LOCAL_ACCESS_DATA_ROOT", $script:ResolvedDataRoot, "Process")
try {
  $process = Start-Process `
    -FilePath $exe `
    -ArgumentList @("--serve") `
    -WorkingDirectory $RepoRoot `
    -WindowStyle Hidden `
    -PassThru `
    -RedirectStandardOutput $stdoutPath `
    -RedirectStandardError $stderrPath
} finally {
  [Environment]::SetEnvironmentVariable("COCKPIT_LOCAL_ACCESS_DATA_ROOT", $previousDataRootEnv, "Process")
}

Set-Content -LiteralPath $pidPath -Value $process.Id -Encoding UTF8
Write-Event @{ event = "stable_gateway_process_started"; processId = $process.Id; exe = $exe; baseUrl = $baseUrl }
$ready = Wait-GatewayReady -Process $process -BaseUrl $baseUrl -ApiKey $apiKey -StdoutPath $stdoutPath -StderrPath $stderrPath

$summary = [ordered]@{
  status = "started"
  processId = $process.Id
  exe = $exe
  command = $commandText
  baseUrl = $baseUrl
  port = $port
  dataRoot = $script:ResolvedDataRoot
  stdoutPath = $stdoutPath
  stderrPath = $stderrPath
  pidPath = $pidPath
  health = $ready
  reportDir = $ReportDir
}
Write-JsonFile -Path $summaryPath -Value $summary
Write-Event @{ event = "stable_gateway_ready"; processId = $process.Id; baseUrl = $baseUrl; port = $port }
