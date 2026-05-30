export type CodexApiProviderMode = "openai_builtin" | "custom";

export interface CodexQuickConfig {
  context_window_1m: boolean;
  auto_compact_token_limit: number;
  detected_model_context_window?: number;
  detected_auto_compact_token_limit?: number;
}

export type CodexAppSpeed = "standard" | "fast";

export interface CodexAppSpeedConfig {
  speed: CodexAppSpeed;
  globalStatePath: string;
}

/** Codex 账号数据 */
export interface CodexAccount {
  id: string;
  email: string;
  auth_mode?: string;
  openai_api_key?: string;
  api_base_url?: string;
  api_provider_mode?: CodexApiProviderMode;
  api_provider_id?: string;
  api_provider_name?: string;
  user_id?: string;
  plan_type?: string;
  subscription_active_until?: string;
  auth_file_plan_type?: string;
  account_id?: string;
  organization_id?: string;
  account_name?: string;
  account_structure?: string;
  account_note?: string;
  app_speed?: CodexAppSpeed;
  tokens: CodexTokens;
  token_generation?: number;
  token_updated_at?: number;
  token_source_mode?: string;
  requires_reauth?: boolean;
  reauth_reason?: string;
  quota?: CodexQuota;
  quota_error?: CodexQuotaErrorInfo;
  tags?: string[];
  created_at: number;
  last_used: number;
}

export interface CodexQuotaErrorInfo {
  code?: string;
  message: string;
  timestamp: number;
}

const CODEX_QUOTA_LIMIT_ERROR_CODES = new Set([
  "usage_limit_reached",
  "insufficient_quota",
  "rate_limit_exceeded",
  "rate_limit_reached",
  "model_cap_reached",
  "model_cap_exceeded",
  "quota_exhausted",
  "upstream_rate_limit",
]);

export type CodexQuotaIssueKind = "none" | "refresh" | "limited" | "error";

export interface CodexQuotaIssueInfo {
  kind: CodexQuotaIssueKind;
  statusCode: string;
  errorCode: string;
  displayCode: string;
  rawMessage: string;
  isRefreshRequestFailure: boolean;
  isQuotaLimitError: boolean;
}

export function normalizeCodexQuotaErrorCode(value?: string | null): string {
  return (value || "").trim().toLowerCase();
}

function readCodexQuotaErrorCodeFromJson(message: string): string {
  const trimmed = message.trim();
  if (!trimmed.startsWith("{")) return "";

  const root = toJsonRecord(parseJsonValue(trimmed));
  if (!root) return "";

  const candidates = [
    toJsonRecord(root.error)?.type,
    toJsonRecord(root.error)?.code,
    toJsonRecord(root.detail)?.type,
    toJsonRecord(root.detail)?.code,
    root.type,
    root.code,
  ];
  for (const candidate of candidates) {
    if (typeof candidate === "string" && candidate.trim()) {
      return candidate.trim();
    }
  }
  return "";
}

function parseJsonValue(value: string): unknown {
  try {
    return JSON.parse(value);
  } catch {
    return null;
  }
}

export function extractCodexQuotaErrorCode(message: string): string {
  const rawMessage = message.trim();
  const jsonCode = readCodexQuotaErrorCodeFromJson(rawMessage);
  if (jsonCode) return jsonCode;

  const normalizedRawMessage = normalizeCodexQuotaErrorCode(rawMessage);
  if (CODEX_QUOTA_LIMIT_ERROR_CODES.has(normalizedRawMessage)) {
    return normalizedRawMessage;
  }

  return (
    rawMessage.match(/\[error_code:([^\]]+)\]/)?.[1] ||
    rawMessage.match(/error_code[=:]\s*([^,\]\s]+)/i)?.[1] ||
    rawMessage.match(/error_type[=:]\s*([^,\]\s]+)/i)?.[1] ||
    rawMessage.match(/provider_code[=:]\s*([^,\]\s]+)/i)?.[1] ||
    ""
  );
}

export function extractCodexQuotaErrorStatusCode(message: string): string {
  return (
    message.match(/API 返回错误\s+(\d{3})/i)?.[1] ||
    message.match(/status[=: ]+(\d{3})/i)?.[1] ||
    ""
  );
}

export function isCodexQuotaLimitError(
  error?: CodexQuotaErrorInfo | null,
): boolean {
  if (!error) return false;
  const rawMessage = (error.message || "").trim();
  const lowerMessage = rawMessage.toLowerCase();
  const statusCode = extractCodexQuotaErrorStatusCode(rawMessage);
  const errorCode = normalizeCodexQuotaErrorCode(
    error.code || extractCodexQuotaErrorCode(rawMessage),
  );

  if (CODEX_QUOTA_LIMIT_ERROR_CODES.has(errorCode)) return true;
  if (!rawMessage) return false;

  return (
    statusCode === "429" ||
    lowerMessage.includes("too many requests") ||
    lowerMessage.includes("rate_limit") ||
    lowerMessage.includes("rate limit") ||
    lowerMessage.includes("limit_reached") ||
    lowerMessage.includes("usage_limit") ||
    lowerMessage.includes("usage limit") ||
    lowerMessage.includes("model_cap") ||
    (lowerMessage.includes("quota") &&
      (lowerMessage.includes("exceed") ||
        lowerMessage.includes("limit") ||
        lowerMessage.includes("exhaust")))
  );
}

export function getCodexQuotaIssueInfo(
  error?: CodexQuotaErrorInfo | null,
): CodexQuotaIssueInfo {
  const rawMessage = (error?.message || "").trim();
  const statusCode = rawMessage
    ? extractCodexQuotaErrorStatusCode(rawMessage)
    : "";
  const errorCode = normalizeCodexQuotaErrorCode(
    error?.code || (rawMessage ? extractCodexQuotaErrorCode(rawMessage) : ""),
  );
  const displayCode = errorCode || statusCode;
  const isRefreshRequestFailure =
    rawMessage.toLowerCase().includes("error sending request") &&
    !statusCode &&
    !errorCode;
  const isQuotaLimitError = isCodexQuotaLimitError(error);
  const kind: CodexQuotaIssueKind =
    !rawMessage && !errorCode
      ? "none"
      : isRefreshRequestFailure
        ? "refresh"
        : isQuotaLimitError
          ? "limited"
          : "error";

  return {
    kind,
    statusCode,
    errorCode,
    displayCode,
    rawMessage,
    isRefreshRequestFailure,
    isQuotaLimitError,
  };
}

export function shouldShowCodexQuotaIssueNotice(
  error?: CodexQuotaErrorInfo | null,
): boolean {
  const issueInfo = getCodexQuotaIssueInfo(error);
  return issueInfo.kind !== "none" && !issueInfo.isQuotaLimitError;
}

export function isCodexAccountErrorState(account: CodexAccount): boolean {
  return Boolean(
    account.requires_reauth ||
      (account.quota_error && !isCodexQuotaLimitError(account.quota_error)),
  );
}

/** Codex Token 数据 */
export interface CodexTokens {
  id_token: string;
  access_token: string;
  refresh_token?: string;
}

/** Codex 配额数据 */
export interface CodexQuota {
  /** 5小时配额百分比 (0-100) */
  hourly_percentage: number;
  /** 5小时配额重置时间 (Unix timestamp) */
  hourly_reset_time?: number;
  /** 主窗口时长（分钟） */
  hourly_window_minutes?: number;
  /** 主窗口是否存在（接口返回） */
  hourly_window_present?: boolean;
  /** 周配额百分比 (0-100) */
  weekly_percentage: number;
  /** 周配额重置时间 (Unix timestamp) */
  weekly_reset_time?: number;
  /** 次窗口时长（分钟） */
  weekly_window_minutes?: number;
  /** 次窗口是否存在（接口返回） */
  weekly_window_present?: boolean;
  /** 原始响应数据 */
  raw_data?: unknown;
}

const COCKPIT_API_BASE_URL = "https://chongcodex.cn/v1";

function normalizeCodexApiBaseUrlForMatch(rawValue?: string | null): string {
  const trimmed = (rawValue || "").trim();
  if (!trimmed) return "";
  try {
    const parsed = new URL(trimmed);
    if (parsed.protocol !== "http:" && parsed.protocol !== "https:") {
      return "";
    }
    return `${parsed.origin}${parsed.pathname}`
      .replace(/\/+$/, "")
      .toLowerCase();
  } catch {
    return "";
  }
}

export function isCodexCockpitApiBaseUrl(rawValue?: string | null): boolean {
  return (
    normalizeCodexApiBaseUrlForMatch(rawValue) ===
    normalizeCodexApiBaseUrlForMatch(COCKPIT_API_BASE_URL)
  );
}

export interface CodexWorkspace {
  id: string;
  title: string;
  role?: string;
  is_default?: boolean;
}

export interface CodexAuthMetadata {
  chatgptAccountId?: string;
  authProvider?: string;
  userId?: string;
  workspaces: CodexWorkspace[];
}

export interface CodexCodeReviewQuotaMetric {
  percentage: number;
  label: string;
  resetTime?: number;
}

export interface CodexInstanceThreadSyncItem {
  instanceId: string;
  instanceName: string;
  addedThreadCount: number;
  backupDir?: string | null;
}

export interface CodexInstanceThreadSyncSummary {
  instanceCount: number;
  threadUniverseCount: number;
  mutatedInstanceCount: number;
  totalSyncedThreadCount: number;
  items: CodexInstanceThreadSyncItem[];
  backupDirs: string[];
  message: string;
}

export interface CodexSessionVisibilityRepairItem {
  instanceId: string;
  instanceName: string;
  targetProvider: string;
  changedRolloutFileCount: number;
  updatedSqliteRowCount: number;
  skippedSqliteFile: boolean;
  backupDir?: string | null;
  running: boolean;
}

export interface CodexSessionVisibilityRepairSummary {
  instanceCount: number;
  mutatedInstanceCount: number;
  changedRolloutFileCount: number;
  updatedSqliteRowCount: number;
  skippedSqliteFileCount: number;
  items: CodexSessionVisibilityRepairItem[];
  backupDirs: string[];
  message: string;
}

export interface CodexSessionLocation {
  instanceId: string;
  instanceName: string;
  running: boolean;
}

export interface CodexSessionRecord {
  sessionId: string;
  title: string;
  cwd: string;
  updatedAt?: number | null;
  locationCount: number;
  locations: CodexSessionLocation[];
}

export interface CodexSessionTokenStats {
  sessionId: string;
  inputTokens: number;
  outputTokens: number;
  totalTokens: number;
}

export interface CodexInstanceTargetThreadSyncSummary {
  requestedSessionCount: number;
  targetInstanceId: string;
  targetInstanceName: string;
  syncedSessionCount: number;
  skippedExistingCount: number;
  missingSessionCount: number;
  backupDir?: string | null;
  running: boolean;
  message: string;
}

export interface CodexSessionTrashSummary {
  requestedSessionCount: number;
  trashedSessionCount: number;
  trashedInstanceCount: number;
  trashDirs: string[];
  message: string;
}

export interface CodexTrashedSessionLocation {
  instanceId: string;
  instanceName: string;
}

export interface CodexTrashedSessionRecord {
  sessionId: string;
  title: string;
  cwd: string;
  deletedAt?: number | null;
  locationCount: number;
  locations: CodexTrashedSessionLocation[];
}

export interface CodexSessionRestoreSummary {
  requestedSessionCount: number;
  restoredSessionCount: number;
  restoredInstanceCount: number;
  message: string;
}

type JsonRecord = Record<string, unknown>;

function toJsonRecord(value: unknown): JsonRecord | null {
  return value && typeof value === "object" && !Array.isArray(value)
    ? (value as JsonRecord)
    : null;
}

function toStringValue(value: unknown): string | undefined {
  if (typeof value === "string") {
    const trimmed = value.trim();
    return trimmed || undefined;
  }
  if (typeof value === "number" || typeof value === "boolean") {
    return String(value);
  }
  return undefined;
}

function toBoolValue(value: unknown): boolean | undefined {
  if (typeof value === "boolean") return value;
  if (typeof value === "number") {
    if (value === 1) return true;
    if (value === 0) return false;
  }
  if (typeof value === "string") {
    const normalized = value.trim().toLowerCase();
    if (normalized === "true") return true;
    if (normalized === "false") return false;
  }
  return undefined;
}

function toFiniteNumber(value: unknown): number | undefined {
  if (typeof value === "number" && Number.isFinite(value)) {
    return value;
  }
  if (typeof value === "string" && value.trim()) {
    const parsed = Number(value.trim());
    return Number.isFinite(parsed) ? parsed : undefined;
  }
  return undefined;
}

function normalizeCodexUnixSeconds(value: number | undefined): number | undefined {
  if (value === undefined || !Number.isFinite(value)) return undefined;
  return value > 1_000_000_000_000 ? Math.floor(value / 1000) : value;
}

function decodeJwtPayload(token: string | undefined): JsonRecord | null {
  if (!token) return null;
  const parts = token.split(".");
  if (parts.length < 2) return null;

  const payloadPart = parts[1];
  const padded = payloadPart + "=".repeat((4 - (payloadPart.length % 4)) % 4);
  const base64 = padded.replace(/-/g, "+").replace(/_/g, "/");

  try {
    const binary = atob(base64);
    const bytes = Uint8Array.from(binary, (char) => char.charCodeAt(0));
    const text = new TextDecoder().decode(bytes);
    return toJsonRecord(JSON.parse(text));
  } catch {
    return null;
  }
}

function normalizeWorkspaceList(value: unknown): CodexWorkspace[] {
  if (!Array.isArray(value)) return [];
  const dedupe = new Set<string>();
  const result: CodexWorkspace[] = [];

  value.forEach((item) => {
    const record = toJsonRecord(item);
    if (!record) return;
    const id =
      toStringValue(record.id) ||
      toStringValue(record.organization_id) ||
      toStringValue(record.workspace_id);
    const title =
      toStringValue(record.title) ||
      toStringValue(record.name) ||
      toStringValue(record.display_name) ||
      toStringValue(record.workspace_name) ||
      toStringValue(record.organization_name);
    if (!id && !title) return;
    const dedupeKey = `${id || ""}::${title || ""}`;
    if (dedupe.has(dedupeKey)) return;
    dedupe.add(dedupeKey);
    result.push({
      id: id || "",
      title: title || id || "",
      role: toStringValue(record.role),
      is_default: toBoolValue(record.is_default),
    });
  });

  return result;
}

export function getCodexAuthMetadata(account: CodexAccount): CodexAuthMetadata {
  const idTokenPayload = decodeJwtPayload(account.tokens?.id_token);
  const accessTokenPayload = decodeJwtPayload(account.tokens?.access_token);
  const idTokenAuthData = toJsonRecord(
    idTokenPayload?.["https://api.openai.com/auth"],
  );
  const accessTokenAuthData = toJsonRecord(
    accessTokenPayload?.["https://api.openai.com/auth"],
  );

  const chatgptAccountId =
    account.account_id ||
    toStringValue(idTokenAuthData?.chatgpt_account_id) ||
    toStringValue(accessTokenAuthData?.chatgpt_account_id) ||
    toStringValue(idTokenAuthData?.account_id);
  const authProvider = toStringValue(idTokenPayload?.auth_provider);
  const userId =
    account.user_id ||
    toStringValue(idTokenAuthData?.chatgpt_user_id) ||
    toStringValue(accessTokenAuthData?.chatgpt_user_id) ||
    toStringValue(idTokenAuthData?.user_id) ||
    toStringValue(accessTokenAuthData?.user_id) ||
    toStringValue(idTokenPayload?.sub);
  const workspaces = normalizeWorkspaceList(idTokenAuthData?.organizations);

  return {
    chatgptAccountId,
    authProvider,
    userId,
    workspaces,
  };
}

export function formatCodexLoginProvider(
  rawProvider: string | undefined,
): string {
  const value = rawProvider?.trim();
  if (!value) return "";
  const normalized = value.toLowerCase();
  if (normalized === "google") return "Google";
  if (normalized === "github") return "GitHub";
  if (normalized === "microsoft") return "Microsoft";
  if (normalized === "apple") return "Apple";
  if (normalized === "password") return "Password";
  return value;
}

function normalizeCodeReviewWindow(
  window: JsonRecord,
  fallback: "hourly" | "weekly",
): CodexCodeReviewQuotaMetric | null {
  const usedPercent = toFiniteNumber(window.used_percent);
  if (usedPercent === undefined) return null;
  const percentage = Math.max(0, Math.min(100, 100 - Math.round(usedPercent)));
  const limitWindowSeconds = toFiniteNumber(window.limit_window_seconds);
  const windowMinutes =
    limitWindowSeconds !== undefined && limitWindowSeconds > 0
      ? Math.ceil(limitWindowSeconds / 60)
      : undefined;
  const resetAt = normalizeCodexUnixSeconds(toFiniteNumber(window.reset_at));
  const resetAfterSeconds = toFiniteNumber(window.reset_after_seconds);
  const resetTime =
    resetAt ??
    (resetAfterSeconds !== undefined && resetAfterSeconds >= 0
      ? Math.floor(Date.now() / 1000) + resetAfterSeconds
      : undefined);

  return {
    percentage,
    label: getCodexQuotaWindowLabel(windowMinutes, fallback),
    resetTime,
  };
}

export function getCodexCodeReviewQuotaMetric(
  quota: CodexQuota | undefined,
): CodexCodeReviewQuotaMetric | null {
  const raw = toJsonRecord(quota?.raw_data);
  const rateLimit = toJsonRecord(raw?.code_review_rate_limit);
  if (!rateLimit) return null;

  const primaryWindow = toJsonRecord(rateLimit.primary_window);
  const secondaryWindow = toJsonRecord(rateLimit.secondary_window);

  return (
    (primaryWindow
      ? normalizeCodeReviewWindow(primaryWindow, "hourly")
      : null) ||
    (secondaryWindow
      ? normalizeCodeReviewWindow(secondaryWindow, "weekly")
      : null)
  );
}

export function isCodexApiKeyAccount(account: CodexAccount): boolean {
  return (account.auth_mode || "").trim().toLowerCase() === "apikey";
}

export function isCodexNewApiAccount(account: CodexAccount): boolean {
  const providerId = (account.api_provider_id || "").trim().toLowerCase();
  const planType = (account.plan_type || "").trim().toUpperCase();
  return (
    isCodexApiKeyAccount(account) &&
    (providerId === "cockpit_api" ||
      providerId === "new_api" ||
      isCodexCockpitApiBaseUrl(account.api_base_url) ||
      planType === "COCKPIT API" ||
      planType === "NEW_API_EXCLUSIVE")
  );
}

/** 获取订阅类型显示名称 */
export function getCodexPlanDisplayName(planType?: string): string {
  if (!planType) return "FREE";
  const upper = planType.toUpperCase();
  if (upper.includes("TEAM")) return "TEAM";
  if (upper.includes("ENTERPRISE")) return "ENTERPRISE";
  if (upper.includes("PLUS")) return "PLUS";
  if (upper.includes("PRO")) return "PRO";
  return upper;
}

function normalizeCodexPlanKey(planType?: string): string {
  const normalized = (planType || "").trim().toLowerCase();
  if (!normalized) return "free";
  if (normalized.includes("api")) return "api_key";
  if (normalized.includes("enterprise")) return "enterprise";
  if (normalized.includes("business")) return "business";
  if (normalized.includes("team")) return "team";
  if (normalized.includes("edu")) return "edu";
  if (normalized.includes("go")) return "go";
  if (normalized.includes("plus")) return "plus";
  if (normalized.includes("pro")) return "pro";
  if (normalized.includes("free")) return "free";
  return normalized;
}

export function isCodexExplicitFreePlanType(planType?: string): boolean {
  const normalized = (planType || "").trim();
  if (!normalized) return false;
  return normalizeCodexPlanKey(planType) === "free";
}

function normalizeCodexAuthFilePlanType(
  value?: string,
): "prolite" | "promax" | undefined {
  const normalized = (value || "")
    .trim()
    .toLowerCase()
    .replace(/[_\s]+/g, "-");
  if (
    normalized === "prolite" ||
    normalized === "pro-lite" ||
    normalized === "pro-5x" ||
    normalized === "codex-pro-5x"
  ) {
    return "prolite";
  }
  if (
    normalized === "promax" ||
    normalized === "pro-max" ||
    normalized === "pro-20x" ||
    normalized === "codex-pro-20x"
  ) {
    return "promax";
  }
  return undefined;
}

export function getCodexPlanBadgeLabel(account: CodexAccount): string {
  if (isCodexNewApiAccount(account)) {
    return account.plan_type?.trim() || "Cockpit Api";
  }
  const baseLabel = getCodexPlanDisplayName(account.plan_type);
  if (normalizeCodexPlanKey(account.plan_type) !== "pro") {
    return baseLabel;
  }

  const authFilePlanType =
    normalizeCodexAuthFilePlanType(account.auth_file_plan_type) ??
    normalizeCodexAuthFilePlanType(account.plan_type);
  if (authFilePlanType === "prolite") {
    return `${baseLabel} 5x`;
  }
  // CPA 对齐：plan_type='pro' 默认视为 20x（Pro Max），
  // 只有显式声明 prolite/pro-lite/pro_lite 才是 5x
  return `${baseLabel} 20x`;
}

export function getCodexPlanBadgeClass(account: CodexAccount): string {
  if (isCodexNewApiAccount(account)) {
    return "api-key new-api-exclusive";
  }
  const baseClass = normalizeCodexPlanKey(account.plan_type);
  if (baseClass === "plus") {
    return "plus codex-plus";
  }
  if (baseClass !== "pro") {
    return baseClass;
  }

  const authFilePlanType =
    normalizeCodexAuthFilePlanType(account.auth_file_plan_type) ??
    normalizeCodexAuthFilePlanType(account.plan_type);
  if (authFilePlanType === "prolite") {
    return "pro codex-pro-lite";
  }
  // CPA 对齐：plan_type='pro' 默认视为 promax (20x)
  return "pro codex-pro-max";
}

export function getCodexPlanFilterKey(account: CodexAccount): string {
  return normalizeCodexPlanKey(account.plan_type).toUpperCase();
}

export function isCodexTeamLikePlan(planType?: string): boolean {
  if (!planType) return false;
  const upper = planType.toUpperCase();
  return (
    upper.includes("TEAM") ||
    upper.includes("BUSINESS") ||
    upper.includes("ENTERPRISE") ||
    upper.includes("EDU")
  );
}

export function hasCodexAccountName(account: CodexAccount): boolean {
  return (
    typeof account.account_name === "string" &&
    account.account_name.trim().length > 0
  );
}

export function hasCodexAccountStructure(account: CodexAccount): boolean {
  return (
    typeof account.account_structure === "string" &&
    account.account_structure.trim().length > 0
  );
}

/** 获取配额百分比的样式类名 */
export function getCodexQuotaClass(percentage: number): string {
  if (percentage >= 80) return "high";
  if (percentage >= 40) return "medium";
  if (percentage >= 10) return "low";
  return "critical";
}

type Translate = (key: string, options?: Record<string, unknown>) => string;

const DAY_IN_MS = 24 * 60 * 60 * 1000;
const HOUR_IN_MS = 60 * 60 * 1000;

export type CodexSubscriptionExpiryBucket =
  | "missing"
  | "expired"
  | "within_24h"
  | "within_7d"
  | "within_30d"
  | "active";

export interface CodexSubscriptionPresentation {
  bucket: CodexSubscriptionExpiryBucket;
  tone: "missing" | "expired" | "warning" | "active";
  valueText: string;
  detailText: string;
  titleText: string;
  timestampMs: number | null;
}

export function parseCodexSubscriptionDate(value?: string): Date | null {
  const trimmed = (value || "").trim();
  if (!trimmed) return null;

  if (/^\d+$/.test(trimmed)) {
    let timestamp = Number(trimmed);
    if (!Number.isFinite(timestamp)) return null;
    if (timestamp < 1_000_000_000_000) {
      timestamp *= 1000;
    }
    const date = new Date(timestamp);
    return Number.isNaN(date.getTime()) ? null : date;
  }

  const parsed = new Date(trimmed);
  return Number.isNaN(parsed.getTime()) ? null : parsed;
}

function formatCodexSubscriptionDate(date: Date): string {
  const pad = (value: number) => String(value).padStart(2, "0");
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())} ${pad(date.getHours())}:${pad(date.getMinutes())}`;
}

export function getCodexSubscriptionExpiryBucket(
  subscriptionActiveUntil?: string,
): CodexSubscriptionExpiryBucket {
  const date = parseCodexSubscriptionDate(subscriptionActiveUntil);
  if (!date) return "missing";

  const diffMs = date.getTime() - Date.now();
  if (diffMs <= 0) return "expired";
  if (diffMs <= HOUR_IN_MS * 24) return "within_24h";
  if (diffMs <= DAY_IN_MS * 7) return "within_7d";
  if (diffMs <= DAY_IN_MS * 30) return "within_30d";
  return "active";
}

export function getCodexSubscriptionPresentation(
  subscriptionActiveUntil: string | undefined,
  t: Translate,
): CodexSubscriptionPresentation {
  const date = parseCodexSubscriptionDate(subscriptionActiveUntil);
  if (!date) {
    const valueText = t("codex.subscription.unknown");
    const detailText = t("codex.subscription.missingDetail");
    return {
      bucket: "missing",
      tone: "missing",
      valueText,
      detailText,
      titleText: t("codex.subscription.titleUnknown"),
      timestampMs: null,
    };
  }

  const timestampMs = date.getTime();
  const diffMs = timestampMs - Date.now();
  const detailText = formatCodexSubscriptionDate(date);

  if (diffMs <= 0) {
    const valueText = t("codex.subscription.expired");
    return {
      bucket: "expired",
      tone: "expired",
      valueText,
      detailText,
      titleText: t("codex.subscription.titleWithDate", { date: detailText }),
      timestampMs,
    };
  }

  if (diffMs < DAY_IN_MS) {
    const hours = Math.max(1, Math.ceil(diffMs / HOUR_IN_MS));
    const valueText = t("codex.subscription.hoursLeft", { count: hours });
    return {
      bucket: "within_24h",
      tone: "warning",
      valueText,
      detailText,
      titleText: t("codex.subscription.titleWithDate", { date: detailText }),
      timestampMs,
    };
  }

  const days = Math.ceil(diffMs / DAY_IN_MS);
  const valueText =
    days > 99
      ? t("codex.subscription.over99Days")
      : t("codex.subscription.daysLeft", { count: days });

  return {
    bucket: getCodexSubscriptionExpiryBucket(subscriptionActiveUntil),
    tone: days <= 7 ? "warning" : "active",
    valueText,
    detailText,
    titleText: t("codex.subscription.titleWithDate", { date: detailText }),
    timestampMs,
  };
}

export interface CodexQuotaWindow {
  id: "primary" | "secondary";
  label: string;
  percentage: number;
  rawPercentage?: number;
  serverBaselineAdjusted?: boolean;
  resetTime?: number;
  windowMinutes?: number;
}

export interface CodexEffectiveQuotaPercentages {
  hourly: number | null;
  weekly: number | null;
  weeklyBlocksHourly: boolean;
}

function clampCodexQuotaPercentage(value: number | null | undefined): number {
  if (typeof value !== "number" || !Number.isFinite(value)) return 0;
  if (value <= 0) return 0;
  if (value >= 100) return 100;
  return Math.round(value);
}

const CODEX_WEEK_WINDOW_SECONDS = 7 * 24 * 60 * 60;
const CODEX_WEEK_WINDOW_MINUTES = 7 * 24 * 60;
const CODEX_WEEKLY_WINDOW_MINUTES_THRESHOLD = 6 * 24 * 60;
const CODEX_FIVE_HOUR_WINDOW_SECONDS = 5 * 60 * 60;
const CODEX_FREE_BASELINE_USED_PERCENT_MAX = 3;
const CODEX_FREE_BASELINE_RESET_GRACE_SECONDS = 5 * 60;
const CODEX_SERVER_INITIAL_BASELINE_REMAINING_PERCENTAGE = 97;

interface CodexNormalizedQuotaWindowSlot {
  percentage: number;
  resetTime?: number;
  windowMinutes?: number;
  present: boolean;
}

interface CodexNormalizedQuotaWindowSlots {
  hourly: CodexNormalizedQuotaWindowSlot;
  weekly: CodexNormalizedQuotaWindowSlot;
}

function isCodexWeeklyWindowMinutes(windowMinutes: number | undefined): boolean {
  return (
    typeof windowMinutes === "number" &&
    Number.isFinite(windowMinutes) &&
    windowMinutes >= CODEX_WEEKLY_WINDOW_MINUTES_THRESHOLD
  );
}

function getCodexNormalizedQuotaWindowSlots(
  quota: CodexQuota,
): CodexNormalizedQuotaWindowSlots {
  const hasPresenceFlags =
    quota.hourly_window_present !== undefined ||
    quota.weekly_window_present !== undefined;
  const bothFlagsFalse =
    quota.hourly_window_present === false &&
    quota.weekly_window_present === false;

  let hourly: CodexNormalizedQuotaWindowSlot = {
    percentage: clampCodexQuotaPercentage(quota.hourly_percentage),
    resetTime: quota.hourly_reset_time,
    windowMinutes: quota.hourly_window_minutes,
    present:
      !hasPresenceFlags || quota.hourly_window_present === true || bothFlagsFalse,
  };
  let weekly: CodexNormalizedQuotaWindowSlot = {
    percentage: clampCodexQuotaPercentage(quota.weekly_percentage),
    resetTime: quota.weekly_reset_time,
    windowMinutes: quota.weekly_window_minutes,
    present: !hasPresenceFlags || quota.weekly_window_present === true,
  };

  if (hourly.present && isCodexWeeklyWindowMinutes(hourly.windowMinutes)) {
    if (!weekly.present || hourly.percentage < weekly.percentage) {
      weekly = {
        percentage: hourly.percentage,
        resetTime: hourly.resetTime ?? weekly.resetTime,
        windowMinutes: hourly.windowMinutes ?? weekly.windowMinutes,
        present: true,
      };
    } else {
      weekly = {
        ...weekly,
        windowMinutes: weekly.windowMinutes ?? hourly.windowMinutes,
        present: true,
      };
    }

    hourly = {
      percentage: 100,
      resetTime: undefined,
      windowMinutes: undefined,
      present: false,
    };
  }

  return { hourly, weekly };
}

function getCodexQuotaRawRateLimitWindow(
  quota: CodexQuota | undefined,
  window: "hourly" | "weekly",
): JsonRecord | null {
  const raw = toJsonRecord(quota?.raw_data);
  const rateLimit = toJsonRecord(raw?.rate_limit);
  return toJsonRecord(
    window === "hourly"
      ? rateLimit?.primary_window
      : rateLimit?.secondary_window,
  );
}

function isInitialQuotaWindow(
  window: JsonRecord | null,
  expectedWindowSeconds: number,
): boolean {
  if (!window) return false;

  const usedPercent = toFiniteNumber(window.used_percent) ?? 0;
  const windowSeconds = toFiniteNumber(window.limit_window_seconds);
  const resetAfterSeconds = toFiniteNumber(window.reset_after_seconds);
  if (windowSeconds === undefined || resetAfterSeconds === undefined) {
    return false;
  }

  return (
    usedPercent >= 0 &&
    usedPercent <= CODEX_FREE_BASELINE_USED_PERCENT_MAX &&
    windowSeconds >= expectedWindowSeconds - CODEX_FREE_BASELINE_RESET_GRACE_SECONDS &&
    resetAfterSeconds >= windowSeconds - CODEX_FREE_BASELINE_RESET_GRACE_SECONDS
  );
}

export function isCodexFreeWeeklyServerBaseline(
  quota: CodexQuota | undefined,
): boolean {
  const raw = toJsonRecord(quota?.raw_data);
  const planType = toStringValue(raw?.plan_type)?.toLowerCase();
  if (planType !== "free") return false;

  const primaryWindow = getCodexQuotaRawRateLimitWindow(quota, "hourly");
  const rateLimit = toJsonRecord(raw?.rate_limit);
  if (!primaryWindow || toJsonRecord(rateLimit?.secondary_window)) return false;

  return isInitialQuotaWindow(primaryWindow, CODEX_WEEK_WINDOW_SECONDS);
}

function isCodexPlusFiveHourServerBaseline(
  quota: CodexQuota | undefined,
): boolean {
  const raw = toJsonRecord(quota?.raw_data);
  const planType = toStringValue(raw?.plan_type)?.toLowerCase();
  if (planType !== "plus") return false;

  return isInitialQuotaWindow(
    getCodexQuotaRawRateLimitWindow(quota, "hourly"),
    CODEX_FIVE_HOUR_WINDOW_SECONDS,
  );
}

function isCodexServerInitialBaseline(
  quota: CodexQuota,
  window: "hourly" | "weekly",
): boolean {
  return (
    (window === "hourly" && isCodexFreeWeeklyServerBaseline(quota)) ||
    (window === "hourly" && isCodexPlusFiveHourServerBaseline(quota))
  );
}

function applyCodexServerBaselineAdjustment(
  quota: CodexQuota,
  window: "hourly" | "weekly",
  percentage: number | null,
): number | null {
  if (percentage == null) return null;
  if (isCodexServerInitialBaseline(quota, window)) {
    return Math.min(percentage, CODEX_SERVER_INITIAL_BASELINE_REMAINING_PERCENTAGE);
  }
  return percentage;
}

export function getCodexEffectiveQuotaPercentages(
  quota: CodexQuota | undefined,
): CodexEffectiveQuotaPercentages {
  if (!quota) {
    return { hourly: null, weekly: null, weeklyBlocksHourly: false };
  }

  const slots = getCodexNormalizedQuotaWindowSlots(quota);
  const hourly = slots.hourly.present
    ? applyCodexServerBaselineAdjustment(
        quota,
        "hourly",
        slots.hourly.percentage,
      )
    : null;
  const weekly = slots.weekly.present
    ? applyCodexServerBaselineAdjustment(
        quota,
        "weekly",
        slots.weekly.percentage,
      )
    : null;
  const weeklyBlocksHourly = weekly === 0 && hourly != null;

  return {
    hourly: weeklyBlocksHourly ? 0 : hourly,
    weekly,
    weeklyBlocksHourly,
  };
}

export function getCodexQuotaWindowLabel(
  windowMinutes: number | undefined,
  fallback: "hourly" | "weekly" = "hourly",
): string {
  const HOUR_MINUTES = 60;
  const DAY_MINUTES = 24 * HOUR_MINUTES;
  const safeMinutes =
    typeof windowMinutes === "number" &&
    Number.isFinite(windowMinutes) &&
    windowMinutes > 0
      ? Math.ceil(windowMinutes)
      : null;

  if (safeMinutes == null) {
    return fallback === "weekly" ? "Weekly" : "5h";
  }

  if (safeMinutes >= CODEX_WEEK_WINDOW_MINUTES - 1) {
    const weeks = Math.ceil(safeMinutes / CODEX_WEEK_WINDOW_MINUTES);
    return weeks <= 1 ? "Weekly" : `${weeks} Week`;
  }

  if (
    fallback === "weekly" &&
    safeMinutes >= CODEX_WEEKLY_WINDOW_MINUTES_THRESHOLD
  ) {
    return "Weekly";
  }

  if (safeMinutes >= DAY_MINUTES - 1) {
    return `${Math.ceil(safeMinutes / DAY_MINUTES)}d`;
  }

  if (safeMinutes >= HOUR_MINUTES) {
    return `${Math.ceil(safeMinutes / HOUR_MINUTES)}h`;
  }

  return `${Math.ceil(safeMinutes)}m`;
}

export function getCodexQuotaWindows(
  quota: CodexQuota | undefined,
): CodexQuotaWindow[] {
  if (!quota) return [];

  const windows: CodexQuotaWindow[] = [];
  const effective = getCodexEffectiveQuotaPercentages(quota);
  const slots = getCodexNormalizedQuotaWindowSlots(quota);
  const appendPrimary = slots.hourly.present;
  const appendSecondary = slots.weekly.present;

  if (appendPrimary) {
    const rawPercentage = slots.hourly.percentage;
    windows.push({
      id: "primary",
      label: getCodexQuotaWindowLabel(slots.hourly.windowMinutes, "hourly"),
      percentage: effective.hourly ?? 0,
      rawPercentage,
      serverBaselineAdjusted:
        isCodexServerInitialBaseline(quota, "hourly") &&
        effective.hourly != null &&
        effective.hourly !== rawPercentage,
      resetTime: slots.hourly.resetTime,
      windowMinutes: slots.hourly.windowMinutes,
    });
  }

  if (appendSecondary) {
    const rawPercentage = slots.weekly.percentage;
    windows.push({
      id: "secondary",
      label: getCodexQuotaWindowLabel(slots.weekly.windowMinutes, "weekly"),
      percentage: effective.weekly ?? 0,
      rawPercentage,
      resetTime: slots.weekly.resetTime,
      windowMinutes: slots.weekly.windowMinutes,
    });
  }

  if (windows.length > 0) {
    return windows;
  }

  return [
    {
      id: "primary",
      label: getCodexQuotaWindowLabel(slots.hourly.windowMinutes, "hourly"),
      percentage: effective.hourly ?? 0,
      resetTime: slots.hourly.resetTime,
      windowMinutes: slots.hourly.windowMinutes,
    },
  ];
}

/** 格式化重置时间显示（相对时间 + 绝对时间） */
export function formatCodexResetTime(
  resetTime: number | undefined,
  _t: Translate,
): string {
  const normalizedResetTime = normalizeCodexUnixSeconds(resetTime);
  if (!normalizedResetTime) return "";

  const now = Math.floor(Date.now() / 1000);
  const diff = normalizedResetTime - now;
  const absolute = formatCodexResetTimeAbsolute(normalizedResetTime);

  if (diff <= 0) return absolute;

  const totalMinutes = Math.floor(diff / 60);
  const days = Math.floor(totalMinutes / (60 * 24));
  const hours = Math.floor((totalMinutes % (60 * 24)) / 60);
  const minutes = totalMinutes % 60;

  let parts = [];
  if (days > 0) parts.push(`${days}d`);
  if (hours > 0) parts.push(`${hours}h`);
  if (minutes > 0) parts.push(`${minutes}m`);

  const relative = parts.length > 0 ? parts.join(" ") : "<1m";
  return `${relative} (${absolute})`;
}

export function formatCodexResetTimeAbsolute(
  resetTime: number | undefined,
): string {
  const normalizedResetTime = normalizeCodexUnixSeconds(resetTime);
  if (!normalizedResetTime) return "";

  const resetDate = new Date(normalizedResetTime * 1000);

  const pad = (value: number) => String(value).padStart(2, "0");
  const month = pad(resetDate.getMonth() + 1);
  const day = pad(resetDate.getDate());
  const hours = pad(resetDate.getHours());
  const minutes = pad(resetDate.getMinutes());

  return `${month}/${day} ${hours}:${minutes}`;
}
