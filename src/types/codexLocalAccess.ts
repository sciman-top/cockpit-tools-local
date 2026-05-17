export type CodexLocalAccessAddressKind = 'local' | 'lan';

export type CodexLocalAccessRoutingStrategy =
  | 'auto'
  | 'quota_high_first'
  | 'quota_low_first'
  | 'plan_high_first'
  | 'plan_low_first'
  | 'expiry_soon_first';

export type CodexRuntimeIntegrationMode =
  | 'direct_projection'
  | 'cockpit_api_service';

export type CodexRuntimeAccountKind = 'oauth' | 'api' | 'unknown';
export type CodexLocalApiFallbackMode = 'disabled' | 'next_request_only' | 'unknown';

export interface CodexRuntimeModeState {
  mode: CodexRuntimeIntegrationMode;
  accountKind: CodexRuntimeAccountKind;
  currentAccountId?: string | null;
  updatedAt: number;
}

export interface CodexLocalApiLoggingConfig {
  redactSensitiveValues: boolean;
  includeRequestId: boolean;
  includeAccountHash: boolean;
  includeRoute: boolean;
  includeModel: boolean;
  includeLatency: boolean;
  includePromptResponse: boolean;
  includeRawUpstreamBody: boolean;
}

export interface CodexLocalApiSafetyConfig {
  schemaVersion: number;
  hardenedLocalMode: boolean;
  maxConcurrentRequests: number;
  minRequestIntervalSeconds: number;
  maxQueueWaitSeconds: number;
  requestTimeoutSeconds: number;
  maxRequestBodyMb: number;
  maxRetries: number;
  maxRetryAccounts: number;
  fallbackMode: CodexLocalApiFallbackMode;
  logging: CodexLocalApiLoggingConfig;
}

export interface CodexLocalAccessCollection {
  enabled: boolean;
  port: number;
  apiKey: string;
  safetyConfig: CodexLocalApiSafetyConfig;
  routingStrategy: CodexLocalAccessRoutingStrategy;
  restrictFreeAccounts: boolean;
  followCurrentAccount: boolean;
  accountIds: string[];
  createdAt: number;
  updatedAt: number;
}

export interface CodexLocalAccessUsageStats {
  requestCount: number;
  successCount: number;
  failureCount: number;
  totalLatencyMs: number;
  inputTokens: number;
  outputTokens: number;
  totalTokens: number;
  cachedTokens: number;
  reasoningTokens: number;
}

export interface CodexLocalAccessAccountStats {
  accountId: string;
  email: string;
  usage: CodexLocalAccessUsageStats;
  updatedAt: number;
}

export interface CodexLocalAccessStatsWindow {
  since: number;
  updatedAt: number;
  totals: CodexLocalAccessUsageStats;
  accounts: CodexLocalAccessAccountStats[];
}

export interface CodexLocalAccessStats {
  since: number;
  updatedAt: number;
  totals: CodexLocalAccessUsageStats;
  accounts: CodexLocalAccessAccountStats[];
  daily: CodexLocalAccessStatsWindow;
  weekly: CodexLocalAccessStatsWindow;
  monthly: CodexLocalAccessStatsWindow;
}

export interface CodexLocalAccessHealthSummary {
  schemaVersion: number;
  updatedAt: number;
  unavailable: boolean;
  loadError: string | null;
  healthyCount: number;
  estimatedAvailableCount: number;
  coolingCount: number;
  exhaustedCount: number;
  authSuspectCount: number;
  manualRequiredCount: number;
  disabledCount: number;
  activeModelCooldownCount: number;
  stickyAccountHash: string | null;
  stickyReason: string | null;
  stickyExpiresAtMs: number | null;
  nearestCooldownUntilMs: number | null;
  lastErrorType: string | null;
  lastStatus: number | null;
  lastRequestId: string | null;
}

export interface CodexLocalAccessState {
  collection: CodexLocalAccessCollection | null;
  running: boolean;
  apiPortUrl: string | null;
  baseUrl: string | null;
  lanBaseUrl: string | null;
  modelIds: string[];
  lastError: string | null;
  memberCount: number;
  effectiveAccountIds: string[];
  stats: CodexLocalAccessStats;
  health: CodexLocalAccessHealthSummary;
}

export interface CodexLocalAccessPortCleanupResult {
  killedCount: number;
  state: CodexLocalAccessState;
}
