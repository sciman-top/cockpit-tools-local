import assert from 'node:assert/strict';
import { mkdir, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { pathToFileURL } from 'node:url';
import * as esbuild from 'esbuild';

const root = process.cwd();
const outdir = path.join(tmpdir(), `cockpit-codex-local-access-health-test-${process.pid}`);

await rm(outdir, { force: true, recursive: true });
await mkdir(outdir, { recursive: true });

await esbuild.build({
  entryPoints: {
    codexLocalAccessHealth: path.join(root, 'src/utils/codexLocalAccessHealth.ts'),
  },
  outdir,
  bundle: true,
  format: 'esm',
  platform: 'node',
  entryNames: '[name]',
  outExtension: { '.js': '.mjs' },
  logLevel: 'silent',
});

const health = await import(pathToFileURL(path.join(outdir, 'codexLocalAccessHealth.mjs')).href);

function summary(overrides = {}) {
  return {
    schemaVersion: 1,
    updatedAt: 1_700_000_000_000,
    unavailable: false,
    loadError: null,
    healthyCount: 1,
    estimatedAvailableCount: 0,
    coolingCount: 0,
    exhaustedCount: 0,
    authSuspectCount: 0,
    manualRequiredCount: 0,
    disabledCount: 0,
    activeModelCooldownCount: 0,
    stickyAccountHash: null,
    stickyReason: null,
    stickyExpiresAtMs: null,
    nearestCooldownUntilMs: null,
    lastErrorType: null,
    lastStatus: null,
    lastRequestId: null,
    auditDegraded: false,
    auditError: null,
    auditDegradedAtMs: null,
    ...overrides,
  };
}

assert.equal(
  health.getCodexLocalAccessQuotaAccountRefreshKey(summary()),
  null,
  'healthy local access snapshots should not refresh account files',
);

assert.match(
  health.getCodexLocalAccessQuotaAccountRefreshKey(
    summary({
      lastErrorType: 'usage_limit_reached',
      lastStatus: 429,
      lastRequestId: 'req-quota',
      activeModelCooldownCount: 1,
      nearestCooldownUntilMs: 1_700_000_060_000,
    }),
  ),
  /usage_limit_reached/,
  'quota exhaustion snapshots should refresh account files even when represented as a model cooldown',
);

assert.equal(
  health.isCodexLocalAccessQuotaHealthIssue({
    lastStatus: 429,
    lastErrorType: 'usage_limit_reached',
  }),
  true,
  'account health with 429 usage_limit_reached should render as quota-limited instead of an account error',
);

assert.equal(
  health.isCodexLocalAccessQuotaHealthIssue({
    lastStatus: 401,
    lastErrorType: 'auth_error',
  }),
  false,
  'auth health issues must stay out of the quota-limited rendering path',
);

assert.match(
  health.getCodexLocalAccessQuotaAccountRefreshKey(
    summary({
      estimatedAvailableCount: 1,
      nearestCooldownUntilMs: 1_700_000_060_000,
    }),
  ),
  /\|1\|0\|/,
  'estimated reset recovery should refresh account files once its health signature changes',
);

assert.equal(
  health.getCodexLocalAccessQuotaAccountRefreshKey(
    summary({
      unavailable: true,
      lastErrorType: 'usage_limit_reached',
      exhaustedCount: 1,
    }),
  ),
  null,
  'unavailable health snapshots should not trigger account refresh loops',
);

await rm(outdir, { force: true, recursive: true });
