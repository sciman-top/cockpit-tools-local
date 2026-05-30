param(
  [string]$RepoRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot "..")).Path,
  [string]$ReportDir = "",
  [int]$PollIntervalSeconds = 5,
  [int]$LaunchCooldownSeconds = 45,
  [ValidateSet("Any", "Debug")]
  [string]$DesiredInstance = "Any",
  [int]$GracefulStopTimeoutSeconds = 8,
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
  $Value | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $Path -Encoding UTF8
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
  $repoPrefix = ([System.IO.Path]::GetFullPath($RepoRoot)).TrimEnd("\")
  Get-CimInstance Win32_Process |
    Where-Object {
      if ($_.ProcessId -eq $PID) {
        return $false
      }
      $cmd = [string]$_.CommandLine
      if ([string]::IsNullOrWhiteSpace($cmd)) {
        return $false
      }
      $hasRepo = $cmd.IndexOf($repoPrefix, [System.StringComparison]::OrdinalIgnoreCase) -ge 0
      $isNpmTauriDev = (
        $cmd -match '(?i)npm(\.cmd)?["\s].*run\s+tauri\s+dev' -or
        $cmd -match '(?i)npm-cli\.js.*run\s+tauri\s+dev' -or
        $cmd -match '(?i)\brun\s+tauri\s+dev\b'
      )
      $hasTauriDev = (
        $cmd.IndexOf("tauri", [System.StringComparison]::OrdinalIgnoreCase) -ge 0 -and
        $cmd.IndexOf("dev", [System.StringComparison]::OrdinalIgnoreCase) -ge 0
      )
      return $isNpmTauriDev -or ($hasRepo -and $hasTauriDev)
    } |
    Select-Object ProcessId, Name, ExecutablePath, CommandLine
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

function Start-CockpitTauriDev {
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
  debugExePath = Get-CockpitDebugExePath
  stopSignalFile = $StopSignalFile
  trigger = if ($DesiredInstance -eq "Debug") {
    "keep target\debug\cockpit-tools.exe as the active Cockpit app process; launch npm run tauri dev when debug is absent"
  } else {
    "launch npm run tauri dev only when no cockpit-tools*.exe app process exists"
  }
  stopSemantics = if ($DesiredInstance -eq "Debug") {
    "create stop.signal to stop watchdog; non-debug cockpit-tools app processes may be closed/stopped to switch to debug; never kill Codex processes"
  } else {
    "create stop.signal; do not kill app/codex/cockpit processes"
  }
}
Write-JsonFile -Path $metaPath -Value $startInfo
Write-LogLine @{ event = "watchdog_started"; pid = $PID; repoRoot = $RepoRoot; reportDir = $ReportDir }

$lastLaunchAt = $null
$launchCount = 0

while ($true) {
  if (Test-Path -LiteralPath $StopSignalFile) {
    Write-LogLine @{ event = "stop_signal_file"; stopSignalFile = $StopSignalFile }
    break
  }

  $processes = @(Get-CockpitAppProcesses)
  $debugProcesses = @($processes | Where-Object { Test-IsDebugCockpitProcess $_ })
  $nonDebugProcesses = @($processes | Where-Object { -not (Test-IsDebugCockpitProcess $_) })
  $tauriDevLaunchers = @(Get-TauriDevLauncherProcesses)
  $heartbeat = [ordered]@{
    timestamp = Get-NowIso
    pid = $PID
    desiredInstance = $DesiredInstance
    debugExePath = Get-CockpitDebugExePath
    runningCockpitProcessCount = $processes.Count
    runningCockpitProcesses = $processes
    runningDebugProcessCount = $debugProcesses.Count
    runningDebugProcesses = $debugProcesses
    runningNonDebugProcessCount = $nonDebugProcesses.Count
    runningNonDebugProcesses = $nonDebugProcesses
    tauriDevLauncherCount = $tauriDevLaunchers.Count
    tauriDevLaunchers = $tauriDevLaunchers
    launchCount = $launchCount
    lastLaunchAt = if ($lastLaunchAt) { $lastLaunchAt.ToString("o") } else { $null }
    stopSignalFile = $StopSignalFile
  }
  Write-JsonFile -Path $heartbeatPath -Value $heartbeat

  $needsTauriDev = $false
  $needsNonDebugStop = $false
  if ($DesiredInstance -eq "Debug") {
    $needsTauriDev = $debugProcesses.Count -eq 0 -and $tauriDevLaunchers.Count -eq 0
    $needsNonDebugStop = $nonDebugProcesses.Count -gt 0
  } else {
    $needsTauriDev = $processes.Count -eq 0
  }

  if ($DesiredInstance -eq "Debug" -and $debugProcesses.Count -gt 0 -and $nonDebugProcesses.Count -gt 0) {
    $stopResults = Stop-CockpitProcessesForDebugSwitch -Processes $nonDebugProcesses
    Write-LogLine @{ event = "non_debug_processes_stopped_while_debug_running"; processes = $stopResults }
  }

  if ($needsTauriDev) {
    $now = Get-Date
    $cooldownReady = $true
    if ($lastLaunchAt) {
      $cooldownReady = (($now - $lastLaunchAt).TotalSeconds -ge $LaunchCooldownSeconds)
    }

    if ($cooldownReady) {
      try {
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
    Write-LogLine @{ event = "waiting_for_existing_tauri_dev_launcher"; launcherCount = $tauriDevLaunchers.Count }
  }

  Start-Sleep -Seconds $PollIntervalSeconds
}

Write-LogLine @{ event = "watchdog_exited"; launchCount = $launchCount }
