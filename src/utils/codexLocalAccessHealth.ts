import type { CodexLocalAccessHealthSummary } from "../types/codexLocalAccess";

const QUOTA_SNAPSHOT_ERROR_TYPES = new Set([
  "usage_limit_reached",
  "insufficient_quota",
]);

function normalizeErrorType(value: string | null | undefined): string {
  return (value || "").trim().toLowerCase();
}

export function getCodexLocalAccessQuotaAccountRefreshKey(
  health: CodexLocalAccessHealthSummary | null | undefined,
): string | null {
  if (!health || health.unavailable || health.updatedAt <= 0) {
    return null;
  }

  const errorType = normalizeErrorType(health.lastErrorType);
  const quotaLikeError = QUOTA_SNAPSHOT_ERROR_TYPES.has(errorType);
  const hasQuotaState =
    health.exhaustedCount > 0 || health.estimatedAvailableCount > 0;

  if (!quotaLikeError && !hasQuotaState) {
    return null;
  }

  return [
    health.updatedAt,
    errorType,
    health.lastStatus ?? "",
    health.lastRequestId ?? "",
    health.exhaustedCount,
    health.estimatedAvailableCount,
    health.activeModelCooldownCount,
    health.nearestCooldownUntilMs ?? "",
  ].join("|");
}
