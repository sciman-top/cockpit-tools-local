# Cockpit-Tools-Local 自用 Hardened API Mode 实施计划

更新时间：2026-05-18

## 目标与边界

本计划承接 `docs/LOCAL_HARDENED_API_ROADMAP.md` 和 `docs/reference-gateway-best-practices.md`。当前落点是 `src-tauri/src/modules/codex_local_access.rs` 驱动的本机 Cockpit API Service；目标归宿是一个默认保守、低并发、可观察、可回滚的自用 Hardened Local API Mode。

不做事项保持不变：不做公网/LAN 网关，不做请求级随机扫号，不把 LiteLLM/New API/Sub2API/CLIProxyAPI 变成强依赖，不把 500+ free 账号池当作高频自动刷新对象。

本地参考源：`D:\CODE\external\_reference_gateway_sources` 保存 `CLIProxyAPI`、`litellm`、`new-api`、`sub2api` 的源码快照。改动 retry/fallback、health registry、audit trail、selector、stream guard 或路线图时，优先参考该目录和 `docs/reference-gateway-best-practices.md`，再决定是否需要外部资料。

## 审查结论固化

### Cockpit 当前代码事实

- API service 核心入口在 `src-tauri/src/modules/codex_local_access.rs`，配置模型在 `src-tauri/src/models/codex_local_access.rs` 和 `src/types/codexLocalAccess.ts`。
- 后端监听常量已固定为 `127.0.0.1`，但 UI 仍展示 `lanBaseUrl`、`本机/局域网`、`监听本机与局域网` 等口径。
- 实际调度池仍是单账号：`build_effective_local_access_account_ids()` 对 `account_ids` 执行 `take(1)`，`sanitize_collection()` 在第一个有效账号后 `break`，单测 `effective_local_access_pool_is_single_account` 固化了该事实。
- 代理循环已经有多账号雏形、response affinity、model cooldown 和 retry 结构，但由于有效池被裁剪为 1 个账号，多账号排序和 fallback 目前不是完整能力。
- 429 cooldown 只从 `usage_limit_reached` body 中解析 `resets_at` / `resets_in_seconds`，还没有读取 `Retry-After` / `Retry-After-Ms` header。
- model cooldown 只在内存 `GatewayRuntime.model_cooldowns` 中，应用重启后会丢失。
- 没有全局 semaphore、请求启动间隔 limiter、最大排队等待和请求级 timeout 配置模型。
- `MAX_HTTP_REQUEST_BYTES = 64 MiB`、`REQUEST_READ_TIMEOUT = 15s`、`MAX_REQUEST_RETRY_ATTEMPTS = 1`、`UPSTREAM_SEND_RETRY_ATTEMPTS = 3` 等仍是代码常量；hardened preset 需要显式决定哪些暴露为配置，哪些保留内部上限。
- 后端 bind host 是 `127.0.0.1`，但 `snapshot_local_access_state()` 仍会返回按 LAN IPv4 推导的 `lan_base_url`；hardened UI 必须把它当兼容字段而非默认入口。
- 401 会尝试刷新并重试，但没有持久化的 `auth_suspect` / `manual_required` 账号状态。
- `CodexLocalAccessModal` 在 API key 隐藏时仍把完整 key 放入 DOM `title`，需要先修。
- `CodexAccountsPage` 的 inline API service 卡片也会在隐藏 key 时把完整 key 放入 DOM `title`，需要和 modal 同步修。
- 失败日志经 `logger::sanitize_message()` 会做邮箱脱敏，但 `log_codex_api_failure()` 仍传入 raw `account_id`、raw upstream detail 和部分含邮箱拼接消息；需要改成结构化 error type 与 account hash/alias。
- usage stats 记录 `account_id` 和 email，属于本机状态可接受范围，但导出、日志和 UI 展示必须明确区分，不可把 stats 当作可外发审计日志。
- 成功 upstream response 进入 `write_gateway_response()` 后会向下游写 headers/chunks；一旦写出，当前请求只能完成或失败，不能再换账号续接。

### 外部项目可迁移实践

- New API 证明“重试/禁用 channel”应配置化，但其默认 `RetryTimes = 0`、自动禁用关闭，不支持把它解读成默认激进 fallback。
- Sub2API 最值得借鉴的是 `IsSchedulable()` 一等状态判断、持久化 rate limit reset、temp unschedulable、sticky 清理和账号健康面板；不应搬 DB/Redis/scheduler 的重量。
- CLIProxyAPI 最值得借鉴的是 `fill-first`、`Session_id` 粘性、model cooldown error 携带 `Retry-After`、以及“首字节后不再重试/切号”的流式边界。
- LiteLLM 最值得借鉴的是 cooldown 决策矩阵、header cooldown 优先级、pre-call RPM/TPM/parallel checks 和本地 429 `retry-after`；不应引入整个平台级 router。
- 所有参考项目也都有不宜照搬的面：New API 和 CLIProxyAPI 默认更偏平台/多凭据 retry，Sub2API 带 DB/Redis/scheduler 重量，LiteLLM 是完整 proxy 平台；Cockpit 只吸收可本地文件化、可低频运行、可手动恢复的部分。

## 实施原则

1. 先补安全护栏，再打开多账号池。
2. 先新增可测试的纯逻辑模块，再接入长函数热路径。
3. 所有重试和切号必须经过同一个 `ErrorClassifier`。
4. multi-account 保存和调度不得先于 persistent health registry 与 backpressure。
5. 流式响应一旦开始向客户端写出 payload，当前请求禁止切号续接。
6. 用户可见状态只显示 alias/hash/count/status，不显示 token、完整 key、完整邮箱、prompt、response。
7. 风控监察以被动事件和低频健康快照为主，不做主动扫号、伪装探测或以规避平台识别为目标的逻辑。
8. 每个 slice 都可单独回滚，且完成后系统仍可运行。

## 依赖顺序

```mermaid
flowchart TD
  HLA00["HLA-00 UI/日志泄露止血"] --> HLA01["HLA-01 配置与类型模型"]
  HLA01 --> HLA02["HLA-02 ErrorClassifier"]
  HLA01 --> HLA03["HLA-03 LocalBackpressure"]
  HLA02 --> HLA04["HLA-04 Persistent AccountHealthRegistry"]
  HLA03 --> HLA04
  HLA04 --> HLA04A["HLA-04A SafetyObserver/AuditTrail"]
  HLA04A --> HLA05["HLA-05 RetryFailoverController"]
  HLA05 --> HLA06["HLA-06 打开真实账号池"]
  HLA06 --> HLA07["HLA-07 sticky/fill-first selector"]
  HLA07 --> HLA08["HLA-08 状态面板与手动恢复"]
  HLA08 --> HLA09["HLA-09 刷新/唤醒降噪"]
  HLA09 --> HLA10["HLA-10 preset/文档/smoke"]
```

## 任务清单

### HLA-00 UI/日志泄露止血

描述：先修低风险但高敏感度的问题，避免在后续调试中继续暴露完整 key、raw upstream detail 或误导性 LAN 文案。

状态：已实现（2026-05-17）。已完成静态检查、前端 typecheck、Rust 全量测试；尚未启动桌面 UI 做 live DOM/log tail smoke。

验收：

- [x] API key 隐藏状态下 DOM `title` 不包含完整 key，复制按钮仍可复制。
- [x] API service 卡片和 modal 默认文案改为“仅本机”。
- [x] `localAccessAddressKind = "lan"` 的旧持久化偏好在 hardened mode 下会自动回落到 `local` 展示。
- [x] `lanBaseUrl` 仍可作为旧状态兼容字段返回，但 hardened 默认 UI 不展示 LAN 选项。
- [x] 失败日志只输出 `error_type`、status、route、model、latency、account hash/alias、request id。
- [x] 脱敏回归覆盖 `Authorization`、API key、OAuth token、完整邮箱、raw upstream body 和含 prompt/response 字样的错误文本。

验证：

- [x] `npm run typecheck`
- [x] `cargo test --package cockpit-tools --quiet`
- [x] `cargo test --package cockpit-tools --quiet -- --test-threads=1`
- [x] `cargo fmt --check`
- [x] `git diff --check`
- [x] 静态扫描：`title={collection.apiKey}`、`title={localAccessCollection?.apiKey`、`本机/局域网`、`监听本机与局域网`、`Local/LAN`、`Listens on local and LAN` 在 HLA-00 相关 UI/locale 文件中无命中。
- [ ] 手动检查 UI DOM title 和日志 tail。

可能文件：

- `src/components/CodexLocalAccessModal.tsx`
- `src/pages/CodexAccountsPage.tsx`
- `src/locales/zh-CN.json`
- `src/locales/en.json`
- `src-tauri/src/modules/codex_local_access.rs`
- `src-tauri/src/modules/logger.rs`

依赖：无。

### HLA-01 配置与类型模型

描述：新增 `LocalApiSafetyConfig`，只做模型、默认值、读写迁移和状态回显，暂不改变调度行为。

状态：已实现（2026-05-17）。已新增 Rust/TS 配置合同、旧配置兼容迁移、未来 schema fail-closed 归一化；当前只回显配置，不改变请求调度热路径。

验收：

- [x] 旧 `codex_local_access.json` 缺字段时自动补安全默认值。
- [x] Rust model 与 TS type 字段一致。
- [x] `hardenedLocalMode` 默认 `true`。
- [x] 配置包含 `schemaVersion`，未来字段迁移能 fail-closed。
- [x] 配置状态能回显 `maxConcurrentRequests`、`minRequestIntervalSeconds`、`maxQueueWaitSeconds`、`requestTimeoutSeconds`、`maxRequestBodyMb`、`maxRetries`、`maxRetryAccounts`、`fallbackMode`、`logging`。
- [x] 当前硬编码常量要么迁入配置默认值，要么在代码旁标注为不可放宽内部上限并加入测试。

验证：

- [x] `cargo test --package cockpit-tools local_api_safety_config --quiet`
- [x] `cargo test --package cockpit-tools --quiet`
- [x] `npm run typecheck`
- [x] `cargo fmt --check`
- [x] `git diff --check`

可能文件：

- `src-tauri/src/models/codex_local_access.rs`
- `src-tauri/src/modules/codex_local_access.rs`
- `src/types/codexLocalAccess.ts`

依赖：HLA-00 可并行，但推荐先完成 HLA-00。

### HLA-02 ErrorClassifier

描述：抽出 `ErrorClassifier`，统一解析 HTTP status、headers、body、OpenAI/Codex provider fields，并产出结构化错误。

状态：2026-05-17 已完成本 slice。实现落点仍在 `src-tauri/src/modules/codex_local_access.rs`，后续复杂度增加时再拆 `codex_local_access_classifier.rs`。当前只收紧单账号/请求级安全边界，不打开真实多账号池。

验收：

- [x] `Retry-After` 秒数和 HTTP-date 均可解析。
- [x] `Retry-After-Ms` 可解析，且优先级高于 `Retry-After` 和 body reset。
- [x] `usage_limit_reached.resets_at` / `resets_in_seconds` 可解析。
- [x] `insufficient_quota`、`quota exceeded`、`selected model is at capacity` 分类清晰。
- [x] 401/403/captcha/suspicious 进入保守分类，不触发请求级扫号。
- [x] 未知 429 只产生上游限流分类和可选 cooldown，不触发跨账号扫射。
- [x] `ClassifiedError` 至少包含 `source`、`scope`、`status`、`provider_code`、`retry_after`、`manual_required`、`safe_message`、`log_fields`。
- [x] `upstream_rate_limit`、`usage_limit_reached`、`auth_error`、`captcha_or_suspicious`、`insufficient_quota`、`model_capacity`、`network_error`、`server_error` 分开处理；`local_rate_limit` 留到 HLA-03 本地 backpressure 接入。
- [x] raw body 不直接进入日志或 `safe_message`。

验证：

- [x] classifier 单测覆盖 header/body/status 组合。
- [x] `cargo test --package cockpit-tools classifier --quiet`
- [x] `cargo test --package cockpit-tools retry_after --quiet`
- [x] `cargo test --package cockpit-tools --quiet`
- [x] `npm run typecheck`

可能文件：

- `src-tauri/src/modules/codex_local_access.rs`
- 后续可拆到 `src-tauri/src/modules/codex_local_access_classifier.rs`

依赖：HLA-01。

### HLA-03 LocalBackpressure

描述：在进入上游前增加本地 backpressure：global semaphore、请求启动间隔、bounded queue、请求超时。

状态：2026-05-18 已完成本地背压切片。当前实现仍保持单账号保守路径，不打开多账号池；stream guard 的“已写出后不跨账号续接”继续归入 HLA-05。

验收：

- [x] hardened 默认同一时间最多 1 个上游请求。
- [x] 新请求启动默认至少间隔 20 秒。
- [x] 等待超时返回本地 429/503，并带 `Retry-After`。
- [x] 本地 backpressure 返回的错误使用 `error_type = local_backpressure` 或等价结构，不能伪装成 upstream quota。
- [x] streaming 成功、客户端断开、上游异常都会通过 scoped permit drop 释放 permit；HLA-05 继续覆盖 stream 已写出后的禁止重试/切号。
- [x] `/v1/models` 不占用上游请求 permit。

验证：

- [x] 并发请求单测：`cargo test --package cockpit-tools local_backpressure --quiet`
- [x] permit drop 释放单测：`cargo test --package cockpit-tools local_backpressure --quiet`
- [x] `cargo test --package cockpit-tools --quiet`
- [x] `cargo test --package cockpit-tools --quiet -- --test-threads=1`
- [x] `npm run typecheck`
- [x] `cargo fmt --package cockpit-tools --check`
- [x] `git diff --check`

可能文件：

- `src-tauri/src/modules/codex_local_access.rs`
- 后续可拆到 `src-tauri/src/modules/codex_local_access_backpressure.rs`

依赖：HLA-01。

### HLA-04 Persistent AccountHealthRegistry

描述：新增 API service 专用运行态文件，不污染原始账号凭据；记录账号/模型 cooldown、auth suspect、manual required 和最近错误。

状态：2026-05-18 已完成基础持久化切片：新增 health registry schema、原子写入、损坏 fail-closed、上游错误分类到健康状态、真实请求入口加载健康状态并跳过不可调度账号。手动清除和 UI 状态面板继续归入 HLA-08。

建议文件：

- `codex_local_access_health.json`

建议顶层结构：

- `schema_version`
- `updated_at`
- `accounts`
- `model_cooldowns`
- `sticky_bindings`
- `last_global_error`

验收：

- [x] 429 cooldown 重启后仍生效。
- [x] 401/403/captcha/suspicious 重启后仍需人工确认。
- [ ] 用户可手动清除某个账号/模型状态。
- [x] 状态文件不保存 prompt、response、token、cookie、完整 API key。
- [x] 状态文件使用原子写入，损坏时 fail-closed 并提示用户，而不是悄悄打开全部账号调度。
- [x] health registry 是 API service 运行态，不反向覆盖 Cockpit 账号中心里的 OAuth/API Key 凭据。

验证：

- [x] health registry serde/fail-closed 单测：`cargo test --package cockpit-tools health_registry --quiet`
- [x] cooldown 持久化单测：`cargo test --package cockpit-tools health_registry --quiet`
- [x] unknown 429 不判定 exhausted 单测：`cargo test --package cockpit-tools health_registry --quiet`

可能文件：

- `src-tauri/src/models/codex_local_access.rs`
- `src-tauri/src/modules/codex_local_access.rs`
- 后续可拆到 `src-tauri/src/modules/codex_local_access_health.rs`

依赖：HLA-02、HLA-03。

### HLA-04A SafetyObserver/AuditTrail

描述：在 API service 内部加入被动监察层，记录风控相关关键节点的脱敏事件；不新增独立常驻探测进程，不通过额外请求判断额度，不设计规避官方识别的策略。

状态：2026-05-18 已完成首个运行路径切片：新增本地 JSONL audit trail、结构化脱敏事件、大小轮转，并接入真实请求的 listener、selector、classifier、health update、stream write/final response 边界。认证投影聚合、上游转发细分事件和 UI degraded 提示继续归入 HLA-08/HLA-05 的相邻切片。

事件来源：

- 服务监听：request accepted、route、model、method、request_id。
- 认证投影：current account source、account alias/hash、refresh decision，不记录 token/cookie；首个切片先由 401/refresh 分类事件承接。
- 上游转发：route、model、stream flag、request_id、account alias/hash、latency、status；后续在 HLA-05 拆出 `upstream_forward` 明确阶段。
- 429/401/403/captcha/suspicious 分类：`error_type`、source、scope、manual required、retry-after。
- selector 排序：chosen account hash；candidate count、skipped reason 和 sticky binding reason 后续补齐。
- stream 写出：headers_written、first_chunk_written、finish/upstream_error。
- Codex CLI 直连 smoke：仅记录本地 loopback 探针结果和状态码，不记录 prompt/response。

验收：

- [x] audit trail 只保存结构化元数据，不保存 prompt、response、messages、token、cookie、Authorization、完整邮箱、完整 API key、raw upstream body。
- [x] 额度为零只通过真实业务请求返回的明确 quota/exhaustion 信号或人工标记确认；未知 429 只标为 rate limit/cooldown，不直接判定 exhausted。
- [x] 监察逻辑不发起额外上游请求，不批量刷新 500+ 账号，不通过高频探测等待恢复。
- [ ] 每个 request_id 可串起：listener -> auth projection -> selector -> upstream -> classifier -> health update -> stream write/final response。当前已覆盖 listener、selector、classifier、health update、stream write/final response；auth projection 和 upstream forward 细分待补。
- [ ] audit 文件采用大小/天数轮转，损坏时不影响 API service 启动，但 UI 明确提示 audit degraded。当前已完成大小轮转；天数轮转和 UI degraded 待补。
- [ ] UI 只展示聚合状态和最近脱敏事件，默认不展开账号级细节。

验证：

- [x] audit event serde/redaction 单测：`cargo test --package cockpit-tools audit_event --quiet`
- [x] quota exhausted vs unknown 429 分类审计单测：`cargo test --package cockpit-tools classifier --quiet`、`cargo test --package cockpit-tools health_registry --quiet`
- [x] stream headers/first chunk 审计边界单测：`cargo test --package cockpit-tools audit_event --quiet`
- [ ] `git diff --check`

建议文件：

- `codex_local_access_audit.jsonl`
- 后续可拆到 `src-tauri/src/modules/codex_local_access_audit.rs`

依赖：HLA-02、HLA-03、HLA-04。

### HLA-05 RetryFailoverController

描述：把 retry、cooldown、single-account retry、next-account fallback、stream guard 统一到一个控制器。

状态：2026-05-18 已完成首个控制器边界切片：`maxRetries` 进入单账号状态重试和请求级冷却等待重试，`maxRetryAccounts` + `fallbackMode` 进入有效账号尝试上限，默认仍只尝试 1 个账号；新增 stream write state，headers 或首个 chunk 写出后禁止 fallback 的契约已单测固化。完整 `RetryFailoverController` 类型拆分、client disconnect 分类和 UI 高级开关继续留在后续切片。

验收：

- [x] 默认 `maxRetries = 1`。
- [x] 默认 `maxRetryAccounts = 1` 表示一个请求最多使用 1 个 distinct account，不扫完整账号池。
- [x] 只有在未向客户端写出 headers 和 payload 前才允许 fallback。
- [x] 首字节后上游错误只结束当前请求，不切号续接。当前由 stream write state 和写出路径约束；client disconnect 细分日志待补。
- [x] local 429 与 upstream 429 的 `error_type` 不同。

验证：

- [x] stream-first-byte guard 单测：`cargo test --package cockpit-tools stream_write_state --quiet`
- [x] fallback cap 单测：`cargo test --package cockpit-tools retry_failover --quiet`
- [x] local/upstream 429 response 单测：`cargo test --package cockpit-tools local_backpressure --quiet`、`cargo test --package cockpit-tools classifier --quiet`

可能文件：

- `src-tauri/src/modules/codex_local_access.rs`
- 后续可拆到 `src-tauri/src/modules/codex_local_access_failover.rs`

依赖：HLA-02、HLA-04、HLA-04A。

### HLA-06 打开真实账号池

描述：在护栏完成后，移除 `take(1)` / `break` 裁剪，保存用户选择的多个有效账号。

状态：2026-05-18 已完成数据面切片：UI 可多选，保存/规范化层保留多个有效账号并去重过滤无效账号；请求选择器可看到完整账号池，但实际上游尝试仍受 HLA-05 的 `maxRetryAccounts` 与 `fallbackMode` 控制，默认一个请求只尝试 1 个账号，不提升并发或缩短请求间隔。500+ selector cap 与 sticky/fill-first 默认路由已在 HLA-07 落地。

验收：

- [x] `account_ids` 可保存多个有效账号。
- [x] 请求选择器使用完整账号候选池，健康过滤后只尝试当前 cap 允许的账号数；projection 仍按安全 cap 取单账号写入。
- [x] 旧单账号配置无缝迁移。
- [x] 多账号池不会自动提高并发或缩短请求间隔。
- [x] 现有单账号池单测必须被改写为“旧配置默认单账号安全迁移”和“多账号需依赖 health registry”的新契约，不能直接删除无替代证据。
- [x] 500+ fake accounts 测试覆盖 selector 候选池和一次请求最多尝试账号数。

验证：

- [x] `sanitize_collection()` 多账号保存单测：`cargo test --package cockpit-tools local_access_account_filter --quiet`
- [x] 旧配置迁移单测：`cargo test --package cockpit-tools effective_local_access_pool --quiet`
- [x] 500+ fake accounts selector cap 单测：`cargo test --package cockpit-tools hardened_routing --quiet`

可能文件：

- `src-tauri/src/modules/codex_local_access.rs`
- `src/components/CodexLocalAccessModal.tsx`

依赖：HLA-04、HLA-05。

### HLA-07 sticky/fill-first selector

描述：把当前 round-robin cursor 改成 hardened 默认的 sticky/fill-first selector。

状态：2026-05-18 已完成核心选择器切片：hardened mode 下请求起点固定为 0，不再随每个请求推进 round-robin cursor；候选池保留完整账号列表，实际上游尝试仍受 `maxRetryAccounts` / `fallbackMode` cap 控制；默认 fill-first 保持用户排序并跳过会触发账号快照刷新的 plan/quota 排序；process sticky binding 写入 health registry，健康时置顶，cooldown/auth/manual/过期/失效时清理或绕过；`previous_response_id` affinity 仍在最后置顶。`Session_id` / `X-Client-Request-Id` 作为后续任务级扩展保留。

策略：

- `sticky_process`：AI 推荐。默认固定进程级账号，账号不可调度时才 fallback。
- `fill_first`：适合 quota drain careful，按用户排序用第一个健康账号。
- `balanced_low_rate`：仅手动 opt-in，低频分散请求。
- `round_robin`：仅保留显式高级选项，不进入 hardened 默认。

验收：

- [x] `previous_response_id` affinity 优先。
- [ ] `Session_id` / `X-Client-Request-Id` 可作为后续扩展来源。
- [x] 当前账号 healthy 时不会请求级轮换。
- [x] cooldown/auth/manual 状态会清理或绕过粘性绑定。

验证：

- [x] sticky_process 单测：`cargo test --package cockpit-tools hardened_routing --quiet`
- [x] fill_first 单测：`cargo test --package cockpit-tools hardened_routing --quiet`
- [x] cooldown clears sticky 单测：`cargo test --package cockpit-tools hardened_routing --quiet`
- [x] 500+ selector cap 单测：`cargo test --package cockpit-tools hardened_routing --quiet`

可能文件：

- `src-tauri/src/modules/codex_local_access.rs`

依赖：HLA-06。

### HLA-08 状态面板与手动恢复

描述：UI 展示 API service 健康状态，并提供手动恢复/暂停，不展示敏感内容。

状态：2026-05-18 已完成只读可观测性与手动恢复切片：`CodexLocalAccessState.health` 暴露脱敏 health summary，包含 healthy/cooling/auth/manual/model cooldown 计数、sticky account hash、最近错误类型和最近 cooldown 到期时间；API 服务面板展示这些字段。`codex_local_access_recover_health` 仅在用户显式点击时本地清理单账号或当前模型 cooldown，写入脱敏 audit event，不发起上游请求、不刷新额度、不展示账号 ID/邮箱。手动暂停仍留到后续切片。

验收：

- [x] 展示当前 sticky account hash。
- [x] 展示 healthy/cooling/auth_failed/manual_required 数量。
- [x] 展示最近错误类型和 cooldown 到期时间。
- [x] 可手动恢复单账号或单模型 cooldown。
- [x] health/recovery 面板不展示完整邮箱、完整 key、token、prompt、response。

验证：

- [x] health summary 脱敏单测：`cargo test --package cockpit-tools health_summary --quiet`
- [x] 手动恢复单测：`cargo test --package cockpit-tools manual_recovery --quiet`
- [x] `npm run typecheck`
- [ ] 手动 UI smoke。

可能文件：

- `src/components/CodexLocalAccessModal.tsx`
- `src/pages/CodexAccountsPage.tsx`
- `src/types/codexLocalAccess.ts`

依赖：HLA-04、HLA-04A、HLA-07。

### HLA-09 刷新/唤醒降噪

描述：hardened mode 下把配额刷新、quota reset wakeup、startup wakeup 统一纳入低频和显式 opt-in 规则。

状态：2026-05-18 已完成首个前端策略切片：`useAutoRefresh` 不再因 quota reset wakeup 自动把全局刷新间隔改为 2 分钟；Codex 自动刷新不再读取 API service OAuth 账号池做批量探测，只保留 API key 类账号刷新，且超过 50 个目标时跳过；后台唤醒总开关开启前增加风险确认。后续仍需补 backend wakeup/reset 单测或 smoke。

验收：

- [x] 默认不批量刷新 500+ 账号。
- [x] cooldown 账号不通过高频刷新探测恢复。
- [x] quota reset wakeup 不自动把全局刷新间隔调到高频。
- [x] 启用后台唤醒前有清晰风险提示。

验证：

- [ ] wakeup/reset 相关单测或轻量 smoke。
- [ ] `npm run typecheck`

可能文件：

- `src/hooks/useAutoRefresh.ts`
- `src/components/codex/CodexWakeupContent.tsx`
- `src-tauri/src/modules/codex_wakeup.rs`
- `src-tauri/src/modules/codex_wakeup_scheduler.rs`

依赖：HLA-08。

### HLA-10 preset/文档/smoke

描述：把 hardened defaults、balanced self-use、quota drain careful 做成可恢复 preset，并补最终用户文档。

状态：2026-05-18 已完成文档契约切片：新增 `docs/LOCAL_HARDENED_API.md`，写明默认安全姿态、三类 preset 目标值、Codex CLI 直连 Cockpit、可选 LiteLLM 桥接、风险边界和回滚。UI/command 级一键 preset 与直连 smoke 继续保留。

验收：

- [x] `maximum_safety` 展开为单账号、单并发、60 秒间隔、manual fallback。
- [x] `balanced_self_use` 展开为单并发、20-30 秒间隔、sticky then next healthy。
- [x] `quota_drain_careful` 展开为 fill-first、严格 cooldown、低速率。
- [x] 文档写明 Codex CLI 直连 Cockpit 和可选 LiteLLM 桥接两条路径。
- [ ] 关闭 LiteLLM 后 Codex CLI 仍可直连 Cockpit。

验证：

- [ ] `npm run typecheck`
- [ ] `npm run build`
- [ ] 手动 smoke：Codex CLI 使用 `http://127.0.0.1:2876/v1`。

可能文件：

- `docs/LOCAL_HARDENED_API.md`
- `docs/LOCAL_HARDENED_API_ROADMAP.md`
- `docs/reference-gateway-best-practices.md`
- `src/components/CodexLocalAccessModal.tsx`

依赖：HLA-09。

## 推荐执行顺序

AI 推荐：按 `HLA-00 -> HLA-01 -> HLA-02 -> HLA-03 -> HLA-04 -> HLA-05` 先完成 P0 护栏，再进入 `HLA-06` 多账号池。理由：当前代码已经有轮转雏形，但缺 persistent health、header cooldown、semaphore 和 stream guard；先打开多账号会放大 429 连撞和账号风控风险。

## 完成态证据

每个 slice 至少记录：

- `依据`：本计划中的任务 ID 和相关代码路径。
- `命令`：实际运行的 build/test/typecheck。
- `证据`：命令 exit code、关键输出、必要时 UI 截图或日志 tail。
- `回滚`：对应 commit 或可撤销配置。

docs-only 更新可使用：

```powershell
git diff --check
```

代码更新按风险顺序使用：

```powershell
cargo fmt
cargo test --package cockpit-tools codex_local_access --quiet
cargo test --package cockpit-core
npm run typecheck
npm run build
```
