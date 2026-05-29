import {
  useEffect,
  useMemo,
  useRef,
  useState,
} from 'react';
import {
  Activity,
  Check,
  CircleAlert,
  Clock,
  Copy,
  Eye,
  EyeOff,
  FolderPlus,
  Gauge,
  Info,
  KeyRound,
  Pause,
  Power,
  RefreshCw,
  Search,
  Server,
  ShieldCheck,
  Trash2,
  Wrench,
  X,
} from 'lucide-react';
import { confirm as confirmDialog } from '@tauri-apps/plugin-dialog';
import { useTranslation } from 'react-i18next';
import type { CodexAccount } from '../types/codex';
import type { CodexAccountGroup } from '../services/codexAccountGroupService';
import type {
  CodexLocalAccessAccountHealthView,
  CodexLocalAccessAddressKind,
  CodexLocalAccessRoutingStrategy,
  CodexLocalAccessState,
  CodexLocalApiSafetyConfig,
  CodexLocalApiSafetyPresetId,
  CodexLocalAccessStatsWindow,
  CodexRuntimeIntegrationMode,
  CodexRuntimeModeState,
} from '../types/codexLocalAccess';
import {
  getCodexPlanFilterKey,
  getCodexQuotaIssueInfo,
  isCodexAccountErrorState,
  isCodexExplicitFreePlanType,
  isCodexApiKeyAccount,
  shouldShowCodexQuotaIssueNotice,
} from '../types/codex';
import {
  buildCodexAccountPresentation,
  buildQuotaPreviewLines,
} from '../presentation/platformAccountPresentation';
import { buildValidAccountsFilterOption, splitValidityFilterValues } from '../utils/accountValidityFilter';
import {
  formatCodexQuotaPoolPercent,
  summarizeCodexQuotaPool,
  type CodexQuotaPoolItem,
} from '../utils/codexQuotaPool';
import { AccountTagFilterDropdown } from './AccountTagFilterDropdown';
import {
  MultiSelectFilterDropdown,
  type MultiSelectFilterOption,
} from './MultiSelectFilterDropdown';
import { SingleSelectDropdown } from './SingleSelectDropdown';
import {
  areStringArraysEqual,
  normalizeAccountOrder,
  normalizeSelectedAccountOrder,
} from '../utils/accountOrder';
import {
  sortCodexLocalAccessAccountsForStableDisplay,
  sortCodexLocalAccessAccountsForScheduling,
} from '../utils/codexAccountSort';
import { isCodexLocalAccessQuotaHealthIssue } from '../utils/codexLocalAccessHealth';
import './GroupAccountPickerModal.css';
import './CodexLocalAccessModal.css';

interface CodexLocalAccessModalProps {
  isOpen: boolean;
  mode: 'panel' | 'members';
  state: CodexLocalAccessState | null;
  runtimeMode: CodexRuntimeModeState | null;
  addressKind: CodexLocalAccessAddressKind;
  addressOptions: Array<{ value: string; label: string }>;
  onAddressKindChange: (value: string) => void;
  accounts: CodexAccount[];
  accountGroups: CodexAccountGroup[];
  initialSelectedIds: string[];
  currentAccountId?: string | null;
  maskAccountText: (value?: string | null) => string;
  onClose: () => void;
  onSaveAccounts: (payload: {
    accountIds: string[];
    restrictFreeAccounts: boolean;
  }) => Promise<unknown> | unknown;
  onRefreshAccounts: (
    accountIds: string[],
  ) =>
    | Promise<{ successCount: number; total: number }>
    | { successCount: number; total: number };
  onClearStats: () => Promise<unknown> | unknown;
  onRefreshStats: () => Promise<unknown> | unknown;
  onRecoverHealth: (
    accountId: string,
    model?: string | null,
  ) => Promise<unknown> | unknown;
  onPauseHealth: (accountId: string) => Promise<unknown> | unknown;
  onUpdatePort: (port: number) => Promise<unknown> | unknown;
  onUpdateRoutingStrategy: (
    strategy: CodexLocalAccessRoutingStrategy,
  ) => Promise<unknown> | unknown;
  onApplySafetyPreset: (
    preset: CodexLocalApiSafetyPresetId,
  ) => Promise<unknown> | unknown;
  onSetRuntimeMode: (
    mode: CodexRuntimeIntegrationMode,
    options?: { force?: boolean },
  ) => Promise<unknown> | unknown;
  onRotateApiKey: () => Promise<unknown> | unknown;
  onKillPort: () => Promise<unknown> | unknown;
  onToggleEnabled: () => Promise<unknown> | unknown;
  onTest: () => Promise<number> | number;
  saving: boolean;
  refreshing: boolean;
  testing: boolean;
  starting: boolean;
  portCleanupBusy: boolean;
}

type StatsRangeKey = 'daily' | 'weekly' | 'monthly';
type CopyableField = 'apiPortUrl' | 'baseUrl' | 'apiKey' | 'modelId';
type LocalAccessAccountIssueIcon = 'alert' | 'clock' | 'info' | 'pause';

interface LocalAccessAccountIssueMeta {
  badge: string;
  detail: string;
  className: string;
  icon: LocalAccessAccountIssueIcon;
  blocksSelection: boolean;
  canPause: boolean;
}

const CODEX_LOCAL_ACCESS_STATS_RANGE_STORAGE_KEY =
  'agtools.codex.local_access.stats_range.v1';

function isLocalAccessAuthHealthIssue(
  health?: CodexLocalAccessAccountHealthView | null,
): boolean {
  if (!health) return false;
  const lastErrorType = (health.lastErrorType || '').trim().toLowerCase();
  return (
    health.manualRequired ||
    health.status === 'manual_required' ||
    health.status === 'auth_suspect' ||
    health.lastStatus === 401 ||
    health.lastStatus === 403 ||
    lastErrorType === 'auth_error'
  );
}

function isLocalAccessSelectionBlockedByIssue(
  account: CodexAccount,
  health?: CodexLocalAccessAccountHealthView | null,
): boolean {
  return Boolean(
    health?.status === 'disabled' ||
      isLocalAccessAuthHealthIssue(health) ||
      isCodexAccountErrorState(account),
  );
}

function normalizeStatsRangeKey(value: string | null | undefined): StatsRangeKey {
  if (value === 'weekly' || value === 'monthly') {
    return value;
  }
  return 'daily';
}

function readStoredStatsRange(): StatsRangeKey {
  try {
    return normalizeStatsRangeKey(localStorage.getItem(CODEX_LOCAL_ACCESS_STATS_RANGE_STORAGE_KEY));
  } catch {
    return 'daily';
  }
}

function persistStatsRange(value: StatsRangeKey): void {
  try {
    localStorage.setItem(CODEX_LOCAL_ACCESS_STATS_RANGE_STORAGE_KEY, value);
  } catch {
    // ignore storage write failures
  }
}

function resolveSafetyPresetId(
  config: CodexLocalApiSafetyConfig | null | undefined,
): CodexLocalApiSafetyPresetId | 'custom' {
  if (!config) return 'custom';
  const queueWaitCoversStartInterval =
    config.maxQueueWaitSeconds >= Math.min(config.minRequestIntervalSeconds + 1, 300);
  const isBaseHardened =
    config.hardenedLocalMode &&
    config.maxConcurrentRequests === 1 &&
    queueWaitCoversStartInterval &&
    config.requestTimeoutSeconds === 600 &&
    config.maxRequestBodyMb === 64 &&
    config.maxRetries === 1 &&
    config.logging?.redactSensitiveValues === true &&
    config.logging?.includePromptResponse === false &&
    config.logging?.includeRawUpstreamBody === false;
  if (!isBaseHardened) return 'custom';
  if (
    config.minRequestIntervalSeconds === 60 &&
    config.maxRetryAccounts === 2 &&
    config.fallbackMode === 'disabled'
  ) {
    return 'maximum_safety';
  }
  if (
    config.minRequestIntervalSeconds === 20 &&
    config.maxRetryAccounts === 2 &&
    config.fallbackMode === 'disabled'
  ) {
    return 'balanced_self_use';
  }
  if (
    config.minRequestIntervalSeconds === 30 &&
    config.maxRetryAccounts === 2 &&
    config.fallbackMode === 'next_request_only'
  ) {
    return 'quota_drain_careful';
  }
  return 'custom';
}

function formatCompactNumber(value: number): string {
  return new Intl.NumberFormat('en', {
    notation: value >= 1000 ? 'compact' : 'standard',
    maximumFractionDigits: value >= 1000 ? 1 : 0,
  }).format(value || 0);
}

function formatLatencyMs(value: number): string {
  if (!Number.isFinite(value) || value <= 0) return '--';
  if (value >= 1000) return `${(value / 1000).toFixed(2)}s`;
  return `${Math.round(value)}ms`;
}

function formatTimestampMs(value?: number | null): string {
  if (!value || !Number.isFinite(value)) return '--';
  return new Date(value).toLocaleString();
}

function formatQuotaPoolLabel(
  baseLabel: string,
  pool: CodexQuotaPoolItem,
  hourlyLabel: string,
  weeklyLabel: string,
): string {
  return `${baseLabel} · ${hourlyLabel} ${formatCodexQuotaPoolPercent(pool.hourly)} · ${weeklyLabel} ${formatCodexQuotaPoolPercent(pool.weekly)}`;
}

export function CodexLocalAccessModal({
  isOpen,
  mode,
  state,
  runtimeMode,
  addressKind,
  addressOptions,
  onAddressKindChange,
  accounts,
  accountGroups,
  initialSelectedIds,
  currentAccountId,
  maskAccountText,
  onClose,
  onSaveAccounts,
  onRefreshAccounts,
  onClearStats,
  onRefreshStats,
  onRecoverHealth,
  onPauseHealth,
  onUpdatePort,
  onUpdateRoutingStrategy,
  onApplySafetyPreset,
  onSetRuntimeMode,
  onRotateApiKey,
  onKillPort,
  onToggleEnabled,
  onTest,
  saving,
  refreshing,
  testing,
  starting,
  portCleanupBusy,
}: CodexLocalAccessModalProps) {
  const { t } = useTranslation();
  const [query, setQuery] = useState('');
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [selectedOrder, setSelectedOrder] = useState<string[]>([]);
  const [memberRemovalSelected, setMemberRemovalSelected] = useState<Set<string>>(new Set());
  const [filterTypes, setFilterTypes] = useState<string[]>([]);
  const [tagFilter, setTagFilter] = useState<string[]>([]);
  const [groupFilter, setGroupFilter] = useState<string[]>([]);
  const [restrictFreeAccounts, setRestrictFreeAccounts] = useState(false);
  const [error, setError] = useState('');
  const [notice, setNotice] = useState('');
  const [portInput, setPortInput] = useState('');
  const [keyVisible, setKeyVisible] = useState(false);
  const [copiedField, setCopiedField] = useState<CopyableField | null>(null);
  const [selectedModelId, setSelectedModelId] = useState('');
  const [statsRange, setStatsRange] = useState<StatsRangeKey>(() => readStoredStatsRange());
  const [statsRefreshing, setStatsRefreshing] = useState(false);
  const selectAllCheckboxRef = useRef<HTMLInputElement | null>(null);
  const searchInputRef = useRef<HTMLInputElement | null>(null);
  const draftInitKeyRef = useRef<string | null>(null);

  const collection = state?.collection ?? null;
  const apiPortUrl = state?.apiPortUrl ?? '';
  const baseUrl = state?.baseUrl ?? '';
  const displayBaseUrl = baseUrl;
  const apiKeyTitle =
    collection && keyVisible
      ? collection.apiKey
      : t('codex.localAccess.hiddenKeyTitle', '密钥已隐藏');
  const modelIds = state?.modelIds ?? [];
  const stats = state?.stats;
  const statsRangeOptions = useMemo(
    () =>
      [
        { key: 'daily', label: t('codex.localAccess.statsRange.daily', '日') },
        { key: 'weekly', label: t('codex.localAccess.statsRange.weekly', '周') },
        { key: 'monthly', label: t('codex.localAccess.statsRange.monthly', '月') },
      ] satisfies Array<{ key: StatsRangeKey; label: string }>,
    [t],
  );
  const quotaPoolLabels = useMemo(
    () => ({
      hourly: t('codex.localAccess.quotaPool.hourlyShort', '5h'),
      weekly: t('codex.localAccess.quotaPool.weeklyShort', '周'),
      title: t('codex.localAccess.quotaPool.title', '额度池'),
    }),
    [t],
  );
  const selectedStatsWindow = useMemo<CodexLocalAccessStatsWindow | null>(() => {
    if (!stats) return null;
    return stats[statsRange];
  }, [stats, statsRange]);
  const selectedTotals = selectedStatsWindow?.totals;
  const health = state?.health ?? null;
  const concurrencyDiagnostics = state?.concurrencyDiagnostics ?? null;
  const accountHealthById = useMemo(() => {
    const next = new Map<string, CodexLocalAccessAccountHealthView>();
    (health?.accounts ?? []).forEach((item) => {
      const accountId = item.accountId.trim();
      if (accountId) {
        next.set(accountId, item);
      }
    });
    return next;
  }, [health?.accounts]);
  const routingStrategy = collection?.routingStrategy ?? 'auto';
  const safetyPresetId = resolveSafetyPresetId(collection?.safetyConfig);
  const maxRetryAccountsManualOptIn =
    (collection?.safetyConfig.maxRetryAccounts ?? 2) > 2;
  const safetyPresetOptions = useMemo(
    () =>
      [
        {
          id: 'maximum_safety',
          label: t('codex.localAccess.safetyPreset.maximumSafety', '最高安全'),
          desc: t('codex.localAccess.safetyPreset.maximumSafetyDesc', '1 并发 · 60s · 2账号'),
        },
        {
          id: 'balanced_self_use',
          label: t('codex.localAccess.safetyPreset.balancedSelfUse', '自用均衡'),
          desc: t(
            'codex.localAccess.safetyPreset.balancedSelfUseDesc',
            '1 并发 · 20s · 2账号 · 手动 opt-in',
          ),
        },
        {
          id: 'quota_drain_careful',
          label: t('codex.localAccess.safetyPreset.quotaDrainCareful', '谨慎消耗'),
          desc: t(
            'codex.localAccess.safetyPreset.quotaDrainCarefulDesc',
            '1 并发 · 30s · 2账号 · 手动 opt-in',
          ),
        },
      ] satisfies Array<{
        id: CodexLocalApiSafetyPresetId;
        label: string;
        desc: string;
      }>,
    [t],
  );
  const selectedRuntimeMode = runtimeMode?.mode ?? 'direct_projection';
  const modelIdOptions = useMemo(
    () => modelIds.map((modelId) => ({ value: modelId, label: modelId })),
    [modelIds],
  );
  const avgLatencyMs =
    selectedTotals && selectedTotals.requestCount > 0
      ? selectedTotals.totalLatencyMs / selectedTotals.requestCount
      : 0;
  const successRate =
    selectedTotals && selectedTotals.requestCount > 0
      ? Math.round((selectedTotals.successCount / selectedTotals.requestCount) * 100)
      : 0;
  const healthMetricItems = useMemo(
    () => [
      {
        key: 'healthy',
        label: t('codex.localAccess.health.healthy', '健康'),
        value: (health?.healthyCount ?? 0) + (health?.estimatedAvailableCount ?? 0),
      },
      {
        key: 'cooling',
        label: t('codex.localAccess.health.cooling', '冷却'),
        value: health?.coolingCount ?? 0,
      },
      {
        key: 'exhausted',
        label: t('codex.localAccess.health.exhausted', '额度耗尽'),
        value: health?.exhaustedCount ?? 0,
      },
      {
        key: 'auth',
        label: t('codex.localAccess.health.auth', '认证'),
        value: health?.authSuspectCount ?? 0,
      },
      {
        key: 'manual',
        label: t('codex.localAccess.health.manual', '人工'),
        value: health?.manualRequiredCount ?? 0,
      },
      {
        key: 'manualPaused',
        label: t('codex.localAccess.health.manualPaused', '手动暂停'),
        value: health?.disabledCount ?? 0,
      },
      {
        key: 'modelCooldown',
        label: t('codex.localAccess.health.modelCooldown', '模型冷却'),
        value: health?.activeModelCooldownCount ?? 0,
      },
    ],
    [health, t],
  );
  const concurrencyMetricItems = useMemo(() => {
    if (!concurrencyDiagnostics) return [];
    const auditWindowMinutes = Math.max(
      1,
      Math.round(concurrencyDiagnostics.auditWindowMs / 60_000),
    );
    return [
      {
        key: 'active',
        label: t('codex.localAccess.diagnostics.activeRequests', '执行中请求'),
        value: `${formatCompactNumber(concurrencyDiagnostics.activeRequestCount)}/${formatCompactNumber(
          concurrencyDiagnostics.maxConcurrentRequests,
        )}`,
      },
      {
        key: 'streams',
        label: t('codex.localAccess.diagnostics.activeStreams', '活跃流'),
        value: formatCompactNumber(concurrencyDiagnostics.activeStreamCount),
      },
      {
        key: 'interval',
        label: t('codex.localAccess.diagnostics.startInterval', '启动间隔'),
        value:
          concurrencyDiagnostics.startIntervalRemainingMs > 0
            ? formatLatencyMs(concurrencyDiagnostics.startIntervalRemainingMs)
            : '0ms',
      },
      {
        key: 'requests',
        label: t('codex.localAccess.diagnostics.recentRequests', {
          minutes: auditWindowMinutes,
          defaultValue: '近 {{minutes}} 分钟请求',
        }),
        value: formatCompactNumber(concurrencyDiagnostics.recentRequestCount),
      },
      {
        key: 'localBackpressure',
        label: t('codex.localAccess.diagnostics.localBackpressure', '本地排队'),
        value: formatCompactNumber(concurrencyDiagnostics.recentLocalBackpressureCount),
      },
      {
        key: 'poolWait',
        label: t('codex.localAccess.diagnostics.poolWait', '号池等待'),
        value: formatCompactNumber(concurrencyDiagnostics.recentPoolWaitCount),
      },
      {
        key: 'upstreamLimit',
        label: t('codex.localAccess.diagnostics.upstreamLimit', '上游限额'),
        value: formatCompactNumber(concurrencyDiagnostics.recentUpstreamLimitCount),
      },
      {
        key: 'streamError',
        label: t('codex.localAccess.diagnostics.streamError', '流错误'),
        value: formatCompactNumber(concurrencyDiagnostics.recentStreamErrorCount),
      },
    ];
  }, [concurrencyDiagnostics, t]);
  const concurrencyDiagnosis = useMemo(() => {
    if (!concurrencyDiagnostics) return null;
    if (concurrencyDiagnostics.auditLoadError) {
      return {
        tone: 'warning',
        text: t('codex.localAccess.diagnostics.auditLoadError', {
          error: concurrencyDiagnostics.auditLoadError,
          defaultValue: '审计日志读取降级：{{error}}',
        }),
      };
    }
    if (
      concurrencyDiagnostics.activeStreamCount > 0 &&
      concurrencyDiagnostics.recentPoolWaitCount > 0
    ) {
      return {
        tone: 'warning',
        text: t(
          'codex.localAccess.diagnostics.hintActiveStreamPoolWait',
          '有旧任务仍占用活跃流，新任务可能在等待旧流结束或同账号恢复。',
        ),
      };
    }
    if (concurrencyDiagnostics.recentLocalBackpressureCount > 0) {
      return {
        tone: 'warning',
        text: t(
          'codex.localAccess.diagnostics.hintLocalBackpressure',
          'Cockpit 本地 admission 正在限速，这是保护上游与账号池的排队信号。',
        ),
      };
    }
    if (concurrencyDiagnostics.recentUpstreamLimitCount > 0) {
      return {
        tone: 'danger',
        text: t(
          'codex.localAccess.diagnostics.hintUpstreamLimit',
          '账号、模型 quota 或 cooldown 更可能是当前瓶颈，优先看健康状态与冷却时间。',
        ),
      };
    }
    if (
      concurrencyDiagnostics.activeRequestCount >=
        concurrencyDiagnostics.maxConcurrentRequests ||
      concurrencyDiagnostics.startIntervalRemainingMs > 0
    ) {
      return {
        tone: 'warning',
        text: t(
          'codex.localAccess.diagnostics.hintAdmission',
          '请求 admission 正在限速，新的请求会等到并发或启动间隔释放。',
        ),
      };
    }
    if (concurrencyDiagnostics.recentStreamErrorCount > 0) {
      return {
        tone: 'warning',
        text: t(
          'codex.localAccess.diagnostics.hintStreamError',
          '近窗口内出现流错误，若 Codex App 仍显示 Thinking，需要区分后台任务是否已继续执行。',
        ),
      };
    }
    return {
      tone: 'neutral',
      text: t(
        'codex.localAccess.diagnostics.hintNeutral',
        'Cockpit 侧没有明显阻塞；若 Codex App 仍停在 Thinking，更可能是 App/session queue 或 UI 状态。',
      ),
    };
  }, [concurrencyDiagnostics, t]);
  const actionBusy =
    saving || refreshing || testing || starting || portCleanupBusy || statsRefreshing;
  const summaryStats = useMemo(
    () => [
      {
        key: 'requests',
        label: t('codex.localAccess.stats.requests', '总请求数'),
        value: formatCompactNumber(selectedTotals?.requestCount ?? 0),
        detail: t('codex.localAccess.stats.requestsDetail', {
          success: formatCompactNumber(selectedTotals?.successCount ?? 0),
          failed: formatCompactNumber(selectedTotals?.failureCount ?? 0),
          defaultValue: '成功 {{success}} / 失败 {{failed}}',
        }),
      },
      {
        key: 'tokens',
        label: t('codex.localAccess.stats.tokens', '总 Token 数'),
        value: formatCompactNumber(selectedTotals?.totalTokens ?? 0),
        detail: t('codex.localAccess.stats.tokensDetail', {
          input: formatCompactNumber(selectedTotals?.inputTokens ?? 0),
          output: formatCompactNumber(selectedTotals?.outputTokens ?? 0),
          defaultValue: '输入 {{input}} / 输出 {{output}}',
        }),
      },
      {
        key: 'specialTokens',
        label: t('codex.localAccess.stats.specialTokens', '缓存 / 思考'),
        value: formatCompactNumber(
          (selectedTotals?.cachedTokens ?? 0) + (selectedTotals?.reasoningTokens ?? 0),
        ),
        detail: t('codex.localAccess.stats.specialTokensDetail', {
          cached: formatCompactNumber(selectedTotals?.cachedTokens ?? 0),
          reasoning: formatCompactNumber(selectedTotals?.reasoningTokens ?? 0),
          defaultValue: '缓存 {{cached}} / 思考 {{reasoning}}',
        }),
      },
      {
        key: 'latency',
        label: t('codex.localAccess.stats.avgLatency', '平均延迟'),
        value: formatLatencyMs(avgLatencyMs),
        detail: t('codex.localAccess.stats.successRate', {
          rate: successRate,
          defaultValue: '成功率 {{rate}}%',
        }),
      },
    ],
    [avgLatencyMs, selectedTotals, successRate, t],
  );

  const serviceAccounts = useMemo(
    () => accounts.filter((account) => !isCodexApiKeyAccount(account)),
    [accounts],
  );
  const accountById = useMemo(
    () => new Map(serviceAccounts.map((account) => [account.id, account])),
    [serviceAccounts],
  );
  const quotaPoolSummary = useMemo(
    () => summarizeCodexQuotaPool(serviceAccounts),
    [serviceAccounts],
  );
  const currentQuotaPoolSummary = useMemo(() => {
    const accountIds = new Set(collection?.accountIds ?? []);
    return summarizeCodexQuotaPool(serviceAccounts.filter((account) => accountIds.has(account.id)));
  }, [collection?.accountIds, serviceAccounts]);
  const serviceAccountIdSet = useMemo(
    () => new Set(serviceAccounts.map((account) => account.id)),
    [serviceAccounts],
  );
  const normalizedInitialSelectedIds = useMemo(
    () => initialSelectedIds.filter((accountId) => serviceAccountIdSet.has(accountId)),
    [initialSelectedIds, serviceAccountIdSet],
  );
  const persistedMemberIdSet = useMemo(
    () => new Set(collection?.accountIds ?? normalizedInitialSelectedIds),
    [collection?.accountIds, normalizedInitialSelectedIds],
  );
  const serviceAccountIds = useMemo(
    () => serviceAccounts.map((account) => account.id),
    [serviceAccounts],
  );
  const selectedOrderForSave = useMemo(() => {
    const normalized = normalizeAccountOrder(selectedOrder, serviceAccountIds);
    return normalized.filter((accountId) => selected.has(accountId));
  }, [selected, selectedOrder, serviceAccountIds]);

  useEffect(() => {
    if (!isOpen) {
      draftInitKeyRef.current = null;
      return;
    }
    if (draftInitKeyRef.current === mode) return;
    draftInitKeyRef.current = mode;
    setQuery('');
    setSelected(new Set(normalizedInitialSelectedIds));
    setSelectedOrder(normalizedInitialSelectedIds);
    setMemberRemovalSelected(new Set());
    setFilterTypes([]);
    setTagFilter([]);
    setGroupFilter([]);
    setRestrictFreeAccounts(collection?.restrictFreeAccounts ?? false);
    setError('');
    setNotice('');
    setKeyVisible(false);
    setCopiedField(null);
    setPortInput(collection?.port ? String(collection.port) : '');
    if (mode === 'members') {
      window.setTimeout(() => {
        searchInputRef.current?.focus();
      }, 0);
    }
  }, [collection?.port, collection?.restrictFreeAccounts, isOpen, mode, normalizedInitialSelectedIds]);

  useEffect(() => {
    setMemberRemovalSelected((prev) => {
      if (prev.size === 0) return prev;
      const next = new Set<string>();
      for (const accountId of prev) {
        if (selected.has(accountId)) {
          next.add(accountId);
        }
      }
      return next.size === prev.size ? prev : next;
    });
  }, [selected]);

  useEffect(() => {
    if (modelIds.length === 0) {
      setSelectedModelId('');
      return;
    }
    setSelectedModelId((current) => (modelIds.includes(current) ? current : modelIds[0]));
  }, [modelIds]);

  useEffect(() => {
    persistStatsRange(statsRange);
  }, [statsRange]);

  const normalizeTag = (value: string) => value.trim().toLowerCase();

  const availableTags = useMemo(() => {
    const next = new Set<string>();
    serviceAccounts.forEach((account) => {
      (account.tags || []).forEach((tag) => {
        const trimmed = tag.trim();
        if (trimmed) next.add(trimmed);
      });
    });
    return Array.from(next).sort((left, right) => left.localeCompare(right));
  }, [serviceAccounts]);

  const groupIdsByAccountId = useMemo(() => {
    const next = new Map<string, Set<string>>();
    accountGroups.forEach((group) => {
      group.accountIds.forEach((accountId) => {
        const current = next.get(accountId) ?? new Set<string>();
        current.add(group.id);
        next.set(accountId, current);
      });
    });
    return next;
  }, [accountGroups]);

  const groupNameByAccountId = useMemo(() => {
    const next = new Map<string, string[]>();
    accountGroups.forEach((group) => {
      group.accountIds.forEach((accountId) => {
        const current = next.get(accountId) ?? [];
        current.push(group.name);
        next.set(accountId, current);
      });
    });
    return next;
  }, [accountGroups]);

  const groupFilterOptions = useMemo<MultiSelectFilterOption[]>(
    () =>
      accountGroups
        .map((group) => ({
          value: group.id,
          label: `${group.name} (${group.accountIds.length})`,
        }))
        .sort((left, right) => left.label.localeCompare(right.label)),
    [accountGroups],
  );

  const tierCounts = useMemo(() => {
    const counts = { all: serviceAccounts.length, VALID: 0, FREE: 0, PLUS: 0, PRO: 0, TEAM: 0, ENTERPRISE: 0, ERROR: 0 };
    serviceAccounts.forEach((account) => {
      const hasAccountError = isLocalAccessSelectionBlockedByIssue(
        account,
        accountHealthById.get(account.id),
      );
      if (!hasAccountError) {
        counts.VALID += 1;
      }
      const tier = getCodexPlanFilterKey(account);
      if (tier in counts) {
        counts[tier as keyof typeof counts] += 1;
      }
      if (hasAccountError) {
        counts.ERROR += 1;
      }
    });
    return counts;
  }, [accountHealthById, serviceAccounts]);

  const allTierFilterLabel = useMemo(
    () =>
      formatQuotaPoolLabel(
        t('common.shared.filter.all', { count: tierCounts.all }),
        quotaPoolSummary.all,
        quotaPoolLabels.hourly,
        quotaPoolLabels.weekly,
      ),
    [quotaPoolLabels.hourly, quotaPoolLabels.weekly, quotaPoolSummary.all, t, tierCounts.all],
  );

  const tierFilterOptions = useMemo<MultiSelectFilterOption[]>(
    () => [
      {
        value: 'FREE',
        label: formatQuotaPoolLabel(
          `FREE (${tierCounts.FREE})`,
          quotaPoolSummary.byPlan.FREE,
          quotaPoolLabels.hourly,
          quotaPoolLabels.weekly,
        ),
      },
      {
        value: 'PLUS',
        label: formatQuotaPoolLabel(
          `PLUS (${tierCounts.PLUS})`,
          quotaPoolSummary.byPlan.PLUS,
          quotaPoolLabels.hourly,
          quotaPoolLabels.weekly,
        ),
      },
      {
        value: 'PRO',
        label: formatQuotaPoolLabel(
          `PRO (${tierCounts.PRO})`,
          quotaPoolSummary.byPlan.PRO,
          quotaPoolLabels.hourly,
          quotaPoolLabels.weekly,
        ),
      },
      {
        value: 'TEAM',
        label: formatQuotaPoolLabel(
          `TEAM (${tierCounts.TEAM})`,
          quotaPoolSummary.byPlan.TEAM,
          quotaPoolLabels.hourly,
          quotaPoolLabels.weekly,
        ),
      },
      {
        value: 'ENTERPRISE',
        label: formatQuotaPoolLabel(
          `ENTERPRISE (${tierCounts.ENTERPRISE})`,
          quotaPoolSummary.byPlan.ENTERPRISE,
          quotaPoolLabels.hourly,
          quotaPoolLabels.weekly,
        ),
      },
      { value: 'ERROR', label: `ERROR (${tierCounts.ERROR})` },
      buildValidAccountsFilterOption(t, tierCounts.VALID),
    ],
    [quotaPoolLabels.hourly, quotaPoolLabels.weekly, quotaPoolSummary.byPlan, t, tierCounts],
  );

  const visibleAccounts = useMemo(() => {
    const queryText = query.trim().toLowerCase();
    const sorted = sortCodexLocalAccessAccountsForScheduling(
      serviceAccounts,
      currentAccountId,
    );
    const selectedTags = new Set(tagFilter.map(normalizeTag));
    const selectedGroups = new Set(groupFilter);
    const { requireValidAccounts, selectedTypes } = splitValidityFilterValues(filterTypes);

    return sorted.filter((account) => {
      const presentation = buildCodexAccountPresentation(account, t);
      const displayName = presentation.displayName.toLowerCase();
      const groupNames = (groupNameByAccountId.get(account.id) ?? []).join(' ').toLowerCase();
      const matchesQuery =
        !queryText || displayName.includes(queryText) || groupNames.includes(queryText);
      if (!matchesQuery) return false;

      if (selectedTags.size > 0) {
        const accountTags = (account.tags || []).map(normalizeTag);
        if (!accountTags.some((tag) => selectedTags.has(tag))) {
          return false;
        }
      }

      if (selectedGroups.size > 0) {
        const accountGroupIds = groupIdsByAccountId.get(account.id);
        if (!accountGroupIds || !Array.from(accountGroupIds).some((id) => selectedGroups.has(id))) {
          return false;
        }
      }

      if (
        requireValidAccounts &&
        isLocalAccessSelectionBlockedByIssue(account, accountHealthById.get(account.id))
      ) {
        return false;
      }

      if (selectedTypes.size > 0) {
        const planKey = getCodexPlanFilterKey(account);
        const isBlockedByIssue = isLocalAccessSelectionBlockedByIssue(
          account,
          accountHealthById.get(account.id),
        );
        const matchesType = Array.from(selectedTypes).some((type) => {
          if (type === 'ERROR') return isBlockedByIssue;
          return type === planKey;
        });
        if (!matchesType) {
          return false;
        }
      }

      return true;
    });
  }, [accountHealthById, currentAccountId, filterTypes, groupFilter, groupIdsByAccountId, groupNameByAccountId, query, serviceAccounts, t, tagFilter]);

  const visibleSelectableAccounts = useMemo(
    () =>
      visibleAccounts.filter((account) => {
        if (persistedMemberIdSet.has(account.id)) {
          return false;
        }
        if (
          isLocalAccessSelectionBlockedByIssue(account, accountHealthById.get(account.id)) &&
          !selected.has(account.id)
        ) {
          return false;
        }
        if (!restrictFreeAccounts) return true;
        if (!isCodexExplicitFreePlanType(account.plan_type)) return true;
        return selected.has(account.id);
      }),
    [accountHealthById, persistedMemberIdSet, restrictFreeAccounts, selected, visibleAccounts],
  );

  const selectedVisibleCount = useMemo(
    () =>
      visibleSelectableAccounts.reduce(
        (count, account) => count + (selected.has(account.id) ? 1 : 0),
        0,
      ),
    [selected, visibleSelectableAccounts],
  );

  const allVisibleSelected =
    visibleSelectableAccounts.length > 0 &&
    selectedVisibleCount === visibleSelectableAccounts.length;

  useEffect(() => {
    if (!selectAllCheckboxRef.current) return;
    selectAllCheckboxRef.current.indeterminate =
      selectedVisibleCount > 0 && !allVisibleSelected;
  }, [allVisibleSelected, selectedVisibleCount]);

  const selectionDirty = useMemo(
    () =>
      !areStringArraysEqual(selectedOrderForSave, normalizedInitialSelectedIds) ||
      restrictFreeAccounts !== (collection?.restrictFreeAccounts ?? false),
    [collection?.restrictFreeAccounts, normalizedInitialSelectedIds, restrictFreeAccounts, selectedOrderForSave],
  );

  const allStatsByAccountId = useMemo(() => {
    const next = new Map<string, NonNullable<CodexLocalAccessState['stats']>['accounts'][number]>();
    stats?.accounts.forEach((item) => next.set(item.accountId, item));
    return next;
  }, [stats?.accounts]);

  const selectedMemberAccounts = useMemo(() => {
    const accountsForDisplay = selectedOrderForSave
      .map((accountId) => accountById.get(accountId))
      .filter((account): account is CodexAccount => Boolean(account));
    return sortCodexLocalAccessAccountsForStableDisplay(
      accountsForDisplay,
      currentAccountId,
    );
  }, [accountById, currentAccountId, selectedOrderForSave]);

  const memberRemovalSelectedCount = memberRemovalSelected.size;
  const allMembersSelectedForRemoval =
    selectedMemberAccounts.length > 0 &&
    memberRemovalSelectedCount === selectedMemberAccounts.length;

  const windowStatsByAccountId = useMemo(() => {
    const next = new Map<string, NonNullable<CodexLocalAccessState['stats']>['accounts'][number]>();
    selectedStatsWindow?.accounts.forEach((item) => next.set(item.accountId, item));
    return next;
  }, [selectedStatsWindow?.accounts]);

  const currentMemberStats = useMemo(() => {
    const currentIds = collection?.accountIds ?? [];
    const memberAccounts = currentIds
      .map((accountId) => accountById.get(accountId))
      .filter((account): account is CodexAccount => Boolean(account));
    return sortCodexLocalAccessAccountsForStableDisplay(
      memberAccounts,
      currentAccountId,
    )
      .map((account) => {
        const presentation = buildCodexAccountPresentation(account, t);
        const accountStats = windowStatsByAccountId.get(account.id);
        return {
          account,
          presentation,
          stats: accountStats?.usage ?? null,
        };
      });
  }, [
    accountById,
    collection?.accountIds,
    currentAccountId,
    t,
    windowStatsByAccountId,
  ]);

  const routingStrategyOptions = useMemo(
    () => [
      {
        value: 'auto',
        label: t('codex.localAccess.routingStrategy.auto', '自动（推荐）'),
      },
      {
        value: 'quota_high_first',
        label: t('codex.localAccess.routingStrategy.quotaHighFirst', '优先高配额'),
      },
      {
        value: 'quota_low_first',
        label: t('codex.localAccess.routingStrategy.quotaLowFirst', '优先低配额'),
      },
      {
        value: 'plan_high_first',
        label: t('codex.localAccess.routingStrategy.planHighFirst', '优先高订阅'),
      },
      {
        value: 'plan_low_first',
        label: t('codex.localAccess.routingStrategy.planLowFirst', '优先低订阅'),
      },
      {
        value: 'expiry_soon_first',
        label: t('codex.localAccess.routingStrategy.expirySoonFirst', '优先近到期'),
      },
    ] satisfies Array<{ value: CodexLocalAccessRoutingStrategy; label: string }>,
    [t],
  );
  const runtimeModeOptions = useMemo(
    () =>
      [
        {
          value: 'direct_projection',
          label: t('codex.localAccess.runtimeMode.direct', 'Direct API/OAuth'),
        },
        {
          value: 'cockpit_api_service',
          label: t('codex.localAccess.runtimeMode.gateway', 'Cockpit API Service'),
        },
      ],
    [t],
  );

  const renderQuotaPreview = (
    presentation: ReturnType<typeof buildCodexAccountPresentation>,
    limit = 2,
  ) => {
    const quotaLines = buildQuotaPreviewLines(presentation.quotaItems, limit);
    if (quotaLines.length === 0) {
      return null;
    }

    return (
      <div className="codex-local-access-quota-line">
        {quotaLines.map((line) => (
          <span
            key={line.key}
            className={`codex-local-access-quota-chip ${line.quotaClass}`}
            title={line.title}
          >
            <span className="codex-local-access-quota-dot" />
            <span>{line.text}</span>
          </span>
        ))}
      </div>
    );
  };

  const resolveStoredAccountIssueMeta = (
    account: CodexAccount,
  ): LocalAccessAccountIssueMeta | null => {
    if (account.requires_reauth) {
      const detail =
        account.reauth_reason?.trim() ||
        t('codex.localAccess.accountIssue.reauthRequired', '该账号需要重新授权');
      return {
        badge: t('codex.authError.badge', '授权异常'),
        detail,
        className: 'quota-error',
        icon: 'alert',
        blocksSelection: true,
        canPause: true,
      };
    }

    const issueInfo = getCodexQuotaIssueInfo(account.quota_error);
    if (issueInfo.kind === 'none') {
      return null;
    }
    if (!shouldShowCodexQuotaIssueNotice(account.quota_error)) {
      return null;
    }

    const detail = issueInfo.rawMessage || issueInfo.displayCode;
    if (issueInfo.kind === 'refresh') {
      return {
        badge: t('codex.quotaError.refreshFailedBadge', '刷新失败'),
        detail,
        className: 'quota-refresh',
        icon: 'info',
        blocksSelection: true,
        canPause: false,
      };
    }
    if (issueInfo.kind === 'limited') {
      return {
        badge: t('codex.quotaError.limitBadge', '额度用尽'),
        detail: t('codex.quotaError.limitDetail', {
          code: issueInfo.displayCode || 'usage_limit_reached',
          defaultValue: '额度已用尽或正在冷却：{{code}}',
        }),
        className: 'quota-limited',
        icon: 'clock',
        blocksSelection: false,
        canPause: true,
      };
    }
    return {
      badge: issueInfo.statusCode || t('codex.quotaError.badge', '配额异常'),
      detail,
      className: 'quota-error',
      icon: 'alert',
      blocksSelection: true,
      canPause: true,
    };
  };

  const resolveHealthIssueMeta = (
    healthView?: CodexLocalAccessAccountHealthView | null,
  ): LocalAccessAccountIssueMeta | null => {
    if (!healthView) {
      return null;
    }
    if (healthView.status === 'disabled') {
      return {
        badge: t('codex.localAccess.accountIssue.paused', '已暂停'),
        detail: t(
          'codex.localAccess.accountIssue.pausedDetail',
          '该账号已被手动暂停，不会进入 API 服务调度。',
        ),
        className: 'health-disabled',
        icon: 'pause',
        blocksSelection: true,
        canPause: false,
      };
    }
    if (isLocalAccessAuthHealthIssue(healthView)) {
      const statusText = healthView.lastStatus ? `HTTP ${healthView.lastStatus}` : '';
      const providerText = healthView.lastProviderCode
        ? `${t('codex.localAccess.accountIssue.providerCode', '供应商代码')} ${healthView.lastProviderCode}`
        : '';
      const detailParts = [
        t('codex.localAccess.accountIssue.authDetail', 'API 服务健康状态：授权异常'),
        statusText,
        providerText,
      ].filter(Boolean);
      return {
        badge: healthView.lastStatus
          ? String(healthView.lastStatus)
          : t('codex.authError.badge', '授权异常'),
        detail: detailParts.join(' · '),
        className: 'quota-error',
        icon: 'alert',
        blocksSelection: true,
        canPause: true,
      };
    }
    if (healthView.status === 'exhausted') {
      return {
        badge: t('codex.quotaError.limitBadge', '额度用尽'),
        detail: t('codex.localAccess.accountIssue.exhaustedDetail', 'API 服务健康状态：额度耗尽'),
        className: 'quota-limited',
        icon: 'clock',
        blocksSelection: false,
        canPause: true,
      };
    }
    if (healthView.status === 'cooling_down') {
      return {
        badge: t('codex.localAccess.accountIssue.cooling', '冷却中'),
        detail: t('codex.localAccess.accountIssue.coolingDetail', 'API 服务健康状态：等待冷却结束'),
        className: 'quota-limited',
        icon: 'clock',
        blocksSelection: false,
        canPause: true,
      };
    }
    if (isCodexLocalAccessQuotaHealthIssue(healthView)) {
      const detailParts = [
        t('codex.localAccess.accountIssue.coolingDetail', 'API 服务健康状态：等待冷却结束'),
        healthView.lastStatus ? `HTTP ${healthView.lastStatus}` : '',
        healthView.lastProviderCode
          ? `${t('codex.localAccess.accountIssue.providerCode', '供应商代码')} ${healthView.lastProviderCode}`
          : '',
      ].filter(Boolean);
      return {
        badge: t('codex.quotaError.limitBadge', '额度用尽'),
        detail: detailParts.join(' · '),
        className: 'quota-limited',
        icon: 'clock',
        blocksSelection: false,
        canPause: true,
      };
    }
    if (healthView.activeModelCooldownCount > 0) {
      return {
        badge: t('codex.localAccess.accountIssue.modelCooling', '模型冷却'),
        detail: t(
          'codex.localAccess.accountIssue.modelCoolingDetail',
          '该账号存在模型级 cooldown，当前模型会暂时跳过。',
        ),
        className: 'health-cooling',
        icon: 'clock',
        blocksSelection: false,
        canPause: true,
      };
    }
    return null;
  };

  const resolveLocalAccessAccountIssueMeta = (
    account: CodexAccount,
  ): LocalAccessAccountIssueMeta | null => {
    const healthIssue = resolveHealthIssueMeta(accountHealthById.get(account.id));
    if (healthIssue?.blocksSelection) {
      return healthIssue;
    }
    const storedIssue = resolveStoredAccountIssueMeta(account);
    if (storedIssue?.blocksSelection) {
      return storedIssue;
    }
    return healthIssue || storedIssue;
  };

  const renderAccountIssuePill = (issue: LocalAccessAccountIssueMeta) => {
    const IssueIcon =
      issue.icon === 'clock'
        ? Clock
        : issue.icon === 'info'
          ? Info
          : issue.icon === 'pause'
            ? Pause
            : CircleAlert;
    return (
      <span
        className={`codex-local-access-account-issue-pill ${issue.className}`}
        title={issue.detail}
      >
        <IssueIcon size={12} />
        <span>{issue.badge}</span>
      </span>
    );
  };

  const handleCopy = async (field: CopyableField, value: string) => {
    try {
      await navigator.clipboard.writeText(value);
      setCopiedField(field);
      window.setTimeout(
        () => setCopiedField((current) => (current === field ? null : current)),
        1200,
      );
    } catch (err) {
      setError(t('common.shared.export.copyFailed', '复制失败，请手动复制'));
      console.error('Failed to copy local access value:', err);
    }
  };

  const runAction = async (task: () => Promise<void>, successText: string) => {
    setError('');
    setNotice('');
    try {
      await task();
      setNotice(successText);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    }
  };

  const toggleSelectAllVisible = () => {
    if (actionBusy || visibleSelectableAccounts.length === 0) return;
    const visibleIds = visibleSelectableAccounts.map((account) => account.id);
    const visibleIdSet = new Set(visibleIds);
    setSelected((prev) => {
      const next = new Set(prev);
      if (allVisibleSelected) {
        visibleIds.forEach((accountId) => next.delete(accountId));
      } else {
        visibleIds.forEach((accountId) => next.add(accountId));
      }
      return next;
    });
    setSelectedOrder((prev) =>
      allVisibleSelected
        ? prev.filter((accountId) => !visibleIdSet.has(accountId))
        : normalizeAccountOrder([...prev, ...visibleIds], serviceAccountIds),
    );
  };

  const toggleMemberRemovalSelectAll = () => {
    if (actionBusy || selectedMemberAccounts.length === 0) return;
    setMemberRemovalSelected(() =>
      allMembersSelectedForRemoval
        ? new Set()
        : new Set(selectedMemberAccounts.map((account) => account.id)),
    );
  };

  const toggleMemberRemovalSelect = (accountId: string) => {
    if (actionBusy) return;
    setMemberRemovalSelected((prev) => {
      const next = new Set(prev);
      if (next.has(accountId)) {
        next.delete(accountId);
      } else {
        next.add(accountId);
      }
      return next;
    });
  };

  const handleToggleRestrictFreeAccounts = async () => {
    if (actionBusy) return;
    if (!restrictFreeAccounts) {
      const freeAccountIds = new Set(
        serviceAccounts
          .filter((account) => isCodexExplicitFreePlanType(account.plan_type))
          .map((account) => account.id),
      );
      if (freeAccountIds.size > 0) {
        setSelected((prev) => {
          const next = new Set(prev);
          freeAccountIds.forEach((accountId) => next.delete(accountId));
          return next.size === prev.size ? prev : next;
        });
        setSelectedOrder((prev) => prev.filter((accountId) => !freeAccountIds.has(accountId)));
        setMemberRemovalSelected((prev) => {
          if (prev.size === 0) return prev;
          const next = new Set(prev);
          freeAccountIds.forEach((accountId) => next.delete(accountId));
          return next.size === prev.size ? prev : next;
        });
      }
    }
    setRestrictFreeAccounts((prev) => !prev);
  };

  const toggleSelect = (accountId: string) => {
    if (actionBusy) return;
    const account = accountById.get(accountId);
    if (!account) return;
    setSelected((prev) => {
      if (prev.has(accountId) && persistedMemberIdSet.has(accountId)) {
        return prev;
      }
      const isFreeAccount = isCodexExplicitFreePlanType(account.plan_type);
      if (isFreeAccount && restrictFreeAccounts && !prev.has(accountId)) {
        return prev;
      }
      if (
        isLocalAccessSelectionBlockedByIssue(account, accountHealthById.get(account.id)) &&
        !prev.has(accountId)
      ) {
        return prev;
      }
      const next = new Set(prev);
      if (next.has(accountId)) {
        next.delete(accountId);
        setSelectedOrder((current) => current.filter((id) => id !== accountId));
      } else {
        next.add(accountId);
        setSelectedOrder((current) =>
          current.includes(accountId)
            ? current
            : normalizeAccountOrder([...current, accountId], serviceAccountIds),
        );
      }
      return next;
    });
  };

  const persistMembers = async (
    order: string[],
    successText: string,
    options?: { closeAfterSave?: boolean },
  ) => {
    const filtered = buildPersistedMemberIds(order);
    await onSaveAccounts({
      accountIds: filtered,
      restrictFreeAccounts,
    });
    setSelected(new Set(filtered));
    setSelectedOrder(filtered);
    setMemberRemovalSelected((prev) => {
      if (prev.size === 0) return prev;
      const filteredIdSet = new Set(filtered);
      const next = new Set<string>();
      for (const accountId of prev) {
        if (filteredIdSet.has(accountId)) {
          next.add(accountId);
        }
      }
      return next.size === prev.size ? prev : next;
    });
    if (options?.closeAfterSave) {
      onClose();
      return;
    }
    setNotice(successText);
  };

  const addCandidateMember = async (accountId: string) => {
    if (actionBusy || selected.has(accountId)) return;
    const account = accountById.get(accountId);
    if (!account) return;
    if (restrictFreeAccounts && isCodexExplicitFreePlanType(account.plan_type)) {
      return;
    }
    if (isLocalAccessSelectionBlockedByIssue(account, accountHealthById.get(account.id))) {
      return;
    }
    setError('');
    setNotice('');
    try {
      const nextOrder = normalizeAccountOrder(
        [...selectedOrderForSave, accountId],
        serviceAccountIds,
      );
      await persistMembers(
        nextOrder,
        t('codex.localAccess.modal.addMemberSuccess', '已加入 API 服务号池'),
      );
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    }
  };

  const buildPersistedMemberIds = (order: string[]) =>
    normalizeSelectedAccountOrder(
      order.filter((accountId) => {
        const account = accountById.get(accountId);
        if (!account) return false;
        if (restrictFreeAccounts && isCodexExplicitFreePlanType(account.plan_type)) {
          return false;
        }
        return true;
      }),
      serviceAccountIds,
    );

  const handleSaveMembers = async () => {
    setError('');
    setNotice('');
    try {
      await persistMembers(
        selectedOrderForSave,
        t('codex.localAccess.modal.saveSuccess', 'API 服务账号池已更新'),
        { closeAfterSave: true },
      );
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    }
  };

  const removeMemberIds = async (accountIds: Iterable<string>) => {
    if (actionBusy) return;
    const removalIds = new Set(accountIds);
    if (removalIds.size === 0) return;
    const removedAccountIds = selectedOrderForSave.filter((accountId) =>
      removalIds.has(accountId),
    );
    const removedCount = removedAccountIds.length;
    if (removedCount === 0) return;
    const removedAccount =
      removedCount === 1 ? accountById.get(removedAccountIds[0]) : null;
    const removedAccountLabel = removedAccount
      ? maskAccountText(buildCodexAccountPresentation(removedAccount, t).displayName)
      : t('codex.localAccess.modal.thisAccount', '该账号');
    const confirmed = await confirmDialog(
      removedCount === 1
        ? t('codex.localAccess.modal.removeMemberConfirmMessage', {
            account: removedAccountLabel,
            defaultValue:
              '确定要将 {{account}} 移出 API 服务吗？移出后它不会参与本机 API 服务调度，账号本身不会被删除。',
          })
        : t('codex.localAccess.modal.removeMembersConfirmMessage', {
            count: removedCount,
            defaultValue:
              '确定要将 {{count}} 个账号移出 API 服务吗？移出后这些账号不会参与本机 API 服务调度，账号本身不会被删除。',
          }),
      {
        title: t(
          'codex.localAccess.modal.removeMemberConfirmTitle',
          '确认移出 API 服务',
        ),
        kind: 'warning',
        okLabel: t('codex.localAccess.modal.removeMemberConfirmAction', '确认移出'),
        cancelLabel: t('common.cancel', '取消'),
      },
    );
    if (!confirmed) return;
    setError('');
    setNotice('');
    const nextSelected = new Set(selected);
    removalIds.forEach((accountId) => nextSelected.delete(accountId));
    const nextOrder = selectedOrderForSave.filter(
      (accountId) => !removalIds.has(accountId),
    );
    try {
      const filtered = buildPersistedMemberIds(nextOrder);
      await onSaveAccounts({
        accountIds: filtered,
        restrictFreeAccounts,
      });
      setSelected(nextSelected);
      setSelectedOrder(nextOrder);
      setMemberRemovalSelected((prev) => {
        if (prev.size === 0) return prev;
        const next = new Set<string>();
        for (const accountId of prev) {
          if (!removalIds.has(accountId)) {
            next.add(accountId);
          }
        }
        return next;
      });
      setNotice(
        t('codex.localAccess.modal.removeMembersSuccess', {
          count: removedCount,
          defaultValue: '已移出 {{count}} 个账号',
        }),
      );
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    }
  };

  const handleRemoveSelectedMembers = async () => {
    await removeMemberIds(memberRemovalSelected);
  };

  const handleRemoveMember = async (accountId: string) => {
    await removeMemberIds([accountId]);
  };

  const handleRefreshSelectedMembers = async () => {
    if (memberRemovalSelected.size === 0 || actionBusy) return;
    setError('');
    setNotice('');
    const accountIds = Array.from(memberRemovalSelected);
    try {
      const result = await onRefreshAccounts(accountIds);
      setNotice(
        t('codex.localAccess.modal.refreshMembersSuccess', {
          count: result.successCount,
          total: result.total,
          defaultValue: '已刷新 {{count}} 个账号额度',
        }),
      );
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    }
  };

  const handleSavePort = async () => {
    const nextPort = Number(portInput.trim());
    if (!Number.isInteger(nextPort) || nextPort <= 0 || nextPort > 65535) {
      setError(t('codex.localAccess.portInvalid', '请输入 1 到 65535 之间的端口'));
      return;
    }

    await runAction(
      async () => {
        await onUpdatePort(nextPort);
      },
      t('codex.localAccess.portSaveSuccess', 'API 服务端口已更新'),
    );
  };

  const handleChangeRoutingStrategy = async (nextStrategy: string) => {
    if (!collection) return;
    if (nextStrategy === routingStrategy) return;

    await runAction(
      async () => {
        await onUpdateRoutingStrategy(nextStrategy as CodexLocalAccessRoutingStrategy);
      },
      t('codex.localAccess.routingSaveSuccess', 'API 服务调度策略已更新'),
    );
  };

  const handleApplySafetyPreset = async (preset: CodexLocalApiSafetyPresetId) => {
    if (!collection) return;

    await runAction(
      async () => {
        await onApplySafetyPreset(preset);
      },
      t('codex.localAccess.safetyPresetSaveSuccess', 'API 服务策略预设已恢复'),
    );
  };

  const handleChangeRuntimeMode = async (nextMode: string) => {
    if (nextMode === selectedRuntimeMode) return;

    const forceDirectProjection = nextMode === 'direct_projection';
    if (forceDirectProjection) {
      const confirmed = await confirmDialog(
        t(
          'codex.localAccess.disableServiceConfirmMessage',
          '停用 API 服务会断开 Codex 当前使用的本地 provider，正在创建、恢复或流式执行的任务可能失败。确认停用吗？',
        ),
        {
          title: t(
            'codex.localAccess.disableServiceConfirmTitle',
            '确认停用 API 服务',
          ),
          kind: 'warning',
          okLabel: t('codex.localAccess.disableServiceAction', '确认停用'),
          cancelLabel: t('common.cancel', '取消'),
        },
      );
      if (!confirmed) return;
    }

    await runAction(
      async () => {
        await onSetRuntimeMode(nextMode as CodexRuntimeIntegrationMode, {
          force: forceDirectProjection,
        });
      },
      nextMode === 'cockpit_api_service'
        ? t('codex.localAccess.runtimeModeGatewaySuccess', '已切换为 Cockpit API Service 模式')
        : t('codex.localAccess.runtimeModeDirectSuccess', '已切换为 Direct API/OAuth 模式'),
    );
  };

  const handleResetKey = async () => {
    const confirmed = await confirmDialog(
      t(
        'codex.localAccess.rotateConfirmMessage',
        '重置后当前 API 服务密钥会立即失效，正在进行中的请求可能不可用。确认继续吗？',
      ),
      {
        title: t('codex.localAccess.rotateKey', '重置密钥'),
        kind: 'warning',
        okLabel: t('common.confirm'),
        cancelLabel: t('common.cancel'),
      },
    );

    if (!confirmed) {
      return;
    }

    await runAction(
      async () => {
        await onRotateApiKey();
        setKeyVisible(true);
      },
      t('codex.localAccess.rotateSuccess', 'API 服务密钥已重置'),
    );
  };

  const handleClearStats = async () => {
    const confirmed = await confirmDialog(
      t('codex.localAccess.clearStatsConfirm', '确定要清空 API 服务统计吗？'),
      {
        title: t('codex.localAccess.clearStats', '清除统计'),
        kind: 'warning',
        okLabel: t('common.confirm'),
        cancelLabel: t('common.cancel'),
      },
    );

    if (!confirmed) {
      return;
    }

    await runAction(async () => {
      await onClearStats();
    }, t('codex.localAccess.clearStatsSuccess', 'API 服务统计已清空'));
  };

  const handleKillPort = async () => {
    await runAction(
      async () => {
        await onKillPort();
      },
      t('codex.localAccess.killPortSuccessUnknown', 'API 服务端口已清理'),
    );
  };

  const handleRefreshStats = async () => {
    setError('');
    setNotice('');
    setStatsRefreshing(true);
    try {
      await onRefreshStats();
      setNotice(t('codex.localAccess.refreshStatsSuccess', 'API 服务统计已刷新'));
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setStatsRefreshing(false);
    }
  };

  const handleRecoverHealth = async (accountId: string, model?: string | null) => {
    await runAction(async () => {
      await onRecoverHealth(accountId, model || null);
    }, model
      ? t('codex.localAccess.recoverModelSuccess', '当前模型 cooldown 已恢复')
      : t('codex.localAccess.recoverAccountSuccess', '账号健康状态已恢复'));
  };

  const handlePauseHealth = async (accountId: string) => {
    const confirmed = await confirmDialog(
      t(
        'codex.localAccess.pauseAccountConfirmMessage',
        '暂停后该账号不会进入 API 服务调度；该动作只修改本地健康状态，不刷新额度也不访问上游。确认继续吗？',
      ),
      {
        title: t('codex.localAccess.pauseAccount', '暂停账号调度'),
        kind: 'warning',
        okLabel: t('common.confirm'),
        cancelLabel: t('common.cancel'),
      },
    );

    if (!confirmed) {
      return;
    }

    await runAction(
      async () => {
        await onPauseHealth(accountId);
      },
      t('codex.localAccess.pauseAccountSuccess', '账号已从 API 服务调度中暂停'),
    );
  };

  const handleToggleEnabled = async () => {
    await runAction(
      async () => {
        await onToggleEnabled();
      },
      collection?.enabled
        ? t('codex.localAccess.disabledSuccess', 'API 服务已停用')
        : t('codex.localAccess.enabledSuccess', 'API 服务已启用'),
    );
  };

  const handleTest = async () => {
    setError('');
    setNotice('');
    try {
      const modelCount = await onTest();
      setNotice(
        t('codex.localAccess.testSuccess', {
          count: modelCount,
          defaultValue:
            modelCount > 0 ? 'API 服务测试成功（{{count}} 个模型）' : 'API 服务测试成功',
        }),
      );
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    }
  };

  const handleRequestClose = () => {
    if (actionBusy) return;
    onClose();
  };

  if (!isOpen) return null;
  const isMembersMode = mode === 'members';

  return (
    <div
      className={`modal-overlay codex-local-access-modal-overlay${
        isMembersMode ? '' : ' codex-local-access-modal-overlay-panel'
      }`}
      onClick={handleRequestClose}
    >
      <div
        className={`modal codex-local-access-modal${
          isMembersMode
            ? ' codex-local-access-modal-members group-account-picker-modal'
            : ' codex-local-access-modal-panel'
        }`}
        onClick={(event) => event.stopPropagation()}
      >
        <div className="modal-header codex-local-access-modal-header">
          <div className="codex-local-access-header-main">
            <h2 className="group-account-picker-title">
              <Server size={18} />
              <span>
                {isMembersMode
                  ? t('codex.localAccess.entryAction', '添加至 API 服务')
                  : t('codex.localAccess.title', 'API 服务')}
              </span>
            </h2>
            {!isMembersMode && (
              <div className="codex-local-access-header-meta">
                <div className="codex-local-access-header-badges">
                  <span
                    className={`codex-local-access-status ${
                      state?.running ? 'running' : 'stopped'
                    }`}
                  >
                    {collection?.enabled
                      ? state?.running
                        ? t('codex.localAccess.statusRunning', '运行中')
                        : t('codex.localAccess.statusStopped', '未运行')
                      : t('codex.localAccess.statusDisabled', '已停用')}
                  </span>
                  <span className="codex-local-access-subtle-badge">
                    {t('codex.localAccess.memberOnlyLocal', '仅本机')}
                  </span>
                </div>
                <div className="codex-local-access-header-tools">
                  <button
                    type="button"
                    className="folder-icon-btn codex-local-access-toolbar-btn"
                    onClick={() => void handleRefreshStats()}
                    disabled={!collection || actionBusy}
                    title={t('codex.localAccess.refreshStats', '刷新统计')}
                    aria-label={t('codex.localAccess.refreshStats', '刷新统计')}
                  >
                    <RefreshCw size={14} className={statsRefreshing ? 'loading-spinner' : ''} />
                  </button>
                  {collection && (
                    <div className="codex-local-access-header-routing">
                      <SingleSelectDropdown
                        value={routingStrategy}
                        options={routingStrategyOptions}
                        onChange={(value) => void handleChangeRoutingStrategy(value)}
                        disabled={actionBusy}
                        ariaLabel={t('codex.localAccess.routingLabel', '调度策略')}
                      />
                    </div>
                  )}
                  <button
                    type="button"
                    className="folder-icon-btn codex-local-access-toolbar-btn"
                    onClick={() => void handleTest()}
                    disabled={!collection || actionBusy}
                    title={t('codex.localAccess.testAction', '测试 API 服务')}
                    aria-label={t('codex.localAccess.testAction', '测试 API 服务')}
                  >
                    <ShieldCheck size={14} className={testing ? 'loading-spinner' : ''} />
                  </button>
                  <button
                    type="button"
                    className={`folder-icon-btn codex-local-access-toolbar-btn ${
                      collection?.enabled ? 'is-danger' : 'is-primary'
                    }`}
                    onClick={() => void handleToggleEnabled()}
                    disabled={!collection || actionBusy}
                    title={
                      collection?.enabled
                        ? t('codex.localAccess.disableService', '停用服务')
                        : t('codex.localAccess.enableService', '启用服务')
                    }
                    aria-label={
                      collection?.enabled
                        ? t('codex.localAccess.disableService', '停用服务')
                        : t('codex.localAccess.enableService', '启用服务')
                    }
                  >
                    <Power size={14} />
                  </button>
                </div>
              </div>
            )}
          </div>
          <button
            className="modal-close codex-local-access-close"
            onClick={handleRequestClose}
            disabled={actionBusy}
            aria-label={t('common.close')}
          >
            <X size={18} />
          </button>
        </div>

        <div className="modal-body codex-local-access-modal-body">
          {state?.lastError && (
            <div className="codex-local-access-inline-error codex-local-access-inline-error-with-action">
              <CircleAlert size={14} />
              <span>{state.lastError}</span>
              {collection && (
                <button
                  type="button"
                  className="btn btn-secondary btn-sm codex-local-access-inline-action"
                  onClick={() => void handleKillPort()}
                  disabled={actionBusy}
                >
                  {portCleanupBusy ? (
                    <RefreshCw size={14} className="loading-spinner" />
                  ) : (
                    <Wrench size={14} />
                  )}
                  {t('codex.localAccess.killPortAction', '清理端口')}
                </button>
              )}
            </div>
          )}

          {error && (
            <div className="codex-local-access-inline-error">
              <CircleAlert size={14} />
              <span>{error}</span>
            </div>
          )}

          {notice && (
            <div className="codex-local-access-inline-success">
              <Check size={14} />
              <span>{notice}</span>
            </div>
          )}

          {!isMembersMode && (
            <section className="codex-local-access-section codex-local-access-section-surface codex-local-access-summary-block">
              <div className="codex-local-access-summary-head">
                <div className="codex-local-access-section-title">
                  <Activity size={16} />
                  <span>{t('codex.localAccess.statsTitle', '总量统计')}</span>
                </div>
                <div className="codex-local-access-summary-actions">
                  <div
                    className="codex-local-access-stats-range-tabs"
                    role="tablist"
                    aria-label={t('codex.localAccess.statsRange.label', '统计范围')}
                  >
                    {statsRangeOptions.map((option) => (
                      <button
                        key={option.key}
                        type="button"
                        role="tab"
                        className={`codex-local-access-stats-range-tab${
                          statsRange === option.key ? ' is-active' : ''
                        }`}
                        aria-selected={statsRange === option.key}
                        onClick={() => setStatsRange(option.key)}
                        disabled={actionBusy}
                      >
                        {option.label}
                      </button>
                    ))}
                  </div>
                  <button
                    type="button"
                    className="btn btn-danger btn-sm"
                    onClick={() => void handleClearStats()}
                    disabled={!collection || actionBusy}
                    title={t('codex.localAccess.clearStats', '清除统计')}
                    aria-label={t('codex.localAccess.clearStats', '清除统计')}
                  >
                    <Trash2 size={14} />
                    {t('codex.localAccess.clearStats', '清除统计')}
                  </button>
                </div>
              </div>
              <div className="codex-local-access-stats-grid">
                {summaryStats.map((item) => (
                  <div
                    key={item.key}
                    className={`codex-local-access-stat-card codex-local-access-stat-card-${item.key}`}
                  >
                    <span className="codex-local-access-stat-label">{item.label}</span>
                    <strong>{item.value}</strong>
                    <span className="codex-local-access-stat-sub">{item.detail}</span>
                  </div>
                ))}
              </div>
              {concurrencyDiagnostics && concurrencyDiagnosis && (
                <div
                  className={`codex-local-access-diagnostics-panel is-${concurrencyDiagnosis.tone}`}
                >
                  <div className="codex-local-access-diagnostics-head">
                    <span className="codex-local-access-diagnostics-title">
                      <Gauge size={15} />
                      {t('codex.localAccess.diagnostics.title', '并发诊断')}
                    </span>
                    <span className="codex-local-access-diagnostics-updated">
                      {formatTimestampMs(concurrencyDiagnostics.updatedAt)}
                    </span>
                  </div>
                  <div className="codex-local-access-diagnostics-metrics">
                    {concurrencyMetricItems.map((item) => (
                      <span
                        key={item.key}
                        className={`codex-local-access-diagnostics-metric codex-local-access-diagnostics-metric-${item.key}`}
                      >
                        <span>{item.label}</span>
                        <strong>{item.value}</strong>
                      </span>
                    ))}
                  </div>
                  <div className="codex-local-access-diagnostics-hint">
                    <Info size={14} />
                    <span>{concurrencyDiagnosis.text}</span>
                  </div>
                  <div className="codex-local-access-diagnostics-meta">
                    <span>
                      {t('codex.localAccess.diagnostics.capacity', '剩余容量')}:{' '}
                      <code>{formatCompactNumber(concurrencyDiagnostics.requestCapacity)}</code>
                    </span>
                    <span>
                      {t('codex.localAccess.diagnostics.lastProblem', '最近问题')}:{' '}
                      <code>{concurrencyDiagnostics.lastProblemKind ?? '--'}</code>
                    </span>
                    <span>
                      {t('codex.localAccess.diagnostics.lastProblemAt', '问题时间')}:{' '}
                      {formatTimestampMs(concurrencyDiagnostics.lastProblemAtMs)}
                    </span>
                  </div>
                </div>
              )}
              {health && (
                <div className="codex-local-access-health-panel">
                  <div className="codex-local-access-health-head">
                    <span className="codex-local-access-health-title">
                      <ShieldCheck size={15} />
                      {t('codex.localAccess.health.title', '健康状态')}
                    </span>
                    {health.unavailable && (
                      <span
                        className="codex-local-access-health-badge is-warning"
                        title={health.loadError ?? undefined}
                      >
                        {t('codex.localAccess.health.unavailable', '不可用')}
                      </span>
                    )}
                    {health.auditDegraded && (
                      <span
                        className="codex-local-access-health-badge is-warning"
                        title={health.auditError ?? undefined}
                      >
                        {t('codex.localAccess.health.auditDegraded', '审计降级')}
                      </span>
                    )}
                    {(health.estimatedAvailableCount ?? 0) > 0 && (
                      <span
                        className="codex-local-access-health-badge is-warning"
                        title={t(
                          'codex.localAccess.health.estimatedAvailableTitle',
                          '调度层已按 reset 时间估算恢复，真实配额需等待刷新确认',
                        )}
                      >
                        {t('codex.localAccess.health.estimatedAvailable', {
                          count: health.estimatedAvailableCount,
                          defaultValue: '估算恢复 {{count}}',
                        })}
                      </span>
                    )}
                  </div>
                  <div className="codex-local-access-health-metrics">
                    {healthMetricItems.map((item) => (
                      <span
                        key={item.key}
                        className={`codex-local-access-health-metric codex-local-access-health-metric-${item.key}`}
                      >
                        <span>{item.label}</span>
                        <strong>{formatCompactNumber(item.value)}</strong>
                      </span>
                    ))}
                  </div>
                  <div className="codex-local-access-health-meta">
                    <span>
                      {t('codex.localAccess.health.sticky', 'Sticky')}:{' '}
                      <code>{health.stickyAccountHash ?? '--'}</code>
                    </span>
                    <span>
                      {t('codex.localAccess.health.lastError', '最近错误')}:{' '}
                      <code>{health.lastErrorType ?? '--'}</code>
                    </span>
                    <span>
                      {t('codex.localAccess.health.cooldownUntil', '冷却至')}:{' '}
                      {formatTimestampMs(health.nearestCooldownUntilMs)}
                    </span>
                    {(health.estimatedAvailableCount ?? 0) > 0 && (
                      <span>
                        {t(
                          'codex.localAccess.health.estimatedAvailableHint',
                          '估算恢复，等待真实配额刷新',
                        )}
                      </span>
                    )}
                    {health.auditDegraded && (
                      <span>
                        {t('codex.localAccess.health.audit', 'Audit')}:{' '}
                        <code>{health.auditError ?? '--'}</code>
                      </span>
                    )}
                  </div>
                </div>
              )}
              {currentQuotaPoolSummary.visiblePlans.length > 0 && (
                <div
                  className="codex-local-access-quota-pool-grid"
                  aria-label={quotaPoolLabels.title}
                >
                  {currentQuotaPoolSummary.visiblePlans.map((item) => (
                    <div key={item.key} className="codex-local-access-quota-pool-card">
                      <span className="codex-local-access-quota-pool-plan">
                        {item.key} ({item.count})
                      </span>
                      <span className="codex-local-access-quota-pool-value">
                        {quotaPoolLabels.hourly} {formatCodexQuotaPoolPercent(item.hourly)}
                      </span>
                      <span className="codex-local-access-quota-pool-value">
                        {quotaPoolLabels.weekly} {formatCodexQuotaPoolPercent(item.weekly)}
                      </span>
                    </div>
                  ))}
                </div>
              )}
            </section>
          )}

          {!isMembersMode && (
            <div className="codex-local-access-panel-grid">
              <section className="codex-local-access-section codex-local-access-section-surface codex-local-access-config-section">
                <div className="codex-local-access-section-title">
                  <KeyRound size={16} />
                  <span>{t('codex.localAccess.configTitle', '服务配置')}</span>
                </div>
                {collection ? (
                  <div className="codex-local-access-config-grid">
                    <div className="codex-local-access-config-card codex-local-access-config-card-base">
                      <div className="codex-local-access-config-head">
                        <div className="codex-local-access-config-label codex-local-access-address-select">
                          <SingleSelectDropdown
                            value={addressKind}
                            options={addressOptions}
                            onChange={onAddressKindChange}
                            menuClassName="codex-local-access-address-menu"
                            menuWidth={92}
                            menuMaxHeight={120}
                            disabled={addressOptions.length < 2}
                            ariaLabel={t('codex.localAccess.addressKind', '地址类型')}
                          />
                        </div>
                        <div className="codex-local-access-config-actions">
                          <button
                            type="button"
                            className="folder-icon-btn"
                            onClick={() => void handleCopy('baseUrl', displayBaseUrl)}
                            title={t('common.copy', '复制')}
                          >
                            {copiedField === 'baseUrl' ? <Check size={14} /> : <Copy size={14} />}
                          </button>
                        </div>
                      </div>
                      <code className="codex-local-access-code" title={displayBaseUrl}>
                        {displayBaseUrl}
                      </code>
                    </div>

                    <div className="codex-local-access-config-card codex-local-access-config-card-key">
                      <div className="codex-local-access-config-head">
                        <span className="codex-local-access-config-label">
                          {t('codex.localAccess.apiKey', '密钥')}
                        </span>
                        <div className="codex-local-access-config-actions">
                          <button
                            type="button"
                            className="folder-icon-btn"
                            onClick={() => setKeyVisible((prev) => !prev)}
                            title={
                              keyVisible
                                ? t('codex.localAccess.hideKey', '隐藏密钥')
                                : t('codex.localAccess.showKey', '显示密钥')
                            }
                          >
                            {keyVisible ? <EyeOff size={14} /> : <Eye size={14} />}
                          </button>
                          <button
                            type="button"
                            className="folder-icon-btn"
                            onClick={() => void handleCopy('apiKey', collection.apiKey)}
                            title={t('common.copy', '复制')}
                          >
                            {copiedField === 'apiKey' ? <Check size={14} /> : <Copy size={14} />}
                          </button>
                          <button
                            type="button"
                            className="btn btn-secondary btn-sm"
                            onClick={() => void handleResetKey()}
                            disabled={actionBusy}
                          >
                            {saving ? (
                              <RefreshCw size={14} className="loading-spinner" />
                            ) : (
                              <RefreshCw size={14} />
                            )}
                            {t('codex.localAccess.rotateKey', '重置密钥')}
                          </button>
                        </div>
                      </div>
                      <code className="codex-local-access-code" title={apiKeyTitle}>
                        {keyVisible
                          ? collection.apiKey
                          : `${collection.apiKey.slice(0, 10)}••••••••••••`}
                      </code>
                    </div>

                    <div className="codex-local-access-config-card codex-local-access-config-card-port codex-local-access-port-card">
                      <div className="codex-local-access-config-head">
                        <label
                          className="codex-local-access-config-label"
                          htmlFor="codex-local-access-port"
                        >
                          {t('codex.localAccess.portLabel', '服务端口')}
                        </label>
                        <div className="codex-local-access-config-actions">
                          <button
                            type="button"
                            className="btn btn-secondary btn-sm"
                            onClick={() => void handleSavePort()}
                            disabled={actionBusy}
                          >
                            {saving ? (
                              <RefreshCw size={14} className="loading-spinner" />
                            ) : (
                              <Gauge size={14} />
                            )}
                            {t('codex.localAccess.portSave', '保存端口')}
                          </button>
                        </div>
                      </div>
                      <div className="codex-local-access-port-row">
                        <input
                          id="codex-local-access-port"
                          type="number"
                          min={1}
                          max={65535}
                          value={portInput}
                          onChange={(event) => setPortInput(event.target.value)}
                          disabled={actionBusy}
                        />
                      </div>
                    </div>

                    <div className="codex-local-access-config-card codex-local-access-config-card-runtime">
                      <div className="codex-local-access-config-head">
                        <span className="codex-local-access-config-label">
                          {t('codex.localAccess.runtimeMode.title', 'Codex 模式')}
                        </span>
                      </div>
                      <SingleSelectDropdown
                        value={selectedRuntimeMode}
                        options={runtimeModeOptions}
                        onChange={(value) => void handleChangeRuntimeMode(value)}
                        menuClassName="codex-local-access-runtime-mode-menu"
                        menuWidth={190}
                        menuMaxHeight={120}
                        disabled={actionBusy}
                        ariaLabel={t('codex.localAccess.runtimeMode.title', 'Codex 模式')}
                      />
                      <div className="codex-local-access-runtime-mode-meta">
                        {runtimeMode?.accountKind
                          ? t('codex.localAccess.runtimeMode.accountKind', {
                              kind: runtimeMode.accountKind,
                              defaultValue: '账号类型：{{kind}}',
                            })
                          : t('codex.localAccess.runtimeMode.accountKindUnknown', '账号类型：unknown')}
                      </div>
                    </div>

                    <div className="codex-local-access-config-card codex-local-access-config-card-preset">
                      <div className="codex-local-access-config-head">
                        <span className="codex-local-access-config-label">
                          {t('codex.localAccess.safetyPreset.label', '策略预设')}
                        </span>
                        <span className="codex-local-access-view-only-badge">
                          {safetyPresetId === 'custom'
                            ? t('codex.localAccess.safetyPreset.custom', '自定义')
                            : t('codex.localAccess.safetyPreset.current', '当前')}
                        </span>
                        {maxRetryAccountsManualOptIn ? (
                          <span className="codex-local-access-manual-opt-in-badge">
                            <CircleAlert size={12} />
                            maxRetryAccounts &gt; 2 · 手动 opt-in
                          </span>
                        ) : null}
                      </div>
                      <div className="codex-local-access-preset-grid">
                        {safetyPresetOptions.map((option) => (
                          <button
                            key={option.id}
                            type="button"
                            className={`codex-local-access-preset-btn${
                              safetyPresetId === option.id ? ' is-active' : ''
                            }`}
                            onClick={() => void handleApplySafetyPreset(option.id)}
                            disabled={!collection || actionBusy}
                          >
                            <ShieldCheck size={14} />
                            <span>
                              <strong>{option.label}</strong>
                              <small>{option.desc}</small>
                            </span>
                          </button>
                        ))}
                      </div>
                    </div>
                  </div>
                ) : (
                  <div className="group-account-empty">
                    {t(
                      'codex.localAccess.configEmpty',
                      '先把账号保存到 API 服务集合，随后会自动生成地址、密钥和端口。',
                    )}
                  </div>
                )}
                {collection || modelIdOptions.length > 0 ? (
                  <div className="codex-local-access-config-extra-grid">
                    {collection ? (
                      <div className="codex-local-access-config-card codex-local-access-config-card-root">
                        <div className="codex-local-access-config-head">
                          <span className="codex-local-access-config-label">
                            {t('codex.localAccess.apiPortUrl', 'API端口URL')}
                          </span>
                          <div className="codex-local-access-config-actions">
                            <button
                              type="button"
                              className="folder-icon-btn"
                              onClick={() => void handleCopy('apiPortUrl', apiPortUrl)}
                              title={t('common.copy', '复制')}
                            >
                              {copiedField === 'apiPortUrl' ? <Check size={14} /> : <Copy size={14} />}
                            </button>
                          </div>
                        </div>
                        <code className="codex-local-access-code" title={apiPortUrl}>
                          {apiPortUrl}
                        </code>
                      </div>
                    ) : null}

                    {modelIdOptions.length > 0 ? (
                      <div className="codex-local-access-config-card codex-local-access-config-card-model">
                        <div className="codex-local-access-config-head">
                          <span className="codex-local-access-config-label">
                            {t('codex.localAccess.modelId', '模型 ID')}
                          </span>
                          <span className="codex-local-access-view-only-badge">
                            {t('codex.localAccess.modelIdViewOnly', '仅查看使用，无切换功能')}
                          </span>
                          <div className="codex-local-access-config-actions">
                            <button
                              type="button"
                              className="folder-icon-btn"
                              onClick={() => void handleCopy('modelId', selectedModelId)}
                              title={t('common.copy', '复制')}
                              disabled={!selectedModelId}
                            >
                              {copiedField === 'modelId' ? <Check size={14} /> : <Copy size={14} />}
                            </button>
                          </div>
                        </div>
                        <div className="codex-local-access-model-row">
                          <SingleSelectDropdown
                            value={selectedModelId}
                            options={modelIdOptions}
                            onChange={setSelectedModelId}
                            disabled={modelIdOptions.length === 0}
                            ariaLabel={t('codex.localAccess.modelId', '模型 ID')}
                            placeholder={t('codex.localAccess.modelIdPlaceholder', '选择模型 ID')}
                            menuPlacement="up"
                            menuMaxHeight={240}
                          />
                        </div>
                      </div>
                    ) : null}
                  </div>
                ) : null}
              </section>

              <section className="codex-local-access-section codex-local-access-section-surface codex-local-access-account-stats-section">
                <div className="codex-local-access-section-title">
                  <Server size={16} />
                  <span>{t('codex.localAccess.accountStatsTitle', '按账号统计')}</span>
                </div>
                <div className="codex-local-access-account-stats">
                  {currentMemberStats.length === 0 ? (
                    <div className="group-account-empty">
                      {t('codex.localAccess.statsEmpty', '当前还没有统计数据')}
                    </div>
                  ) : (
                    currentMemberStats.map(({ account, presentation, stats: accountStats }) => {
                      const accountIssueMeta = resolveLocalAccessAccountIssueMeta(account);
                      return (
                        <div
                          key={account.id}
                          className={`codex-local-access-account-stat-row${
                            accountIssueMeta?.blocksSelection ? ' is-account-issue' : ''
                          }`}
                        >
                          <div className="codex-local-access-account-stat-top">
                            <div className="codex-local-access-account-stat-main">
                              <span
                                className="group-account-email"
                                title={maskAccountText(presentation.displayName)}
                              >
                                {maskAccountText(presentation.displayName)}
                              </span>
                              <span className={`tier-badge ${presentation.planClass}`}>
                                {presentation.planLabel}
                              </span>
                              {accountIssueMeta && renderAccountIssuePill(accountIssueMeta)}
                            </div>
                            <div className="codex-local-access-account-stat-block codex-local-access-account-stat-block-quota">
                              {renderQuotaPreview(presentation, 3)}
                            </div>
                            <div className="codex-local-access-account-stat-block codex-local-access-account-stat-block-metrics">
                              <div className="codex-local-access-account-stat-metrics">
                                <span className="codex-local-access-account-stat-pill">
                                  {t('codex.localAccess.stats.accountResult', {
                                    success: accountStats?.successCount ?? 0,
                                    failed: accountStats?.failureCount ?? 0,
                                    defaultValue: '成功 {{success}} / 失败 {{failed}}',
                                  })}
                                </span>
                                <span className="codex-local-access-account-stat-pill">
                                  {(accountStats?.totalTokens ?? 0) === 0
                                    ? t('codex.localAccess.stats.accountTokens', {
                                        count: 0,
                                        defaultValue: '0 Tokens',
                                      })
                                    : t('codex.localAccess.stats.accountTokensCompact', {
                                        value: formatCompactNumber(accountStats?.totalTokens ?? 0),
                                        defaultValue: '{{value}}',
                                      })}
                                </span>
                              </div>
                            </div>
                            <div className="codex-local-access-account-stat-actions">
                              <button
                                type="button"
                                className="folder-icon-btn"
                                onClick={() => void handlePauseHealth(account.id)}
                                title={t('codex.localAccess.pauseAccount', '暂停账号调度')}
                                aria-label={t('codex.localAccess.pauseAccount', '暂停账号调度')}
                                disabled={actionBusy}
                              >
                                <Pause size={14} />
                              </button>
                              <button
                                type="button"
                                className="folder-icon-btn"
                                onClick={() => void handleRecoverHealth(account.id, null)}
                                title={t('codex.localAccess.recoverAccount', '恢复账号健康状态')}
                                aria-label={t('codex.localAccess.recoverAccount', '恢复账号健康状态')}
                                disabled={actionBusy}
                              >
                                <Wrench size={14} />
                              </button>
                              {selectedModelId ? (
                                <button
                                  type="button"
                                  className="folder-icon-btn"
                                  onClick={() => void handleRecoverHealth(account.id, selectedModelId)}
                                  title={t('codex.localAccess.recoverModel', {
                                    model: selectedModelId,
                                    defaultValue: '恢复当前模型 cooldown：{{model}}',
                                  })}
                                  aria-label={t('codex.localAccess.recoverModel', {
                                    model: selectedModelId,
                                    defaultValue: '恢复当前模型 cooldown：{{model}}',
                                  })}
                                  disabled={actionBusy}
                                >
                                  <RefreshCw size={14} />
                                </button>
                              ) : null}
                            </div>
                          </div>
                        </div>
                      );
                    })
                  )}
                </div>
              </section>
            </div>
          )}

          {isMembersMode && (
            <section className="codex-local-access-section codex-local-access-section-surface codex-local-access-member-section">
              <div className="codex-local-access-section-head">
                <div className="codex-local-access-section-title">
                  <FolderPlus size={16} />
                  <span>{t('codex.localAccess.memberTitle', '集合成员')}</span>
                </div>
                <label className="codex-local-access-free-toggle">
                  <input
                    type="checkbox"
                    checked={restrictFreeAccounts}
                    onChange={() => void handleToggleRestrictFreeAccounts()}
                    disabled={actionBusy}
                  />
                  <span>
                    {t(
                      'codex.localAccess.modal.restrictFreeToggle',
                      '限制 Free 账号使用',
                    )}
                  </span>
                </label>
              </div>

              {selectedMemberAccounts.length > 0 && (
                <div className="codex-local-access-current-members">
                  <div className="codex-local-access-current-members-head">
                    <div className="codex-local-access-current-members-title">
                      <span>
                        {t('codex.localAccess.modal.currentPoolMembers', {
                          count: selectedMemberAccounts.length,
                          defaultValue: '当前号池 {{count}} 个',
                        })}
                      </span>
                    </div>
                    <div className="codex-local-access-current-member-actions">
                      <button
                        type="button"
                        className="btn btn-secondary codex-local-access-member-action-btn codex-local-access-member-select-btn"
                        onClick={toggleMemberRemovalSelectAll}
                        disabled={actionBusy}
                      >
                        <Check size={14} />
                        <span>
                          {allMembersSelectedForRemoval
                            ? t('common.cancelSelectAll', '取消全选')
                            : t('common.selectAll', '全选')}
                          {memberRemovalSelectedCount > 0 ? ` (${memberRemovalSelectedCount})` : ''}
                        </span>
                      </button>
                      <button
                        type="button"
                        className="btn btn-secondary codex-local-access-member-action-btn"
                        onClick={() => void handleRefreshSelectedMembers()}
                        disabled={actionBusy || memberRemovalSelectedCount === 0}
                      >
                        <RefreshCw
                          size={14}
                          className={refreshing ? 'loading-spinner' : ''}
                        />
                        <span>
                          {t('common.shared.refreshQuota', '刷新配额')}
                          {memberRemovalSelectedCount > 0 ? ` (${memberRemovalSelectedCount})` : ''}
                        </span>
                      </button>
                      <button
                        type="button"
                        className="btn btn-secondary codex-local-access-member-action-btn is-danger"
                        onClick={() => void handleRemoveSelectedMembers()}
                        disabled={actionBusy || memberRemovalSelectedCount === 0}
                      >
                        <Trash2 size={14} />
                        <span>
                          {t('codex.localAccess.removeSelectedMembers', '移出 API 服务')}
                          {memberRemovalSelectedCount > 0 ? ` (${memberRemovalSelectedCount})` : ''}
                        </span>
                      </button>
                    </div>
                  </div>
                  <div className="codex-local-access-current-member-list">
                    {selectedMemberAccounts.map((account) => {
                      const presentation = buildCodexAccountPresentation(account, t);
                      const isCurrentAccount = currentAccountId === account.id;
                      const isRemovalChecked = memberRemovalSelected.has(account.id);
                      const accountStats = allStatsByAccountId.get(account.id)?.usage;
                      const accountIssueMeta = resolveLocalAccessAccountIssueMeta(account);
                      const canPauseAccountIssue =
                        accountIssueMeta?.canPause && accountIssueMeta.className !== 'health-disabled';

                      return (
                        <div
                          key={`pool-member-${account.id}`}
                          className={`codex-local-access-current-member${
                            isCurrentAccount ? ' is-active-account' : ''
                          }${isRemovalChecked ? ' is-marked' : ''}${
                            accountIssueMeta?.blocksSelection ? ' is-account-issue' : ''
                          }`}
                        >
                          <input
                            type="checkbox"
                            checked={isRemovalChecked}
                            onChange={() => toggleMemberRemovalSelect(account.id)}
                            disabled={actionBusy}
                          />
                          <span
                            className="codex-local-access-current-member-email"
                            title={maskAccountText(presentation.displayName)}
                          >
                            {maskAccountText(presentation.displayName)}
                          </span>
                          {isCurrentAccount && (
                            <span className="group-account-badge is-current">
                              {t('codex.current', '当前')}
                            </span>
                          )}
                          {accountIssueMeta && renderAccountIssuePill(accountIssueMeta)}
                          <span className={`tier-badge ${presentation.planClass}`}>
                            {presentation.planLabel}
                          </span>
                          <span className="codex-local-access-member-metric">
                            {t('codex.localAccess.stats.accountRequests', {
                              count: accountStats?.requestCount ?? 0,
                              defaultValue: '{{count}} 次请求',
                            })}
                          </span>
                          {renderQuotaPreview(presentation, 1)}
                          <span className="codex-local-access-member-inline-actions">
                            {canPauseAccountIssue && (
                              <button
                                type="button"
                                className="folder-icon-btn codex-local-access-member-icon-btn codex-local-access-pause-member-btn"
                                onClick={() => void handlePauseHealth(account.id)}
                                disabled={actionBusy}
                                title={t('codex.localAccess.pauseAccount', '暂停账号调度')}
                                aria-label={`${t(
                                  'codex.localAccess.pauseAccount',
                                  '暂停账号调度',
                                )}: ${maskAccountText(presentation.displayName)}`}
                              >
                                <Pause size={14} />
                              </button>
                            )}
                            <button
                              type="button"
                              className="folder-icon-btn codex-local-access-member-icon-btn codex-local-access-remove-member-btn"
                              onClick={() => void handleRemoveMember(account.id)}
                              disabled={actionBusy}
                              title={t('codex.localAccess.removeMember', '移出 API 服务')}
                              aria-label={t('codex.localAccess.removeMember', '移出 API 服务')}
                            >
                              <Trash2 size={14} />
                            </button>
                          </span>
                        </div>
                      );
                    })}
                  </div>
                </div>
              )}

              <div className="group-account-toolbar">
                <div className="group-account-search">
                  <Search size={16} className="group-account-search-icon" />
                  <input
                    ref={searchInputRef}
                    type="text"
                    value={query}
                    onChange={(event) => setQuery(event.target.value)}
                    placeholder={t('accounts.search')}
                  />
                </div>
                <div className="group-account-picker-filters">
                  <MultiSelectFilterDropdown
                    options={tierFilterOptions}
                    selectedValues={filterTypes}
                    allLabel={allTierFilterLabel}
                    filterLabel={t('common.shared.filterLabel', '筛选')}
                    clearLabel={t('accounts.clearFilter', '清空筛选')}
                    emptyLabel={t('common.none', '暂无')}
                    ariaLabel={t('common.shared.filterLabel', '筛选')}
                    onToggleValue={(value) =>
                      setFilterTypes((prev) =>
                        prev.includes(value)
                          ? prev.filter((item) => item !== value)
                          : [...prev, value],
                      )
                    }
                    onClear={() => setFilterTypes([])}
                  />
                  <AccountTagFilterDropdown
                    availableTags={availableTags}
                    selectedTags={tagFilter}
                    onToggleTag={(value) =>
                      setTagFilter((prev) =>
                        prev.includes(value)
                          ? prev.filter((item) => item !== value)
                          : [...prev, value],
                      )
                    }
                    onClear={() => setTagFilter([])}
                  />
                  <MultiSelectFilterDropdown
                    options={groupFilterOptions}
                    selectedValues={groupFilter}
                    allLabel={t('accounts.groups.allGroups', '全部分组')}
                    filterLabel={t('accounts.groups.manageTitle', '分组管理')}
                    clearLabel={t('accounts.clearFilter', '清空筛选')}
                    emptyLabel={t('common.none', '暂无')}
                    ariaLabel={t('accounts.groups.manageTitle', '分组管理')}
                    onToggleValue={(value) =>
                      setGroupFilter((prev) =>
                        prev.includes(value)
                          ? prev.filter((item) => item !== value)
                          : [...prev, value],
                      )
                    }
                    onClear={() => setGroupFilter([])}
                  />
                </div>
              </div>

              <label className="group-account-item group-account-item-header codex-local-access-select-visible-row">
                <input
                  ref={selectAllCheckboxRef}
                  type="checkbox"
                  checked={allVisibleSelected}
                  onChange={toggleSelectAllVisible}
                  disabled={actionBusy || visibleSelectableAccounts.length === 0}
                />
                <div className="group-account-main">
                  <span className="codex-local-access-select-visible-title">
                    {t(
                      'codex.localAccess.modal.selectVisibleAccounts',
                      '全选当前筛选',
                    )}
                  </span>
                  <span className="codex-local-access-select-visible-count">
                    {t('codex.localAccess.modal.selectVisibleAccountsCount', {
                      selected: selectedVisibleCount,
                      count: visibleSelectableAccounts.length,
                      defaultValue: '{{selected}} / {{count}} 个已选',
                    })}
                  </span>
                </div>
              </label>

              <div className="group-account-list codex-local-access-member-list">
                {serviceAccounts.length === 0 ? (
                  <div className="group-account-empty">
                    {t('codex.localAccess.modal.empty', '暂无可加入 API 服务的账号/API key')}
                  </div>
                ) : visibleAccounts.length === 0 ? (
                  <div className="group-account-empty">
                    {t('common.shared.noMatch.title', '没有匹配的账号')}
                  </div>
                ) : (
                  visibleAccounts.map((account) => {
                    const presentation = buildCodexAccountPresentation(account, t);
                    const isChecked = selected.has(account.id);
                    const isPersistedMember = persistedMemberIdSet.has(account.id);
                    const isCurrentAccount = currentAccountId === account.id;
                    const isFreeAccount = isCodexExplicitFreePlanType(account.plan_type);
                    const accountIssueMeta = resolveLocalAccessAccountIssueMeta(account);
                    const isFreeSelectionBlocked =
                      isFreeAccount && restrictFreeAccounts && !isChecked;
                    const isAccountIssueBlocked =
                      Boolean(accountIssueMeta?.blocksSelection) && !isChecked;
                    const accountStats = allStatsByAccountId.get(account.id)?.usage;

                    const memberInputId = `codex-local-access-member-${account.id}`;
                    const addButtonLabel = isChecked
                      ? t('codex.localAccess.modal.memberAlreadyAdded', '已加入')
                      : isAccountIssueBlocked
                        ? t('codex.localAccess.modal.memberBlockedByIssue', '需处理异常')
                      : t('codex.localAccess.modal.addMember', '加入号池');

                    return (
                      <div
                        key={account.id}
                        className={`group-account-item${isChecked ? ' is-current' : ''}${
                          isCurrentAccount ? ' is-active-account' : ''
                        }${
                          isFreeSelectionBlocked || isAccountIssueBlocked ? ' is-disabled' : ''
                        }${
                          accountIssueMeta?.blocksSelection ? ' is-account-issue' : ''
                        }`}
                      >
                        <input
                          id={memberInputId}
                          type="checkbox"
                          checked={isChecked}
                          disabled={
                            actionBusy ||
                            isPersistedMember ||
                            isFreeSelectionBlocked ||
                            isAccountIssueBlocked
                          }
                          onChange={() => toggleSelect(account.id)}
                        />
                        <label className="group-account-main" htmlFor={memberInputId}>
                          <div className="codex-local-access-member-mainline">
                            <span
                              className="group-account-email"
                              title={maskAccountText(presentation.displayName)}
                            >
                              {maskAccountText(presentation.displayName)}
                            </span>
                            <span className={`tier-badge ${presentation.planClass}`}>
                              {presentation.planLabel}
                            </span>
                            {isCurrentAccount && (
                              <span className="group-account-badge is-current">
                                {t('codex.current', '当前')}
                              </span>
                            )}
                            {accountIssueMeta && renderAccountIssuePill(accountIssueMeta)}
                            <span className="codex-local-access-member-metric">
                              {t('codex.localAccess.stats.accountRequests', {
                                count: accountStats?.requestCount ?? 0,
                                defaultValue: '{{count}} 次请求',
                              })}
                            </span>
                            {renderQuotaPreview(presentation, 2)}
                          </div>
                        </label>
                        <button
                          type="button"
                          className={`btn btn-secondary codex-local-access-candidate-add-btn${
                            isChecked ? ' is-added' : ''
                          }`}
                          onClick={(event) => {
                            event.preventDefault();
                            event.stopPropagation();
                            void addCandidateMember(account.id);
                          }}
                          disabled={
                            actionBusy || isChecked || isFreeSelectionBlocked || isAccountIssueBlocked
                          }
                          title={addButtonLabel}
                          aria-label={`${addButtonLabel}: ${maskAccountText(presentation.displayName)}`}
                        >
                          {isChecked ? <Check size={14} /> : <FolderPlus size={14} />}
                          <span>{addButtonLabel}</span>
                        </button>
                      </div>
                    );
                  })
                )}
              </div>
            </section>
          )}
        </div>

        <div className="modal-footer group-account-picker-footer codex-local-access-modal-footer">
          {isMembersMode ? (
            <>
              <button className="btn btn-secondary" onClick={handleRequestClose} disabled={actionBusy}>
                {t('common.cancel')}
              </button>
              <button
                className="btn btn-primary"
                onClick={() => void handleSaveMembers()}
                disabled={actionBusy || !selectionDirty}
              >
                {saving ? t('common.saving') : t('codex.localAccess.modal.save', '保存集合')}
              </button>
            </>
          ) : (
            <button className="btn btn-secondary" onClick={handleRequestClose} disabled={actionBusy}>
              {t('common.close')}
            </button>
          )}
        </div>
      </div>
    </div>
  );
}

export default CodexLocalAccessModal;
