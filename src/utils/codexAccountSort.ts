import type { CodexAccount } from "../types/codex";
import {
  getCodexEffectiveQuotaPercentages,
  getCodexPlanFilterKey,
  isCodexApiKeyAccount,
  isCodexNewApiAccount,
} from "../types/codex";

export const CODEX_RECOMMENDED_SORT_BY = "recommended";

export type CodexNumericSortDirection = "asc" | "desc";

export type CodexGroupSortMeta = {
  sortOrder: number;
  accountIndex: number;
};

type CodexQuotaAvailabilityRank = {
  bottleneck: number;
  total: number;
  hourly: number | null;
  weekly: number | null;
};

export interface CodexAccountSortOptions {
  sortBy: string;
  sortDirection: CodexNumericSortDirection;
  apiServiceSortMeta?: Map<string, number>;
  groupSortMeta?: Map<string, CodexGroupSortMeta>;
  currentAccountId?: string | null;
  getSubscriptionTimestampMs?: (account: CodexAccount) => number | null | undefined;
}

function toNullableSortNumber(value: number | null | undefined): number | null {
  return typeof value === "number" && Number.isFinite(value) ? value : null;
}

function toNullablePositiveSortNumber(
  value: number | null | undefined,
): number | null {
  const normalized = toNullableSortNumber(value);
  return normalized != null && normalized > 0 ? normalized : null;
}

export function compareNullableSortNumber(
  left: number | null | undefined,
  right: number | null | undefined,
  direction: CodexNumericSortDirection,
): number {
  const leftValue = toNullableSortNumber(left);
  const rightValue = toNullableSortNumber(right);
  if (leftValue == null && rightValue == null) return 0;
  if (leftValue == null) return 1;
  if (rightValue == null) return -1;
  const diff =
    direction === "desc" ? rightValue - leftValue : leftValue - rightValue;
  return diff === 0 ? 0 : diff;
}

export function compareCodexCurrentAccountFirst(
  left: CodexAccount,
  right: CodexAccount,
  currentAccountId: string | null | undefined,
): number {
  const normalizedCurrentId = currentAccountId?.trim();
  if (!normalizedCurrentId) return 0;

  const leftIsCurrent = left.id === normalizedCurrentId;
  const rightIsCurrent = right.id === normalizedCurrentId;
  if (leftIsCurrent === rightIsCurrent) return 0;
  return leftIsCurrent ? -1 : 1;
}

export function compareCodexAccountTieBreak(
  left: CodexAccount,
  right: CodexAccount,
  direction: CodexNumericSortDirection = "desc",
): number {
  const createdDiff =
    direction === "desc"
      ? right.created_at - left.created_at
      : left.created_at - right.created_at;
  if (createdDiff !== 0) return createdDiff;
  return direction === "desc"
    ? right.id.localeCompare(left.id)
    : left.id.localeCompare(right.id);
}

function compareCodexAccountCreatedAt(
  left: CodexAccount,
  right: CodexAccount,
  direction: CodexNumericSortDirection,
): number {
  const diff =
    direction === "desc"
      ? right.created_at - left.created_at
      : left.created_at - right.created_at;
  return diff !== 0 ? diff : left.id.localeCompare(right.id);
}

function getCodexQuotaAvailabilityRank(
  account: CodexAccount,
): CodexQuotaAvailabilityRank | null {
  if (isCodexApiKeyAccount(account) && !isCodexNewApiAccount(account)) {
    return null;
  }

  const percentages = getCodexEffectiveQuotaPercentages(account.quota);
  const values = [percentages.hourly, percentages.weekly].filter(
    (value): value is number => typeof value === "number" && Number.isFinite(value),
  );
  if (values.length === 0) return null;

  return {
    bottleneck: Math.min(...values),
    total: values.reduce((sum, value) => sum + value, 0),
    hourly: percentages.hourly,
    weekly: percentages.weekly,
  };
}

export function getCodexAccountQuotaAvailabilityScore(
  account: CodexAccount,
): number | null {
  return getCodexQuotaAvailabilityRank(account)?.bottleneck ?? null;
}

export function compareCodexAccountsByQuotaAvailability(
  left: CodexAccount,
  right: CodexAccount,
  direction: CodexNumericSortDirection = "desc",
): number {
  const leftRank = getCodexQuotaAvailabilityRank(left);
  const rightRank = getCodexQuotaAvailabilityRank(right);
  if (!leftRank && !rightRank) return 0;
  if (!leftRank) return 1;
  if (!rightRank) return -1;

  const bottleneckDiff = compareNullableSortNumber(
    leftRank.bottleneck,
    rightRank.bottleneck,
    direction,
  );
  if (bottleneckDiff !== 0) return bottleneckDiff;

  const totalDiff = compareNullableSortNumber(
    leftRank.total,
    rightRank.total,
    direction,
  );
  if (totalDiff !== 0) return totalDiff;

  const weeklyDiff = compareNullableSortNumber(
    leftRank.weekly,
    rightRank.weekly,
    direction,
  );
  if (weeklyDiff !== 0) return weeklyDiff;

  return compareNullableSortNumber(leftRank.hourly, rightRank.hourly, direction);
}

function getCodexAccountRecommendedSortBucket(
  account: CodexAccount,
  apiServiceSortMeta: Map<string, number>,
  groupSortMeta: Map<string, CodexGroupSortMeta>,
  currentAccountId: string | null | undefined,
): number {
  if (apiServiceSortMeta.has(account.id) || isCodexNewApiAccount(account)) {
    return 0;
  }
  if (groupSortMeta.has(account.id)) return 1;
  if (currentAccountId === account.id) return 2;
  if (isCodexApiKeyAccount(account)) return 3;

  const planKey = getCodexPlanFilterKey(account).toLowerCase();
  const hasUsableQuota = (getCodexAccountQuotaAvailabilityScore(account) ?? 0) > 0;
  if ((planKey === "pro" || planKey === "plus") && hasUsableQuota) {
    return 4;
  }
  if (planKey === "free") return 5;
  return 6;
}

function compareCodexAccountTopSortPriority(
  left: CodexAccount,
  right: CodexAccount,
  apiServiceSortMeta: Map<string, number>,
  groupSortMeta: Map<string, CodexGroupSortMeta>,
  currentAccountId: string | null | undefined,
): number {
  const leftBucket = getCodexAccountRecommendedSortBucket(
    left,
    apiServiceSortMeta,
    groupSortMeta,
    currentAccountId,
  );
  const rightBucket = getCodexAccountRecommendedSortBucket(
    right,
    apiServiceSortMeta,
    groupSortMeta,
    currentAccountId,
  );
  const leftTopBucket = leftBucket <= 2 ? leftBucket : 3;
  const rightTopBucket = rightBucket <= 2 ? rightBucket : 3;
  if (leftTopBucket !== rightTopBucket) {
    return leftTopBucket - rightTopBucket;
  }
  if (leftTopBucket <= 2) {
    return compareCodexCurrentAccountFirst(left, right, currentAccountId);
  }
  return 0;
}

function compareCodexRecommendedFreeAccounts(
  left: CodexAccount,
  right: CodexAccount,
): number {
  const quotaDiff = compareCodexAccountsByQuotaAvailability(left, right, "desc");
  if (quotaDiff !== 0) return quotaDiff;

  const resetDiff = compareNullableSortNumber(
    getCodexQuotaResetSortValue(left, "weekly_reset"),
    getCodexQuotaResetSortValue(right, "weekly_reset"),
    "asc",
  );
  return resetDiff !== 0 ? resetDiff : compareCodexAccountTieBreak(left, right);
}

function compareCodexRecommendedGroupedAccounts(
  left: CodexAccount,
  right: CodexAccount,
  groupSortMeta: Map<string, CodexGroupSortMeta>,
): number {
  const quotaDiff = compareCodexAccountsByQuotaAvailability(left, right, "desc");
  if (quotaDiff !== 0) return quotaDiff;

  const leftMeta = groupSortMeta.get(left.id);
  const rightMeta = groupSortMeta.get(right.id);
  const sortOrderDiff =
    (leftMeta?.sortOrder ?? Number.MAX_SAFE_INTEGER) -
    (rightMeta?.sortOrder ?? Number.MAX_SAFE_INTEGER);
  if (sortOrderDiff !== 0) return sortOrderDiff;

  const accountIndexDiff =
    (leftMeta?.accountIndex ?? Number.MAX_SAFE_INTEGER) -
    (rightMeta?.accountIndex ?? Number.MAX_SAFE_INTEGER);
  return accountIndexDiff !== 0
    ? accountIndexDiff
    : compareCodexAccountTieBreak(left, right);
}

function compareCodexRecommendedApiServiceAccounts(
  left: CodexAccount,
  right: CodexAccount,
  apiServiceSortMeta: Map<string, number>,
): number {
  const quotaDiff = compareCodexAccountsByQuotaAvailability(left, right, "desc");
  if (quotaDiff !== 0) return quotaDiff;

  if (isCodexNewApiAccount(left) !== isCodexNewApiAccount(right)) {
    return isCodexNewApiAccount(left) ? -1 : 1;
  }

  const orderDiff =
    (apiServiceSortMeta.get(left.id) ?? Number.MAX_SAFE_INTEGER) -
    (apiServiceSortMeta.get(right.id) ?? Number.MAX_SAFE_INTEGER);
  if (orderDiff !== 0) return orderDiff;

  return compareCodexAccountTieBreak(left, right);
}

function getCodexQuotaSortValue(
  account: CodexAccount,
  metric: "weekly" | "hourly",
): number | null {
  if (isCodexApiKeyAccount(account) && !isCodexNewApiAccount(account)) {
    return null;
  }
  const percentages = getCodexEffectiveQuotaPercentages(account.quota);
  return metric === "weekly" ? percentages.weekly : percentages.hourly;
}

function getCodexQuotaResetSortValue(
  account: CodexAccount,
  metric: "weekly_reset" | "hourly_reset",
): number | null {
  return toNullablePositiveSortNumber(
    metric === "weekly_reset"
      ? account.quota?.weekly_reset_time
      : account.quota?.hourly_reset_time,
  );
}

export function compareCodexAccountsByRecommendedSort(
  left: CodexAccount,
  right: CodexAccount,
  options: Pick<
    CodexAccountSortOptions,
    "apiServiceSortMeta" | "groupSortMeta" | "currentAccountId"
  > = {},
): number {
  const apiServiceSortMeta = options.apiServiceSortMeta ?? new Map<string, number>();
  const groupSortMeta =
    options.groupSortMeta ?? new Map<string, CodexGroupSortMeta>();
  const currentAccountId = options.currentAccountId ?? null;
  const topPriority = compareCodexAccountTopSortPriority(
    left,
    right,
    apiServiceSortMeta,
    groupSortMeta,
    currentAccountId,
  );
  if (topPriority !== 0) return topPriority;

  const leftBucket = getCodexAccountRecommendedSortBucket(
    left,
    apiServiceSortMeta,
    groupSortMeta,
    currentAccountId,
  );
  const rightBucket = getCodexAccountRecommendedSortBucket(
    right,
    apiServiceSortMeta,
    groupSortMeta,
    currentAccountId,
  );
  if (leftBucket !== rightBucket) {
    return leftBucket - rightBucket;
  }
  if (leftBucket === 0) {
    return compareCodexRecommendedApiServiceAccounts(
      left,
      right,
      apiServiceSortMeta,
    );
  }
  if (leftBucket === 1) {
    return compareCodexRecommendedGroupedAccounts(left, right, groupSortMeta);
  }
  if (leftBucket === 5) {
    return compareCodexRecommendedFreeAccounts(left, right);
  }

  const quotaDiff = compareCodexAccountsByQuotaAvailability(left, right, "desc");
  return quotaDiff !== 0 ? quotaDiff : compareCodexAccountTieBreak(left, right);
}

export function compareCodexAccountsBySort(
  left: CodexAccount,
  right: CodexAccount,
  options: CodexAccountSortOptions,
): number {
  const apiServiceSortMeta = options.apiServiceSortMeta ?? new Map<string, number>();
  const groupSortMeta =
    options.groupSortMeta ?? new Map<string, CodexGroupSortMeta>();
  const currentAccountId = options.currentAccountId ?? null;
  const { sortBy, sortDirection } = options;

  if (sortBy === CODEX_RECOMMENDED_SORT_BY) {
    return compareCodexAccountsByRecommendedSort(left, right, {
      apiServiceSortMeta,
      groupSortMeta,
      currentAccountId,
    });
  }

  const topPriority = compareCodexAccountTopSortPriority(
    left,
    right,
    apiServiceSortMeta,
    groupSortMeta,
    currentAccountId,
  );
  if (topPriority !== 0) return topPriority;

  if (sortBy === "created_at") {
    return compareCodexAccountCreatedAt(left, right, sortDirection);
  }
  if (sortBy === "weekly_reset" || sortBy === "hourly_reset") {
    const diff = compareNullableSortNumber(
      getCodexQuotaResetSortValue(left, sortBy),
      getCodexQuotaResetSortValue(right, sortBy),
      sortDirection,
    );
    return diff !== 0
      ? diff
      : compareCodexAccountTieBreak(left, right, sortDirection);
  }
  if (sortBy === "subscription_expiry") {
    const leftTimestamp = isCodexApiKeyAccount(left)
      ? null
      : toNullablePositiveSortNumber(options.getSubscriptionTimestampMs?.(left));
    const rightTimestamp = isCodexApiKeyAccount(right)
      ? null
      : toNullablePositiveSortNumber(options.getSubscriptionTimestampMs?.(right));
    const diff = compareNullableSortNumber(
      leftTimestamp,
      rightTimestamp,
      sortDirection,
    );
    return diff !== 0
      ? diff
      : compareCodexAccountTieBreak(left, right, sortDirection);
  }
  if (sortBy === "weekly" || sortBy === "hourly") {
    const diff = compareNullableSortNumber(
      getCodexQuotaSortValue(left, sortBy),
      getCodexQuotaSortValue(right, sortBy),
      sortDirection,
    );
    return diff !== 0
      ? diff
      : compareCodexAccountTieBreak(left, right, sortDirection);
  }

  return compareCodexAccountCreatedAt(left, right, sortDirection);
}
