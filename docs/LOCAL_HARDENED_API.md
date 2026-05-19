# Cockpit Local Hardened API

本页是自用版 Cockpit API service 的执行说明。它只描述本机低风险用法；代码、运行事实和 `docs/LOCAL_HARDENED_API_IMPLEMENTATION_PLAN.md` 优先级更高。

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
| `maxRetryAccounts` | `1` | 默认不在同一请求内扫账号池 |
| `fallbackMode` | `disabled` | 下一个请求才重新选择账号 |
| `logging.includePromptResponse` | `false` | 不记录正文 |
| `logging.includeRawUpstreamBody` | `false` | 不记录上游原始 body |

## Preset 契约

### `maximum_safety`

用途：默认自用、最低风控暴露。

- 单账号或单 sticky 账号。
- `maxConcurrentRequests = 1`
- `minRequestIntervalSeconds >= 60` 可用于更保守场景；当前代码默认 20 秒。
- `maxRetryAccounts = 1`
- `fallbackMode = disabled`
- 自动配额刷新关闭或不低于 60 分钟。
- 唤醒/keepalive 默认关闭。

### `balanced_self_use`

用途：自用多账号池，但仍避免随机扫射。

- `maxConcurrentRequests = 1`
- `minRequestIntervalSeconds = 20..30`
- sticky/fill-first 优先；当前账号健康时不轮换。
- `maxRetryAccounts = 1`，可手动提升到 `2` 但必须保留 stream guard。
- 账号进入 cooldown/manual/auth 状态后排到后面。
- 自动刷新不扫描 API service OAuth 池。

### `quota_drain_careful`

用途：明确希望先消耗某些账号额度，但保持低速率。

- 用户排序优先，fill-first。
- 严格遵守 persistent health registry。
- `maxConcurrentRequests = 1`
- `minRequestIntervalSeconds >= 30`
- `fallbackMode = next_request_only`
- 只在下一请求边界选择下一个 healthy 账号。

## Preset 恢复入口

API 服务面板的“策略预设”按钮会调用 `codex_local_access_apply_safety_preset`，把当前集合恢复到对应 safety config，并重置为 hardened fill-first 起点。Preset 不会改账号池成员、端口、API key 或运行模式。

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

若上一轮已把该账号/模型写入 cooldown，后续同模型 smoke 应直接返回本地 429 和 `Retry-After`，不再继续打上游。

## 额度耗尽后的请求边界

Hardened API Mode 的目标是接近 Direct OAuth 的稳定体验，但不伪造上游 quota grace：

- 已被上游接纳的当前 stream/response 应继续 pipe 到完成、上游 terminal error、客户端断开或 transport fatal error。
- 本地 cooldown、exhausted、health registry 或 `selection_eligible=false` 只影响新的 admission，不 retroactively cancel active stream。
- 新的独立请求不需要等待其他 active stream 结束；调度器可以立即避开 cooldown/exhausted 账号，选择健康账号。
- 带 `previous_response_id` 的 continuation 优先粘原账号；不能把原账号的 `previous_response_id` 直接发给新账号。
- 如果原账号在 continuation admission 阶段真实返回 429，只能 bounded backoff、返回 429，或在有完整上下文/压缩上下文重放时把它作为新 admission 交给健康账号。

单号池通过后，再放入 2-3 个账号，但第一步仍保持 `maxRetryAccounts = 1`，只验证 selector/sticky/health 不乱轮换：

```powershell
.\scripts\smoke-local-hardened-api.ps1 -Stage small_pool -WriteReport
```

只有小池稳定后，才临时切到 `fallbackMode = next_request_only` 且 `maxRetryAccounts = 2`，做有限 fallback 探针：

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
  -AutoPopulateProbeAccountPool `
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

推荐直接使用一键验收入口，它会自动传入 App-safe、临时 fallback、自动号池、上游 smoke、nested
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
  -AutoPopulateProbeAccountPool `
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

`-AutoPopulateProbeAccountPool` 只在 `-AppSafeIsolatedProbe` 下可用。它会从现有 Cockpit
`codex_accounts.json` 和账号详情目录中扫描账号。选择顺序是先检查当前 API service 号池中已有的
账号；若不能凑齐验收所需的 `exhausted + available`，再利用账号详情里的 cached quota 按验收优先级
预排序全库候选，并只对候选 OAuth 账号即时请求 `wham/usage` 刷新配额判定。普通 API key 账号、
非 free 账号、需要重新认证或已禁用账号都不会进入测试号池。
free 账号按当前上游语义只接受 weekly-only quota：`primary_window.limit_window_seconds = 604800`
且 `secondary_window = null`，不把它误判成 5h quota。

为避免验收脚本为了找账号而扫大号池，任何真实上游请求、`wham/usage` 配额刷新、自动号池或 drain
都必须显式传入 `-AcknowledgeLiveUpstreamRisk`，否则脚本返回 `blocked`，不会访问上游。
`-AutoPopulateProbeAccountPool` 默认最多只做 2 次真实 `wham/usage` 刷新；找不到满足条件的账号就阻断，
不继续刷新更多账号。只有在明确接受扩大扫描风险时，才手动提高
`-AutoPopulateProbeMaxRefreshAttempts`；超过 2 次刷新、超过默认 drain 请求量，或把 drain 间隔降到
20 秒以下，还必须同时传入 `-AcknowledgeExpandedLiveUpstreamRisk`。一键验收入口对应参数是
`-MaxProbeQuotaRefreshAttempts`。

自动号池会强制写入恰好 2 个账号到临时 `codex_local_access.json`：第一个必须是 refreshed 后
`exhausted` 的 free OAuth weekly 账号，第二个必须是 refreshed 后仍 `available` 的 free OAuth
weekly 账号；多余账号会被排除。报告只记录 `selectedAccountHashes`、角色和 weekly 百分比，不记录
原始 accountId 或 token；不会写 live 号池，也不会改当前 Codex CLI/App 的 `config.toml` / `auth.json`。

该命令适合验证“第一个账号真实 429 后，同一请求是否选择下一个账号并返回 200”。如果刷新后找不到
exhausted + available 这一对 free weekly OAuth 账号，脚本会阻断，不能当作额度耗尽不中断验收通过。
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

该模式仍只接受 OAuth free weekly 账号；第 1 个账号可以是 `available`，第 2 个账号必须是
`available`。脚本会通过隔离 gateway 低频发送小请求消耗第 1 个账号，直到 audit 证明
`429 usage_limit_reached -> fallback_selected -> 200`，或达到 `-DrainMaxRequests` 后阻断停止。
该模式默认关闭，且不会用于普通验收。

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

若直连 Cockpit 正常、LiteLLM 失败，应优先查 LiteLLM route/config；若 Cockpit 返回 429，先判断是当前账号 cooldown/额度耗尽还是网关故障。

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
