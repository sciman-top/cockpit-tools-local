param()

$ErrorActionPreference = "Stop"

function Assert-True {
  param([bool]$Condition, [string]$Message)
  if (-not $Condition) {
    throw $Message
  }
}

function Assert-Contains {
  param([string]$Text, [string]$Needle, [string]$Message)
  Assert-True ($Text.Contains($Needle)) $Message
}

function Assert-Matches {
  param([string]$Text, [string]$Pattern, [string]$Message)
  Assert-True ($Text -match $Pattern) $Message
}

function Convert-JsonOutput {
  param([object[]]$Output, [string]$Context)
  $text = ($Output | Out-String).Trim()
  if (-not $text) {
    throw "$Context did not emit JSON"
  }
  $text | ConvertFrom-Json
}

function Get-GuardMarkdownFiles {
  param([string]$Root)

  $files = @()
  foreach ($name in @("README.md", "SECURITY.md", "AGENTS.md")) {
    $path = Join-Path $Root $name
    if (Test-Path -LiteralPath $path) {
      $files += Get-Item -LiteralPath $path
    }
  }

  $docsRoot = Join-Path $Root "docs"
  if (Test-Path -LiteralPath $docsRoot) {
    $files += Get-ChildItem -LiteralPath $docsRoot -Recurse -File -Filter "*.md"
  }

  $files | Sort-Object FullName -Unique
}

function Assert-LiveUpstreamDocExamplesRequireAcknowledgement {
  param([string]$Root)

  $violations = @()
  foreach ($file in Get-GuardMarkdownFiles $Root) {
    $relativePath = [System.IO.Path]::GetRelativePath($Root, $file.FullName)
    $text = Get-Content -LiteralPath $file.FullName -Raw

    $fencedBlocks = [regex]::Matches($text, '```[^\r\n]*\r?\n(?<block>[\s\S]*?)```')
    foreach ($match in $fencedBlocks) {
      $block = $match.Groups["block"].Value
      if ($block -match 'smoke-local-hardened-api\.ps1' -and $block -match '-RunUpstreamSmoke' -and $block -notmatch '-AcknowledgeLiveUpstreamRisk') {
        $violations += "$relativePath fenced smoke command is missing -AcknowledgeLiveUpstreamRisk"
      }
      if ($block -match 'accept-local-hardened-api-continuity\.ps1' -and $block -notmatch '-AcknowledgeLiveUpstreamRisk') {
        $violations += "$relativePath fenced acceptance command is missing -AcknowledgeLiveUpstreamRisk"
      }
    }

    $lines = Get-Content -LiteralPath $file.FullName
    for ($i = 0; $i -lt $lines.Count; $i++) {
      $line = $lines[$i]
      $lineNumber = $i + 1
      $continuesOnNextLine = $line.TrimEnd().EndsWith([string][char]0x60)
      if (-not $continuesOnNextLine -and $line -match 'smoke-local-hardened-api\.ps1' -and $line -match '-RunUpstreamSmoke' -and $line -notmatch '-AcknowledgeLiveUpstreamRisk') {
        $violations += ("{0}:{1} inline smoke command is missing -AcknowledgeLiveUpstreamRisk" -f $relativePath, $lineNumber)
      }
      if (-not $continuesOnNextLine -and $line -match 'accept-local-hardened-api-continuity\.ps1' -and $line -notmatch '-AcknowledgeLiveUpstreamRisk') {
        $violations += ("{0}:{1} inline acceptance command is missing -AcknowledgeLiveUpstreamRisk" -f $relativePath, $lineNumber)
      }
    }
  }

  Assert-True ($violations.Count -eq 0) ("live upstream doc examples missing acknowledgement:`n{0}" -f ($violations -join "`n"))
}

$repoRoot = Split-Path -Parent $PSScriptRoot
$agentsPath = Join-Path $repoRoot "AGENTS.md"
$agents = if (Test-Path -LiteralPath $agentsPath) { Get-Content -LiteralPath $agentsPath -Raw } else { $null }
$doc = Get-Content -LiteralPath (Join-Path $repoRoot "docs\LOCAL_HARDENED_API.md") -Raw
$smoke = Get-Content -LiteralPath (Join-Path $PSScriptRoot "smoke-local-hardened-api.ps1") -Raw
$accept = Get-Content -LiteralPath (Join-Path $PSScriptRoot "accept-local-hardened-api-continuity.ps1") -Raw

if ($agents) {
  Assert-Contains $agents "AcknowledgeLiveUpstreamRisk" "AGENTS.md must require live upstream acknowledgement"
  Assert-Contains $agents "AcknowledgeExpandedLiveUpstreamRisk" "AGENTS.md must require expanded live upstream acknowledgement"
  Assert-Contains $agents "Cooldown recovery must be inferred from stored reset times/health registry" "AGENTS.md must forbid cooldown polling by default"
  Assert-True ($agents -notmatch "AutoPopulateProbeAccountPool") "AGENTS.md must not document removed auto-populate pool mode"
}

Assert-Contains $doc "AcknowledgeLiveUpstreamRisk" "LOCAL_HARDENED_API.md must show live upstream acknowledgement"
Assert-Contains $doc "AcknowledgeExpandedLiveUpstreamRisk" "LOCAL_HARDENED_API.md must show expanded live upstream acknowledgement"
Assert-True ($doc -notmatch "AutoPopulateProbeAccountPool") "LOCAL_HARDENED_API.md must not document removed auto-populate pool mode"
Assert-True ($doc -notmatch "自动号池") "LOCAL_HARDENED_API.md must not document automatic probe pool population"
Assert-LiveUpstreamDocExamplesRequireAcknowledgement $repoRoot

Assert-Contains $smoke "live_upstream_risk_ack_required" "smoke script must fail closed without live acknowledgement"
Assert-Contains $smoke "expanded_live_upstream_risk_ack_required" "smoke script must fail closed for expanded live risk"
Assert-True ($smoke -notmatch "AutoPopulateProbeAccountPool") "smoke script must not expose removed auto-populate pool switch"
Assert-True ($smoke -notmatch "AutoPopulateProbeMaxRefreshAttempts") "smoke script must not expose removed auto-populate refresh switch"
Assert-Matches $smoke '\[int\]\$AutoDrainRequestIntervalSeconds\s*=\s*22' "smoke script must keep drain interval default at 22 seconds"
Assert-Matches $smoke '\$AutoDrainFirstFreeAccountUntilFallback\s+-and\s+\$AutoDrainRequestIntervalSeconds\s+-lt\s+20' "smoke script must guard drain intervals below 20 seconds"

Assert-Contains $accept "live_upstream_risk_ack_required" "acceptance wrapper must fail closed without live acknowledgement"
Assert-Contains $accept "expanded_live_upstream_risk_ack_required" "acceptance wrapper must fail closed for expanded live risk"
Assert-Contains $accept '"-AcknowledgeLiveUpstreamRisk"' "acceptance wrapper must pass live acknowledgement to smoke script"
Assert-Contains $accept '"-AcknowledgeExpandedLiveUpstreamRisk"' "acceptance wrapper must pass expanded acknowledgement to smoke script"

$smokeBlockedOutput = & pwsh -NoProfile -ExecutionPolicy Bypass -File (Join-Path $PSScriptRoot "smoke-local-hardened-api.ps1") `
  -Stage single `
  -RunUpstreamSmoke 2>$null
Assert-True ($LASTEXITCODE -ne 0) "smoke script should block live upstream smoke without acknowledgement"
$smokeBlocked = Convert-JsonOutput $smokeBlockedOutput "smoke live-risk block"
Assert-True ($smokeBlocked.overall -eq "blocked") "smoke live-risk block should report blocked"
Assert-True ($smokeBlocked.reason -eq "live_upstream_risk_ack_required") "smoke live-risk block should require acknowledgement"

$smokeExpandedBlockedOutput = & pwsh -NoProfile -ExecutionPolicy Bypass -File (Join-Path $PSScriptRoot "smoke-local-hardened-api.ps1") `
  -Stage fallback_probe `
  -AutoDrainFirstFreeAccountUntilFallback `
  -AutoDrainMaxRequests 31 `
  -AcknowledgeLiveUpstreamRisk 2>$null
Assert-True ($LASTEXITCODE -ne 0) "smoke script should block expanded refresh attempts without expanded acknowledgement"
$smokeExpandedBlocked = Convert-JsonOutput $smokeExpandedBlockedOutput "smoke expanded-risk block"
Assert-True ($smokeExpandedBlocked.overall -eq "blocked") "smoke expanded-risk block should report blocked"
Assert-True ($smokeExpandedBlocked.reason -eq "expanded_live_upstream_risk_ack_required") "smoke expanded-risk block should require expanded acknowledgement"

$acceptBlockedOutput = & pwsh -NoProfile -ExecutionPolicy Bypass -File (Join-Path $PSScriptRoot "accept-local-hardened-api-continuity.ps1") 2>$null
Assert-True ($LASTEXITCODE -ne 0) "acceptance wrapper should block without live acknowledgement"
$acceptBlocked = Convert-JsonOutput $acceptBlockedOutput "acceptance live-risk block"
Assert-True ($acceptBlocked.overall -eq "blocked") "acceptance live-risk block should report blocked"
Assert-True ($acceptBlocked.reason -eq "live_upstream_risk_ack_required") "acceptance live-risk block should require acknowledgement"

"PASS local hardened API live-risk guard tests"
