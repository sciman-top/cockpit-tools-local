param(
  [string]$RepoRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot "..")).Path,
  [string]$ReportDir = "",
  [int]$PollIntervalSeconds = 5,
  [int]$LaunchCooldownSeconds = 45,
  [ValidateSet("Any", "Debug")]
  [string]$DesiredInstance = "Any",
  [ValidateSet("Immediate", "PrebuildThenStop")]
  [string]$DebugSwitchMode = "PrebuildThenStop",
  [int]$GracefulStopTimeoutSeconds = 8,
  [int]$DebugStartupGraceSeconds = 120,
  [int]$ReleaseFallbackCooldownSeconds = 10,
  [int]$ActiveStreamAuditWindowMinutes = 720,
  [switch]$DisableStableLocalAccessGateway,
  [switch]$DisableReleaseFallback,
  [switch]$EnableReleaseFallback,
  [string]$StopSignalFile = "",
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
  $json = $Value | ConvertTo-Json -Depth 8
  $tempPath = "{0}.{1}.{2}.tmp" -f $Path, $PID, ([guid]::NewGuid().ToString("N"))
  $lastError = $null

  for ($attempt = 1; $attempt -le 6; $attempt++) {
    try {
      Set-Content -LiteralPath $tempPath -Value $json -Encoding UTF8
      Move-Item -LiteralPath $tempPath -Destination $Path -Force
      return
    } catch {
      $lastError = $_.Exception.Message
      Remove-Item -LiteralPath $tempPath -Force -ErrorAction SilentlyContinue
      Start-Sleep -Milliseconds (120 * $attempt)
    }
  }

  if (-not $Quiet) {
    Write-Warning "failed to write json file '$Path': $lastError"
  }
}

function Write-LogLine {
  param([Parameter(Mandatory = $true)][hashtable]$Event)

  $Event.timestamp = Get-NowIso
  $json = $Event | ConvertTo-Json -Depth 8 -Compress
  Add-Content -LiteralPath $script:EventLogPath -Value $json -Encoding UTF8
  if (-not $Quiet) {
    Write-Host $json
  }
}

function Get-CockpitAppProcesses {
  $repoPrefix = ([System.IO.Path]::GetFullPath($RepoRoot)).TrimEnd("\")
  Get-CimInstance Win32_Process |
    Where-Object {
      $name = [string]$_.Name
      $path = [string]$_.ExecutablePath
      if ($name -notlike "cockpit-tools*.exe") {
        return $false
      }
      if (-not $path) {
        return $true
      }
      return $path.StartsWith($repoPrefix, [System.StringComparison]::OrdinalIgnoreCase) -or
        $path.IndexOf("Cockpit Tools Local", [System.StringComparison]::OrdinalIgnoreCase) -ge 0 -or
        $path.IndexOf("cockpit-tools-local", [System.StringComparison]::OrdinalIgnoreCase) -ge 0
    } |
    Select-Object ProcessId, Name, ExecutablePath, CommandLine
}

function Get-CockpitDebugExePath {
  return [System.IO.Path]::GetFullPath((Join-Path $RepoRoot "target\debug\cockpit-tools.exe"))
}

function Get-CockpitReleaseExePath {
  return [System.IO.Path]::GetFullPath((Join-Path $RepoRoot "target\release\cockpit-tools.exe"))
}

function Test-IsDebugCockpitProcess {
  param([Parameter(Mandatory = $true)]$ProcessInfo)

  $path = [string]$ProcessInfo.ExecutablePath
  if ([string]::IsNullOrWhiteSpace($path)) {
    return $false
  }
  return [System.IO.Path]::GetFullPath($path).Equals(
    (Get-CockpitDebugExePath),
    [System.StringComparison]::OrdinalIgnoreCase
  )
}

function Get-TauriDevLauncherProcesses {
  Get-CimInstance Win32_Process |
    Where-Object {
      if ($_.ProcessId -eq $PID) {
        return $false
      }
      $cmd = [string]$_.CommandLine
      if ([string]::IsNullOrWhiteSpace($cmd)) {
        return $false
      }
      $isNpmTauriDev = (
        $cmd -match '(?i)npm(\.cmd)?["\s].*run\s+tauri\s+dev' -or
        $cmd -match '(?i)npm-cli\.js.*run\s+tauri\s+dev'
      )
      $isTauriCliDev = $cmd -match '(?i)(@tauri-apps[\\/]+cli[\\/]+tauri\.js|tauri(\.cmd|\.exe|\.js)?)["\s]+dev\b'
      return $isNpmTauriDev -or $isTauriCliDev
    } |
    Select-Object ProcessId, Name, ExecutablePath, CommandLine
}

function Get-LocalAccessDataRoot {
  $envRoot = [Environment]::GetEnvironmentVariable("COCKPIT_LOCAL_ACCESS_DATA_ROOT", "Process")
  if (-not [string]::IsNullOrWhiteSpace($envRoot)) {
    return [System.IO.Path]::GetFullPath($envRoot)
  }
  return [System.IO.Path]::GetFullPath((Join-Path $HOME ".antigravity_cockpit"))
}

function Get-LocalAccessAuditPaths {
  $root = Get-LocalAccessDataRoot
  $path = Join-Path $root "codex_local_access_audit.jsonl"
  @("$path.1", $path) | Where-Object { Test-Path -LiteralPath $_ -PathType Leaf }
}

function Get-LocalAccessConfigSnapshot {
  $path = Join-Path (Get-LocalAccessDataRoot) "codex_local_access.json"
  if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
    return [ordered]@{
      exists = $false
      path = $path
      enabled = $null
      port = $null
      updatedAt = $null
    }
  }

  try {
    $config = Get-Content -LiteralPath $path -Raw | ConvertFrom-Json
    return [ordered]@{
      exists = $true
      path = $path
      enabled = $config.enabled
      port = $config.port
      updatedAt = $config.updatedAt
    }
  } catch {
    return [ordered]@{
      exists = $true
      path = $path
      enabled = $null
      port = $null
      updatedAt = $null
      error = $_.Exception.Message
    }
  }
}

function Get-StableLocalAccessGatewayProcesses {
  Get-CimInstance Win32_Process |
    Where-Object {
      $_.Name -eq "codex-local-access-gateway.exe"
    } |
    Select-Object ProcessId, Name, ExecutablePath, CommandLine
}

function Get-ObjectPropertyValue {
  param($Object, [string]$Name)
  if ($null -eq $Object) {
    return $null
  }
  $property = $Object.PSObject.Properties[$Name]
  if ($property) {
    return $property.Value
  }
  return $null
}

function Get-ActiveCodexLocalAccessStreamGuard {
  $activeLeases = @{}
  $events = New-Object System.Collections.Generic.List[object]
  $auditPaths = @(Get-LocalAccessAuditPaths)
  $nowMs = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()
  $windowStartMs = $nowMs - ([int64]$ActiveStreamAuditWindowMinutes * 60 * 1000)
  $sequence = 0
  $lastEventTimestampMs = $null
  $parseErrorCount = 0

  foreach ($path in $auditPaths) {
    try {
      foreach ($line in @(Get-Content -LiteralPath $path -Tail 4000 -ErrorAction Stop)) {
        if ([string]::IsNullOrWhiteSpace($line)) {
          continue
        }
        try {
          $event = $line | ConvertFrom-Json -ErrorAction Stop
        } catch {
          $parseErrorCount += 1
          continue
        }
        $timestamp = Get-ObjectPropertyValue $event "timestamp"
        if ($null -eq $timestamp) {
          continue
        }
        $timestamp = [int64]$timestamp
        if ($timestamp -lt $windowStartMs) {
          continue
        }
        $phase = [string](Get-ObjectPropertyValue $event "phase")
        if ($phase -ne "lease_granted" -and $phase -ne "lease_released") {
          continue
        }
        $detail = Get-ObjectPropertyValue $event "detail"
        $leaseId = [string](Get-ObjectPropertyValue $detail "lease_id")
        if ([string]::IsNullOrWhiteSpace($leaseId)) {
          continue
        }
        $events.Add([pscustomobject][ordered]@{
            timestamp = $timestamp
            sequence = $sequence
            phase = $phase
            leaseId = $leaseId
            path = $path
          })
        $sequence += 1
      }
    } catch {
      $parseErrorCount += 1
    }
  }

  foreach ($event in @($events | Sort-Object timestamp, sequence)) {
    $lastEventTimestampMs = $event.timestamp
    if ($event.phase -eq "lease_granted") {
      $activeLeases[$event.leaseId] = $event
    } elseif ($event.phase -eq "lease_released") {
      [void]$activeLeases.Remove($event.leaseId)
    }
  }

  [ordered]@{
    activeStreamCount = $activeLeases.Count
    activeLeaseIds = @($activeLeases.Keys)
    auditPaths = $auditPaths
    dataRoot = Get-LocalAccessDataRoot
    windowMinutes = $ActiveStreamAuditWindowMinutes
    lastEventTimestampMs = $lastEventTimestampMs
    parseErrorCount = $parseErrorCount
  }
}

function Stop-CockpitProcessesForDebugSwitch {
  param([Parameter(Mandatory = $true)][array]$Processes)

  $results = @()
  foreach ($processInfo in $Processes) {
    $id = [int]$processInfo.ProcessId
    $result = [ordered]@{
      processId = $id
      name = $processInfo.Name
      executablePath = $processInfo.ExecutablePath
      closeMainWindow = $false
      forcedStop = $false
      exited = $false
      error = $null
    }

    try {
      $process = Get-Process -Id $id -ErrorAction Stop
      if ($process.MainWindowHandle -ne 0) {
        $result.closeMainWindow = [bool]$process.CloseMainWindow()
      }

      $deadline = (Get-Date).AddSeconds($GracefulStopTimeoutSeconds)
      while ((Get-Date) -lt $deadline) {
        Start-Sleep -Milliseconds 250
        $stillRunning = Get-Process -Id $id -ErrorAction SilentlyContinue
        if (-not $stillRunning) {
          $result.exited = $true
          break
        }
      }

      if (-not $result.exited) {
        $stillRunning = Get-Process -Id $id -ErrorAction SilentlyContinue
        if ($stillRunning) {
          Stop-Process -Id $id -Force -ErrorAction Stop
          $result.forcedStop = $true
          Start-Sleep -Milliseconds 500
          $result.exited = -not [bool](Get-Process -Id $id -ErrorAction SilentlyContinue)
        }
      }
    } catch {
      $result.error = $_.Exception.Message
    }

    $results += [pscustomobject]$result
  }

  return $results
}

function Stop-ProcessTrees {
  param([Parameter(Mandatory = $true)][array]$Processes)

  $all = @(Get-CimInstance Win32_Process)
  $ids = New-Object System.Collections.Generic.List[int]

  function Add-ProcessTreeId {
    param([Parameter(Mandatory = $true)][int]$ProcessId)

    foreach ($child in @($all | Where-Object { [int]$_.ParentProcessId -eq $ProcessId })) {
      Add-ProcessTreeId -ProcessId ([int]$child.ProcessId)
    }
    if (-not $ids.Contains($ProcessId)) {
      $ids.Add($ProcessId)
    }
  }

  foreach ($processInfo in $Processes) {
    Add-ProcessTreeId -ProcessId ([int]$processInfo.ProcessId)
  }

  $results = @()
  foreach ($id in $ids) {
    $processInfo = $all | Where-Object { [int]$_.ProcessId -eq $id } | Select-Object -First 1
    $result = [ordered]@{
      processId = $id
      name = if ($processInfo) { $processInfo.Name } else { $null }
      executablePath = if ($processInfo) { $processInfo.ExecutablePath } else { $null }
      stopped = $false
      error = $null
    }
    try {
      if (Get-Process -Id $id -ErrorAction SilentlyContinue) {
        Stop-Process -Id $id -Force -ErrorAction Stop
        $result.stopped = $true
      }
    } catch {
      $result.error = $_.Exception.Message
    }
    $results += [pscustomobject]$result
  }

  return $results
}

function Start-CockpitTauriDev {
  $gateway = Ensure-StableLocalAccessGateway -Reason "before_tauri_dev_launch"
  $stdoutPath = Join-Path $ReportDir "tauri-dev.stdout.log"
  $stderrPath = Join-Path $ReportDir "tauri-dev.stderr.log"
  $process = Start-Process `
    -FilePath "npm.cmd" `
    -ArgumentList @("run", "tauri", "dev") `
    -WorkingDirectory $RepoRoot `
    -WindowStyle Hidden `
    -PassThru `
    -RedirectStandardOutput $stdoutPath `
    -RedirectStandardError $stderrPath

  return [ordered]@{
    processId = $process.Id
    command = "npm run tauri dev"
    stdoutPath = $stdoutPath
    stderrPath = $stderrPath
    stableLocalAccessGateway = $gateway
  }
}

function Start-CockpitReleaseFallback {
  $gateway = Ensure-StableLocalAccessGateway -Reason "before_release_fallback_launch"
  $releaseExePath = Get-CockpitReleaseExePath
  if (-not (Test-Path -LiteralPath $releaseExePath -PathType Leaf)) {
    throw "Release fallback executable does not exist: $releaseExePath"
  }

  $process = Start-Process `
    -FilePath $releaseExePath `
    -WorkingDirectory $RepoRoot `
    -PassThru

  return [ordered]@{
    processId = $process.Id
    command = $releaseExePath
    executablePath = $releaseExePath
    stableLocalAccessGateway = $gateway
  }
}

function Start-CockpitDebugPrebuild {
  $stdoutPath = Join-Path $ReportDir "debug-prebuild.stdout.log"
  $stderrPath = Join-Path $ReportDir "debug-prebuild.stderr.log"
  $manifestPath = Join-Path $RepoRoot "src-tauri\Cargo.toml"
  $process = Start-Process `
    -FilePath "cargo.exe" `
    -ArgumentList @("build", "--manifest-path", $manifestPath, "--no-default-features", "--color", "never") `
    -WorkingDirectory $RepoRoot `
    -WindowStyle Hidden `
    -PassThru `
    -RedirectStandardOutput $stdoutPath `
    -RedirectStandardError $stderrPath

  return [ordered]@{
    process = $process
    processId = $process.Id
    command = "cargo build --manifest-path src-tauri/Cargo.toml --no-default-features --color never"
    stdoutPath = $stdoutPath
    stderrPath = $stderrPath
    startedAt = Get-Date
  }
}

function Get-DebugPrebuildSnapshot {
  param($Prebuild)

  if (-not $Prebuild) {
    return $null
  }

  $hasExited = $false
  $exitCode = $null
  try {
    $hasExited = [bool]$Prebuild.process.HasExited
    if ($hasExited) {
      $exitCode = [int]$Prebuild.process.ExitCode
    }
  } catch {
    $hasExited = $true
    $exitCode = $null
  }

  return [ordered]@{
    processId = $Prebuild.processId
    command = $Prebuild.command
    stdoutPath = $Prebuild.stdoutPath
    stderrPath = $Prebuild.stderrPath
    startedAt = $Prebuild.startedAt.ToString("o")
    hasExited = $hasExited
    exitCode = $exitCode
  }
}

function Ensure-StableLocalAccessGateway {
  param([string]$Reason)

  if ($DisableStableLocalAccessGateway) {
    return [ordered]@{
      requested = $false
      status = "disabled"
      reason = "DisableStableLocalAccessGateway"
    }
  }

  $scriptPath = Join-Path $RepoRoot "scripts\start-codex-stable-local-access-gateway.ps1"
  if (-not (Test-Path -LiteralPath $scriptPath -PathType Leaf)) {
    return [ordered]@{
      requested = $true
      status = "missing_script"
      scriptPath = $scriptPath
    }
  }

  $stamp = Get-Date -Format "yyyyMMdd-HHmmssfff"
  $gatewayReportDir = Join-Path $ReportDir ("stable-local-access-gateway-{0}" -f $stamp)
  $args = @(
    "-NoProfile",
    "-ExecutionPolicy",
    "Bypass",
    "-File",
    $scriptPath,
    "-RepoRoot",
    $RepoRoot,
    "-ReportDir",
    $gatewayReportDir,
    "-AllowConfigEnable",
    "-Quiet"
  )

  $summaryPath = Join-Path $gatewayReportDir "gateway-summary.json"
  try {
    & pwsh @args
    $exitCode = $LASTEXITCODE
    $summary = if (Test-Path -LiteralPath $summaryPath -PathType Leaf) {
      Get-Content -LiteralPath $summaryPath -Raw | ConvertFrom-Json
    } else {
      $null
    }
    $result = [ordered]@{
      requested = $true
      status = if ($summary -and $summary.status) { [string]$summary.status } else { "unknown" }
      exitCode = $exitCode
      reason = $Reason
      reportDir = $gatewayReportDir
      summaryPath = $summaryPath
      summary = $summary
    }
    Write-LogLine @{ event = "stable_local_access_gateway_checked"; result = $result }
    return $result
  } catch {
    $result = [ordered]@{
      requested = $true
      status = "error"
      reason = $Reason
      reportDir = $gatewayReportDir
      message = $_.Exception.Message
    }
    Write-LogLine @{ event = "stable_local_access_gateway_check_failed"; result = $result }
    return $result
  }
}

$RepoRoot = [System.IO.Path]::GetFullPath($RepoRoot)
if (-not (Test-Path -LiteralPath $RepoRoot -PathType Container)) {
  throw "RepoRoot does not exist: $RepoRoot"
}

if ([string]::IsNullOrWhiteSpace($ReportDir)) {
  $stamp = Get-Date -Format "yyyyMMdd-HHmmss"
  $ReportDir = Join-Path $RepoRoot "reports\cockpit-dev-watchdog\$stamp"
}
$ReportDir = [System.IO.Path]::GetFullPath($ReportDir)
New-Item -ItemType Directory -Force -Path $ReportDir | Out-Null

if ([string]::IsNullOrWhiteSpace($StopSignalFile)) {
  $StopSignalFile = Join-Path $ReportDir "stop.signal"
}
$releaseFallbackEnabled = [bool]$EnableReleaseFallback -and -not [bool]$DisableReleaseFallback

$script:EventLogPath = Join-Path $ReportDir "watchdog-events.jsonl"
$heartbeatPath = Join-Path $ReportDir "watchdog-heartbeat.json"
$metaPath = Join-Path $ReportDir "watchdog-start-info.json"
$pidPath = Join-Path $ReportDir "watchdog-pid.txt"

Set-Content -LiteralPath $pidPath -Value $PID -Encoding UTF8

$startInfo = [ordered]@{
  startedAt = Get-NowIso
  pid = $PID
  repoRoot = $RepoRoot
  reportDir = $ReportDir
  pollIntervalSeconds = $PollIntervalSeconds
  launchCooldownSeconds = $LaunchCooldownSeconds
  desiredInstance = $DesiredInstance
  debugSwitchMode = $DebugSwitchMode
  releaseFallbackEnabled = $releaseFallbackEnabled
  releaseFallbackCooldownSeconds = $ReleaseFallbackCooldownSeconds
  stableLocalAccessGatewayEnabled = -not [bool]$DisableStableLocalAccessGateway
  debugStartupGraceSeconds = $DebugStartupGraceSeconds
  debugExePath = Get-CockpitDebugExePath
  releaseExePath = Get-CockpitReleaseExePath
  stopSignalFile = $StopSignalFile
  trigger = if ($DesiredInstance -eq "Debug") {
    if ($DebugSwitchMode -eq "PrebuildThenStop" -and $releaseFallbackEnabled) {
      "keep target\debug\cockpit-tools.exe as the active Cockpit app process; if debug exits, start target\release\cockpit-tools.exe as fallback, prebuild debug, then stop fallback only after debug prebuild succeeds"
    } elseif ($DebugSwitchMode -eq "PrebuildThenStop") {
      "keep target\debug\cockpit-tools.exe as the active Cockpit app process; launch npm run tauri dev when debug is absent, and only prebuild before stopping an already-running non-debug Cockpit process"
    } else {
      "keep target\debug\cockpit-tools.exe as the active Cockpit app process; launch npm run tauri dev when debug is absent"
    }
  } else {
    "launch npm run tauri dev only when no cockpit-tools*.exe app process exists"
  }
  stopSemantics = if ($DesiredInstance -eq "Debug") {
    if ($DebugSwitchMode -eq "PrebuildThenStop" -and $releaseFallbackEnabled) {
      "create stop.signal to stop watchdog; non-debug cockpit-tools app processes are used as fallback and are preserved until debug prebuild succeeds or debug is already running; never kill Codex processes"
    } elseif ($DebugSwitchMode -eq "PrebuildThenStop") {
      "create stop.signal to stop watchdog; no release fallback is started unless -EnableReleaseFallback is set; existing non-debug cockpit-tools app processes are preserved until debug prebuild succeeds or debug is already running; never kill Codex processes"
    } else {
      "create stop.signal to stop watchdog; non-debug cockpit-tools app processes may be closed/stopped to switch to debug; never kill Codex processes"
    }
  } else {
    "create stop.signal; do not kill app/codex/cockpit processes"
  }
}
Write-JsonFile -Path $metaPath -Value $startInfo
Write-LogLine @{ event = "watchdog_started"; pid = $PID; repoRoot = $RepoRoot; reportDir = $ReportDir }

$lastLaunchAt = $null
$lastPrebuildAt = $null
$lastReleaseFallbackAt = $null
$debugPrebuild = $null
$pendingDebugLaunchAfterPrebuild = $false
$pendingDebugLaunchPrebuild = $null
$debugMissingSince = $null
$launchCount = 0
$prebuildCount = 0
$releaseFallbackCount = 0

while ($true) {
  if (Test-Path -LiteralPath $StopSignalFile) {
    Write-LogLine @{ event = "stop_signal_file"; stopSignalFile = $StopSignalFile }
    break
  }

  $processes = @(Get-CockpitAppProcesses)
  $debugProcesses = @($processes | Where-Object { Test-IsDebugCockpitProcess $_ })
  $nonDebugProcesses = @($processes | Where-Object { -not (Test-IsDebugCockpitProcess $_) })
  $tauriDevLaunchers = @(Get-TauriDevLauncherProcesses)
  $stableGatewayProcesses = @(Get-StableLocalAccessGatewayProcesses)
  $localAccessConfig = Get-LocalAccessConfigSnapshot
  $activeStreamGuard = Get-ActiveCodexLocalAccessStreamGuard
  $hasActiveCodexStreams = ([int]$activeStreamGuard.activeStreamCount) -gt 0
  $periodicStableGatewayGuard = $null
  if (-not $DisableStableLocalAccessGateway -and (
      $stableGatewayProcesses.Count -eq 0 -or
      $localAccessConfig.enabled -ne $true
    )) {
    $periodicStableGatewayGuard = Ensure-StableLocalAccessGateway -Reason "watchdog_periodic_guard"
    $stableGatewayProcesses = @(Get-StableLocalAccessGatewayProcesses)
    $localAccessConfig = Get-LocalAccessConfigSnapshot
  }
  $completedPrebuild = $null
  if ($debugPrebuild -and $debugPrebuild.process.HasExited) {
    $completedPrebuild = Get-DebugPrebuildSnapshot -Prebuild $debugPrebuild
    $lastPrebuildAt = Get-Date
    $debugPrebuild = $null
    Write-LogLine @{ event = "debug_prebuild_completed"; prebuild = $completedPrebuild }
  }

  $heartbeat = [ordered]@{
    timestamp = Get-NowIso
    pid = $PID
    desiredInstance = $DesiredInstance
    debugSwitchMode = $DebugSwitchMode
    releaseFallbackEnabled = $releaseFallbackEnabled
    releaseFallbackCooldownSeconds = $ReleaseFallbackCooldownSeconds
    debugExePath = Get-CockpitDebugExePath
    releaseExePath = Get-CockpitReleaseExePath
    runningCockpitProcessCount = $processes.Count
    runningCockpitProcesses = $processes
    runningDebugProcessCount = $debugProcesses.Count
    runningDebugProcesses = $debugProcesses
    runningNonDebugProcessCount = $nonDebugProcesses.Count
    runningNonDebugProcesses = $nonDebugProcesses
    tauriDevLauncherCount = $tauriDevLaunchers.Count
    tauriDevLaunchers = $tauriDevLaunchers
    stableLocalAccessGatewayProcesses = $stableGatewayProcesses
    localAccessConfig = $localAccessConfig
    periodicStableLocalAccessGateway = $periodicStableGatewayGuard
    activeCodexStreamGuard = $activeStreamGuard
    launchCount = $launchCount
    prebuildCount = $prebuildCount
    releaseFallbackCount = $releaseFallbackCount
    lastLaunchAt = if ($lastLaunchAt) { $lastLaunchAt.ToString("o") } else { $null }
    lastPrebuildAt = if ($lastPrebuildAt) { $lastPrebuildAt.ToString("o") } else { $null }
    lastReleaseFallbackAt = if ($lastReleaseFallbackAt) { $lastReleaseFallbackAt.ToString("o") } else { $null }
    debugPrebuild = Get-DebugPrebuildSnapshot -Prebuild $debugPrebuild
    pendingDebugLaunchAfterPrebuild = $pendingDebugLaunchAfterPrebuild
    pendingDebugLaunchPrebuild = $pendingDebugLaunchPrebuild
    debugMissingSince = if ($debugMissingSince) { $debugMissingSince.ToString("o") } else { $null }
    stopSignalFile = $StopSignalFile
  }
  Write-JsonFile -Path $heartbeatPath -Value $heartbeat

  $needsTauriDev = $false
  $needsDebugPrebuild = $false
  $needsReleaseFallback = $false
  $needsNonDebugStop = $false
  if ($DesiredInstance -eq "Debug") {
    if ($debugProcesses.Count -gt 0) {
      $debugMissingSince = $null
      $pendingDebugLaunchAfterPrebuild = $false
      $pendingDebugLaunchPrebuild = $null
    } elseif (-not $debugMissingSince) {
      $debugMissingSince = Get-Date
    }

    $hasPrebuildRunning = $null -ne $debugPrebuild
    $canStartDebugWork = $debugProcesses.Count -eq 0 -and $tauriDevLaunchers.Count -eq 0 -and -not $hasPrebuildRunning
    $usePrebuildSwitch = $DebugSwitchMode -eq "PrebuildThenStop" -and $nonDebugProcesses.Count -gt 0
    $needsReleaseFallback = (
      $DebugSwitchMode -eq "PrebuildThenStop" -and
      $releaseFallbackEnabled -and
      $debugProcesses.Count -eq 0 -and
      $nonDebugProcesses.Count -eq 0 -and
      $tauriDevLaunchers.Count -eq 0 -and
      -not $hasPrebuildRunning
    )
    $needsDebugPrebuild = $canStartDebugWork -and $usePrebuildSwitch
    $needsTauriDev = $canStartDebugWork -and -not $usePrebuildSwitch
    $needsNonDebugStop = $nonDebugProcesses.Count -gt 0 -and (
      $DebugSwitchMode -eq "Immediate" -or
      $debugProcesses.Count -gt 0
    )
  } else {
    $needsTauriDev = $processes.Count -eq 0
  }

  if ($pendingDebugLaunchAfterPrebuild -and $DesiredInstance -eq "Debug") {
    $needsDebugPrebuild = $false
    if ($debugProcesses.Count -gt 0) {
      $pendingDebugLaunchAfterPrebuild = $false
      $pendingDebugLaunchPrebuild = $null
    } elseif ($tauriDevLaunchers.Count -eq 0) {
      if ($nonDebugProcesses.Count -gt 0 -and $hasActiveCodexStreams) {
        Write-LogLine @{
          event = "restart_deferred_active_stream"
          action = "pending_stop_release_and_launch_debug_after_prebuild"
          activeCodexStreamGuard = $activeStreamGuard
          runningNonDebugProcessCount = $nonDebugProcesses.Count
          prebuild = $pendingDebugLaunchPrebuild
        }
      } else {
        if ($nonDebugProcesses.Count -gt 0) {
          $stopResults = Stop-CockpitProcessesForDebugSwitch -Processes $nonDebugProcesses
          Write-LogLine @{ event = "pending_non_debug_processes_stopped_after_debug_prebuild"; processes = $stopResults }
        }
        try {
          $launch = Start-CockpitTauriDev
          $lastLaunchAt = Get-Date
          $launchCount += 1
          $pendingDebugLaunchAfterPrebuild = $false
          $pendingDebugLaunchPrebuild = $null
          $needsTauriDev = $false
          Write-LogLine @{ event = "pending_tauri_dev_launched_after_debug_prebuild"; launch = $launch; launchCount = $launchCount }
        } catch {
          Write-LogLine @{ event = "pending_tauri_dev_launch_failed_after_debug_prebuild"; message = $_.Exception.Message }
        }
      }
    }
  }

  if ($needsReleaseFallback) {
    $now = Get-Date
    $cooldownReady = $true
    if ($lastReleaseFallbackAt) {
      $cooldownReady = (($now - $lastReleaseFallbackAt).TotalSeconds -ge $ReleaseFallbackCooldownSeconds)
    }

    if ($cooldownReady) {
      try {
        $fallback = Start-CockpitReleaseFallback
        $lastReleaseFallbackAt = $now
        $releaseFallbackCount += 1
        $needsTauriDev = $false
        Write-LogLine @{
          event = "release_fallback_started_for_missing_debug"
          fallback = $fallback
          releaseFallbackCount = $releaseFallbackCount
          tauriDevLauncherCount = $tauriDevLaunchers.Count
        }
      } catch {
        $lastReleaseFallbackAt = $now
        Write-LogLine @{ event = "release_fallback_start_failed"; message = $_.Exception.Message }
      }
    } else {
      $needsTauriDev = $false
      Write-LogLine @{ event = "release_fallback_cooldown"; releaseFallbackCount = $releaseFallbackCount }
    }
  }

  if ($DesiredInstance -eq "Debug" -and $needsNonDebugStop) {
    if ($hasActiveCodexStreams) {
      Write-LogLine @{
        event = "restart_deferred_active_stream"
        action = "stop_non_debug_for_debug_switch"
        activeCodexStreamGuard = $activeStreamGuard
        runningNonDebugProcessCount = $nonDebugProcesses.Count
      }
    } else {
      $stopResults = Stop-CockpitProcessesForDebugSwitch -Processes $nonDebugProcesses
      Write-LogLine @{
        event = if ($debugProcesses.Count -gt 0) {
          "non_debug_processes_stopped_while_debug_running"
        } else {
          "non_debug_processes_stopped_while_waiting_for_debug"
        }
        processes = $stopResults
      }
    }
  }

  if ($completedPrebuild -and $DesiredInstance -eq "Debug") {
    if ($completedPrebuild.exitCode -eq 0) {
      if ($debugProcesses.Count -eq 0) {
        if ($nonDebugProcesses.Count -gt 0 -and $hasActiveCodexStreams) {
          $pendingDebugLaunchAfterPrebuild = $true
          $pendingDebugLaunchPrebuild = $completedPrebuild
          Write-LogLine @{
            event = "restart_deferred_active_stream"
            action = "stop_release_and_launch_debug_after_prebuild"
            activeCodexStreamGuard = $activeStreamGuard
            runningNonDebugProcessCount = $nonDebugProcesses.Count
          }
        } else {
          if ($nonDebugProcesses.Count -gt 0) {
            $stopResults = Stop-CockpitProcessesForDebugSwitch -Processes $nonDebugProcesses
            Write-LogLine @{ event = "non_debug_processes_stopped_after_debug_prebuild"; processes = $stopResults }
          }
          try {
            $launch = Start-CockpitTauriDev
            $lastLaunchAt = Get-Date
            $launchCount += 1
            Write-LogLine @{ event = "tauri_dev_launched_after_debug_prebuild"; launch = $launch; launchCount = $launchCount }
          } catch {
            Write-LogLine @{ event = "tauri_dev_launch_failed_after_debug_prebuild"; message = $_.Exception.Message }
          }
        }
      }
    } else {
      Write-LogLine @{
        event = "debug_prebuild_failed_release_preserved"
        prebuild = $completedPrebuild
        runningNonDebugProcessCount = $nonDebugProcesses.Count
      }
    }
  } elseif ($needsDebugPrebuild) {
    $now = Get-Date
    $cooldownReady = $true
    if ($lastPrebuildAt) {
      $cooldownReady = (($now - $lastPrebuildAt).TotalSeconds -ge $LaunchCooldownSeconds)
    }

    if ($cooldownReady) {
      try {
        $debugPrebuild = Start-CockpitDebugPrebuild
        $prebuildCount += 1
        Write-LogLine @{
          event = "debug_prebuild_started_release_preserved"
          prebuild = Get-DebugPrebuildSnapshot -Prebuild $debugPrebuild
          runningNonDebugProcessCount = $nonDebugProcesses.Count
          prebuildCount = $prebuildCount
        }
      } catch {
        $lastPrebuildAt = $now
        Write-LogLine @{ event = "debug_prebuild_start_failed_release_preserved"; message = $_.Exception.Message }
      }
    } else {
      Write-LogLine @{ event = "debug_prebuild_cooldown_release_preserved"; prebuildCount = $prebuildCount }
    }
  }

  if ($needsTauriDev) {
    $now = Get-Date
    $cooldownReady = $true
    if ($lastLaunchAt) {
      $cooldownReady = (($now - $lastLaunchAt).TotalSeconds -ge $LaunchCooldownSeconds)
    }

    if ($cooldownReady) {
      try {
        if ($needsNonDebugStop -and $hasActiveCodexStreams) {
          Write-LogLine @{
            event = "restart_deferred_active_stream"
            action = "stop_non_debug_before_debug_launch"
            activeCodexStreamGuard = $activeStreamGuard
            runningNonDebugProcessCount = $nonDebugProcesses.Count
          }
          Start-Sleep -Seconds $PollIntervalSeconds
          continue
        }
        if ($needsNonDebugStop) {
          $stopResults = Stop-CockpitProcessesForDebugSwitch -Processes $nonDebugProcesses
          Write-LogLine @{ event = "non_debug_processes_stopped_before_debug_launch"; processes = $stopResults }
        }
        $launch = Start-CockpitTauriDev
        $lastLaunchAt = $now
        $launchCount += 1
        Write-LogLine @{ event = "tauri_dev_launched"; launch = $launch; launchCount = $launchCount }
      } catch {
        Write-LogLine @{ event = "tauri_dev_launch_failed"; message = $_.Exception.Message }
      }
    } else {
      Write-LogLine @{ event = "no_cockpit_process_cooldown"; launchCount = $launchCount }
    }
  } elseif ($DesiredInstance -eq "Debug" -and $debugProcesses.Count -eq 0 -and $tauriDevLaunchers.Count -gt 0) {
    $missingSeconds = if ($debugMissingSince) { [int]((Get-Date) - $debugMissingSince).TotalSeconds } else { 0 }
    if ($missingSeconds -ge $DebugStartupGraceSeconds) {
      if ($hasActiveCodexStreams) {
        Write-LogLine @{
          event = "restart_deferred_active_stream"
          action = "stop_stale_tauri_dev_launchers"
          activeCodexStreamGuard = $activeStreamGuard
          missingSeconds = $missingSeconds
          launcherCount = $tauriDevLaunchers.Count
        }
      } else {
        $stopResults = Stop-ProcessTrees -Processes $tauriDevLaunchers
        Write-LogLine @{
          event = "stale_tauri_dev_launchers_stopped"
          missingSeconds = $missingSeconds
          launcherCount = $tauriDevLaunchers.Count
          processes = $stopResults
        }
        $lastLaunchAt = $null
        $debugMissingSince = Get-Date
      }
    } else {
      Write-LogLine @{
        event = "waiting_for_existing_tauri_dev_launcher"
        launcherCount = $tauriDevLaunchers.Count
        missingSeconds = $missingSeconds
        graceSeconds = $DebugStartupGraceSeconds
      }
    }
  }

  Start-Sleep -Seconds $PollIntervalSeconds
}

Write-LogLine @{
  event = "watchdog_exited"
  launchCount = $launchCount
  prebuildCount = $prebuildCount
  releaseFallbackCount = $releaseFallbackCount
}
