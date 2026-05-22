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
    floatingCardSelectors: path.join(root, 'src/utils/floatingCardSelectors.ts'),
    codexAccountSort: path.join(root, 'src/utils/codexAccountSort.ts'),
  },
  outdir,
  bundle: true,
  format: 'esm',
  platform: 'node',
  entryNames: '[name]',
  outExtension: { '.js': '.mjs' },
  logLevel: 'silent',
});

const selectors = await import(pathToFileURL(path.join(outdir, 'floatingCardSelectors.mjs')).href);
const sort = await import(pathToFileURL(path.join(outdir, 'codexAccountSort.mjs')).href);

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

await rm(outdir, { force: true, recursive: true });
