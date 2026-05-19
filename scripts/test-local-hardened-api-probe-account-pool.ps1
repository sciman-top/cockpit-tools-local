param(
  [switch]$KeepTemp
)

$ErrorActionPreference = "Stop"

$repoRoot = Split-Path -Parent $PSScriptRoot
$libraryPath = Join-Path $PSScriptRoot "lib\local-hardened-api-probe-account-pool.ps1"
. $libraryPath

function Assert-True {
  param([bool]$Condition, [string]$Message)
  if (-not $Condition) {
    throw $Message
  }
}

function Assert-Equal {
  param([object]$Actual, [object]$Expected, [string]$Message)
  if ($Actual -ne $Expected) {
    throw "$Message; expected=[$Expected], actual=[$Actual]"
  }
}

function New-UsagePayload {
  param(
    [string]$PlanType = "free",
    [int]$UsedPercent = 3,
    [bool]$Allowed = $true,
    [bool]$LimitReached = $false,
    [int]$PrimaryWindowSeconds = 604800,
    [object]$SecondaryWindow = $null
  )

  $rateLimit = [ordered]@{
    allowed = $Allowed
    limit_reached = $LimitReached
    primary_window = [ordered]@{
      used_percent = $UsedPercent
      limit_window_seconds = $PrimaryWindowSeconds
      reset_after_seconds = 3600
    }
  }
  if ($null -ne $SecondaryWindow) {
    $rateLimit.secondary_window = $SecondaryWindow
  }

  [ordered]@{
    plan_type = $PlanType
    rate_limit = $rateLimit
  }
}

function New-TestAccount {
  param(
    [string]$DataRoot,
    [string]$Id,
    [string]$AuthMode = "oauth",
    [string]$PlanType = "free",
    [object]$UsagePayload,
    [bool]$RequiresReauth = $false,
    [bool]$Disabled = $false
  )

  $detailRoot = Join-Path $DataRoot "codex_accounts"
  New-Item -ItemType Directory -Force -Path $detailRoot | Out-Null
  $account = [ordered]@{
    id = $Id
    email = "$Id@example.invalid"
    auth_mode = $AuthMode
    plan_type = $PlanType
    account_id = "chatgpt-$Id"
    requires_reauth = $RequiresReauth
    disabled = $Disabled
    tokens = [ordered]@{
      access_token = "access-$Id"
      id_token = "id-$Id"
      refresh_token = "refresh-$Id"
    }
    quota = [ordered]@{
      raw_data = $UsagePayload
    }
    _test_usage = $UsagePayload
  }
  $account | ConvertTo-Json -Depth 20 | Set-Content -LiteralPath (Join-Path $detailRoot "$Id.json") -Encoding UTF8
}

function New-TestIndex {
  param(
    [string]$DataRoot,
    [string[]]$Ids,
    [string]$CurrentId = $null
  )

  $index = [ordered]@{
    version = "1.0"
    current_account_id = $CurrentId
    accounts = @($Ids | ForEach-Object {
      [ordered]@{
        id = $_
        email = "$_@example.invalid"
        plan_type = $null
        created_at = 1
        last_used = 1
      }
    })
  }
  $index | ConvertTo-Json -Depth 20 | Set-Content -LiteralPath (Join-Path $DataRoot "codex_accounts.json") -Encoding UTF8
}

function Invoke-TestQuotaRefresh {
  param([object]$Account)
  $script:QuotaRefreshCallCount++
  [ordered]@{
    status = "ok"
    source = "fixture"
    usage = $Account._test_usage
  }
}

$tempRoot = Join-Path ([System.IO.Path]::GetTempPath()) ("cockpit-hla-pool-test-{0}-{1}" -f $PID, (Get-Date -Format "yyyyMMddHHmmssfff"))
New-Item -ItemType Directory -Force -Path $tempRoot | Out-Null

try {
  New-TestAccount -DataRoot $tempRoot -Id "api-key" -AuthMode "apikey" -PlanType "API_KEY" -UsagePayload (New-UsagePayload)
  New-TestAccount -DataRoot $tempRoot -Id "plus-oauth" -AuthMode "oauth" -PlanType "plus" -UsagePayload (New-UsagePayload -PlanType "plus" -UsedPercent 1 -PrimaryWindowSeconds 300 -SecondaryWindow ([ordered]@{ used_percent = 10; limit_window_seconds = 604800 }))
  New-TestAccount -DataRoot $tempRoot -Id "free-available" -AuthMode "oauth" -PlanType "free" -UsagePayload (New-UsagePayload -PlanType "free" -UsedPercent 3)
  New-TestAccount -DataRoot $tempRoot -Id "free-exhausted" -AuthMode "oauth" -PlanType "free" -UsagePayload (New-UsagePayload -PlanType "free" -UsedPercent 100 -Allowed $false -LimitReached $true)
  for ($i = 0; $i -lt 40; $i++) {
    New-TestAccount -DataRoot $tempRoot -Id ("trailing-{0}" -f $i) -AuthMode "oauth" -PlanType "free" -UsagePayload (New-UsagePayload -PlanType "free" -UsedPercent 3)
  }
  New-TestIndex -DataRoot $tempRoot -Ids (@("api-key", "plus-oauth", "free-available") + @(0..39 | ForEach-Object { "trailing-$_" }) + @("free-exhausted")) -CurrentId "plus-oauth"

  $script:QuotaRefreshCallCount = 0
  $pool = Resolve-CockpitProbeOAuthAccountPool `
    -ExistingAccountIds @("plus-oauth") `
    -RequiredCount 2 `
    -DataRoot $tempRoot `
    -RefreshQuotaScript ${function:Invoke-TestQuotaRefresh} `
    -MaxRefreshAttempts 2

  Assert-Equal $pool.accountIds[0] "free-exhausted" "expected exhausted free weekly account first"
  Assert-Equal $pool.accountIds[1] "free-available" "expected available free weekly account second"
  Assert-Equal $pool.selectedAccountRoles[0].role "exhausted" "expected first role"
  Assert-Equal $pool.selectedAccountRoles[1].role "available" "expected second role"
  Assert-Equal $pool.selectedAccountRoles[0].quotaKind "free_weekly_primary" "expected free weekly quota kind"
  Assert-Equal $pool.selectionOrder "existing_pool_then_cached_quota_acceptance_priority" "expected cached priority selection order"
  Assert-Equal $pool.maxRefreshAttempts 2 "expected max refresh attempts"
  Assert-Equal $pool.refreshAttemptCount 2 "expected exactly selected pair to be refreshed"
  Assert-True ([bool](@($pool.skipped | Where-Object { $_.reason -eq "not_oauth" }).Count)) "expected API key account to be skipped"
  Assert-True ([bool](@($pool.skipped | Where-Object { $_.reason -eq "not_free" }).Count)) "expected plus account to be skipped"
  Assert-True ($script:QuotaRefreshCallCount -le 2) "expected cached quota pre-sort to refresh only the exhausted/available pair"

  $existingRoot = Join-Path $tempRoot "existing"
  New-Item -ItemType Directory -Force -Path $existingRoot | Out-Null
  New-TestAccount -DataRoot $existingRoot -Id "pool-available" -AuthMode "oauth" -PlanType "free" -UsagePayload (New-UsagePayload -PlanType "free" -UsedPercent 12)
  New-TestAccount -DataRoot $existingRoot -Id "pool-exhausted" -AuthMode "oauth" -PlanType "free" -UsagePayload (New-UsagePayload -PlanType "free" -UsedPercent 100 -Allowed $false -LimitReached $true)
  New-TestAccount -DataRoot $existingRoot -Id "other-exhausted" -AuthMode "oauth" -PlanType "free" -UsagePayload (New-UsagePayload -PlanType "free" -UsedPercent 100 -Allowed $false -LimitReached $true)
  New-TestAccount -DataRoot $existingRoot -Id "other-available" -AuthMode "oauth" -PlanType "free" -UsagePayload (New-UsagePayload -PlanType "free" -UsedPercent 1)
  New-TestIndex -DataRoot $existingRoot -Ids @("other-exhausted", "other-available", "pool-available", "pool-exhausted")

  $script:QuotaRefreshCallCount = 0
  $existingPool = Resolve-CockpitProbeOAuthAccountPool `
    -ExistingAccountIds @("pool-available", "pool-exhausted") `
    -RequiredCount 2 `
    -DataRoot $existingRoot `
    -RefreshQuotaScript ${function:Invoke-TestQuotaRefresh} `
    -MaxRefreshAttempts 2

  Assert-Equal $existingPool.accountIds[0] "pool-exhausted" "expected existing exhausted account first"
  Assert-Equal $existingPool.accountIds[1] "pool-available" "expected existing available account second"
  Assert-True ($script:QuotaRefreshCallCount -le 2) "expected satisfied existing pool to avoid refreshing outside accounts"

  $drainRoot = Join-Path $tempRoot "drain"
  New-Item -ItemType Directory -Force -Path $drainRoot | Out-Null
  New-TestAccount -DataRoot $drainRoot -Id "free-high" -AuthMode "oauth" -PlanType "free" -UsagePayload (New-UsagePayload -PlanType "free" -UsedPercent 5)
  New-TestAccount -DataRoot $drainRoot -Id "free-low" -AuthMode "oauth" -PlanType "free" -UsagePayload (New-UsagePayload -PlanType "free" -UsedPercent 90)
  New-TestIndex -DataRoot $drainRoot -Ids @("free-high", "free-low")

  $script:QuotaRefreshCallCount = 0
  $drainPool = Resolve-CockpitProbeOAuthAccountPool `
    -ExistingAccountIds @() `
    -RequiredCount 2 `
    -DataRoot $drainRoot `
    -RefreshQuotaScript ${function:Invoke-TestQuotaRefresh} `
    -MaxRefreshAttempts 2 `
    -AllowFirstAccountDrain

  Assert-Equal $drainPool.accountIds[0] "free-low" "expected drain source to prefer low remaining free account"
  Assert-Equal $drainPool.accountIds[1] "free-high" "expected fallback to prefer high remaining free account"
  Assert-Equal $drainPool.selectedAccountRoles[0].role "available" "expected drain source can be available"
  Assert-Equal $drainPool.selectedAccountRoles[1].role "available" "expected fallback role"
  Assert-Equal $drainPool.selectionOrder "existing_pool_then_cached_quota_drain_source_priority" "expected drain selection order"
  Assert-True ([bool]$drainPool.allowFirstAccountDrain) "expected drain mode flag"
  Assert-True ([bool]$drainPool.drainRequired) "expected drain to be required when source is not exhausted"
  Assert-Equal $drainPool.refreshAttemptCount 2 "expected drain pool to refresh only selected pair"

  $badRoot = Join-Path $tempRoot "bad"
  New-Item -ItemType Directory -Force -Path $badRoot | Out-Null
  New-TestAccount -DataRoot $badRoot -Id "free-5h-like" -AuthMode "oauth" -PlanType "free" -UsagePayload (New-UsagePayload -PlanType "free" -UsedPercent 0 -PrimaryWindowSeconds 300 -SecondaryWindow ([ordered]@{ used_percent = 0; limit_window_seconds = 604800 }))
  New-TestAccount -DataRoot $badRoot -Id "free-available" -AuthMode "oauth" -PlanType "free" -UsagePayload (New-UsagePayload -PlanType "free" -UsedPercent 3)
  New-TestIndex -DataRoot $badRoot -Ids @("free-5h-like", "free-available")

  $threw = $false
  try {
    Resolve-CockpitProbeOAuthAccountPool `
      -ExistingAccountIds @() `
      -RequiredCount 2 `
      -DataRoot $badRoot `
      -RefreshQuotaScript ${function:Invoke-TestQuotaRefresh} `
      -MaxRefreshAttempts 2 | Out-Null
  } catch {
    $threw = $true
    Assert-True ($_.Exception.Message -match "exhausted") "expected missing exhausted role in error"
  }
  Assert-True $threw "expected selector to reject missing exhausted free weekly account"

  "PASS local hardened API probe account pool tests"
} finally {
  if (-not $KeepTemp -and (Test-Path -LiteralPath $tempRoot)) {
    Remove-Item -LiteralPath $tempRoot -Recurse -Force
  }
}
