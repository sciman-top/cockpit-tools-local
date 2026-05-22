# Cockpit Local Hardened API

本页是自用版 Cockpit API service 的执行说明。它只描述本机低风险用法；代码、运行事实和 `docs/LOCAL_HARDENED_API_IMPLEMENTATION_PLAN.md` 优先级更高。

账号池调度专项见 `docs/LOCAL_HARDENED_API_ACCOUNT_POOL_SCHEDULING_PLAN.md`。该专项固定默认策略为 `sticky_process + fill_first + capped fallback`，并明确禁止随机扫号、高频 live probe 和风控规避型逻辑。

## 目标

- 只监听 `127.0.0.1`，不提供 LAN/public 入口。
- 单账号先跑通；多账号池只在 health registry、stream guard、backpressure 和 audit trail 均可用后启用。
- 当前请求一旦开始写出，后续不换账号；额度耗尽只影响下一个请求的选择。
- 429/401/5xx 只通过真实业务请求被动写入健康状态，不用高频刷新探测恢复。
- 日志和 UI 只展示结构化脱敏字段，不记录 prompt、response、token、cookie、Authorization header 或完整邮箱。

## 默认安全姿态

API service 的默认安全配置等价于 `balanced_self_use`；需要更保守时可一键恢复 `maximum_safety`：

| 字段 | 默认值 | 说明 |
| --- | --- | --- |
| `hardenedLocalMode` | `true` | 开启本机 hardened 行为 |
| `maxConcurrentRequests` | `1` | 同时最多一个上游请求 |
| `minRequestIntervalSeconds` | `20` | 请求启动间隔不低于 20 秒 |
| `maxQueueWaitSeconds` | `21` | 本地排队等待上限，至少覆盖启动间隔并留 1 秒余量 |
| `requestTimeoutSeconds` | `600` | 长任务允许继续写出 |
| `maxRetries` | `1` | 单账号有限 retry |
| `maxRetryAccounts` | `2` | 默认允许 failover-safe 429 在同一请求内无感切到下一个健康账号 |
| `fallbackMode` | `disabled` | 下一个请求才重新选择账号 |
| `logging.includePromptResponse` | `false` | 不记录正文 |
| `logging.includeRawUpstreamBody` | `false` | 不记录上游原始 body |

## Preset 契约

### `maximum_safety`

用途：默认自用、最低风控暴露。

- 单账号或单 sticky 账号。
- `maxConcurrentRequests = 1`
- `minRequestIntervalSeconds >= 60` 可用于更保守场景；当前代码默认 20 秒。
- `maxRetryAccounts = 2`（单账号池实际仍只尝试 1 个账号）
- `fallbackMode = disabled`
- 自动配额刷新关闭或不低于 60 分钟。
- 唤醒/keepalive 默认关闭。

### `balanced_self_use`

用途：自用多账号池，但仍避免随机扫射。

- `maxConcurrentRequests = 1`
- `minRequestIntervalSeconds = 20..30`
- sticky/fill-first 优先；当前账号健康时不轮换。
- `maxRetryAccounts = 2` 起步；可手动提升到 `3+`，但必须保留 stream guard。
- 账号进入 cooldown/manual/auth 状态后排到后面。
- 自动刷新不扫描 API service OAuth 池。

### `quota_drain_careful`

用途：明确希望先消耗某些账号额度，但保持低速率。

- 用户排序优先，fill-first。
- 严格遵守 persistent health registry。
- `maxConcurrentRequests = 1`
- `minRequestIntervalSeconds >= 30`
- `fallbackMode = next_request_only`
- `maxRetryAccounts = 2`
- 真实 quota/cooldown 429 且尚未向客户端写出 stream 时，同一次请求可以重投递到下一个 healthy 账号；后续独立请求也必须避开 exhausted/cooldown 账号。

## Preset 恢复入口

API 服务面板的“策略预设”按钮会调用 `codex_local_access_apply_safety_preset`，把当前集合恢复到对应 safety config，并重置为 hardened fill-first 起点。Preset 不会改账号池成员、端口、API key 或运行模式。

## 账号池成员与切换

- “添加至 API 服务”保存的账号列表是 API 服务号池的配置真相；`effectiveAccountIds` 应与该列表一致。
- 多账号池的高效调度目标是减少失败重试、避免 429 连撞并保持任务连续性，不是提高并发或把每个请求随机分配给不同账号。
- 同一出口 IP 下尽量少切换不同账号；曾经被 API service 成功使用过、且一周重置后恢复的账号，应优先于从未使用过的新账号。新账号作为 reserve 展示和调度，不因为显示 100% 周额度就默认排到最前。
- 健康状态、cooldown、exhausted、manual-required 只影响运行时是否可调度，不应把账号从配置号池或 UI 有效号池里隐式移除。
- API 服务面板和 smoke report 默认展示当前配置号池作用域的 health summary；历史 `codex_local_access_health.json` 仍保留旧账号记录，但只能作为 `healthRegistry` 历史证据，不能污染当前号池健康判断。
- 在账号卡片或分组内点击普通“切换”只切换当前 Codex 账号，不清空、不替换 API 服务号池；只有显式开启“跟随当前账号”时才同步号池，且同步方式是把当前账号置前并保留原成员。
- 在账号卡片主页点击“API 服务”卡片本体进入服务面板；卡片内“添加账号”按钮进入号池成员选择。
- 当号池成员全部不可调度时，普通 HTTP 客户端会收到本地 `503/pool_unavailable` JSON error 和可解释 `Retry-After`；卡片根据 health summary 显示“额度均已耗尽”或“暂无可调度账号”的原因摘要，不能伪装成 upstream `429/rate_limited`。
- Codex-facing `/v1/responses` 是例外：`429 retry-limit`、静默 SSE、transport `503/pool_unavailable`、`response.failed`、heartbeat-only open wait 都会破坏当前 Codex turn。全池不可调度时，只允许短等待恢复，且等待必须落在本次请求超时预算内；若不能在短等待内恢复，必须返回 `200` completed Responses 形态：streaming 使用完整 `response.created -> ... -> response.completed -> [DONE]`，non-stream 使用 `status=completed` JSON，并在 assistant text 中说明 `Cockpit API Service pool_unavailable`。该本地响应不打上游、不释放无关 active stream。
- monitor 必须把 transport 503、`response.failed`、旧 `outcome=in_band_synthetic`、heartbeat/open/parked pool_wait、SSE idle，以及 `stream disconnected before completion: Cockpit API Service pool_unavailable` 标成连续性回归；`streamState=completed` / `outcome=in_band_local_completion` 是 Codex-facing 全池耗尽时的本地闭合证据。
- Stream 请求遇到全池不可用时不得静默 park，也不得持续 heartbeat 或静默等待长 cooldown；audit 应记录短等待 `pool_wait` 后恢复，或 `final_response` / `streamState=completed` / `outcome=in_band_local_completion` / `errorType=pool_unavailable`。

## 单号池实跑优先级

功能面可以先支持多号池、sticky/fill-first 和有限 fallback；运行面必须分阶段放量。先用单账号池证明 API service 全链路确实按 hardened 预期工作：

```powershell
.\scripts\smoke-local-hardened-api.ps1 -Stage single -WriteReport
```

默认 smoke 只验证本机 loopback、API key guard、`/v1/models` 和本地 health/audit 文件摘要，不调用真实上游、不改 Codex provider、不保存 API key。只有在 API service 已启用、账号池恰好 1 个账号、且明确接受一次真实请求时，才运行：

若桌面端当前没有启用 API service，且只想验证 gateway 代码路径而不切换 live Codex provider，可使用短生命周期 runner：

```powershell
.\scripts\smoke-local-hardened-api.ps1 -Stage single -StartEphemeralGateway -WriteReport
```

```powershell
.\scripts\smoke-local-hardened-api.ps1 -Stage single -AcknowledgeLiveUpstreamRisk -RunUpstreamSmoke -WriteReport
```

当前上游 429 链路 smoke 默认使用 `gpt-5.4`；若要把 429 视为预期结果，加入 `-Expect429`：

```powershell
.\scripts\smoke-local-hardened-api.ps1 -Stage single -StartEphemeralGateway -AcknowledgeLiveUpstreamRisk -RunUpstreamSmoke -Expect429 -WriteReport
```

若上一轮已把该账号/模型写入 cooldown，后续同模型 smoke 不应继续打上游。普通 HTTP 客户端应收到本地 `503/pool_unavailable` 和 `Retry-After`；Codex-facing `/v1/responses` 若无法在本次请求预算内恢复，应返回本地 completed Responses SSE/JSON，避免 503、`response.failed` 或静默挂起。

## 额度耗尽后的请求边界

Hardened API Mode 的目标是接近 Direct OAuth 的稳定体验，但不伪造上游 quota grace：

- 已被上游接纳的当前 stream/response 应继续 pipe 到完成、上游 terminal error、客户端断开或 transport fatal error。
- 本地 cooldown、exhausted、health registry 或 `selection_eligible=false` 只影响新的 admission，不 retroactively cancel active stream。
- 新的独立请求在仍有健康账号时不需要等待其他 active stream 结束；调度器可以立即避开 cooldown/exhausted 账号，选择健康账号。若全池都不可调度，Codex-facing 新请求只能短等待恢复且必须落在本次请求预算内；超出短等待或预算必须以本地 completed Responses 闭合，不能无限保活，也不能发 `response.failed`。
- 带 `previous_response_id` 的 continuation 优先粘原账号；不能把原账号的 `previous_response_id` 直接发给新账号。
- 如果原账号在 continuation admission 阶段真实返回 429，只能 bounded backoff、对普通 HTTP 返回本地 `503/pool_unavailable`、对 Codex-facing `/v1/responses` 短等待恢复后重试，或用本地 completed Responses 明确闭合；只有具备完整上下文/压缩上下文重放时，才能把它作为新 admission 交给健康账号。
- Bounded backoff 只约束普通 HTTP 和内联账号重试；Codex-facing streaming 的本地全池不可用不得用 retry-limit、transport 503 或 heartbeat-only open wait 表达，且恢复等待同时受短等待上限和本次请求超时预算约束。

单号池通过后，再放入 2-3 个账号，保持 `maxRetryAccounts >= 2`，验证 selector/sticky/health 不乱轮换且 failover-safe 429 可在同一请求切到下一个健康账号：

```powershell
.\scripts\smoke-local-hardened-api.ps1 -Stage small_pool -WriteReport
```

只有小池稳定后，才在 `maxRetryAccounts >= 2` 下做有限 fallback 探针；`fallbackMode` 可保持默认 `disabled`，它不阻断当前请求内的 failover-safe 429 切号：

```powershell
.\scripts\smoke-local-hardened-api.ps1 -Stage fallback_probe -AcknowledgeLiveUpstreamRisk -RunUpstreamSmoke -WriteReport
```

若 Codex CLI 正在使用 Direct API/OAuth 并且会话不能断线，必须使用旁路探针入口。该入口只临时改
`~/.antigravity_cockpit/codex_local_access.json`，且在结束后恢复；不会读取或写入当前 CLI 使用的
`~/.codex/config.toml` / `~/.codex/auth.json` 内容，只记录文件 hash 以证明未被触碰：

```powershell
.\scripts\smoke-local-hardened-api.ps1 `
  -Stage fallback_probe `
  -StartEphemeralGateway `
  -TemporaryFallbackConfig `
  -AcknowledgeLiveUpstreamRisk `
  -RunUpstreamSmoke `
  -AssertCodexCliConfigUntouched `
  -WriteReport
```

若 Codex App 也必须不断线，使用 App-safe isolated probe。该模式会把 API service 配置复制到临时
data root，临时 gateway 只读写该目录下的 `codex_local_access*.json/jsonl`，并让端口重新分配，避免
改动 live Cockpit API service 配置或抢占 App 正在使用的端口：

```powershell
.\scripts\smoke-local-hardened-api.ps1 `
  -Stage fallback_probe `
  -StartEphemeralGateway `
  -TemporaryFallbackConfig `
  -AppSafeIsolatedProbe `
  -AcknowledgeLiveUpstreamRisk `
  -RunUpstreamSmoke `
  -RequireQuotaFallback `
  -AssertCodexCliConfigUntouched `
  -AssertCodexAppProcessStable `
  -WriteReport
```

若验收目标是“Codex 任务本身在账号额度耗尽后不中断”，使用任务级 E2E。该模式会额外启动一个
临时 `CODEX_HOME` 的 `codex exec --ephemeral`，让它通过隔离 gateway 执行一个真实小型编码任务；
当前 Codex App 和当前 CLI 会话仍不参与本次任务流量：

推荐直接使用一键验收入口。它会自动传入 App-safe、临时 fallback、上游 smoke、nested
`codex exec`、CLI/App 守卫和 report 参数，并输出简短 JSON 摘要：

```powershell
.\scripts\accept-local-hardened-api-continuity.ps1 -Model gpt-5.5 -AcknowledgeLiveUpstreamRisk -SkipEphemeralGatewayBuild
```

若需要手动展开底层 smoke 参数，可使用等价命令：

```powershell
.\scripts\smoke-local-hardened-api.ps1 `
  -Stage fallback_probe `
  -Model gpt-5.5 `
 -StartEphemeralGateway `
 -TemporaryFallbackConfig `
 -AppSafeIsolatedProbe `
 -AcknowledgeLiveUpstreamRisk `
  -RunCodexExecSmoke `
  -RequireQuotaFallback `
  -AssertCodexCliConfigUntouched `
  -AssertCodexAppProcessStable `
  -WriteReport
```

任务级 E2E 的验收看 `codex_exec_task_e2e` 是否 `pass`，并结合 `quota_fallback_audit_contract`
是否 `pass` 判断是否真的覆盖了额度耗尽后的 failover。传入 `-RequireQuotaFallback` 后，脚本要求
audit 中同时出现 `429 usage_limit_reached`、`model_cooldown_applied`、`fallback_selected` 和
后续 `200`；如果第一个账号没有返回 429，报告会标为 `blocked`，不能当作额度耗尽不中断验收通过。

该 smoke 会在临时 `CODEX_HOME` 和临时 workspace 内对 nested `codex exec` 使用
`--dangerously-bypass-approvals-and-sandbox`。这是为了规避 Codex CLI 0.131 在子进程
`workspace-write` probe 中把写入判为 read-only；报告会记录
`sandboxBypassForIsolatedWorkspace = true`。不要把该 bypass 用到 live CLI/App 会话。

fallback continuity 验收只使用当前 API service 号池中已经手动添加的账号。脚本不会扫描
`codex_accounts.json`、不会自动挑选账号、不会刷新 `wham/usage`，也不会把账号写入临时号池。
运行前请在 Cockpit API 服务号池中手动放入 2 到 3 个账号；如果号池为空或少于 2 个账号，
`fallback_probe` 会返回 `blocked`，提示先添加账号后再运行验收。

为避免验收脚本为了找账号而扫大号池，任何真实上游请求、配额刷新或 drain
都必须显式传入 `-AcknowledgeLiveUpstreamRisk`，否则脚本返回 `blocked`，不会访问上游。
超过默认 drain 请求量，或把 drain 间隔降到 20 秒以下，还必须同时传入
`-AcknowledgeExpandedLiveUpstreamRisk`。

该命令适合验证“手动号池中的某个账号真实 429 后，同一请求是否选择下一个健康账号并返回 200”。
如果手动号池无法产生 `429 -> fallback -> 200` 审计链，脚本会阻断，不能当作额度耗尽不中断验收通过。
不要用它替代 release binary 部署验证；
替换 release exe、重启 Cockpit/Codex App、kill `codex` 或改当前 CLI provider 仍需要单独确认。

如果当前没有已耗尽的 free 账号，但接受为了验收消耗第 1 个 free 账号，可显式启用消耗型验收：

```powershell
.\scripts\accept-local-hardened-api-continuity.ps1 `
  -Model gpt-5.5 `
  -AcknowledgeLiveUpstreamRisk `
  -SkipEphemeralGatewayBuild `
  -DrainFirstFreeAccountUntilFallback `
  -DrainMaxRequests 30 `
  -DrainRequestIntervalSeconds 22
```

该模式使用当前手动号池。脚本会通过隔离 gateway 低频发送小请求，直到 audit 证明
`429 usage_limit_reached -> fallback_selected -> 200`，或达到 `-DrainMaxRequests` 后阻断停止。
该模式默认关闭，且不会用于普通验收。

## Live Codex App 手动实跑旁路监测

若当前 Codex CLI 会话必须保持 Direct API/OAuth，不允许本会话改 `~/.codex/config.toml` /
`~/.codex/auth.json`，但需要人工把 Codex App 切到 Cockpit API service 后跑一段真实编码任务，可在
CLI 会话中启动只读监测入口：

```powershell
.\scripts\monitor-live-codex-app-cockpit-acceptance.ps1 `
  -DurationSeconds 900 `
  -RequireQuotaFallback `
  -RequireStreamCompletion `
  -RequireCliConfigUntouched `
  -RequireAppStable `
  -WriteReport
```

`-StopWhenSatisfied` 只适合单次快速验收，观察到第一条完整链路后会提前退出。若要连续观察后续每一次请求、
多次账号切换、以及前序 stream 是否继续完成，不要加 `-StopWhenSatisfied`，并按验收目标提高计数：

```powershell
.\scripts\monitor-live-codex-app-cockpit-acceptance.ps1 `
  -DurationSeconds 1800 `
  -RequireQuotaFallback `
  -RequireStreamCompletion `
  -RequireCliConfigUntouched `
  -RequireAppStable `
  -RequiredFallbackCycles 3 `
  -RequiredDistinctHealthyAccounts 3 `
  -RequiredCompletedStreams 3 `
  -WriteReport
```

该脚本只读取 live `codex_local_access_audit.jsonl`、记录 Codex App 进程集合，并对
`~/.codex/config.toml` / `~/.codex/auth.json` 取 hash 证明当前 CLI 配置是否变化；不会启动、停止、
重启或 kill Codex App / Codex CLI / Cockpit service，也不会切 provider、刷新 quota 或消耗上游额度。
它用于回答人工实跑期间是否观察到：

- `429 usage_limit_reached -> model_cooldown_applied -> fallback_selected -> 200`
- fallback 后由同一个 `requestId` 内的不同健康账号返回 `200`
- 已接纳 stream 完成，且没有在 stream 已开始后被本地 cooldown 中断
- 历史 `exceeded retry limit, last status: 429 Too Many Requests` 是否复现
- 本地号池耗尽是否被单独记录为 `pool_unavailable`，而不是 retry-limit regression
- Codex-facing `/v1/responses` 全池耗尽是否没有 transport `503/pool_unavailable`
- 是否出现旧 `200 + outcome=in_band_synthetic` 或 `response.failed`；二者都会让 Codex-facing 连续性退化
- 全池耗尽时是否出现本地 completed Responses SSE/JSON；若只有 heartbeat/open/parked `pool_wait`、`stream disconnected before completion: idle timeout waiting for SSE` 或 `stream disconnected before completion: Cockpit API Service pool_unavailable`，属于静默停滞/断线回归
- 当前 CLI `config.toml` / `auth.json` 是否保持不变，以及 Codex App 进程集合是否稳定

报告中的 `audit.fallbackTransitions`、`audit.streamSummaries` 和 `audit.accountSummaries` 用于复盘多账号切换：
每次 fallback 的 exhausted account hash、同请求后续 `200` 的 healthy account hash、每个 request 的 stream 状态、
以及每个 account hash 的 `200/429/cooldown/completed` 计数都会落入报告。

该 monitor 不创建临时 provider 配置，因此报告中的 `temporaryConfig.restored` 为 `not_applicable`。
如果需要由脚本创建并恢复临时配置，继续使用上面的 App-safe isolated acceptance 入口。

若要验证 429 链路，优先使用真实业务请求自然返回的 429；脚本只记录状态码、`Retry-After`、health registry 和 audit phase，不记录 prompt/response。

## Codex CLI 直连 Cockpit

在 Codex 配置中使用本机 API service：

```toml
model_provider = "cockpit_api_service"

[model_providers.cockpit_api_service]
name = "Cockpit API Service"
base_url = "http://127.0.0.1:<port>/v1"
wire_api = "responses"
env_key = "OPENAI_API_KEY"
```

`OPENAI_API_KEY` 使用 Cockpit API service 面板中的本机 key。端口以面板当前显示为准。

## 可选 LiteLLM 桥接

LiteLLM 只作为客户端兼容外壳，不持有 ChatGPT OAuth token。推荐链路：

```text
Codex CLI -> LiteLLM -> Cockpit API service -> ChatGPT/Codex upstream
```

当需要排错时，先验证 Cockpit 直连：

```powershell
codex exec --skip-git-repo-check --json "Reply with exactly OK"
```

若直连 Cockpit 正常、LiteLLM 失败，应优先查 LiteLLM route/config；若 Cockpit 返回 upstream 429，先判断是否已写入 cooldown 并触发 fallback；若普通 HTTP 返回本地 `503/pool_unavailable`，或 Codex-facing `/v1/responses` 返回本地 `completed/pool_unavailable`，先看当前号池是否还有可调度账号以及 health registry 最近 cooldown/reset 记录。若 Codex-facing 路径出现 `response.failed/pool_unavailable`，这是 fatal stream failure 回归。

## 风险边界

- 不把 API service 暴露到 `0.0.0.0`、LAN IP 或公网。
- 不把 OAuth token、refresh token、cookie 或 ChatGPT session 写入 LiteLLM。
- 不通过 quota reset wakeup 自动把刷新间隔调到高频。
- 不在 cooldown 期间用刷新任务反复探测账号是否恢复。
- 不在 stream 已开始写出后切换账号。

## 回滚

1. 在 Codex 设置中切回 Direct API/OAuth 模式。
2. 停用 Cockpit API service。
3. 若使用 LiteLLM，临时把 Codex `model_provider` 改回官方或直连 Cockpit。
4. 保留 `codex_local_access_health.json` 和 `codex_local_access_audit.jsonl` 作为诊断证据；不要上传其中任何本机私有路径或账号材料。
