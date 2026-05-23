const { spawnSync } = require('node:child_process');
const path = require('node:path');

if (process.platform !== 'win32') {
  process.exit(0);
}

const repoRoot = path.resolve(__dirname, '..');
const targetExeCandidates = [
  path.join(repoRoot, 'target', 'debug', 'cockpit-tools.exe'),
  // Keep the legacy underscore name so older local build artifacts are still cleaned up.
  path.join(repoRoot, 'target', 'debug', 'cockpit_tools.exe'),
];
const escapedTargets = targetExeCandidates
  .map((targetExe) => `'${targetExe.replace(/'/g, "''").toLowerCase()}'`)
  .join(",\n  ");

const script = `
$ErrorActionPreference = 'Stop'
$targets = @(
  ${escapedTargets}
)
$processes = Get-CimInstance Win32_Process |
  Where-Object {
    $_.ExecutablePath -and
    $_.Name -in @('cockpit-tools.exe', 'cockpit_tools.exe') -and
    ($targets -contains $_.ExecutablePath.ToLowerInvariant())
  }
foreach ($process in $processes) {
  Stop-Process -Id $process.ProcessId -Force
  Write-Output ("Stopped stale Cockpit Tools debug process PID " + $process.ProcessId)
}
`;

const result = spawnSync(
  'powershell.exe',
  ['-NoProfile', '-ExecutionPolicy', 'Bypass', '-Command', script],
  {
    encoding: 'utf8',
    stdio: ['ignore', 'pipe', 'pipe'],
  }
);

if (result.stdout) {
  process.stdout.write(result.stdout);
}

if (result.status !== 0) {
  if (result.stderr) {
    process.stderr.write(result.stderr);
  }
  process.exit(result.status ?? 1);
}
