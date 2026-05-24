# Reference Gateway Best Practices Review

审查时间：2026-05-24

审查目标：为 Cockpit Tools Local 的个人本机 API service / Hardened Local API Mode 提炼可借鉴实践。本文只作为设计参考，不把社区项目的策略直接等同于本仓应启用的默认行为。Codex-facing 行为以官方 `openai-codex` 源码和本仓实测为最高语义锚点，社区项目只用于调度结构、cooldown、限流和可观测性启发。

## Source Snapshot

| Project | Local path | Revision | Role |
| --- | --- | --- | --- |
| OpenAI Codex | `D:\CODE\external\_reference_gateway_sources\openai-codex` | `7d47056` | 官方 Codex 源码；Codex-facing `/v1/responses`、stream terminal、turn metadata、重放/连续性语义 |
| New API | `D:\CODE\external\_reference_gateway_sources\new-api` | `49bc3a1` | 渠道网关、渠道禁用、重试、限流 |
| Sub2API | `D:\CODE\external\_reference_gateway_sources\sub2api` | `63b0631a` | 账号健康、调度、临时不可调度、粘性会话 |
| CLIProxyAPI | `D:\CODE\external\_reference_gateway_sources\CLIProxyAPI` | `50d19e20` | CLI/OAuth 代理、凭据选择、模型冷却、流式重试边界 |
| LiteLLM | `D:\CODE\external\_reference_gateway_sources\litellm` | `4148667` | 通用 router、cooldown、pre-call rate checks、proxy limits |

## Evidence Precedence

后续修改 API service、号池调度、排序、风控降噪或 Codex-facing fallback 时，证据优先级固定如下：

1. 本仓运行事实、focused tests、smoke/acceptance report。
2. 官方 `openai-codex` 源码：Codex 请求形态、turn metadata、Responses SSE terminal、同 turn 是否可重放、`previous_response_id` 语义、失败/完成事件处理。
3. OpenAI 官方 API 文档：公开错误码、rate limit、`Retry-After`/backoff 语义。
4. 本地参考项目源码：Sub2API 的 `IsSchedulable()`/persistent cooldown，CLIProxyAPI 的 fill-first/session affinity/首字节后不重试，LiteLLM 的 pre-call rate checks/cooldown matrix，New API 的 retry/disable framework。
5. 社区文章或 issue 只能作为待核线索，不能覆盖官方源码、本仓实测和项目合同。

如果官方 Codex 源码与社区网关实践冲突，Codex-facing 行为优先复刻官方 Codex 源码；社区网关只能影响 Cockpit 内部调度实现形状。

## Official Codex Anchors

这些锚点是后续修改 `src-tauri/src/modules/codex_local_access.rs` admission/stream/fallback/turn-affinity 逻辑前必须复核的最小集合：

- `codex-rs/codex-api/src/sse/responses.rs`：`response.failed` 被转成 terminal `ApiError`，`response.completed` 才产出 `ResponseEvent::Completed`，SSE 结束但没有 `response.completed` 会报 `stream closed before response.completed`。这对应本仓“已接纳 stream 继续在原账号跑到 terminal”的 terminal 判定。
- `codex-rs/app-server-protocol/src/protocol/v2/shared.rs` 和 `codex-rs/app-server/README.md`：官方区分 `ResponseStreamConnectionFailed` 与 `ResponseStreamDisconnected`，后者表示 turn 中途 SSE 断开。Cockpit 不应把中途断开静默解释成可跨账号续接。
- `codex-rs/core/src/client.rs`：`ModelClientSession` 是 per-turn 状态，缓存 `x-codex-turn-state` 并复用 `previous_response_id`；`ResponseEvent::Completed` 后才保存 `LastResponse.response_id` 供后续 continuation。跨账号 fallback 不得伪造同一个 `previous_response_id`。
- `codex-rs/app-server-protocol/src/protocol/thread_history.rs`：`TurnContext.turn_id` 是 canonical turn id；晚到的 turn-scoped item 按原 `turn_id` 路由，未知 `turn_id` 被 drop，`late_turn_complete_does_not_close_active_turn` 固化了旧 turn terminal 不能关闭新 turn。

对应官方 API 文档锚点：

- Error codes 文档的 `previous_response_not_found` 要求使用完整 input context 并把 `previous_response_id` 置空重试；这与本仓“同 turn 不跨账号伪造 continuation”一致。
- 429/503 文档要求 pacing、backoff、尊重 response headers、稳定速率后逐步恢复；这与本仓低并发、低刷新、persistent cooldown 和手动恢复策略一致。

## Executive Conclusions

0. Codex-facing `/v1/responses` 不是普通 OpenAI-compatible HTTP 客户端。当前 turn、stream terminal、`previous_response_id`、本地 completed Responses 闭合和同 turn 禁止静默跨账号重放，必须优先对齐官方 `openai-codex` 源码，再参考社区网关策略。
1. `429` 必须拦截，但不应理解为“马上扫下一个账号”。社区成熟做法更接近：分类错误、读取 `Retry-After` 或 provider reset 字段、把当前账号/模型放入 cooldown，再由健康选择器决定是否可 fallback。
2. Hardened Local API Mode 的默认路由应是 `sticky_process` 或 `fill_first`，不是 request-level random / round-robin。多账号池可以支持排序和健康 fallback，但默认不做每请求轮询。
3. 当前任务能否“额度耗完仍继续”取决于上游连接是否已经建立、是否已经开始流式输出、以及服务端是否还能继续发送。网关只能保证本地不因自己的重试/切号策略中断；不能把上游已经返回的 `429` 变成继续执行。
4. 最值得借鉴的是 Sub2API 的账号状态机、CLIProxyAPI 的流式重试边界、LiteLLM 的 cooldown 决策矩阵、New API 的可配置重试和渠道禁用框架。
5. 最不宜照搬的是面向公网/多用户网关的激进默认值：请求级轮询、跨账号扫射、无限或多凭据重试、高频额度刷新、请求/响应正文日志、LAN/public listen。
6. local limit 与 upstream limit 必须分开：本地 backpressure 产生的 429 是保护性拒绝，上游 429 是账号/模型状态信号，两者要有不同 `error_type`、日志字段和恢复提示。
7. 参考项目里的“自动禁用/自动切换”大多是平台级能力；Cockpit 自用版默认应该是“进入 cooldown 或人工确认态”，删除账号、禁用账号和扫完整池都必须显式 opt-in。

## Cockpit Fit-Gap Matrix

| Cockpit current gap | Evidence in Cockpit | Reference signal | Roadmap action |
| --- | --- | --- | --- |
| 多账号 UI/路由语义与后端实际单账号不一致 | 旧实现中 `build_effective_local_access_account_ids()` 只取 `take(1)`，`sanitize_collection()` 第一个账号后 `break` | Sub2API 把调度资格集中到 `IsSchedulable()`，CLIProxyAPI 有 fill-first/session affinity | 2026-05-18 已移除保存/规范化层单账号裁剪；Hardened selector 现在保留完整候选池，fill-first 不做账号快照刷新，实际上游尝试仍默认单账号 cap |
| 没有本地 backpressure | API service 没有 global semaphore、请求启动间隔和 bounded queue | LiteLLM 在 pre-call 阶段检查 RPM/TPM/parallel limit，New API 有全局/模型限流 | `HLA-03 LocalBackpressure` |
| 429 cooldown 解析不完整 | 只解析 `usage_limit_reached` body 的 reset 字段 | LiteLLM/New API/CLIProxyAPI/Sub2API 都把 retry/cooldown 作为独立策略面；LiteLLM 读取 header cooldown | `HLA-02 ErrorClassifier` 增加 `Retry-After` / `Retry-After-Ms` / body reset |
| cooldown 不持久 | `GatewayRuntime.model_cooldowns` 为内存状态 | Sub2API 持久化 `RateLimitResetAt` 和模型 cooldown | `HLA-04 Persistent AccountHealthRegistry` |
| 401/403/captcha/suspicious 没有人工确认态 | 401 刷新失败后只记录错误/失效 prepared account | Sub2API 对 OAuth 401 设置临时不可调度，403/captcha 类进入保守状态 | `HLA-04` 增加 `auth_suspect` / `manual_required` |
| 风控关键节点不可串联回放 | 监听、认证投影、selector、上游转发、classifier、stream 写出目前分散在普通日志/内存状态 | LiteLLM callbacks/observability、New API request log、CLIProxyAPI usage logging 都把请求事件结构化，但 Cockpit 只应本地脱敏保留 | `HLA-04A SafetyObserver/AuditTrail`，被动记录 request_id 事件链，不新增主动探测；2026-05-18 已落地本地 JSONL 脱敏事件首个运行路径 |
| 流式切号边界不够显式 | 代理层在拿到 upstream response 后才写客户端，缺少统一 stream guard 抽象 | CLIProxyAPI 单测固化“首字节后不重试” | `HLA-05 RetryFailoverController`；2026-05-18 已落地 stream write state 和默认单账号/单 retry cap |
| UI 和日志泄露面 | UI 仍展示 LAN 口径，隐藏 API key 时 `title` 含完整 key；失败日志含 raw detail/account id | Sub2API `logredact` 做 JSON/text 脱敏；CLIProxyAPI 对 header/API key 脱敏 | `HLA-00` 先止血 |
| 请求体/超时/retry 常量未进入 preset | `MAX_HTTP_REQUEST_BYTES`、`REQUEST_READ_TIMEOUT`、`UPSTREAM_SEND_RETRY_ATTEMPTS` 等仍是代码常量 | LiteLLM/New API 把 rate/retry limit 作为可配置面，CLIProxyAPI 暴露 request retry / retry interval | `HLA-01` 明确配置字段与内部硬上限 |
| local 429 与 upstream 429 未分层 | 当前只有上游失败摘要和内存 cooldown | LiteLLM pre-call limiter 与 proxy limiter 返回本地 429 + `retry-after`，上游 cooldown 另走 router state | `HLA-02/HLA-03` 区分 `local_backpressure` 和 `upstream_rate_limit` |

结论：Cockpit 不缺“更多路由策略”的雏形，缺的是打开多账号池前的安全状态机、冷却持久化、backpressure 和流式边界。因此路线图应先做 P0 护栏，而不是先做账号池扩容。

## Deep Review Addendum

- Cockpit 的 `127.0.0.1` bind 是正确基线，但 `lan_base_url` 仍会被计算并返回给前端；hardened UI 应默认不展示 LAN 入口，旧字段仅保留兼容。
- Cockpit 的上游请求有发送层 retry、单账号 5xx retry、账号池 fallback 三层概念；后续实现必须统一到 `RetryFailoverController`，否则很容易出现“配置显示保守，但某层仍在重试”的漂移。
- Cockpit 的流式写出路径在下游 headers 或 chunk 发出后已不能改变响应身份；stream guard 应跟随写出状态，而不是只跟随 upstream response 是否成功。
- Sub2API 的 `IsSchedulable()` 和 CLIProxyAPI 的 `isAuthBlockedForModel()` 都把“可调度性”做成单一判定入口；Cockpit 应避免把 cooldown、auth suspect、manual pause 分散在 selector、retry 和 UI 三处各自判断。
- 多账号池打开后，默认路由仍应是稳定起点或 sticky/fill-first；2026-05-18 已把 hardened mode 的请求起点固定为 0，加入 process sticky binding、cooldown/auth/manual sticky 清理，以及完整候选池 + 单请求尝试 cap，避免每个请求推进 round-robin cursor。
- UI 只应展示脱敏 health summary：计数、sticky account hash、最近错误类型和 cooldown 到期时间；2026-05-18 已接入 API 服务面板和显式用户动作触发的单账号/单模型 cooldown 恢复，恢复事件只写入脱敏 audit event。
- LiteLLM 的 pre-call rate checks 提醒 Cockpit：本地限流要发生在真正调用上游前，并返回本地 `Retry-After`；不能等上游 429 后才反应。
- 多个参考项目有 request/body logging 能力或 raw error 兜底日志。Cockpit hardened mode 应借鉴其脱敏工具和测试，不借鉴默认捕获 prompt/response 的能力。
- CLIProxyAPI 的 usage logging、New API 的 request log 与 LiteLLM observability 都证明“结构化事件链”是成熟网关常见实践；Cockpit 的差异化边界是本机 JSONL、脱敏字段、低频被动写入，不记录正文、不把 audit 当额度探测器。

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
- 已被上游接纳的 active stream 应由本地 lease 保护：后续 cooldown、exhausted、health registry 或 selector 状态变化只影响新的 admission，不 retroactively cancel 当前 stream。
- 新的 independent request 不需要等待其他 active stream 完成；但 `previous_response_id` continuation 不能跨账号直接复用，跨账号 fallback 必须走 full context replay 或 compacted replay。
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
