export type CodexLocalAccessAddressKind = 'local' | 'lan';

export type CodexLocalAccessRoutingStrategy =
  | 'auto'
  | 'quota_high_first'
  | 'quota_low_first'
  | 'plan_high_first'
  | 'plan_low_first'
  | 'expiry_soon_first';

export type CodexRuntimeIntegrationMode = 'direct_projection' | 'gateway_litellm';

export type CodexRuntimeAccountKind = 'oauth' | 'api' | 'unknown';

export interface CodexRuntimeModeState {
  mode: CodexRuntimeIntegrationMode;
  accountKind: CodexRuntimeAccountKind;
  currentAccountId?: string | null;
  updatedAt: number;
}

export interface CodexLocalAccessCollection {
  enabled: boolean;
  port: number;
  apiKey: string;
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

export interface CodexLocalAccessState {
  collection: CodexLocalAccessCollection | null;
  running: boolean;
  apiPortUrl: string | null;
  baseUrl: string | null;
  lanBaseUrl: string | null;
  modelIds: string[];
  lastError: string | null;
  memberCount: number;
  stats: CodexLocalAccessStats;
}

export interface CodexLocalAccessPortCleanupResult {
  killedCount: number;
  state: CodexLocalAccessState;
}
