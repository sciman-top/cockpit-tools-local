# API Service quota drain sticky-routing root cause and fix

Date: 2026-05-27

## Goal

核查第一账号额度耗尽后、用户没有新发消息但第二账号仍被消耗，以及任务长时间停滞后才出现 pool `503` 的原因，并给出不消耗新额度的代码级修复和验证证据。

## Runtime Evidence

- 现场 audit 来源：
  - `C:\Users\sciman\.antigravity_cockpit\codex_local_access_audit.jsonl.1`
  - `C:\Users\sciman\.antigravity_cockpit\codex_local_access_audit.jsonl`
- sidecar checkpoint 来源：
  - `reports/local-hardened-api-realrun/manual-api-service-quota-drain-sidecar-20260527-231035/monitor-checkpoint.json`
- 组合 audit 中，同一个 `x-codex-turn-state` 哈希 lineage 在第一账号 `sha256:154577cc8ccd` 后继续出现在第二账号 `sha256:a3006672e6b9`，例如：
  - `x-codex-turn-state:sha256:d0280debbc2d`
  - `x-codex-turn-state:sha256:a2ed646a542f`
  - `x-codex-turn-state:sha256:02c4f1995874`
  - `x-codex-turn-state:sha256:ad9ff1386e51`
  - `x-codex-turn-state:sha256:3935cde72b8b`
- 这些切号路径的 `routing_decision` 写出了 `request_affinity_mode="soft"` 与 `hard_affinity_bound="false"`，证明现场运行的是此前 soft-affinity 行为，而非过期构建。
- 第二账号随后出现 `upstream_forward 429 -> account_quota_snapshot -> classifier -> quota_classification -> model_cooldown_applied -> fallback_selected -> final_response 503`；同 lineage 的 `final_response 503` 延迟约 67 秒至 100 秒。
- sidecar 较晚启动的窗口显示 `overall="fail"`、`usageLimit=true`、`cooldown=true`、`fallbackSelected=1`，但未覆盖第一账号到第二账号的完整切换起点；跨轮转 audit 才是切号根因证据。

## Official Contract Comparison

本机参考源码：`D:\CODE\external\_reference_gateway_sources\openai-codex`

- `codex-rs/core/src/client.rs` 说明 `ModelClientSession` 按 turn 创建；`x-codex-turn-state` 必须在同一 turn 的 retries、incremental appends 与 continuation requests 中原样复用，不得跨 turn 复用。
- `codex-rs/core/src/session/turn.rs` 在同一 turn 的 sampling loop 内复用同一个 `ModelClientSession`。

因此，一个用户任务在工具调用后产生的后续 `/v1/responses` 请求仍属于原 turn；第一账号已接收前序步骤后，该任务不能因额度耗尽而迁移到第二账号继续消耗额度。

## Root Cause

1. 上一次修改将已完成 stream 后仍携带 `x-codex-turn-state` 的后续请求降级为 soft affinity，允许同一 Codex turn 从耗尽账号切到备用账号。
2. 同一修改把 hard-affinity inline retry wait 扩展至最长 8 天。遇到带长 reset 的额度终止时，请求会挂起等待，最终表现为任务停在可见工具动作或思考阶段，随后才暴露 pool `503`。
3. monitor 旧判据错误要求 sticky task 在额度终止后继续完成，把协议正确的原账号结构化 `429 usage_limit_reached` 误判为回归，从而没有拦住上述错误方向。

## Fix

- `src-tauri/src/modules/codex_local_access.rs`
  - 恢复持久化 `x-codex-turn-state` request affinity 为 hard affinity。
  - 同 turn 的后续请求仅可继续使用原账号；原账号确定性额度耗尽时返回结构化 `429`，不切去备用账号，不退化为池级 `503`。
  - hard-affinity inline retry 仅允许最多 `3s` 的短 reset 恢复；长 reset 立即终止请求。
  - 在 `fallback_blocked` audit 增加 `hard_affinity_bound`、`hard_affinity_source` 与 `request_affinity_mode`，便于现场直接证实 sticky 保护。
- `scripts/monitor-live-codex-app-cockpit-acceptance.ps1`
  - 将 hard-affinity 后结构化 `429 usage_limit_reached` 识别为协议保持的合法终态。
  - 新增超过 `3s` sticky inline wait budget 的失败判据，禁止再次以长等待掩盖额度终止。
- `scripts/smoke-local-hardened-api.ps1`
  - 对齐 live monitor 的合同：同任务 hard-affinity 后的结构化 `usage_limit_reached` 429 是合法终态；非结构化 429 仍失败。
- `scripts/test-local-hardened-api-live-monitor.ps1`
  - 增加结构化 sticky `429` 通过场景与超大等待预算失败场景。
- `scripts/test-local-hardened-api-continuity-acceptance.ps1`
  - 增加 smoke audit fixture，覆盖结构化 sticky 429 通过和非结构化 429 失败。

## Live Validation 2026-05-28

- 新构建 gateway：
  - `cargo build --manifest-path src-tauri\Cargo.toml --target-dir target --bin codex-local-access-gateway`: pass。
  - 二进制路径：`target\debug\codex-local-access-gateway.exe`。
- 真实上游 bounded drain：
  - 命令入口：`scripts\accept-local-hardened-api-continuity.ps1 -Model gpt-5.5 -AcknowledgeLiveUpstreamRisk -DrainFirstFreeAccountUntilFallback -DrainMaxRequests 30 -DrainRequestIntervalSeconds 22 -TimeoutSeconds 1200`。
  - 证据目录：`reports/local-hardened-api-realrun/new-gateway-acceptance-20260528-001841/`。
  - 结果：当前 isolated pool 中两个候选账号均被上游确认 `usage_limit_reached`，后续请求在本地快速返回 pool unavailable；该 run 无法证明“先成功建立同 turn sticky，再额度耗尽”的完整路径，因此提前停止自有验证进程树，避免空等 30 个间隔。
  - sidecar 摘要：`has429=true`、`blockedAccountCount=2`、`localPoolUnavailableCount=11`、`retryLimitErrorFound=false`、`responsesTransport503PoolUnavailableCount=0`、`lineageAccountSwitchCount=0`、`codexCliGuard.comparison.unchanged=true`。
- 新 gateway sticky-affinity 探针：
  - 证据：`reports/local-hardened-api-realrun/new-gateway-acceptance-20260528-001841/sticky-affinity-probe-summary.json`。
  - 结果：`overall=pass`；同一 `x-codex-turn-state` 绑定后只观察到一个账号哈希；`secondAccountTouched=false`；`requestAffinityModes=["turn_state_hard"]`；`hardAffinityBoundFlags=["true"]`；`hardAffinityFallbackBlockedCount=1`；`attemptLimitFallbackBlockedCount=0`；最终 `statusCode=429`，耗时约 `1250ms`。
  - 该探针复核了本轮核心回归：同 turn 额度终止后不再切到第二账号，也不进入长时间停滞。

## Verification

- `npm run build`: pass.
- `cargo test --manifest-path src-tauri/Cargo.toml --lib`: pass, `267 passed; 0 failed; 2 ignored`.
- `node scripts/release/preflight.cjs --skip-typecheck --skip-build --skip-cargo --skip-cargo-test`: pass.
- `pwsh -NoProfile -ExecutionPolicy Bypass -File scripts/test-local-hardened-api-continuity-acceptance.ps1`: pass.
- `pwsh -NoProfile -ExecutionPolicy Bypass -File scripts/test-local-hardened-api-live-monitor.ps1`: pass.
- `pwsh -NoProfile -ExecutionPolicy Bypass -File scripts/test-local-hardened-api-live-risk-guard.ps1`: pass.
- `git diff --check`: pass; only line-ending conversion warnings reported by Git.

## Follow-up Audit 2026-05-28 01:40

- User-observed interruption:
  - `x-codex-turn-state:sha256:85637d98520a` was bound to the first account `sha256:cd32ac761d93`.
  - The previous same-turn request completed with `stream_terminal(response_completed_seen=true)` and `stream_completed`.
  - The next same-turn request received upstream `429 usage_limit_reached` with `retry_after_ms=604293000`, then `fallback_blocked(outcome=hard_affinity)`.
  - Runtime `final_response` preserved HTTP 429 but misclassified the audit error as `rate_limited` and omitted `provider_code`, causing the sidecar to report `sameTaskAffinityUnstructuredTerminal429Count=1`.
- Second-account exhaustion:
  - `x-codex-turn-state:sha256:73bb1cede902` repeated the same pattern on account `sha256:07420f7e5566`, with upstream `429 usage_limit_reached` and `retry_after_ms=603168000`.
  - This is expected once both pool accounts are exhausted: same-turn sticky routing must not consume another account; the terminal must be quick, structured, and attributable to upstream quota.
- Official source comparison:
  - Local reference `D:\CODE\external\_reference_gateway_sources\openai-codex` at `c4e53d103c102f8d5201247adbc60bbddd47c88d` documents `x-codex-turn-state` as a per-turn sticky-routing token replayed for retries, incremental appends, and continuation requests.
  - `previous_response_id` is generated for websocket incremental continuation; `x-codex-turn-metadata` remains observability lineage only.
- Additional fix:
  - `write_proxy_dispatch_error_response` now derives final audit error type from sticky Responses 429 context and records `provider_code=usage_limit_reached`, `terminal_origin=upstream_quota_error`, and `sticky_boundary`.
  - The live monitor now treats a terminal 429 as structured quota when the immediately preceding hard-affinity block already carried `usage_limit_reached`; this lets old audit snapshots be diagnosed correctly without rewriting history.
  - Added monitor fixture for the observed shape: structured `fallback_blocked` plus old `final_response(errorType=rate_limited)` must pass as structured quota terminal, not unstructured terminal 429.
- Read-only monitor recheck:
  - Replaying current local audit after the monitor fix converts `x-codex-turn-state:sha256:85637d98520a` and `x-codex-turn-state:sha256:73bb1cede902` to `sameTaskAffinityStructuredQuotaTerminal429Count=2` and `sameTaskAffinityUnstructuredTerminal429Count=0`.
  - Overall replay still fails because older events in the broad audit window include one upstream stream error and 30 historical Responses transport `503/pool_unavailable` events; these are separate pre-existing findings, not the structured quota terminal bug fixed here.
- Verification delta:
  - `pwsh -NoProfile -ExecutionPolicy Bypass -File .\scripts\test-local-hardened-api-live-monitor.ps1`: pass.
  - `cargo test --manifest-path src-tauri/Cargo.toml codex_turn_state_hard_affinity_blocks_fallback_after_usage_limit --lib`: pass.
  - `npm run build`: pass.
  - `cargo test --manifest-path src-tauri/Cargo.toml --lib`: pass, `267 passed; 0 failed; 2 ignored`.
  - `node scripts/release/preflight.cjs --skip-typecheck --skip-build --skip-cargo --skip-cargo-test`: pass.
  - `git diff --check`: pass; only Git line-ending conversion warnings.

## Follow-up Monitor Window Filter 2026-05-28

- Problem:
  - A broad `-IncludeExistingAudit` replay over `C:\Users\sciman\.antigravity_cockpit\codex_local_access_audit.jsonl` mixed the current quota-drain evidence with older stream error and transport `503/pool_unavailable` events.
  - That made a useful root-cause replay noisy: the fixed `85637...` and `73bb...` quota terminals were classified correctly, but the overall result still failed from historical unrelated events.
- Fix:
  - `scripts/monitor-live-codex-app-cockpit-acceptance.ps1` now accepts:
    - `-AuditSinceTimestampMs <ms>`
    - `-AuditUntilTimestampMs <ms>`
    - `-FocusGatewayRequestIds <id[]>`
  - These filters apply only to the in-memory verdict window; the raw audit file remains untouched.
  - Reports include an `auditWindow` block with `sinceTimestampMs`, `untilTimestampMs`, `focusGatewayRequestIds`, `rawObservedEventCount`, `filteredEventCount`, and `droppedEventCount`.
- Synthetic proof:
  - Added a fixture where an old Codex-facing Responses transport `503` is present in the same audit file, while the focused current `gateway_request_id` completes normally.
  - With `-AuditSinceTimestampMs 100 -FocusGatewayRequestIds gw-window-good`, the verdict ignores the old `503`: `responsesTransport503PoolUnavailableCount=0`, `completedStreamCount=1`, `rawObservedEventCount=6`, `filteredEventCount=4`, `droppedEventCount=2`.
- Verification delta:
  - `pwsh -NoProfile -ExecutionPolicy Bypass -File .\scripts\test-local-hardened-api-live-monitor.ps1`: pass.
  - `node scripts/release/preflight.cjs --skip-typecheck --skip-build --skip-cargo --skip-cargo-test`: pass.

## Follow-up Runtime Availability Probe 2026-05-28 02:35

- User-observed interruption:
  - `stream disconnected before completion: idle timeout waiting for SSE` appeared transiently.
  - More importantly, after quota exhaustion all tasks later terminated with `stream disconnected before completion: error sending request for url (http://127.0.0.1:45336/v1/responses)`.
- Runtime root cause:
  - Read-only probe at `2026-05-28T02:34:51+08:00` found live Cockpit process `target\debug\cockpit-tools.exe`, but `codex_local_access.json` had `enabled=false`, `port=45336`, `fallbackMode=disabled`.
  - `codex_runtime_mode.json` had `mode=direct_projection`.
  - `Get-NetTCPConnection -LocalPort 45336 -State Listen` returned no listener.
  - Therefore the `error sending request for url (http://127.0.0.1:45336/v1/responses)` symptom is a local gateway runtime availability failure: Codex was pointed at a local Responses base URL after the gateway had been disabled/stopped or restored to Direct Projection. It is not a structured upstream quota terminal.
- Build freshness finding:
  - `target\debug\codex-local-access-gateway.exe`: `2026-05-28T02:43:52+08:00`, freshly rebuilt during this investigation.
  - `target\debug\cockpit-tools.exe`: `2026-05-28T01:37:32+08:00`.
  - `target\release\cockpit-tools.exe`: `2026-05-24T22:54:52+08:00`.
  - So the standalone gateway smoke used the latest gateway code, while the currently running desktop app binary was still older than the latest monitor/root-cause changes. Any claim about the live desktop app must distinguish these binaries.
- Monitor fix:
  - `scripts/monitor-live-codex-app-cockpit-acceptance.ps1` now records an `apiServiceRuntime` snapshot from `codex_local_access.json`, `codex_runtime_mode.json`, `server.json`, and the local TCP listener.
  - New switch: `-RequireApiServiceRuntimeAvailable`.
  - New result: `api_service_runtime_available`. When required, `enabled=false`, missing port, or no listener fails the report with a direct runtime reason instead of blending the symptom into quota/fallback analysis.
- Synthetic proof:
  - `scripts/test-local-hardened-api-live-monitor.ps1` now includes a fixture with good historical audit events but `codex_local_access.json enabled=false` and `codex_runtime_mode.json mode=direct_projection`.
  - Expected result: `overall=fail`, `api_service_runtime_available=fail`, `apiServiceRuntime.reason=local_access_disabled`.
- Read-only live proof:
  - Command: `pwsh -NoProfile -ExecutionPolicy Bypass -File .\scripts\monitor-live-codex-app-cockpit-acceptance.ps1 -DurationSeconds 0 -IncludeExistingAudit -RequireApiServiceRuntimeAvailable -WriteReport -ReportDir reports\local-hardened-api-realrun\runtime-state-monitor-20260528-0235 -Quiet`.
  - Report: `reports/local-hardened-api-realrun/runtime-state-monitor-20260528-0235/live-monitor-20260528-023956.json`.
  - Result: `overall=fail`, `api_service_runtime_available=fail`, `apiServiceRuntime.reason=local_access_disabled`, `apiBaseUrl=http://127.0.0.1:45336/v1`, `listenerCount=0`, `runtimeMode.mode=direct_projection`.

## New Gateway Real Smoke 2026-05-28 02:45

- App-safe isolated probe:
  - Command shape: `scripts\smoke-local-hardened-api.ps1 -Stage small_pool -Model gpt-5.5 -StartEphemeralGateway -AppSafeIsolatedProbe -AcknowledgeLiveUpstreamRisk -RunUpstreamSmoke -RunCodexExecSmoke -AssertCodexCliConfigUntouched -AssertCodexAppProcessStable -WriteReport`.
  - Report: `reports/local-hardened-api-smoke/smoke-20260528-024553.json`.
  - Result: `overall=pass`, `stage=small_pool`, `single_account_upstream_chat=pass`, `codex_exec_task_e2e=pass`, `codex_cli_config_auth_untouched=pass`, `codex_app_process_stable=pass`.
  - Gateway: new `target\debug\codex-local-access-gateway.exe`, ready at `http://127.0.0.1:45335/v1`, stopped and restored by the script.
- Live data-root temporary gateway probe:
  - Command shape: `scripts\smoke-local-hardened-api.ps1 -Stage small_pool -Model gpt-5.5 -StartEphemeralGateway -AcknowledgeLiveUpstreamRisk -RunUpstreamSmoke -RunCodexExecSmoke -AssertCodexCliConfigUntouched -AssertCodexAppProcessStable -WriteReport`.
  - Report: `reports/local-hardened-api-smoke/smoke-20260528-024703.json`.
  - Result: `overall=pass`, `dataRoot=C:\Users\sciman\.antigravity_cockpit`, `single_account_upstream_chat=pass`, `codex_exec_task_e2e=pass`, `codex_cli_config_auth_untouched=pass`, `codex_app_process_stable=pass`.
  - Gateway: ready at `http://127.0.0.1:45336/v1`, `restoredConfig=true`, stopped after the smoke.
  - Post-smoke restore check: `codex_local_access.json enabled=false`, `listenerCount=0`, `codex_runtime_mode.json mode=direct_projection`, matching the pre-existing live state.
- Interpretation:
  - The latest standalone gateway code path works against real upstream and an isolated Codex task.
  - The live failure observed by the user requires a separate runtime-switch guard: when Codex is configured to use `http://127.0.0.1:45336/v1`, monitor must also prove the gateway is still enabled and listening.
  - No additional quota drain was run in this follow-up; the smoke only performed bounded real upstream and isolated Codex exec checks.

## Risk And Rollback

- 本轮没有启动、停止或重启 Codex App、Codex CLI 或 Cockpit live service；只启动并停止了自有 isolated ephemeral gateway/probe 进程。
- 已按授权执行真实上游 bounded drain；当前账号池状态使完整 drain acceptance 被阻断，但 sticky-affinity 探针已用新 gateway 复核核心合同。
- 若新的受控 live run 显示协议不兼容，可仅回退本报告列出的代码/脚本文件变更；保留原始 audit 与 sidecar 数据作为事故证据。
