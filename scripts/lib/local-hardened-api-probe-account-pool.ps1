$script:CockpitFreeWeeklyWindowSeconds = 604800

function Get-CockpitStableHashPrefix {
  param([string]$Value)
  $bytes = [System.Text.Encoding]::UTF8.GetBytes($Value)
  $hash = [System.Security.Cryptography.SHA256]::HashData($bytes)
  $hex = [System.BitConverter]::ToString($hash).Replace("-", "").ToLowerInvariant()
  "sha256:{0}" -f $hex.Substring(0, 12)
}

function Test-CockpitTruthyJsonValue {
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

function Get-CockpitUniqueTrimmedStrings {
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

function Get-CockpitJsonProperty {
  param([object]$Object, [string]$Name)
  if ($null -eq $Object) {
    return $null
  }
  if ($Object -is [System.Collections.IDictionary]) {
    if ($Object.Contains($Name)) {
      return $Object[$Name]
    }
    return $null
  }
  $property = $Object.PSObject.Properties[$Name]
  if ($property) {
    return $property.Value
  }
  $null
}

function Get-CockpitJsonString {
  param([object]$Object, [string[]]$Names)
  foreach ($name in $Names) {
    $value = Get-CockpitJsonProperty $Object $name
    if ($null -ne $value) {
      $text = ([string]$value).Trim()
      if ($text) {
        return $text
      }
    }
  }
  $null
}

function Get-CockpitJsonInt {
  param([object]$Object, [string[]]$Names)
  foreach ($name in $Names) {
    $value = Get-CockpitJsonProperty $Object $name
    if ($null -eq $value) {
      continue
    }
    try {
      return [int]$value
    } catch {
    }
  }
  $null
}

function Get-CockpitJsonBool {
  param([object]$Object, [string[]]$Names)
  foreach ($name in $Names) {
    $value = Get-CockpitJsonProperty $Object $name
    if ($null -eq $value) {
      continue
    }
    if ($value -is [bool]) {
      return [bool]$value
    }
    $text = ([string]$value).Trim()
    if ($text -match '^(?i:true|1|yes)$') {
      return $true
    }
    if ($text -match '^(?i:false|0|no)$') {
      return $false
    }
  }
  $null
}

function Get-CockpitCodexAccountIndexPath {
  param([string]$DataRoot)
  Join-Path $DataRoot "codex_accounts.json"
}

function Get-CockpitCodexAccountDetailPath {
  param([string]$DataRoot, [string]$AccountId)
  Join-Path (Join-Path $DataRoot "codex_accounts") ("{0}.json" -f $AccountId)
}

function ConvertTo-CockpitFreeWeeklyQuotaState {
  param(
    [string]$AccountId,
    [object]$Account,
    [object]$RefreshResult
  )

  $usage = $null
  $refreshSource = "cached"
  $refreshStatus = "not_requested"
  $refreshStatusCode = $null
  $refreshError = $null
  $hasRefreshResult = $null -ne $RefreshResult
  if ($RefreshResult) {
    $refreshSource = Get-CockpitJsonString $RefreshResult @("source")
    if (-not $refreshSource) {
      $refreshSource = "refresh"
    }
    $refreshStatus = Get-CockpitJsonString $RefreshResult @("status")
    if (-not $refreshStatus) {
      $refreshStatus = "ok"
    }
    $refreshStatusCode = Get-CockpitJsonInt $RefreshResult @("statusCode", "status_code")
    $refreshError = Get-CockpitJsonString $RefreshResult @("error", "message")
    $usage = Get-CockpitJsonProperty $RefreshResult "usage"
  }
  if ($null -eq $usage) {
    $quota = Get-CockpitJsonProperty $Account "quota"
    $usage = Get-CockpitJsonProperty $quota "raw_data"
  }

  if ($hasRefreshResult -and $refreshStatus -ne "ok") {
    return [ordered]@{
      eligible = $false
      reason = "quota_refresh_failed"
      refreshStatus = $refreshStatus
      refreshStatusCode = $refreshStatusCode
      refreshError = $refreshError
    }
  }

  $planType = Get-CockpitJsonString $usage @("plan_type", "planType")
  if (-not $planType) {
    $planType = Get-CockpitJsonString $RefreshResult @("planType", "plan_type")
  }
  if (-not $planType) {
    $planType = Get-CockpitJsonString $Account @("plan_type", "planType")
  }
  $normalizedPlanType = if ($planType) { $planType.Trim().ToLowerInvariant() } else { "" }
  if ($normalizedPlanType -ne "free") {
    return [ordered]@{
      eligible = $false
      reason = "not_free"
      planType = $normalizedPlanType
      refreshStatus = $refreshStatus
      refreshSource = $refreshSource
    }
  }

  $rateLimit = Get-CockpitJsonProperty $usage "rate_limit"
  if ($null -eq $rateLimit) {
    return [ordered]@{
      eligible = $false
      reason = "quota_rate_limit_missing"
      planType = $normalizedPlanType
      refreshStatus = $refreshStatus
      refreshSource = $refreshSource
    }
  }

  $primary = Get-CockpitJsonProperty $rateLimit "primary_window"
  $secondary = Get-CockpitJsonProperty $rateLimit "secondary_window"
  $primaryWindowSeconds = Get-CockpitJsonInt $primary @("limit_window_seconds", "limitWindowSeconds")
  $usedPercent = Get-CockpitJsonInt $primary @("used_percent", "usedPercent")
  $allowed = Get-CockpitJsonBool $rateLimit @("allowed")
  $limitReached = Get-CockpitJsonBool $rateLimit @("limit_reached", "limitReached")
  $weeklyOnly = ($primaryWindowSeconds -eq $script:CockpitFreeWeeklyWindowSeconds -and $null -eq $secondary)

  if (-not $weeklyOnly) {
    return [ordered]@{
      eligible = $false
      reason = "not_free_weekly_only"
      planType = $normalizedPlanType
      primaryWindowSeconds = $primaryWindowSeconds
      hasSecondaryWindow = ($null -ne $secondary)
      refreshStatus = $refreshStatus
      refreshSource = $refreshSource
    }
  }

  if ($null -eq $usedPercent) {
    return [ordered]@{
      eligible = $false
      reason = "quota_used_percent_missing"
      planType = $normalizedPlanType
      primaryWindowSeconds = $primaryWindowSeconds
      refreshStatus = $refreshStatus
      refreshSource = $refreshSource
    }
  }

  $remainingPercent = (100 - [Math]::Max(0, [Math]::Min(100, $usedPercent)))
  $isExhausted = ($allowed -eq $false -or $limitReached -eq $true -or $remainingPercent -le 0)
  $isAvailable = ($allowed -ne $false -and $limitReached -ne $true -and $remainingPercent -gt 0)
  $role = if ($isExhausted) {
    "exhausted"
  } elseif ($isAvailable) {
    "available"
  } else {
    "unknown"
  }

  [ordered]@{
    eligible = ($role -ne "unknown")
    reason = if ($role -eq "unknown") { "quota_state_unknown" } else { $null }
    accountHash = Get-CockpitStableHashPrefix $AccountId
    role = $role
    planType = $normalizedPlanType
    quotaKind = "free_weekly_primary"
    weeklyRemainingPercent = $remainingPercent
    weeklyUsedPercent = $usedPercent
    primaryWindowSeconds = $primaryWindowSeconds
    allowed = $allowed
    limitReached = $limitReached
    refreshStatus = $refreshStatus
    refreshSource = $refreshSource
    refreshStatusCode = $refreshStatusCode
  }
}

function Get-CockpitProbeCandidateRoleBucket {
  param(
    [System.Collections.IDictionary]$Candidate,
    [string]$Role
  )

  $cachedRole = Get-CockpitJsonString $Candidate @("cachedRole")
  if ($Role -eq "drain_source") {
    if ($cachedRole -eq "exhausted") {
      return 0
    }
    if ($cachedRole -eq "available") {
      return 1
    }
    return 2
  }
  if ($cachedRole -eq $Role) {
    return 0
  }
  if ($cachedRole -eq "unknown") {
    return 1
  }
  2
}

function Get-CockpitProbeCandidateQuotaSortValue {
  param(
    [System.Collections.IDictionary]$Candidate,
    [string]$Role
  )

  $remaining = Get-CockpitJsonInt $Candidate @("cachedWeeklyRemainingPercent")
  if ($null -eq $remaining) {
    return [int]::MaxValue
  }
  if ($Role -eq "available") {
    return -1 * $remaining
  }
  $remaining
}

function Sort-CockpitProbeCandidatesForRole {
  param(
    [object[]]$Candidates,
    [string]$Role
  )

  @($Candidates | Sort-Object `
    @{ Expression = { Get-CockpitProbeCandidateRoleBucket -Candidate $_ -Role $Role }; Ascending = $true },
    @{ Expression = { Get-CockpitProbeCandidateQuotaSortValue -Candidate $_ -Role $Role }; Ascending = $true },
    @{ Expression = { Get-CockpitJsonInt $_ @("order") }; Ascending = $true })
}

function New-CockpitProbeQuotaSkip {
  param(
    [string]$AccountId,
    [System.Collections.IDictionary]$QuotaState
  )

  $skip = [ordered]@{
    accountHash = Get-CockpitStableHashPrefix $AccountId
    reason = $QuotaState.reason
  }
  foreach ($key in @("planType", "primaryWindowSeconds", "hasSecondaryWindow", "refreshStatus", "refreshStatusCode")) {
    if ($QuotaState.Contains($key) -and $null -ne $QuotaState[$key]) {
      $skip[$key] = $QuotaState[$key]
    }
  }
  $skip
}

function Invoke-CockpitProbeQuotaCandidateRefresh {
  param(
    [System.Collections.IDictionary]$Candidate,
    [scriptblock]$RefreshQuotaScript
  )

  $accountId = Get-CockpitJsonString $Candidate @("accountId")
  $account = Get-CockpitJsonProperty $Candidate "account"
  $refreshResult = $null
  if ($RefreshQuotaScript) {
    try {
      $refreshResult = & $RefreshQuotaScript $account
    } catch {
      $refreshResult = [ordered]@{
        status = "error"
        source = "wham_usage_refresh"
        error = $_.Exception.Message
      }
    }
  }

  $quotaState = ConvertTo-CockpitFreeWeeklyQuotaState -AccountId $accountId -Account $account -RefreshResult $refreshResult
  if (-not $quotaState.eligible) {
    return [ordered]@{
      eligible = $false
      skip = (New-CockpitProbeQuotaSkip -AccountId $accountId -QuotaState $quotaState)
    }
  }

  [ordered]@{
    eligible = $true
    candidate = [ordered]@{
      accountId = $accountId
      accountHash = $quotaState.accountHash
      role = $quotaState.role
      planType = $quotaState.planType
      quotaKind = $quotaState.quotaKind
      weeklyRemainingPercent = $quotaState.weeklyRemainingPercent
      weeklyUsedPercent = $quotaState.weeklyUsedPercent
      primaryWindowSeconds = $quotaState.primaryWindowSeconds
      allowed = $quotaState.allowed
      limitReached = $quotaState.limitReached
      refreshStatus = $quotaState.refreshStatus
      refreshSource = $quotaState.refreshSource
      refreshStatusCode = $quotaState.refreshStatusCode
    }
  }
}

function Resolve-CockpitProbeOAuthAccountPool {
  param(
    [object[]]$ExistingAccountIds,
    [int]$RequiredCount = 2,
    [string]$DataRoot,
    [scriptblock]$RefreshQuotaScript,
    [int]$MaxRefreshAttempts = 2,
    [switch]$AllowFirstAccountDrain
  )

  if ($RequiredCount -ne 2) {
    throw "fallback_probe 自动号池验收需要 exactly 2 个账号：第一个 exhausted free OAuth，第二个 available free OAuth"
  }
  if ($MaxRefreshAttempts -lt $RequiredCount) {
    throw "fallback_probe 自动号池验收的 MaxRefreshAttempts 不能小于 RequiredCount=$RequiredCount"
  }

  $indexPath = Get-CockpitCodexAccountIndexPath $DataRoot
  if (-not (Test-Path -LiteralPath $indexPath)) {
    throw "无法自动补齐 probe 号池：live codex_accounts.json 不存在"
  }

  $index = Get-Content -LiteralPath $indexPath -Raw | ConvertFrom-Json
  $existingIds = Get-CockpitUniqueTrimmedStrings @($ExistingAccountIds)
  $existingIdSet = @{}
  foreach ($existingId in $existingIds) {
    $existingIdSet[$existingId] = $true
  }

  $orderedIds = @()
  $orderedIds += @($existingIds)
  if ($index.current_account_id) {
    $orderedIds += $index.current_account_id
  }
  if ($index.accounts) {
    $orderedIds += @($index.accounts | ForEach-Object { $_.id })
  }
  $orderedIds = Get-CockpitUniqueTrimmedStrings $orderedIds

  $eligibleByRole = @{
    exhausted = @()
    available = @()
  }
  $skipped = @()
  $refreshAttempted = $null -ne $RefreshQuotaScript
  $candidates = @()
  $order = 0

  foreach ($accountId in $orderedIds) {
    $order++
    $detailPath = Get-CockpitCodexAccountDetailPath $DataRoot $accountId
    if (-not (Test-Path -LiteralPath $detailPath)) {
      $skipped += [ordered]@{ accountHash = Get-CockpitStableHashPrefix $accountId; reason = "detail_missing" }
      continue
    }

    try {
      $account = Get-Content -LiteralPath $detailPath -Raw | ConvertFrom-Json
    } catch {
      $skipped += [ordered]@{ accountHash = Get-CockpitStableHashPrefix $accountId; reason = "detail_invalid_json" }
      continue
    }

    $authMode = Get-CockpitJsonString $account @("auth_mode", "authMode")
    $authMode = if ($authMode) { $authMode.ToLowerInvariant() } else { "" }
    if ($authMode -ne "oauth") {
      $skipped += [ordered]@{ accountHash = Get-CockpitStableHashPrefix $accountId; reason = "not_oauth" }
      continue
    }
    if (Test-CockpitTruthyJsonValue (Get-CockpitJsonProperty $account "requires_reauth")) {
      $skipped += [ordered]@{ accountHash = Get-CockpitStableHashPrefix $accountId; reason = "requires_reauth" }
      continue
    }
    if (Test-CockpitTruthyJsonValue (Get-CockpitJsonProperty $account "disabled")) {
      $skipped += [ordered]@{ accountHash = Get-CockpitStableHashPrefix $accountId; reason = "disabled" }
      continue
    }

    $declaredPlanType = Get-CockpitJsonString $account @("plan_type", "planType")
    if ($declaredPlanType -and $declaredPlanType.Trim().ToLowerInvariant() -ne "free") {
      $skipped += [ordered]@{
        accountHash = Get-CockpitStableHashPrefix $accountId
        reason = "not_free"
        planType = $declaredPlanType.Trim().ToLowerInvariant()
      }
      continue
    }

    $cachedQuotaState = ConvertTo-CockpitFreeWeeklyQuotaState -AccountId $accountId -Account $account -RefreshResult $null
    if (-not $cachedQuotaState.eligible -and $cachedQuotaState.reason -in @("not_free", "not_free_weekly_only")) {
      $skipped += (New-CockpitProbeQuotaSkip -AccountId $accountId -QuotaState $cachedQuotaState)
      continue
    }

    $candidates += [ordered]@{
      accountId = $accountId
      accountHash = Get-CockpitStableHashPrefix $accountId
      account = $account
      isExistingPoolAccount = $existingIdSet.ContainsKey($accountId)
      order = $order
      cachedRole = if ($cachedQuotaState.eligible) { $cachedQuotaState.role } else { "unknown" }
      cachedWeeklyRemainingPercent = if ($cachedQuotaState.eligible) { $cachedQuotaState.weeklyRemainingPercent } else { $null }
      cachedWeeklyUsedPercent = if ($cachedQuotaState.eligible) { $cachedQuotaState.weeklyUsedPercent } else { $null }
      cachedReason = if ($cachedQuotaState.eligible) { $null } else { $cachedQuotaState.reason }
    }
  }

  $refreshedById = @{}
  $acceptedById = @{}
  $refreshAttemptCount = 0
  $refreshLimitReached = $false
  $selected = $null
  $drainRequired = $false
  $selectionMode = if ($AllowFirstAccountDrain) {
    "existing_pool_then_cached_quota_drain_source_priority"
  } else {
    "existing_pool_then_cached_quota_acceptance_priority"
  }

  if ($AllowFirstAccountDrain) {
    $scopeSets = @(
      @($candidates | Where-Object { $_.isExistingPoolAccount }),
      @($candidates | Where-Object { -not $_.isExistingPoolAccount })
    )

    $source = $null
    foreach ($scopeCandidates in $scopeSets) {
      foreach ($candidate in (Sort-CockpitProbeCandidatesForRole -Candidates $scopeCandidates -Role "drain_source")) {
        $candidateId = Get-CockpitJsonString $candidate @("accountId")
        if (-not $candidateId) {
          continue
        }
        if (-not $refreshedById.ContainsKey($candidateId)) {
          if ($refreshAttemptCount -ge $MaxRefreshAttempts) {
            $refreshLimitReached = $true
            break
          }
          $refreshAttemptCount++
          $refreshOutcome = Invoke-CockpitProbeQuotaCandidateRefresh -Candidate $candidate -RefreshQuotaScript $RefreshQuotaScript
          $refreshedById[$candidateId] = $refreshOutcome
          if (-not $refreshOutcome.eligible) {
            $skipped += $refreshOutcome.skip
          }
        } else {
          $refreshOutcome = $refreshedById[$candidateId]
        }
        if (-not $refreshOutcome.eligible) {
          continue
        }
        $role = Get-CockpitJsonString $refreshOutcome.candidate @("role")
        if ($role -in @("exhausted", "available")) {
          $source = $refreshOutcome.candidate
          $acceptedById[$candidateId] = $true
          break
        }
      }
      if ($source -or $refreshLimitReached) {
        break
      }
    }

    $fallback = $null
    if ($source -and -not $refreshLimitReached) {
      foreach ($scopeCandidates in $scopeSets) {
        foreach ($candidate in (Sort-CockpitProbeCandidatesForRole -Candidates $scopeCandidates -Role "available")) {
          $candidateId = Get-CockpitJsonString $candidate @("accountId")
          if (-not $candidateId -or $acceptedById.ContainsKey($candidateId)) {
            continue
          }
          if (-not $refreshedById.ContainsKey($candidateId)) {
            if ($refreshAttemptCount -ge $MaxRefreshAttempts) {
              $refreshLimitReached = $true
              break
            }
            $refreshAttemptCount++
            $refreshOutcome = Invoke-CockpitProbeQuotaCandidateRefresh -Candidate $candidate -RefreshQuotaScript $RefreshQuotaScript
            $refreshedById[$candidateId] = $refreshOutcome
            if (-not $refreshOutcome.eligible) {
              $skipped += $refreshOutcome.skip
            }
          } else {
            $refreshOutcome = $refreshedById[$candidateId]
          }
          if (-not $refreshOutcome.eligible) {
            continue
          }
          $role = Get-CockpitJsonString $refreshOutcome.candidate @("role")
          if ($role -eq "available") {
            $fallback = $refreshOutcome.candidate
            $acceptedById[$candidateId] = $true
            break
          }
        }
        if ($fallback -or $refreshLimitReached) {
          break
        }
      }
    }

    if ($source -and $fallback) {
      $selected = @($source, $fallback)
      $drainRequired = ((Get-CockpitJsonString $source @("role")) -ne "exhausted")
    }
  }

  if (-not $AllowFirstAccountDrain) {
    foreach ($scope in @("existing_pool", "remaining_cached_priority")) {
    if (@($eligibleByRole.exhausted).Count -ge 1 -and @($eligibleByRole.available).Count -ge 1) {
      break
    }

    $scopeCandidates = if ($scope -eq "existing_pool") {
      @($candidates | Where-Object { $_.isExistingPoolAccount })
    } else {
      @($candidates | Where-Object { -not $_.isExistingPoolAccount })
    }

    foreach ($role in @("exhausted", "available")) {
      if (@($eligibleByRole[$role]).Count -ge 1) {
        continue
      }

      $roleCandidates = Sort-CockpitProbeCandidatesForRole -Candidates $scopeCandidates -Role $role
      foreach ($candidate in $roleCandidates) {
        $candidateId = Get-CockpitJsonString $candidate @("accountId")
        if (-not $candidateId -or $acceptedById.ContainsKey($candidateId)) {
          continue
        }

        if ($refreshedById.ContainsKey($candidateId)) {
          $refreshOutcome = $refreshedById[$candidateId]
        } else {
          if ($refreshAttemptCount -ge $MaxRefreshAttempts) {
            $refreshLimitReached = $true
            break
          }
          $refreshAttemptCount++
          $refreshOutcome = Invoke-CockpitProbeQuotaCandidateRefresh -Candidate $candidate -RefreshQuotaScript $RefreshQuotaScript
          $refreshedById[$candidateId] = $refreshOutcome
          if (-not $refreshOutcome.eligible) {
            $skipped += $refreshOutcome.skip
          }
        }

        if (-not $refreshOutcome.eligible) {
          continue
        }

        $refreshedCandidate = $refreshOutcome.candidate
        $refreshedRole = Get-CockpitJsonString $refreshedCandidate @("role")
        if (-not $acceptedById.ContainsKey($candidateId) -and @($eligibleByRole[$refreshedRole]).Count -lt 1) {
          $eligibleByRole[$refreshedRole] += $refreshedCandidate
          $acceptedById[$candidateId] = $true
        }

        if (@($eligibleByRole[$role]).Count -ge 1) {
          break
        }
      }
      if ($refreshLimitReached) {
        break
      }
    }
    if ($refreshLimitReached) {
      break
    }
  }
  }

  if ($AllowFirstAccountDrain) {
    if (-not $selected) {
      throw "无法自动补齐 drain probe 号池：需要第 1 个 free weekly OAuth 账号和第 2 个 available free weekly OAuth 账号；refreshAttemptCount=$refreshAttemptCount，maxRefreshAttempts=$MaxRefreshAttempts"
    }
  } else {
    if (@($eligibleByRole.exhausted).Count -lt 1) {
      throw "无法自动补齐 probe 号池：需要 1 个 exhausted free weekly OAuth 账号，当前 exhausted=0；refreshAttemptCount=$refreshAttemptCount，maxRefreshAttempts=$MaxRefreshAttempts"
    }
    if (@($eligibleByRole.available).Count -lt 1) {
      throw "无法自动补齐 probe 号池：需要 1 个 available free weekly OAuth 账号，当前 available=0；refreshAttemptCount=$refreshAttemptCount，maxRefreshAttempts=$MaxRefreshAttempts"
    }
    $selected = @($eligibleByRole.exhausted[0], $eligibleByRole.available[0])
  }

  $existingCandidateCount = @($candidates | Where-Object { $_.isExistingPoolAccount }).Count
  $cachedKnownCandidateCount = @($candidates | Where-Object { $_.cachedRole -in @("exhausted", "available") }).Count
  [ordered]@{
    requested = $true
    status = "applied"
    mode = "live_free_oauth_weekly_quota_to_isolated_probe_pool"
    selectionOrder = $selectionMode
    requiredCount = $RequiredCount
    selectedCount = $selected.Count
    allowFirstAccountDrain = [bool]$AllowFirstAccountDrain
    drainRequired = [bool]$drainRequired
    maxRefreshAttempts = $MaxRefreshAttempts
    refreshAttemptCount = $refreshAttemptCount
    refreshLimitReached = $refreshLimitReached
    candidateCount = @($candidates).Count
    existingCandidateCount = $existingCandidateCount
    cachedKnownCandidateCount = $cachedKnownCandidateCount
    selectedAccountHashes = @($selected | ForEach-Object { $_.accountHash })
    selectedAccountRoles = @($selected | ForEach-Object {
      [ordered]@{
        accountHash = $_.accountHash
        role = $_.role
        planType = $_.planType
        quotaKind = $_.quotaKind
        weeklyRemainingPercent = $_.weeklyRemainingPercent
        weeklyUsedPercent = $_.weeklyUsedPercent
        primaryWindowSeconds = $_.primaryWindowSeconds
        allowed = $_.allowed
        limitReached = $_.limitReached
        refreshStatus = $_.refreshStatus
        refreshSource = $_.refreshSource
        refreshStatusCode = $_.refreshStatusCode
      }
    })
    skipped = @($skipped)
    sourceIndexPath = $indexPath
    refreshAttempted = $refreshAttempted
    writesLivePool = $false
    writesIsolatedPool = $true
    accountIds = @($selected | ForEach-Object { $_.accountId })
  }
}
