# Cockpit API 服务号池调度专项计划

更新时间：2026-05-23

## 目标与裁决边界

本专项承接 `docs/LOCAL_HARDENED_API.md`、`docs/LOCAL_HARDENED_API_ROADMAP.md`、`docs/LOCAL_HARDENED_API_IMPLEMENTATION_PLAN.md` 和 `docs/reference-gateway-best-practices.md`。当前落点是 Cockpit 本机 API service 的多账号号池调度；目标归宿是高连续性、低并发、低刷新、可解释、可手动恢复的自用调度系统。

本专项不追求把 500+ free 账号做成高频吞吐池，也不把随机轮换、全池扫射或风控规避作为优化目标。高效调度的定义是：

- 当前任务不因单个账号额度耗尽而过早中断。
- 新请求能避开明确 cooling/exhausted/manual/auth blocked 的账号。
- 本地 backpressure 先于上游请求生效，减少 429 连撞。
- 失败、fallback、cooldown 和恢复都能通过脱敏 audit 与 UI 解释。
- 实跑验收默认隔离，不破坏当前 Codex CLI/App 会话。

## 官方与本地证据分层

官方文档用于确认限流和错误语义；社区优秀项目只作为结构参考。

- OpenAI 官方限流文档说明 rate limits 按 RPM/RPD/TPM 等维度生效，且可在 organization 和 project 层定义；失败重试仍会消耗 per-minute limit，所以不能连续重发。参考：<https://developers.openai.com/api/docs/guides/rate-limits>
- OpenAI 官方错误码文档将 429 区分为 request rate limit 和 quota/billing limit，将 503 slow down 解释为突增流量导致的临时节流；建议 pacing、backoff、尊重 response headers，并保持稳定速率后逐步恢复。参考：<https://developers.openai.com/api/docs/guides/error-codes>
- Gemini API 官方 rate limits 明确限制按 project 统计，不按 API key 统计；多个 key 不应被假设为多个独立额度池。参考：<https://ai.google.dev/gemini-api/docs/rate-limits>
- Anthropic 官方 rate limits 文档说明限制按 organization/workspace/model class 管理，并通过 `retry-after` header 表达等待窗口。参考：<https://docs.anthropic.com/en/api/rate-limits>
- 本地参考源仍优先使用 `D:\CODE\external\_reference_gateway_sources` 和 `docs/reference-gateway-best-practices.md`，只吸收 `IsSchedulable()`、persistent cooldown、fill-first/session affinity、pre-call rate checks、首字节后不重试等可本地文件化的模式。

## 默认策略

AI 推荐默认策略：`sticky_process + fill_first + capped fallback`。

理由：它在保持低速率的前提下最大化任务连续性，且不会把每个请求变成跨账号随机探测。

默认配置语义：

- `maxConcurrentRequests = 1`
- `minRequestIntervalSeconds >= 20`
- `maxQueueWaitSeconds >= minRequestIntervalSeconds + 1`
- `maxRetries = 1`
- `effectiveMaxRetryAccounts = 2`
- `fallbackMode` 不阻断当前请求内的 failover-safe 429 切号。
- 当前 process sticky account healthy 时继续使用该账号。
- 当前账号因明确 `usage_limit_reached`、`insufficient_quota`、`auth_error`、`captcha_or_suspicious`、manual pause 或 model cooldown 不可调度时，才进入下一个 healthy candidate。
- candidate pool 可以包含完整配置号池，但单请求真实上游尝试数必须受 cap 控制。

禁止默认开启：

- request-level random routing。
- 每个请求推进 round-robin cursor。
- weighted routing 作为 hardened 默认。
- 一个请求失败后扫完整号池。
- cooldown 期间通过高频 `wham/usage` 或上游小请求探测恢复。
- 自动扫描 `codex_accounts.json` 后挑账号写入临时号池。
- 为规避平台识别而设计 UA/IP/指纹伪装逻辑。

## 排序与轮换策略

同一个出口 IP 下，应尽量少切换不同账号。排序策略的核心不是“优先使用没用过的新账号”，而是“优先延续已用过且已恢复的账号，把新账号作为 reserve”。这样能减少账号/IP 组合变化，也能让一周重置后的账号自然回到队首。

排序必须分三类，不要混用：

- 调度排序：决定 API service 下一次真实上游请求选哪个账号。
- 卡片排序：帮助用户看清当前可用、冷却、重置和 reserve 状态。
- 刷新排序：决定有限 quota refresh 应优先刷新哪些账号。

### 调度排序

调度排序只在 `AccountHealthRegistry` 判定可调度后才进入比较。推荐 bucket 顺序：

1. `sticky_current_healthy`：当前 process sticky 或 `previous_response_id` affinity 的健康账号。
2. `used_recovered_healthy`：曾经在 API service 中成功使用过，且 weekly reset 已到或已刷新证明周额度恢复的账号。
3. `used_remaining_healthy`：曾经使用过，仍有明确可用周额度的账号。
4. `manual_order_healthy`：用户在 API 服务号池中手动排在前面的健康账号。
5. `new_reserve_healthy`：从未被 API service 成功使用过的账号，只作为 reserve。
6. `cooling_or_exhausted`：有 reset/cooldown hint 的账号，不可调度，但在 UI 中按 reset 时间展示。
7. `auth_or_manual_required`：需要重新登录、人工确认或用户暂停的账号，永不自动调度。

同 bucket 内的 tie-break：

- 优先 `last_success_at` 最近的账号，保持同 IP 下账号连续性。
- 对已耗尽但 reset 已到的账号，优先 `last_quota_exhausted_at` 更早的账号，避免刚恢复账号马上被重复打满。
- 周额度相同且都健康时，保留 API 服务号池手动顺序；不要因为卡片视图排序变化重排调度顺序。
- `weekly_reset_time` 只用于判断恢复或展示等待，不应让“还没恢复但 reset 时间最近”的账号抢在健康账号前面。

### 卡片主页与分组排序

卡片主页、Codex 分组和 API 服务号池视图应使用同一套状态 bucket，但展示目标和调度目标不同：

1. API 服务号池成员置顶，但成员内部显示上述调度 bucket。
2. 当前账号/本次已启用账号保留明显标记，避免用户误以为系统会频繁换号。
3. 分组内默认保留用户手动顺序；选择“推荐排序”时才按 API service scheduling score 排。
4. 曾用且已重置的账号应排在新账号前面；新账号用“备用”语义展示，不应因为 100% 周额度排到最前。
5. 已耗尽或冷却账号保留在可见列表中，按 nearest reset/cooldown 升序展示，方便判断何时恢复。
6. auth/manual/suspicious 账号放到最后，明确需要人工动作，不参与自动切换。

### 刷新排序

刷新排序服务于低频确认，不服务于扫号。

优先刷新：

1. API 服务号池内、曾用过、weekly reset 已到但 quota cache 过期的账号。
2. 当前 sticky 账号和最近成功账号。
3. 用户当前打开分组内的账号。

降低优先级或跳过：

- 从未使用过的新账号。
- cooldown 未到期账号。
- auth/manual/suspicious 账号。
- 超过 50 个目标的批量刷新。

刷新排序必须受 live-risk guard 约束：默认最多低频、少量刷新；不得为了找恢复账号而扫完整池。

## 调度状态机

账号可调度性统一由 `AccountHealthRegistry` 决定，selector、retry/fallback、UI 不应各自复制判断。

建议状态：

| 状态 | 可调度 | 触发来源 | 恢复方式 |
| --- | --- | --- | --- |
| `healthy` | 是 | 成功请求或无阻断状态 | N/A |
| `cooling_down` | 否 | `429`、`Retry-After`、body reset、model capacity | reset/cooldown 到期或手动恢复 |
| `quota_exhausted` | 否 | 明确 `usage_limit_reached`、`insufficient_quota`、quota exceeded | reset 到期、人工确认或手动恢复 |
| `auth_suspect` | 否 | 401 refresh 失败、revoked/invalid token | 重新登录或人工确认 |
| `manual_required` | 否 | 403、captcha、suspicious、policy/safety block | 人工确认 |
| `manual_paused` | 否 | 用户显式暂停 | 用户显式恢复 |
| `unknown_rate_limited` | 否或短冷却 | 未知 429 | 短 cooldown，不能直接判定 exhausted |

模型级 cooldown 优先写入 `model_cooldowns`，不要把单模型耗尽误判为账号全局耗尽。只有账号级 quota/billing/credit 明确信号才进入账号级 exhausted。

## Codex-facing 不可用响应语义

`pool_unavailable` 不是所有客户端都应收到同一种 transport 响应。

- 普通 HTTP 客户端可以收到本地 `503/pool_unavailable` JSON error 和可解释 `Retry-After`。
- Codex-facing `/v1/responses` 不能直接暴露 transport `503/pool_unavailable`，也不能把 upstream 429 包装成 retry-limit 终止；全池不可用时不得返回 `response.failed` 或静默断开。等待只能是短等待且必须发生在本次请求预算内；超出短等待或预算必须返回 `200` completed Responses SSE/JSON，streaming 序列必须完整闭合到 `response.completed` + `[DONE]`。
- Stream 请求遇到全池不可用时，`pool_wait` 必须可观测且最终闭合：短等待后恢复、或 `final_response` / `streamState=completed` / `outcome=in_band_local_completion` / `errorType=pool_unavailable` 显式闭合。parked pool_wait、SSE idle、heartbeat-only open wait、`response.failed` 和旧 `outcome=in_band_synthetic` 都是连续性回归。
- Bounded backoff 只约束普通 HTTP 和内联账号重试；Codex-facing 的本地全池不可用只能短等待 health/cooldown 恢复并转发真实上游，超出短等待或预算必须用本地 completed Responses 闭合本轮。

## 路线图

### Phase S1 - 调度合同收口

目标：把已经实现的调度边界整理成一个稳定合同，避免后续 UI 或脚本误读。

任务：

- [ ] 在 `docs/LOCAL_HARDENED_API.md` 增加本专项入口和默认策略摘要。
- [ ] 在 smoke report 中显式输出 `candidate_pool_count`、`effective_max_retry_accounts`、`attempted_account_count`、`fallback_blocked_reason`。
- [ ] 给 `pool_unavailable` 的 report 增加 `nearest_retry_after_ms` 和 `blocking_status_counts`。
- [ ] 在 Codex-facing `/v1/responses` 验收中区分 `transport_pool_unavailable`、`local_completion_pool_unavailable`、`legacy_synthetic_pool_unavailable`、`failed_pool_unavailable`、`heartbeat_pool_wait` 和 `parked_pool_wait_timeout`。
- [ ] 保持 `single -> small_pool -> fallback_probe -> app-safe continuity` staged rollout 不变。

验收：

- [ ] 读者能从文档直接判断默认是否会随机轮换账号。
- [ ] report 能解释“为什么没有切号”或“为什么切到下一个账号”。
- [ ] Codex-facing 全池不可用不出现 transport `503/pool_unavailable`、`response.failed`、旧 `outcome=in_band_synthetic`、heartbeat-only open wait 或 parked SSE idle timeout；短等待内可恢复时继续转发真实上游，超出短等待或预算时必须出现本地 completed Responses SSE/JSON。
- [ ] `git diff --check` 通过。

### Phase S2 - Selector 可解释性增强

目标：让每次选择账号都有可审计理由，而不是只看到最终 account hash。

任务：

- [ ] 在 selector audit 中记录脱敏 candidate 摘要：`candidate_count`、`eligible_count`、`skipped_counts_by_reason`、`selected_reason`。
- [ ] 对 process sticky 命中、sticky 失效、fill-first 命中、previous_response affinity 命中分别给出 `selected_reason`。
- [ ] 对 `maxRetryAccounts` cap 截断给出 `cap_applied=true` 和 cap 数值。
- [ ] 不记录完整账号 ID、邮箱、API key、token 或 raw upstream body。

验收：

- [ ] audit 里能分辨 `sticky_selected`、`sticky_cleared`、`fill_first_selected`、`previous_response_affinity_selected`、`health_skipped`。
- [ ] 500+ fake account 单测仍保持毫秒级 selector 路径，不触发 quota/account snapshot refresh。
- [ ] `cargo test --manifest-path .\src-tauri\Cargo.toml --target-dir .\target hardened_routing --quiet` 通过。

### Phase S3 - 风控降噪增强

目标：继续降低 live upstream 探测和后台刷新带来的风险。

任务：

- [ ] 把 live upstream probe、quota refresh、drain、cooldown recovery probe 全部集中到同一 live-risk guard 分类。
- [ ] 默认 cooldown recovery 只读 health registry/reset time，不主动 poll。
- [ ] 对超过 2 次 quota refresh、超过默认 drain 请求量或低于 20 秒 drain 间隔的验收，继续要求 `-AcknowledgeExpandedLiveUpstreamRisk`。
- [ ] UI 中把 `balanced_low_rate`、`quota_drain_careful`、任何 `maxRetryAccounts > 2` 明确标为手动 opt-in。

验收：

- [ ] `npm run release:preflight` 能阻断缺少 live-risk acknowledgement 的示例或脚本路径。
- [ ] 文档里没有鼓励扫号、规避识别或高频探测恢复的口径。
- [ ] `scripts/test-local-hardened-api-live-risk-guard.ps1` 通过。

### Phase S4 - 状态面板与人工恢复闭环

目标：让用户不看日志也能判断号池为什么不可用、哪个动作是低风险恢复。

任务：

- [ ] 在 API 服务面板增加最近脱敏 request/audit 摘要列表。
- [ ] 展示 `healthy/cooling/quota_exhausted/auth_suspect/manual_required/manual_paused/model_cooldown` 计数。
- [ ] 展示 nearest cooldown/reset，不展示完整邮箱或账号 ID。
- [ ] 增加单账号暂停/恢复的显式用户动作；恢复只改本地 health，不刷新额度、不打上游。
- [ ] `pool_unavailable` UI 文案区分“全部冷却”、“全部额度耗尽”、“需要人工确认”、“没有配置账号”。

验收：

- [ ] 手动恢复写入脱敏 audit event。
- [ ] 恢复动作不发起上游请求。
- [ ] UI typecheck 通过：`npm run typecheck`。

### Phase S4A - 账号卡片与分组推荐排序

目标：让卡片主页、Codex 分组和 API 服务号池对“曾用账号重置后优先、新账号 reserve、同 IP 少切号”给出一致排序。

任务：

- [ ] 在 API service health registry 或派生 read model 中暴露 `api_service_used_before`、`last_success_at`、`last_selected_at`、`last_quota_exhausted_at`、`last_weekly_reset_seen_at` 的脱敏状态。
- [ ] 新增 `api_service_recommended` 排序 score，只作为推荐排序，不覆盖用户手动号池顺序。
- [ ] 调整 Codex `recommended` 卡片排序：API 服务成员内部按 scheduling bucket 展示；新账号 reserve 不因 100% 周额度压过已恢复老账号。
- [ ] Codex 分组内推荐排序复用同一 comparator；普通分组手动排序保持原样。
- [ ] 刷新队列使用单独 refresh priority，不复用调度排序。

验收：

- [ ] 两个周额度相同账号中，曾用且 reset 后恢复的账号排在新账号前面。
- [ ] sticky/current healthy 账号不因另一个账号周额度更高而被挤下调度首位。
- [ ] 冷却未到期账号只显示在“等待恢复”段，不进入调度候选。
- [ ] 用户手动 API 服务号池顺序不会被卡片推荐排序隐式改写。
- [ ] `npm run typecheck` 通过；排序纯函数有 focused unit test 或最小 comparator test。

### Phase S5 - 任务级连续性验收固化

目标：把用户关心的 bug oracle 固化为可重复验收：一个账号 429 不应让当前 Codex 任务直接以 `exceeded retry limit, last status: 429 Too Many Requests` 结束。

任务：

- [ ] 保持 `scripts/accept-local-hardened-api-continuity.ps1` 作为最高价值入口。
- [ ] 在 acceptance summary 中同时输出两半结论：当前请求是否 `429 -> cooldown -> fallback -> 200`，新请求是否避开 exhausted/cooldown 账号。
- [ ] 继续要求 `-AppSafeIsolatedProbe` 和 `-AssertCodexAppProcessStable` 覆盖 App 不断线场景。
- [ ] 对没有真实 429 的 run 标记 `blocked`，不能当作 quota exhaustion continuity 通过。

验收：

- [ ] `quota_fallback_audit_contract` pass 时必须出现 `usage_limit_reached`、`model_cooldown_applied`、`fallback_selected` 和后续 `200`。
- [ ] `codex_exec_task_e2e` pass 时使用临时 `CODEX_HOME`，不修改 live Codex config/auth。
- [ ] report 明确记录 live config/auth hashes untouched。

## 实施任务清单

| ID | 任务 | 主要文件 | 验证 | 风险 |
| --- | --- | --- | --- | --- |
| APS-01 | 新增本专项文档并挂入口 | `docs/LOCAL_HARDENED_API_ACCOUNT_POOL_SCHEDULING_PLAN.md`、`docs/LOCAL_HARDENED_API.md`、`docs/LOCAL_HARDENED_API_ROADMAP.md` | `git diff --check` | 低 |
| APS-02 | smoke/report 增加调度摘要字段 | `scripts/smoke-local-hardened-api.ps1`、`scripts/accept-local-hardened-api-continuity.ps1` | focused script test + `git diff --check` | 低 |
| APS-03 | selector audit 增加候选与跳过原因 | `src-tauri/src/modules/codex_local_access.rs` | `cargo test ... hardened_routing --quiet` | 中 |
| APS-04 | live-risk guard 覆盖所有上游探针入口 | `scripts/test-local-hardened-api-live-risk-guard.ps1`、`scripts/release/preflight.cjs`、相关 docs | `npm run release:preflight` | 中 |
| APS-05 | API 服务面板增加最近脱敏 audit 摘要 | `src/components/CodexLocalAccessModal.tsx`、`src/types/codexLocalAccess.ts`、CSS | `npm run typecheck`，UI smoke 需确认 live command | 中 |
| APS-06 | Codex-facing `pool_unavailable` heartbeat/failed/synthetic 验收 | `src-tauri/src/modules/codex_local_access.rs`、`scripts/monitor-live-codex-app-cockpit-acceptance.ps1` | focused Rust tests + monitor script tests | 中 |
| APS-07 | 手动暂停/恢复闭环 | `src-tauri/src/modules/codex_local_access.rs`、`src/components/CodexLocalAccessModal.tsx` | health/manual recovery tests + typecheck | 中 |
| APS-08 | 任务级连续性 summary 双结论 | `scripts/accept-local-hardened-api-continuity.ps1`、`scripts/monitor-live-codex-app-cockpit-acceptance.ps1` | app-safe isolated dry/static tests；live upstream 需 acknowledgement | 中 |
| APS-09 | 账号卡片/分组/API 服务推荐排序合同 | `src/pages/CodexAccountsPage.tsx`、`src/types/codexLocalAccess.ts`、`src-tauri/src/modules/codex_local_access.rs` | comparator tests + `npm run typecheck` | 中 |
| APS-10 | 低频刷新排序与 live-risk guard 联动 | `src/hooks/useAutoRefresh.ts`、`scripts/test-local-hardened-api-live-risk-guard.ps1`、相关 docs | focused refresh tests + preflight | 中 |

## 验收矩阵

默认低风险验证：

```powershell
git diff --check
cargo test --manifest-path .\src-tauri\Cargo.toml --target-dir .\target hardened_routing --quiet
cargo test --manifest-path .\src-tauri\Cargo.toml --target-dir .\target health_registry --quiet
npm run typecheck
```

默认 smoke，不打上游：

```powershell
.\scripts\smoke-local-hardened-api.ps1 -Stage single -StartEphemeralGateway -WriteReport
.\scripts\smoke-local-hardened-api.ps1 -Stage small_pool -StartEphemeralGateway -WriteReport
```

真实 fallback 验收，需要显式 live-risk：

```powershell
.\scripts\accept-local-hardened-api-continuity.ps1 `
  -Model gpt-5.5 `
  -AcknowledgeLiveUpstreamRisk `
  -SkipEphemeralGatewayBuild
```

如果当前 Codex App 必须不断线，继续使用 App-safe isolated path；不要改 live CLI provider，不要重启/停止/kill Codex App 或 `codex` 进程。

## 回滚入口

- 文档层：删除本专项入口或回退 `APS-01`。
- 调度层：恢复 `maximum_safety` preset，保持单并发、低频、sticky/fill-first。
- 状态层：通过 UI 或 `codex_local_access_recover_health` 清理指定账号/模型 cooldown；不得直接删除账号凭据。
- 验收层：live-risk run 出现异常时，停止新增上游探针，保留 report，回到 static/offline gates。
- 运行层：不替换 release exe、不重启 Cockpit/Codex App，除非当前任务明确确认。

## 不做事项

- 不把多个 API key 解释成多个独立官方限流池。
- 不通过多账号随机轮换绕过 organization/project/workspace/model 级限制。
- 不因为新账号显示 100% 周额度就让它默认压过曾用且已恢复的老账号。
- 不主动批量 refresh OAuth/free accounts。
- 不为了找一个 429 账号而扫完整号池。
- 不高频 drain、不降低 20 秒以下间隔，除非显式 expanded live-risk acknowledgement。
- 不记录 prompt、response、Authorization、cookie、完整邮箱、完整 API key 或 raw upstream body。
- 不把 LiteLLM/Sub2API/New API/CLIProxyAPI 作为 Cockpit 自用版强依赖。
