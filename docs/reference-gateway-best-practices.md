# Reference Gateway Best Practices Review

审查时间：2026-05-17

审查目标：为 Cockpit Tools Local 的个人本机 API service / Hardened Local API Mode 提炼可借鉴实践。本文只作为设计参考，不把社区项目的策略直接等同于本仓应启用的默认行为。

## Source Snapshot

| Project | Local path | Revision | Role |
| --- | --- | --- | --- |
| New API | `D:\CODE\external\_reference_gateway_sources\new-api` | `5dd0d3b` | 渠道网关、渠道禁用、重试、限流 |
| Sub2API | `D:\CODE\external\_reference_gateway_sources\sub2api` | `f5bd25b` | 账号健康、调度、临时不可调度、粘性会话 |
| CLIProxyAPI | `D:\CODE\external\_reference_gateway_sources\CLIProxyAPI` | `26d13af` | CLI/OAuth 代理、凭据选择、模型冷却、流式重试边界 |
| LiteLLM | `D:\CODE\external\_reference_gateway_sources\litellm` | `cf9b5e4` | 通用 router、cooldown、pre-call rate checks、proxy limits |

## Executive Conclusions

1. `429` 必须拦截，但不应理解为“马上扫下一个账号”。社区成熟做法更接近：分类错误、读取 `Retry-After` 或 provider reset 字段、把当前账号/模型放入 cooldown，再由健康选择器决定是否可 fallback。
2. Hardened Local API Mode 的默认路由应是 `sticky_process` 或 `fill_first`，不是 request-level random / round-robin。多账号池可以支持排序和健康 fallback，但默认不做每请求轮询。
3. 当前任务能否“额度耗完仍继续”取决于上游连接是否已经建立、是否已经开始流式输出、以及服务端是否还能继续发送。网关只能保证本地不因自己的重试/切号策略中断；不能把上游已经返回的 `429` 变成继续执行。
4. 最值得借鉴的是 Sub2API 的账号状态机、CLIProxyAPI 的流式重试边界、LiteLLM 的 cooldown 决策矩阵、New API 的可配置重试和渠道禁用框架。
5. 最不宜照搬的是面向公网/多用户网关的激进默认值：请求级轮询、跨账号扫射、无限或多凭据重试、高频额度刷新、请求/响应正文日志、LAN/public listen。

## Project Findings

### New API

证据入口：

- `common/constants.go:148` 默认 `AutomaticDisableChannelEnabled = false`。
- `common/constants.go:153` 默认 `RetryTimes = 0`。
- `controller/relay.go:190` 和 `controller/relay.go:324` 实现 relay 重试循环与 `shouldRetry`。
- `service/channel.go:45` 实现 `ShouldDisableChannel`。
- `model/ability.go:106` 实现按 priority/weight 选择 channel。
- `middleware/rate-limit.go` 与 `middleware/model-rate-limit.go` 实现全局和模型粒度限流。
- `types/error.go:148` 与 `common/str.go:188` 提供错误脱敏，但 `controller/relay.go:357` 仍会直接记录 raw `err.Error()`。

可借鉴：

- 把“是否重试”“是否禁用/隔离 channel”做成可配置策略，不写死在请求处理里。
- selection 先按 priority，再按 weight，是一个可解释排序框架。
- 区分全局请求限制和模型请求限制，避免只靠上游 `429` 才发现超载。

不宜照搬：

- 对 Cockpit 本机自用，priority + weighted random 容易变成请求级轮换，应改成 sticky/fill-first。
- New API 默认 `RetryTimes = 0`、自动禁用默认关闭，说明它并不天然“自动拦截并切走所有 429”。
- raw error 日志风险较高，Cockpit hardened mode 应只记录结构化字段，不记录 prompt、response、token、header、cookie。

### Sub2API

证据入口：

- `backend/internal/service/account.go:44`、`:45`、`:48` 定义 `RateLimitedAt`、`RateLimitResetAt`、`TempUnschedulableUntil`。
- `backend/internal/service/account.go:107` 的 `IsSchedulable()` 将状态、过期、rate limit、临时不可调度、额度等统一成“可调度”判断。
- `backend/internal/repository/account_repo.go:1048` 的 `SetRateLimited()` 持久化 rate-limit reset 时间。
- `backend/internal/repository/account_repo.go:1065` 的 `SetModelRateLimit()` 持久化模型粒度 cooldown。
- `backend/internal/service/gateway_service.go:451` 的 `shouldClearStickySession()` 在账号不可调度或模型限流时清理粘性绑定。
- `backend/internal/service/gateway_service.go:1403` 的 `SelectAccountWithLoadAwareness()` 是健康、模型、额度、并发、粘性的综合选择器。
- `backend/internal/service/ratelimit_service.go:132` 的 `HandleUpstreamError()` 和 `:1571` 的 `tryTempUnschedulable()` 处理上游错误、401/403/429、关键字规则和临时不可调度。
- `backend/internal/util/logredact/redact.go:14`、`:50`、`:62`、`:86` 提供结构化和文本脱敏。

可借鉴：

- 把账号健康状态建模成一等对象：`rate_limit_reset_at`、`model_rate_limits`、`temp_unschedulable_until`、`overload_until`、`last_error_type`。
- 选择器只从 `IsSchedulable()` 为真的账号里挑选；429/auth/captcha/suspicious 不应只是日志事件，而应改变账号可调度性。
- 粘性绑定不是永久绑定；账号不可调度或对应模型冷却时必须自动失效。
- 401 对 OAuth 账号更适合“invalidate token cache + temp quarantine + 人工确认/后续刷新”，不宜直接删除账号。

不宜照搬：

- Sub2API 的 DB、Redis、outbox、scheduler snapshot 适合服务端平台，Cockpit 本机版不应引入同等重量。可用轻量本地状态文件或现有配置存储承接核心状态。
- 它的一些刷新/调度能力面向多用户服务，不应默认搬到“个人低频本机 API service”。

### CLIProxyAPI

证据入口：

- `config.example.yaml:106` 默认 `request-retry: 3`。
- `config.example.yaml:110` 默认 `max-retry-credentials: 0`，其语义是旧行为下尝试所有可用凭据。
- `config.example.yaml:141` 默认 `session-affinity: false`。
- `sdk/cliproxy/auth/selector.go:27`、`:36` 定义 `RoundRobinSelector` 与 `FillFirstSelector`。
- `sdk/cliproxy/auth/selector.go:47` 定义 `modelCooldownError`，`:105` 会返回 `Retry-After`。
- `sdk/cliproxy/auth/selector.go:371` 的 `isAuthBlockedForModel()` 统一判断 disabled、model cooldown、quota exceeded。
- `sdk/cliproxy/auth/selector.go:437` 和 `:484` 实现 session affinity 选择器。
- `sdk/api/handlers/handlers_stream_bootstrap_test.go:399` 验证流式响应一旦发出首字节就不再重试。
- `internal/runtime/executor/codex_executor.go:978` 解析 Codex `usage_limit_reached` 的 `resets_at` / `resets_in_seconds`。
- `internal/util/provider.go:187`、`:208` 和 `internal/runtime/executor/helps/logging_helpers.go:462` 提供 header/API key 脱敏。

可借鉴：

- `fill-first` 是个人自用更合适的“尽量用完当前账号，再转下一个”模式，比 round-robin 更低扰动。
- 模型冷却错误应带 `Retry-After`，让 Codex CLI/App 或调用方知道何时重试。
- 流式请求只允许在“还没有给客户端发送任何 payload”之前重试或切号；首字节之后必须保持当前上游，不跨账号续接。
- Codex/OpenAI 429 的 body 里若有 reset 信息，应优先用于 cooldown，而不是固定 30 分钟。

不宜照搬：

- 默认 `request-retry: 3`、`max-retry-credentials: 0`、`session-affinity: false` 对 Cockpit hardened mode 偏激进。
- request logging 能力即使有脱敏，也不应在本机 hardened mode 默认开启 body capture。

### LiteLLM

证据入口：

- `litellm/router.py:225` 定义通用 `Router`。
- `litellm/router.py:292`、`:298`、`:301` 暴露 `allowed_fails`、`cooldown_time`、`disable_cooldowns`。
- `litellm/router_utils/cooldown_handlers.py:40` 的 `_is_cooldown_required()` 对 429、401、408、404、5xx 等做 cooldown 分类。
- `litellm/router_utils/cooldown_handlers.py:98` 的 `_should_run_cooldown_logic()` 统一处理 deployment、cooldown time、禁用 cooldown 等条件。
- `litellm/router.py:6709` 的 `deployment_callback_on_failure()` 在失败回调中写入 deployment cooldown。
- `litellm/utils.py:6855` 的 `_calculate_retry_after()` 解析 `Retry-After` 或使用指数退避。
- `litellm/router_strategy/lowest_tpm_rpm_v2.py:66`、`:94`、`:119` 在 pre-call 阶段检查 RPM/TPM 并抛出本地 429。
- `litellm/proxy/hooks/parallel_request_limiter.py:100`、`:101`、`:128`、`:136` 对并发/RPM/TPM 超限返回 429 和 `retry-after`。

可借鉴：

- cooldown 决策矩阵清晰：429/可恢复 auth/timeout/not-found/5xx 可冷却，普通业务 4xx 不应盲目冷却。
- cooldown 时间优先级应为：账号/模型显式配置 > 上游 `Retry-After`/reset 字段 > 本地默认值。
- pre-call rate check 应在占用并发 slot 后、真正调用上游前执行，防止并发场景穿透。
- 本地限流产生的 429 要和上游 429 区分，但都应带 `Retry-After`。

不宜照搬：

- LiteLLM 是平台级 proxy/router，功能面远大于 Cockpit 本机 API mode，不应引入其整体架构。
- 部分 proxy hook 的错误细节可能包含 key 或内部 ID，Cockpit 应坚持最小结构化日志。
- `simple-shuffle`、weighted failover 等策略若默认启用，会变成请求级随机轮换。

## Cross-Project Best Practices For Cockpit

1. 建立轻量 `AccountHealthRegistry`：每个账号记录 `healthy | cooling_down | auth_suspect | disabled | manual_required`，附 `cooldown_until`、`reason`、`last_status`、`last_error_type`、`last_request_id`。
2. 429 处理必须 parse reset：优先读取 `Retry-After`、`Retry-After-Ms`、`resets_at`、`resets_in_seconds`；没有 reset 时才使用默认 cooldown。
3. 错误分类要比 status code 更细：`upstream_rate_limit`、`local_rate_limit`、`usage_limit_reached`、`auth_error`、`captcha_or_suspicious`、`network_error`、`server_error` 分开处理。
4. 默认路由使用 `sticky_process`：API service 启动后固定当前账号；当前账号冷却或不可用时才考虑 fallback。
5. 增加 `fill_first` 作为可选策略：按用户排序和健康状态使用第一个可用账号，用尽/冷却后再移动到下一个。
6. `round_robin` / weighted random 只能作为显式 opt-in，不应进入 hardened 默认。
7. 流式请求必须有 `stream_started` guard：上游首字节前可保守重试，首字节后禁止切号续接。
8. 本地 backpressure 要先于上游请求：global semaphore、start-rate limiter、最大排队等待、request timeout、body size limit。
9. 重试边界要硬：默认 `max_retries = 1`，`max_retry_accounts = 1` 或最多 `2`；禁止无限重试和请求级扫号。
10. 401/403/captcha/suspicious 不自动移除账号；默认进入隔离并提示人工确认。删除账号必须手动。
11. 额度刷新降频：自动刷新默认关闭或 >=60 分钟；429/cooldown 期间不通过高频刷新“探测恢复”。
12. 日志只记录结构化元数据：timestamp、route、model、account hash/alias、status、latency、error type、request id、cooldown_until；禁止 prompt/response/token/cookie/Authorization/header/body。
13. 日志要有 size/age cleanup，且清理不应依赖用户手动打开 UI。
14. UI 要把策略分成 Conservative / Balanced / Aggressive 三档，并明确 Aggressive 可能增加风控风险。
15. 127.0.0.1 是 hardened mode 的硬边界：不监听 `0.0.0.0`、LAN IP 或 public IP。

## Recommended Cockpit Defaults

### Hardened Local API Mode

| Area | Recommended default |
| --- | --- |
| listen host | `127.0.0.1` only |
| concurrency | `max_concurrent_requests = 1` |
| request start interval | `min_request_interval_seconds = 20`, `burst = 1` |
| route mode | `sticky_process` |
| request-level rotation | disabled |
| fallback | only before stream starts, only when current account is explicitly unschedulable |
| max retries | `1` |
| max retry accounts | `1` by default, optional `2` for manual balanced profile |
| 429 cooldown | upstream reset if available, otherwise 30 minutes |
| auth/captcha/suspicious | quarantine + manual confirmation |
| quota refresh | manual or >=60 minutes |
| wakeup/keepalive | disabled |
| logs | metadata only, redacted, size/age cleanup |

### Balanced Personal Mode

适合用户明确接受略高活跃度时启用：

- `max_concurrent_requests = 1..2`
- `min_request_interval_seconds = 5..10`
- `routing.mode = fill_first`
- `max_retry_accounts = 2`
- 自动刷新 >=60 分钟，并在 cooldown 中跳过探测。

### Aggressive Mode

只应作为显式 opt-in：

- round-robin / weighted strategy
- 更多 fallback accounts
- 更短请求间隔
- 更频繁健康检查

UI 必须提示：该模式更可能触发上游风控，不属于 hardened 默认。

## What Cockpit Should Not Do

- 不做请求级随机轮询。
- 不在一个请求失败后扫完整账号池。
- 不把 401/403 账号自动删除。
- 不通过高频额度刷新判断账号是否恢复。
- 不记录 prompt、response、OAuth token、refresh token、cookie、Authorization header。
- 不在 hardened mode 暴露 LAN/public host。
- 不用随机 UA 当主要安全策略；它只能是低价值兼容项，不能替代低频、低并发、粘性路由和最小日志。

## Suggested Implementation Shape

1. `LocalApiSafetyConfig`：承接 hardened host、concurrency、rate limit、retry、fallback、logging、refresh interval。
2. `AccountHealthRegistry`：维护账号/模型 cooldown、auth suspect、manual required、last error。
3. `ErrorClassifier`：将 HTTP status、provider error code、headers/body reset 字段归一化。
4. `LocalBackpressure`：global semaphore + request-start limiter + bounded wait。
5. `StickyAccountSelector`：`fixed_account_id` > process sticky > fill-first fallback；只选择 schedulable account。
6. `RetryFailoverController`：只在未开始 stream 时允许 fallback；所有 retry 都走同一错误分类和日志路径。
7. `SafeAuditLogger`：结构化字段、统一 redact、日志轮转/清理。

## Test Matrix To Port Into Cockpit

- 强制 `127.0.0.1` 监听。
- global semaphore 单并发。
- start-rate limiter 默认 20 秒间隔。
- streaming 首字节后释放 semaphore 且不重试切号。
- 429 写入账号/模型 cooldown。
- 401/403/captcha/suspicious 写入 quarantine/manual-required。
- sticky_process 不发生请求级轮换。
- fill-first 用尽/冷却后才移动到下一个账号。
- local 429 与 upstream 429 均带 `Retry-After`，但 `error_type` 不同。
- 日志脱敏覆盖 Authorization、OAuth token、cookie、prompt、response。
- 自动额度刷新间隔 >=60 分钟，cooldown 账号不高频探测。
- request timeout 和异常中断都释放 semaphore。

## Rollback And Adoption

本报告是 docs-only 参考资料，无运行时行为变更。若后续实现 Hardened Local API Mode，应按小步闭环落地：先新增配置和测试，再接入 API service，最后加 UI 与文档；每个 slice 都保留回滚点。
