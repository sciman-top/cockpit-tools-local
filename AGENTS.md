# AGENTS.md - Cockpit Tools Local Shared Project Rules / Codex Direct
**项目**: Cockpit Tools Local
**类型**: Tauri 2 desktop app, React 19/Vite frontend, Rust backend
**承接来源**: `GlobalUser/AGENTS.md v9.53`
**适用范围**: 项目级（仓库根）
**最后更新**: 2026-05-29

## 1. 阅读指引
- 本文件是三工具共同项目规则主体；Codex 直接读取，Claude/Gemini 通过各自 wrapper 的 `AGENTS.md` import 承接并只追加平台差异。
- 固定结构：本文件保持 `1 / A / B / C / D`；Claude/Gemini wrapper 保持 `1 / B / D`，并通过 import 承接本文件 `A/C/D`。
- 裁决链：`运行事实/代码 > package.json/src-tauri/Cargo.toml/scripts/reports > 项目级规则 > 全局规则 > 临时上下文`。
- 根文件只保留本仓边界、门禁、阻断、证据和回滚；长 roadmap、release runbook、live probe 报告和参考网关分析放入 `docs/`、`reports/` 或脚本输出。

## A. 项目基线
### A.1 事实边界
- 本仓是本机 Cockpit Tools desktop app；核心职责是账户、provider、OAuth、quota、local hardened API、Tauri shell 与 React UI 的本机管理。
- 本仓已纳入 `D:\CODE\governed-ai-coding-runtime` target catalog，`target_repo_id=cockpit-tools-local`。
- 本项目规则由控制仓 `rules/manifest.json` 管理；目标仓现场修改必须先回写控制仓源文件，再通过同步入口下发。
- 受管治理资产归 `.governed-ai/`、`.claude/`、`.ignore`、`AGENTS.md`、`CLAUDE.md`、`GEMINI.md`；应用代码、release 脚本、Tauri capabilities 和 live config 不由治理同步盲覆盖。

### A.2 执行锚点
- 每轮先检查 `git status --short --branch`，区分用户改动、治理生成文件和本轮改动。
- 默认中文沟通、中文解释、中文汇报；代码标识符、命令、日志、报错、API 名和 schema 字段保留英文原文。
- 维护 Cockpit 时把当前 Codex 与正在运行中的 Cockpit 连续性当作硬护栏：未经当前任务明确确认，不停止、重启、kill、自动拉起、覆盖、替换或原地升级 `Codex App`、`codex`、Cockpit release exe、packaged exe、desktop shortcut 指向目标或 live dev app/server。
- 需要 live UI、packaged exe 或“正在运行的 Cockpit 是否已是最新代码”验证时，先做只读进程、路径、hash、端口或配置探针；若仍需启动、重启、替换、覆盖、安装或单实例唤醒 Cockpit，先说明确切命令、原因、端口/窗口/单实例唤醒/资源占用/Codex 与 Cockpit 连续性影响，并询问：`是否允许我现在执行 <command> 进行实时验证？`
- Codex API service、routing、quota、pool scheduling 或 failover 变更前，优先查本仓 `docs/reference-gateway-best-practices.md` 和 `D:\CODE\external\_reference_gateway_sources`；外部参考只作结构证据，不覆盖本仓运行事实。

<!-- auto-merged target-local additions -->
- Codex API service routing、quota continuity、pool scheduling, sorting, or risk-reduction changes 必须优先对照本机参考源；Official `openai-codex` source is the highest reference。若参考源陈旧，只允许非破坏性 `git fetch --prune` / `pull --ff-only` 刷新后再用作证据。
### A.3 N/A 分类与字段
- `platform_na`：平台能力缺失、命令不存在或非交互限制导致命令不可用。
- `gate_na`：门禁步骤客观不可执行（含脚本缺失、当前任务纯文档/注释/排版、live dev/build 未获授权）。
- 两类 N/A 均必须记录：`reason`、`alternative_verification`、`evidence_link`、`expires_at`。
- N/A 不得改变门禁顺序：`build -> test -> contract/invariant -> hotspot`。

## B. Codex 平台差异
- Codex 直接读取本文件；不要假定 Codex 会自动读取 `CLAUDE.md`、`GEMINI.md` 或未配置的 fallback 文件。
- 规则变更后用新 Codex run/session 复核，不假定当前会话热加载。
- 诊断优先：`codex --version`、`codex --help`；加载链可疑时新会话询问已加载规则来源，并记录 `active_rule_path`。
- `AGENTS.md` 是上下文规则；危险命令、权限、sandbox 和重复 allowlist 应落到 `.codex/rules/*.rules`、控制仓门禁、hooks 或 CI。
- 修改 auth/provider/API service/live config 前先区分登录链路、执行权限、模型能力和仓库代码问题；先做文件级状态、dry-run、isolated probe 和 hash/进程稳定性证据。

## C. 项目差异
### C.1 模块边界
- `src/`：React UI、hooks、types、utilities、styles、assets、i18n locales。
- `src-tauri/`：Tauri shell、commands、capabilities、platform integration 和 Rust app crate。
- `crates/cockpit-core/`：accounts、providers、OAuth、quotas、config、persistence 的共享 Rust 逻辑。
- `crates/cockpit-cli/`：CLI entrypoint。
- `scripts/`：version sync、locale checks、release preflight、local hardened API smoke/acceptance、checksums 和 publishing helpers。
- `docs/`、`reports/`、`public/`、`Casks/`：文档、验证报告、静态资源和 release metadata。
- 不编辑生成输出：`dist/`、`target/`、`target-test/`、`target-codex-verify/`、`target-codex-tauri-*`、`node_modules/`。

### C.2 门禁命令与顺序
- fixed order：`build -> test -> contract/invariant -> hotspot`。
- build：`npm run build`
- test：`cargo test --manifest-path src-tauri/Cargo.toml --lib`
- contract/invariant：`node scripts/release/preflight.cjs --skip-typecheck --skip-build --skip-cargo --skip-cargo-test`
- hotspot：按本轮影响面复核 `SECURITY.md`、`src-tauri/capabilities/`、release scripts、local hardened API risk guards、quota/cooldown/pool routing、i18n 文案和 live session continuity；无法执行 live smoke 时按 `gate_na` 写替代验证。

### C.3 快速反馈边界
- quick test：`npm run typecheck`
- quick contract：`node scripts/release/preflight.cjs --skip-typecheck --skip-build --skip-cargo --skip-cargo-test`
- 控制仓 daily quick：`pwsh -NoProfile -ExecutionPolicy Bypass -File D:\CODE\governed-ai-coding-runtime\scripts\runtime-flow-preset.ps1 -Target cockpit-tools-local -FlowMode daily -Mode quick -Json`
- quick 只证明 TypeScript 静态契约、locale completeness 和 local hardened API live-risk guard；不能替代 full build、Rust lib tests、release preflight 或 live UI smoke。

### C.4 失败分流与阻断
- build/typecheck/Rust test 失败：阻断，先修回归再继续。
- `SECURITY.md`、Tauri capabilities、updater/signing、release scripts、auth/provider/local API service、quota routing 和 live config 属高影响面；发布或持久化前必须有回滚路径和验证证据。
- live upstream quota probe 默认不跑；只有当前任务明确要求并带 `-AcknowledgeLiveUpstreamRisk` 时才可执行。
- Fallback continuity probe 默认使用 app-safe isolated/bypass 路径；不得为了 smoke 修改当前 active Codex CLI provider 或自动切换账号池。

<!-- auto-merged target-local additions -->
- 超过默认 drain 请求量、把 drain 间隔降到 20 秒以下，或扩大真实上游消耗范围时，还必须同时带 `-AcknowledgeExpandedLiveUpstreamRisk` 并在报告中解释为什么缓存/静态 reset 数据不足。
- Cooldown recovery must be inferred from stored reset times/health registry by default；不得通过重复刷新/polling 冷却恢复来消耗账号池。
### C.5 证据与回滚
- 治理接入证据落 `D:\CODE\governed-ai-coding-runtime\docs\change-evidence\`；本仓验证报告优先落 `reports/`。
- 最低字段：规则 ID、风险等级、执行命令、关键输出、兼容性判断、回滚动作。
- 默认回滚优先 Git；live config、account/provider、release exe、updater/signing 或本机 state 变更必须额外记录备份路径、restore 命令和进程/会话连续性复测。

## D. 维护校验清单
- 仅落地本仓事实，不复述全局规则正文。
- 协同链完整：`规则 -> 落点 -> 命令 -> 证据 -> 回滚`。
- 仅凭全局 + 本项目规则，必须能推出当前落点、目标归宿、门禁顺序、证据路径和回滚入口。
- `Global Rule -> Repo Action`：
  - `R1`: 每轮声明 `目标归宿 -> 当前落点 -> 验证方式`，尤其区分控制仓治理、目标仓应用代码和 live runtime state。
  - `R2`: 用 quick/full gate、isolated probe 和报告小步闭环。
  - `R3`: auth/provider/quota/routing 问题先追配置、projection、gateway、scheduler 和 runtime evidence 链。
  - `R4`: live dev/build、release exe、provider/auth/API service 和 upstream quota probe 按中高风险处理。
  - `R5`: 不为一次症状引入新 router 或守护进程；先用现有 core、scripts、preflight 和 reports。
  - `R6`: C.2 门禁顺序不可绕过；quick 只能作日常反馈。
  - `R7`: 不破坏账号数据、provider config、Tauri capabilities、updater/signing、locale keys、API service contract 和 backward compatibility。
  - `R8`: 每次变更必须留下命令、关键输出、证据路径和回滚动作。
  - `E4`: `npm run build`、Rust lib tests、release preflight、risk guard 和 reports 承接健康指标。
  - `E5`: npm/Cargo/Tauri/updater/signing/provider 变化必须记录供应链和发布风险。
  - `E6`: account/provider/config/report schema、quota health registry、updater metadata 和 persisted state 结构变化必须记录迁移、兼容和回滚。
- 本文件属于控制仓 manifest 管理；目标仓现场修改必须回写控制仓源文件后同步。
- 三工具协同约束：`AGENTS.md` 承载共同 A/C/D 项目事实；`CLAUDE.md` / `GEMINI.md` 通过 import 追加 B/D 平台差异，不复制共同正文。
