import assert from 'node:assert/strict';
import { mkdir, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { pathToFileURL } from 'node:url';
import * as esbuild from 'esbuild';

const root = process.cwd();
const outdir = path.join(tmpdir(), `cockpit-codex-sort-test-${process.pid}`);

await rm(outdir, { force: true, recursive: true });
await mkdir(outdir, { recursive: true });

await esbuild.build({
  entryPoints: {
    accountOrder: path.join(root, 'src/utils/accountOrder.ts'),
    floatingCardSelectors: path.join(root, 'src/utils/floatingCardSelectors.ts'),
    codexAccountSort: path.join(root, 'src/utils/codexAccountSort.ts'),
    codexLocalAccessUiState: path.join(root, 'src/utils/codexLocalAccessUiState.ts'),
    codexTypes: path.join(root, 'src/types/codex.ts'),
  },
  outdir,
  bundle: true,
  format: 'esm',
  platform: 'node',
  entryNames: '[name]',
  outExtension: { '.js': '.mjs' },
  logLevel: 'silent',
});

const accountOrder = await import(pathToFileURL(path.join(outdir, 'accountOrder.mjs')).href);
const selectors = await import(pathToFileURL(path.join(outdir, 'floatingCardSelectors.mjs')).href);
const sort = await import(pathToFileURL(path.join(outdir, 'codexAccountSort.mjs')).href);
const localAccessUiState = await import(pathToFileURL(path.join(outdir, 'codexLocalAccessUiState.mjs')).href);
const codexTypes = await import(pathToFileURL(path.join(outdir, 'codexTypes.mjs')).href);

function codexAccount(id, quota, extra = {}) {
  return {
    id,
    email: `${id}@example.test`,
    tokens: {
      id_token: `${id}-id-token`,
      access_token: `${id}-access-token`,
    },
    quota,
    created_at: extra.created_at ?? 1,
    last_used: extra.last_used ?? 1,
    ...extra,
  };
}

function quota(hourly, weekly, extra = {}) {
  return {
    hourly_percentage: hourly,
    weekly_percentage: weekly,
    hourly_window_present: true,
    weekly_window_present: true,
    ...extra,
  };
}

assert.deepEqual(
  accountOrder.normalizeAccountOrder(['pool-member'], ['candidate-a', 'pool-member', 'candidate-b']),
  ['pool-member', 'candidate-a', 'candidate-b'],
  'Full account order normalization keeps its custom-sort fill behavior',
);

assert.deepEqual(
  accountOrder.normalizeSelectedAccountOrder(
    ['pool-member', 'missing', 'pool-member'],
    ['candidate-a', 'pool-member', 'candidate-b'],
  ),
  ['pool-member'],
  'API service member persistence must not append every available candidate account',
);

const exhaustedWeeklyButRecentlyUsed = codexAccount(
  'exhausted-weekly',
  quota(100, 0),
  { last_used: 999 },
);
const availableLowQuota = codexAccount('available-low', quota(10, 10));

assert.equal(
  selectors.getRecommendedCodexAccount(
    [exhaustedWeeklyButRecentlyUsed, availableLowQuota],
    null,
  )?.id,
  'available-low',
  'Codex recommendation must not prefer an account whose weekly quota is exhausted',
);

assert.deepEqual(
  [
    exhaustedWeeklyButRecentlyUsed,
    availableLowQuota,
    codexAccount('available-high', quota(80, 80)),
  ]
    .sort((left, right) =>
      sort.compareCodexAccountsByRecommendedSort(left, right, {
        apiServiceSortMeta: new Map([
          ['exhausted-weekly', 0],
          ['available-low', 1],
          ['available-high', 2],
        ]),
      }),
    )
    .map((account) => account.id),
  ['available-high', 'available-low', 'exhausted-weekly'],
  'API service members should keep pool priority but sort usable quota before exhausted accounts',
);

assert.deepEqual(
  [
    exhaustedWeeklyButRecentlyUsed,
    availableLowQuota,
    codexAccount('available-medium', quota(40, 40)),
  ]
    .sort((left, right) =>
      sort.compareCodexAccountsByRecommendedSort(left, right, {
        groupSortMeta: new Map([
          ['exhausted-weekly', { sortOrder: 0, accountIndex: 0 }],
          ['available-low', { sortOrder: 0, accountIndex: 1 }],
          ['available-medium', { sortOrder: 0, accountIndex: 2 }],
        ]),
      }),
    )
    .map((account) => account.id),
  ['available-medium', 'available-low', 'exhausted-weekly'],
  'Grouped Codex cards should sort usable quota before stale group insertion order',
);

assert.deepEqual(
  sort.sortCodexLocalAccessAccountIdsForScheduling(
    ['quota-80-late', 'current-low', 'quota-80-soon', 'quota-30'],
    [
      codexAccount('quota-80-late', quota(80, 80, { hourly_reset_time: 900 })),
      codexAccount('current-low', quota(1, 1, { hourly_reset_time: 100 })),
      codexAccount('quota-80-soon', quota(80, 80, { hourly_reset_time: 300 })),
      codexAccount('quota-30', quota(30, 30, { hourly_reset_time: 200 })),
    ],
    'current-low',
  ),
  ['current-low', 'quota-80-soon', 'quota-80-late', 'quota-30'],
  'Scheduling helper should pin the current account, then sort by quota and reset time',
);

assert.deepEqual(
  sort.sortCodexLocalAccessAccountsForStableDisplay(
    [
      codexAccount('saved-first', quota(0, 97), { created_at: 30 }),
      codexAccount('current-saved-second', quota(2, 97), { created_at: 20 }),
      codexAccount('saved-third', quota(0, 97), { created_at: 10 }),
      codexAccount('weekly-low', quota(0, 4), { created_at: 40 }),
    ],
    'current-saved-second',
  ).map((account) => account.id),
  ['current-saved-second', 'saved-first', 'saved-third', 'weekly-low'],
  'API service member display should pin the usable current account without reordering the saved pool',
);

assert.deepEqual(
  sort.sortCodexLocalAccessAccountIdsForScheduling(
    ['quota-40-late', 'quota-90', 'current-low', 'quota-40-soon'],
    [
      codexAccount('quota-40-late', quota(40, 40, { hourly_reset_time: 800 })),
      codexAccount('quota-90', quota(90, 90, { hourly_reset_time: 700 })),
      codexAccount('current-low', quota(1, 1, { hourly_reset_time: 100 })),
      codexAccount('quota-40-soon', quota(40, 40, { hourly_reset_time: 200 })),
    ],
    'current-low',
  ),
  ['current-low', 'quota-90', 'quota-40-soon', 'quota-40-late'],
  'Scheduling helper should re-rank schedulable accounts by quota and reset time',
);

assert.deepEqual(
  sort.sortCodexLocalAccessAccountIdsForStableDisplay(
    [
      'current-weekly-0',
      'weekly-0-more-requests',
      'weekly-0-fewer-requests',
      'weekly-97',
    ],
    [
      codexAccount('current-weekly-0', quota(31, 0)),
      codexAccount('weekly-0-more-requests', quota(37, 0)),
      codexAccount('weekly-0-fewer-requests', quota(18, 0)),
      codexAccount('weekly-97', quota(20, 97, { hourly_reset_time: 200 })),
    ],
    'current-weekly-0',
  ),
  [
    'weekly-0-more-requests',
    'weekly-0-fewer-requests',
    'weekly-97',
    'current-weekly-0',
  ],
  'API service member display should move only the exhausted current account to the end',
);

assert.deepEqual(
  sort.sortCodexLocalAccessAccountIdsForStableDisplay(
    ['current-low', 'saved-first', 'quota-90', 'quota-40-soon'],
    [
      codexAccount('current-low', quota(1, 1, { hourly_reset_time: 100 })),
      codexAccount('saved-first', quota(15, 15, { hourly_reset_time: 900 })),
      codexAccount('quota-90', quota(90, 90, { hourly_reset_time: 700 })),
      codexAccount('quota-40-soon', quota(40, 40, { hourly_reset_time: 200 })),
    ],
    'current-low',
  ),
  ['current-low', 'saved-first', 'quota-90', 'quota-40-soon'],
  'API service member display should not reshuffle non-current accounts after quota refresh',
);

assert.deepEqual(
  sort.sortCodexLocalAccessAccountIdsForStableDisplay(
    ['current-refresh-error', 'saved-first', 'quota-90'],
    [
      codexAccount('current-refresh-error', quota(40, 40), {
        quota_error: { message: 'quota refresh failed', timestamp: 1 },
      }),
      codexAccount('saved-first', quota(15, 15)),
      codexAccount('quota-90', quota(90, 90)),
    ],
    'current-refresh-error',
  ),
  ['current-refresh-error', 'saved-first', 'quota-90'],
  'API service member display should not move the current account to the end for a non-limit refresh error',
);

{
  const displayAccounts = [
    codexAccount('current-weekly-0', quota(31, 0)),
    codexAccount('oauth-next', quota(100, 100)),
    codexAccount('api-key', quota(100, 100), { auth_mode: 'apikey' }),
  ];
  const displayIds = sort.sortCodexLocalAccessAccountIdsForStableDisplay(
    ['current-weekly-0', 'oauth-next', 'api-key'],
    displayAccounts,
    'current-weekly-0',
  );

  assert.deepEqual(
    displayIds,
    ['oauth-next', 'api-key', 'current-weekly-0'],
    'API service card display should keep the saved order and move an exhausted current account to the end',
  );
  assert.equal(
    sort.getCodexLocalAccessPrimaryRefreshAccountId(displayIds, displayAccounts),
    'oauth-next',
    'API service card refresh must target the first displayed OAuth account after stable display ordering',
  );
}

assert.equal(
  sort.getCodexLocalAccessPrimaryRefreshAccountId(
    ['quota-error', 'api-key'],
    [
      codexAccount('quota-error', quota(100, 100), {
        quota_error: { message: 'quota refresh failed', timestamp: 1 },
      }),
      codexAccount('api-key', quota(100, 100), { auth_mode: 'apikey' }),
    ],
  ),
  'quota-error',
  'API service card refresh must use display order instead of refresh-priority sorting',
);

assert.equal(
  sort.getCodexLocalAccessPrimaryRefreshAccountId(
    ['api-key', 'oauth-second'],
    [
      codexAccount('api-key', quota(100, 100), { auth_mode: 'apikey' }),
      codexAccount('oauth-second', quota(50, 50)),
    ],
  ),
  'oauth-second',
  'API service card refresh should skip API-key credentials when resolving the displayed primary account',
);

assert.equal(
  localAccessUiState.getCodexLocalAccessPrimaryActionKind(
    false,
    { mode: 'direct_projection', accountKind: 'oauth', currentAccountId: 'acc-direct', updatedAt: 1 },
  ),
  'activate',
  'API service card should offer activation while Codex remains in Direct API/OAuth mode',
);

assert.equal(
  localAccessUiState.getCodexLocalAccessPrimaryActionKind(
    false,
    { mode: 'cockpit_api_service', accountKind: 'oauth', currentAccountId: 'acc-api', updatedAt: 1 },
  ),
  'deactivate',
  'API service card should offer deactivation only when Codex is using Cockpit API Service mode',
);

assert.equal(
  localAccessUiState.getCodexLocalAccessPrimaryActionKind(true, null),
  'deactivate',
  'API service card should also offer deactivation when the default Codex launch binding is the API service account',
);

const refreshNowMs = 1_700_000_000_000;
const quotaLimitedError = {
  code: 'usage_limit_reached',
  message: 'API 返回错误 429 [error_code:usage_limit_reached] [reset_after_seconds:120]',
  timestamp: refreshNowMs - 20_000,
};

assert.equal(
  codexTypes.isCodexQuotaLimitError(quotaLimitedError),
  true,
  'Codex 429 usage_limit_reached should be classified as quota-limited, not an account error',
);

assert.deepEqual(
  {
    kind: codexTypes.getCodexQuotaIssueInfo(quotaLimitedError).kind,
    displayCode: codexTypes.getCodexQuotaIssueInfo(quotaLimitedError).displayCode,
  },
  {
    kind: 'limited',
    displayCode: 'usage_limit_reached',
  },
  'Codex quota issue presentation should keep usage_limit_reached out of the red error path',
);

assert.equal(
  codexTypes.getCodexQuotaIssueInfo({
    code: 'usage_limit_reached',
    message: '',
    timestamp: refreshNowMs,
  }).kind,
  'limited',
  'Codex quota issue presentation should classify code-only usage_limit_reached as quota-limited',
);

assert.equal(
  codexTypes.getCodexQuotaIssueInfo({
    message:
      'Cockpit API service upstream quota exhausted: status=429, error_type=usage_limit_reached, provider_code=usage_limit_reached, reset_at=1780158587',
    timestamp: refreshNowMs,
  }).displayCode,
  'usage_limit_reached',
  'Codex quota issue presentation should extract error_type from Cockpit API service quota snapshots',
);

assert.equal(
  codexTypes.getCodexQuotaIssueInfo({
    message: '{"error":{"type":"usage_limit_reached","code":"usage_limit_reached"}}',
    timestamp: refreshNowMs,
  }).kind,
  'limited',
  'Codex quota issue presentation should classify JSON usage_limit_reached bodies as quota-limited',
);

assert.equal(
  codexTypes.isCodexAccountErrorState(codexAccount('limited', quota(0, 0), {
    quota_error: quotaLimitedError,
  })),
  false,
  'Quota-limited Codex accounts should stay out of the ERROR/abnormal bucket',
);

assert.equal(
  codexTypes.isCodexAccountErrorState(codexAccount('unauthorized', quota(100, 100), {
    quota_error: {
      message: 'API 返回错误 401 [error_code:invalid_token]',
      timestamp: refreshNowMs - 20_000,
    },
  })),
  true,
  'Codex 401 invalid_token should remain an account error',
);

assert.deepEqual(
  sort.sortCodexLocalAccessAccountIdsForRefresh(
    [
      'current-schedulable',
      'future-exhausted',
      'quota-limited',
      'missing-quota',
      'reset-due',
      'quota-error',
      'api-key',
    ],
    [
      codexAccount('current-schedulable', quota(95, 95), { last_used: 999 }),
      codexAccount(
        'future-exhausted',
        quota(0, 0, { weekly_reset_time: Math.floor((refreshNowMs + 60_000) / 1000) }),
      ),
      codexAccount(
        'quota-limited',
        quota(0, 0, { weekly_reset_time: Math.floor((refreshNowMs + 120_000) / 1000) }),
        { quota_error: quotaLimitedError },
      ),
      codexAccount('missing-quota', undefined),
      codexAccount(
        'reset-due',
        quota(0, 0, { weekly_reset_time: Math.floor((refreshNowMs - 60_000) / 1000) }),
      ),
      codexAccount('quota-error', quota(100, 100), {
        quota_error: { message: 'quota refresh failed', timestamp: refreshNowMs - 10_000 },
      }),
      codexAccount('api-key', quota(100, 100), { auth_mode: 'apikey' }),
    ],
    refreshNowMs,
  ),
  [
    'quota-error',
    'missing-quota',
    'reset-due',
    'future-exhausted',
    'quota-limited',
    'current-schedulable',
    'api-key',
  ],
  'Local access refresh priority must refresh stale state first without treating quota-limited accounts as hard errors',
);

assert.deepEqual(
  sort.sortCodexLocalAccessAccountIdsForRefresh(
    ['healthy-b', 'healthy-a'],
    [
      codexAccount('healthy-a', quota(80, 80), { created_at: 20 }),
      codexAccount('healthy-b', quota(80, 80), { created_at: 10 }),
    ],
    refreshNowMs,
  ),
  ['healthy-b', 'healthy-a'],
  'Local access refresh priority should preserve caller order when accounts have the same refresh need',
);

await rm(outdir, { force: true, recursive: true });
