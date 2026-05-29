use crate::models::codex::{CodexAccount, CodexApiProviderMode, CodexQuota, CodexQuotaErrorInfo};
use crate::models::codex_local_access::{
    CodexLocalAccessAccountHealth, CodexLocalAccessAccountHealthStatus,
    CodexLocalAccessAccountHealthView, CodexLocalAccessAccountStats, CodexLocalAccessCollection,
    CodexLocalAccessConcurrencyDiagnostics, CodexLocalAccessGlobalError,
    CodexLocalAccessHealthRegistry, CodexLocalAccessHealthSummary, CodexLocalAccessModelCooldown,
    CodexLocalAccessPortCleanupResult, CodexLocalAccessRoutingStrategy, CodexLocalAccessState,
    CodexLocalAccessStats, CodexLocalAccessStatsWindow, CodexLocalAccessStickyBinding,
    CodexLocalAccessUsageEvent, CodexLocalAccessUsageStats, CodexLocalApiFallbackMode,
    CodexLocalApiSafetyConfig, CodexLocalApiSafetyPresetId, CodexRuntimeAccountKind,
    CodexRuntimeIntegrationMode, CodexRuntimeModeState, CODEX_LOCAL_ACCESS_HEALTH_SCHEMA_VERSION,
    CODEX_LOCAL_API_SAFETY_SCHEMA_VERSION,
};
use crate::modules::atomic_write::write_string_atomic;
use crate::modules::{codex_account, codex_oauth, codex_wakeup, logger, process};
use base64::{engine::general_purpose, Engine as _};
use futures_util::StreamExt;
use rand::{distributions::Alphanumeric, Rng};
use reqwest::header::{
    HeaderMap, HeaderName, HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_TYPE, RETRY_AFTER,
    USER_AGENT,
};
use reqwest::{Client, Method, NoProxy, Proxy, StatusCode, Url};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Write;
use std::net::{Ipv4Addr, TcpListener as StdTcpListener};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Instant, SystemTime};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{watch, Mutex as TokioMutex};
use tokio::time::{timeout, Duration};

const CODEX_LOCAL_ACCESS_FILE: &str = "codex_local_access.json";
const CODEX_LOCAL_ACCESS_STATS_FILE: &str = "codex_local_access_stats.json";
const CODEX_LOCAL_ACCESS_HEALTH_FILE: &str = "codex_local_access_health.json";
const CODEX_LOCAL_ACCESS_AUDIT_FILE: &str = "codex_local_access_audit.jsonl";
const CODEX_RUNTIME_MODE_FILE: &str = "codex_runtime_mode.json";
const CODEX_LOCAL_ACCESS_DATA_ROOT_ENV: &str = "COCKPIT_LOCAL_ACCESS_DATA_ROOT";
const CODEX_LOCAL_ACCESS_BIND_HOST: &str = "127.0.0.1";
const CODEX_LOCAL_ACCESS_URL_HOST: &str = "127.0.0.1";
const LITELLM_GATEWAY_HEALTH_TIMEOUT: Duration = Duration::from_secs(3);
// Internal hard cap; HLA safety config is clamped to this value.
const MAX_HTTP_REQUEST_BYTES: usize = 64 * 1024 * 1024;
const REQUEST_READ_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_INLINE_ACCOUNT_RETRY_WAIT: Duration = Duration::from_secs(3);
const MAX_POOL_UNAVAILABLE_PRE_ADMISSION_WAIT: Duration = Duration::from_secs(3);
const UPSTREAM_SEND_RETRY_ATTEMPTS: usize = 3;
const UPSTREAM_SEND_RETRY_BASE_DELAY: Duration = Duration::from_millis(200);
const UPSTREAM_SEND_RETRY_MAX_DELAY: Duration = Duration::from_millis(1200);
const SINGLE_ACCOUNT_STATUS_RETRY_BASE_DELAY: Duration = Duration::from_millis(300);
const SINGLE_ACCOUNT_STATUS_RETRY_MAX_DELAY: Duration = Duration::from_millis(1500);
const STATS_FLUSH_INTERVAL: Duration = Duration::from_secs(1);
const MAX_RETRY_CREDENTIALS_PER_REQUEST: usize = 24;
const RESPONSE_AFFINITY_TTL_MS: i64 = 24 * 60 * 60 * 1000;
const MAX_RESPONSE_AFFINITY_BINDINGS: usize = 4096;
const REQUEST_AFFINITY_TTL_MS: i64 = 60 * 60 * 1000;
const MAX_REQUEST_AFFINITY_BINDINGS: usize = 4096;
const REQUEST_AFFINITY_BINDING_REASON: &str = "codex_turn_affinity";
const PROCESS_STICKY_BINDING_KEY: &str = "process";
const PROCESS_STICKY_BINDING_REASON: &str = "sticky_process";
const PROCESS_STICKY_BINDING_TTL_MS: i64 = 24 * 60 * 60 * 1000;
const PREPARED_ACCOUNT_CACHE_TTL_MS: i64 = 30 * 1000;
const DAY_WINDOW_MS: i64 = 24 * 60 * 60 * 1000;
const WEEK_WINDOW_MS: i64 = 7 * DAY_WINDOW_MS;
const MONTH_WINDOW_MS: i64 = 30 * DAY_WINDOW_MS;
const GATEWAY_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);
const DEFAULT_UNKNOWN_RATE_LIMIT_COOLDOWN: Duration = Duration::from_secs(60);
const CODEX_LOCAL_ACCESS_AUDIT_SCHEMA_VERSION: u32 = 1;
const CODEX_LOCAL_ACCESS_AUDIT_MAX_BYTES: usize = 2 * 1024 * 1024;
const CONCURRENCY_DIAGNOSTICS_AUDIT_WINDOW_MS: i64 = 10 * 60 * 1000;
const RUNTIME_PROJECTION_RECENT_AUDIT_GRACE_MS: u128 = 5 * 60 * 1000;
const UPSTREAM_CODEX_BASE_URL: &str = "https://chatgpt.com/backend-api/codex";
const DEFAULT_CODEX_USER_AGENT: &str =
    "codex-tui/0.118.0 (Mac OS 26.3.1; arm64) iTerm.app/3.6.9 (codex-tui; 0.118.0)";
const DEFAULT_CODEX_ORIGINATOR: &str = "codex-tui";
const LEGACY_DEFAULT_CODEX_LOCAL_ACCESS_PORT: u16 = 5335;
const PREFERRED_CODEX_LOCAL_ACCESS_PORTS: &[u16] =
    &[45335, 45336, 45435, 45436, 45535, 45536, 46335, 47335];
const CODEX_LOCAL_ACCESS_RUNTIME_PROVIDER_ID: &str = "codex_local_access";
const CODEX_LOCAL_ACCESS_RUNTIME_PROVIDER_NAME: &str = "Cockpit API Service";
const CORS_ALLOW_HEADERS: &str = "Authorization, Content-Type, OpenAI-Beta, X-API-Key, X-Codex-Beta-Features, X-Codex-Turn-State, X-Codex-Turn-Metadata, X-Client-Request-Id, Originator, Session_id, ChatGPT-Account-Id";
const DEFAULT_CODEX_MODELS: &[&str] = &[
    "gpt-5.5",
    "gpt-5.5-mini",
    "gpt-5-codex",
    "gpt-5-codex-mini",
    "gpt-5.4",
    "gpt-5.4-mini",
    "gpt-5.3-codex",
    "gpt-5.3-codex-spark",
    "gpt-5.2",
    "gpt-5.2-codex",
    "gpt-5.1-codex-max",
    "gpt-5.1-codex-mini",
];
const CODEX_IMAGE_MODEL_ID: &str = "gpt-image-2";
const DEFAULT_IMAGES_MAIN_MODEL: &str = "gpt-5.4-mini";
const CHAT_COMPLETIONS_PATH: &str = "/v1/chat/completions";
const RESPONSES_PATH: &str = "/v1/responses";
const IMAGES_GENERATIONS_PATH: &str = "/v1/images/generations";
const IMAGES_EDITS_PATH: &str = "/v1/images/edits";
const X_CODEX_TURN_STATE_HEADER: &str = "x-codex-turn-state";
const X_CODEX_TURN_METADATA_HEADER: &str = "x-codex-turn-metadata";
const LOCAL_CODEX_TURN_STATE_PREFIX: &str = "cockpit-turn-";
static GATEWAY_RUNTIME: OnceLock<TokioMutex<GatewayRuntime>> = OnceLock::new();
static GATEWAY_ROUND_ROBIN_CURSOR: AtomicUsize = AtomicUsize::new(0);
static GATEWAY_REQUEST_ID_COUNTER: AtomicU64 = AtomicU64::new(0);
static UPSTREAM_HTTP_CLIENT: OnceLock<Mutex<Option<CachedUpstreamHttpClient>>> = OnceLock::new();
static LOCAL_API_BACKPRESSURE_STATE: OnceLock<Mutex<LocalApiBackpressureState>> = OnceLock::new();
static LOCAL_API_BACKPRESSURE_ADMISSION_QUEUE: OnceLock<TokioMutex<()>> = OnceLock::new();
static ACTIVE_STREAM_LEASE_REGISTRY: OnceLock<Mutex<ActiveStreamLeaseRegistry>> = OnceLock::new();
static AUDIT_TRAIL_STATUS: OnceLock<Mutex<AuditTrailStatus>> = OnceLock::new();
static AUDIT_TRAIL_APPEND_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

#[derive(Default)]
struct GatewayRuntime {
    loaded: bool,
    collection: Option<CodexLocalAccessCollection>,
    stats: CodexLocalAccessStats,
    stats_dirty: bool,
    stats_flush_inflight: bool,
    response_affinity: HashMap<String, ResponseAffinityBinding>,
    request_affinity: HashMap<String, ResponseAffinityBinding>,
    model_cooldowns: HashMap<String, AccountModelCooldown>,
    prepared_accounts: HashMap<String, CachedPreparedAccount>,
    running: bool,
    actual_port: Option<u16>,
    last_error: Option<String>,
    shutdown_sender: Option<watch::Sender<bool>>,
    task: Option<tokio::task::JoinHandle<()>>,
}

#[derive(Debug, Clone, Default)]
struct UsageCapture {
    input_tokens: u64,
    output_tokens: u64,
    total_tokens: u64,
    cached_tokens: u64,
    reasoning_tokens: u64,
}

#[derive(Debug, Clone, Default)]
struct ResponseCapture {
    usage: Option<UsageCapture>,
    response_id: Option<String>,
    response_completed_seen: bool,
    compaction_summary_seen: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ResponseHeaderValue {
    name: &'static str,
    value: String,
}

#[derive(Debug, Clone, Default)]
struct ImageCallResult {
    result: String,
    revised_prompt: String,
    output_format: String,
    size: String,
    background: String,
    quality: String,
}

#[derive(Debug, Clone)]
struct MultipartFilePart {
    name: String,
    content_type: String,
    data: Vec<u8>,
}

#[derive(Debug, Clone, Default)]
struct MultipartFormData {
    fields: HashMap<String, String>,
    files: Vec<MultipartFilePart>,
}

#[derive(Debug, Clone)]
struct ResponseAffinityBinding {
    account_id: String,
    updated_at_ms: i64,
}

#[derive(Debug, Clone)]
struct AccountModelCooldown {
    next_retry_at_ms: i64,
}

#[derive(Debug, Clone)]
struct CachedPreparedAccount {
    account: CodexAccount,
    cached_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct UpstreamHttpClientSignature {
    proxy_url: Option<String>,
    no_proxy: Option<String>,
}

#[derive(Clone)]
struct CachedUpstreamHttpClient {
    signature: UpstreamHttpClientSignature,
    client: Client,
}

#[derive(Debug)]
struct ProxyDispatchSuccess {
    upstream: reqwest::Response,
    account_id: String,
    account_email: String,
}

#[derive(Debug)]
struct ProxyDispatchError {
    status: u16,
    message: String,
    account_id: Option<String>,
    account_email: Option<String>,
    retry_after: Option<Duration>,
    defer_until_pool_available: bool,
}

#[derive(Debug, Clone, Default)]
struct LocalApiBackpressureState {
    active_requests: u32,
    last_started_at: Option<Instant>,
}

#[derive(Debug)]
struct LocalApiBackpressurePermit {
    released: bool,
}

#[derive(Debug, Default)]
struct ActiveStreamLeaseRegistry {
    next_lease_id: u64,
    leases: BTreeMap<u64, ActiveStreamLeaseRecord>,
}

#[derive(Clone, Debug, Default)]
struct AuditTrailStatus {
    degraded: bool,
    error: Option<String>,
    degraded_at_ms: Option<i64>,
}

#[derive(Debug, Default)]
struct ConcurrencyAuditRollup {
    recent_audit_event_count: usize,
    recent_request_count: usize,
    recent_local_backpressure_count: usize,
    recent_pool_wait_count: usize,
    recent_upstream_limit_count: usize,
    recent_stream_error_count: usize,
    last_problem_at_ms: Option<i64>,
    last_problem_kind: Option<String>,
    audit_load_error: Option<String>,
}

#[derive(Debug, Clone)]
struct ActiveStreamLeaseRecord {
    account_id: String,
    request_affinity_key: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ActiveStreamTerminal {
    Completed,
    StreamError,
    ClientAborted,
    Dropped,
}

#[derive(Debug)]
struct ActiveStreamLease {
    lease_id: u64,
    context: AuditContext,
    released: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct RuntimeProjectionChangeOptions {
    pub force: bool,
    pub source: &'static str,
}

impl RuntimeProjectionChangeOptions {
    pub const fn new(source: &'static str, force: bool) -> Self {
        Self { force, source }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct RuntimeProjectionContinuityRisk {
    active_stream_count: usize,
    codex_app_process_count: usize,
    recent_audit_activity: bool,
    audit_last_modified_age_ms: Option<u128>,
}

impl RuntimeProjectionContinuityRisk {
    fn has_live_continuity_risk(&self) -> bool {
        self.active_stream_count > 0
            || self.codex_app_process_count > 0
            || self.recent_audit_activity
    }

    fn blocking_reasons(&self) -> Vec<&'static str> {
        let mut reasons = Vec::new();
        if self.active_stream_count > 0 {
            reasons.push("active_stream");
        }
        if self.codex_app_process_count > 0 {
            reasons.push("codex_app_running");
        }
        if self.recent_audit_activity {
            reasons.push("recent_audit_activity");
        }
        reasons
    }

    fn audit_detail(&self) -> BTreeMap<String, String> {
        let mut detail = BTreeMap::from([
            (
                "active_stream_count".to_string(),
                self.active_stream_count.to_string(),
            ),
            (
                "codex_app_process_count".to_string(),
                self.codex_app_process_count.to_string(),
            ),
            (
                "recent_audit_activity".to_string(),
                self.recent_audit_activity.to_string(),
            ),
        ]);
        if let Some(age_ms) = self.audit_last_modified_age_ms {
            detail.insert("audit_last_modified_age_ms".to_string(), age_ms.to_string());
        }
        let reasons = self.blocking_reasons();
        if !reasons.is_empty() {
            detail.insert("blocking_reasons".to_string(), reasons.join(","));
        }
        detail
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct StreamWriteState {
    headers_written: bool,
    first_chunk_written: bool,
}

impl StreamWriteState {
    fn mark_headers_written(&mut self) {
        self.headers_written = true;
    }

    fn mark_first_chunk_written(&mut self) {
        self.first_chunk_written = true;
    }

    fn can_attempt_account_fallback(self) -> bool {
        !self.headers_written && !self.first_chunk_written
    }
}

struct ResponseUsageCollector {
    is_stream: bool,
    body: Vec<u8>,
    stream_buffer: Vec<u8>,
    usage: Option<UsageCapture>,
    response_id: Option<String>,
    response_completed_seen: bool,
    compaction_summary_seen: bool,
}

#[derive(Debug)]
struct ParsedRequest {
    method: String,
    target: String,
    headers: HashMap<String, String>,
    body: Vec<u8>,
    gateway_request_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AuditContext {
    request_id: String,
    request_id_source: String,
    route: String,
    model: String,
    account_hash: String,
    gateway_request_id: String,
    turn_lineage_id: Option<String>,
    turn_lineage_source: Option<String>,
    previous_response_id_hash: Option<String>,
    is_continuation: bool,
    is_auto_compact_candidate: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct CodexLocalAccessAuditEvent {
    schema_version: u32,
    timestamp: i64,
    request_id: String,
    phase: String,
    route: String,
    model: String,
    account_hash: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    status: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream_state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    outcome: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    detail: BTreeMap<String, String>,
}

#[derive(Debug, Clone)]
enum GatewayResponseAdapter {
    Passthrough {
        request_is_stream: bool,
    },
    ChatCompletions {
        stream: bool,
        requested_model: String,
        original_request_body: Vec<u8>,
    },
    Images {
        stream: bool,
        response_format: String,
        stream_prefix: String,
    },
}

impl GatewayResponseAdapter {
    fn audit_kind(&self) -> &'static str {
        match self {
            Self::Passthrough { .. } => "passthrough",
            Self::ChatCompletions { .. } => "chat_completions",
            Self::Images { stream_prefix, .. } if stream_prefix == "image_edit" => "images_edit",
            Self::Images { .. } => "images_generations",
        }
    }
}

#[derive(Debug, Clone, Default)]
struct RequestRoutingHint {
    model_key: String,
    previous_response_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum OfficialCodexStickyRoutingBoundary {
    TurnState { affinity_key: String },
    PreviousResponseId { response_id: String },
}

impl OfficialCodexStickyRoutingBoundary {
    fn reason(&self) -> &'static str {
        match self {
            Self::TurnState { .. } => "codex_turn_state",
            Self::PreviousResponseId { .. } => "previous_response_id",
        }
    }
}

#[derive(Debug, Clone)]
struct RoutingCandidate {
    account_id: String,
    plan_rank: Option<i32>,
    remaining_quota: Option<i32>,
    subscription_expiry_ms: Option<i64>,
}

#[derive(Debug, Default)]
struct PoolUnavailableSummary {
    total_count: usize,
    schedulable_count: usize,
    exhausted_count: usize,
    cooling_count: usize,
    model_cooldown_count: usize,
    manual_required_count: usize,
    disabled_count: usize,
    unknown_blocked_count: usize,
    nearest_wait: Option<Duration>,
}

fn gateway_runtime() -> &'static TokioMutex<GatewayRuntime> {
    GATEWAY_RUNTIME.get_or_init(|| TokioMutex::new(GatewayRuntime::default()))
}

fn upstream_http_client_cache() -> &'static Mutex<Option<CachedUpstreamHttpClient>> {
    UPSTREAM_HTTP_CLIENT.get_or_init(|| Mutex::new(None))
}

fn local_api_backpressure_state() -> &'static Mutex<LocalApiBackpressureState> {
    LOCAL_API_BACKPRESSURE_STATE.get_or_init(|| Mutex::new(LocalApiBackpressureState::default()))
}

fn local_api_backpressure_admission_queue() -> &'static TokioMutex<()> {
    LOCAL_API_BACKPRESSURE_ADMISSION_QUEUE.get_or_init(|| TokioMutex::new(()))
}

fn active_stream_lease_registry() -> &'static Mutex<ActiveStreamLeaseRegistry> {
    ACTIVE_STREAM_LEASE_REGISTRY.get_or_init(|| Mutex::new(ActiveStreamLeaseRegistry::default()))
}

fn active_stream_lease_count() -> usize {
    active_stream_lease_registry()
        .lock()
        .map(|registry| registry.leases.len())
        .unwrap_or(0)
}

fn current_local_backpressure_snapshot() -> LocalApiBackpressureState {
    local_api_backpressure_state()
        .lock()
        .map(|state| state.clone())
        .unwrap_or_default()
}

fn local_backpressure_start_interval_remaining(
    state: &LocalApiBackpressureState,
    config: &CodexLocalApiSafetyConfig,
    now: Instant,
) -> Duration {
    if state.active_requests >= config.max_concurrent_requests.max(1) {
        return Duration::from_millis(0);
    }

    let min_interval = Duration::from_secs(config.min_request_interval_seconds);
    if min_interval.is_zero() {
        return Duration::from_millis(0);
    }

    let Some(last_started_at) = state.last_started_at else {
        return Duration::from_millis(0);
    };
    let Some(next_allowed_at) = last_started_at.checked_add(min_interval) else {
        return Duration::from_millis(0);
    };
    next_allowed_at
        .checked_duration_since(now)
        .unwrap_or_else(|| Duration::from_millis(0))
}

fn duration_as_millis_u64(duration: Duration) -> u64 {
    duration.as_millis().min(u64::MAX as u128) as u64
}

fn concurrency_problem_kind(event: &CodexLocalAccessAuditEvent) -> Option<&'static str> {
    let phase = event.phase.to_ascii_lowercase();
    let error_type = event
        .error_type
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase();

    if phase == "local_backpressure" || error_type == "local_backpressure" {
        return Some("local_backpressure");
    }
    if phase == "pool_wait" || error_type == "cockpit_pool_wait" {
        return Some("pool_wait");
    }
    if matches!(
        error_type.as_str(),
        "usage_limit_reached"
            | "upstream_rate_limit"
            | "insufficient_quota"
            | "model_capacity"
            | "rate_limited"
    ) {
        return Some("upstream_limit");
    }
    if phase.contains("stream_error") || error_type.contains("stream_error") {
        return Some("stream_error");
    }
    None
}

fn update_concurrency_audit_rollup(
    rollup: &mut ConcurrencyAuditRollup,
    event: &CodexLocalAccessAuditEvent,
) {
    rollup.recent_audit_event_count = rollup.recent_audit_event_count.saturating_add(1);
    if event.phase == "listener" {
        rollup.recent_request_count = rollup.recent_request_count.saturating_add(1);
    }

    let Some(kind) = concurrency_problem_kind(event) else {
        return;
    };
    match kind {
        "local_backpressure" => {
            rollup.recent_local_backpressure_count =
                rollup.recent_local_backpressure_count.saturating_add(1);
        }
        "pool_wait" => {
            rollup.recent_pool_wait_count = rollup.recent_pool_wait_count.saturating_add(1);
        }
        "upstream_limit" => {
            rollup.recent_upstream_limit_count =
                rollup.recent_upstream_limit_count.saturating_add(1);
        }
        "stream_error" => {
            rollup.recent_stream_error_count = rollup.recent_stream_error_count.saturating_add(1);
        }
        _ => {}
    }

    if rollup
        .last_problem_at_ms
        .map(|timestamp| event.timestamp >= timestamp)
        .unwrap_or(true)
    {
        rollup.last_problem_at_ms = Some(event.timestamp);
        rollup.last_problem_kind = Some(kind.to_string());
    }
}

fn read_concurrency_audit_file(
    path: &Path,
    now: i64,
    audit_window_ms: i64,
    rollup: &mut ConcurrencyAuditRollup,
) {
    if !path.exists() {
        return;
    }

    match path.metadata() {
        Ok(metadata) if metadata.len() as usize > CODEX_LOCAL_ACCESS_AUDIT_MAX_BYTES => {
            if rollup.audit_load_error.is_none() {
                rollup.audit_load_error = Some(format!(
                    "审计日志过大，已跳过 {}",
                    path.file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or("audit")
                ));
            }
            return;
        }
        Err(err) => {
            if rollup.audit_load_error.is_none() {
                rollup.audit_load_error = Some(format!("读取审计日志元数据失败: {}", err));
            }
            return;
        }
        _ => {}
    }

    let content = match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(err) => {
            if rollup.audit_load_error.is_none() {
                rollup.audit_load_error = Some(format!("读取审计日志失败: {}", err));
            }
            return;
        }
    };

    let window_start = now.saturating_sub(audit_window_ms.max(0));
    for (index, line) in content.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let event = match serde_json::from_str::<CodexLocalAccessAuditEvent>(line) {
            Ok(event) => event,
            Err(err) => {
                if rollup.audit_load_error.is_none() {
                    rollup.audit_load_error =
                        Some(format!("解析审计日志第 {} 行失败: {}", index + 1, err));
                }
                continue;
            }
        };
        if event.timestamp < window_start || event.timestamp > now.saturating_add(60_000) {
            continue;
        }
        update_concurrency_audit_rollup(rollup, &event);
    }
}

fn load_concurrency_audit_rollup(now: i64, audit_window_ms: i64) -> ConcurrencyAuditRollup {
    let mut rollup = ConcurrencyAuditRollup::default();
    let path = match local_access_audit_file_path() {
        Ok(path) => path,
        Err(err) => {
            rollup.audit_load_error = Some(err);
            return rollup;
        }
    };

    let rotated_path = audit_rotated_path(&path);
    read_concurrency_audit_file(&rotated_path, now, audit_window_ms, &mut rollup);
    read_concurrency_audit_file(&path, now, audit_window_ms, &mut rollup);
    rollup
}

fn recent_audit_activity() -> (bool, Option<u128>) {
    let Ok(path) = local_access_audit_file_path() else {
        return (false, None);
    };
    let Ok(metadata) = std::fs::metadata(path) else {
        return (false, None);
    };
    let Ok(modified_at) = metadata.modified() else {
        return (false, None);
    };
    let Ok(age) = SystemTime::now().duration_since(modified_at) else {
        return (true, Some(0));
    };
    let age_ms = age.as_millis();
    (
        age_ms <= RUNTIME_PROJECTION_RECENT_AUDIT_GRACE_MS,
        Some(age_ms),
    )
}

fn collect_runtime_projection_continuity_risk() -> RuntimeProjectionContinuityRisk {
    let (recent_audit_activity, audit_last_modified_age_ms) = recent_audit_activity();
    RuntimeProjectionContinuityRisk {
        active_stream_count: active_stream_lease_count(),
        codex_app_process_count: process::collect_codex_process_entries().len(),
        recent_audit_activity,
        audit_last_modified_age_ms,
    }
}

fn should_block_direct_projection_change(
    current_mode: CodexRuntimeIntegrationMode,
    target_mode: CodexRuntimeIntegrationMode,
    force: bool,
    risk: &RuntimeProjectionContinuityRisk,
) -> bool {
    current_mode == CodexRuntimeIntegrationMode::CockpitApiService
        && target_mode == CodexRuntimeIntegrationMode::DirectProjection
        && should_block_runtime_projection_change(current_mode, target_mode, force, risk)
}

fn should_block_runtime_projection_change(
    current_mode: CodexRuntimeIntegrationMode,
    target_mode: CodexRuntimeIntegrationMode,
    force: bool,
    risk: &RuntimeProjectionContinuityRisk,
) -> bool {
    current_mode != target_mode && !force && risk.has_live_continuity_risk()
}

fn audit_trail_status() -> &'static Mutex<AuditTrailStatus> {
    AUDIT_TRAIL_STATUS.get_or_init(|| Mutex::new(AuditTrailStatus::default()))
}

fn audit_trail_append_lock() -> &'static Mutex<()> {
    AUDIT_TRAIL_APPEND_LOCK.get_or_init(|| Mutex::new(()))
}

fn current_audit_trail_status() -> AuditTrailStatus {
    audit_trail_status()
        .lock()
        .map(|status| status.clone())
        .unwrap_or_default()
}

fn mark_audit_trail_degraded(err: &str) {
    if let Ok(mut status) = audit_trail_status().lock() {
        status.degraded = true;
        status.error = Some(safe_log_field(Some(err), 240));
        status.degraded_at_ms = Some(now_ms());
    }
}

fn mark_audit_trail_healthy() {
    if let Ok(mut status) = audit_trail_status().lock() {
        *status = AuditTrailStatus::default();
    }
}

fn apply_audit_trail_status_to_health_summary(
    summary: &mut CodexLocalAccessHealthSummary,
    status: &AuditTrailStatus,
) {
    summary.audit_degraded = status.degraded;
    summary.audit_error = status.error.clone();
    summary.audit_degraded_at_ms = status.degraded_at_ms;
}

impl Drop for LocalApiBackpressurePermit {
    fn drop(&mut self) {
        self.release();
    }
}

impl LocalApiBackpressurePermit {
    fn release(&mut self) {
        if self.released {
            return;
        }
        self.released = true;
        let Ok(mut state) = local_api_backpressure_state().lock() else {
            return;
        };
        state.active_requests = state.active_requests.saturating_sub(1);
    }
}

impl ActiveStreamTerminal {
    fn phase(self) -> &'static str {
        match self {
            Self::Completed => "stream_completed",
            Self::StreamError => "stream_error",
            Self::ClientAborted => "client_aborted",
            Self::Dropped => "stream_error",
        }
    }

    fn outcome(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::StreamError => "error",
            Self::ClientAborted => "aborted",
            Self::Dropped => "dropped",
        }
    }
}

impl ActiveStreamLease {
    fn release(&mut self, terminal: ActiveStreamTerminal) {
        if self.released {
            return;
        }
        self.released = true;
        let active_count = release_active_stream_lease(self.lease_id);
        record_audit_event_from_context(
            &self.context,
            terminal.phase(),
            None,
            None,
            None,
            Some(terminal.outcome()),
            BTreeMap::from([("lease_id".to_string(), self.lease_id.to_string())]),
        );
        record_audit_event_from_context(
            &self.context,
            "lease_released",
            None,
            None,
            None,
            Some(terminal.outcome()),
            BTreeMap::from([
                ("lease_id".to_string(), self.lease_id.to_string()),
                ("active_count".to_string(), active_count.to_string()),
            ]),
        );
    }
}

impl Drop for ActiveStreamLease {
    fn drop(&mut self) {
        self.release(ActiveStreamTerminal::Dropped);
    }
}

fn local_backpressure_wait_duration(
    state: &LocalApiBackpressureState,
    config: &CodexLocalApiSafetyConfig,
    now: Instant,
) -> Option<Duration> {
    let max_concurrent = config.max_concurrent_requests.max(1);
    if state.active_requests >= max_concurrent {
        return Some(Duration::from_millis(50));
    }

    let min_interval = Duration::from_secs(config.min_request_interval_seconds);
    if min_interval.is_zero() {
        return None;
    }

    let last_started_at = state.last_started_at?;
    let next_allowed_at = last_started_at.checked_add(min_interval)?;
    next_allowed_at
        .checked_duration_since(now)
        .filter(|wait| *wait > Duration::from_secs(0))
}

fn local_backpressure_retry_after(config: &CodexLocalApiSafetyConfig) -> Duration {
    Duration::from_secs(
        config
            .min_request_interval_seconds
            .max(1)
            .min(config.max_queue_wait_seconds.max(1)),
    )
}

fn local_backpressure_error(config: &CodexLocalApiSafetyConfig) -> ProxyDispatchError {
    ProxyDispatchError {
        status: StatusCode::TOO_MANY_REQUESTS.as_u16(),
        message: "本地接入队列等待超时，请稍后重试".to_string(),
        account_id: None,
        account_email: None,
        retry_after: Some(local_backpressure_retry_after(config)),
        defer_until_pool_available: false,
    }
}

fn try_acquire_local_api_backpressure(
    config: &CodexLocalApiSafetyConfig,
) -> Result<LocalApiBackpressurePermit, Duration> {
    let mut state = local_api_backpressure_state()
        .lock()
        .map_err(|_| Duration::from_millis(50))?;
    let now = Instant::now();
    if let Some(wait) = local_backpressure_wait_duration(&state, config, now) {
        return Err(wait.min(Duration::from_millis(250)));
    }

    state.active_requests = state.active_requests.saturating_add(1);
    state.last_started_at = Some(now);
    Ok(LocalApiBackpressurePermit { released: false })
}

#[cfg(test)]
async fn acquire_local_api_backpressure(
    config: &CodexLocalApiSafetyConfig,
) -> Result<LocalApiBackpressurePermit, ProxyDispatchError> {
    acquire_local_api_backpressure_with_wait(
        config,
        Duration::from_secs(config.max_queue_wait_seconds.max(1)),
    )
    .await
}

async fn acquire_local_api_backpressure_with_wait(
    config: &CodexLocalApiSafetyConfig,
    queue_wait: Duration,
) -> Result<LocalApiBackpressurePermit, ProxyDispatchError> {
    let _admission_turn = local_api_backpressure_admission_queue().lock().await;
    let queue_wait = queue_wait.max(Duration::from_millis(1));
    timeout(queue_wait, async {
        loop {
            match try_acquire_local_api_backpressure(config) {
                Ok(permit) => return Ok(permit),
                Err(wait) => tokio::time::sleep(wait).await,
            }
        }
    })
    .await
    .map_err(|_| local_backpressure_error(config))?
}

#[cfg(test)]
fn reset_local_api_backpressure_for_tests() {
    if let Ok(mut state) = local_api_backpressure_state().lock() {
        *state = LocalApiBackpressureState::default();
    }
}

#[cfg(test)]
fn active_stream_lease_count_for_account(account_id: &str) -> usize {
    let account_id = account_id.trim();
    if account_id.is_empty() {
        return 0;
    }
    let Ok(registry) = active_stream_lease_registry().lock() else {
        return 0;
    };
    registry
        .leases
        .values()
        .filter(|lease| lease.account_id == account_id)
        .count()
}

#[cfg(test)]
fn active_stream_request_affinity_key_from_context(context: &AuditContext) -> Option<String> {
    let request_id = context.request_id.trim();
    if request_id.is_empty() || request_id == "-" {
        return None;
    }
    health_registry_request_id(Some(request_id))
}

fn active_stream_affinity_account_for_request(request: &ParsedRequest) -> Option<String> {
    let request_key = request_affinity_key(request)?;
    let Ok(registry) = active_stream_lease_registry().lock() else {
        return None;
    };
    registry.leases.iter().rev().find_map(|(_, lease)| {
        (lease.request_affinity_key.as_deref() == Some(request_key.as_str()))
            .then(|| lease.account_id.clone())
    })
}

fn grant_active_stream_lease_with_affinity_key(
    context: &AuditContext,
    account_id: &str,
    request_affinity_key: Option<String>,
) -> ActiveStreamLease {
    let account_id = account_id.trim();
    let (lease_id, active_count) = match active_stream_lease_registry().lock() {
        Ok(mut registry) => {
            registry.next_lease_id = registry.next_lease_id.saturating_add(1).max(1);
            let lease_id = registry.next_lease_id;
            registry.leases.insert(
                lease_id,
                ActiveStreamLeaseRecord {
                    account_id: account_id.to_string(),
                    request_affinity_key,
                },
            );
            (
                lease_id,
                registry
                    .leases
                    .values()
                    .filter(|lease| lease.account_id == account_id)
                    .count(),
            )
        }
        Err(_) => (0, 0),
    };
    record_audit_event_from_context(
        context,
        "lease_granted",
        None,
        None,
        None,
        Some("active"),
        BTreeMap::from([
            ("lease_id".to_string(), lease_id.to_string()),
            ("active_count".to_string(), active_count.to_string()),
        ]),
    );
    ActiveStreamLease {
        lease_id,
        context: context.clone(),
        released: false,
    }
}

#[cfg(test)]
fn grant_active_stream_lease(context: &AuditContext, account_id: &str) -> ActiveStreamLease {
    grant_active_stream_lease_with_affinity_key(
        context,
        account_id,
        active_stream_request_affinity_key_from_context(context),
    )
}

fn grant_active_stream_lease_for_request(
    context: &AuditContext,
    account_id: &str,
    request: &ParsedRequest,
) -> ActiveStreamLease {
    grant_active_stream_lease_with_affinity_key(context, account_id, request_affinity_key(request))
}

fn release_active_stream_lease(lease_id: u64) -> usize {
    if lease_id == 0 {
        return 0;
    }
    let Ok(mut registry) = active_stream_lease_registry().lock() else {
        return 0;
    };
    let account_id = registry
        .leases
        .remove(&lease_id)
        .map(|lease| lease.account_id);
    account_id
        .as_deref()
        .map(|account_id| {
            registry
                .leases
                .values()
                .filter(|lease| lease.account_id == account_id)
                .count()
        })
        .unwrap_or(0)
}

fn classify_active_stream_terminal_error(err: &str) -> ActiveStreamTerminal {
    let lower = err.to_ascii_lowercase();
    if lower.contains("写入")
        || lower.contains("broken pipe")
        || lower.contains("connection reset")
        || lower.contains("connection aborted")
        || lower.contains("early eof")
    {
        ActiveStreamTerminal::ClientAborted
    } else {
        ActiveStreamTerminal::StreamError
    }
}

#[cfg(test)]
fn reset_active_stream_leases_for_tests() {
    if let Ok(mut registry) = active_stream_lease_registry().lock() {
        *registry = ActiveStreamLeaseRegistry::default();
    }
}

fn current_upstream_http_client_signature() -> UpstreamHttpClientSignature {
    let config = crate::modules::config::get_user_config();
    if !config.global_proxy_enabled {
        return UpstreamHttpClientSignature {
            proxy_url: None,
            no_proxy: None,
        };
    }

    let proxy_url = config.global_proxy_url.trim();
    if proxy_url.is_empty() {
        return UpstreamHttpClientSignature {
            proxy_url: None,
            no_proxy: None,
        };
    }

    let no_proxy = config.global_proxy_no_proxy.trim();
    UpstreamHttpClientSignature {
        proxy_url: Some(proxy_url.to_string()),
        no_proxy: (!no_proxy.is_empty()).then(|| no_proxy.to_string()),
    }
}

fn redact_proxy_url_for_log(proxy_url: &str) -> String {
    match Url::parse(proxy_url) {
        Ok(mut url) => {
            if !url.username().is_empty() {
                let _ = url.set_username("redacted");
            }
            if url.password().is_some() {
                let _ = url.set_password(Some("redacted"));
            }
            url.to_string()
        }
        Err(_) => "<invalid>".to_string(),
    }
}

fn build_upstream_http_client(signature: &UpstreamHttpClientSignature) -> Result<Client, String> {
    let mut builder = Client::builder();

    if let Some(proxy_url) = signature.proxy_url.as_deref() {
        let mut proxy =
            Proxy::all(proxy_url).map_err(|e| format!("Codex 本地接入代理地址无效: {}", e))?;
        if let Some(no_proxy) = signature.no_proxy.as_deref() {
            proxy = proxy.no_proxy(NoProxy::from_string(no_proxy));
        }
        builder = builder.proxy(proxy);
    }

    builder
        .build()
        .map_err(|e| format!("创建 Codex 上游 HTTP 客户端失败: {}", e))
}

fn log_upstream_http_client_signature(signature: &UpstreamHttpClientSignature) {
    match signature.proxy_url.as_deref() {
        Some(proxy_url) => logger::log_info(&format!(
            "[CodexLocalAccess] 上游 HTTP 客户端已应用全局代理 proxy_url={} no_proxy={}",
            redact_proxy_url_for_log(proxy_url),
            signature.no_proxy.as_deref().unwrap_or("<empty>")
        )),
        None => logger::log_info("[CodexLocalAccess] 上游 HTTP 客户端使用系统代理配置"),
    }
}

fn upstream_http_client() -> Result<Client, String> {
    let signature = current_upstream_http_client_signature();
    let mut cache = upstream_http_client_cache()
        .lock()
        .map_err(|_| "Codex 上游 HTTP 客户端缓存已损坏".to_string())?;

    if let Some(cached) = cache.as_ref() {
        if cached.signature == signature {
            return Ok(cached.client.clone());
        }
    }

    let client = build_upstream_http_client(&signature)?;
    log_upstream_http_client_signature(&signature);
    *cache = Some(CachedUpstreamHttpClient {
        signature,
        client: client.clone(),
    });
    Ok(client)
}

fn local_access_data_root() -> Result<PathBuf, String> {
    if let Ok(raw) = std::env::var(CODEX_LOCAL_ACCESS_DATA_ROOT_ENV) {
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            let path = PathBuf::from(trimmed);
            std::fs::create_dir_all(&path)
                .map_err(|e| format!("创建本地接入临时数据目录失败: {}", e))?;
            return Ok(path);
        }
    }

    let home = dirs::home_dir().ok_or("Cannot find home directory")?;
    let path = home.join(".antigravity_cockpit");
    std::fs::create_dir_all(&path).map_err(|e| format!("创建本地接入数据目录失败: {}", e))?;
    Ok(path)
}

fn local_access_file_path() -> Result<PathBuf, String> {
    Ok(local_access_data_root()?.join(CODEX_LOCAL_ACCESS_FILE))
}

fn local_access_stats_file_path() -> Result<PathBuf, String> {
    Ok(local_access_data_root()?.join(CODEX_LOCAL_ACCESS_STATS_FILE))
}

fn local_access_health_file_path() -> Result<PathBuf, String> {
    Ok(local_access_data_root()?.join(CODEX_LOCAL_ACCESS_HEALTH_FILE))
}

fn local_access_audit_file_path() -> Result<PathBuf, String> {
    Ok(local_access_data_root()?.join(CODEX_LOCAL_ACCESS_AUDIT_FILE))
}

fn runtime_mode_file_path() -> Result<PathBuf, String> {
    Ok(local_access_data_root()?.join(CODEX_RUNTIME_MODE_FILE))
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn next_gateway_request_id() -> String {
    let sequence = GATEWAY_REQUEST_ID_COUNTER
        .fetch_add(1, Ordering::Relaxed)
        .saturating_add(1);
    format!("gw-{}-{}", now_ms(), sequence)
}

fn runtime_account_identity(account: &CodexAccount) -> (CodexRuntimeAccountKind, Option<String>) {
    let account_kind = if account.is_api_key_auth() {
        CodexRuntimeAccountKind::Api
    } else {
        CodexRuntimeAccountKind::OAuth
    };
    (account_kind, Some(account.id.clone()))
}

fn current_runtime_account_kind() -> (CodexRuntimeAccountKind, Option<String>) {
    let account = codex_account::get_current_account();
    account
        .as_ref()
        .map(runtime_account_identity)
        .unwrap_or((CodexRuntimeAccountKind::Unknown, None))
}

fn runtime_account_kind_for_mode(
    mode: CodexRuntimeIntegrationMode,
) -> (CodexRuntimeAccountKind, Option<String>) {
    if mode == CodexRuntimeIntegrationMode::CockpitApiService {
        if let Ok(Some(collection)) = load_collection_from_disk() {
            if let Ok(account) = resolve_local_access_projection_account(&collection) {
                return runtime_account_identity(&account);
            }
        }
    }
    current_runtime_account_kind()
}

fn build_runtime_mode_state(mode: CodexRuntimeIntegrationMode) -> CodexRuntimeModeState {
    let (account_kind, current_account_id) = runtime_account_kind_for_mode(mode);
    CodexRuntimeModeState {
        mode,
        account_kind,
        current_account_id,
        updated_at: now_ms(),
    }
}

fn save_runtime_mode_state(state: &CodexRuntimeModeState) -> Result<(), String> {
    let path = runtime_mode_file_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("创建 Codex runtime mode 目录失败: {}", e))?;
    }
    let content = serde_json::to_string_pretty(state)
        .map_err(|e| format!("序列化 runtime mode 失败: {}", e))?;
    write_string_atomic(&path, &content).map_err(|e| format!("写入 runtime mode 失败: {}", e))
}

fn should_sync_local_access_collection_on_account_switch(
    _mode: CodexRuntimeIntegrationMode,
    _collection: &CodexLocalAccessCollection,
) -> bool {
    false
}

fn should_restore_direct_projection_before_app_exit(mode: CodexRuntimeIntegrationMode) -> bool {
    mode == CodexRuntimeIntegrationMode::CockpitApiService
}

fn build_projection_seed_local_access_account_ids(
    account_id: &str,
    collection: Option<&CodexLocalAccessCollection>,
) -> Option<Vec<String>> {
    collection.is_none().then(|| vec![account_id.to_string()])
}

fn repair_runtime_projection_history_visibility() -> Result<(), String> {
    let summary =
        crate::modules::codex_session_visibility::repair_session_visibility_across_instances()?;
    logger::log_info(&format!(
        "[CodexRuntimeMode] 历史会话 provider 投影已同步: {}",
        summary.message
    ));
    Ok(())
}

fn repair_runtime_projection_history_visibility_if_needed(source: &str) {
    let mode = load_runtime_mode_state()
        .map(|state| state.mode)
        .unwrap_or(CodexRuntimeIntegrationMode::DirectProjection);
    if mode != CodexRuntimeIntegrationMode::CockpitApiService {
        return;
    }

    if let Err(err) = repair_runtime_projection_history_visibility() {
        logger::log_warn(&format!(
            "[CodexRuntimeMode] {} 后同步历史会话 provider 投影失败: {}",
            source, err
        ));
    }
}

async fn assert_cockpit_local_access_ready(api_key: &str, port: u16) -> Result<(), String> {
    let models_url = format!("http://{CODEX_LOCAL_ACCESS_URL_HOST}:{port}/v1/models");
    let client = Client::builder()
        .timeout(LITELLM_GATEWAY_HEALTH_TIMEOUT)
        .no_proxy()
        .build()
        .map_err(|e| format!("创建 Cockpit local access 健康检查客户端失败: {}", e))?;

    let response = client
        .get(&models_url)
        .bearer_auth(api_key)
        .send()
        .await
        .map_err(|e| {
            format!(
                "Cockpit local access 未就绪，无法访问 {}: {}。已保留 Direct Projection；请先修复本地接入后再切换 API 服务模式。",
                models_url, e
            )
        })?;

    if !response.status().is_success() {
        return Err(format!(
            "Cockpit local access 健康检查失败: {} -> {}。已保留 Direct Projection；请先修复本地接入后再切换 API 服务模式。",
            models_url,
            response.status()
        ));
    }

    Ok(())
}

async fn materialize_cockpit_api_service_projection() -> Result<(), String> {
    ensure_runtime_loaded_without_start().await?;
    let seed_account_ids = {
        let current_account = codex_account::get_current_account();
        let runtime = gateway_runtime().lock().await;
        current_account.and_then(|account| {
            build_projection_seed_local_access_account_ids(&account.id, runtime.collection.as_ref())
        })
    };
    if let Some(account_ids) = seed_account_ids {
        let _ = save_local_access_accounts(account_ids, false).await?;
    }

    let state = snapshot_state().await?;
    let api_key = state
        .collection
        .as_ref()
        .map(|collection| collection.api_key.clone())
        .ok_or_else(|| "API 服务集合尚未创建，无法写入 API 服务投影".to_string())?;
    let collection = state
        .collection
        .as_ref()
        .ok_or_else(|| "API 服务集合尚未创建，无法写入 API 服务投影".to_string())?;
    let projection_account = resolve_local_access_projection_account(collection)?;
    let enabled_state = set_local_access_enabled(true).await?;
    let enabled_collection = enabled_state
        .collection
        .as_ref()
        .ok_or_else(|| "Cockpit local access 集合尚未创建，无法写入 API 服务投影".to_string())?;
    if !enabled_state.running {
        return Err("Cockpit local access 未启动，已保留 Direct Projection；请先修复本地接入后再切换 API 服务模式。".to_string());
    }
    assert_cockpit_local_access_ready(&api_key, enabled_collection.port).await?;
    let base_url = enabled_state
        .base_url
        .clone()
        .unwrap_or_else(|| build_base_url(enabled_collection.port));
    let runtime_account = build_runtime_account(base_url, api_key, &projection_account);
    codex_account::write_account_bundle_to_dir(&codex_account::get_codex_home(), &runtime_account)?;
    crate::modules::codex_instance::update_default_settings(None, None, Some(true), None)?;
    repair_runtime_projection_history_visibility()?;
    Ok(())
}

async fn materialize_direct_projection(
    options: RuntimeProjectionChangeOptions,
) -> Result<(), String> {
    let account = codex_account::get_current_account()
        .or_else(codex_account::get_current_or_fallback_oauth_account)
        .ok_or_else(|| "未找到当前 Codex 账号".to_string())?;
    if let Ok(state) = snapshot_state().await {
        if state.collection.is_some() {
            let _ = set_local_access_enabled_with_options(false, options).await?;
        }
    }
    codex_account::write_account_bundle_to_dir(&codex_account::get_codex_home(), &account)?;
    crate::modules::codex_instance::update_default_settings(None, None, Some(true), None)?;
    repair_runtime_projection_history_visibility()?;
    Ok(())
}

pub fn load_runtime_mode_state() -> Result<CodexRuntimeModeState, String> {
    let path = runtime_mode_file_path()?;
    if !path.exists() {
        let state = build_runtime_mode_state(CodexRuntimeIntegrationMode::DirectProjection);
        save_runtime_mode_state(&state)?;
        return Ok(state);
    }

    let content =
        std::fs::read_to_string(&path).map_err(|e| format!("读取 runtime mode 失败: {}", e))?;
    let mut state: CodexRuntimeModeState =
        serde_json::from_str(&content).map_err(|e| format!("解析 runtime mode 失败: {}", e))?;
    let (account_kind, current_account_id) = runtime_account_kind_for_mode(state.mode);
    if state.account_kind != account_kind || state.current_account_id != current_account_id {
        state.account_kind = account_kind;
        state.current_account_id = current_account_id;
        state.updated_at = now_ms();
        save_runtime_mode_state(&state)?;
    }
    Ok(state)
}

pub async fn set_runtime_integration_mode(
    mode: CodexRuntimeIntegrationMode,
) -> Result<CodexRuntimeModeState, String> {
    set_runtime_integration_mode_with_options(
        mode,
        RuntimeProjectionChangeOptions::new("runtime_mode_set", false),
    )
    .await
}

pub async fn set_runtime_integration_mode_with_options(
    mode: CodexRuntimeIntegrationMode,
    options: RuntimeProjectionChangeOptions,
) -> Result<CodexRuntimeModeState, String> {
    let current_mode = load_runtime_mode_state()
        .map(|state| state.mode)
        .unwrap_or(CodexRuntimeIntegrationMode::DirectProjection);
    let risk = if mode != current_mode {
        Some(collect_runtime_projection_continuity_risk())
    } else {
        None
    };
    if let Some(risk) = risk.as_ref() {
        if should_block_runtime_projection_change(current_mode, mode, options.force, risk) {
            record_runtime_projection_audit_event(
                "runtime_mode_transition",
                "blocked",
                options.source,
                options.force,
                Some(current_mode),
                Some(mode),
                Some(risk),
            );
            let action = match (current_mode, mode) {
                (
                    CodexRuntimeIntegrationMode::DirectProjection,
                    CodexRuntimeIntegrationMode::CockpitApiService,
                ) => "启用 API 服务会替换 Codex auth 投影，可能让旧 Direct API/OAuth 任务把本地 API 服务 token 发到官方端点",
                (
                    CodexRuntimeIntegrationMode::CockpitApiService,
                    CodexRuntimeIntegrationMode::DirectProjection,
                ) => "停用 API 服务会替换 Codex auth 投影，可能断开正在使用本地 provider 的任务",
                _ => "切换 Codex runtime mode 会替换 Codex auth 投影",
            };
            return Err(format!(
                "Codex 连续性保护窗口内已阻止切换（{}）。{}。如确需切换，请在确认没有运行中任务后使用强制切换。",
                risk.blocking_reasons().join(", "),
                action
            ));
        }
    }

    match mode {
        CodexRuntimeIntegrationMode::CockpitApiService => {
            materialize_cockpit_api_service_projection().await?;
        }
        CodexRuntimeIntegrationMode::DirectProjection => {
            materialize_direct_projection(options).await?;
        }
    }

    let state = build_runtime_mode_state(mode);
    save_runtime_mode_state(&state)?;
    record_runtime_projection_audit_event(
        "runtime_mode_transition",
        "changed",
        options.source,
        options.force,
        Some(current_mode),
        Some(mode),
        risk.as_ref(),
    );
    Ok(state)
}

pub async fn prepare_runtime_projection_for_app_exit() -> Result<bool, String> {
    let mode = load_runtime_mode_state()
        .map(|state| state.mode)
        .unwrap_or(CodexRuntimeIntegrationMode::DirectProjection);
    if !should_restore_direct_projection_before_app_exit(mode) {
        return Ok(false);
    }

    let state = set_runtime_integration_mode_with_options(
        CodexRuntimeIntegrationMode::DirectProjection,
        RuntimeProjectionChangeOptions::new("app_exit", false),
    )
    .await?;
    logger::log_info(&format!(
        "[CodexRuntimeMode] 应用退出前已恢复 Direct Projection: mode={:?}",
        state.mode
    ));
    Ok(true)
}

pub async fn sync_runtime_projection_after_account_switch(account_id: &str) -> Result<(), String> {
    let mode = load_runtime_mode_state()
        .map(|state| state.mode)
        .unwrap_or(CodexRuntimeIntegrationMode::DirectProjection);

    match mode {
        CodexRuntimeIntegrationMode::CockpitApiService => {
            sync_local_access_to_current_account_on_switch(account_id).await?;
            materialize_cockpit_api_service_projection().await?;
        }
        CodexRuntimeIntegrationMode::DirectProjection => {
            materialize_direct_projection(RuntimeProjectionChangeOptions::new(
                "account_switch",
                false,
            ))
            .await?;
        }
    }

    let state = build_runtime_mode_state(mode);
    save_runtime_mode_state(&state)?;
    Ok(())
}

fn is_prepared_account_cache_valid(entry: &CachedPreparedAccount, now: i64) -> bool {
    now.saturating_sub(entry.cached_at_ms) <= PREPARED_ACCOUNT_CACHE_TTL_MS
        && (entry.account.is_api_key_auth()
            || !codex_oauth::is_token_expired(&entry.account.tokens.access_token))
}

fn prune_prepared_account_cache(runtime: &mut GatewayRuntime, now: i64) {
    let allowed_account_ids = runtime.collection.as_ref().map(|collection| {
        collection
            .account_ids
            .iter()
            .map(String::as_str)
            .collect::<HashSet<&str>>()
    });

    runtime.prepared_accounts.retain(|account_id, entry| {
        let in_collection = allowed_account_ids
            .as_ref()
            .map(|ids| ids.contains(account_id.as_str()))
            .unwrap_or(true);
        in_collection && is_prepared_account_cache_valid(entry, now)
    });
}

fn sync_runtime_collection(runtime: &mut GatewayRuntime, collection: CodexLocalAccessCollection) {
    runtime.collection = Some(collection);
    runtime.loaded = true;
    runtime.last_error = None;
    prune_prepared_account_cache(runtime, now_ms());
}

async fn cache_prepared_account(account: &CodexAccount) {
    let mut runtime = gateway_runtime().lock().await;
    let now = now_ms();
    prune_prepared_account_cache(&mut runtime, now);
    runtime.prepared_accounts.insert(
        account.id.clone(),
        CachedPreparedAccount {
            account: account.clone(),
            cached_at_ms: now,
        },
    );
}

async fn invalidate_prepared_account(account_id: &str) {
    let mut runtime = gateway_runtime().lock().await;
    runtime.prepared_accounts.remove(account_id);
}

fn try_get_cached_account_for_routing(account_id: &str) -> Option<CodexAccount> {
    let Ok(mut runtime) = gateway_runtime().try_lock() else {
        return None;
    };
    let now = now_ms();
    prune_prepared_account_cache(&mut runtime, now);
    runtime
        .prepared_accounts
        .get(account_id)
        .filter(|entry| is_prepared_account_cache_valid(entry, now))
        .map(|entry| entry.account.clone())
}

async fn get_prepared_account(account_id: &str) -> Result<CodexAccount, String> {
    {
        let mut runtime = gateway_runtime().lock().await;
        let now = now_ms();
        prune_prepared_account_cache(&mut runtime, now);
        if let Some(entry) = runtime.prepared_accounts.get(account_id) {
            if is_prepared_account_cache_valid(entry, now) {
                return Ok(entry.account.clone());
            }
        }
    }

    let account = codex_account::prepare_account_for_injection(account_id).await?;
    cache_prepared_account(&account).await;
    Ok(account)
}

async fn schedule_stats_flush_if_needed() {
    let should_spawn = {
        let mut runtime = gateway_runtime().lock().await;
        if runtime.stats_flush_inflight {
            false
        } else {
            runtime.stats_flush_inflight = true;
            true
        }
    };

    if !should_spawn {
        return;
    }

    tokio::spawn(async move {
        loop {
            tokio::time::sleep(STATS_FLUSH_INTERVAL).await;

            let stats_snapshot = {
                let mut runtime = gateway_runtime().lock().await;
                if !runtime.stats_dirty {
                    runtime.stats_flush_inflight = false;
                    return;
                }
                runtime.stats_dirty = false;
                runtime.stats.clone()
            };

            if let Err(err) = save_stats_to_disk(&stats_snapshot) {
                logger::log_codex_api_warn(&format!(
                    "[CodexLocalAccess] 后台写入请求统计失败: {}",
                    err
                ));
                let mut runtime = gateway_runtime().lock().await;
                runtime.stats_dirty = true;
                runtime.stats_flush_inflight = false;
                return;
            }
        }
    });
}

fn normalize_model_key(model: &str) -> String {
    model.trim().to_ascii_lowercase()
}

fn has_date_snapshot_suffix(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 11
        && bytes[0] == b'-'
        && bytes[5] == b'-'
        && bytes[8] == b'-'
        && bytes
            .iter()
            .enumerate()
            .all(|(index, byte)| matches!(index, 0 | 5 | 8) || byte.is_ascii_digit())
}

fn supported_codex_model_ids() -> Vec<String> {
    let mut seen = HashSet::new();
    let mut model_ids: Vec<String> = codex_wakeup::load_state_for_scheduler()
        .ok()
        .map(|state| {
            state
                .model_presets
                .into_iter()
                .map(|preset| preset.model.trim().to_string())
                .filter(|model| !model.is_empty())
                .filter(|model| seen.insert(model.to_ascii_lowercase()))
                .collect()
        })
        .unwrap_or_default();

    if model_ids.is_empty() {
        model_ids = DEFAULT_CODEX_MODELS
            .iter()
            .map(|model| (*model).to_string())
            .collect();
    }

    let mut seen_model_ids: HashSet<String> = model_ids
        .iter()
        .map(|model| model.trim().to_ascii_lowercase())
        .filter(|model| !model.is_empty())
        .collect();
    if seen_model_ids.insert(CODEX_IMAGE_MODEL_ID.to_string()) {
        model_ids.push(CODEX_IMAGE_MODEL_ID.to_string());
    }

    model_ids
}

fn resolve_supported_model_alias(model: &str) -> String {
    let trimmed = model.trim();
    let normalized = trimmed.to_ascii_lowercase();

    for alias in supported_codex_model_ids() {
        if normalized == alias {
            return alias;
        }

        if let Some(suffix) = normalized.strip_prefix(&alias) {
            if has_date_snapshot_suffix(suffix) {
                return alias;
            }
        }
    }

    trimmed.to_string()
}

fn rewrite_request_model_alias(body: &[u8]) -> Result<Option<Vec<u8>>, String> {
    let Some(mut body_value) = parse_request_body_json(body) else {
        return Ok(None);
    };

    let Some(body_obj) = body_value.as_object_mut() else {
        return Ok(None);
    };
    let Some(model) = body_obj.get("model").and_then(Value::as_str) else {
        return Ok(None);
    };

    let resolved_model = resolve_supported_model_alias(model);
    if resolved_model == model {
        return Ok(None);
    }

    body_obj.insert("model".to_string(), Value::String(resolved_model));
    serde_json::to_vec(&body_value)
        .map(Some)
        .map_err(|e| format!("重写请求 model 失败: {}", e))
}

fn parse_request_body_json(body: &[u8]) -> Option<Value> {
    if body.is_empty() {
        return None;
    }
    serde_json::from_slice::<Value>(body).ok()
}

fn proxy_target_path(target: &str) -> &str {
    target.split('?').next().unwrap_or(target).trim()
}

fn is_images_generations_request(target: &str) -> bool {
    let path = proxy_target_path(target);
    path == IMAGES_GENERATIONS_PATH || path.ends_with("/images/generations")
}

fn is_images_edits_request(target: &str) -> bool {
    let path = proxy_target_path(target);
    path == IMAGES_EDITS_PATH || path.ends_with("/images/edits")
}

fn is_responses_request(target: &str) -> bool {
    let path = proxy_target_path(target);
    path == RESPONSES_PATH || path.ends_with("/responses")
}

fn normalize_image_model_base(model: &str) -> String {
    let mut base_model = model.trim();
    if let Some(index) = base_model.rfind('/') {
        if index < base_model.len().saturating_sub(1) {
            base_model = base_model[index + 1..].trim();
        }
    }
    base_model.to_string()
}

fn normalize_image_response_format(value: Option<&Value>) -> String {
    value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .unwrap_or("b64_json")
        .to_ascii_lowercase()
}

fn validate_image_model(model: &str) -> Result<String, String> {
    let trimmed = model.trim();
    let base_model = normalize_image_model_base(trimmed);
    if base_model == CODEX_IMAGE_MODEL_ID {
        return Ok(CODEX_IMAGE_MODEL_ID.to_string());
    }

    Err(format!(
        "Model {} is not supported on {} or {}. Use {}.",
        if trimmed.is_empty() {
            "<empty>"
        } else {
            trimmed
        },
        IMAGES_GENERATIONS_PATH,
        IMAGES_EDITS_PATH,
        CODEX_IMAGE_MODEL_ID
    ))
}

fn json_string_field<'a>(object: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    object
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn insert_json_string_field(
    target: &mut Map<String, Value>,
    source: &Map<String, Value>,
    key: &str,
) {
    if let Some(value) = json_string_field(source, key) {
        target.insert(key.to_string(), Value::String(value.to_string()));
    }
}

fn insert_json_number_field(
    target: &mut Map<String, Value>,
    source: &Map<String, Value>,
    key: &str,
) {
    if let Some(value) = source.get(key).filter(|item| item.is_number()) {
        target.insert(key.to_string(), value.clone());
    }
}

fn build_image_generation_tool(
    source: &Map<String, Value>,
    action: &str,
    include_edit_fields: bool,
) -> Result<Value, String> {
    let image_model = json_string_field(source, "model").unwrap_or(CODEX_IMAGE_MODEL_ID);
    let canonical_model = validate_image_model(image_model)?;

    let mut tool = Map::new();
    tool.insert(
        "type".to_string(),
        Value::String("image_generation".to_string()),
    );
    tool.insert("action".to_string(), Value::String(action.to_string()));
    tool.insert("model".to_string(), Value::String(canonical_model));

    for key in [
        "size",
        "quality",
        "background",
        "output_format",
        "moderation",
    ] {
        insert_json_string_field(&mut tool, source, key);
    }
    if include_edit_fields {
        insert_json_string_field(&mut tool, source, "input_fidelity");
    }
    for key in ["output_compression", "partial_images"] {
        insert_json_number_field(&mut tool, source, key);
    }

    Ok(Value::Object(tool))
}

fn should_inject_image_generation_tool(model: &str) -> bool {
    let normalized = model.trim().to_ascii_lowercase();
    !normalized.is_empty() && !normalized.ends_with("spark")
}

fn ensure_image_generation_tool_in_object(object: &mut Map<String, Value>) -> bool {
    let model = object.get("model").and_then(Value::as_str).unwrap_or("");
    if !should_inject_image_generation_tool(model) {
        return false;
    }

    let tool = json!({
        "type": "image_generation",
        "output_format": "png",
    });

    match object.get_mut("tools") {
        Some(Value::Array(tools)) => {
            if tools
                .iter()
                .any(|item| item.get("type").and_then(Value::as_str) == Some("image_generation"))
            {
                false
            } else {
                tools.push(tool);
                true
            }
        }
        _ => {
            object.insert("tools".to_string(), Value::Array(vec![tool]));
            true
        }
    }
}

fn build_images_responses_body(prompt: &str, images: &[String], tool: Value) -> Value {
    let mut content = vec![json!({
        "type": "input_text",
        "text": prompt,
    })];
    for image in images {
        let image_url = image.trim();
        if image_url.is_empty() {
            continue;
        }
        content.push(json!({
            "type": "input_image",
            "image_url": image_url,
        }));
    }

    json!({
        "instructions": "",
        "stream": true,
        "reasoning": {
            "effort": "medium",
            "summary": "auto",
        },
        "parallel_tool_calls": true,
        "include": ["reasoning.encrypted_content"],
        "model": DEFAULT_IMAGES_MAIN_MODEL,
        "store": false,
        "tool_choice": {
            "type": "image_generation",
        },
        "input": [{
            "type": "message",
            "role": "user",
            "content": content,
        }],
        "tools": [tool],
    })
}

fn build_images_generation_request(body: &Value) -> Result<(Value, bool, String), String> {
    let request_obj = body
        .as_object()
        .ok_or("images/generations 请求体必须是 JSON 对象".to_string())?;
    let prompt = json_string_field(request_obj, "prompt")
        .ok_or("images/generations 请求缺少 prompt".to_string())?;
    let response_format = normalize_image_response_format(request_obj.get("response_format"));
    let stream = request_obj
        .get("stream")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let tool = build_image_generation_tool(request_obj, "generate", false)?;

    Ok((
        build_images_responses_body(prompt, &[], tool),
        stream,
        response_format,
    ))
}

fn extract_json_edit_images(request_obj: &Map<String, Value>) -> Vec<String> {
    let mut images = Vec::new();

    if let Some(image) = request_obj.get("image").and_then(Value::as_str) {
        let trimmed = image.trim();
        if !trimmed.is_empty() {
            images.push(trimmed.to_string());
        }
    }

    if let Some(image_array) = request_obj.get("images").and_then(Value::as_array) {
        for image in image_array {
            if let Some(url) = image
                .get("image_url")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                images.push(url.to_string());
            } else if let Some(url) = image
                .as_str()
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                images.push(url.to_string());
            }
        }
    }

    images
}

fn build_images_edit_request_from_json(body: &Value) -> Result<(Value, bool, String), String> {
    let request_obj = body
        .as_object()
        .ok_or("images/edits 请求体必须是 JSON 对象".to_string())?;
    let prompt = json_string_field(request_obj, "prompt")
        .ok_or("images/edits 请求缺少 prompt".to_string())?;
    let images = extract_json_edit_images(request_obj);
    if images.is_empty() {
        return Err("images/edits 请求缺少 images[].image_url".to_string());
    }

    let response_format = normalize_image_response_format(request_obj.get("response_format"));
    let stream = request_obj
        .get("stream")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let mut tool = build_image_generation_tool(request_obj, "edit", true)?;
    if let Some(mask_url) = request_obj
        .get("mask")
        .and_then(|mask| mask.get("image_url"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        if let Some(tool_obj) = tool.as_object_mut() {
            tool_obj.insert(
                "input_image_mask".to_string(),
                json!({ "image_url": mask_url }),
            );
        }
    }

    Ok((
        build_images_responses_body(prompt, &images, tool),
        stream,
        response_format,
    ))
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn extract_multipart_boundary(content_type: &str) -> Option<String> {
    content_type.split(';').find_map(|part| {
        let trimmed = part.trim();
        let (name, value) = trimmed.split_once('=')?;
        if !name.trim().eq_ignore_ascii_case("boundary") {
            return None;
        }
        let boundary = value.trim().trim_matches('"').to_string();
        if boundary.is_empty() {
            None
        } else {
            Some(boundary)
        }
    })
}

fn parse_content_disposition_params(value: &str) -> HashMap<String, String> {
    let mut params = HashMap::new();
    for part in value.split(';').skip(1) {
        let Some((name, raw_value)) = part.trim().split_once('=') else {
            continue;
        };
        let key = name.trim().to_ascii_lowercase();
        let value = raw_value.trim().trim_matches('"').to_string();
        if !key.is_empty() {
            params.insert(key, value);
        }
    }
    params
}

fn trim_part_trailing_newline(mut data: &[u8]) -> &[u8] {
    if data.ends_with(b"\r\n") {
        data = &data[..data.len().saturating_sub(2)];
    } else if data.ends_with(b"\n") {
        data = &data[..data.len().saturating_sub(1)];
    }
    data
}

fn parse_multipart_form_data(content_type: &str, body: &[u8]) -> Result<MultipartFormData, String> {
    let boundary = extract_multipart_boundary(content_type)
        .ok_or("multipart/form-data 缺少 boundary".to_string())?;
    let marker = format!("--{}", boundary).into_bytes();
    let mut form = MultipartFormData::default();
    let mut search_from = 0usize;

    loop {
        let Some(marker_index) = find_subslice(&body[search_from..], &marker) else {
            break;
        };
        let marker_start = search_from + marker_index;
        let mut part_start = marker_start + marker.len();

        if body
            .get(part_start..part_start + 2)
            .map(|bytes| bytes == b"--")
            .unwrap_or(false)
        {
            break;
        }
        if body
            .get(part_start..part_start + 2)
            .map(|bytes| bytes == b"\r\n")
            .unwrap_or(false)
        {
            part_start += 2;
        } else if body
            .get(part_start..part_start + 1)
            .map(|bytes| bytes == b"\n")
            .unwrap_or(false)
        {
            part_start += 1;
        }

        let Some(next_marker_offset) = find_subslice(&body[part_start..], &marker) else {
            break;
        };
        let next_marker_start = part_start + next_marker_offset;
        let part = trim_part_trailing_newline(&body[part_start..next_marker_start]);
        search_from = next_marker_start;

        let Some(header_end) = find_header_end(part) else {
            continue;
        };
        let header_text = String::from_utf8_lossy(&part[..header_end]);
        let part_body = &part[header_end..];
        let mut part_name = String::new();
        let mut part_filename = String::new();
        let mut part_content_type = String::new();

        for line in header_text.lines() {
            let Some((name, value)) = line.split_once(':') else {
                continue;
            };
            if name.trim().eq_ignore_ascii_case("content-disposition") {
                let params = parse_content_disposition_params(value);
                part_name = params.get("name").cloned().unwrap_or_default();
                part_filename = params.get("filename").cloned().unwrap_or_default();
            } else if name.trim().eq_ignore_ascii_case("content-type") {
                part_content_type = value.trim().to_string();
            }
        }

        if part_name.is_empty() {
            continue;
        }
        if part_filename.is_empty() {
            let text = String::from_utf8_lossy(part_body).trim().to_string();
            form.fields.insert(part_name, text);
        } else {
            form.files.push(MultipartFilePart {
                name: part_name,
                content_type: part_content_type,
                data: part_body.to_vec(),
            });
        }
    }

    Ok(form)
}

fn detect_image_mime_type(data: &[u8], fallback: &str) -> String {
    let fallback = fallback.trim();
    if !fallback.is_empty() && fallback != "application/octet-stream" {
        return fallback.to_string();
    }
    if data.starts_with(b"\x89PNG\r\n\x1a\n") {
        "image/png".to_string()
    } else if data.starts_with(b"\xff\xd8\xff") {
        "image/jpeg".to_string()
    } else if data.starts_with(b"RIFF")
        && data
            .get(8..12)
            .map(|bytes| bytes == b"WEBP")
            .unwrap_or(false)
    {
        "image/webp".to_string()
    } else if data.starts_with(b"GIF87a") || data.starts_with(b"GIF89a") {
        "image/gif".to_string()
    } else {
        "application/octet-stream".to_string()
    }
}

fn multipart_file_to_data_url(file: &MultipartFilePart) -> String {
    let mime_type = detect_image_mime_type(&file.data, &file.content_type);
    format!(
        "data:{};base64,{}",
        mime_type,
        general_purpose::STANDARD.encode(&file.data)
    )
}

fn multipart_field_value<'a>(form: &'a MultipartFormData, key: &str) -> Option<&'a str> {
    form.fields
        .get(key)
        .map(String::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn multipart_field_bool(form: &MultipartFormData, key: &str, fallback: bool) -> bool {
    match multipart_field_value(form, key)
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "1" | "true" | "yes" | "on" => true,
        "0" | "false" | "no" | "off" => false,
        _ => fallback,
    }
}

fn multipart_field_number(form: &MultipartFormData, key: &str) -> Option<Value> {
    let raw = multipart_field_value(form, key)?;
    raw.parse::<i64>().ok().map(|value| json!(value))
}

fn build_images_edit_request_from_multipart(
    content_type: &str,
    body: &[u8],
) -> Result<(Value, bool, String), String> {
    let form = parse_multipart_form_data(content_type, body)?;
    let prompt =
        multipart_field_value(&form, "prompt").ok_or("images/edits 请求缺少 prompt".to_string())?;
    let image_files: Vec<&MultipartFilePart> = form
        .files
        .iter()
        .filter(|file| file.name == "image" || file.name == "image[]")
        .collect();
    if image_files.is_empty() {
        return Err("images/edits 请求缺少 image".to_string());
    }

    let mut request_obj = Map::new();
    request_obj.insert(
        "model".to_string(),
        Value::String(
            multipart_field_value(&form, "model")
                .unwrap_or(CODEX_IMAGE_MODEL_ID)
                .to_string(),
        ),
    );
    for key in [
        "size",
        "quality",
        "background",
        "output_format",
        "input_fidelity",
        "moderation",
    ] {
        if let Some(value) = multipart_field_value(&form, key) {
            request_obj.insert(key.to_string(), Value::String(value.to_string()));
        }
    }
    for key in ["output_compression", "partial_images"] {
        if let Some(value) = multipart_field_number(&form, key) {
            request_obj.insert(key.to_string(), value);
        }
    }

    let response_format = multipart_field_value(&form, "response_format")
        .unwrap_or("b64_json")
        .to_ascii_lowercase();
    let stream = multipart_field_bool(&form, "stream", false);
    let mut tool = build_image_generation_tool(&request_obj, "edit", true)?;
    if let Some(mask_file) = form.files.iter().find(|file| file.name == "mask") {
        if let Some(tool_obj) = tool.as_object_mut() {
            tool_obj.insert(
                "input_image_mask".to_string(),
                json!({ "image_url": multipart_file_to_data_url(mask_file) }),
            );
        }
    }

    let images: Vec<String> = image_files
        .into_iter()
        .map(multipart_file_to_data_url)
        .collect();

    Ok((
        build_images_responses_body(prompt, &images, tool),
        stream,
        response_format,
    ))
}

fn build_request_routing_hint(request: &ParsedRequest) -> RequestRoutingHint {
    let Some(body) = parse_request_body_json(&request.body) else {
        return RequestRoutingHint::default();
    };

    RequestRoutingHint {
        model_key: body
            .get("model")
            .and_then(Value::as_str)
            .map(resolve_supported_model_alias)
            .map(|model| normalize_model_key(&model))
            .unwrap_or_default(),
        previous_response_id: body
            .get("previous_response_id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string),
    }
}

fn official_codex_turn_state_affinity_key(request: &ParsedRequest) -> Option<String> {
    request_header_value(request, X_CODEX_TURN_STATE_HEADER)
        .and_then(|value| hashed_request_correlation_id(X_CODEX_TURN_STATE_HEADER, value))
}

fn official_codex_sticky_routing_boundary(
    request: &ParsedRequest,
) -> Option<OfficialCodexStickyRoutingBoundary> {
    // Mirrors openai-codex core semantics:
    // - x-codex-turn-state is the per-turn sticky routing token.
    // - previous_response_id binds Responses continuations.
    // - x-codex-turn-metadata is observability lineage only.
    if let Some(affinity_key) = official_codex_turn_state_affinity_key(request) {
        return Some(OfficialCodexStickyRoutingBoundary::TurnState { affinity_key });
    }

    build_request_routing_hint(request)
        .previous_response_id
        .map(|response_id| OfficialCodexStickyRoutingBoundary::PreviousResponseId { response_id })
}

async fn local_backpressure_bypass_reason(request: &ParsedRequest) -> Option<&'static str> {
    if !is_responses_request(&request.target) {
        return None;
    }

    if let Some(boundary) = official_codex_sticky_routing_boundary(request) {
        let reason = boundary.reason();
        match boundary {
            OfficialCodexStickyRoutingBoundary::PreviousResponseId { .. } => {
                return Some(reason);
            }
            OfficialCodexStickyRoutingBoundary::TurnState { .. } => {
                if resolve_request_affinity_account(request).await.is_some() {
                    return Some(reason);
                }
            }
        }
    }

    None
}

fn is_chat_completions_request(target: &str) -> bool {
    let path = target.split('?').next().unwrap_or(target).trim();
    path == CHAT_COMPLETIONS_PATH || path.ends_with("/chat/completions")
}

fn is_responses_completion_event(event_type: &str) -> bool {
    matches!(event_type, "response.completed" | "response.done")
}

fn response_event_type<'a>(event: &'a Value, event_name: Option<&'a str>) -> &'a str {
    event
        .get("type")
        .and_then(Value::as_str)
        .or(event_name)
        .unwrap_or("")
}

fn response_event_is_compaction_summary(event: &Value, event_type: &str) -> bool {
    event_type == "compaction_summary"
        || event
            .get("item")
            .and_then(|item| item.get("type"))
            .and_then(Value::as_str)
            .is_some_and(|value| value == "compaction_summary")
        || event
            .get("output")
            .and_then(Value::as_array)
            .is_some_and(|items| {
                items.iter().any(|item| {
                    item.get("type")
                        .and_then(Value::as_str)
                        .is_some_and(|value| value == "compaction_summary")
                })
            })
}

fn update_response_capture_trace(
    response_capture: &mut ResponseCapture,
    event: &Value,
    event_name: Option<&str>,
) {
    let event_type = response_event_type(event, event_name);
    if is_responses_completion_event(event_type)
        || event
            .get("status")
            .and_then(Value::as_str)
            .is_some_and(|value| value == "completed")
        || event
            .get("response")
            .and_then(|response| response.get("status"))
            .and_then(Value::as_str)
            .is_some_and(|value| value == "completed")
    {
        response_capture.response_completed_seen = true;
    }
    if response_event_is_compaction_summary(event, event_type) {
        response_capture.compaction_summary_seen = true;
    }
}

fn response_text_type_for_role(role: &str) -> &'static str {
    if role.eq_ignore_ascii_case("assistant") {
        "output_text"
    } else {
        "input_text"
    }
}

fn truncate_to_byte_limit(value: &str, limit: usize) -> String {
    if value.len() <= limit {
        return value.to_string();
    }

    let mut end = 0usize;
    for (index, ch) in value.char_indices() {
        let next = index + ch.len_utf8();
        if next > limit {
            break;
        }
        end = next;
    }
    value[..end].to_string()
}

fn shorten_tool_name_if_needed(name: &str) -> String {
    const LIMIT: usize = 64;
    if name.len() <= LIMIT {
        return name.to_string();
    }
    if name.starts_with("mcp__") {
        if let Some(index) = name.rfind("__") {
            if index > 0 {
                let candidate = format!("mcp__{}", &name[index + 2..]);
                return truncate_to_byte_limit(&candidate, LIMIT);
            }
        }
    }
    truncate_to_byte_limit(name, LIMIT)
}

fn build_short_tool_name_map(body: &Value) -> HashMap<String, String> {
    const LIMIT: usize = 64;

    let mut names = Vec::new();
    if let Some(tools) = body.get("tools").and_then(Value::as_array) {
        for tool in tools {
            if tool.get("type").and_then(Value::as_str) != Some("function") {
                continue;
            }
            if let Some(name) = tool
                .get("function")
                .and_then(Value::as_object)
                .and_then(|function| function.get("name"))
                .and_then(Value::as_str)
            {
                names.push(name.to_string());
            }
        }
    }

    let mut used = HashSet::new();
    let mut short_name_map = HashMap::new();
    for name in names {
        let base_candidate = shorten_tool_name_if_needed(&name);
        let unique = if used.insert(base_candidate.clone()) {
            base_candidate
        } else {
            let mut suffix_index = 1usize;
            loop {
                let suffix = format!("_{}", suffix_index);
                let allowed = LIMIT.saturating_sub(suffix.len());
                let candidate = format!(
                    "{}{}",
                    truncate_to_byte_limit(&base_candidate, allowed),
                    suffix
                );
                if used.insert(candidate.clone()) {
                    break candidate;
                }
                suffix_index += 1;
            }
        };
        short_name_map.insert(name, unique);
    }

    short_name_map
}

fn build_reverse_tool_name_map_from_request(
    original_request_body: &[u8],
) -> HashMap<String, String> {
    let Some(body) = parse_request_body_json(original_request_body) else {
        return HashMap::new();
    };

    build_short_tool_name_map(&body)
        .into_iter()
        .map(|(original, shortened)| (shortened, original))
        .collect()
}

fn map_tool_name(name: &str, short_name_map: &HashMap<String, String>) -> String {
    short_name_map
        .get(name)
        .cloned()
        .unwrap_or_else(|| shorten_tool_name_if_needed(name))
}

fn normalize_chat_content_part(part: &Value, role: &str) -> Option<Value> {
    match part {
        Value::String(text) => Some(json!({
            "type": response_text_type_for_role(role),
            "text": text,
        })),
        Value::Object(obj) => {
            let part_type = obj.get("type").and_then(Value::as_str).unwrap_or("");
            match part_type {
                "" | "text" => {
                    let text = obj.get("text").and_then(Value::as_str).unwrap_or("");
                    Some(json!({
                        "type": response_text_type_for_role(role),
                        "text": text,
                    }))
                }
                "image_url" => {
                    if !role.eq_ignore_ascii_case("user") {
                        return None;
                    }
                    let image_url_value = obj.get("image_url")?;
                    match image_url_value {
                        Value::Object(image_url_obj) => {
                            let url = image_url_obj.get("url").and_then(Value::as_str)?;
                            Some(json!({
                                "type": "input_image",
                                "image_url": url,
                            }))
                        }
                        _ => None,
                    }
                }
                "file" => {
                    if !role.eq_ignore_ascii_case("user") {
                        return None;
                    }
                    let file_data = obj
                        .get("file")
                        .and_then(Value::as_object)
                        .and_then(|file| file.get("file_data"))
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    if file_data.is_empty() {
                        return None;
                    }
                    let filename = obj
                        .get("file")
                        .and_then(Value::as_object)
                        .and_then(|file| file.get("filename"))
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    let mut next = Map::new();
                    next.insert("type".to_string(), Value::String("input_file".to_string()));
                    next.insert(
                        "file_data".to_string(),
                        Value::String(file_data.to_string()),
                    );
                    if !filename.is_empty() {
                        next.insert("filename".to_string(), Value::String(filename.to_string()));
                    }
                    Some(Value::Object(next))
                }
                _ => None,
            }
        }
        _ => None,
    }
}

fn normalize_chat_content_parts(content: &Value, role: &str) -> Vec<Value> {
    match content {
        Value::Array(parts) => parts
            .iter()
            .filter_map(|part| normalize_chat_content_part(part, role))
            .collect(),
        other => normalize_chat_content_part(other, role)
            .map(|part| vec![part])
            .unwrap_or_default(),
    }
}

fn normalize_chat_tool_call(
    tool_call: &Value,
    short_name_map: &HashMap<String, String>,
) -> Option<Value> {
    let tool_call_obj = tool_call.as_object()?;
    let tool_type = tool_call_obj
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("function");
    if tool_type != "function" {
        return None;
    }

    let function_obj = tool_call_obj.get("function").and_then(Value::as_object);
    let name = function_obj
        .and_then(|function| function.get("name"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())?;
    let arguments = function_obj
        .and_then(|function| function.get("arguments"))
        .and_then(Value::as_str)
        .unwrap_or("{}");
    let call_id = tool_call_obj
        .get("id")
        .or_else(|| tool_call_obj.get("call_id"))
        .and_then(Value::as_str)
        .unwrap_or("");

    Some(json!({
        "type": "function_call",
        "call_id": call_id,
        "name": map_tool_name(name, short_name_map),
        "arguments": arguments,
    }))
}

fn normalize_chat_tool_calls(
    tool_calls: &Value,
    short_name_map: &HashMap<String, String>,
) -> Vec<Value> {
    tool_calls
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|tool_call| normalize_chat_tool_call(tool_call, short_name_map))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

fn normalize_chat_message_for_responses(
    message_obj: &Map<String, Value>,
    short_name_map: &HashMap<String, String>,
) -> Vec<Value> {
    let role = message_obj
        .get("role")
        .and_then(Value::as_str)
        .unwrap_or("user");

    if role.eq_ignore_ascii_case("tool") {
        let output = message_obj
            .get("content")
            .map(extract_message_content_text)
            .unwrap_or_default();
        let call_id = message_obj
            .get("tool_call_id")
            .and_then(Value::as_str)
            .unwrap_or("");
        return vec![json!({
            "type": "function_call_output",
            "call_id": call_id,
            "output": output,
        })];
    }

    let normalized_content = message_obj
        .get("content")
        .map(|content| normalize_chat_content_parts(content, role))
        .unwrap_or_default();
    let mut items = Vec::new();

    if !normalized_content.is_empty() {
        let mapped_role = if role.eq_ignore_ascii_case("system") {
            "developer"
        } else {
            role
        };
        let next = json!({
            "type": "message",
            "role": mapped_role,
            "content": normalized_content,
        });
        items.push(next);
    }

    if role.eq_ignore_ascii_case("assistant") {
        if let Some(tool_calls) = message_obj.get("tool_calls") {
            items.extend(normalize_chat_tool_calls(tool_calls, short_name_map));
        }
    }

    items
}

fn normalize_chat_messages_for_responses(
    messages: &Value,
    short_name_map: &HashMap<String, String>,
) -> Value {
    let Some(message_items) = messages.as_array() else {
        return messages.clone();
    };

    let mut normalized = Vec::new();
    for item in message_items {
        let Some(message_obj) = item.as_object() else {
            normalized.push(item.clone());
            continue;
        };
        normalized.extend(normalize_chat_message_for_responses(
            message_obj,
            short_name_map,
        ));
    }

    Value::Array(normalized)
}

fn normalize_chat_tool(tool: &Value, short_name_map: &HashMap<String, String>) -> Option<Value> {
    let tool_obj = tool.as_object()?;
    let tool_type = tool_obj
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("function");

    if tool_type != "function" {
        return Some(Value::Object(tool_obj.clone()));
    }

    let function_obj = tool_obj.get("function").and_then(Value::as_object);
    let name = function_obj
        .and_then(|function| function.get("name"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())?;

    let mut normalized = Map::new();
    normalized.insert("type".to_string(), Value::String("function".to_string()));
    normalized.insert(
        "name".to_string(),
        Value::String(map_tool_name(name, short_name_map)),
    );

    if let Some(description) = function_obj.and_then(|function| function.get("description")) {
        normalized.insert("description".to_string(), description.clone());
    }
    if let Some(parameters) = function_obj.and_then(|function| function.get("parameters")) {
        normalized.insert("parameters".to_string(), parameters.clone());
    }

    if let Some(strict) = function_obj
        .and_then(|function| function.get("strict"))
        .and_then(Value::as_bool)
    {
        normalized.insert("strict".to_string(), Value::Bool(strict));
    }

    Some(Value::Object(normalized))
}

fn normalize_chat_tools(tools: &Value, short_name_map: &HashMap<String, String>) -> Value {
    Value::Array(
        tools
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .filter_map(|tool| normalize_chat_tool(tool, short_name_map))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default(),
    )
}

fn normalize_chat_tool_choice(
    tool_choice: &Value,
    short_name_map: &HashMap<String, String>,
) -> Option<Value> {
    if let Some(mode) = tool_choice.as_str() {
        return Some(Value::String(mode.to_string()));
    }

    let Some(choice_obj) = tool_choice.as_object() else {
        return None;
    };
    let choice_type = choice_obj
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("function");
    if choice_type != "function" {
        return Some(Value::Object(choice_obj.clone()));
    }

    let name = choice_obj
        .get("function")
        .and_then(Value::as_object)
        .and_then(|function| function.get("name"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());

    name.map(|name| {
        json!({
            "type": "function",
            "name": map_tool_name(name, short_name_map),
        })
    })
}

fn extract_message_content_text(content: &Value) -> String {
    match content {
        Value::String(raw) => raw.to_string(),
        Value::Array(parts) => {
            let mut text = String::new();
            for part in parts {
                if let Some(part_text) = part.get("text").and_then(Value::as_str) {
                    append_non_empty_text(&mut text, part_text);
                    continue;
                }
                if let Some(part_text) = part.get("content").and_then(Value::as_str) {
                    append_non_empty_text(&mut text, part_text);
                }
            }
            text
        }
        _ => String::new(),
    }
}

fn build_responses_body_from_chat_completions(
    body: &Value,
) -> Result<(Value, bool, String), String> {
    let request_obj = body
        .as_object()
        .ok_or("chat/completions 请求体必须是 JSON 对象".to_string())?;
    let model = request_obj
        .get("model")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(resolve_supported_model_alias)
        .ok_or("chat/completions 请求缺少 model".to_string())?;
    let messages = request_obj
        .get("messages")
        .ok_or("chat/completions 请求缺少 messages".to_string())?;
    let short_name_map = build_short_tool_name_map(body);
    let input = normalize_chat_messages_for_responses(messages, &short_name_map);
    let stream = request_obj
        .get("stream")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let mut responses_obj = Map::new();
    responses_obj.insert("instructions".to_string(), Value::String(String::new()));
    responses_obj.insert("stream".to_string(), Value::Bool(true));
    responses_obj.insert("store".to_string(), Value::Bool(false));
    responses_obj.insert("model".to_string(), Value::String(model.clone()));
    responses_obj.insert("input".to_string(), input);
    responses_obj.insert("parallel_tool_calls".to_string(), Value::Bool(true));
    responses_obj.insert(
        "reasoning".to_string(),
        json!({
            "effort": request_obj
                .get("reasoning_effort")
                .cloned()
                .unwrap_or_else(|| Value::String("medium".to_string())),
            "summary": "auto",
        }),
    );
    responses_obj.insert(
        "include".to_string(),
        Value::Array(vec![Value::String(
            "reasoning.encrypted_content".to_string(),
        )]),
    );

    if let Some(tools) = request_obj.get("tools") {
        responses_obj.insert(
            "tools".to_string(),
            normalize_chat_tools(tools, &short_name_map),
        );
    }

    if let Some(tool_choice) = request_obj.get("tool_choice") {
        if let Some(choice) = normalize_chat_tool_choice(tool_choice, &short_name_map) {
            responses_obj.insert("tool_choice".to_string(), choice);
        }
    }

    let mut text_obj = Map::new();
    if let Some(response_format) = request_obj
        .get("response_format")
        .and_then(Value::as_object)
    {
        match response_format
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("")
        {
            "text" => {
                text_obj.insert("format".to_string(), json!({ "type": "text" }));
            }
            "json_schema" => {
                if let Some(json_schema) = response_format
                    .get("json_schema")
                    .and_then(Value::as_object)
                {
                    let mut format_obj = Map::new();
                    format_obj.insert("type".to_string(), Value::String("json_schema".to_string()));
                    if let Some(name) = json_schema.get("name") {
                        format_obj.insert("name".to_string(), name.clone());
                    }
                    if let Some(strict) = json_schema.get("strict") {
                        format_obj.insert("strict".to_string(), strict.clone());
                    }
                    if let Some(schema) = json_schema.get("schema") {
                        format_obj.insert("schema".to_string(), schema.clone());
                    }
                    text_obj.insert("format".to_string(), Value::Object(format_obj));
                }
            }
            _ => {}
        }
    }
    if let Some(text_value) = request_obj.get("text").and_then(Value::as_object) {
        if let Some(verbosity) = text_value.get("verbosity") {
            text_obj.insert("verbosity".to_string(), verbosity.clone());
        }
    }
    if !text_obj.is_empty() {
        responses_obj.insert("text".to_string(), Value::Object(text_obj));
    }

    ensure_image_generation_tool_in_object(&mut responses_obj);

    Ok((Value::Object(responses_obj), stream, model))
}

fn prepare_gateway_request(
    mut request: ParsedRequest,
) -> Result<(ParsedRequest, GatewayResponseAdapter), String> {
    if is_images_generations_request(&request.target) {
        if !request.method.eq_ignore_ascii_case("POST") {
            return Err("images/generations 仅支持 POST".to_string());
        }
        let body_value = parse_request_body_json(&request.body)
            .ok_or("images/generations 请求体必须是合法 JSON".to_string())?;
        let (responses_body, stream, response_format) =
            build_images_generation_request(&body_value)?;
        request.target = RESPONSES_PATH.to_string();
        request.body = serde_json::to_vec(&responses_body)
            .map_err(|e| format!("序列化 images/generations 请求体失败: {}", e))?;
        request
            .headers
            .insert("accept".to_string(), "text/event-stream".to_string());
        request
            .headers
            .insert("content-type".to_string(), "application/json".to_string());
        return Ok((
            request,
            GatewayResponseAdapter::Images {
                stream,
                response_format,
                stream_prefix: "image_generation".to_string(),
            },
        ));
    }

    if is_images_edits_request(&request.target) {
        if !request.method.eq_ignore_ascii_case("POST") {
            return Err("images/edits 仅支持 POST".to_string());
        }
        let content_type = request
            .headers
            .get("content-type")
            .map(String::as_str)
            .unwrap_or("");
        let content_type_lower = content_type.to_ascii_lowercase();
        let (responses_body, stream, response_format) =
            if content_type_lower.starts_with("multipart/form-data") {
                build_images_edit_request_from_multipart(&content_type, &request.body)?
            } else {
                let body_value = parse_request_body_json(&request.body)
                    .ok_or("images/edits 请求体必须是合法 JSON".to_string())?;
                build_images_edit_request_from_json(&body_value)?
            };
        request.target = RESPONSES_PATH.to_string();
        request.body = serde_json::to_vec(&responses_body)
            .map_err(|e| format!("序列化 images/edits 请求体失败: {}", e))?;
        request
            .headers
            .insert("accept".to_string(), "text/event-stream".to_string());
        request
            .headers
            .insert("content-type".to_string(), "application/json".to_string());
        return Ok((
            request,
            GatewayResponseAdapter::Images {
                stream,
                response_format,
                stream_prefix: "image_edit".to_string(),
            },
        ));
    }

    if !is_chat_completions_request(&request.target) {
        if let Some(rewritten_body) = rewrite_request_model_alias(&request.body)? {
            request.body = rewritten_body;
        }
        if is_responses_request(&request.target) {
            if let Some(mut body_value) = parse_request_body_json(&request.body) {
                if let Some(body_obj) = body_value.as_object_mut() {
                    if ensure_image_generation_tool_in_object(body_obj) {
                        request.body = serde_json::to_vec(&body_value)
                            .map_err(|e| format!("序列化 responses 请求体失败: {}", e))?;
                    }
                }
            }
        }
        let request_is_stream = is_stream_request(&request.headers, &request.body);
        return Ok((
            request,
            GatewayResponseAdapter::Passthrough { request_is_stream },
        ));
    }

    if !request.method.eq_ignore_ascii_case("POST") {
        return Err("chat/completions 仅支持 POST".to_string());
    }

    let body_value = parse_request_body_json(&request.body)
        .ok_or("chat/completions 请求体必须是合法 JSON".to_string())?;
    let original_request_body = request.body.clone();
    let (responses_body, stream, requested_model) =
        build_responses_body_from_chat_completions(&body_value)?;
    request.target = RESPONSES_PATH.to_string();
    request.body = serde_json::to_vec(&responses_body)
        .map_err(|e| format!("序列化 responses 请求体失败: {}", e))?;
    request
        .headers
        .insert("accept".to_string(), "text/event-stream".to_string());
    request
        .headers
        .insert("content-type".to_string(), "application/json".to_string());

    Ok((
        request,
        GatewayResponseAdapter::ChatCompletions {
            stream,
            requested_model,
            original_request_body,
        },
    ))
}

fn response_payload_root(value: &Value) -> &Value {
    value
        .get("response")
        .filter(|item| item.is_object())
        .unwrap_or(value)
}

fn append_non_empty_text(buffer: &mut String, text: &str) {
    if text.trim().is_empty() {
        return;
    }
    buffer.push_str(text);
}

fn extract_output_text_from_response(response_body: &Value) -> String {
    let root = response_payload_root(response_body);
    let mut text = String::new();
    if let Some(output_items) = root.get("output").and_then(Value::as_array) {
        for item in output_items {
            if item.get("type").and_then(Value::as_str) != Some("message") {
                continue;
            }
            if let Some(content) = item.get("content").and_then(Value::as_array) {
                for part in content {
                    if part.get("type").and_then(Value::as_str) != Some("output_text") {
                        continue;
                    }
                    if let Some(part_text) = part.get("text").and_then(Value::as_str) {
                        append_non_empty_text(&mut text, part_text);
                    }
                }
            }
        }
    }
    text
}

fn extract_reasoning_text_from_response(response_body: &Value) -> String {
    let root = response_payload_root(response_body);
    let mut reasoning_text = String::new();
    if let Some(output_items) = root.get("output").and_then(Value::as_array) {
        for item in output_items {
            if item.get("type").and_then(Value::as_str) != Some("reasoning") {
                continue;
            }
            if let Some(summary_items) = item.get("summary").and_then(Value::as_array) {
                for summary_item in summary_items {
                    if summary_item.get("type").and_then(Value::as_str) != Some("summary_text") {
                        continue;
                    }
                    if let Some(text) = summary_item.get("text").and_then(Value::as_str) {
                        append_non_empty_text(&mut reasoning_text, text);
                    }
                }
            }
        }
    }
    reasoning_text
}

fn extract_response_tool_calls(
    response_body: &Value,
    reverse_tool_name_map: &HashMap<String, String>,
) -> Vec<Value> {
    let root = response_payload_root(response_body);
    root.get("output")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    let item_obj = item.as_object()?;
                    if item_obj.get("type").and_then(Value::as_str) != Some("function_call") {
                        return None;
                    }
                    let name = item_obj
                        .get("name")
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|value| !value.is_empty())?;
                    let restored_name = reverse_tool_name_map
                        .get(name)
                        .cloned()
                        .unwrap_or_else(|| name.to_string());
                    let arguments = item_obj
                        .get("arguments")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    let call_id = item_obj
                        .get("call_id")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    Some(json!({
                        "id": call_id,
                        "type": "function",
                        "function": {
                            "name": restored_name,
                            "arguments": arguments,
                        },
                    }))
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

fn build_chat_completion_message(
    response_body: &Value,
    reverse_tool_name_map: &HashMap<String, String>,
) -> Value {
    let content = extract_output_text_from_response(response_body);
    let reasoning_content = extract_reasoning_text_from_response(response_body);
    let tool_calls = extract_response_tool_calls(response_body, reverse_tool_name_map);
    let mut message = Map::new();
    message.insert("role".to_string(), Value::String("assistant".to_string()));
    message.insert("content".to_string(), Value::Null);
    message.insert("reasoning_content".to_string(), Value::Null);
    message.insert("tool_calls".to_string(), Value::Null);

    if !content.is_empty() {
        message.insert("content".to_string(), Value::String(content));
    }
    if !reasoning_content.is_empty() {
        message.insert(
            "reasoning_content".to_string(),
            Value::String(reasoning_content),
        );
    }
    if !tool_calls.is_empty() {
        message.insert("tool_calls".to_string(), Value::Array(tool_calls));
    }

    Value::Object(message)
}

fn resolve_chat_finish_reason(response_body: &Value, has_tool_calls: bool) -> String {
    let root = response_payload_root(response_body);
    if root.get("status").and_then(Value::as_str) == Some("completed") {
        if has_tool_calls {
            "tool_calls".to_string()
        } else {
            "stop".to_string()
        }
    } else {
        "stop".to_string()
    }
}

fn build_chat_completion_payload(
    response_body: &Value,
    requested_model: &str,
    original_request_body: &[u8],
) -> Value {
    let root = response_payload_root(response_body);
    let reverse_tool_name_map = build_reverse_tool_name_map_from_request(original_request_body);
    let id = root
        .get("id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("chatcmpl-local-{}", now_ms()));
    let created = root
        .get("created_at")
        .or_else(|| root.get("created"))
        .and_then(Value::as_i64)
        .unwrap_or_else(|| chrono::Utc::now().timestamp());
    let model = root
        .get("model")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| requested_model.to_string());
    let message = build_chat_completion_message(response_body, &reverse_tool_name_map);
    let has_tool_calls = message
        .get("tool_calls")
        .and_then(Value::as_array)
        .map(|tool_calls| !tool_calls.is_empty())
        .unwrap_or(false);
    let finish_reason = resolve_chat_finish_reason(response_body, has_tool_calls);
    let usage = extract_usage_capture(response_body).unwrap_or_default();

    json!({
        "id": id,
        "object": "chat.completion",
        "created": created,
        "model": model,
        "choices": [{
            "index": 0,
            "message": message,
            "finish_reason": finish_reason,
            "native_finish_reason": finish_reason,
        }],
        "usage": {
            "prompt_tokens": usage.input_tokens,
            "completion_tokens": usage.output_tokens,
            "total_tokens": usage.total_tokens,
            "prompt_tokens_details": {
                "cached_tokens": usage.cached_tokens,
            },
            "completion_tokens_details": {
                "reasoning_tokens": usage.reasoning_tokens,
            },
        },
    })
}

#[derive(Debug, Default)]
struct ChatCompletionStreamState {
    response_id: String,
    created_at: i64,
    model: String,
    function_call_index: i64,
    has_received_arguments_delta: bool,
    has_tool_call_announced: bool,
}

fn push_sse_payload(stream_body: &mut String, payload: Value) {
    stream_body.push_str("data: ");
    stream_body.push_str(
        serde_json::to_string(&payload)
            .unwrap_or_else(|_| "{\"error\":\"failed to encode stream payload\"}".to_string())
            .as_str(),
    );
    stream_body.push_str("\n\n");
}

#[derive(Debug)]
struct ChatCompletionStreamTransformer {
    reverse_tool_name_map: HashMap<String, String>,
    requested_model: String,
    stream_buffer: Vec<u8>,
    state: ChatCompletionStreamState,
    response_capture: ResponseCapture,
}

impl ChatCompletionStreamTransformer {
    fn new(original_request_body: &[u8], requested_model: &str) -> Self {
        Self {
            reverse_tool_name_map: build_reverse_tool_name_map_from_request(original_request_body),
            requested_model: requested_model.to_string(),
            stream_buffer: Vec::new(),
            state: ChatCompletionStreamState {
                model: requested_model.to_string(),
                function_call_index: -1,
                ..Default::default()
            },
            response_capture: ResponseCapture::default(),
        }
    }

    fn feed(&mut self, chunk: &[u8]) -> Vec<u8> {
        if chunk.is_empty() {
            return Vec::new();
        }
        self.stream_buffer.extend_from_slice(chunk);
        self.process_buffer(false)
    }

    fn finish(mut self) -> (Vec<u8>, ResponseCapture) {
        let mut output = self.process_buffer(true);
        output.extend_from_slice(b"data: [DONE]\n\n");
        (output, self.response_capture)
    }

    fn process_buffer(&mut self, flush_tail: bool) -> Vec<u8> {
        let mut stream_body = String::new();

        loop {
            let Some((boundary_index, separator_len)) =
                find_sse_frame_boundary(&self.stream_buffer)
            else {
                break;
            };
            let frame = self.stream_buffer[..boundary_index].to_vec();
            self.stream_buffer.drain(..boundary_index + separator_len);
            self.process_frame(&frame, &mut stream_body);
        }

        if flush_tail && !self.stream_buffer.is_empty() {
            let frame = std::mem::take(&mut self.stream_buffer);
            self.process_frame(&frame, &mut stream_body);
        }

        stream_body.into_bytes()
    }

    fn process_frame(&mut self, frame: &[u8], stream_body: &mut String) {
        if frame.is_empty() {
            return;
        }

        let text = String::from_utf8_lossy(frame);
        let mut event_name: Option<String> = None;
        let mut data_lines = Vec::new();
        for raw_line in text.lines() {
            let line = raw_line.trim();
            if let Some(rest) = line.strip_prefix("event:") {
                let value = rest.trim();
                if !value.is_empty() {
                    event_name = Some(value.to_string());
                }
                continue;
            }
            if let Some(rest) = line.strip_prefix("data:") {
                let payload = rest.trim();
                if !payload.is_empty() {
                    data_lines.push(payload.to_string());
                }
            }
        }

        let payload = if data_lines.is_empty() {
            let trimmed = text.trim();
            if trimmed.is_empty() {
                return;
            }
            trimmed.to_string()
        } else {
            data_lines.join("\n")
        };

        if payload == "[DONE]" {
            return;
        }

        let Ok(event) = serde_json::from_str::<Value>(&payload) else {
            return;
        };

        if let Some(usage) = extract_usage_capture(&event) {
            self.response_capture.usage = Some(usage);
        }
        if self.response_capture.response_id.is_none() {
            self.response_capture.response_id = extract_response_id(&event);
        }

        update_response_capture_trace(&mut self.response_capture, &event, event_name.as_deref());

        let event_type = response_event_type(&event, event_name.as_deref());

        if event_type == "response.created" {
            if let Some(response) = event.get("response").and_then(Value::as_object) {
                self.state.response_id = response
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                self.state.created_at = response
                    .get("created_at")
                    .and_then(Value::as_i64)
                    .unwrap_or_else(|| chrono::Utc::now().timestamp());
                self.state.model = response
                    .get("model")
                    .and_then(Value::as_str)
                    .unwrap_or(self.requested_model.as_str())
                    .to_string();
            }
            if self.response_capture.response_id.is_none() && !self.state.response_id.is_empty() {
                self.response_capture.response_id = Some(self.state.response_id.clone());
            }
            return;
        }

        let mut template = build_chat_chunk_template(&self.state, &self.requested_model, &event);

        match event_type {
            "response.reasoning_summary_text.delta" => {
                if let Some(delta) = event.get("delta").and_then(Value::as_str) {
                    template["choices"][0]["delta"]["role"] =
                        Value::String("assistant".to_string());
                    template["choices"][0]["delta"]["reasoning_content"] =
                        Value::String(delta.to_string());
                    push_sse_payload(stream_body, template);
                }
            }
            "response.reasoning_summary_text.done" => {
                template["choices"][0]["delta"]["role"] = Value::String("assistant".to_string());
                template["choices"][0]["delta"]["reasoning_content"] =
                    Value::String("\n\n".to_string());
                push_sse_payload(stream_body, template);
            }
            "response.output_text.delta" => {
                if let Some(delta) = event.get("delta").and_then(Value::as_str) {
                    template["choices"][0]["delta"]["role"] =
                        Value::String("assistant".to_string());
                    template["choices"][0]["delta"]["content"] = Value::String(delta.to_string());
                    push_sse_payload(stream_body, template);
                }
            }
            "response.output_item.added" => {
                let Some(item) = event.get("item").and_then(Value::as_object) else {
                    return;
                };
                if item.get("type").and_then(Value::as_str) != Some("function_call") {
                    return;
                }

                self.state.function_call_index += 1;
                self.state.has_received_arguments_delta = false;
                self.state.has_tool_call_announced = true;

                let name = item.get("name").and_then(Value::as_str).unwrap_or("");
                let restored_name = self
                    .reverse_tool_name_map
                    .get(name)
                    .cloned()
                    .unwrap_or_else(|| name.to_string());
                template["choices"][0]["delta"]["role"] = Value::String("assistant".to_string());
                template["choices"][0]["delta"]["tool_calls"] = json!([{
                    "index": self.state.function_call_index,
                    "id": item.get("call_id").cloned().unwrap_or(Value::String(String::new())),
                    "type": "function",
                    "function": {
                        "name": restored_name,
                        "arguments": "",
                    }
                }]);
                push_sse_payload(stream_body, template);
            }
            "response.function_call_arguments.delta" => {
                self.state.has_received_arguments_delta = true;
                if let Some(delta) = event.get("delta").and_then(Value::as_str) {
                    template["choices"][0]["delta"]["tool_calls"] = json!([{
                        "index": self.state.function_call_index,
                        "function": {
                            "arguments": delta,
                        }
                    }]);
                    push_sse_payload(stream_body, template);
                }
            }
            "response.function_call_arguments.done" => {
                if self.state.has_received_arguments_delta {
                    return;
                }
                if let Some(arguments) = event.get("arguments").and_then(Value::as_str) {
                    template["choices"][0]["delta"]["tool_calls"] = json!([{
                        "index": self.state.function_call_index,
                        "function": {
                            "arguments": arguments,
                        }
                    }]);
                    push_sse_payload(stream_body, template);
                }
            }
            "response.output_item.done" => {
                let Some(item) = event.get("item").and_then(Value::as_object) else {
                    return;
                };
                if item.get("type").and_then(Value::as_str) != Some("function_call") {
                    return;
                }

                if self.state.has_tool_call_announced {
                    self.state.has_tool_call_announced = false;
                    return;
                }

                self.state.function_call_index += 1;
                let name = item.get("name").and_then(Value::as_str).unwrap_or("");
                let restored_name = self
                    .reverse_tool_name_map
                    .get(name)
                    .cloned()
                    .unwrap_or_else(|| name.to_string());
                template["choices"][0]["delta"]["role"] = Value::String("assistant".to_string());
                template["choices"][0]["delta"]["tool_calls"] = json!([{
                    "index": self.state.function_call_index,
                    "id": item.get("call_id").cloned().unwrap_or(Value::String(String::new())),
                    "type": "function",
                    "function": {
                        "name": restored_name,
                        "arguments": item
                            .get("arguments")
                            .cloned()
                            .unwrap_or(Value::String(String::new())),
                    }
                }]);
                push_sse_payload(stream_body, template);
            }
            event_type if is_responses_completion_event(event_type) => {
                let finish_reason = if self.state.function_call_index >= 0 {
                    "tool_calls"
                } else {
                    "stop"
                };
                template["choices"][0]["finish_reason"] = Value::String(finish_reason.to_string());
                template["choices"][0]["native_finish_reason"] =
                    Value::String(finish_reason.to_string());
                push_sse_payload(stream_body, template);
            }
            _ => {}
        }
    }
}

fn build_chat_chunk_template(
    state: &ChatCompletionStreamState,
    requested_model: &str,
    event: &Value,
) -> Value {
    let model = event
        .get("model")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
        .or_else(|| {
            if state.model.trim().is_empty() {
                None
            } else {
                Some(state.model.clone())
            }
        })
        .unwrap_or_else(|| requested_model.to_string());
    let id = if state.response_id.trim().is_empty() {
        format!("chatcmpl-local-{}", now_ms())
    } else {
        state.response_id.clone()
    };
    let created = if state.created_at > 0 {
        state.created_at
    } else {
        chrono::Utc::now().timestamp()
    };

    let usage = event
        .get("response")
        .and_then(|response| response.get("usage"))
        .cloned()
        .or_else(|| event.get("usage").cloned());

    let mut template = json!({
        "id": id,
        "object": "chat.completion.chunk",
        "created": created,
        "model": model,
        "choices": [{
            "index": 0,
            "delta": {},
            "finish_reason": Value::Null,
            "native_finish_reason": Value::Null,
        }],
    });
    if let Some(usage) = usage {
        let parsed_usage = extract_usage_capture(&json!({ "response": { "usage": usage } }))
            .or_else(|| extract_usage_capture(&json!({ "usage": usage })))
            .unwrap_or_default();
        template["usage"] = json!({
            "prompt_tokens": parsed_usage.input_tokens,
            "completion_tokens": parsed_usage.output_tokens,
            "total_tokens": parsed_usage.total_tokens,
            "prompt_tokens_details": {
                "cached_tokens": parsed_usage.cached_tokens,
            },
            "completion_tokens_details": {
                "reasoning_tokens": parsed_usage.reasoning_tokens,
            },
        });
    }
    template
}

fn build_chat_completion_stream_body(
    upstream_body: &[u8],
    original_request_body: &[u8],
    requested_model: &str,
) -> String {
    let mut transformer =
        ChatCompletionStreamTransformer::new(original_request_body, requested_model);
    let mut stream_body = transformer.feed(upstream_body);
    let (tail, _) = transformer.finish();
    stream_body.extend_from_slice(&tail);
    String::from_utf8(stream_body).unwrap_or_default()
}

fn build_cooldown_key(account_id: &str, model_key: &str) -> Option<String> {
    let account_id = account_id.trim();
    let model_key = model_key.trim();
    if account_id.is_empty() || model_key.is_empty() {
        return None;
    }
    Some(format!("{}\u{1f}{}", account_id, model_key))
}

fn build_ordered_account_ids(
    account_ids: &[String],
    start: usize,
    preferred_account_id: Option<&str>,
) -> Vec<String> {
    if account_ids.is_empty() {
        return Vec::new();
    }

    let mut ordered = Vec::with_capacity(account_ids.len());
    if let Some(preferred) = preferred_account_id {
        if account_ids.iter().any(|account_id| account_id == preferred) {
            ordered.push(preferred.to_string());
        }
    }

    for offset in 0..account_ids.len() {
        let account_id = &account_ids[(start + offset) % account_ids.len()];
        if ordered.iter().any(|value| value == account_id) {
            continue;
        }
        ordered.push(account_id.clone());
    }

    ordered
}

fn next_routing_start_index(collection: &CodexLocalAccessCollection) -> usize {
    if collection.safety_config.hardened_local_mode {
        0
    } else {
        GATEWAY_ROUND_ROBIN_CURSOR.fetch_add(1, Ordering::Relaxed)
    }
}

fn normalize_plan_key(plan_type: Option<&str>) -> String {
    let normalized = plan_type.unwrap_or("").trim().to_ascii_lowercase();
    if normalized.is_empty() {
        return "free".to_string();
    }
    if normalized.contains("enterprise") {
        return "enterprise".to_string();
    }
    if normalized.contains("business") {
        return "business".to_string();
    }
    if normalized.contains("team") {
        return "team".to_string();
    }
    if normalized.contains("edu") {
        return "edu".to_string();
    }
    if normalized.contains("go") {
        return "go".to_string();
    }
    if normalized.contains("plus") {
        return "plus".to_string();
    }
    if normalized.contains("pro") {
        return "pro".to_string();
    }
    if normalized.contains("free") {
        return "free".to_string();
    }
    normalized
}

fn normalize_auth_file_plan_type(plan_type: Option<&str>) -> Option<&'static str> {
    let normalized = plan_type?
        .trim()
        .to_ascii_lowercase()
        .replace(['_', ' '], "-");
    match normalized.as_str() {
        "prolite" | "pro-lite" => Some("prolite"),
        "promax" | "pro-max" => Some("promax"),
        _ => None,
    }
}

fn resolve_plan_rank(account: &CodexAccount) -> Option<i32> {
    let plan_key = normalize_plan_key(account.plan_type.as_deref());
    let auth_file_plan_type = normalize_auth_file_plan_type(account.auth_file_plan_type.as_deref())
        .or_else(|| normalize_auth_file_plan_type(account.plan_type.as_deref()));

    let rank = match plan_key.as_str() {
        "enterprise" => 700,
        "business" => 650,
        "team" => 640,
        "edu" => 630,
        // CPA 对齐：plan_type='pro' 默认视为 promax (20x)，
        // 只有显式声明 prolite 时才降级
        "pro" => match auth_file_plan_type {
            Some("prolite") => 520,
            _ => 560, // pro / promax 均为 20x 级别
        },
        "plus" => 420,
        "go" => 360,
        "free" => 300,
        _ => return None,
    };

    Some(rank)
}

fn resolve_remaining_quota(account: &CodexAccount) -> Option<i32> {
    let quota = account.quota.as_ref()?;
    let mut percentages = Vec::new();
    if quota.hourly_window_present.unwrap_or(true) {
        percentages.push(quota.hourly_percentage.clamp(0, 100));
    }
    if quota.weekly_window_present.unwrap_or(true) {
        percentages.push(quota.weekly_percentage.clamp(0, 100));
    }
    percentages.into_iter().min()
}

fn normalize_unix_timestamp_millis(value: i64) -> i64 {
    if value > 0 && value < 1_000_000_000_000 {
        value.saturating_mul(1000)
    } else {
        value
    }
}

fn resolve_earliest_quota_reset_ms(account: &CodexAccount) -> Option<i64> {
    let quota = account.quota.as_ref()?;
    [quota.hourly_reset_time, quota.weekly_reset_time]
        .into_iter()
        .flatten()
        .filter(|value| *value > 0)
        .map(normalize_unix_timestamp_millis)
        .min()
}

fn resolve_subscription_expiry_ms(account: &CodexAccount) -> Option<i64> {
    let raw = account.subscription_active_until.as_deref()?.trim();
    if raw.is_empty() {
        return None;
    }

    if raw.chars().all(|ch| ch.is_ascii_digit()) {
        let mut timestamp = raw.parse::<i64>().ok()?;
        if timestamp < 1_000_000_000_000 {
            timestamp *= 1000;
        }
        return Some(timestamp);
    }

    chrono::DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|parsed| parsed.timestamp_millis())
}

fn retry_failover_max_retries(config: &CodexLocalApiSafetyConfig) -> usize {
    config.max_retries.clamp(1, 3) as usize
}

fn retry_failover_account_attempt_limit(config: &CodexLocalApiSafetyConfig) -> usize {
    (config.max_retry_accounts.max(2) as usize).clamp(1, MAX_RETRY_CREDENTIALS_PER_REQUEST)
}

fn build_effective_local_access_account_ids(
    collection: &CodexLocalAccessCollection,
) -> Vec<String> {
    let mut account_ids = collection.account_ids.clone();
    account_ids.sort();
    account_ids
}

fn build_effective_local_access_account_ids_from_registry(
    collection: &CodexLocalAccessCollection,
    registry: &CodexLocalAccessHealthRegistry,
    now: i64,
) -> Vec<String> {
    let mut account_ids = build_effective_local_access_account_ids(collection);
    sort_account_ids_by_health_estimate(&mut account_ids, registry, now);
    pin_process_sticky_account(account_ids, registry, None, now)
}

fn build_effective_local_access_account_ids_for_state(
    collection: &CodexLocalAccessCollection,
) -> Vec<String> {
    let Ok(registry) = load_health_registry_from_disk() else {
        let mut account_ids = build_effective_local_access_account_ids(collection);
        sort_account_ids_by_health_estimate(
            &mut account_ids,
            &empty_health_registry(now_ms()),
            now_ms(),
        );
        return account_ids;
    };
    build_effective_local_access_account_ids_from_registry(collection, &registry, now_ms())
}

fn build_routing_pool_account_ids(collection: &CodexLocalAccessCollection) -> Vec<String> {
    collection.account_ids.clone()
}

fn apply_collection_routing_strategy(
    account_ids: &[String],
    collection: &CodexLocalAccessCollection,
) -> Vec<String> {
    if collection.safety_config.hardened_local_mode {
        return account_ids.to_vec();
    }
    apply_routing_strategy(account_ids, collection.routing_strategy)
}

fn build_routing_candidates(ordered_account_ids: &[String]) -> Vec<RoutingCandidate> {
    ordered_account_ids
        .iter()
        .map(|account_id| {
            let account = try_get_cached_account_for_routing(account_id)
                .or_else(|| codex_account::load_account(account_id));
            RoutingCandidate {
                account_id: account_id.clone(),
                plan_rank: account.as_ref().and_then(resolve_plan_rank),
                remaining_quota: account.as_ref().and_then(resolve_remaining_quota),
                subscription_expiry_ms: account.as_ref().and_then(resolve_subscription_expiry_ms),
            }
        })
        .collect()
}

fn compare_routing_candidates(
    left: &RoutingCandidate,
    right: &RoutingCandidate,
    strategy: CodexLocalAccessRoutingStrategy,
) -> std::cmp::Ordering {
    use std::cmp::Ordering;

    let compare_option_desc = |a: Option<i32>, b: Option<i32>| match (a, b) {
        (Some(left), Some(right)) => right.cmp(&left),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    };
    let compare_option_asc = |a: Option<i32>, b: Option<i32>| match (a, b) {
        (Some(left), Some(right)) => left.cmp(&right),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    };
    let compare_option_i64_asc = |a: Option<i64>, b: Option<i64>| match (a, b) {
        (Some(left), Some(right)) => left.cmp(&right),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    };

    let ordering = match strategy {
        CodexLocalAccessRoutingStrategy::Auto => {
            compare_option_desc(left.plan_rank, right.plan_rank)
                .then_with(|| compare_option_desc(left.remaining_quota, right.remaining_quota))
        }
        CodexLocalAccessRoutingStrategy::QuotaHighFirst => {
            compare_option_desc(left.remaining_quota, right.remaining_quota)
                .then_with(|| compare_option_desc(left.plan_rank, right.plan_rank))
        }
        CodexLocalAccessRoutingStrategy::QuotaLowFirst => {
            compare_option_asc(left.remaining_quota, right.remaining_quota)
                .then_with(|| compare_option_desc(left.plan_rank, right.plan_rank))
        }
        CodexLocalAccessRoutingStrategy::PlanHighFirst => {
            compare_option_desc(left.plan_rank, right.plan_rank)
                .then_with(|| compare_option_desc(left.remaining_quota, right.remaining_quota))
        }
        CodexLocalAccessRoutingStrategy::PlanLowFirst => {
            compare_option_asc(left.plan_rank, right.plan_rank)
                .then_with(|| compare_option_desc(left.remaining_quota, right.remaining_quota))
        }
        CodexLocalAccessRoutingStrategy::ExpirySoonFirst => {
            compare_option_i64_asc(left.subscription_expiry_ms, right.subscription_expiry_ms)
                .then_with(|| compare_option_desc(left.plan_rank, right.plan_rank))
                .then_with(|| compare_option_desc(left.remaining_quota, right.remaining_quota))
        }
    };

    ordering.then_with(|| left.account_id.cmp(&right.account_id))
}

fn apply_routing_strategy(
    account_ids: &[String],
    strategy: CodexLocalAccessRoutingStrategy,
) -> Vec<String> {
    let mut candidates = build_routing_candidates(account_ids);
    candidates.sort_by(|left, right| compare_routing_candidates(left, right, strategy));
    candidates
        .into_iter()
        .map(|candidate| candidate.account_id)
        .collect()
}

fn pin_account_to_front(
    account_ids: Vec<String>,
    preferred_account_id: Option<&str>,
) -> Vec<String> {
    let Some(preferred_account_id) = preferred_account_id else {
        return account_ids;
    };
    let preferred_account_id = preferred_account_id.trim();
    if preferred_account_id.is_empty() {
        return account_ids;
    }

    let mut ordered = Vec::with_capacity(account_ids.len());
    if account_ids
        .iter()
        .any(|account_id| account_id == preferred_account_id)
    {
        ordered.push(preferred_account_id.to_string());
    }
    for account_id in account_ids {
        if account_id == preferred_account_id {
            continue;
        }
        ordered.push(account_id);
    }
    ordered
}

fn constrain_previous_response_affinity(
    account_ids: Vec<String>,
    affinity_account_id: Option<&str>,
) -> Vec<String> {
    let Some(affinity_account_id) = affinity_account_id
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return account_ids;
    };
    if account_ids
        .iter()
        .any(|account_id| account_id == affinity_account_id)
    {
        vec![affinity_account_id.to_string()]
    } else {
        Vec::new()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct SelectorAuditSummary {
    candidate_count: usize,
    eligible_count: usize,
    skipped_counts_by_reason: BTreeMap<String, usize>,
    cap_applied: bool,
    cap_limit: usize,
    sticky_cleared: bool,
}

fn account_id_matches(account_id: Option<&str>, expected: &str) -> bool {
    let expected = expected.trim();
    if expected.is_empty() {
        return false;
    }
    account_id
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value == expected)
        .unwrap_or(false)
}

fn increment_selector_skip_count(counts: &mut BTreeMap<String, usize>, reason: &str) {
    let count = counts.entry(reason.to_string()).or_insert(0);
    *count = count.saturating_add(1);
}

fn selector_selected_reason(
    selected_account_id: &str,
    active_stream_affinity_account_id: Option<&str>,
    previous_response_affinity_account_id: Option<&str>,
    request_affinity_account_id: Option<&str>,
    process_sticky_account_id: Option<&str>,
) -> &'static str {
    if account_id_matches(active_stream_affinity_account_id, selected_account_id) {
        return "active_stream_affinity_selected";
    }
    if account_id_matches(previous_response_affinity_account_id, selected_account_id) {
        return "previous_response_affinity_selected";
    }
    if account_id_matches(request_affinity_account_id, selected_account_id) {
        return "request_affinity_selected";
    }
    if account_id_matches(process_sticky_account_id, selected_account_id) {
        return "sticky_selected";
    }
    "fill_first_selected"
}

fn build_selector_audit_summary(
    candidate_ids: &[String],
    registry: &CodexLocalAccessHealthRegistry,
    model: Option<&str>,
    now: i64,
    max_credential_attempts: usize,
    affinity_account_id: Option<&str>,
    sticky_cleared: bool,
) -> SelectorAuditSummary {
    let cap_limit = max_credential_attempts.max(1);
    let mut summary = SelectorAuditSummary {
        candidate_count: candidate_ids.len(),
        cap_applied: candidate_ids.len() > cap_limit,
        cap_limit,
        sticky_cleared,
        ..SelectorAuditSummary::default()
    };

    for account_id in candidate_ids.iter().take(cap_limit) {
        let account_id = account_id.trim();
        if account_id.is_empty() {
            increment_selector_skip_count(
                &mut summary.skipped_counts_by_reason,
                "invalid_candidate",
            );
            continue;
        }

        let is_affinity_continuation = account_id_matches(affinity_account_id, account_id);
        let has_hard_block = health_registry_account_has_hard_block(registry, account_id);
        if health_registry_account_cooldown_wait(registry, account_id, model, now).is_some()
            && (!is_affinity_continuation || has_hard_block)
        {
            increment_selector_skip_count(&mut summary.skipped_counts_by_reason, "health_skipped");
            continue;
        }

        if (!is_affinity_continuation || has_hard_block)
            && !health_registry_account_is_schedulable(registry, account_id, model, now)
        {
            increment_selector_skip_count(&mut summary.skipped_counts_by_reason, "health_skipped");
            continue;
        }

        summary.eligible_count = summary.eligible_count.saturating_add(1);
    }

    if summary.cap_applied {
        let truncated = candidate_ids.len().saturating_sub(cap_limit);
        if truncated > 0 {
            summary
                .skipped_counts_by_reason
                .insert("cap_truncated".to_string(), truncated);
        }
    }
    if sticky_cleared {
        increment_selector_skip_count(&mut summary.skipped_counts_by_reason, "sticky_cleared");
    }

    summary
}

fn selector_audit_detail(
    summary: &SelectorAuditSummary,
    selected_reason: &str,
    model_key: &str,
) -> BTreeMap<String, String> {
    let skipped_counts = serde_json::to_string(&summary.skipped_counts_by_reason)
        .unwrap_or_else(|_| "{}".to_string());
    BTreeMap::from([
        ("model_key".to_string(), model_key.to_string()),
        (
            "candidate_count".to_string(),
            summary.candidate_count.to_string(),
        ),
        (
            "eligible_count".to_string(),
            summary.eligible_count.to_string(),
        ),
        ("skipped_counts_by_reason".to_string(), skipped_counts),
        ("selected_reason".to_string(), selected_reason.to_string()),
        ("cap_applied".to_string(), summary.cap_applied.to_string()),
        ("cap_limit".to_string(), summary.cap_limit.to_string()),
        (
            "sticky_cleared".to_string(),
            summary.sticky_cleared.to_string(),
        ),
    ])
}

fn infer_single_account_continuation_affinity(
    previous_response_id: Option<&str>,
    account_ids: &[String],
) -> Option<String> {
    previous_response_id?;
    if account_ids.len() == 1 {
        return account_ids.first().cloned();
    }
    None
}

fn resolve_local_access_projection_account(
    collection: &CodexLocalAccessCollection,
) -> Result<CodexAccount, String> {
    let ordered_account_ids = build_effective_local_access_account_ids_for_state(collection);
    let first_account_id = ordered_account_ids
        .first()
        .ok_or_else(|| "API 服务实际调度池为空；请先把手动配置账号加入 API 服务".to_string())?;
    codex_account::load_account(first_account_id).ok_or_else(|| {
        format!(
            "API 服务成员 {} 不存在，无法写入 API 服务投影",
            first_account_id
        )
    })
}

fn format_retry_after_duration(wait: Duration) -> String {
    let seconds = wait.as_secs().max(1);
    format!("{} 秒", seconds)
}

fn build_cooldown_unavailable_message(model_key: &str, wait: Duration) -> String {
    let wait_text = format_retry_after_duration(wait);
    if model_key.trim().is_empty() {
        format!("当前 API 服务账号均在冷却中，请 {} 后重试", wait_text)
    } else {
        format!(
            "模型 {} 的可用账号均在冷却中，请 {} 后重试",
            model_key, wait_text,
        )
    }
}

fn summarize_pool_unavailability(
    registry: &CodexLocalAccessHealthRegistry,
    account_ids: &[String],
    model: Option<&str>,
    now: i64,
) -> PoolUnavailableSummary {
    let mut summary = PoolUnavailableSummary {
        total_count: account_ids.len(),
        ..PoolUnavailableSummary::default()
    };

    for account_id in account_ids {
        let account_id = account_id.trim();
        if account_id.is_empty() {
            summary.unknown_blocked_count += 1;
            continue;
        }

        let mut blocked = false;
        if let Some(account) = registry.accounts.get(account_id) {
            match account.status {
                CodexLocalAccessAccountHealthStatus::Healthy
                | CodexLocalAccessAccountHealthStatus::EstimatedAvailable => {}
                CodexLocalAccessAccountHealthStatus::CoolingDown => {
                    if let Some(wait) = cooldown_wait_from_until_ms(account.cooldown_until_ms, now)
                    {
                        summary.cooling_count += 1;
                        summary.nearest_wait = min_cooldown_wait(summary.nearest_wait, wait);
                        blocked = true;
                    }
                }
                CodexLocalAccessAccountHealthStatus::Exhausted => {
                    summary.exhausted_count += 1;
                    if let Some(wait) = cooldown_wait_from_until_ms(
                        account.estimated_reset_at_ms.or(account.cooldown_until_ms),
                        now,
                    ) {
                        summary.nearest_wait = min_cooldown_wait(summary.nearest_wait, wait);
                    }
                    blocked = true;
                }
                CodexLocalAccessAccountHealthStatus::AuthSuspect
                | CodexLocalAccessAccountHealthStatus::ManualRequired => {
                    summary.manual_required_count += 1;
                    blocked = true;
                }
                CodexLocalAccessAccountHealthStatus::Disabled => {
                    summary.disabled_count += 1;
                    blocked = true;
                }
            }

            if account.manual_required
                && !matches!(
                    account.status,
                    CodexLocalAccessAccountHealthStatus::AuthSuspect
                        | CodexLocalAccessAccountHealthStatus::ManualRequired
                )
            {
                summary.manual_required_count += 1;
                blocked = true;
            }
        }

        if let Some(model) = model.map(str::trim).filter(|value| !value.is_empty()) {
            let key = health_registry_model_key(account_id, model);
            if let Some(wait) = registry.model_cooldowns.get(&key).and_then(|cooldown| {
                cooldown_wait_from_until_ms(Some(cooldown.cooldown_until_ms), now)
            }) {
                summary.model_cooldown_count += 1;
                summary.nearest_wait = min_cooldown_wait(summary.nearest_wait, wait);
                blocked = true;
            }
        }

        if !blocked && health_registry_account_is_schedulable(registry, account_id, model, now) {
            summary.schedulable_count += 1;
        } else if !blocked {
            summary.unknown_blocked_count += 1;
        }
    }

    summary
}

fn status_for_pool_unavailable(summary: &PoolUnavailableSummary) -> u16 {
    if summary.total_count > 0
        && summary.manual_required_count == summary.total_count
        && summary.schedulable_count == 0
    {
        return StatusCode::UNAUTHORIZED.as_u16();
    }
    StatusCode::SERVICE_UNAVAILABLE.as_u16()
}

fn should_use_pool_unavailable_summary(summary: &PoolUnavailableSummary) -> bool {
    summary.total_count > 0
        && summary.schedulable_count == 0
        && (summary.exhausted_count
            + summary.cooling_count
            + summary.model_cooldown_count
            + summary.manual_required_count
            + summary.disabled_count
            + summary.unknown_blocked_count)
            > 0
}

fn should_defer_pool_unavailable(summary: &PoolUnavailableSummary) -> bool {
    should_use_pool_unavailable_summary(summary)
        && summary.nearest_wait.is_some()
        && (summary.exhausted_count + summary.cooling_count + summary.model_cooldown_count) > 0
}

fn pool_wait_fits_request_budget(
    wait: Duration,
    elapsed: Duration,
    request_timeout: Duration,
) -> bool {
    let timeout_guard = Duration::from_secs(2);
    let remaining = request_timeout.saturating_sub(elapsed);
    wait <= MAX_POOL_UNAVAILABLE_PRE_ADMISSION_WAIT
        && remaining > timeout_guard
        && wait < remaining.saturating_sub(timeout_guard)
}

fn backpressure_wait_budget(elapsed: Duration, request_timeout: Duration) -> Option<Duration> {
    let timeout_guard = Duration::from_secs(2);
    let remaining = request_timeout.saturating_sub(elapsed);
    if remaining <= timeout_guard {
        return None;
    }
    Some(remaining.saturating_sub(timeout_guard))
}

fn build_pool_unavailable_message(model_key: &str, summary: &PoolUnavailableSummary) -> String {
    if summary.total_count == 0 {
        return "API 服务号池为空，请先加入账号".to_string();
    }

    let model_prefix = model_key
        .trim()
        .is_empty()
        .then(String::new)
        .unwrap_or_else(|| format!("模型 {} 的", model_key.trim()));

    if summary.exhausted_count == summary.total_count {
        if let Some(wait) = summary.nearest_wait {
            return format!(
                "{}API 服务号池账号额度均已耗尽，请 {} 后重试",
                model_prefix,
                format_retry_after_duration(wait)
            );
        }
        return format!(
            "{}API 服务号池账号额度均已耗尽，且未拿到上游 reset 时间；请刷新配额、调整号池或恢复账号后重试",
            model_prefix
        );
    }

    if summary.manual_required_count == summary.total_count {
        return format!(
            "{}API 服务号池账号均需人工处理（重新登录或风控验证），请在 Cockpit 中恢复账号后重试",
            model_prefix
        );
    }

    let mut parts = Vec::new();
    if summary.exhausted_count > 0 {
        parts.push(format!("额度耗尽 {} 个", summary.exhausted_count));
    }
    let cooling_total = summary
        .cooling_count
        .saturating_add(summary.model_cooldown_count);
    if cooling_total > 0 {
        parts.push(format!("冷却中 {} 个", cooling_total));
    }
    if summary.manual_required_count > 0 {
        parts.push(format!("需人工处理 {} 个", summary.manual_required_count));
    }
    if summary.disabled_count > 0 {
        parts.push(format!("已禁用 {} 个", summary.disabled_count));
    }
    if summary.unknown_blocked_count > 0 {
        parts.push(format!("状态未知 {} 个", summary.unknown_blocked_count));
    }

    if parts.is_empty() {
        return format!(
            "{}API 服务号池暂无可调度账号，请刷新配额或调整号池后重试",
            model_prefix
        );
    }

    format!(
        "{}API 服务号池暂无可调度账号（{}）；请刷新配额、恢复账号或调整号池后重试",
        model_prefix,
        parts.join("，")
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CodexLocalAccessErrorType {
    UpstreamRateLimit,
    UsageLimitReached,
    AuthError,
    CaptchaOrSuspicious,
    InsufficientQuota,
    ModelCapacity,
    NetworkError,
    ServerError,
    Unknown,
}

impl CodexLocalAccessErrorType {
    fn as_str(self) -> &'static str {
        match self {
            Self::UpstreamRateLimit => "upstream_rate_limit",
            Self::UsageLimitReached => "usage_limit_reached",
            Self::AuthError => "auth_error",
            Self::CaptchaOrSuspicious => "captcha_or_suspicious",
            Self::InsufficientQuota => "insufficient_quota",
            Self::ModelCapacity => "model_capacity",
            Self::NetworkError => "network_error",
            Self::ServerError => "server_error",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CodexLocalAccessErrorSource {
    Network,
    Upstream,
}

impl CodexLocalAccessErrorSource {
    fn as_str(self) -> &'static str {
        match self {
            Self::Network => "network",
            Self::Upstream => "upstream",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CodexLocalAccessErrorScope {
    Request,
    Account,
    Model,
    Provider,
    Unknown,
}

impl CodexLocalAccessErrorScope {
    fn as_str(self) -> &'static str {
        match self {
            Self::Request => "request",
            Self::Account => "account",
            Self::Model => "model",
            Self::Provider => "provider",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ClassifiedCodexUpstreamError {
    error_type: CodexLocalAccessErrorType,
    source: CodexLocalAccessErrorSource,
    scope: CodexLocalAccessErrorScope,
    status: u16,
    provider_code: Option<String>,
    retry_after: Option<Duration>,
    manual_required: bool,
    safe_message: String,
    log_fields: BTreeMap<String, String>,
}

impl ClassifiedCodexUpstreamError {
    fn safe_for_request_failover(&self) -> bool {
        if self.manual_required || matches!(self.status, 401 | 403) {
            return false;
        }

        matches!(
            self.error_type,
            CodexLocalAccessErrorType::UsageLimitReached
                | CodexLocalAccessErrorType::InsufficientQuota
                | CodexLocalAccessErrorType::ModelCapacity
                | CodexLocalAccessErrorType::NetworkError
                | CodexLocalAccessErrorType::ServerError
        )
    }
}

fn sanitize_provider_code(value: &str) -> Option<String> {
    let mut safe = String::new();
    for ch in value.trim().chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.' | ':') {
            safe.push(ch);
        }
        if safe.len() >= 96 {
            break;
        }
    }

    if safe.is_empty() {
        return None;
    }

    Some(safe)
}

fn first_safe_json_metadata_field(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        value
            .get(*key)
            .and_then(Value::as_str)
            .and_then(sanitize_provider_code)
    })
}

fn insert_safe_upstream_header_metadata(
    log_fields: &mut BTreeMap<String, String>,
    headers: Option<&HeaderMap>,
    field_name: &str,
    header_name: &str,
) {
    let Some(headers) = headers else {
        return;
    };
    let Some(value) = headers
        .get(header_name)
        .and_then(|value| value.to_str().ok())
        .and_then(sanitize_provider_code)
    else {
        return;
    };

    log_fields.insert(field_name.to_string(), value);
}

fn insert_codex_upstream_quota_metadata(
    log_fields: &mut BTreeMap<String, String>,
    headers: Option<&HeaderMap>,
    parsed: Option<&Value>,
) {
    if let Some(root) = parsed {
        let candidates = [
            Some(root),
            root.get("error"),
            root.get("detail"),
            root.get("data"),
        ];

        for candidate in candidates.into_iter().flatten() {
            if !log_fields.contains_key("plan_type") {
                if let Some(plan_type) =
                    first_safe_json_metadata_field(candidate, &["plan_type", "planType"])
                {
                    log_fields.insert("plan_type".to_string(), plan_type);
                }
            }
            if !log_fields.contains_key("provider_plan_type") {
                if let Some(plan_type) = first_safe_json_metadata_field(
                    candidate,
                    &["provider_plan_type", "providerPlanType"],
                ) {
                    log_fields.insert("provider_plan_type".to_string(), plan_type);
                }
            }
            if !log_fields.contains_key("reset_at") {
                if let Some(reset_at) = first_i64_json_field(
                    candidate,
                    [
                        "reset_at",
                        "resets_at",
                        "resetAt",
                        "reset_at_seconds",
                        "reset_timestamp",
                        "reset_time",
                    ],
                )
                .map(normalize_unix_timestamp_seconds)
                {
                    log_fields.insert("reset_at".to_string(), reset_at.to_string());
                }
            }
            if !log_fields.contains_key("reset_after_seconds") {
                if let Some(reset_after_seconds) = first_i64_json_field(
                    candidate,
                    [
                        "reset_after_seconds",
                        "resets_in_seconds",
                        "resetAfterSeconds",
                        "retry_after_seconds",
                        "retry_after",
                    ],
                )
                .filter(|seconds| *seconds >= 0)
                {
                    log_fields.insert(
                        "reset_after_seconds".to_string(),
                        reset_after_seconds.to_string(),
                    );
                }
            }
            if !log_fields.contains_key("active_limit") {
                if let Some(active_limit) =
                    first_safe_json_metadata_field(candidate, &["active_limit", "activeLimit"])
                {
                    log_fields.insert("active_limit".to_string(), active_limit);
                }
            }
            if !log_fields.contains_key("rate_limit_reached_type") {
                if let Some(limit_type) = first_safe_json_metadata_field(
                    candidate,
                    &["rate_limit_reached_type", "rateLimitReachedType"],
                ) {
                    log_fields.insert("rate_limit_reached_type".to_string(), limit_type);
                }
            }
            if !log_fields.contains_key("promo_message_present")
                && (candidate.get("promo_message").is_some()
                    || candidate.get("promoMessage").is_some())
            {
                log_fields.insert("promo_message_present".to_string(), "true".to_string());
            }
        }
    }

    insert_safe_upstream_header_metadata(
        log_fields,
        headers,
        "provider_plan_type",
        "x-codex-plan-type",
    );
    insert_safe_upstream_header_metadata(
        log_fields,
        headers,
        "active_limit",
        "x-codex-active-limit",
    );
    insert_safe_upstream_header_metadata(
        log_fields,
        headers,
        "rate_limit_reached_type",
        "x-codex-rate-limit-reached-type",
    );
    if headers
        .and_then(|headers| headers.get("x-codex-promo-message"))
        .is_some()
    {
        log_fields.insert("promo_message_present".to_string(), "true".to_string());
    }
}

fn extract_provider_error_code(parsed: &Value) -> Option<String> {
    for value in [
        parsed
            .get("error")
            .and_then(|error| error.get("type"))
            .and_then(Value::as_str),
        parsed
            .get("error")
            .and_then(|error| error.get("code"))
            .and_then(Value::as_str),
        parsed
            .get("detail")
            .and_then(|detail| detail.get("type"))
            .and_then(Value::as_str),
        parsed
            .get("detail")
            .and_then(|detail| detail.get("code"))
            .and_then(Value::as_str),
        parsed.get("type").and_then(Value::as_str),
        parsed.get("code").and_then(Value::as_str),
    ] {
        if let Some(safe) = value.and_then(sanitize_provider_code) {
            return Some(safe);
        }
    }

    None
}

fn parse_retry_after_ms_header_value(value: &str) -> Option<Duration> {
    let milliseconds = value.trim().parse::<u64>().ok()?;
    if milliseconds == 0 {
        return None;
    }

    Some(Duration::from_millis(milliseconds))
}

fn parse_retry_after_header_value(
    value: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> Option<Duration> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }

    if let Ok(seconds) = trimmed.parse::<u64>() {
        if seconds == 0 {
            return None;
        }
        return Some(Duration::from_secs(seconds));
    }

    let retry_at = chrono::DateTime::parse_from_rfc2822(trimmed)
        .ok()?
        .with_timezone(&chrono::Utc);
    retry_at
        .signed_duration_since(now)
        .to_std()
        .ok()
        .filter(|wait| *wait > Duration::from_secs(0))
}

fn parse_retry_after_headers(headers: Option<&HeaderMap>) -> Option<Duration> {
    let headers = headers?;

    if let Some(wait) = headers
        .get("retry-after-ms")
        .and_then(|value| value.to_str().ok())
        .and_then(parse_retry_after_ms_header_value)
    {
        return Some(wait);
    }

    headers
        .get(RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| parse_retry_after_header_value(value, chrono::Utc::now()))
}

fn parse_upstream_body_retry_after(status: StatusCode, error_body: &str) -> Option<Duration> {
    if error_body.trim().is_empty() {
        return None;
    }
    if !matches!(
        status,
        StatusCode::TOO_MANY_REQUESTS | StatusCode::SERVICE_UNAVAILABLE
    ) {
        return None;
    }

    let payload = serde_json::from_str::<Value>(error_body).ok()?;
    let provider_code = extract_provider_error_code(&payload)
        .unwrap_or_default()
        .to_ascii_lowercase();
    let body_lower = error_body.to_ascii_lowercase();
    let has_retryable_marker = [
        "usage_limit_reached",
        "insufficient_quota",
        "rate_limit",
        "too_many_requests",
        "quota",
        "model_capacity",
        "capacity",
    ]
    .iter()
    .any(|marker| provider_code.contains(marker) || body_lower.contains(marker));
    if !has_retryable_marker {
        return None;
    }

    let now_seconds = chrono::Utc::now().timestamp();
    let reset_hint = extract_account_quota_reset_hint_from_body(error_body, now_seconds);
    if let Some(reset_at) = reset_hint
        .reset_at_seconds
        .filter(|reset_at| *reset_at > now_seconds)
    {
        let delta = reset_at.saturating_sub(now_seconds) as u64;
        if delta > 0 {
            return Some(Duration::from_secs(delta));
        }
    }

    reset_hint
        .reset_after_seconds
        .filter(|seconds| *seconds > 0)
        .map(|seconds| Duration::from_secs(seconds as u64))
}

fn classify_codex_upstream_error(
    status: StatusCode,
    headers: Option<&HeaderMap>,
    error_body: &str,
) -> ClassifiedCodexUpstreamError {
    let parsed = serde_json::from_str::<Value>(error_body).ok();
    let provider_code = parsed.as_ref().and_then(extract_provider_error_code);
    let provider_code_lower = provider_code
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let body_lower = error_body.to_ascii_lowercase();
    let retry_after = parse_retry_after_headers(headers)
        .or_else(|| parse_upstream_body_retry_after(status, error_body));

    let has_marker = |markers: &[&str]| {
        markers
            .iter()
            .any(|marker| provider_code_lower.contains(marker) || body_lower.contains(marker))
    };

    let (error_type, source, scope, manual_required, safe_message) = if status
        == StatusCode::UNAUTHORIZED
        || has_marker(&["unauthorized", "invalid_api_key", "invalid_token", "auth"])
    {
        (
            CodexLocalAccessErrorType::AuthError,
            CodexLocalAccessErrorSource::Upstream,
            CodexLocalAccessErrorScope::Account,
            true,
            "上游账号鉴权失败，请手动重新登录或刷新账号".to_string(),
        )
    } else if status == StatusCode::FORBIDDEN
        && has_marker(&["captcha", "suspicious", "abuse", "risk", "challenge"])
    {
        (
            CodexLocalAccessErrorType::CaptchaOrSuspicious,
            CodexLocalAccessErrorSource::Upstream,
            CodexLocalAccessErrorScope::Account,
            true,
            "上游要求人工验证或触发风控，请手动处理该账号".to_string(),
        )
    } else if status == StatusCode::FORBIDDEN {
        (
            CodexLocalAccessErrorType::AuthError,
            CodexLocalAccessErrorSource::Upstream,
            CodexLocalAccessErrorScope::Account,
            true,
            "上游拒绝该账号或请求，请手动确认账号状态".to_string(),
        )
    } else if provider_code_lower == "usage_limit_reached"
        || has_marker(&["usage_limit_reached", "limit reached"])
    {
        (
            CodexLocalAccessErrorType::UsageLimitReached,
            CodexLocalAccessErrorSource::Upstream,
            CodexLocalAccessErrorScope::Model,
            false,
            "上游返回使用额度冷却，请稍后重试".to_string(),
        )
    } else if has_marker(&["insufficient_quota", "quota exceeded"]) {
        (
            CodexLocalAccessErrorType::InsufficientQuota,
            CodexLocalAccessErrorSource::Upstream,
            CodexLocalAccessErrorScope::Account,
            false,
            "上游账号额度不足，请更换或恢复账号额度".to_string(),
        )
    } else if has_marker(&[
        "selected model is at capacity",
        "model is at capacity",
        "model_capacity",
    ]) {
        (
            CodexLocalAccessErrorType::ModelCapacity,
            CodexLocalAccessErrorSource::Upstream,
            CodexLocalAccessErrorScope::Model,
            false,
            "上游模型容量暂不可用，请稍后重试".to_string(),
        )
    } else if status == StatusCode::TOO_MANY_REQUESTS {
        (
            CodexLocalAccessErrorType::UpstreamRateLimit,
            CodexLocalAccessErrorSource::Upstream,
            CodexLocalAccessErrorScope::Provider,
            false,
            "上游请求频率受限，请稍后重试".to_string(),
        )
    } else if status == StatusCode::REQUEST_TIMEOUT {
        (
            CodexLocalAccessErrorType::NetworkError,
            CodexLocalAccessErrorSource::Network,
            CodexLocalAccessErrorScope::Request,
            false,
            "上游请求超时，请稍后重试".to_string(),
        )
    } else if status.is_server_error() {
        (
            CodexLocalAccessErrorType::ServerError,
            CodexLocalAccessErrorSource::Upstream,
            CodexLocalAccessErrorScope::Provider,
            false,
            "上游服务暂不可用，请稍后重试".to_string(),
        )
    } else {
        (
            CodexLocalAccessErrorType::Unknown,
            CodexLocalAccessErrorSource::Upstream,
            CodexLocalAccessErrorScope::Unknown,
            false,
            format!("上游接口返回状态 {}", status.as_u16()),
        )
    };

    let mut log_fields = BTreeMap::from([
        ("error_type".to_string(), error_type.as_str().to_string()),
        ("source".to_string(), source.as_str().to_string()),
        ("scope".to_string(), scope.as_str().to_string()),
        ("status".to_string(), status.as_u16().to_string()),
        ("manual_required".to_string(), manual_required.to_string()),
    ]);

    if let Some(provider_code) = provider_code.as_deref() {
        log_fields.insert("provider_code".to_string(), provider_code.to_string());
    }
    if let Some(wait) = retry_after {
        log_fields.insert("retry_after_ms".to_string(), wait.as_millis().to_string());
    }
    insert_codex_upstream_quota_metadata(&mut log_fields, headers, parsed.as_ref());

    ClassifiedCodexUpstreamError {
        error_type,
        source,
        scope,
        status: status.as_u16(),
        provider_code,
        retry_after,
        manual_required,
        safe_message,
        log_fields,
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct AccountQuotaResetHint {
    reset_at_seconds: Option<i64>,
    reset_after_seconds: Option<i64>,
}

fn should_persist_account_quota_exhaustion(classified: &ClassifiedCodexUpstreamError) -> bool {
    matches!(
        classified.error_type,
        CodexLocalAccessErrorType::UsageLimitReached | CodexLocalAccessErrorType::InsufficientQuota
    )
}

fn normalize_unix_timestamp_seconds(value: i64) -> i64 {
    if value > 1_000_000_000_000 {
        value / 1000
    } else {
        value
    }
}

fn first_i64_json_field<'a>(
    value: &'a Value,
    keys: impl IntoIterator<Item = &'a str>,
) -> Option<i64> {
    keys.into_iter().find_map(|key| {
        value.get(key).and_then(|item| {
            item.as_i64()
                .or_else(|| item.as_u64().and_then(|v| i64::try_from(v).ok()))
                .or_else(|| {
                    item.as_str()
                        .and_then(|value| value.trim().parse::<i64>().ok())
                })
        })
    })
}

fn extract_account_quota_reset_hint_from_body(
    error_body: &str,
    now_seconds: i64,
) -> AccountQuotaResetHint {
    let Ok(root) = serde_json::from_str::<Value>(error_body) else {
        return AccountQuotaResetHint::default();
    };
    let candidates = [
        Some(&root),
        root.get("error"),
        root.get("detail"),
        root.get("data"),
    ];

    for candidate in candidates.into_iter().flatten() {
        let reset_at = first_i64_json_field(
            candidate,
            [
                "reset_at",
                "resets_at",
                "resetAt",
                "reset_at_seconds",
                "reset_timestamp",
                "reset_time",
            ],
        )
        .map(normalize_unix_timestamp_seconds);
        let reset_after_seconds = first_i64_json_field(
            candidate,
            [
                "reset_after_seconds",
                "resets_in_seconds",
                "resetAfterSeconds",
                "retry_after_seconds",
                "retry_after",
            ],
        )
        .filter(|seconds| *seconds >= 0);

        if reset_at.is_some() || reset_after_seconds.is_some() {
            return AccountQuotaResetHint {
                reset_at_seconds: reset_at.or_else(|| {
                    reset_after_seconds.map(|seconds| now_seconds.saturating_add(seconds))
                }),
                reset_after_seconds,
            };
        }
    }

    AccountQuotaResetHint::default()
}

fn duration_to_millis_i64(duration: Duration) -> i64 {
    i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
}

fn duration_to_ceiled_seconds_i64(duration: Duration) -> i64 {
    let seconds = duration.as_millis().saturating_add(999) / 1000;
    i64::try_from(seconds).unwrap_or(i64::MAX)
}

fn account_quota_reset_hint_from_classified_error(
    classified: &ClassifiedCodexUpstreamError,
    error_body: &str,
    now_ms: i64,
) -> AccountQuotaResetHint {
    let now_seconds = now_ms.div_euclid(1000);
    let mut hint = extract_account_quota_reset_hint_from_body(error_body, now_seconds);

    if hint.reset_at_seconds.is_none() {
        if let Some(retry_after) = classified.retry_after {
            let reset_after_seconds = duration_to_ceiled_seconds_i64(retry_after);
            hint.reset_at_seconds = Some(now_seconds.saturating_add(reset_after_seconds));
            hint.reset_after_seconds.get_or_insert(reset_after_seconds);
        }
    }

    hint
}

fn build_account_quota_exhaustion_message(
    classified: &ClassifiedCodexUpstreamError,
    reset_at_seconds: Option<i64>,
) -> String {
    let mut message = format!(
        "Cockpit API service upstream quota exhausted: status={}, error_type={}",
        classified.status,
        classified.error_type.as_str()
    );
    if let Some(provider_code) = classified.provider_code.as_deref() {
        message.push_str(&format!(", provider_code={}", provider_code));
    }
    if let Some(reset_at) = reset_at_seconds {
        message.push_str(&format!(", reset_at={}", reset_at));
    }
    message
}

fn apply_account_quota_exhaustion_snapshot(
    account: &mut CodexAccount,
    classified: &ClassifiedCodexUpstreamError,
    error_body: &str,
    now_ms: i64,
) -> bool {
    if !should_persist_account_quota_exhaustion(classified) {
        return false;
    }

    let now_seconds = now_ms.div_euclid(1000);
    let previous = account.quota.as_ref();
    let reset_hint = account_quota_reset_hint_from_classified_error(classified, error_body, now_ms);
    let reset_at = reset_hint.reset_at_seconds;
    let retry_after_ms = classified.retry_after.map(duration_to_millis_i64);

    account.quota = Some(CodexQuota {
        hourly_percentage: 0,
        hourly_reset_time: reset_at.or_else(|| previous.and_then(|quota| quota.hourly_reset_time)),
        hourly_window_minutes: previous.and_then(|quota| quota.hourly_window_minutes),
        hourly_window_present: previous
            .and_then(|quota| quota.hourly_window_present)
            .or(Some(true)),
        weekly_percentage: 0,
        weekly_reset_time: reset_at.or_else(|| previous.and_then(|quota| quota.weekly_reset_time)),
        weekly_window_minutes: previous.and_then(|quota| quota.weekly_window_minutes),
        weekly_window_present: previous
            .and_then(|quota| quota.weekly_window_present)
            .or(Some(true)),
        raw_data: Some(json!({
            "source": "codex_local_access_upstream_error",
            "quota_exhausted": true,
            "exhausted_at": now_seconds,
            "reset_at": reset_at,
            "reset_after_seconds": reset_hint.reset_after_seconds,
            "retry_after_ms": retry_after_ms,
            "status": classified.status,
            "error_type": classified.error_type.as_str(),
            "provider_code": classified.provider_code.as_deref(),
        })),
    });
    account.quota_error = Some(CodexQuotaErrorInfo {
        code: classified
            .provider_code
            .clone()
            .or_else(|| Some(classified.error_type.as_str().to_string())),
        message: build_account_quota_exhaustion_message(classified, reset_at),
        timestamp: now_seconds,
    });
    account.usage_updated_at = Some(now_seconds);
    true
}

fn persist_account_quota_exhaustion_from_classified_error(
    account_id: &str,
    classified: &ClassifiedCodexUpstreamError,
    error_body: &str,
    now_ms: i64,
) -> Result<bool, String> {
    let account_id = account_id.trim();
    if account_id.is_empty() || !should_persist_account_quota_exhaustion(classified) {
        return Ok(false);
    }

    let Some(mut account) = codex_account::load_account(account_id) else {
        return Err(format!("账号详情不存在: account_id={}", account_id));
    };
    if !apply_account_quota_exhaustion_snapshot(&mut account, classified, error_body, now_ms) {
        return Ok(false);
    }

    codex_account::save_account(&account)?;
    Ok(true)
}

fn persist_account_quota_exhaustion_with_audit(
    account_id: &str,
    request: &ParsedRequest,
    classified: &ClassifiedCodexUpstreamError,
    error_body: &str,
) {
    if !should_persist_account_quota_exhaustion(classified) {
        return;
    }

    let context = build_audit_context(request, Some(account_id));
    match persist_account_quota_exhaustion_from_classified_error(
        account_id,
        classified,
        error_body,
        now_ms(),
    ) {
        Ok(true) => record_audit_event_from_context(
            &context,
            "account_quota_snapshot",
            Some(classified.status),
            Some(classified.error_type.as_str()),
            None,
            Some("recorded"),
            classified_audit_detail(classified),
        ),
        Ok(false) => {}
        Err(err) => {
            logger::log_warn(&format!(
                "[CodexLocalAccess][QuotaSnapshot] 写入账号额度耗尽快照失败: {}",
                err
            ));
            record_audit_event_from_context(
                &context,
                "account_quota_snapshot",
                Some(classified.status),
                Some(classified.error_type.as_str()),
                None,
                Some("error"),
                BTreeMap::from([("reason".to_string(), "persist_failed".to_string())]),
            );
        }
    }
}

fn empty_health_registry(now: i64) -> CodexLocalAccessHealthRegistry {
    CodexLocalAccessHealthRegistry {
        schema_version: CODEX_LOCAL_ACCESS_HEALTH_SCHEMA_VERSION,
        updated_at: now,
        ..CodexLocalAccessHealthRegistry::default()
    }
}

fn normalize_health_registry(
    mut registry: CodexLocalAccessHealthRegistry,
    now: i64,
) -> CodexLocalAccessHealthRegistry {
    if registry.schema_version == 0 {
        registry.schema_version = CODEX_LOCAL_ACCESS_HEALTH_SCHEMA_VERSION;
    }
    if registry.updated_at <= 0 {
        registry.updated_at = now;
    }
    demote_model_scoped_account_cooldowns(&mut registry, now);
    apply_estimated_quota_recovery(&mut registry, now);
    prune_persisted_request_affinity_bindings(&mut registry, None, now);
    registry
}

fn is_model_scoped_cooldown_type(error_type: CodexLocalAccessErrorType) -> bool {
    matches!(
        error_type,
        CodexLocalAccessErrorType::UsageLimitReached | CodexLocalAccessErrorType::ModelCapacity
    )
}

fn is_model_scoped_cooldown_error_name(error_type: &str) -> bool {
    matches!(
        error_type.trim().to_ascii_lowercase().as_str(),
        "usage_limit_reached" | "model_capacity"
    )
}

fn is_model_scoped_cooldown(
    classified: &ClassifiedCodexUpstreamError,
    model: Option<&str>,
) -> bool {
    model
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .is_some()
        && classified.scope == CodexLocalAccessErrorScope::Model
        && is_model_scoped_cooldown_type(classified.error_type)
}

fn demote_model_scoped_account_cooldowns(registry: &mut CodexLocalAccessHealthRegistry, now: i64) {
    let active_model_scoped_accounts: HashSet<String> = registry
        .model_cooldowns
        .values()
        .filter(|cooldown| cooldown.cooldown_until_ms > now)
        .filter(|cooldown| {
            cooldown
                .last_error_type
                .as_deref()
                .map(is_model_scoped_cooldown_error_name)
                .unwrap_or(false)
        })
        .map(|cooldown| cooldown.account_id.trim().to_string())
        .filter(|account_id| !account_id.is_empty())
        .collect();

    if active_model_scoped_accounts.is_empty() {
        return;
    }

    let mut changed = false;
    for (account_id, account) in registry.accounts.iter_mut() {
        if !active_model_scoped_accounts.contains(account_id.as_str()) {
            continue;
        }
        if !account
            .last_error_type
            .as_deref()
            .map(is_model_scoped_cooldown_error_name)
            .unwrap_or(false)
        {
            continue;
        }
        if !matches!(
            account.status,
            CodexLocalAccessAccountHealthStatus::CoolingDown
                | CodexLocalAccessAccountHealthStatus::Exhausted
        ) {
            continue;
        }

        account.status = CodexLocalAccessAccountHealthStatus::Healthy;
        account.cooldown_until_ms = None;
        account.exhausted_at_ms = None;
        account.estimated_reset_at_ms = None;
        account.estimated_remaining_percentage = None;
        account.last_observed_remaining_percentage = None;
        account.reset_source = None;
        account.confidence = None;
        account.manual_required = false;
        account.updated_at = now;
        changed = true;
    }

    if changed {
        registry.schema_version = CODEX_LOCAL_ACCESS_HEALTH_SCHEMA_VERSION;
        registry.updated_at = now;
    }
}

fn apply_estimated_quota_recovery(registry: &mut CodexLocalAccessHealthRegistry, now: i64) {
    for account in registry.accounts.values_mut() {
        if account.manual_required {
            continue;
        }
        if !matches!(
            account.status,
            CodexLocalAccessAccountHealthStatus::CoolingDown
                | CodexLocalAccessAccountHealthStatus::Exhausted
        ) {
            continue;
        }
        let Some(reset_at) = account.estimated_reset_at_ms.or(account.cooldown_until_ms) else {
            continue;
        };
        if reset_at > now {
            continue;
        }

        account.status = CodexLocalAccessAccountHealthStatus::EstimatedAvailable;
        account.cooldown_until_ms = None;
        account.estimated_remaining_percentage = Some(100);
        account.confidence = Some("estimated".to_string());
        account.updated_at = now;
    }
}

fn load_health_registry_from_path(path: &Path) -> Result<CodexLocalAccessHealthRegistry, String> {
    if !path.exists() {
        return Ok(empty_health_registry(now_ms()));
    }
    let content =
        std::fs::read_to_string(path).map_err(|e| format!("读取 API 服务健康状态失败: {}", e))?;
    let parsed = serde_json::from_str::<CodexLocalAccessHealthRegistry>(&content)
        .map_err(|e| format!("解析 API 服务健康状态失败: {}", e))?;
    Ok(normalize_health_registry(parsed, now_ms()))
}

fn save_health_registry_to_path(
    path: &Path,
    registry: &CodexLocalAccessHealthRegistry,
) -> Result<(), String> {
    let content = serde_json::to_string_pretty(registry)
        .map_err(|e| format!("序列化 API 服务健康状态失败: {}", e))?;
    write_string_atomic(path, &content).map_err(|e| format!("写入 API 服务健康状态失败: {}", e))
}

fn load_health_registry_from_disk() -> Result<CodexLocalAccessHealthRegistry, String> {
    let path = local_access_health_file_path()?;
    load_health_registry_from_path(&path)
}

fn save_health_registry_to_disk(registry: &CodexLocalAccessHealthRegistry) -> Result<(), String> {
    let path = local_access_health_file_path()?;
    save_health_registry_to_path(&path, registry)
}

fn health_registry_model_key(account_id: &str, model: &str) -> String {
    format!(
        "{}::{}",
        account_id.trim(),
        model.trim().to_ascii_lowercase()
    )
}

fn health_registry_request_id(request_id: Option<&str>) -> Option<String> {
    request_id
        .and_then(sanitize_provider_code)
        .map(|value| value.chars().take(128).collect())
}

fn health_registry_cooldown_until(now: i64, wait: Option<Duration>) -> i64 {
    let wait = wait.unwrap_or(DEFAULT_UNKNOWN_RATE_LIMIT_COOLDOWN);
    let wait_ms = i64::try_from(wait.as_millis()).unwrap_or(i64::MAX);
    now.saturating_add(wait_ms.max(1))
}

fn health_registry_reset_source(classified: &ClassifiedCodexUpstreamError) -> Option<String> {
    if classified.retry_after.is_some() {
        if matches!(
            classified.error_type,
            CodexLocalAccessErrorType::UsageLimitReached
                | CodexLocalAccessErrorType::InsufficientQuota
                | CodexLocalAccessErrorType::UpstreamRateLimit
                | CodexLocalAccessErrorType::ModelCapacity
        ) {
            return Some("upstream_reset_hint".to_string());
        }
    }
    None
}

fn update_health_registry_from_classified_error(
    registry: &mut CodexLocalAccessHealthRegistry,
    account_id: &str,
    model: Option<&str>,
    request_id: Option<&str>,
    classified: &ClassifiedCodexUpstreamError,
    now: i64,
) {
    registry.schema_version = CODEX_LOCAL_ACCESS_HEALTH_SCHEMA_VERSION;
    registry.updated_at = now;

    let safe_account_id = account_id.trim();
    if safe_account_id.is_empty() {
        registry.last_global_error = Some(CodexLocalAccessGlobalError {
            error_type: classified.error_type.as_str().to_string(),
            status: Some(classified.status),
            request_id: health_registry_request_id(request_id),
            updated_at: now,
        });
        return;
    }

    let safe_model = model.map(str::trim).filter(|m| !m.is_empty());
    let model_scoped_cooldown = is_model_scoped_cooldown(classified, safe_model);
    let cooldown_until = classified
        .retry_after
        .or_else(|| {
            matches!(
                classified.error_type,
                CodexLocalAccessErrorType::UpstreamRateLimit
                    | CodexLocalAccessErrorType::UsageLimitReached
                    | CodexLocalAccessErrorType::ModelCapacity
            )
            .then_some(DEFAULT_UNKNOWN_RATE_LIMIT_COOLDOWN)
        })
        .map(|wait| health_registry_cooldown_until(now, Some(wait)));
    let is_quota_zero_signal = matches!(
        classified.error_type,
        CodexLocalAccessErrorType::UsageLimitReached | CodexLocalAccessErrorType::InsufficientQuota
    );
    let account_cooldown_until = if model_scoped_cooldown {
        None
    } else {
        cooldown_until
    };
    let account_quota_zero_signal = is_quota_zero_signal;

    let (status, manual_required) = if classified.manual_required {
        (CodexLocalAccessAccountHealthStatus::ManualRequired, true)
    } else {
        match classified.error_type {
            CodexLocalAccessErrorType::AuthError
            | CodexLocalAccessErrorType::CaptchaOrSuspicious => {
                (CodexLocalAccessAccountHealthStatus::ManualRequired, true)
            }
            CodexLocalAccessErrorType::InsufficientQuota => {
                (CodexLocalAccessAccountHealthStatus::Exhausted, false)
            }
            CodexLocalAccessErrorType::UsageLimitReached
            | CodexLocalAccessErrorType::ModelCapacity
                if model_scoped_cooldown =>
            {
                (CodexLocalAccessAccountHealthStatus::Healthy, false)
            }
            CodexLocalAccessErrorType::UsageLimitReached
            | CodexLocalAccessErrorType::UpstreamRateLimit
            | CodexLocalAccessErrorType::ModelCapacity => {
                (CodexLocalAccessAccountHealthStatus::CoolingDown, false)
            }
            _ => (CodexLocalAccessAccountHealthStatus::Healthy, false),
        }
    };

    let request_id = health_registry_request_id(request_id);
    let previous_account_health = registry
        .accounts
        .get(safe_account_id)
        .cloned()
        .unwrap_or_default();
    registry.accounts.insert(
        safe_account_id.to_string(),
        CodexLocalAccessAccountHealth {
            status,
            cooldown_until_ms: account_cooldown_until,
            exhausted_at_ms: account_quota_zero_signal.then_some(now),
            estimated_reset_at_ms: cooldown_until.filter(|_| {
                matches!(
                    classified.error_type,
                    CodexLocalAccessErrorType::UsageLimitReached
                        | CodexLocalAccessErrorType::InsufficientQuota
                )
            }),
            estimated_remaining_percentage: account_quota_zero_signal.then_some(0),
            last_observed_remaining_percentage: account_quota_zero_signal.then_some(0),
            reset_source: cooldown_until.and_then(|_| health_registry_reset_source(classified)),
            confidence: account_quota_zero_signal.then_some("confirmed".to_string()),
            manual_required,
            last_status: Some(classified.status),
            last_error_type: Some(classified.error_type.as_str().to_string()),
            last_provider_code: classified.provider_code.clone(),
            last_request_id: request_id.clone(),
            last_selected_at_ms: previous_account_health.last_selected_at_ms,
            last_success_at_ms: previous_account_health.last_success_at_ms,
            last_quota_exhausted_at_ms: account_quota_zero_signal
                .then_some(now)
                .or(previous_account_health.last_quota_exhausted_at_ms),
            api_service_success_count: previous_account_health.api_service_success_count,
            updated_at: now,
            ..CodexLocalAccessAccountHealth::default()
        },
    );

    if let (Some(model), Some(cooldown_until)) = (safe_model, cooldown_until) {
        let key = health_registry_model_key(safe_account_id, model);
        registry.model_cooldowns.insert(
            key,
            CodexLocalAccessModelCooldown {
                account_id: safe_account_id.to_string(),
                model: model.to_string(),
                cooldown_until_ms: cooldown_until,
                last_error_type: Some(classified.error_type.as_str().to_string()),
                last_request_id: request_id,
                updated_at: now,
            },
        );
    }
}

fn recover_health_registry_account(
    registry: &mut CodexLocalAccessHealthRegistry,
    account_id: &str,
    model: Option<&str>,
    now: i64,
) -> bool {
    let account_id = account_id.trim();
    if account_id.is_empty() {
        return false;
    }

    let model = model.map(str::trim).filter(|value| !value.is_empty());
    let mut changed = false;

    if let Some(model) = model {
        changed |= registry
            .model_cooldowns
            .remove(&health_registry_model_key(account_id, model))
            .is_some();
    } else {
        let before_len = registry.model_cooldowns.len();
        let model_key_prefix = format!("{}::", account_id);
        registry.model_cooldowns.retain(|key, cooldown| {
            cooldown.account_id.trim() != account_id && !key.starts_with(&model_key_prefix)
        });
        changed |= registry.model_cooldowns.len() != before_len;

        if let Some(account) = registry.accounts.get_mut(account_id) {
            let should_recover = account.manual_required
                || matches!(
                    account.status,
                    CodexLocalAccessAccountHealthStatus::CoolingDown
                        | CodexLocalAccessAccountHealthStatus::Exhausted
                        | CodexLocalAccessAccountHealthStatus::AuthSuspect
                        | CodexLocalAccessAccountHealthStatus::ManualRequired
                        | CodexLocalAccessAccountHealthStatus::Disabled
                );

            if should_recover {
                account.status = CodexLocalAccessAccountHealthStatus::EstimatedAvailable;
                account.cooldown_until_ms = None;
                account.exhausted_at_ms = None;
                account.estimated_reset_at_ms = None;
                account.estimated_remaining_percentage = Some(100);
                account.reset_source = Some("manual_recovery".to_string());
                account.confidence = Some("manual".to_string());
                account.manual_required = false;
                account.last_status = None;
                account.last_error_type = None;
                account.last_provider_code = None;
                account.last_request_id = None;
                account.updated_at = now;
                changed = true;
            }
        }
    }

    if changed {
        registry.schema_version = CODEX_LOCAL_ACCESS_HEALTH_SCHEMA_VERSION;
        registry.updated_at = now;
    }

    changed
}

fn pause_health_registry_account(
    registry: &mut CodexLocalAccessHealthRegistry,
    account_id: &str,
    now: i64,
) -> bool {
    let account_id = account_id.trim();
    if account_id.is_empty() {
        return false;
    }

    let mut next = registry
        .accounts
        .get(account_id)
        .cloned()
        .unwrap_or_default();
    next.status = CodexLocalAccessAccountHealthStatus::Disabled;
    next.cooldown_until_ms = None;
    next.exhausted_at_ms = None;
    next.estimated_reset_at_ms = None;
    next.reset_source = Some("manual_pause".to_string());
    next.confidence = Some("manual".to_string());
    next.manual_required = false;
    next.last_status = None;
    next.last_error_type = Some("manual_paused".to_string());
    next.last_provider_code = None;
    next.last_request_id = Some("manual_pause".to_string());
    next.updated_at = now;

    let mut changed = registry.accounts.get(account_id) != Some(&next);
    registry.accounts.insert(account_id.to_string(), next);

    let before_sticky_len = registry.sticky_bindings.len();
    registry
        .sticky_bindings
        .retain(|_, binding| binding.account_id.trim() != account_id);
    changed |= registry.sticky_bindings.len() != before_sticky_len;

    let before_affinity_len = registry.request_affinity.len();
    registry
        .request_affinity
        .retain(|_, binding| binding.account_id.trim() != account_id);
    changed |= registry.request_affinity.len() != before_affinity_len;

    let before_model_cooldown_len = registry.model_cooldowns.len();
    registry
        .model_cooldowns
        .retain(|_, cooldown| cooldown.account_id.trim() != account_id);
    changed |= registry.model_cooldowns.len() != before_model_cooldown_len;

    if changed {
        registry.schema_version = CODEX_LOCAL_ACCESS_HEALTH_SCHEMA_VERSION;
        registry.updated_at = now;
    }

    changed
}

fn health_registry_account_is_schedulable(
    registry: &CodexLocalAccessHealthRegistry,
    account_id: &str,
    model: Option<&str>,
    now: i64,
) -> bool {
    let account_id = account_id.trim();
    if account_id.is_empty() {
        return false;
    }

    if let Some(account) = registry.accounts.get(account_id) {
        match account.status {
            CodexLocalAccessAccountHealthStatus::Healthy
            | CodexLocalAccessAccountHealthStatus::EstimatedAvailable => {}
            CodexLocalAccessAccountHealthStatus::CoolingDown => {
                if account.cooldown_until_ms.unwrap_or(i64::MAX) > now {
                    return false;
                }
            }
            CodexLocalAccessAccountHealthStatus::Exhausted
            | CodexLocalAccessAccountHealthStatus::AuthSuspect
            | CodexLocalAccessAccountHealthStatus::ManualRequired
            | CodexLocalAccessAccountHealthStatus::Disabled => return false,
        }
        if account.manual_required {
            return false;
        }
    }

    if let Some(model) = model.map(str::trim).filter(|value| !value.is_empty()) {
        let key = health_registry_model_key(account_id, model);
        if registry
            .model_cooldowns
            .get(&key)
            .map(|cooldown| cooldown.cooldown_until_ms > now)
            .unwrap_or(false)
        {
            return false;
        }
    }

    true
}

fn health_registry_account_has_hard_block(
    registry: &CodexLocalAccessHealthRegistry,
    account_id: &str,
) -> bool {
    let account_id = account_id.trim();
    if account_id.is_empty() {
        return true;
    }

    registry
        .accounts
        .get(account_id)
        .map(|account| {
            account.manual_required
                || matches!(
                    account.status,
                    CodexLocalAccessAccountHealthStatus::AuthSuspect
                        | CodexLocalAccessAccountHealthStatus::ManualRequired
                        | CodexLocalAccessAccountHealthStatus::Disabled
                )
        })
        .unwrap_or(false)
}

fn cooldown_wait_from_until_ms(until_ms: Option<i64>, now: i64) -> Option<Duration> {
    let until_ms = until_ms?;
    let wait_ms = until_ms.saturating_sub(now);
    if wait_ms <= 0 {
        return None;
    }
    Some(Duration::from_millis(wait_ms as u64))
}

fn min_cooldown_wait(current: Option<Duration>, candidate: Duration) -> Option<Duration> {
    Some(match current {
        Some(existing) if existing <= candidate => existing,
        _ => candidate,
    })
}

fn health_registry_account_cooldown_wait(
    registry: &CodexLocalAccessHealthRegistry,
    account_id: &str,
    model: Option<&str>,
    now: i64,
) -> Option<Duration> {
    let mut wait: Option<Duration> = None;

    if let Some(account) = registry.accounts.get(account_id.trim()) {
        match account.status {
            CodexLocalAccessAccountHealthStatus::CoolingDown => {
                if let Some(candidate) = cooldown_wait_from_until_ms(account.cooldown_until_ms, now)
                {
                    wait = min_cooldown_wait(wait, candidate);
                }
            }
            CodexLocalAccessAccountHealthStatus::Exhausted => {
                if let Some(candidate) = cooldown_wait_from_until_ms(
                    account.estimated_reset_at_ms.or(account.cooldown_until_ms),
                    now,
                ) {
                    wait = min_cooldown_wait(wait, candidate);
                }
            }
            CodexLocalAccessAccountHealthStatus::Healthy
            | CodexLocalAccessAccountHealthStatus::EstimatedAvailable
            | CodexLocalAccessAccountHealthStatus::AuthSuspect
            | CodexLocalAccessAccountHealthStatus::ManualRequired
            | CodexLocalAccessAccountHealthStatus::Disabled => {}
        }
    }

    if let Some(model) = model.map(str::trim).filter(|value| !value.is_empty()) {
        let key = health_registry_model_key(account_id, model);
        if let Some(candidate) = registry
            .model_cooldowns
            .get(&key)
            .and_then(|cooldown| cooldown_wait_from_until_ms(Some(cooldown.cooldown_until_ms), now))
        {
            wait = min_cooldown_wait(wait, candidate);
        }
    }

    wait
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct HealthRegistryAccountSortKey {
    bucket: u8,
    remaining_percentage: i32,
    asc_time_ms: i64,
    continuity_time_ms: i64,
    updated_at_ms: i64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct AccountQuotaSortHint {
    remaining_percentage: Option<i32>,
    earliest_reset_at_ms: Option<i64>,
}

fn account_quota_sort_hint(account: &CodexAccount) -> AccountQuotaSortHint {
    AccountQuotaSortHint {
        remaining_percentage: resolve_remaining_quota(account),
        earliest_reset_at_ms: resolve_earliest_quota_reset_ms(account),
    }
}

fn load_account_quota_sort_hints(account_ids: &[String]) -> HashMap<String, AccountQuotaSortHint> {
    account_ids
        .iter()
        .filter_map(|account_id| {
            let account_id = account_id.trim();
            if account_id.is_empty() {
                return None;
            }
            let account = try_get_cached_account_for_routing(account_id)
                .or_else(|| codex_account::load_account(account_id))?;
            Some((account_id.to_string(), account_quota_sort_hint(&account)))
        })
        .collect()
}

fn account_continuity_time_ms(account: &CodexLocalAccessAccountHealth) -> i64 {
    account
        .last_success_at_ms
        .or(account.last_selected_at_ms)
        .unwrap_or(account.updated_at)
}

fn health_registry_account_sort_key(
    registry: &CodexLocalAccessHealthRegistry,
    account_id: &str,
    now: i64,
    quota_hint: AccountQuotaSortHint,
) -> HealthRegistryAccountSortKey {
    let quota_remaining = quota_hint.remaining_percentage;
    let quota_reset_at_ms = quota_hint.earliest_reset_at_ms.unwrap_or(i64::MAX);
    let Some(account) = registry.accounts.get(account_id.trim()) else {
        let has_quota_hint =
            quota_hint.remaining_percentage.is_some() || quota_hint.earliest_reset_at_ms.is_some();
        return HealthRegistryAccountSortKey {
            bucket: if has_quota_hint { 0 } else { 2 },
            remaining_percentage: quota_remaining.unwrap_or(0),
            asc_time_ms: quota_reset_at_ms,
            continuity_time_ms: 0,
            updated_at_ms: 0,
        };
    };

    let continuity_time_ms = account_continuity_time_ms(account);
    match account.status {
        CodexLocalAccessAccountHealthStatus::Healthy => HealthRegistryAccountSortKey {
            bucket: 0,
            remaining_percentage: quota_remaining
                .or(account.estimated_remaining_percentage)
                .or(account.last_observed_remaining_percentage)
                .unwrap_or(100),
            asc_time_ms: quota_reset_at_ms,
            continuity_time_ms,
            updated_at_ms: account.updated_at,
        },
        CodexLocalAccessAccountHealthStatus::EstimatedAvailable => HealthRegistryAccountSortKey {
            bucket: 1,
            remaining_percentage: quota_remaining
                .or(account.estimated_remaining_percentage)
                .unwrap_or(100),
            asc_time_ms: quota_hint
                .earliest_reset_at_ms
                .or(account.last_quota_exhausted_at_ms)
                .or(account.estimated_reset_at_ms)
                .unwrap_or(account.updated_at),
            continuity_time_ms,
            updated_at_ms: account.updated_at,
        },
        CodexLocalAccessAccountHealthStatus::CoolingDown => {
            let reset_at = account.cooldown_until_ms.unwrap_or(i64::MAX);
            if reset_at <= now {
                HealthRegistryAccountSortKey {
                    bucket: 1,
                    remaining_percentage: quota_remaining
                        .or(account.estimated_remaining_percentage)
                        .unwrap_or(100),
                    asc_time_ms: quota_hint
                        .earliest_reset_at_ms
                        .or(account.last_quota_exhausted_at_ms)
                        .unwrap_or(reset_at),
                    continuity_time_ms,
                    updated_at_ms: account.updated_at,
                }
            } else {
                HealthRegistryAccountSortKey {
                    bucket: 5,
                    remaining_percentage: account.last_observed_remaining_percentage.unwrap_or(0),
                    asc_time_ms: reset_at,
                    continuity_time_ms,
                    updated_at_ms: account.updated_at,
                }
            }
        }
        CodexLocalAccessAccountHealthStatus::Exhausted => {
            let reset_at = account.estimated_reset_at_ms.unwrap_or(i64::MAX);
            if reset_at <= now {
                HealthRegistryAccountSortKey {
                    bucket: 1,
                    remaining_percentage: quota_remaining
                        .or(account.estimated_remaining_percentage)
                        .unwrap_or(100),
                    asc_time_ms: quota_hint
                        .earliest_reset_at_ms
                        .or(account.last_quota_exhausted_at_ms)
                        .unwrap_or(reset_at),
                    continuity_time_ms,
                    updated_at_ms: account.updated_at,
                }
            } else {
                HealthRegistryAccountSortKey {
                    bucket: 6,
                    remaining_percentage: account.last_observed_remaining_percentage.unwrap_or(0),
                    asc_time_ms: reset_at,
                    continuity_time_ms,
                    updated_at_ms: account.updated_at,
                }
            }
        }
        CodexLocalAccessAccountHealthStatus::AuthSuspect
        | CodexLocalAccessAccountHealthStatus::ManualRequired => HealthRegistryAccountSortKey {
            bucket: 7,
            remaining_percentage: 0,
            asc_time_ms: account.updated_at,
            continuity_time_ms,
            updated_at_ms: account.updated_at,
        },
        CodexLocalAccessAccountHealthStatus::Disabled => HealthRegistryAccountSortKey {
            bucket: 8,
            remaining_percentage: 0,
            asc_time_ms: account.updated_at,
            continuity_time_ms,
            updated_at_ms: account.updated_at,
        },
    }
}

fn sort_account_ids_by_health_estimate(
    account_ids: &mut [String],
    registry: &CodexLocalAccessHealthRegistry,
    now: i64,
) {
    let quota_hints = load_account_quota_sort_hints(account_ids);
    sort_account_ids_by_health_estimate_with_quota_hints(account_ids, registry, now, &quota_hints);
}

fn sort_account_ids_by_health_estimate_with_quota_hints(
    account_ids: &mut [String],
    registry: &CodexLocalAccessHealthRegistry,
    now: i64,
    quota_hints: &HashMap<String, AccountQuotaSortHint>,
) {
    account_ids.sort_by(|left, right| {
        let left_key = health_registry_account_sort_key(
            registry,
            left,
            now,
            quota_hints.get(left.trim()).copied().unwrap_or_default(),
        );
        let right_key = health_registry_account_sort_key(
            registry,
            right,
            now,
            quota_hints.get(right.trim()).copied().unwrap_or_default(),
        );
        left_key
            .bucket
            .cmp(&right_key.bucket)
            .then_with(|| {
                right_key
                    .remaining_percentage
                    .cmp(&left_key.remaining_percentage)
            })
            .then_with(|| left_key.asc_time_ms.cmp(&right_key.asc_time_ms))
            .then_with(|| {
                right_key
                    .continuity_time_ms
                    .cmp(&left_key.continuity_time_ms)
            })
            .then_with(|| right_key.updated_at_ms.cmp(&left_key.updated_at_ms))
            .then_with(|| left.cmp(right))
    });
}

fn update_health_summary_nearest_cooldown(
    summary: &mut CodexLocalAccessHealthSummary,
    now: i64,
    cooldown_until_ms: Option<i64>,
) {
    let Some(cooldown_until_ms) = cooldown_until_ms else {
        return;
    };
    if cooldown_until_ms <= now {
        return;
    }
    summary.nearest_cooldown_until_ms = Some(match summary.nearest_cooldown_until_ms {
        Some(current) if current <= cooldown_until_ms => current,
        _ => cooldown_until_ms,
    });
}

#[cfg(test)]
fn build_health_summary_from_registry(
    registry: &CodexLocalAccessHealthRegistry,
    now: i64,
) -> CodexLocalAccessHealthSummary {
    build_health_summary_from_registry_with_scope(registry, now, None)
}

fn build_health_summary_from_registry_for_accounts(
    registry: &CodexLocalAccessHealthRegistry,
    now: i64,
    account_ids: &[String],
) -> CodexLocalAccessHealthSummary {
    let scoped_account_ids: HashSet<&str> = account_ids
        .iter()
        .map(|account_id| account_id.trim())
        .filter(|account_id| !account_id.is_empty())
        .collect();
    build_health_summary_from_registry_with_scope(registry, now, Some(&scoped_account_ids))
}

fn build_health_summary_from_registry_with_scope(
    registry: &CodexLocalAccessHealthRegistry,
    now: i64,
    account_scope: Option<&HashSet<&str>>,
) -> CodexLocalAccessHealthSummary {
    let mut summary = CodexLocalAccessHealthSummary {
        schema_version: registry.schema_version,
        updated_at: registry.updated_at,
        ..CodexLocalAccessHealthSummary::default()
    };

    let mut last_error_updated_at = i64::MIN;
    let mut observed_scoped_accounts: HashSet<&str> = HashSet::new();
    let mut active_model_cooldowns_by_account: BTreeMap<String, (usize, Option<i64>)> =
        BTreeMap::new();
    let account_is_in_scope = |account_id: &str| {
        account_scope
            .map(|scope| scope.contains(account_id.trim()))
            .unwrap_or(true)
    };

    for (account_id, account) in registry.accounts.iter() {
        if !account_is_in_scope(account_id) {
            continue;
        }
        observed_scoped_accounts.insert(account_id.as_str());
        match account.status {
            CodexLocalAccessAccountHealthStatus::Healthy => summary.healthy_count += 1,
            CodexLocalAccessAccountHealthStatus::EstimatedAvailable => {
                summary.estimated_available_count += 1
            }
            CodexLocalAccessAccountHealthStatus::CoolingDown => summary.cooling_count += 1,
            CodexLocalAccessAccountHealthStatus::Exhausted => summary.exhausted_count += 1,
            CodexLocalAccessAccountHealthStatus::AuthSuspect => summary.auth_suspect_count += 1,
            CodexLocalAccessAccountHealthStatus::ManualRequired => {
                summary.manual_required_count += 1
            }
            CodexLocalAccessAccountHealthStatus::Disabled => summary.disabled_count += 1,
        }
        if account.manual_required
            && !matches!(
                account.status,
                CodexLocalAccessAccountHealthStatus::ManualRequired
            )
        {
            summary.manual_required_count += 1;
        }
        update_health_summary_nearest_cooldown(&mut summary, now, account.cooldown_until_ms);
        update_health_summary_nearest_cooldown(&mut summary, now, account.estimated_reset_at_ms);

        if account.last_error_type.is_some() && account.updated_at >= last_error_updated_at {
            last_error_updated_at = account.updated_at;
            summary.last_error_type = account.last_error_type.clone();
            summary.last_status = account.last_status;
            summary.last_request_id = account.last_request_id.clone();
        }
    }

    if let Some(scope) = account_scope {
        for account_id in scope {
            if !observed_scoped_accounts.contains(account_id) {
                summary.healthy_count += 1;
            }
        }
    }

    for cooldown in registry.model_cooldowns.values() {
        if !account_is_in_scope(cooldown.account_id.as_str()) {
            continue;
        }
        if cooldown.cooldown_until_ms > now {
            summary.active_model_cooldown_count += 1;
            update_health_summary_nearest_cooldown(
                &mut summary,
                now,
                Some(cooldown.cooldown_until_ms),
            );
            let entry = active_model_cooldowns_by_account
                .entry(cooldown.account_id.trim().to_string())
                .or_insert((0, None));
            entry.0 += 1;
            entry.1 = Some(match entry.1 {
                Some(current) if current <= cooldown.cooldown_until_ms => current,
                _ => cooldown.cooldown_until_ms,
            });
        }
        if cooldown.last_error_type.is_some() && cooldown.updated_at >= last_error_updated_at {
            last_error_updated_at = cooldown.updated_at;
            summary.last_error_type = cooldown.last_error_type.clone();
            summary.last_status = None;
            summary.last_request_id = cooldown.last_request_id.clone();
        }
    }

    if let Some(global_error) = registry.last_global_error.as_ref() {
        if global_error.updated_at >= last_error_updated_at {
            summary.last_error_type = Some(global_error.error_type.clone());
            summary.last_status = global_error.status;
            summary.last_request_id = global_error.request_id.clone();
        }
    }

    if let Some(binding) = registry.sticky_bindings.get(PROCESS_STICKY_BINDING_KEY) {
        if binding.expires_at_ms > now && account_is_in_scope(binding.account_id.as_str()) {
            let account_hash = failure_log_account_hash(Some(binding.account_id.as_str()));
            if account_hash != "-" {
                summary.sticky_account_hash = Some(account_hash);
            }
            summary.sticky_reason = sanitize_provider_code(binding.reason.as_str());
            summary.sticky_expires_at_ms = Some(binding.expires_at_ms);
        }
    }

    if let Some(scope) = account_scope {
        let mut account_ids_for_views: Vec<String> = scope
            .iter()
            .map(|account_id| (*account_id).to_string())
            .collect();
        account_ids_for_views.sort();

        let default_account = CodexLocalAccessAccountHealth::default();
        summary.accounts = account_ids_for_views
            .into_iter()
            .map(|account_id| {
                let account = registry
                    .accounts
                    .get(account_id.as_str())
                    .unwrap_or(&default_account);
                let (active_model_cooldown_count, nearest_model_cooldown_until_ms) =
                    active_model_cooldowns_by_account
                        .get(account_id.as_str())
                        .cloned()
                        .unwrap_or((0, None));
                CodexLocalAccessAccountHealthView {
                    account_id,
                    status: account.status,
                    manual_required: account.manual_required,
                    cooldown_until_ms: account.cooldown_until_ms,
                    exhausted_at_ms: account.exhausted_at_ms,
                    estimated_reset_at_ms: account.estimated_reset_at_ms,
                    last_status: account.last_status,
                    last_error_type: account.last_error_type.clone(),
                    last_provider_code: account
                        .last_provider_code
                        .as_deref()
                        .and_then(sanitize_provider_code),
                    updated_at: account.updated_at,
                    active_model_cooldown_count,
                    nearest_model_cooldown_until_ms,
                }
            })
            .collect();
    }

    summary
}

fn build_unavailable_health_summary(now: i64, err: &str) -> CodexLocalAccessHealthSummary {
    CodexLocalAccessHealthSummary {
        schema_version: CODEX_LOCAL_ACCESS_HEALTH_SCHEMA_VERSION,
        updated_at: now,
        unavailable: true,
        load_error: Some(safe_log_field(Some(err), 240)),
        ..CodexLocalAccessHealthSummary::default()
    }
}

fn build_health_summary_from_disk_for_accounts(
    account_ids: &[String],
) -> CodexLocalAccessHealthSummary {
    let now = now_ms();
    let mut summary = match load_health_registry_from_disk() {
        Ok(registry) => {
            build_health_summary_from_registry_for_accounts(&registry, now, account_ids)
        }
        Err(err) => build_unavailable_health_summary(now, &err),
    };
    apply_audit_trail_status_to_health_summary(&mut summary, &current_audit_trail_status());
    summary
}

fn process_sticky_account_id(
    registry: &CodexLocalAccessHealthRegistry,
    account_ids: &[String],
    model: Option<&str>,
    now: i64,
) -> Option<String> {
    let binding = registry.sticky_bindings.get(PROCESS_STICKY_BINDING_KEY)?;
    let account_id = binding.account_id.trim();
    if account_id.is_empty() {
        return None;
    }
    if binding.expires_at_ms <= now {
        return None;
    }
    if !account_ids.iter().any(|candidate| candidate == account_id) {
        return None;
    }
    if !health_registry_account_is_schedulable(registry, account_id, model, now) {
        return None;
    }
    Some(account_id.to_string())
}

fn pin_process_sticky_account(
    account_ids: Vec<String>,
    registry: &CodexLocalAccessHealthRegistry,
    model: Option<&str>,
    now: i64,
) -> Vec<String> {
    let sticky_account_id = process_sticky_account_id(registry, &account_ids, model, now);
    pin_account_to_front(account_ids, sticky_account_id.as_deref())
}

fn upsert_process_sticky_binding(
    registry: &mut CodexLocalAccessHealthRegistry,
    account_id: &str,
    now: i64,
) -> bool {
    let account_id = account_id.trim();
    if account_id.is_empty() {
        return false;
    }

    let binding = CodexLocalAccessStickyBinding {
        binding_key: PROCESS_STICKY_BINDING_KEY.to_string(),
        account_id: account_id.to_string(),
        reason: PROCESS_STICKY_BINDING_REASON.to_string(),
        expires_at_ms: now.saturating_add(PROCESS_STICKY_BINDING_TTL_MS),
        updated_at: now,
    };
    let changed = registry
        .sticky_bindings
        .get(PROCESS_STICKY_BINDING_KEY)
        .map(|current| current != &binding)
        .unwrap_or(true);
    if changed {
        registry.schema_version = CODEX_LOCAL_ACCESS_HEALTH_SCHEMA_VERSION;
        registry.updated_at = now;
        registry
            .sticky_bindings
            .insert(PROCESS_STICKY_BINDING_KEY.to_string(), binding);
    }
    changed
}

fn prune_persisted_request_affinity_bindings(
    registry: &mut CodexLocalAccessHealthRegistry,
    account_ids: Option<&[String]>,
    now: i64,
) -> bool {
    let account_scope = account_ids.map(|ids| {
        ids.iter()
            .map(|account_id| account_id.trim())
            .filter(|account_id| !account_id.is_empty())
            .collect::<HashSet<&str>>()
    });
    let before = registry.request_affinity.len();
    registry.request_affinity.retain(|request_id, binding| {
        let account_id = binding.account_id.trim();
        !request_id.trim().is_empty()
            && !account_id.is_empty()
            && binding.expires_at_ms > now
            && account_scope
                .as_ref()
                .map(|scope| scope.contains(account_id))
                .unwrap_or(true)
    });

    if registry.request_affinity.len() > MAX_REQUEST_AFFINITY_BINDINGS {
        let remove_count = registry
            .request_affinity
            .len()
            .saturating_sub(MAX_REQUEST_AFFINITY_BINDINGS);
        let mut oldest: Vec<(String, i64)> = registry
            .request_affinity
            .iter()
            .map(|(request_id, binding)| (request_id.clone(), binding.updated_at))
            .collect();
        oldest.sort_by(|left, right| left.1.cmp(&right.1).then_with(|| left.0.cmp(&right.0)));
        for (request_id, _) in oldest.into_iter().take(remove_count) {
            registry.request_affinity.remove(&request_id);
        }
    }

    let changed = registry.request_affinity.len() != before;
    if changed {
        registry.schema_version = CODEX_LOCAL_ACCESS_HEALTH_SCHEMA_VERSION;
        registry.updated_at = now;
    }
    changed
}

fn request_affinity_account_from_registry(
    registry: &CodexLocalAccessHealthRegistry,
    request: &ParsedRequest,
    now: i64,
) -> Option<String> {
    let request_id = request_affinity_key(request)?;
    let binding = registry.request_affinity.get(&request_id)?;
    let account_id = binding.account_id.trim();
    if account_id.is_empty() || binding.expires_at_ms <= now {
        return None;
    }
    Some(account_id.to_string())
}

fn upsert_request_affinity_binding(
    registry: &mut CodexLocalAccessHealthRegistry,
    request: &ParsedRequest,
    account_id: &str,
    now: i64,
) -> bool {
    let Some(request_id) = request_affinity_key(request) else {
        return false;
    };
    upsert_request_affinity_binding_key(registry, &request_id, account_id, now)
}

fn upsert_request_affinity_binding_key(
    registry: &mut CodexLocalAccessHealthRegistry,
    request_id: &str,
    account_id: &str,
    now: i64,
) -> bool {
    let request_id = request_id.trim();
    if request_id.is_empty() {
        return false;
    }
    let account_id = account_id.trim();
    if account_id.is_empty() {
        return false;
    }

    let binding = CodexLocalAccessStickyBinding {
        binding_key: request_id.to_string(),
        account_id: account_id.to_string(),
        reason: REQUEST_AFFINITY_BINDING_REASON.to_string(),
        expires_at_ms: now.saturating_add(REQUEST_AFFINITY_TTL_MS),
        updated_at: now,
    };
    let changed = registry
        .request_affinity
        .get(request_id)
        .map(|current| current != &binding)
        .unwrap_or(true);
    if changed {
        registry.schema_version = CODEX_LOCAL_ACCESS_HEALTH_SCHEMA_VERSION;
        registry.updated_at = now;
        registry
            .request_affinity
            .insert(request_id.to_string(), binding);
    }
    prune_persisted_request_affinity_bindings(registry, None, now) || changed
}

fn upsert_successful_account_health(
    registry: &mut CodexLocalAccessHealthRegistry,
    account_id: &str,
    now: i64,
) -> bool {
    let account_id = account_id.trim();
    if account_id.is_empty() {
        return false;
    }

    let mut next = registry
        .accounts
        .get(account_id)
        .cloned()
        .unwrap_or_default();
    let previous = next.clone();

    next.status = CodexLocalAccessAccountHealthStatus::Healthy;
    next.cooldown_until_ms = None;
    next.exhausted_at_ms = None;
    next.estimated_reset_at_ms = None;
    next.estimated_remaining_percentage = None;
    next.manual_required = false;
    next.last_status = Some(StatusCode::OK.as_u16());
    next.last_error_type = None;
    next.last_provider_code = None;
    next.last_request_id = None;
    next.last_selected_at_ms = Some(now);
    next.last_success_at_ms = Some(now);
    next.api_service_success_count = next.api_service_success_count.saturating_add(1);
    next.updated_at = now;

    if next == previous {
        return false;
    }

    registry.schema_version = CODEX_LOCAL_ACCESS_HEALTH_SCHEMA_VERSION;
    registry.updated_at = now;
    registry.accounts.insert(account_id.to_string(), next);
    true
}

fn prune_process_sticky_binding(
    registry: &mut CodexLocalAccessHealthRegistry,
    account_ids: &[String],
    model: Option<&str>,
    now: i64,
) -> bool {
    let Some(binding) = registry.sticky_bindings.get(PROCESS_STICKY_BINDING_KEY) else {
        return false;
    };
    let account_id = binding.account_id.trim();
    let should_remove = account_id.is_empty()
        || binding.expires_at_ms <= now
        || !account_ids.iter().any(|candidate| candidate == account_id)
        || !health_registry_account_is_schedulable(registry, account_id, model, now);
    if !should_remove {
        return false;
    }

    registry.sticky_bindings.remove(PROCESS_STICKY_BINDING_KEY);
    registry.schema_version = CODEX_LOCAL_ACCESS_HEALTH_SCHEMA_VERSION;
    registry.updated_at = now;
    true
}

fn persist_successful_routing_state(
    account_id: &str,
    request: &ParsedRequest,
    persist_process_sticky: bool,
) {
    let mut registry = match load_health_registry_from_disk() {
        Ok(registry) => registry,
        Err(err) => {
            log_health_registry_update_error(&err);
            return;
        }
    };
    let now = now_ms();
    let sticky_changed =
        persist_process_sticky && upsert_process_sticky_binding(&mut registry, account_id, now);
    let affinity_changed = upsert_request_affinity_binding(&mut registry, request, account_id, now);
    let health_changed = upsert_successful_account_health(&mut registry, account_id, now);
    if !sticky_changed && !affinity_changed && !health_changed {
        return;
    }
    if let Err(err) = save_health_registry_to_disk(&registry) {
        log_health_registry_update_error(&err);
        return;
    }
    if !sticky_changed && !affinity_changed {
        return;
    }

    let context = build_audit_context(request, Some(account_id));
    record_audit_event_from_context(
        &context,
        "selector",
        None,
        None,
        None,
        Some(if sticky_changed {
            "sticky_bound"
        } else {
            "request_affinity_bound"
        }),
        BTreeMap::from([(
            "binding_key".to_string(),
            if sticky_changed {
                PROCESS_STICKY_BINDING_KEY.to_string()
            } else {
                request_affinity_key(request).unwrap_or_default()
            },
        )]),
    );
}

fn persist_request_affinity_key(account_id: &str, request: &ParsedRequest, request_id: &str) {
    let mut registry = match load_health_registry_from_disk() {
        Ok(registry) => registry,
        Err(err) => {
            log_health_registry_update_error(&err);
            return;
        }
    };
    let now = now_ms();
    if !upsert_request_affinity_binding_key(&mut registry, request_id, account_id, now) {
        return;
    }
    if let Err(err) = save_health_registry_to_disk(&registry) {
        log_health_registry_update_error(&err);
        return;
    }

    let context = build_audit_context(request, Some(account_id));
    record_audit_event_from_context(
        &context,
        "selector",
        None,
        None,
        None,
        Some("request_affinity_bound"),
        BTreeMap::from([
            ("binding_key".to_string(), request_id.to_string()),
            ("source".to_string(), "response_turn_state".to_string()),
        ]),
    );
}

fn hashed_request_correlation_id(prefix: &str, value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }

    let digest = Sha256::digest(value.as_bytes());
    let hex = format!("{:x}", digest);
    Some(format!("{}:sha256:{}", prefix, &hex[..12]))
}

fn generated_codex_turn_state() -> String {
    let suffix: String = rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(32)
        .map(char::from)
        .collect();
    format!("{}{}", LOCAL_CODEX_TURN_STATE_PREFIX, suffix)
}

fn codex_turn_state_request_id(request: &ParsedRequest) -> Option<String> {
    official_codex_turn_state_affinity_key(request)
}

fn codex_turn_state_request_id_from_value(value: &str) -> Option<String> {
    hashed_request_correlation_id(X_CODEX_TURN_STATE_HEADER, value)
}

fn codex_turn_metadata_request_id_with_source(
    request: &ParsedRequest,
) -> Option<(String, &'static str)> {
    let value = request_header_value(request, X_CODEX_TURN_METADATA_HEADER)?;
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }

    if let Ok(metadata) = serde_json::from_str::<Value>(trimmed) {
        if let Some(turn_id) = metadata
            .get("turn_id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|turn_id| !turn_id.is_empty())
        {
            return hashed_request_correlation_id("x-codex-turn-metadata.turn_id", turn_id)
                .map(|request_id| (request_id, "codex_turn_metadata_turn_id"));
        }
    }

    hashed_request_correlation_id(X_CODEX_TURN_METADATA_HEADER, trimmed)
        .map(|request_id| (request_id, "codex_turn_metadata"))
}

fn codex_turn_metadata_request_id(request: &ParsedRequest) -> Option<String> {
    codex_turn_metadata_request_id_with_source(request).map(|(request_id, _)| request_id)
}

fn request_lineage_id_with_source(
    request: &ParsedRequest,
) -> (Option<String>, Option<&'static str>) {
    if let Some(value) = codex_turn_state_request_id(request) {
        return (Some(value), Some("codex_turn_state"));
    }
    if let Some((value, source)) = codex_turn_metadata_request_id_with_source(request) {
        return (Some(value), Some(source));
    }
    if let Some(previous_response_id) = build_request_routing_hint(request).previous_response_id {
        return (
            hashed_request_correlation_id("response", &previous_response_id),
            Some("previous_response_id"),
        );
    }

    (None, None)
}

fn previous_response_id_hash(request: &ParsedRequest) -> Option<String> {
    build_request_routing_hint(request)
        .previous_response_id
        .and_then(|value| hashed_request_correlation_id("response", &value))
}

fn request_body_value_contains_compaction_trigger(value: &Value) -> bool {
    match value {
        Value::Object(map) => {
            if map
                .get("type")
                .and_then(Value::as_str)
                .is_some_and(|value| value.eq_ignore_ascii_case("compaction_trigger"))
            {
                return true;
            }
            map.values()
                .any(request_body_value_contains_compaction_trigger)
        }
        Value::Array(items) => items
            .iter()
            .any(request_body_value_contains_compaction_trigger),
        _ => false,
    }
}

fn request_body_is_auto_compact_candidate(request: &ParsedRequest) -> bool {
    is_responses_request(&request.target)
        && parse_request_body_json(&request.body)
            .as_ref()
            .is_some_and(request_body_value_contains_compaction_trigger)
}

fn health_registry_request_id_from_request(request: &ParsedRequest) -> Option<String> {
    if let Some(value) = codex_turn_state_request_id(request) {
        return Some(value);
    }
    if let Some(value) = codex_turn_metadata_request_id(request) {
        return Some(value);
    }
    for header_name in [
        "x-client-request-id",
        "x-request-id",
        "request-id",
        "openai-request-id",
    ] {
        if let Some(value) = request_header_value(request, header_name) {
            return health_registry_request_id(Some(value));
        }
    }
    None
}

fn persist_health_registry_from_classified_error(
    account_id: &str,
    model: Option<&str>,
    request: &ParsedRequest,
    classified: &ClassifiedCodexUpstreamError,
) -> Result<(), String> {
    let mut registry = load_health_registry_from_disk()?;
    let request_id = health_registry_request_id_from_request(request);
    update_health_registry_from_classified_error(
        &mut registry,
        account_id,
        model,
        request_id.as_deref(),
        classified,
        now_ms(),
    );
    save_health_registry_to_disk(&registry)
}

fn log_health_registry_update_error(err: &str) {
    logger::log_warn(&format!(
        "[CodexLocalAccess][HealthRegistry] 更新 API 服务健康状态失败: {}",
        err
    ));
}

#[cfg(test)]
fn parse_codex_retry_after(status: StatusCode, error_body: &str) -> Option<Duration> {
    classify_codex_upstream_error(status, None, error_body).retry_after
}

fn empty_stats_snapshot() -> CodexLocalAccessStats {
    let now = now_ms();
    let day_since = now.saturating_sub(DAY_WINDOW_MS);
    let week_since = now.saturating_sub(WEEK_WINDOW_MS);
    let month_since = now.saturating_sub(MONTH_WINDOW_MS);
    CodexLocalAccessStats {
        since: now,
        updated_at: now,
        totals: CodexLocalAccessUsageStats::default(),
        accounts: Vec::new(),
        daily: CodexLocalAccessStatsWindow {
            since: day_since,
            updated_at: now,
            totals: CodexLocalAccessUsageStats::default(),
            accounts: Vec::new(),
        },
        weekly: CodexLocalAccessStatsWindow {
            since: week_since,
            updated_at: now,
            totals: CodexLocalAccessUsageStats::default(),
            accounts: Vec::new(),
        },
        monthly: CodexLocalAccessStatsWindow {
            since: month_since,
            updated_at: now,
            totals: CodexLocalAccessUsageStats::default(),
            accounts: Vec::new(),
        },
        events: Vec::new(),
    }
}

fn empty_stats_window(since: i64, updated_at: i64) -> CodexLocalAccessStatsWindow {
    CodexLocalAccessStatsWindow {
        since,
        updated_at,
        totals: CodexLocalAccessUsageStats::default(),
        accounts: Vec::new(),
    }
}

fn sort_usage_accounts(accounts: &mut [CodexLocalAccessAccountStats]) {
    accounts.sort_by(|left, right| {
        right
            .usage
            .request_count
            .cmp(&left.usage.request_count)
            .then_with(|| right.updated_at.cmp(&left.updated_at))
            .then_with(|| left.account_id.cmp(&right.account_id))
    });
}

fn trim_recent_events(events: &mut Vec<CodexLocalAccessUsageEvent>, month_since: i64) {
    events.retain(|event| event.timestamp > 0 && event.timestamp >= month_since);
    events.sort_by_key(|event| event.timestamp);
}

fn append_usage_event(
    events: &mut Vec<CodexLocalAccessUsageEvent>,
    now: i64,
    account_id: Option<&str>,
    account_email: Option<&str>,
    success: bool,
    latency_ms: u64,
    usage: Option<&UsageCapture>,
) {
    let usage = usage.cloned().unwrap_or_default();
    events.push(CodexLocalAccessUsageEvent {
        timestamp: now,
        account_id: account_id.unwrap_or_default().trim().to_string(),
        email: account_email.unwrap_or_default().trim().to_string(),
        success,
        latency_ms,
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        total_tokens: usage.total_tokens,
        cached_tokens: usage.cached_tokens,
        reasoning_tokens: usage.reasoning_tokens,
    });
}

fn apply_usage_event_to_window(
    window: &mut CodexLocalAccessStatsWindow,
    event: &CodexLocalAccessUsageEvent,
) {
    let usage = UsageCapture {
        input_tokens: event.input_tokens,
        output_tokens: event.output_tokens,
        total_tokens: event.total_tokens,
        cached_tokens: event.cached_tokens,
        reasoning_tokens: event.reasoning_tokens,
    };
    apply_usage_stats(
        &mut window.totals,
        event.success,
        event.latency_ms,
        Some(&usage),
    );
    upsert_account_usage_stats(
        &mut window.accounts,
        Some(event.account_id.as_str()),
        Some(event.email.as_str()),
        event.success,
        event.latency_ms,
        Some(&usage),
        event.timestamp,
    );
    window.updated_at = window.updated_at.max(event.timestamp);
}

fn recompute_time_windows(stats: &mut CodexLocalAccessStats, now: i64) {
    let day_since = now.saturating_sub(DAY_WINDOW_MS);
    let week_since = now.saturating_sub(WEEK_WINDOW_MS);
    let month_since = now.saturating_sub(MONTH_WINDOW_MS);

    trim_recent_events(&mut stats.events, month_since);

    let mut daily = empty_stats_window(day_since, stats.updated_at.max(day_since));
    let mut weekly = empty_stats_window(week_since, stats.updated_at.max(week_since));
    let mut monthly = empty_stats_window(month_since, stats.updated_at.max(month_since));

    for event in &stats.events {
        if event.timestamp >= month_since {
            apply_usage_event_to_window(&mut monthly, event);
        }
        if event.timestamp >= week_since {
            apply_usage_event_to_window(&mut weekly, event);
        }
        if event.timestamp >= day_since {
            apply_usage_event_to_window(&mut daily, event);
        }
    }

    sort_usage_accounts(&mut daily.accounts);
    sort_usage_accounts(&mut weekly.accounts);
    sort_usage_accounts(&mut monthly.accounts);

    stats.daily = daily;
    stats.weekly = weekly;
    stats.monthly = monthly;
}

fn build_api_port_url(port: u16) -> String {
    format!("http://{CODEX_LOCAL_ACCESS_URL_HOST}:{port}{CHAT_COMPLETIONS_PATH}")
}

fn build_base_url(port: u16) -> String {
    format!("http://{CODEX_LOCAL_ACCESS_URL_HOST}:{port}/v1")
}

fn build_lan_base_url(port: u16) -> Option<String> {
    resolve_primary_lan_ipv4().map(|addr| format!("http://{addr}:{port}/v1"))
}

#[derive(Debug)]
struct LanIpv4Candidate {
    interface_name: String,
    addr: Ipv4Addr,
}

fn resolve_primary_lan_ipv4() -> Option<Ipv4Addr> {
    let mut candidates = collect_private_lan_ipv4_candidates();
    candidates.sort_by_key(|candidate| {
        (
            lan_interface_score(&candidate.interface_name),
            lan_addr_score(candidate.addr),
            candidate.addr.octets(),
        )
    });
    candidates
        .into_iter()
        .next()
        .map(|candidate| candidate.addr)
}

fn is_lan_ipv4(addr: Ipv4Addr) -> bool {
    addr.is_private()
}

fn lan_interface_score(interface_name: &str) -> u8 {
    let name = interface_name.to_ascii_lowercase();
    if name.starts_with("en")
        || name.starts_with("eth")
        || name.starts_with("wlan")
        || name.starts_with("wi-fi")
        || name.starts_with("wifi")
        || name.starts_with("ethernet")
        || name.contains("wireless")
    {
        return 0;
    }
    if name.starts_with("lo")
        || name.starts_with("utun")
        || name.starts_with("tun")
        || name.starts_with("tap")
        || name.starts_with("awdl")
        || name.starts_with("llw")
        || name.starts_with("bridge")
        || name.starts_with("br-")
        || name.starts_with("docker")
        || name.starts_with("veth")
        || name.starts_with("virbr")
        || name.starts_with("vmnet")
        || name.starts_with("vbox")
        || name.starts_with("tailscale")
        || name.starts_with("wg")
    {
        return 2;
    }
    1
}

fn lan_addr_score(addr: Ipv4Addr) -> u8 {
    let octets = addr.octets();
    if octets[0] == 192 && octets[1] == 168 {
        return 0;
    }
    if octets[0] == 10 {
        return 1;
    }
    2
}

#[cfg(target_os = "macos")]
fn collect_private_lan_ipv4_candidates() -> Vec<LanIpv4Candidate> {
    let output = Command::new("ifconfig").arg("-a").output();
    match output {
        Ok(output) => parse_ifconfig_ipv4_candidates(&String::from_utf8_lossy(&output.stdout)),
        Err(_) => Vec::new(),
    }
}

#[cfg(target_os = "linux")]
fn collect_private_lan_ipv4_candidates() -> Vec<LanIpv4Candidate> {
    let output = Command::new("ip")
        .args(["-o", "-4", "addr", "show", "scope", "global"])
        .output();
    match output {
        Ok(output) => parse_linux_ip_addr_candidates(&String::from_utf8_lossy(&output.stdout)),
        Err(_) => Vec::new(),
    }
}

#[cfg(target_os = "windows")]
fn collect_private_lan_ipv4_candidates() -> Vec<LanIpv4Candidate> {
    let mut command = Command::new("ipconfig");
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    match command.output() {
        Ok(output) => parse_windows_ipconfig_candidates(&String::from_utf8_lossy(&output.stdout)),
        Err(_) => Vec::new(),
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn collect_private_lan_ipv4_candidates() -> Vec<LanIpv4Candidate> {
    Vec::new()
}

#[cfg(target_os = "macos")]
fn parse_ifconfig_ipv4_candidates(output: &str) -> Vec<LanIpv4Candidate> {
    let mut candidates = Vec::new();
    let mut current_interface = String::new();
    for line in output.lines() {
        if !line
            .chars()
            .next()
            .map(|item| item.is_whitespace())
            .unwrap_or(false)
        {
            current_interface = line
                .split(':')
                .next()
                .unwrap_or_default()
                .trim()
                .to_string();
            continue;
        }
        let mut parts = line.split_whitespace();
        while let Some(part) = parts.next() {
            if part != "inet" {
                continue;
            }
            let Some(raw_addr) = parts.next() else {
                continue;
            };
            if let Ok(addr) = raw_addr.parse::<Ipv4Addr>() {
                if is_lan_ipv4(addr) {
                    candidates.push(LanIpv4Candidate {
                        interface_name: current_interface.clone(),
                        addr,
                    });
                }
            }
        }
    }
    candidates
}

#[cfg(target_os = "linux")]
fn parse_linux_ip_addr_candidates(output: &str) -> Vec<LanIpv4Candidate> {
    let mut candidates = Vec::new();
    for line in output.lines() {
        let mut parts = line.split_whitespace();
        let _index = parts.next();
        let Some(interface_name) = parts.next() else {
            continue;
        };
        while let Some(part) = parts.next() {
            if part != "inet" {
                continue;
            }
            let Some(raw_addr) = parts.next() else {
                continue;
            };
            let addr_text = raw_addr.split('/').next().unwrap_or_default();
            if let Ok(addr) = addr_text.parse::<Ipv4Addr>() {
                if is_lan_ipv4(addr) {
                    candidates.push(LanIpv4Candidate {
                        interface_name: interface_name.trim_end_matches(':').to_string(),
                        addr,
                    });
                }
            }
        }
    }
    candidates
}

#[cfg(target_os = "windows")]
fn parse_windows_ipconfig_candidates(output: &str) -> Vec<LanIpv4Candidate> {
    let mut candidates = Vec::new();
    let mut current_interface = String::new();
    for line in output.lines() {
        let trimmed = line.trim();
        let is_indented = line
            .chars()
            .next()
            .map(|item| item.is_whitespace())
            .unwrap_or(false);
        if trimmed.ends_with(':') && !is_indented {
            current_interface = trimmed.trim_end_matches(':').to_string();
            continue;
        }
        if !trimmed.contains("IPv4") {
            continue;
        }
        let Some(raw_addr) = trimmed.rsplit(':').next() else {
            continue;
        };
        if let Ok(addr) = raw_addr.trim().parse::<Ipv4Addr>() {
            if is_lan_ipv4(addr) {
                candidates.push(LanIpv4Candidate {
                    interface_name: current_interface.clone(),
                    addr,
                });
            }
        }
    }
    candidates
}

fn build_runtime_account(
    base_url: String,
    api_key: String,
    current_account: &CodexAccount,
) -> CodexAccount {
    let _ = current_account;
    let mut runtime_account = CodexAccount::new_api_key(
        "codex_local_access_runtime".to_string(),
        "api-service-local".to_string(),
        api_key,
        CodexApiProviderMode::Custom,
        Some(base_url),
        Some(CODEX_LOCAL_ACCESS_RUNTIME_PROVIDER_ID.to_string()),
        Some(CODEX_LOCAL_ACCESS_RUNTIME_PROVIDER_NAME.to_string()),
    );
    runtime_account.account_name = Some("API Service".to_string());
    runtime_account
}

fn generate_local_api_key() -> String {
    let suffix: String = rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(32)
        .map(char::from)
        .collect();
    format!("agt_codex_{}", suffix)
}

fn allocate_random_local_port() -> Result<u16, String> {
    let listener = StdTcpListener::bind((CODEX_LOCAL_ACCESS_BIND_HOST, 0))
        .map_err(|e| format!("分配本地接入端口失败: {}", e))?;
    listener
        .local_addr()
        .map(|addr| addr.port())
        .map_err(|e| format!("读取本地接入端口失败: {}", e))
}

fn is_local_port_bindable(port: u16) -> bool {
    port != 0 && StdTcpListener::bind((CODEX_LOCAL_ACCESS_BIND_HOST, port)).is_ok()
}

fn first_stable_local_access_port(
    exclude_port: Option<u16>,
    is_bindable: impl Fn(u16) -> bool,
) -> Option<u16> {
    PREFERRED_CODEX_LOCAL_ACCESS_PORTS
        .iter()
        .copied()
        .filter(|port| Some(*port) != exclude_port)
        .find(|port| is_bindable(*port))
}

fn allocate_stable_local_access_port(exclude_port: Option<u16>) -> Result<u16, String> {
    first_stable_local_access_port(exclude_port, is_local_port_bindable)
        .map(Ok)
        .unwrap_or_else(allocate_random_local_port)
}

fn load_collection_from_disk() -> Result<Option<CodexLocalAccessCollection>, String> {
    let path = local_access_file_path()?;
    if !path.exists() {
        return Ok(None);
    }

    let content =
        std::fs::read_to_string(&path).map_err(|e| format!("读取本地接入配置失败: {}", e))?;
    let parsed = serde_json::from_str::<CodexLocalAccessCollection>(&content)
        .map_err(|e| format!("解析本地接入配置失败: {}", e))?;
    Ok(Some(parsed))
}

fn save_collection_to_disk(collection: &CodexLocalAccessCollection) -> Result<(), String> {
    let path = local_access_file_path()?;
    let content = serde_json::to_string_pretty(collection)
        .map_err(|e| format!("序列化本地接入配置失败: {}", e))?;
    write_string_atomic(&path, &content)
}

fn normalize_stats(stats: &mut CodexLocalAccessStats) {
    let now = now_ms();
    if stats.since <= 0 {
        stats.since = now;
    }
    if stats.updated_at <= 0 {
        stats.updated_at = stats.since;
    }
    sort_usage_accounts(&mut stats.accounts);
    recompute_time_windows(stats, now);
}

fn load_stats_from_disk() -> Result<CodexLocalAccessStats, String> {
    let path = local_access_stats_file_path()?;
    if !path.exists() {
        return Ok(empty_stats_snapshot());
    }

    let content =
        std::fs::read_to_string(&path).map_err(|e| format!("读取 API 服务统计失败: {}", e))?;
    let mut parsed = serde_json::from_str::<CodexLocalAccessStats>(&content)
        .map_err(|e| format!("解析 API 服务统计失败: {}", e))?;
    normalize_stats(&mut parsed);
    Ok(parsed)
}

fn save_stats_to_disk(stats: &CodexLocalAccessStats) -> Result<(), String> {
    let path = local_access_stats_file_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("创建 API 服务统计目录失败: {}", e))?;
    }
    let content = serde_json::to_string_pretty(stats)
        .map_err(|e| format!("序列化 API 服务统计失败: {}", e))?;
    write_string_atomic(&path, &content)
}

fn prune_runtime_routing_state(runtime: &mut GatewayRuntime, now: i64) {
    runtime
        .response_affinity
        .retain(|_, binding| now.saturating_sub(binding.updated_at_ms) <= RESPONSE_AFFINITY_TTL_MS);
    runtime
        .request_affinity
        .retain(|_, binding| now.saturating_sub(binding.updated_at_ms) <= REQUEST_AFFINITY_TTL_MS);
    runtime
        .model_cooldowns
        .retain(|_, cooldown| cooldown.next_retry_at_ms > now);

    prune_affinity_bindings(
        &mut runtime.response_affinity,
        MAX_RESPONSE_AFFINITY_BINDINGS,
    );
    prune_affinity_bindings(&mut runtime.request_affinity, MAX_REQUEST_AFFINITY_BINDINGS);
}

fn prune_affinity_bindings(
    bindings: &mut HashMap<String, ResponseAffinityBinding>,
    max_bindings: usize,
) {
    if bindings.len() <= max_bindings {
        return;
    }

    let mut ordered: Vec<(String, i64)> = bindings
        .iter()
        .map(|(key, binding)| (key.clone(), binding.updated_at_ms))
        .collect();
    ordered.sort_by_key(|(_, updated_at_ms)| *updated_at_ms);

    let remove_count = bindings.len().saturating_sub(max_bindings);
    for (key, _) in ordered.into_iter().take(remove_count) {
        bindings.remove(&key);
    }
}

async fn resolve_affinity_account(previous_response_id: &str) -> Option<String> {
    let mut runtime = gateway_runtime().lock().await;
    let now = now_ms();
    prune_runtime_routing_state(&mut runtime, now);
    runtime
        .response_affinity
        .get(previous_response_id)
        .map(|binding| binding.account_id.clone())
}

fn request_affinity_key(request: &ParsedRequest) -> Option<String> {
    official_codex_turn_state_affinity_key(request)
}

async fn resolve_request_affinity_account_from_runtime(request: &ParsedRequest) -> Option<String> {
    let request_id = request_affinity_key(request)?;
    let mut runtime = gateway_runtime().lock().await;
    let now = now_ms();
    prune_runtime_routing_state(&mut runtime, now);
    runtime
        .request_affinity
        .get(&request_id)
        .map(|binding| binding.account_id.clone())
}

async fn resolve_request_affinity_account(request: &ParsedRequest) -> Option<String> {
    if let Some(account_id) = resolve_request_affinity_account_from_runtime(request).await {
        return Some(account_id);
    }

    let mut registry = load_health_registry_from_disk().ok()?;
    let now = now_ms();
    if prune_persisted_request_affinity_bindings(&mut registry, None, now) {
        if let Err(err) = save_health_registry_to_disk(&registry) {
            log_health_registry_update_error(&err);
        }
    }
    request_affinity_account_from_registry(&registry, request, now)
}

async fn bind_response_affinity(response_id: &str, account_id: &str) {
    let response_id = response_id.trim();
    let account_id = account_id.trim();
    if response_id.is_empty() || account_id.is_empty() {
        return;
    }

    let mut runtime = gateway_runtime().lock().await;
    let now = now_ms();
    prune_runtime_routing_state(&mut runtime, now);
    runtime.response_affinity.insert(
        response_id.to_string(),
        ResponseAffinityBinding {
            account_id: account_id.to_string(),
            updated_at_ms: now,
        },
    );
    prune_runtime_routing_state(&mut runtime, now);
}

async fn bind_request_affinity(request: &ParsedRequest, account_id: &str) {
    let Some(request_id) = request_affinity_key(request) else {
        return;
    };
    bind_request_affinity_key(request_id, account_id).await;
}

async fn bind_request_affinity_key(request_id: String, account_id: &str) {
    let account_id = account_id.trim();
    if account_id.is_empty() {
        return;
    }

    let mut runtime = gateway_runtime().lock().await;
    let now = now_ms();
    prune_runtime_routing_state(&mut runtime, now);
    runtime.request_affinity.insert(
        request_id,
        ResponseAffinityBinding {
            account_id: account_id.to_string(),
            updated_at_ms: now,
        },
    );
    prune_runtime_routing_state(&mut runtime, now);
}

async fn clear_model_cooldown(account_id: &str, model_key: &str) -> bool {
    let Some(cooldown_key) = build_cooldown_key(account_id, model_key) else {
        return false;
    };

    let mut runtime = gateway_runtime().lock().await;
    let now = now_ms();
    prune_runtime_routing_state(&mut runtime, now);
    runtime.model_cooldowns.remove(&cooldown_key).is_some()
}

async fn clear_account_model_cooldowns(account_id: &str) -> bool {
    let account_id = account_id.trim();
    if account_id.is_empty() {
        return false;
    }

    let mut runtime = gateway_runtime().lock().await;
    let now = now_ms();
    prune_runtime_routing_state(&mut runtime, now);
    let prefix = format!("{}\u{1f}", account_id);
    let before_len = runtime.model_cooldowns.len();
    runtime
        .model_cooldowns
        .retain(|cooldown_key, _| !cooldown_key.starts_with(&prefix));
    runtime.model_cooldowns.len() != before_len
}

async fn clear_runtime_account_affinity(account_id: &str) -> bool {
    let account_id = account_id.trim();
    if account_id.is_empty() {
        return false;
    }

    let mut runtime = gateway_runtime().lock().await;
    let now = now_ms();
    prune_runtime_routing_state(&mut runtime, now);

    let before_response_len = runtime.response_affinity.len();
    runtime
        .response_affinity
        .retain(|_, binding| binding.account_id.trim() != account_id);

    let before_request_len = runtime.request_affinity.len();
    runtime
        .request_affinity
        .retain(|_, binding| binding.account_id.trim() != account_id);

    runtime.response_affinity.len() != before_response_len
        || runtime.request_affinity.len() != before_request_len
}

async fn set_model_cooldown(account_id: &str, model_key: &str, retry_after: Duration) {
    let Some(cooldown_key) = build_cooldown_key(account_id, model_key) else {
        return;
    };
    if retry_after <= Duration::ZERO {
        return;
    }

    let mut runtime = gateway_runtime().lock().await;
    let now = now_ms();
    let next_retry_at_ms = now.saturating_add(retry_after.as_millis() as i64);
    prune_runtime_routing_state(&mut runtime, now);
    runtime
        .model_cooldowns
        .insert(cooldown_key, AccountModelCooldown { next_retry_at_ms });
}

async fn get_model_cooldown_wait(account_id: &str, model_key: &str) -> Option<Duration> {
    let cooldown_key = build_cooldown_key(account_id, model_key)?;
    let mut runtime = gateway_runtime().lock().await;
    let now = now_ms();
    prune_runtime_routing_state(&mut runtime, now);
    let cooldown = runtime.model_cooldowns.get(&cooldown_key)?;
    let wait_ms = cooldown.next_retry_at_ms.saturating_sub(now);
    if wait_ms <= 0 {
        return None;
    }
    Some(Duration::from_millis(wait_ms as u64))
}

fn ensure_local_port_available(port: u16, current_port: Option<u16>) -> Result<(), String> {
    if port == 0 {
        return Err("端口必须在 1 到 65535 之间".to_string());
    }
    if current_port == Some(port) {
        return Ok(());
    }
    let listener = StdTcpListener::bind((CODEX_LOCAL_ACCESS_BIND_HOST, port))
        .map_err(|e| format!("端口 {} 不可用: {}", port, e))?;
    drop(listener);
    Ok(())
}

fn format_gateway_bind_error(port: u16, error: &std::io::Error) -> String {
    if error.kind() == std::io::ErrorKind::AddrInUse {
        return format!(
            "启动本地接入服务失败: 端口 {} 已被占用，请先清理端口或改用其他端口（{}）",
            port, error
        );
    }
    format!("启动本地接入服务失败: {}", error)
}

fn is_free_plan_type(plan_type: Option<&str>) -> bool {
    let Some(plan_type) = plan_type else {
        return false;
    };
    let normalized = plan_type.trim().to_ascii_lowercase();
    !normalized.is_empty() && normalized.contains("free")
}

fn is_local_access_eligible_account(account: &CodexAccount, restrict_free_accounts: bool) -> bool {
    if restrict_free_accounts && is_free_plan_type(account.plan_type.as_deref()) {
        return false;
    }
    true
}

fn clamp_u32_field(value: &mut u32, default_value: u32, min_value: u32, max_value: u32) -> bool {
    let original = *value;
    if *value == 0 {
        *value = default_value;
    }
    *value = (*value).clamp(min_value, max_value);
    *value != original
}

fn clamp_u64_field(value: &mut u64, default_value: u64, min_value: u64, max_value: u64) -> bool {
    let original = *value;
    if *value == 0 {
        *value = default_value;
    }
    *value = (*value).clamp(min_value, max_value);
    *value != original
}

fn normalize_local_api_safety_config(collection: &mut CodexLocalAccessCollection) -> bool {
    if collection.safety_config.schema_version > CODEX_LOCAL_API_SAFETY_SCHEMA_VERSION {
        collection.safety_config = CodexLocalApiSafetyConfig::default();
        return true;
    }

    let mut changed = false;
    let defaults = CodexLocalApiSafetyConfig::default();
    let config = &mut collection.safety_config;

    if config.schema_version != CODEX_LOCAL_API_SAFETY_SCHEMA_VERSION {
        config.schema_version = CODEX_LOCAL_API_SAFETY_SCHEMA_VERSION;
        changed = true;
    }
    if !config.hardened_local_mode {
        config.hardened_local_mode = defaults.hardened_local_mode;
        changed = true;
    }
    changed |= clamp_u32_field(
        &mut config.max_concurrent_requests,
        defaults.max_concurrent_requests,
        1,
        4,
    );
    changed |= clamp_u64_field(
        &mut config.min_request_interval_seconds,
        defaults.min_request_interval_seconds,
        1,
        3600,
    );
    changed |= clamp_u64_field(
        &mut config.max_queue_wait_seconds,
        defaults.max_queue_wait_seconds,
        1,
        300,
    );
    let minimum_queue_wait_seconds =
        local_api_queue_wait_seconds_for_start_interval(config.min_request_interval_seconds);
    if config.max_queue_wait_seconds < minimum_queue_wait_seconds {
        config.max_queue_wait_seconds = minimum_queue_wait_seconds;
        changed = true;
    }
    changed |= clamp_u64_field(
        &mut config.request_timeout_seconds,
        defaults.request_timeout_seconds,
        30,
        3600,
    );
    changed |= clamp_u32_field(
        &mut config.max_request_body_mb,
        defaults.max_request_body_mb,
        1,
        (MAX_HTTP_REQUEST_BYTES / (1024 * 1024)) as u32,
    );
    changed |= clamp_u32_field(&mut config.max_retries, defaults.max_retries, 1, 3);
    changed |= clamp_u32_field(
        &mut config.max_retry_accounts,
        defaults.max_retry_accounts,
        1,
        MAX_RETRY_CREDENTIALS_PER_REQUEST as u32,
    );
    if config.max_retry_accounts < 2 {
        config.max_retry_accounts = 2;
        changed = true;
    }
    if matches!(config.fallback_mode, CodexLocalApiFallbackMode::Unknown) {
        config.fallback_mode = defaults.fallback_mode;
        changed = true;
    }

    if !config.logging.redact_sensitive_values {
        config.logging.redact_sensitive_values = true;
        changed = true;
    }
    if config.logging.include_prompt_response {
        config.logging.include_prompt_response = false;
        changed = true;
    }
    if config.logging.include_raw_upstream_body {
        config.logging.include_raw_upstream_body = false;
        changed = true;
    }

    changed
}

fn local_api_queue_wait_seconds_for_start_interval(min_request_interval_seconds: u64) -> u64 {
    min_request_interval_seconds.saturating_add(1).clamp(1, 300)
}

fn local_api_safety_config_for_preset(
    preset: CodexLocalApiSafetyPresetId,
) -> CodexLocalApiSafetyConfig {
    let mut config = CodexLocalApiSafetyConfig::default();

    match preset {
        CodexLocalApiSafetyPresetId::MaximumSafety => {
            config.min_request_interval_seconds = 60;
            config.max_retry_accounts = 2;
            config.fallback_mode = CodexLocalApiFallbackMode::Disabled;
        }
        CodexLocalApiSafetyPresetId::BalancedSelfUse => {
            config.min_request_interval_seconds = 20;
            config.max_retry_accounts = 2;
            config.fallback_mode = CodexLocalApiFallbackMode::Disabled;
        }
        CodexLocalApiSafetyPresetId::QuotaDrainCareful => {
            config.min_request_interval_seconds = 30;
            config.max_retry_accounts = 2;
            config.fallback_mode = CodexLocalApiFallbackMode::NextRequestOnly;
        }
    }

    config.max_queue_wait_seconds =
        local_api_queue_wait_seconds_for_start_interval(config.min_request_interval_seconds);
    config
}

fn local_api_routing_strategy_for_preset(
    _preset: CodexLocalApiSafetyPresetId,
) -> CodexLocalAccessRoutingStrategy {
    CodexLocalAccessRoutingStrategy::Auto
}

fn apply_local_api_safety_preset_to_collection(
    collection: &mut CodexLocalAccessCollection,
    preset: CodexLocalApiSafetyPresetId,
) -> bool {
    let original_safety_config = collection.safety_config.clone();
    let original_routing_strategy = collection.routing_strategy;

    collection.safety_config = local_api_safety_config_for_preset(preset);
    collection.routing_strategy = local_api_routing_strategy_for_preset(preset);
    let changed = original_safety_config != collection.safety_config
        || original_routing_strategy != collection.routing_strategy;

    let normalized = normalize_local_api_safety_config(collection);
    changed || normalized
}

fn filter_local_access_account_ids<I>(
    account_ids: I,
    valid_account_ids: &HashSet<String>,
) -> Vec<String>
where
    I: IntoIterator<Item = String>,
{
    let mut filtered = Vec::new();
    let mut seen = HashSet::new();
    for account_id in account_ids {
        if !valid_account_ids.contains(&account_id) {
            continue;
        }
        if seen.insert(account_id.clone()) {
            filtered.push(account_id);
        }
    }
    filtered.sort();
    filtered
}

fn sanitize_collection(
    collection: &mut CodexLocalAccessCollection,
) -> Result<(bool, HashSet<String>), String> {
    let mut changed = false;
    changed |= normalize_local_api_safety_config(collection);

    if collection.port == 0 || collection.port == LEGACY_DEFAULT_CODEX_LOCAL_ACCESS_PORT {
        collection.port = allocate_stable_local_access_port(None)?;
        changed = true;
    }
    if collection.api_key.trim().is_empty() {
        collection.api_key = generate_local_api_key();
        changed = true;
    }
    if collection.created_at <= 0 {
        collection.created_at = now_ms();
        changed = true;
    }
    if collection.updated_at <= 0 {
        collection.updated_at = now_ms();
        changed = true;
    }
    if collection.follow_current_account {
        collection.follow_current_account = false;
        changed = true;
    }

    let valid_account_ids: HashSet<String> = codex_account::list_accounts_checked()?
        .into_iter()
        .filter(|account| {
            is_local_access_eligible_account(account, collection.restrict_free_accounts)
        })
        .map(|account| account.id)
        .collect();

    let deduped =
        filter_local_access_account_ids(collection.account_ids.iter().cloned(), &valid_account_ids);
    if deduped != collection.account_ids {
        collection.account_ids = deduped;
        changed = true;
    }

    Ok((changed, valid_account_ids))
}

async fn ensure_runtime_loaded_without_start() -> Result<(), String> {
    {
        let runtime = gateway_runtime().lock().await;
        if runtime.loaded {
            return Ok(());
        }
    }

    let loaded_collection = load_collection_from_disk()?;
    let mut loaded_stats = load_stats_from_disk()?;
    let mut next_collection = loaded_collection;
    let mut persist_after_load = false;

    if next_collection.is_none() {
        next_collection = Some(CodexLocalAccessCollection {
            enabled: false,
            port: allocate_stable_local_access_port(None)?,
            api_key: generate_local_api_key(),
            safety_config: CodexLocalApiSafetyConfig::default(),
            routing_strategy: CodexLocalAccessRoutingStrategy::default(),
            restrict_free_accounts: false,
            follow_current_account: false,
            account_ids: Vec::new(),
            created_at: now_ms(),
            updated_at: now_ms(),
        });
        persist_after_load = true;
    }

    if let Some(collection) = next_collection.as_mut() {
        let (changed, _) = sanitize_collection(collection)?;
        persist_after_load = persist_after_load || changed;
    }

    if persist_after_load {
        if let Some(collection) = next_collection.as_ref() {
            save_collection_to_disk(collection)?;
        }
    }

    {
        let mut runtime = gateway_runtime().lock().await;
        normalize_stats(&mut loaded_stats);
        runtime.stats_dirty = false;
        runtime.stats_flush_inflight = false;
        runtime.stats = loaded_stats;
        if let Some(collection) = next_collection.clone() {
            sync_runtime_collection(&mut runtime, collection);
        } else {
            runtime.loaded = true;
            runtime.collection = None;
            runtime.last_error = None;
            prune_prepared_account_cache(&mut runtime, now_ms());
        }
    }

    Ok(())
}

async fn ensure_runtime_loaded() -> Result<(), String> {
    ensure_runtime_loaded_without_start().await?;

    let should_start = {
        let runtime = gateway_runtime().lock().await;
        runtime
            .collection
            .as_ref()
            .map(|collection| collection.enabled)
            .unwrap_or(false)
    };

    if should_start {
        ensure_gateway_matches_runtime().await?;
    }

    Ok(())
}

async fn ensure_gateway_matches_runtime() -> Result<(), String> {
    let (collection, running, actual_port, stale_task) = {
        let mut runtime = gateway_runtime().lock().await;
        let stale_task = if !runtime.running {
            runtime.task.take()
        } else {
            None
        };
        (
            runtime.collection.clone(),
            runtime.running,
            runtime.actual_port,
            stale_task,
        )
    };

    if let Some(task) = stale_task {
        let _ = task.await;
    }

    let Some(mut collection) = collection else {
        stop_gateway().await;
        return Ok(());
    };

    if !collection.enabled {
        stop_gateway().await;
        return Ok(());
    }

    if running && actual_port == Some(collection.port) {
        return Ok(());
    }

    stop_gateway().await;

    let listener = match TcpListener::bind((CODEX_LOCAL_ACCESS_BIND_HOST, collection.port)).await {
        Ok(listener) => listener,
        Err(error) => {
            if matches!(
                error.kind(),
                std::io::ErrorKind::AddrInUse | std::io::ErrorKind::PermissionDenied
            ) {
                let original_port = collection.port;
                collection.port = allocate_stable_local_access_port(Some(original_port))?;
                collection.updated_at = now_ms();
                save_collection_to_disk(&collection)?;
                {
                    let mut runtime = gateway_runtime().lock().await;
                    sync_runtime_collection(&mut runtime, collection.clone());
                }
                logger::log_codex_api_warn(&format!(
                    "[CodexLocalAccess] 端口 {} 不可绑定（{}），已自动切换到端口 {}",
                    original_port, error, collection.port
                ));
                match TcpListener::bind((CODEX_LOCAL_ACCESS_BIND_HOST, collection.port)).await {
                    Ok(listener) => listener,
                    Err(retry_error) => {
                        let message = format_gateway_bind_error(collection.port, &retry_error);
                        let mut runtime = gateway_runtime().lock().await;
                        runtime.running = false;
                        runtime.actual_port = None;
                        runtime.last_error = Some(message.clone());
                        return Err(message);
                    }
                }
            } else {
                let message = format_gateway_bind_error(collection.port, &error);
                let mut runtime = gateway_runtime().lock().await;
                runtime.running = false;
                runtime.actual_port = None;
                runtime.last_error = Some(message.clone());
                return Err(message);
            }
        }
    };
    let (shutdown_sender, mut shutdown_receiver) = watch::channel(false);
    let port = collection.port;

    let task = tokio::spawn(async move {
        logger::log_codex_api_info(&format!(
            "[CodexLocalAccess] 本地接入服务已启动: {}",
            build_base_url(port)
        ));

        loop {
            tokio::select! {
                changed = shutdown_receiver.changed() => {
                    if changed.is_ok() {
                        break;
                    }
                }
                accepted = listener.accept() => {
                    match accepted {
                        Ok((stream, addr)) => {
                            tokio::spawn(async move {
                                if let Err(err) = handle_connection(stream, addr).await {
                                    logger::log_codex_api_warn(&format!(
                                        "[CodexLocalAccess] 请求处理失败 {}: {}",
                                        addr, err
                                    ));
                                }
                            });
                        }
                        Err(err) => {
                            logger::log_codex_api_warn(&format!(
                                "[CodexLocalAccess] 接收请求失败: {}",
                                err
                            ));
                            break;
                        }
                    }
                }
            }
        }

        let mut runtime = gateway_runtime().lock().await;
        if runtime.actual_port == Some(port) {
            runtime.running = false;
            runtime.actual_port = None;
            runtime.shutdown_sender = None;
        }
    });

    let mut runtime = gateway_runtime().lock().await;
    runtime.running = true;
    runtime.actual_port = Some(collection.port);
    runtime.last_error = None;
    runtime.shutdown_sender = Some(shutdown_sender);
    runtime.task = Some(task);
    Ok(())
}

async fn stop_gateway() {
    let (shutdown_sender, task) = {
        let mut runtime = gateway_runtime().lock().await;
        runtime.running = false;
        runtime.actual_port = None;
        (runtime.shutdown_sender.take(), runtime.task.take())
    };

    if let Some(sender) = shutdown_sender {
        let _ = sender.send(true);
    }
    if let Some(mut task) = task {
        tokio::select! {
            result = &mut task => {
                let _ = result;
            }
            _ = tokio::time::sleep(GATEWAY_SHUTDOWN_TIMEOUT) => {
                logger::log_codex_api_warn("[CodexLocalAccess] 停止本地接入服务超时，已强制中止监听任务");
                task.abort();
                let _ = task.await;
            }
        }
    }
}

fn apply_usage_stats(
    target: &mut CodexLocalAccessUsageStats,
    success: bool,
    latency_ms: u64,
    usage: Option<&UsageCapture>,
) {
    target.request_count = target.request_count.saturating_add(1);
    if success {
        target.success_count = target.success_count.saturating_add(1);
    } else {
        target.failure_count = target.failure_count.saturating_add(1);
    }
    target.total_latency_ms = target.total_latency_ms.saturating_add(latency_ms);

    if let Some(usage) = usage {
        target.input_tokens = target.input_tokens.saturating_add(usage.input_tokens);
        target.output_tokens = target.output_tokens.saturating_add(usage.output_tokens);
        target.total_tokens = target.total_tokens.saturating_add(usage.total_tokens);
        target.cached_tokens = target.cached_tokens.saturating_add(usage.cached_tokens);
        target.reasoning_tokens = target
            .reasoning_tokens
            .saturating_add(usage.reasoning_tokens);
    }
}

fn upsert_account_usage_stats(
    accounts: &mut Vec<CodexLocalAccessAccountStats>,
    account_id: Option<&str>,
    account_email: Option<&str>,
    success: bool,
    latency_ms: u64,
    usage: Option<&UsageCapture>,
    updated_at: i64,
) {
    let Some(account_id) = account_id.map(str::trim).filter(|value| !value.is_empty()) else {
        return;
    };
    let normalized_email = account_email
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or_default()
        .to_string();

    if let Some(account_stats) = accounts
        .iter_mut()
        .find(|item| item.account_id == account_id)
    {
        if !normalized_email.is_empty() {
            account_stats.email = normalized_email;
        }
        account_stats.updated_at = updated_at;
        apply_usage_stats(&mut account_stats.usage, success, latency_ms, usage);
        return;
    }

    let mut account_stats = CodexLocalAccessAccountStats {
        account_id: account_id.to_string(),
        email: normalized_email,
        usage: CodexLocalAccessUsageStats::default(),
        updated_at,
    };
    apply_usage_stats(&mut account_stats.usage, success, latency_ms, usage);
    accounts.push(account_stats);
}

async fn record_request_stats(
    account_id: Option<&str>,
    account_email: Option<&str>,
    success: bool,
    latency_ms: u64,
    usage: Option<UsageCapture>,
) -> Result<(), String> {
    {
        let mut runtime = gateway_runtime().lock().await;
        let now = now_ms();
        let usage_ref = usage.as_ref();
        if runtime.stats.since <= 0 {
            runtime.stats.since = now;
        }
        runtime.stats.updated_at = now;
        apply_usage_stats(&mut runtime.stats.totals, success, latency_ms, usage_ref);
        upsert_account_usage_stats(
            &mut runtime.stats.accounts,
            account_id,
            account_email,
            success,
            latency_ms,
            usage_ref,
            now,
        );
        append_usage_event(
            &mut runtime.stats.events,
            now,
            account_id,
            account_email,
            success,
            latency_ms,
            usage_ref,
        );

        normalize_stats(&mut runtime.stats);
        runtime.stats_dirty = true;
    }

    schedule_stats_flush_if_needed().await;
    Ok(())
}

fn build_state_snapshot(runtime: &GatewayRuntime) -> CodexLocalAccessState {
    let collection = runtime.collection.clone();
    let effective_account_ids = collection
        .as_ref()
        .map(build_effective_local_access_account_ids_for_state)
        .unwrap_or_default();
    let member_count = collection
        .as_ref()
        .map(|item| item.account_ids.len())
        .unwrap_or(0);
    let api_port_url = collection
        .as_ref()
        .map(|item| build_api_port_url(item.port));
    let base_url = collection.as_ref().map(|item| build_base_url(item.port));
    let lan_base_url = collection
        .as_ref()
        .and_then(|item| build_lan_base_url(item.port));
    let model_ids = supported_codex_model_ids();
    let mut stats = runtime.stats.clone();
    stats.events.clear();
    let health = build_health_summary_from_disk_for_accounts(&effective_account_ids);

    CodexLocalAccessState {
        collection,
        running: runtime.running,
        api_port_url,
        base_url,
        lan_base_url,
        model_ids,
        last_error: runtime.last_error.clone(),
        member_count,
        effective_account_ids,
        stats,
        health,
        concurrency_diagnostics: build_concurrency_diagnostics(runtime),
    }
}

fn build_concurrency_diagnostics(
    runtime: &GatewayRuntime,
) -> CodexLocalAccessConcurrencyDiagnostics {
    let now = now_ms();
    let audit_window_ms = CONCURRENCY_DIAGNOSTICS_AUDIT_WINDOW_MS;
    let safety_config = runtime
        .collection
        .as_ref()
        .map(|collection| collection.safety_config.clone())
        .unwrap_or_default();
    let backpressure = current_local_backpressure_snapshot();
    let max_concurrent_requests = safety_config.max_concurrent_requests.max(1);
    let active_request_count = backpressure.active_requests;
    let start_interval_remaining_ms = duration_as_millis_u64(
        local_backpressure_start_interval_remaining(&backpressure, &safety_config, Instant::now()),
    );
    let rollup = load_concurrency_audit_rollup(now, audit_window_ms);

    CodexLocalAccessConcurrencyDiagnostics {
        updated_at: now,
        max_concurrent_requests,
        active_request_count,
        active_stream_count: active_stream_lease_count(),
        request_capacity: max_concurrent_requests.saturating_sub(active_request_count),
        min_request_interval_seconds: safety_config.min_request_interval_seconds,
        max_queue_wait_seconds: safety_config.max_queue_wait_seconds,
        start_interval_remaining_ms,
        audit_window_ms,
        recent_audit_event_count: rollup.recent_audit_event_count,
        recent_request_count: rollup.recent_request_count,
        recent_local_backpressure_count: rollup.recent_local_backpressure_count,
        recent_pool_wait_count: rollup.recent_pool_wait_count,
        recent_upstream_limit_count: rollup.recent_upstream_limit_count,
        recent_stream_error_count: rollup.recent_stream_error_count,
        last_problem_at_ms: rollup.last_problem_at_ms,
        last_problem_kind: rollup.last_problem_kind,
        audit_load_error: rollup.audit_load_error,
    }
}

async fn snapshot_state() -> Result<CodexLocalAccessState, String> {
    ensure_runtime_loaded_without_start().await?;
    if let Err(err) = ensure_gateway_matches_runtime().await {
        let mut runtime = gateway_runtime().lock().await;
        runtime.last_error = Some(err);
        return Ok(build_state_snapshot(&runtime));
    }
    let runtime = gateway_runtime().lock().await;
    Ok(build_state_snapshot(&runtime))
}

pub async fn get_local_access_state() -> Result<CodexLocalAccessState, String> {
    snapshot_state().await
}

pub async fn activate_local_access_for_dir(
    profile_dir: &Path,
) -> Result<CodexLocalAccessState, String> {
    let state = set_local_access_enabled(true).await?;
    let collection = state
        .collection
        .clone()
        .ok_or_else(|| "API 服务集合尚未创建".to_string())?;
    let base_url = state
        .base_url
        .clone()
        .unwrap_or_else(|| build_base_url(collection.port));
    let projection_account = resolve_local_access_projection_account(&collection)?;
    let runtime_account =
        build_runtime_account(base_url, collection.api_key.clone(), &projection_account);
    codex_account::write_account_bundle_to_dir(profile_dir, &runtime_account)?;
    Ok(state)
}

pub async fn save_local_access_accounts(
    account_ids: Vec<String>,
    restrict_free_accounts: bool,
) -> Result<CodexLocalAccessState, String> {
    ensure_runtime_loaded().await?;

    let mut collection = {
        let runtime = gateway_runtime().lock().await;
        runtime
            .collection
            .clone()
            .unwrap_or(CodexLocalAccessCollection {
                enabled: false,
                port: allocate_stable_local_access_port(None)?,
                api_key: generate_local_api_key(),
                safety_config: CodexLocalApiSafetyConfig::default(),
                routing_strategy: CodexLocalAccessRoutingStrategy::default(),
                restrict_free_accounts: false,
                follow_current_account: false,
                account_ids: Vec::new(),
                created_at: now_ms(),
                updated_at: now_ms(),
            })
    };

    let valid_account_ids: HashSet<String> = codex_account::list_accounts_checked()?
        .into_iter()
        .filter(|account| is_local_access_eligible_account(account, restrict_free_accounts))
        .map(|account| account.id)
        .collect();

    let next_account_ids = filter_local_access_account_ids(account_ids, &valid_account_ids);

    collection.restrict_free_accounts = restrict_free_accounts;
    collection.account_ids = next_account_ids;
    collection.updated_at = now_ms();
    let (changed, _) = sanitize_collection(&mut collection)?;
    if changed {
        collection.updated_at = now_ms();
    }
    save_collection_to_disk(&collection)?;

    {
        let mut runtime = gateway_runtime().lock().await;
        sync_runtime_collection(&mut runtime, collection);
    }

    ensure_gateway_matches_runtime().await?;
    snapshot_state().await
}

pub async fn sync_local_access_to_current_account_on_switch(
    _account_id: &str,
) -> Result<(), String> {
    ensure_runtime_loaded_without_start().await?;

    let maybe_collection = {
        let runtime = gateway_runtime().lock().await;
        runtime.collection.clone()
    };

    let Some(collection) = maybe_collection else {
        return Ok(());
    };

    let mode = load_runtime_mode_state()
        .map(|state| state.mode)
        .unwrap_or(CodexRuntimeIntegrationMode::DirectProjection);
    if !should_sync_local_access_collection_on_account_switch(mode, &collection) {
        return Ok(());
    }

    Ok(())
}

pub async fn update_local_access_routing_strategy(
    strategy: CodexLocalAccessRoutingStrategy,
) -> Result<CodexLocalAccessState, String> {
    ensure_runtime_loaded().await?;

    let maybe_collection = {
        let runtime = gateway_runtime().lock().await;
        runtime.collection.clone()
    };

    let Some(mut collection) = maybe_collection else {
        return Err("本地接入集合尚未创建".to_string());
    };

    if collection.routing_strategy == strategy {
        return snapshot_state().await;
    }

    collection.routing_strategy = strategy;
    collection.updated_at = now_ms();
    save_collection_to_disk(&collection)?;

    {
        let mut runtime = gateway_runtime().lock().await;
        sync_runtime_collection(&mut runtime, collection);
    }

    snapshot_state().await
}

pub async fn apply_local_access_safety_preset(
    preset: CodexLocalApiSafetyPresetId,
) -> Result<CodexLocalAccessState, String> {
    ensure_runtime_loaded().await?;

    let maybe_collection = {
        let runtime = gateway_runtime().lock().await;
        runtime.collection.clone()
    };

    let Some(mut collection) = maybe_collection else {
        return Err("本地接入集合尚未创建".to_string());
    };

    if !apply_local_api_safety_preset_to_collection(&mut collection, preset) {
        return snapshot_state().await;
    }

    collection.updated_at = now_ms();
    save_collection_to_disk(&collection)?;

    {
        let mut runtime = gateway_runtime().lock().await;
        sync_runtime_collection(&mut runtime, collection);
    }

    ensure_gateway_matches_runtime().await?;
    snapshot_state().await
}

pub async fn remove_local_access_account(
    account_id: &str,
) -> Result<CodexLocalAccessState, String> {
    ensure_runtime_loaded().await?;

    let maybe_collection = {
        let runtime = gateway_runtime().lock().await;
        runtime.collection.clone()
    };

    let Some(mut collection) = maybe_collection else {
        return snapshot_state().await;
    };

    let before_len = collection.account_ids.len();
    collection.account_ids.retain(|id| id != account_id);
    if collection.account_ids.len() == before_len {
        return snapshot_state().await;
    }

    collection.updated_at = now_ms();
    save_collection_to_disk(&collection)?;

    {
        let mut runtime = gateway_runtime().lock().await;
        sync_runtime_collection(&mut runtime, collection);
    }

    ensure_gateway_matches_runtime().await?;
    snapshot_state().await
}

pub async fn rotate_local_access_api_key() -> Result<CodexLocalAccessState, String> {
    ensure_runtime_loaded().await?;

    let maybe_collection = {
        let runtime = gateway_runtime().lock().await;
        runtime.collection.clone()
    };

    let Some(mut collection) = maybe_collection else {
        return Err("本地接入集合尚未创建".to_string());
    };

    collection.api_key = generate_local_api_key();
    collection.updated_at = now_ms();
    save_collection_to_disk(&collection)?;

    {
        let mut runtime = gateway_runtime().lock().await;
        sync_runtime_collection(&mut runtime, collection);
    }

    snapshot_state().await
}

pub async fn clear_local_access_stats() -> Result<CodexLocalAccessState, String> {
    ensure_runtime_loaded().await?;

    let cleared = empty_stats_snapshot();
    {
        let mut runtime = gateway_runtime().lock().await;
        runtime.stats = cleared;
        runtime.stats_dirty = true;
    }
    schedule_stats_flush_if_needed().await;

    snapshot_state().await
}

pub async fn recover_local_access_health(
    account_id: &str,
    model: Option<&str>,
) -> Result<CodexLocalAccessState, String> {
    ensure_runtime_loaded_without_start().await?;

    let account_id = account_id.trim();
    if account_id.is_empty() {
        return Err("账号 ID 不能为空".to_string());
    }
    let model = model.map(str::trim).filter(|value| !value.is_empty());

    let collection = {
        let runtime = gateway_runtime().lock().await;
        runtime.collection.clone()
    }
    .ok_or_else(|| "API 服务集合尚未创建".to_string())?;

    if !collection
        .account_ids
        .iter()
        .any(|collection_account_id| collection_account_id == account_id)
    {
        return Err("账号不在 API 服务集合中".to_string());
    }

    let now = now_ms();
    let mut registry = load_health_registry_from_disk()?;
    let registry_changed = recover_health_registry_account(&mut registry, account_id, model, now);
    if registry_changed {
        save_health_registry_to_disk(&registry)?;
    }

    let runtime_changed = match model {
        Some(model) => clear_model_cooldown(account_id, model).await,
        None => clear_account_model_cooldowns(account_id).await,
    };
    record_manual_recovery_audit_event(account_id, model, registry_changed || runtime_changed);

    snapshot_state().await
}

pub async fn pause_local_access_health(account_id: &str) -> Result<CodexLocalAccessState, String> {
    ensure_runtime_loaded_without_start().await?;

    let account_id = account_id.trim();
    if account_id.is_empty() {
        return Err("账号 ID 不能为空".to_string());
    }

    let collection = {
        let runtime = gateway_runtime().lock().await;
        runtime.collection.clone()
    }
    .ok_or_else(|| "API 服务集合尚未创建".to_string())?;

    if !collection
        .account_ids
        .iter()
        .any(|collection_account_id| collection_account_id == account_id)
    {
        return Err("账号不在 API 服务集合中".to_string());
    }

    let now = now_ms();
    let mut registry = load_health_registry_from_disk()?;
    let registry_changed = pause_health_registry_account(&mut registry, account_id, now);
    if registry_changed {
        save_health_registry_to_disk(&registry)?;
    }

    let runtime_changed = clear_runtime_account_affinity(account_id).await;
    record_manual_pause_audit_event(account_id, registry_changed || runtime_changed);

    snapshot_state().await
}

pub async fn prepare_local_access_gateway_for_restart() -> Result<CodexLocalAccessState, String> {
    ensure_runtime_loaded_without_start().await?;
    stop_gateway().await;

    let runtime = gateway_runtime().lock().await;
    Ok(build_state_snapshot(&runtime))
}

pub async fn kill_local_access_port_processes() -> Result<CodexLocalAccessPortCleanupResult, String>
{
    if let Err(err) = ensure_runtime_loaded_without_start().await {
        logger::log_codex_api_warn(&format!(
            "[CodexLocalAccess] 清理端口前加载配置失败: {}",
            err
        ));
        return Err(err);
    }

    let collection = {
        let runtime = gateway_runtime().lock().await;
        runtime.collection.clone()
    }
    .ok_or_else(|| "API 服务集合尚未创建".to_string())?;

    stop_gateway().await;

    let killed_count = process::kill_port_processes(collection.port)? as u32;

    if collection.enabled {
        ensure_gateway_matches_runtime().await?;
    }

    let state = snapshot_state().await?;
    Ok(CodexLocalAccessPortCleanupResult {
        killed_count,
        state,
    })
}

pub async fn update_local_access_port(port: u16) -> Result<CodexLocalAccessState, String> {
    ensure_runtime_loaded().await?;

    let maybe_collection = {
        let runtime = gateway_runtime().lock().await;
        runtime.collection.clone()
    };

    let Some(mut collection) = maybe_collection else {
        return Err("本地接入集合尚未创建".to_string());
    };

    ensure_local_port_available(port, Some(collection.port))?;
    if collection.port == port {
        return snapshot_state().await;
    }

    collection.port = port;
    collection.updated_at = now_ms();
    save_collection_to_disk(&collection)?;

    {
        let mut runtime = gateway_runtime().lock().await;
        sync_runtime_collection(&mut runtime, collection);
    }

    ensure_gateway_matches_runtime().await?;
    snapshot_state().await
}

pub async fn set_local_access_enabled(enabled: bool) -> Result<CodexLocalAccessState, String> {
    set_local_access_enabled_with_options(
        enabled,
        RuntimeProjectionChangeOptions::new("local_access_set_enabled", false),
    )
    .await
}

pub async fn set_local_access_enabled_with_options(
    enabled: bool,
    options: RuntimeProjectionChangeOptions,
) -> Result<CodexLocalAccessState, String> {
    ensure_runtime_loaded().await?;

    let maybe_collection = {
        let runtime = gateway_runtime().lock().await;
        runtime.collection.clone()
    };

    let Some(mut collection) = maybe_collection else {
        return Err("本地接入集合尚未创建".to_string());
    };

    let current_mode = load_runtime_mode_state()
        .map(|state| state.mode)
        .unwrap_or(CodexRuntimeIntegrationMode::DirectProjection);
    let risk = if !enabled && current_mode == CodexRuntimeIntegrationMode::CockpitApiService {
        Some(collect_runtime_projection_continuity_risk())
    } else {
        None
    };
    if let Some(risk) = risk.as_ref() {
        if should_block_direct_projection_change(
            current_mode,
            CodexRuntimeIntegrationMode::DirectProjection,
            options.force,
            risk,
        ) {
            record_runtime_projection_audit_event(
                "local_access_enabled_transition",
                "blocked",
                options.source,
                options.force,
                Some(current_mode),
                Some(CodexRuntimeIntegrationMode::DirectProjection),
                Some(risk),
            );
            return Err(format!(
                "API 服务仍处于 Codex 连续性保护窗口（{}）。如确需停用，请在确认没有运行中任务后使用强制停用。",
                risk.blocking_reasons().join(", ")
            ));
        }
    }

    let was_enabled = collection.enabled;
    collection.enabled = enabled;
    collection.updated_at = now_ms();
    save_collection_to_disk(&collection)?;

    {
        let mut runtime = gateway_runtime().lock().await;
        sync_runtime_collection(&mut runtime, collection);
    }

    ensure_gateway_matches_runtime().await?;
    if was_enabled != enabled {
        record_runtime_projection_audit_event(
            "local_access_enabled_transition",
            if enabled { "enabled" } else { "disabled" },
            options.source,
            options.force,
            Some(current_mode),
            Some(if enabled {
                CodexRuntimeIntegrationMode::CockpitApiService
            } else {
                CodexRuntimeIntegrationMode::DirectProjection
            }),
            risk.as_ref(),
        );
    }
    if enabled {
        repair_runtime_projection_history_visibility_if_needed("API 服务启用");
    }
    snapshot_state().await
}

pub async fn restore_local_access_gateway() {
    match ensure_runtime_loaded().await {
        Ok(()) => {
            repair_runtime_projection_history_visibility_if_needed("Cockpit 启动恢复");
        }
        Err(err) => {
            let mut runtime = gateway_runtime().lock().await;
            runtime.loaded = true;
            runtime.last_error = Some(err.clone());
            logger::log_codex_api_warn(&format!("[CodexLocalAccess] 初始化失败: {}", err));
        }
    }
}

fn find_header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|index| index + 4)
}

fn parse_content_length(header_bytes: &[u8]) -> Result<usize, String> {
    let header_text = String::from_utf8_lossy(header_bytes);
    for line in header_text.lines() {
        let mut parts = line.splitn(2, ':');
        let Some(name) = parts.next() else { continue };
        let Some(value) = parts.next() else { continue };
        if name.trim().eq_ignore_ascii_case("content-length") {
            return value
                .trim()
                .parse::<usize>()
                .map_err(|e| format!("非法 Content-Length: {}", e));
        }
    }
    Ok(0)
}

async fn read_http_request<R>(stream: &mut R) -> Result<Vec<u8>, String>
where
    R: AsyncRead + Unpin,
{
    let mut buffer = Vec::with_capacity(4096);
    let mut chunk = [0u8; 2048];
    let mut header_end: Option<usize> = None;
    let mut content_length = 0usize;

    loop {
        let bytes_read = timeout(REQUEST_READ_TIMEOUT, stream.read(&mut chunk))
            .await
            .map_err(|_| "读取请求超时".to_string())?
            .map_err(|e| format!("读取请求失败: {}", e))?;

        if bytes_read == 0 {
            break;
        }

        buffer.extend_from_slice(&chunk[..bytes_read]);
        if buffer.len() > MAX_HTTP_REQUEST_BYTES {
            return Err("请求体过大".to_string());
        }

        if header_end.is_none() {
            if let Some(end) = find_header_end(&buffer) {
                content_length = parse_content_length(&buffer[..end])?;
                header_end = Some(end);
            }
        }

        if let Some(end) = header_end {
            if buffer.len() >= end.saturating_add(content_length) {
                return Ok(buffer[..(end + content_length)].to_vec());
            }
        }
    }

    Err("请求不完整".to_string())
}

fn parse_http_request(raw: &[u8]) -> Result<ParsedRequest, String> {
    let Some(header_end) = find_header_end(raw) else {
        return Err("缺少 HTTP 头结束标记".to_string());
    };

    let header_text = String::from_utf8_lossy(&raw[..header_end]);
    let mut lines = header_text.lines();
    let request_line = lines.next().ok_or("请求行为空")?.trim();

    let mut parts = request_line.split_whitespace();
    let method = parts.next().ok_or("请求行缺少 method")?.to_string();
    let target = parts.next().ok_or("请求行缺少 target")?.to_string();

    let mut headers = HashMap::new();
    for line in lines {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let mut parts = line.splitn(2, ':');
        let Some(name) = parts.next() else { continue };
        let Some(value) = parts.next() else { continue };
        headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_string());
    }

    Ok(ParsedRequest {
        method,
        target,
        headers,
        body: raw[header_end..].to_vec(),
        gateway_request_id: next_gateway_request_id(),
    })
}

fn normalize_proxy_target(target: &str) -> Result<String, String> {
    if target.starts_with("http://") || target.starts_with("https://") {
        let parsed = url::Url::parse(target).map_err(|e| format!("解析请求地址失败: {}", e))?;
        let mut next = parsed.path().to_string();
        if let Some(query) = parsed.query() {
            next.push('?');
            next.push_str(query);
        }
        return Ok(next);
    }

    let parsed = url::Url::parse(&format!("http://localhost{}", target))
        .map_err(|e| format!("解析请求路径失败: {}", e))?;
    let mut next = parsed.path().to_string();
    if let Some(query) = parsed.query() {
        next.push('?');
        next.push_str(query);
    }
    Ok(next)
}

fn extract_local_api_key(headers: &HashMap<String, String>) -> Option<String> {
    if let Some(value) = headers.get("authorization") {
        let trimmed = value.trim();
        if let Some(rest) = trimmed.strip_prefix("Bearer ") {
            let token = rest.trim();
            if !token.is_empty() {
                return Some(token.to_string());
            }
        }
        if let Some(rest) = trimmed.strip_prefix("bearer ") {
            let token = rest.trim();
            if !token.is_empty() {
                return Some(token.to_string());
            }
        }
    }

    headers
        .get("x-api-key")
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn is_local_models_request(target: &str) -> bool {
    target == "/v1/models" || target.starts_with("/v1/models?")
}

fn build_local_models_response() -> Value {
    let data: Vec<Value> = supported_codex_model_ids()
        .into_iter()
        .map(|model| {
            json!({
                "id": model,
                "object": "model",
                "created": 0,
                "owned_by": "openai",
            })
        })
        .collect();

    json!({
        "object": "list",
        "data": data,
    })
}

fn usage_number(value: Option<&Value>) -> Option<u64> {
    value.and_then(Value::as_u64).or_else(|| {
        value
            .and_then(Value::as_i64)
            .filter(|number| *number >= 0)
            .map(|number| number as u64)
    })
}

fn non_null_child<'a>(value: &'a Value, key: &str) -> Option<&'a Value> {
    value.get(key).filter(|item| !item.is_null())
}

fn extract_usage_capture(value: &Value) -> Option<UsageCapture> {
    let usage = non_null_child(value, "usage")
        .or_else(|| {
            value
                .get("response")
                .and_then(|item| non_null_child(item, "usage"))
        })
        .or_else(|| {
            value
                .get("response")
                .and_then(|item| item.get("response"))
                .and_then(|item| non_null_child(item, "usage"))
        })
        .or_else(|| non_null_child(value, "usageMetadata"))
        .or_else(|| non_null_child(value, "usage_metadata"))
        .or_else(|| {
            value
                .get("response")
                .and_then(|item| non_null_child(item, "usageMetadata"))
        })
        .or_else(|| {
            value
                .get("response")
                .and_then(|item| non_null_child(item, "usage_metadata"))
        })?;

    let input_tokens = usage_number(
        usage
            .get("input_tokens")
            .or_else(|| usage.get("prompt_tokens"))
            .or_else(|| usage.get("promptTokenCount")),
    )
    .unwrap_or(0);
    let output_tokens = usage_number(
        usage
            .get("output_tokens")
            .or_else(|| usage.get("completion_tokens"))
            .or_else(|| usage.get("candidatesTokenCount")),
    )
    .unwrap_or(0);
    let explicit_total_tokens = usage_number(
        usage
            .get("total_tokens")
            .or_else(|| usage.get("totalTokenCount")),
    );
    let cached_tokens = usage_number(
        usage
            .get("cached_tokens")
            .or_else(|| {
                usage
                    .get("input_tokens_details")
                    .and_then(|item| item.get("cached_tokens"))
            })
            .or_else(|| {
                usage
                    .get("prompt_tokens_details")
                    .and_then(|item| item.get("cached_tokens"))
            })
            .or_else(|| usage.get("cachedContentTokenCount")),
    )
    .unwrap_or(0);
    let reasoning_tokens = usage_number(
        usage
            .get("reasoning_tokens")
            .or_else(|| {
                usage
                    .get("output_tokens_details")
                    .and_then(|item| item.get("reasoning_tokens"))
            })
            .or_else(|| {
                usage
                    .get("completion_tokens_details")
                    .and_then(|item| item.get("reasoning_tokens"))
            })
            .or_else(|| usage.get("thoughtsTokenCount")),
    )
    .unwrap_or(0);

    Some(UsageCapture {
        input_tokens,
        output_tokens,
        total_tokens: if explicit_total_tokens.unwrap_or(0) == 0 {
            input_tokens
                .saturating_add(output_tokens)
                .saturating_add(reasoning_tokens)
        } else {
            explicit_total_tokens.unwrap_or(0)
        },
        cached_tokens,
        reasoning_tokens,
    })
}

fn extract_response_id(value: &Value) -> Option<String> {
    non_null_child(value, "id")
        .and_then(Value::as_str)
        .or_else(|| {
            value
                .get("response")
                .and_then(|item| non_null_child(item, "id"))
                .and_then(Value::as_str)
        })
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn should_treat_response_as_stream(content_type: &str, request_is_stream: bool) -> bool {
    request_is_stream
        || content_type
            .to_ascii_lowercase()
            .contains("text/event-stream")
}

fn find_sse_frame_boundary(buffer: &[u8]) -> Option<(usize, usize)> {
    if buffer.len() < 2 {
        return None;
    }

    for index in 0..buffer.len().saturating_sub(1) {
        if index + 3 < buffer.len() && &buffer[index..index + 4] == b"\r\n\r\n" {
            return Some((index, 4));
        }
        if &buffer[index..index + 2] == b"\n\n" {
            return Some((index, 2));
        }
    }

    None
}

impl ResponseUsageCollector {
    fn new(is_stream: bool) -> Self {
        Self {
            is_stream,
            body: Vec::new(),
            stream_buffer: Vec::new(),
            usage: None,
            response_id: None,
            response_completed_seen: false,
            compaction_summary_seen: false,
        }
    }

    fn feed(&mut self, chunk: &[u8]) {
        if chunk.is_empty() {
            return;
        }

        if self.is_stream {
            self.feed_stream_chunk(chunk);
        } else {
            self.body.extend_from_slice(chunk);
        }
    }

    fn finish(mut self) -> ResponseCapture {
        if self.is_stream {
            self.process_stream_buffer(true);
            ResponseCapture {
                usage: self.usage,
                response_id: self.response_id,
                response_completed_seen: self.response_completed_seen,
                compaction_summary_seen: self.compaction_summary_seen,
            }
        } else {
            let parsed = serde_json::from_slice::<Value>(&self.body).ok();
            let mut response_capture = ResponseCapture {
                usage: parsed.as_ref().and_then(extract_usage_capture),
                response_id: parsed.as_ref().and_then(extract_response_id),
                ..Default::default()
            };
            if let Some(parsed) = parsed.as_ref() {
                update_response_capture_trace(&mut response_capture, parsed, None);
            }
            response_capture
        }
    }

    fn feed_stream_chunk(&mut self, chunk: &[u8]) {
        self.stream_buffer.extend_from_slice(chunk);
        self.process_stream_buffer(false);
    }

    fn process_stream_buffer(&mut self, flush_tail: bool) {
        loop {
            let Some((boundary_index, separator_len)) =
                find_sse_frame_boundary(&self.stream_buffer)
            else {
                break;
            };
            let frame = self.stream_buffer[..boundary_index].to_vec();
            self.stream_buffer.drain(..boundary_index + separator_len);
            self.process_stream_frame(&frame);
        }

        if flush_tail && !self.stream_buffer.is_empty() {
            let frame = std::mem::take(&mut self.stream_buffer);
            self.process_stream_frame(&frame);
        }
    }

    fn process_stream_frame(&mut self, frame: &[u8]) {
        if frame.is_empty() {
            return;
        }

        let text = String::from_utf8_lossy(frame);
        let mut event_name: Option<String> = None;
        let mut data_lines = Vec::new();
        for raw_line in text.lines() {
            let line = raw_line.trim();
            if let Some(rest) = line.strip_prefix("event:") {
                let value = rest.trim();
                if !value.is_empty() {
                    event_name = Some(value.to_string());
                }
                continue;
            }
            if let Some(rest) = line.strip_prefix("data:") {
                let payload = rest.trim();
                if !payload.is_empty() {
                    data_lines.push(payload.to_string());
                }
            }
        }

        let payload = if data_lines.is_empty() {
            let trimmed = text.trim();
            if trimmed.is_empty() {
                return;
            }
            trimmed.to_string()
        } else {
            data_lines.join("\n")
        };

        if payload == "[DONE]" {
            return;
        }

        if let Ok(value) = serde_json::from_str::<Value>(&payload) {
            if let Some(usage) = extract_usage_capture(&value) {
                self.usage = Some(usage);
            }
            if self.response_id.is_none() {
                self.response_id = extract_response_id(&value);
            }
            let mut response_capture = ResponseCapture {
                usage: self.usage.clone(),
                response_id: self.response_id.clone(),
                response_completed_seen: self.response_completed_seen,
                compaction_summary_seen: self.compaction_summary_seen,
            };
            update_response_capture_trace(&mut response_capture, &value, event_name.as_deref());
            self.usage = response_capture.usage;
            self.response_id = response_capture.response_id;
            self.response_completed_seen = response_capture.response_completed_seen;
            self.compaction_summary_seen = response_capture.compaction_summary_seen;
        }
    }
}

fn resolve_upstream_target(target: &str) -> Result<String, String> {
    if !target.starts_with("/v1") {
        return Err("仅支持 /v1 路径".to_string());
    }

    let trimmed = target.trim_start_matches("/v1");
    if trimmed.is_empty() {
        Ok("/".to_string())
    } else if trimmed.starts_with('/') {
        Ok(trimmed.to_string())
    } else {
        Ok(format!("/{}", trimmed))
    }
}

fn is_stream_request(headers: &HashMap<String, String>, body: &[u8]) -> bool {
    if let Some(accept) = headers.get("accept") {
        if accept.to_ascii_lowercase().contains("text/event-stream") {
            return true;
        }
    }

    serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|value| value.get("stream").and_then(Value::as_bool))
        .unwrap_or(false)
}

fn is_websocket_upgrade_request(headers: &HashMap<String, String>) -> bool {
    let upgrade_to_websocket = headers
        .get("upgrade")
        .map(|value| value.trim().eq_ignore_ascii_case("websocket"))
        .unwrap_or(false);
    let connection_has_upgrade = headers
        .get("connection")
        .map(|value| {
            value
                .split(',')
                .any(|part| part.trim().eq_ignore_ascii_case("upgrade"))
        })
        .unwrap_or(false);
    let has_websocket_key = headers.contains_key("sec-websocket-key");

    upgrade_to_websocket || (connection_has_upgrade && has_websocket_key)
}

fn is_responses_websocket_upgrade_request(request: &ParsedRequest) -> bool {
    request.method.eq_ignore_ascii_case("GET")
        && is_responses_request(&request.target)
        && is_websocket_upgrade_request(&request.headers)
}

fn resolve_upstream_account_id(account: &CodexAccount) -> Option<String> {
    account
        .account_id
        .clone()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            codex_account::extract_chatgpt_account_id_from_access_token(
                &account.tokens.access_token,
            )
        })
}

#[cfg(test)]
fn should_try_next_account(status: StatusCode, body: &str) -> bool {
    classify_codex_upstream_error(status, None, body).safe_for_request_failover()
}

fn json_response_with_extra_headers(
    status: u16,
    status_text: &str,
    body: &Value,
    extra_headers: &[(&str, String)],
) -> Vec<u8> {
    let body_bytes = serde_json::to_vec(body).unwrap_or_else(|_| b"{}".to_vec());
    let mut headers = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: application/json; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\nAccess-Control-Allow-Origin: *\r\nAccess-Control-Allow-Methods: GET, POST, OPTIONS\r\nAccess-Control-Allow-Headers: {}\r\n\r\n",
        status,
        status_text,
        body_bytes.len(),
        CORS_ALLOW_HEADERS
    );
    if !extra_headers.is_empty() {
        let mut head = format!(
            "HTTP/1.1 {} {}\r\nContent-Type: application/json; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\nAccess-Control-Allow-Origin: *\r\nAccess-Control-Allow-Methods: GET, POST, OPTIONS\r\nAccess-Control-Allow-Headers: {}\r\n",
            status,
            status_text,
            body_bytes.len(),
            CORS_ALLOW_HEADERS
        );
        for (name, value) in extra_headers {
            if !name.trim().is_empty() && !value.trim().is_empty() {
                head.push_str(name.trim());
                head.push_str(": ");
                head.push_str(value.trim());
                head.push_str("\r\n");
            }
        }
        head.push_str("\r\n");
        headers = head;
    }
    let mut response = headers.into_bytes();
    response.extend_from_slice(&body_bytes);
    response
}

fn json_response(status: u16, status_text: &str, body: &Value) -> Vec<u8> {
    json_response_with_extra_headers(status, status_text, body, &[])
}

fn json_response_with_retry_after(
    status: u16,
    status_text: &str,
    body: &Value,
    retry_after: Option<Duration>,
) -> Vec<u8> {
    let Some(wait) = retry_after else {
        return json_response(status, status_text, body);
    };
    json_response_with_extra_headers(
        status,
        status_text,
        body,
        &[("Retry-After", wait.as_secs().max(1).to_string())],
    )
}

fn sanitize_response_header_value(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    if value
        .bytes()
        .any(|byte| matches!(byte, b'\r' | b'\n') || byte.is_ascii_control())
    {
        return None;
    }
    Some(value.to_string())
}

fn upstream_response_header_value(
    headers: &reqwest::header::HeaderMap,
    header_name: &str,
) -> Option<String> {
    headers
        .get(header_name)
        .and_then(|value| value.to_str().ok())
        .and_then(sanitize_response_header_value)
}

fn should_emit_codex_turn_state_for_response(request: &ParsedRequest, status: StatusCode) -> bool {
    status.is_success() && is_responses_request(&request.target)
}

fn codex_turn_state_response_header(
    request: &ParsedRequest,
    upstream_headers: &reqwest::header::HeaderMap,
    status: StatusCode,
) -> Option<ResponseHeaderValue> {
    if !should_emit_codex_turn_state_for_response(request, status) {
        return None;
    }

    let value = upstream_response_header_value(upstream_headers, X_CODEX_TURN_STATE_HEADER)
        .or_else(|| {
            request_header_value(request, X_CODEX_TURN_STATE_HEADER)
                .and_then(sanitize_response_header_value)
        })
        .unwrap_or_else(generated_codex_turn_state);
    Some(ResponseHeaderValue {
        name: X_CODEX_TURN_STATE_HEADER,
        value,
    })
}

fn options_response() -> Vec<u8> {
    let headers = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: 0\r\nConnection: close\r\nAccess-Control-Allow-Origin: *\r\nAccess-Control-Allow-Methods: GET, POST, OPTIONS\r\nAccess-Control-Allow-Headers: {}\r\n\r\n",
        CORS_ALLOW_HEADERS
    );
    headers.into_bytes()
}

fn truncate_log_field(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }

    let mut truncated: String = value.chars().take(max_chars.saturating_sub(1)).collect();
    truncated.push('…');
    truncated
}

fn safe_log_field(value: Option<&str>, max_chars: usize) -> String {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return "-".to_string();
    };

    let sanitized: String = value
        .chars()
        .map(|ch| match ch {
            '\r' | '\n' | '\t' => '_',
            _ if ch.is_control() => '_',
            _ => ch,
        })
        .collect();

    truncate_log_field(&sanitized, max_chars)
}

fn request_header_value<'a>(request: &'a ParsedRequest, header_name: &str) -> Option<&'a str> {
    request
        .headers
        .get(header_name)
        .or_else(|| {
            request
                .headers
                .iter()
                .find(|(key, _)| key.eq_ignore_ascii_case(header_name))
                .map(|(_, value)| value)
        })
        .map(String::as_str)
}

fn failure_log_route(request: Option<&ParsedRequest>) -> String {
    let route = request
        .map(|value| {
            value
                .target
                .split('?')
                .next()
                .unwrap_or(value.target.as_str())
        })
        .map(str::trim)
        .filter(|value| !value.is_empty());

    safe_log_field(route, 128)
}

fn failure_log_request_id_with_source(request: Option<&ParsedRequest>) -> (String, &'static str) {
    let Some(request) = request else {
        return ("-".to_string(), "none");
    };

    if let Some(value) = codex_turn_state_request_id(request) {
        return (value, "codex_turn_state");
    }
    if let Some((value, source)) = codex_turn_metadata_request_id_with_source(request) {
        return (value, source);
    }

    for header_name in [
        "x-client-request-id",
        "x-request-id",
        "request-id",
        "openai-request-id",
    ] {
        if let Some(value) = request_header_value(request, header_name) {
            let source = match header_name {
                "x-client-request-id" => "client_request_id",
                "x-request-id" => "x_request_id",
                "request-id" => "request_id_header",
                "openai-request-id" => "openai_request_id",
                _ => "header_request_id",
            };
            return (safe_log_field(Some(value), 96), source);
        }
    }

    ("-".to_string(), "none")
}

fn failure_log_request_id(request: Option<&ParsedRequest>) -> String {
    failure_log_request_id_with_source(request).0
}

fn failure_log_model(request: Option<&ParsedRequest>) -> String {
    let Some(request) = request else {
        return "-".to_string();
    };

    let hint = build_request_routing_hint(request);
    safe_log_field(Some(hint.model_key.as_str()), 96)
}

fn failure_log_account_hash(account_id: Option<&str>) -> String {
    let Some(account_id) = account_id.map(str::trim).filter(|value| !value.is_empty()) else {
        return "-".to_string();
    };

    let digest = Sha256::digest(account_id.as_bytes());
    let hex = format!("{:x}", digest);
    format!("sha256:{}", &hex[..12])
}

fn build_audit_context(request: &ParsedRequest, account_id: Option<&str>) -> AuditContext {
    let (request_id, request_id_source) = failure_log_request_id_with_source(Some(request));
    let (turn_lineage_id, turn_lineage_source) = request_lineage_id_with_source(request);
    let previous_response_id_hash = previous_response_id_hash(request);
    AuditContext {
        request_id,
        request_id_source: request_id_source.to_string(),
        route: failure_log_route(Some(request)),
        model: failure_log_model(Some(request)),
        account_hash: failure_log_account_hash(account_id),
        gateway_request_id: request.gateway_request_id.clone(),
        turn_lineage_id,
        turn_lineage_source: turn_lineage_source.map(str::to_string),
        is_continuation: previous_response_id_hash.is_some(),
        is_auto_compact_candidate: request_body_is_auto_compact_candidate(request),
        previous_response_id_hash,
    }
}

fn audit_key_is_sensitive(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    [
        "authorization",
        "api_key",
        "apikey",
        "x-api-key",
        "openai_api_key",
        "access_token",
        "refresh_token",
        "id_token",
        "oauth_token",
        "prompt",
        "content",
        "messages",
        "request_body",
        "response_body",
        "upstream_body",
        "raw_body",
        "body",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
}

fn audit_value_is_sensitive(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    lower.contains("bearer ")
        || lower.contains("sk-")
        || lower.contains("raw prompt")
        || lower.contains("authorization")
        || value.contains('@')
}

fn safe_audit_detail_value(key: &str, value: &str) -> Option<String> {
    if audit_key_is_sensitive(key) {
        return Some("[redacted]".to_string());
    }

    if audit_value_is_sensitive(value) {
        return Some("[redacted]".to_string());
    }

    let safe = safe_log_field(Some(value), 160);
    (safe != "-").then_some(safe)
}

fn safe_audit_label(value: Option<&str>, max_chars: usize) -> Option<String> {
    let value = safe_log_field(value, max_chars);
    (value != "-").then_some(value)
}

fn build_audit_event(
    timestamp: i64,
    context: &AuditContext,
    phase: &str,
    status: Option<u16>,
    error_type: Option<&str>,
    stream_state: Option<&str>,
    outcome: Option<&str>,
    detail: BTreeMap<String, String>,
) -> CodexLocalAccessAuditEvent {
    let mut detail: BTreeMap<String, String> = detail
        .into_iter()
        .filter_map(|(key, value)| {
            let key = safe_log_field(Some(&key), 64);
            if key == "-" {
                return None;
            }
            safe_audit_detail_value(&key, &value).map(|value| (key, value))
        })
        .collect();
    let request_id_source = safe_log_field(Some(&context.request_id_source), 64);
    if request_id_source != "-" {
        detail
            .entry("request_id_source".to_string())
            .or_insert(request_id_source);
    }
    let gateway_request_id = safe_log_field(Some(&context.gateway_request_id), 96);
    if gateway_request_id != "-" {
        detail
            .entry("gateway_request_id".to_string())
            .or_insert(gateway_request_id);
    }
    if let Some(value) = context
        .turn_lineage_id
        .as_deref()
        .and_then(|value| safe_audit_label(Some(value), 128))
    {
        detail.entry("turn_lineage_id".to_string()).or_insert(value);
    }
    if let Some(value) = context
        .turn_lineage_source
        .as_deref()
        .and_then(|value| safe_audit_label(Some(value), 64))
    {
        detail
            .entry("turn_lineage_source".to_string())
            .or_insert(value);
    }
    if let Some(value) = context
        .previous_response_id_hash
        .as_deref()
        .and_then(|value| safe_audit_label(Some(value), 128))
    {
        detail
            .entry("previous_response_id_hash".to_string())
            .or_insert(value);
    }
    detail
        .entry("is_continuation".to_string())
        .or_insert(context.is_continuation.to_string());
    detail
        .entry("is_auto_compact_candidate".to_string())
        .or_insert(context.is_auto_compact_candidate.to_string());

    CodexLocalAccessAuditEvent {
        schema_version: CODEX_LOCAL_ACCESS_AUDIT_SCHEMA_VERSION,
        timestamp,
        request_id: context.request_id.clone(),
        phase: safe_log_field(Some(phase), 64),
        route: context.route.clone(),
        model: context.model.clone(),
        account_hash: context.account_hash.clone(),
        status,
        error_type: safe_audit_label(error_type, 96),
        stream_state: safe_audit_label(stream_state, 64),
        outcome: safe_audit_label(outcome, 64),
        detail,
    }
}

fn audit_rotated_path(path: &Path) -> PathBuf {
    path.with_extension("jsonl.1")
}

fn audit_day_bucket(timestamp_ms: i64) -> i64 {
    timestamp_ms.div_euclid(DAY_WINDOW_MS)
}

fn first_audit_timestamp_from_path(path: &Path) -> Result<Option<i64>, String> {
    if !path.exists() {
        return Ok(None);
    }

    let content =
        std::fs::read_to_string(path).map_err(|e| format!("读取审计日志轮转状态失败: {}", e))?;
    let Some(line) = content.lines().find(|line| !line.trim().is_empty()) else {
        return Ok(None);
    };
    let mut values = serde_json::Deserializer::from_str(line).into_iter::<Value>();
    let value = match values.next() {
        Some(Ok(value)) => value,
        Some(Err(err)) => return Err(format!("解析审计日志轮转状态失败: {}", err)),
        None => return Ok(None),
    };
    Ok(value.get("timestamp").and_then(Value::as_i64))
}

fn should_rotate_audit_by_day(
    path: &Path,
    event: &CodexLocalAccessAuditEvent,
) -> Result<bool, String> {
    let Some(first_timestamp) = first_audit_timestamp_from_path(path)? else {
        return Ok(false);
    };
    Ok(audit_day_bucket(first_timestamp) != audit_day_bucket(event.timestamp))
}

fn append_audit_event_to_path(
    path: &Path,
    event: &CodexLocalAccessAuditEvent,
    max_bytes: usize,
) -> Result<(), String> {
    let _append_guard = audit_trail_append_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let parent = path.parent().ok_or("无法定位审计日志目录")?;
    std::fs::create_dir_all(parent).map_err(|e| format!("创建审计日志目录失败: {}", e))?;

    let should_rotate_by_size = path
        .metadata()
        .map(|metadata| metadata.len() as usize > max_bytes)
        .unwrap_or(false);
    let should_rotate_by_day = should_rotate_audit_by_day(path, event)?;

    if should_rotate_by_size || should_rotate_by_day {
        let rotated_path = audit_rotated_path(path);
        if rotated_path.exists() {
            std::fs::remove_file(&rotated_path)
                .map_err(|e| format!("删除旧审计日志轮转文件失败: {}", e))?;
        }
        std::fs::rename(path, &rotated_path).map_err(|e| format!("轮转审计日志失败: {}", e))?;
    }

    let mut line =
        serde_json::to_string(event).map_err(|e| format!("序列化审计事件失败: {}", e))?;
    line.push('\n');
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| format!("打开审计日志失败: {}", e))?;
    file.write_all(line.as_bytes())
        .map_err(|e| format!("写入审计事件失败: {}", e))
}

fn append_audit_event_to_disk(event: &CodexLocalAccessAuditEvent) -> Result<(), String> {
    let path = local_access_audit_file_path()?;
    append_audit_event_to_path(&path, event, CODEX_LOCAL_ACCESS_AUDIT_MAX_BYTES)
}

fn record_audit_event(event: CodexLocalAccessAuditEvent) {
    match append_audit_event_to_disk(&event) {
        Ok(()) => mark_audit_trail_healthy(),
        Err(err) => {
            mark_audit_trail_degraded(&err);
            logger::log_warn(&format!(
                "[CodexLocalAccess][AuditTrail] 写入审计事件失败: {}",
                err
            ));
        }
    }
}

fn record_audit_event_from_context(
    context: &AuditContext,
    phase: &str,
    status: Option<u16>,
    error_type: Option<&str>,
    stream_state: Option<&str>,
    outcome: Option<&str>,
    detail: BTreeMap<String, String>,
) {
    record_audit_event(build_audit_event(
        now_ms(),
        context,
        phase,
        status,
        error_type,
        stream_state,
        outcome,
        detail,
    ));
}

fn record_manual_recovery_audit_event(account_id: &str, model: Option<&str>, changed: bool) {
    let model = model.map(str::trim).filter(|value| !value.is_empty());
    let context = AuditContext {
        request_id: "manual_recovery".to_string(),
        request_id_source: "manual".to_string(),
        route: "manual_recovery".to_string(),
        model: safe_log_field(model, 96),
        account_hash: failure_log_account_hash(Some(account_id)),
        gateway_request_id: "manual_recovery".to_string(),
        turn_lineage_id: None,
        turn_lineage_source: None,
        previous_response_id_hash: None,
        is_continuation: false,
        is_auto_compact_candidate: false,
    };
    record_audit_event_from_context(
        &context,
        "manual_recovery",
        None,
        None,
        None,
        Some("recovered"),
        BTreeMap::from([
            (
                "scope".to_string(),
                if model.is_some() { "model" } else { "account" }.to_string(),
            ),
            ("changed".to_string(), changed.to_string()),
        ]),
    );
}

fn record_manual_pause_audit_event(account_id: &str, changed: bool) {
    let context = AuditContext {
        request_id: "manual_pause".to_string(),
        request_id_source: "manual".to_string(),
        route: "manual_pause".to_string(),
        model: "-".to_string(),
        account_hash: failure_log_account_hash(Some(account_id)),
        gateway_request_id: "manual_pause".to_string(),
        turn_lineage_id: None,
        turn_lineage_source: None,
        previous_response_id_hash: None,
        is_continuation: false,
        is_auto_compact_candidate: false,
    };
    record_audit_event_from_context(
        &context,
        "manual_pause",
        None,
        Some("manual_paused"),
        None,
        Some("paused"),
        BTreeMap::from([
            ("scope".to_string(), "account".to_string()),
            ("changed".to_string(), changed.to_string()),
        ]),
    );
}

fn record_runtime_projection_audit_event(
    phase: &str,
    outcome: &str,
    source: &str,
    force: bool,
    from_mode: Option<CodexRuntimeIntegrationMode>,
    to_mode: Option<CodexRuntimeIntegrationMode>,
    risk: Option<&RuntimeProjectionContinuityRisk>,
) {
    let context = AuditContext {
        request_id: "runtime_projection".to_string(),
        request_id_source: "runtime_projection".to_string(),
        route: "runtime_projection".to_string(),
        model: "-".to_string(),
        account_hash: "-".to_string(),
        gateway_request_id: "runtime_projection".to_string(),
        turn_lineage_id: None,
        turn_lineage_source: None,
        previous_response_id_hash: None,
        is_continuation: false,
        is_auto_compact_candidate: false,
    };
    let mut detail = BTreeMap::from([
        ("source".to_string(), source.to_string()),
        ("force".to_string(), force.to_string()),
    ]);
    if let Some(mode) = from_mode {
        detail.insert("from_mode".to_string(), format!("{:?}", mode));
    }
    if let Some(mode) = to_mode {
        detail.insert("to_mode".to_string(), format!("{:?}", mode));
    }
    if let Some(risk) = risk {
        detail.extend(risk.audit_detail());
    }
    record_audit_event_from_context(&context, phase, None, None, None, Some(outcome), detail);
}

fn classified_audit_outcome(classified: &ClassifiedCodexUpstreamError) -> &'static str {
    if classified.manual_required {
        "manual_required"
    } else if classified.safe_for_request_failover() {
        "failover"
    } else if classified.retry_after.is_some() {
        "cooldown"
    } else {
        "error"
    }
}

fn classified_audit_detail(classified: &ClassifiedCodexUpstreamError) -> BTreeMap<String, String> {
    let mut detail = classified.log_fields.clone();
    detail.extend(BTreeMap::from([
        ("source".to_string(), classified.source.as_str().to_string()),
        ("scope".to_string(), classified.scope.as_str().to_string()),
        (
            "manual_required".to_string(),
            classified.manual_required.to_string(),
        ),
        (
            "failover_safe".to_string(),
            classified.safe_for_request_failover().to_string(),
        ),
    ]));

    if let Some(provider_code) = classified.provider_code.as_deref() {
        detail.insert("provider_code".to_string(), provider_code.to_string());
    }
    if let Some(retry_after) = classified.retry_after {
        detail.insert(
            "retry_after_ms".to_string(),
            retry_after.as_millis().to_string(),
        );
    }

    detail
}

fn should_record_quota_classification_trace(classified: &ClassifiedCodexUpstreamError) -> bool {
    classified.status == StatusCode::TOO_MANY_REQUESTS.as_u16()
        || matches!(
            classified.error_type,
            CodexLocalAccessErrorType::UsageLimitReached
                | CodexLocalAccessErrorType::InsufficientQuota
                | CodexLocalAccessErrorType::UpstreamRateLimit
                | CodexLocalAccessErrorType::ModelCapacity
        )
}

fn quota_classification_trace_detail(
    classified: &ClassifiedCodexUpstreamError,
) -> BTreeMap<String, String> {
    let mut detail = classified_audit_detail(classified);
    detail.insert(
        "reset_hint_present".to_string(),
        classified.retry_after.is_some().to_string(),
    );
    if let Some(reset_source) = health_registry_reset_source(classified) {
        detail.insert("reset_source".to_string(), reset_source);
    }
    detail
}

fn persist_health_registry_with_audit(
    account_id: &str,
    model_key: Option<&str>,
    request: &ParsedRequest,
    classified: &ClassifiedCodexUpstreamError,
) {
    let context = build_audit_context(request, Some(account_id));
    let classified_detail = classified_audit_detail(classified);
    record_audit_event_from_context(
        &context,
        "classifier",
        Some(classified.status),
        Some(classified.error_type.as_str()),
        None,
        Some(classified_audit_outcome(classified)),
        classified_detail.clone(),
    );
    if should_record_quota_classification_trace(classified) {
        record_audit_event_from_context(
            &context,
            "quota_classification",
            Some(classified.status),
            Some(classified.error_type.as_str()),
            None,
            Some("classified"),
            quota_classification_trace_detail(classified),
        );
    }

    match persist_health_registry_from_classified_error(account_id, model_key, request, classified)
    {
        Ok(()) => {
            record_audit_event_from_context(
                &context,
                "health_update",
                Some(classified.status),
                Some(classified.error_type.as_str()),
                None,
                Some("recorded"),
                classified_detail.clone(),
            );
            if classified.retry_after.is_some()
                || matches!(
                    classified.error_type,
                    CodexLocalAccessErrorType::UpstreamRateLimit
                        | CodexLocalAccessErrorType::UsageLimitReached
                        | CodexLocalAccessErrorType::InsufficientQuota
                        | CodexLocalAccessErrorType::ModelCapacity
                )
            {
                let phase = if is_model_scoped_cooldown(classified, model_key) {
                    "model_cooldown_applied"
                } else {
                    "account_cooldown_applied"
                };
                record_audit_event_from_context(
                    &context,
                    phase,
                    Some(classified.status),
                    Some(classified.error_type.as_str()),
                    None,
                    Some("recorded"),
                    classified_detail,
                );
            }
        }
        Err(err) => {
            log_health_registry_update_error(&err);
            record_audit_event_from_context(
                &context,
                "health_update",
                Some(classified.status),
                Some(classified.error_type.as_str()),
                None,
                Some("write_failed"),
                BTreeMap::from([("reason".to_string(), "persist_error".to_string())]),
            );
        }
    }
}

fn record_stream_audit_event(
    context: Option<&AuditContext>,
    status: StatusCode,
    stream_state: &str,
    outcome: &str,
    content_type: &str,
) {
    record_stream_audit_event_with_detail(
        context,
        status,
        None,
        stream_state,
        outcome,
        content_type,
        BTreeMap::new(),
    );
}

fn record_stream_audit_event_with_detail(
    context: Option<&AuditContext>,
    status: StatusCode,
    error_type: Option<&str>,
    stream_state: &str,
    outcome: &str,
    content_type: &str,
    mut detail: BTreeMap<String, String>,
) {
    if let Some(context) = context {
        detail
            .entry("content_type".to_string())
            .or_insert_with(|| content_type.to_string());
        record_audit_event_from_context(
            context,
            "stream_write",
            Some(status.as_u16()),
            error_type,
            Some(stream_state),
            Some(outcome),
            detail,
        );
    }
}

fn stream_terminal_audit_detail(
    content_type: &str,
    response_capture: &ResponseCapture,
) -> BTreeMap<String, String> {
    let mut detail = BTreeMap::from([
        ("content_type".to_string(), content_type.to_string()),
        (
            "response_completed_seen".to_string(),
            response_capture.response_completed_seen.to_string(),
        ),
        (
            "compaction_summary_seen".to_string(),
            response_capture.compaction_summary_seen.to_string(),
        ),
        (
            "usage_seen".to_string(),
            response_capture.usage.is_some().to_string(),
        ),
    ]);
    if let Some(response_id_hash) = response_capture
        .response_id
        .as_deref()
        .and_then(|response_id| hashed_request_correlation_id("response", response_id))
    {
        detail.insert("response_id_hash".to_string(), response_id_hash);
    }
    detail
}

fn record_stream_terminal_audit_event(
    context: Option<&AuditContext>,
    status: StatusCode,
    stream_state: &str,
    outcome: &str,
    content_type: &str,
    response_capture: &ResponseCapture,
) {
    if let Some(context) = context {
        record_audit_event_from_context(
            context,
            "stream_terminal",
            Some(status.as_u16()),
            None,
            Some(stream_state),
            Some(outcome),
            stream_terminal_audit_detail(content_type, response_capture),
        );
    }
}

fn record_stream_terminal_error_audit_event(
    context: Option<&AuditContext>,
    status: StatusCode,
    content_type: &str,
    terminal_origin: &str,
    message: &str,
    response_capture: &ResponseCapture,
    terminal_contract: &str,
) {
    if let Some(context) = context {
        let mut detail = stream_terminal_audit_detail(content_type, response_capture);
        detail.insert("terminal_origin".to_string(), terminal_origin.to_string());
        detail.insert("message".to_string(), safe_log_field(Some(message), 512));
        detail.insert(
            "terminal_contract".to_string(),
            terminal_contract.to_string(),
        );
        record_audit_event_from_context(
            context,
            "stream_terminal",
            Some(status.as_u16()),
            Some("upstream_stream_error"),
            Some("upstream_error"),
            Some("error"),
            detail,
        );
    }
}

fn classify_codex_api_failure(status: Option<u16>, detail: &str) -> &'static str {
    let detail = detail.to_ascii_lowercase();

    if status == Some(StatusCode::UNAUTHORIZED.as_u16())
        || detail.contains("unauthorized")
        || detail.contains("鉴权")
    {
        "auth_failed"
    } else if detail.contains("api 服务号池")
        || detail.contains("api 服务账号均在冷却")
        || detail.contains("可用账号均在冷却")
        || detail.contains("本地接入集合暂无可用账号")
    {
        "pool_unavailable"
    } else if detail.contains("websocket") || detail.contains("web socket") {
        "unsupported_websocket"
    } else if detail.contains("本地接入队列")
        || detail.contains("本地接入请求超时")
        || detail.contains("local backpressure")
    {
        "local_backpressure"
    } else if status == Some(StatusCode::TOO_MANY_REQUESTS.as_u16())
        || detail.contains("rate limit")
        || detail.contains("usage_limit")
        || detail.contains("quota")
        || detail.contains("额度")
    {
        "rate_limited"
    } else if detail.contains("预处理") || detail.contains("prepare") {
        "account_prepare_failed"
    } else if detail.contains("刷新") || detail.contains("refresh") {
        "account_refresh_failed"
    } else if detail.contains("free 账号") || detail.contains("free account") {
        "free_account_restricted"
    } else if detail.contains("暂无账号") || detail.contains("no account") {
        "no_account"
    } else if detail.contains("timeout") || detail.contains("超时") {
        "timeout"
    } else if detail.contains("上游请求") || detail.contains("upstream request") {
        "upstream_request_failed"
    } else if detail.contains("上游返回") || detail.contains("upstream") || status.is_some() {
        "upstream_status"
    } else {
        "proxy_error"
    }
}

fn build_codex_api_failure_log(
    request: Option<&ParsedRequest>,
    status: Option<u16>,
    account_id: Option<&str>,
    latency_ms: Option<u64>,
    detail: &str,
) -> String {
    let status_text = status
        .map(|value| value.to_string())
        .unwrap_or_else(|| "-".to_string());
    let latency_text = latency_ms
        .map(|value| value.to_string())
        .unwrap_or_else(|| "-".to_string());

    format!(
        "[CodexLocalAccess][Failure] error_type={} status={} route={} model={} latency_ms={} account_hash={} request_id={}",
        classify_codex_api_failure(status, detail),
        status_text,
        failure_log_route(request),
        failure_log_model(request),
        latency_text,
        failure_log_account_hash(account_id),
        failure_log_request_id(request),
    )
}

fn log_codex_api_failure(
    _addr: Option<&std::net::SocketAddr>,
    request: Option<&ParsedRequest>,
    status: Option<u16>,
    account_id: Option<&str>,
    _account_email: Option<&str>,
    latency_ms: Option<u64>,
    detail: &str,
) {
    logger::log_codex_api_warn(&build_codex_api_failure_log(
        request, status, account_id, latency_ms, detail,
    ));
}

async fn write_json_error_response(
    stream: &mut TcpStream,
    addr: Option<&std::net::SocketAddr>,
    request: Option<&ParsedRequest>,
    status: u16,
    status_text: &str,
    message: &str,
    account_id: Option<&str>,
    account_email: Option<&str>,
    latency_ms: Option<u64>,
) -> Result<(), String> {
    log_codex_api_failure(
        addr,
        request,
        Some(status),
        account_id,
        account_email,
        latency_ms,
        message,
    );

    if let Some(request) = request {
        let context = build_audit_context(request, account_id);
        let mut detail = BTreeMap::new();
        if let Some(latency_ms) = latency_ms {
            detail.insert("latency_ms".to_string(), latency_ms.to_string());
        }
        record_audit_event_from_context(
            &context,
            "final_response",
            Some(status),
            Some(classify_codex_api_failure(Some(status), message)),
            None,
            Some("error"),
            detail,
        );
    }

    let response = json_response(status, status_text, &json!({ "error": message }));
    stream
        .write_all(&response)
        .await
        .map_err(|e| format!("写入错误响应失败: {}", e))
}

fn build_proxy_dispatch_error_body(
    request: &ParsedRequest,
    status: u16,
    message: &str,
    retry_after: Option<Duration>,
) -> Value {
    if status == StatusCode::TOO_MANY_REQUESTS.as_u16()
        && request_has_codex_sticky_routing_boundary(request)
    {
        let resets_in_seconds = retry_after.map(duration_to_ceiled_seconds_i64);
        return json!({
            "error": {
                "type": "usage_limit_reached",
                "code": "usage_limit_reached",
                "message": message,
                "resets_at": resets_in_seconds.map(|seconds| chrono::Utc::now().timestamp().saturating_add(seconds)),
                "resets_in_seconds": resets_in_seconds,
            }
        });
    }

    json!({ "error": message })
}

fn proxy_dispatch_final_error_type(
    request: &ParsedRequest,
    status: u16,
    message: &str,
) -> &'static str {
    if status == StatusCode::TOO_MANY_REQUESTS.as_u16()
        && request_has_codex_sticky_routing_boundary(request)
    {
        return CodexLocalAccessErrorType::UsageLimitReached.as_str();
    }

    classify_codex_api_failure(Some(status), message)
}

fn proxy_dispatch_final_error_detail(
    request: &ParsedRequest,
    status: u16,
    message: &str,
    retry_after: Option<Duration>,
    latency_ms: u64,
) -> BTreeMap<String, String> {
    let mut detail = BTreeMap::from([("latency_ms".to_string(), latency_ms.to_string())]);
    if let Some(retry_after) = retry_after {
        detail.insert(
            "retry_after_ms".to_string(),
            retry_after.as_millis().to_string(),
        );
    }
    detail.insert("message".to_string(), safe_log_field(Some(message), 512));

    if status == StatusCode::TOO_MANY_REQUESTS.as_u16() {
        if let Some(boundary) = official_codex_sticky_routing_boundary(request) {
            detail.insert(
                "provider_code".to_string(),
                "usage_limit_reached".to_string(),
            );
            detail.insert(
                "terminal_origin".to_string(),
                "upstream_quota_error".to_string(),
            );
            detail.insert("sticky_boundary".to_string(), boundary.reason().to_string());
        }
    }

    detail
}

async fn write_http_response(
    stream: &mut TcpStream,
    status: u16,
    status_text: &str,
    content_type: &str,
    body: &[u8],
) -> Result<(), String> {
    write_http_response_with_extra_headers(stream, status, status_text, content_type, body, &[])
        .await
}

async fn write_http_response_with_extra_headers(
    stream: &mut TcpStream,
    status: u16,
    status_text: &str,
    content_type: &str,
    body: &[u8],
    extra_headers: &[ResponseHeaderValue],
) -> Result<(), String> {
    let mut headers = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\nAccess-Control-Allow-Origin: *\r\nAccess-Control-Allow-Methods: GET, POST, OPTIONS\r\nAccess-Control-Allow-Headers: {}\r\n",
        status,
        status_text,
        content_type,
        body.len(),
        CORS_ALLOW_HEADERS
    );
    for header in extra_headers {
        if let Some(value) = sanitize_response_header_value(&header.value) {
            headers.push_str(header.name);
            headers.push_str(": ");
            headers.push_str(&value);
            headers.push_str("\r\n");
        }
    }
    headers.push_str("\r\n");
    stream
        .write_all(headers.as_bytes())
        .await
        .map_err(|e| format!("写入响应头失败: {}", e))?;
    stream
        .write_all(body)
        .await
        .map_err(|e| format!("写入响应体失败: {}", e))?;
    Ok(())
}

async fn write_chunked_response_headers(
    stream: &mut TcpStream,
    status: StatusCode,
    status_text: &str,
    content_type: &str,
    upstream_headers: &reqwest::header::HeaderMap,
    extra_headers: &[ResponseHeaderValue],
) -> Result<(), String> {
    let mut response_headers = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nTransfer-Encoding: chunked\r\nConnection: close\r\nAccess-Control-Allow-Origin: *\r\nAccess-Control-Allow-Methods: GET, POST, OPTIONS\r\nAccess-Control-Allow-Headers: {}\r\n",
        status.as_u16(),
        status_text,
        content_type,
        CORS_ALLOW_HEADERS
    );

    for header_name in ["x-request-id", "openai-processing-ms"] {
        if let Some(value) = upstream_headers
            .get(header_name)
            .and_then(|item| item.to_str().ok())
        {
            response_headers.push_str(&format!("{}: {}\r\n", header_name, value));
        }
    }
    for header in extra_headers {
        if let Some(value) = sanitize_response_header_value(&header.value) {
            response_headers.push_str(header.name);
            response_headers.push_str(": ");
            response_headers.push_str(&value);
            response_headers.push_str("\r\n");
        }
    }

    response_headers.push_str("\r\n");
    stream
        .write_all(response_headers.as_bytes())
        .await
        .map_err(|e| format!("写入响应头失败: {}", e))
}

async fn write_chunked_response_chunk(stream: &mut TcpStream, chunk: &[u8]) -> Result<(), String> {
    if chunk.is_empty() {
        return Ok(());
    }

    let prefix = format!("{:X}\r\n", chunk.len());
    stream
        .write_all(prefix.as_bytes())
        .await
        .map_err(|e| format!("写入响应分块前缀失败: {}", e))?;
    stream
        .write_all(chunk)
        .await
        .map_err(|e| format!("写入响应分块失败: {}", e))?;
    stream
        .write_all(b"\r\n")
        .await
        .map_err(|e| format!("写入响应分块结束失败: {}", e))
}

async fn finish_chunked_response(stream: &mut TcpStream) -> Result<(), String> {
    stream
        .write_all(b"0\r\n\r\n")
        .await
        .map_err(|e| format!("写入响应结束失败: {}", e))
}

fn parse_responses_payload_from_upstream(body_bytes: &[u8]) -> Result<Value, String> {
    if let Ok(parsed) = serde_json::from_slice::<Value>(body_bytes) {
        return Ok(parsed);
    }

    let mut stream_buffer = body_bytes.to_vec();
    let mut completed_response: Option<Value> = None;
    let mut output_text = String::new();
    let mut output_items: Vec<Value> = Vec::new();

    let mut process_frame = |frame: &[u8]| {
        if frame.is_empty() {
            return;
        }
        let text = String::from_utf8_lossy(frame);
        let mut event_name: Option<String> = None;
        let mut data_lines = Vec::new();
        for raw_line in text.lines() {
            let line = raw_line.trim();
            if let Some(rest) = line.strip_prefix("event:") {
                let value = rest.trim();
                if !value.is_empty() {
                    event_name = Some(value.to_string());
                }
                continue;
            }
            if let Some(rest) = line.strip_prefix("data:") {
                let payload = rest.trim();
                if !payload.is_empty() {
                    data_lines.push(payload.to_string());
                }
            }
        }

        let payload = if data_lines.is_empty() {
            let trimmed = text.trim();
            if trimmed.is_empty() {
                return;
            }
            trimmed.to_string()
        } else {
            data_lines.join("\n")
        };
        if payload == "[DONE]" {
            return;
        }

        let Ok(value) = serde_json::from_str::<Value>(&payload) else {
            return;
        };
        match value
            .get("type")
            .and_then(Value::as_str)
            .or(event_name.as_deref())
            .unwrap_or("")
        {
            "response.output_text.delta" => {
                if let Some(delta) = value.get("delta").and_then(Value::as_str) {
                    output_text.push_str(delta);
                }
            }
            "response.output_text.done" => {
                if output_text.trim().is_empty() {
                    if let Some(done_text) = value.get("text").and_then(Value::as_str) {
                        output_text.push_str(done_text);
                    }
                }
            }
            "response.output_item.done" => {
                if let Some(item) = value.get("item") {
                    output_items.push(item.clone());
                }
            }
            event_type if is_responses_completion_event(event_type) => {
                if let Some(response) = value.get("response") {
                    completed_response = Some(response.clone());
                } else {
                    completed_response = Some(value.clone());
                }
            }
            _ => {}
        }
    };

    loop {
        let Some((boundary_index, separator_len)) = find_sse_frame_boundary(&stream_buffer) else {
            break;
        };
        let frame = stream_buffer[..boundary_index].to_vec();
        stream_buffer.drain(..boundary_index + separator_len);
        process_frame(&frame);
    }
    if !stream_buffer.is_empty() {
        process_frame(&stream_buffer);
    }

    let Some(response_value) = completed_response else {
        return Err(
            "解析上游 responses 响应失败: 非 JSON 且未捕获 response.completed/response.done"
                .to_string(),
        );
    };

    let mut root = Map::new();
    match response_value {
        Value::Object(mut response_object) => {
            if response_object
                .get("output")
                .and_then(Value::as_array)
                .map(|items| items.is_empty())
                .unwrap_or(true)
                && !output_items.is_empty()
            {
                response_object.insert("output".to_string(), Value::Array(output_items));
            }
            if !output_text.trim().is_empty() {
                response_object.insert("output_text".to_string(), Value::String(output_text));
            }
            root.insert("response".to_string(), Value::Object(response_object));
        }
        other => {
            root.insert("response".to_string(), other);
            if !output_items.is_empty() {
                root.insert("output".to_string(), Value::Array(output_items));
            }
            if !output_text.trim().is_empty() {
                root.insert("output_text".to_string(), Value::String(output_text));
            }
        }
    }

    Ok(Value::Object(root))
}

fn mime_type_from_output_format(output_format: &str) -> String {
    let output_format = output_format.trim();
    if output_format.contains('/') {
        return output_format.to_string();
    }
    match output_format.to_ascii_lowercase().as_str() {
        "jpg" | "jpeg" => "image/jpeg".to_string(),
        "webp" => "image/webp".to_string(),
        _ => "image/png".to_string(),
    }
}

fn extract_images_from_responses_payload(
    response_body: &Value,
) -> (
    Vec<ImageCallResult>,
    i64,
    Option<Value>,
    Option<ImageCallResult>,
) {
    let root = response_payload_root(response_body);
    let created = root
        .get("created_at")
        .or_else(|| root.get("created"))
        .and_then(Value::as_i64)
        .unwrap_or_else(|| chrono::Utc::now().timestamp());
    let mut results = Vec::new();
    let mut first_meta = None;

    if let Some(output_items) = root.get("output").and_then(Value::as_array) {
        for item in output_items {
            if item.get("type").and_then(Value::as_str) != Some("image_generation_call") {
                continue;
            }
            let result = item
                .get("result")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty());
            let Some(result) = result else {
                continue;
            };
            let entry = ImageCallResult {
                result: result.to_string(),
                revised_prompt: item
                    .get("revised_prompt")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .trim()
                    .to_string(),
                output_format: item
                    .get("output_format")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .trim()
                    .to_string(),
                size: item
                    .get("size")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .trim()
                    .to_string(),
                background: item
                    .get("background")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .trim()
                    .to_string(),
                quality: item
                    .get("quality")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .trim()
                    .to_string(),
            };
            if first_meta.is_none() {
                first_meta = Some(entry.clone());
            }
            results.push(entry);
        }
    }

    let usage = root
        .get("tool_usage")
        .and_then(|tool_usage| tool_usage.get("image_gen"))
        .filter(|value| value.is_object())
        .cloned();

    (results, created, usage, first_meta)
}

fn build_images_api_payload(response_body: &Value, response_format: &str) -> Result<Value, String> {
    let (results, created, usage, first_meta) =
        extract_images_from_responses_payload(response_body);
    if results.is_empty() {
        return Err("upstream did not return image output".to_string());
    }

    let response_format = if response_format.trim().is_empty() {
        "b64_json"
    } else {
        response_format.trim()
    };
    let mut data = Vec::new();
    for image in results {
        let mut item = Map::new();
        if response_format.eq_ignore_ascii_case("url") {
            let mime_type = mime_type_from_output_format(&image.output_format);
            item.insert(
                "url".to_string(),
                Value::String(format!("data:{};base64,{}", mime_type, image.result)),
            );
        } else {
            item.insert("b64_json".to_string(), Value::String(image.result));
        }
        if !image.revised_prompt.is_empty() {
            item.insert(
                "revised_prompt".to_string(),
                Value::String(image.revised_prompt),
            );
        }
        data.push(Value::Object(item));
    }

    let mut out = Map::new();
    out.insert("created".to_string(), json!(created));
    out.insert("data".to_string(), Value::Array(data));

    if let Some(meta) = first_meta {
        if !meta.background.is_empty() {
            out.insert("background".to_string(), Value::String(meta.background));
        }
        if !meta.output_format.is_empty() {
            out.insert(
                "output_format".to_string(),
                Value::String(meta.output_format),
            );
        }
        if !meta.quality.is_empty() {
            out.insert("quality".to_string(), Value::String(meta.quality));
        }
        if !meta.size.is_empty() {
            out.insert("size".to_string(), Value::String(meta.size));
        }
    }
    if let Some(usage) = usage {
        out.insert("usage".to_string(), usage);
    }

    Ok(Value::Object(out))
}

fn push_named_sse_payload(stream_body: &mut String, event_name: &str, payload: Value) {
    let event_name = event_name.trim();
    if !event_name.is_empty() {
        stream_body.push_str("event: ");
        stream_body.push_str(event_name);
        stream_body.push('\n');
    }
    push_sse_payload(stream_body, payload);
}

#[derive(Debug)]
struct ImageStreamTransformer {
    response_format: String,
    stream_prefix: String,
    stream_buffer: Vec<u8>,
    response_capture: ResponseCapture,
}

impl ImageStreamTransformer {
    fn new(response_format: &str, stream_prefix: &str) -> Self {
        Self {
            response_format: if response_format.trim().is_empty() {
                "b64_json".to_string()
            } else {
                response_format.trim().to_ascii_lowercase()
            },
            stream_prefix: stream_prefix.to_string(),
            stream_buffer: Vec::new(),
            response_capture: ResponseCapture::default(),
        }
    }

    fn feed(&mut self, chunk: &[u8]) -> Vec<u8> {
        if chunk.is_empty() {
            return Vec::new();
        }
        self.stream_buffer.extend_from_slice(chunk);
        self.process_buffer(false)
    }

    fn finish(mut self) -> (Vec<u8>, ResponseCapture) {
        let output = self.process_buffer(true);
        (output, self.response_capture)
    }

    fn process_buffer(&mut self, flush_tail: bool) -> Vec<u8> {
        let mut stream_body = String::new();

        loop {
            let Some((boundary_index, separator_len)) =
                find_sse_frame_boundary(&self.stream_buffer)
            else {
                break;
            };
            let frame = self.stream_buffer[..boundary_index].to_vec();
            self.stream_buffer.drain(..boundary_index + separator_len);
            self.process_frame(&frame, &mut stream_body);
        }

        if flush_tail && !self.stream_buffer.is_empty() {
            let frame = std::mem::take(&mut self.stream_buffer);
            self.process_frame(&frame, &mut stream_body);
        }

        stream_body.into_bytes()
    }

    fn process_frame(&mut self, frame: &[u8], stream_body: &mut String) {
        if frame.is_empty() {
            return;
        }

        let text = String::from_utf8_lossy(frame);
        let mut event_name: Option<String> = None;
        let mut data_lines = Vec::new();
        for raw_line in text.lines() {
            let line = raw_line.trim();
            if let Some(rest) = line.strip_prefix("event:") {
                let value = rest.trim();
                if !value.is_empty() {
                    event_name = Some(value.to_string());
                }
                continue;
            }
            if let Some(rest) = line.strip_prefix("data:") {
                let payload = rest.trim();
                if !payload.is_empty() {
                    data_lines.push(payload.to_string());
                }
            }
        }

        let payload = if data_lines.is_empty() {
            let trimmed = text.trim();
            if trimmed.is_empty() {
                return;
            }
            trimmed.to_string()
        } else {
            data_lines.join("\n")
        };

        if payload == "[DONE]" {
            return;
        }

        let Ok(event) = serde_json::from_str::<Value>(&payload) else {
            return;
        };
        if let Some(usage) = extract_usage_capture(&event) {
            self.response_capture.usage = Some(usage);
        }
        if self.response_capture.response_id.is_none() {
            self.response_capture.response_id = extract_response_id(&event);
        }
        update_response_capture_trace(&mut self.response_capture, &event, event_name.as_deref());

        match response_event_type(&event, event_name.as_deref()) {
            "response.image_generation_call.partial_image" => {
                let Some(b64) = event
                    .get("partial_image_b64")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                else {
                    return;
                };
                let output_format = event
                    .get("output_format")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let event_name = format!("{}.partial_image", self.stream_prefix);
                let mut data = Map::new();
                data.insert("type".to_string(), Value::String(event_name.clone()));
                data.insert(
                    "partial_image_index".to_string(),
                    json!(event
                        .get("partial_image_index")
                        .and_then(Value::as_i64)
                        .unwrap_or(0)),
                );
                if self.response_format == "url" {
                    let mime_type = mime_type_from_output_format(output_format);
                    data.insert(
                        "url".to_string(),
                        Value::String(format!("data:{};base64,{}", mime_type, b64)),
                    );
                } else {
                    data.insert("b64_json".to_string(), Value::String(b64.to_string()));
                }
                push_named_sse_payload(stream_body, &event_name, Value::Object(data));
            }
            event_type if is_responses_completion_event(event_type) => {
                let (results, _, usage, _) = extract_images_from_responses_payload(&event);
                if results.is_empty() {
                    push_named_sse_payload(
                        stream_body,
                        "error",
                        json!({ "error": "upstream did not return image output" }),
                    );
                    return;
                }
                let event_name = format!("{}.completed", self.stream_prefix);
                for image in results {
                    let mut data = Map::new();
                    data.insert("type".to_string(), Value::String(event_name.clone()));
                    if self.response_format == "url" {
                        let mime_type = mime_type_from_output_format(&image.output_format);
                        data.insert(
                            "url".to_string(),
                            Value::String(format!("data:{};base64,{}", mime_type, image.result)),
                        );
                    } else {
                        data.insert("b64_json".to_string(), Value::String(image.result));
                    }
                    if let Some(usage) = usage.clone() {
                        data.insert("usage".to_string(), usage);
                    }
                    push_named_sse_payload(stream_body, &event_name, Value::Object(data));
                }
            }
            _ => {}
        }
    }
}

async fn write_chat_completions_compatible_response(
    stream: &mut TcpStream,
    upstream: reqwest::Response,
    stream_mode: bool,
    requested_model: &str,
    original_request_body: &[u8],
    response_headers: &[ResponseHeaderValue],
    audit_context: Option<&AuditContext>,
) -> Result<ResponseCapture, String> {
    let status = upstream.status();
    let status_text = status.canonical_reason().unwrap_or("OK");
    let upstream_headers = upstream.headers().clone();

    if stream_mode {
        let mut write_state = StreamWriteState::default();
        write_chunked_response_headers(
            stream,
            status,
            status_text,
            "text/event-stream; charset=utf-8",
            &upstream_headers,
            response_headers,
        )
        .await?;
        write_state.mark_headers_written();
        debug_assert!(!write_state.can_attempt_account_fallback());
        record_stream_audit_event(
            audit_context,
            status,
            "headers_written",
            "ok",
            "text/event-stream; charset=utf-8",
        );

        let mut transformer =
            ChatCompletionStreamTransformer::new(original_request_body, requested_model);
        let mut body_stream = upstream.bytes_stream();
        let mut wrote_first_chunk = false;
        while let Some(chunk_result) = body_stream.next().await {
            let chunk = match chunk_result {
                Ok(chunk) => chunk,
                Err(e) => {
                    record_stream_audit_event(
                        audit_context,
                        status,
                        "upstream_error",
                        "error",
                        "text/event-stream; charset=utf-8",
                    );
                    return Err(format!("读取上游响应失败: {}", e));
                }
            };
            let transformed = transformer.feed(&chunk);
            write_chunked_response_chunk(stream, &transformed).await?;
            if !wrote_first_chunk && !transformed.is_empty() {
                wrote_first_chunk = true;
                write_state.mark_first_chunk_written();
                record_stream_audit_event(
                    audit_context,
                    status,
                    "first_chunk_written",
                    "ok",
                    "text/event-stream; charset=utf-8",
                );
            }
        }

        let (tail, response_capture) = transformer.finish();
        write_chunked_response_chunk(stream, &tail).await?;
        if !wrote_first_chunk && !tail.is_empty() {
            write_state.mark_first_chunk_written();
            record_stream_audit_event(
                audit_context,
                status,
                "first_chunk_written",
                "ok",
                "text/event-stream; charset=utf-8",
            );
        }
        finish_chunked_response(stream).await?;
        record_stream_audit_event(
            audit_context,
            status,
            "finished",
            "ok",
            "text/event-stream; charset=utf-8",
        );
        record_stream_terminal_audit_event(
            audit_context,
            status,
            "finished",
            "ok",
            "text/event-stream; charset=utf-8",
            &response_capture,
        );
        return Ok(response_capture);
    }

    let body_bytes = upstream
        .bytes()
        .await
        .map_err(|e| format!("读取上游 responses 响应失败: {}", e))?;
    let parsed = parse_responses_payload_from_upstream(&body_bytes)?;
    let mut response_capture = ResponseCapture {
        usage: extract_usage_capture(&parsed),
        response_id: extract_response_id(&parsed),
        ..Default::default()
    };
    update_response_capture_trace(&mut response_capture, &parsed, None);
    let chat_payload =
        build_chat_completion_payload(&parsed, requested_model, original_request_body);

    let payload_bytes = serde_json::to_vec(&chat_payload)
        .map_err(|e| format!("序列化 chat/completions 响应失败: {}", e))?;
    write_http_response_with_extra_headers(
        stream,
        status.as_u16(),
        status_text,
        "application/json; charset=utf-8",
        &payload_bytes,
        response_headers,
    )
    .await?;
    if let Some(context) = audit_context {
        record_audit_event_from_context(
            context,
            "final_response",
            Some(status.as_u16()),
            None,
            None,
            Some("ok"),
            BTreeMap::from([(
                "content_type".to_string(),
                "application/json; charset=utf-8".to_string(),
            )]),
        );
    }

    Ok(response_capture)
}

async fn write_images_compatible_response(
    stream: &mut TcpStream,
    upstream: reqwest::Response,
    stream_mode: bool,
    response_format: &str,
    stream_prefix: &str,
    response_headers: &[ResponseHeaderValue],
    audit_context: Option<&AuditContext>,
) -> Result<ResponseCapture, String> {
    let status = upstream.status();
    let status_text = status.canonical_reason().unwrap_or("OK");
    let upstream_headers = upstream.headers().clone();

    if stream_mode {
        let mut write_state = StreamWriteState::default();
        write_chunked_response_headers(
            stream,
            status,
            status_text,
            "text/event-stream; charset=utf-8",
            &upstream_headers,
            response_headers,
        )
        .await?;
        write_state.mark_headers_written();
        debug_assert!(!write_state.can_attempt_account_fallback());
        record_stream_audit_event(
            audit_context,
            status,
            "headers_written",
            "ok",
            "text/event-stream; charset=utf-8",
        );

        let mut transformer = ImageStreamTransformer::new(response_format, stream_prefix);
        let mut body_stream = upstream.bytes_stream();
        let mut wrote_first_chunk = false;
        while let Some(chunk_result) = body_stream.next().await {
            let chunk = match chunk_result {
                Ok(chunk) => chunk,
                Err(e) => {
                    record_stream_audit_event(
                        audit_context,
                        status,
                        "upstream_error",
                        "error",
                        "text/event-stream; charset=utf-8",
                    );
                    return Err(format!("读取上游图片响应失败: {}", e));
                }
            };
            let transformed = transformer.feed(&chunk);
            write_chunked_response_chunk(stream, &transformed).await?;
            if !wrote_first_chunk && !transformed.is_empty() {
                wrote_first_chunk = true;
                write_state.mark_first_chunk_written();
                record_stream_audit_event(
                    audit_context,
                    status,
                    "first_chunk_written",
                    "ok",
                    "text/event-stream; charset=utf-8",
                );
            }
        }

        let (tail, response_capture) = transformer.finish();
        write_chunked_response_chunk(stream, &tail).await?;
        if !wrote_first_chunk && !tail.is_empty() {
            write_state.mark_first_chunk_written();
            record_stream_audit_event(
                audit_context,
                status,
                "first_chunk_written",
                "ok",
                "text/event-stream; charset=utf-8",
            );
        }
        finish_chunked_response(stream).await?;
        record_stream_audit_event(
            audit_context,
            status,
            "finished",
            "ok",
            "text/event-stream; charset=utf-8",
        );
        record_stream_terminal_audit_event(
            audit_context,
            status,
            "finished",
            "ok",
            "text/event-stream; charset=utf-8",
            &response_capture,
        );
        return Ok(response_capture);
    }

    let body_bytes = upstream
        .bytes()
        .await
        .map_err(|e| format!("读取上游图片响应失败: {}", e))?;
    let parsed = parse_responses_payload_from_upstream(&body_bytes)?;
    let mut response_capture = ResponseCapture {
        usage: extract_usage_capture(&parsed),
        response_id: extract_response_id(&parsed),
        ..Default::default()
    };
    update_response_capture_trace(&mut response_capture, &parsed, None);
    let images_payload = build_images_api_payload(&parsed, response_format)?;
    let payload_bytes = serde_json::to_vec(&images_payload)
        .map_err(|e| format!("序列化 images 响应失败: {}", e))?;

    write_http_response_with_extra_headers(
        stream,
        status.as_u16(),
        status_text,
        "application/json; charset=utf-8",
        &payload_bytes,
        response_headers,
    )
    .await?;
    if let Some(context) = audit_context {
        record_audit_event_from_context(
            context,
            "final_response",
            Some(status.as_u16()),
            None,
            None,
            Some("ok"),
            BTreeMap::from([(
                "content_type".to_string(),
                "application/json; charset=utf-8".to_string(),
            )]),
        );
    }

    Ok(response_capture)
}

async fn write_gateway_response(
    stream: &mut TcpStream,
    upstream: reqwest::Response,
    response_adapter: GatewayResponseAdapter,
    response_headers: &[ResponseHeaderValue],
    audit_context: Option<&AuditContext>,
) -> Result<ResponseCapture, String> {
    match response_adapter {
        GatewayResponseAdapter::Passthrough { request_is_stream } => {
            write_upstream_response(
                stream,
                upstream,
                request_is_stream,
                response_headers,
                audit_context,
            )
            .await
        }
        GatewayResponseAdapter::ChatCompletions {
            stream: stream_mode,
            requested_model,
            original_request_body,
        } => {
            write_chat_completions_compatible_response(
                stream,
                upstream,
                stream_mode,
                requested_model.as_str(),
                original_request_body.as_slice(),
                response_headers,
                audit_context,
            )
            .await
        }
        GatewayResponseAdapter::Images {
            stream: stream_mode,
            response_format,
            stream_prefix,
        } => {
            write_images_compatible_response(
                stream,
                upstream,
                stream_mode,
                response_format.as_str(),
                stream_prefix.as_str(),
                response_headers,
                audit_context,
            )
            .await
        }
    }
}

async fn write_upstream_response(
    stream: &mut TcpStream,
    upstream: reqwest::Response,
    request_is_stream: bool,
    response_headers: &[ResponseHeaderValue],
    audit_context: Option<&AuditContext>,
) -> Result<ResponseCapture, String> {
    let status = upstream.status();
    let status_text = status.canonical_reason().unwrap_or("OK");
    let headers = upstream.headers().clone();
    let content_type = headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("application/json; charset=utf-8");
    write_chunked_response_headers(
        stream,
        status,
        status_text,
        content_type,
        &headers,
        response_headers,
    )
    .await?;
    write_upstream_response_body_chunks(stream, upstream, request_is_stream, audit_context).await
}

async fn write_upstream_response_body_chunks(
    stream: &mut TcpStream,
    upstream: reqwest::Response,
    request_is_stream: bool,
    audit_context: Option<&AuditContext>,
) -> Result<ResponseCapture, String> {
    let status = upstream.status();
    let headers = upstream.headers().clone();
    let content_type = headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("application/json; charset=utf-8");
    let is_stream = should_treat_response_as_stream(content_type, request_is_stream);
    let mut write_state = StreamWriteState::default();
    write_state.mark_headers_written();
    debug_assert!(!write_state.can_attempt_account_fallback());
    record_stream_audit_event(audit_context, status, "headers_written", "ok", content_type);

    let mut usage_collector = ResponseUsageCollector::new(is_stream);
    let mut body_stream = upstream.bytes_stream();
    let mut wrote_first_chunk = false;
    while let Some(chunk_result) = body_stream.next().await {
        let chunk = match chunk_result {
            Ok(chunk) => chunk,
            Err(e) => {
                let message = format!("读取上游响应失败: {}", e);
                let response_capture = usage_collector.finish();
                let mut detail = stream_terminal_audit_detail(content_type, &response_capture);
                detail.insert(
                    "terminal_origin".to_string(),
                    "upstream_stream_error".to_string(),
                );
                detail.insert(
                    "message".to_string(),
                    safe_log_field(Some(message.as_str()), 512),
                );
                detail.insert(
                    "terminal_contract".to_string(),
                    if is_stream {
                        "response_failed_sse"
                    } else {
                        "transport_error"
                    }
                    .to_string(),
                );
                record_stream_audit_event_with_detail(
                    audit_context,
                    status,
                    Some("upstream_stream_error"),
                    "upstream_error",
                    "error",
                    content_type,
                    detail,
                );
                if is_stream {
                    let terminal_sse = build_responses_upstream_stream_error_sse(&message);
                    let write_result =
                        match write_chunked_response_chunk(stream, &terminal_sse).await {
                            Ok(()) => finish_chunked_response(stream).await,
                            Err(err) => Err(err),
                        };
                    match write_result {
                        Ok(()) => {
                            record_stream_terminal_error_audit_event(
                                audit_context,
                                status,
                                content_type,
                                "upstream_stream_error",
                                &message,
                                &response_capture,
                                "response_failed_sse",
                            );
                        }
                        Err(write_err) => {
                            record_stream_terminal_error_audit_event(
                                audit_context,
                                status,
                                content_type,
                                "upstream_stream_error",
                                &format!(
                                    "{}; 写入 downstream terminal SSE 失败: {}",
                                    message, write_err
                                ),
                                &response_capture,
                                "response_failed_sse_write_failed",
                            );
                            return Err(format!("{}; 写入下游失败: {}", message, write_err));
                        }
                    }
                }
                return Err(message);
            }
        };
        if chunk.is_empty() {
            continue;
        }
        write_chunked_response_chunk(stream, &chunk).await?;
        if !wrote_first_chunk {
            wrote_first_chunk = true;
            write_state.mark_first_chunk_written();
            record_stream_audit_event(
                audit_context,
                status,
                "first_chunk_written",
                "ok",
                content_type,
            );
        }
        usage_collector.feed(&chunk);
    }

    finish_chunked_response(stream).await?;
    record_stream_audit_event(audit_context, status, "finished", "ok", content_type);
    let response_capture = usage_collector.finish();
    record_stream_terminal_audit_event(
        audit_context,
        status,
        "finished",
        "ok",
        content_type,
        &response_capture,
    );
    Ok(response_capture)
}

async fn force_refresh_gateway_account(account_id: &str) -> Result<CodexAccount, String> {
    let account =
        codex_account::force_refresh_managed_account(account_id, "本地网关上游返回 401").await?;
    cache_prepared_account(&account).await;
    Ok(account)
}

fn should_retry_upstream_send_error(error: &reqwest::Error) -> bool {
    error.is_timeout() || error.is_connect() || error.is_request()
}

fn upstream_send_retry_delay(retry_attempt: usize) -> Duration {
    let multiplier = match retry_attempt {
        0 | 1 => 1u32,
        2 => 2u32,
        _ => 4u32,
    };
    let delay = UPSTREAM_SEND_RETRY_BASE_DELAY.saturating_mul(multiplier);
    if delay > UPSTREAM_SEND_RETRY_MAX_DELAY {
        UPSTREAM_SEND_RETRY_MAX_DELAY
    } else {
        delay
    }
}

fn should_retry_single_account_upstream_status(status: StatusCode) -> bool {
    matches!(
        status,
        StatusCode::REQUEST_TIMEOUT
            | StatusCode::INTERNAL_SERVER_ERROR
            | StatusCode::BAD_GATEWAY
            | StatusCode::SERVICE_UNAVAILABLE
            | StatusCode::GATEWAY_TIMEOUT
    )
}

fn hard_affinity_inline_retry_wait_limit(_config: &CodexLocalApiSafetyConfig) -> Duration {
    MAX_INLINE_ACCOUNT_RETRY_WAIT
}

fn should_retry_hard_affinity_upstream_failure(
    hard_affinity_active: bool,
    classified: &ClassifiedCodexUpstreamError,
    retry_attempt: usize,
    max_retry_attempts: usize,
    retry_wait_limit: Duration,
) -> Option<Duration> {
    if !hard_affinity_active
        || retry_attempt >= max_retry_attempts
        || !classified.safe_for_request_failover()
    {
        return None;
    }

    classified
        .retry_after
        .filter(|wait| *wait <= retry_wait_limit)
}

fn single_account_status_retry_delay(retry_attempt: usize) -> Duration {
    let multiplier = match retry_attempt {
        0 | 1 => 1u32,
        2 => 2u32,
        _ => 4u32,
    };
    let delay = SINGLE_ACCOUNT_STATUS_RETRY_BASE_DELAY.saturating_mul(multiplier);
    if delay > SINGLE_ACCOUNT_STATUS_RETRY_MAX_DELAY {
        SINGLE_ACCOUNT_STATUS_RETRY_MAX_DELAY
    } else {
        delay
    }
}

fn build_api_key_upstream_url(account: &CodexAccount, target: &str) -> Result<String, String> {
    let base_url = account
        .api_base_url
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("https://api.openai.com/v1");
    let base_url = base_url.trim_end_matches('/');
    let target = if target.starts_with('/') {
        target.to_string()
    } else {
        format!("/{}", target)
    };
    Url::parse(&format!("{}{}", base_url, target))
        .map(|url| url.to_string())
        .map_err(|err| format!("API Key 账号的 Base URL 无效: {}", err))
}

async fn send_upstream_request(
    method: &str,
    target: &str,
    headers: &HashMap<String, String>,
    body: &[u8],
    account: &CodexAccount,
) -> Result<reqwest::Response, String> {
    let method =
        Method::from_bytes(method.as_bytes()).map_err(|e| format!("不支持的请求方法: {}", e))?;
    let api_key = account
        .openai_api_key
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let use_api_key_upstream = account.is_api_key_auth() || api_key.is_some();
    let url = if use_api_key_upstream {
        build_api_key_upstream_url(account, target)?
    } else {
        format!("{}{}", UPSTREAM_CODEX_BASE_URL, target)
    };
    let client = upstream_http_client()?;
    for retry_attempt in 0..=UPSTREAM_SEND_RETRY_ATTEMPTS {
        let mut request = client.request(method.clone(), &url);

        for (name, value) in headers {
            if matches!(
                name.as_str(),
                "authorization"
                    | "host"
                    | "content-length"
                    | "connection"
                    | "accept-encoding"
                    | "upgrade"
                    | "sec-websocket-key"
                    | "sec-websocket-version"
                    | "sec-websocket-protocol"
                    | "sec-websocket-extensions"
                    | "x-api-key"
            ) {
                continue;
            }
            let header_name = HeaderName::from_bytes(name.as_bytes())
                .map_err(|e| format!("无效请求头 {}: {}", name, e))?;
            let header_value = HeaderValue::from_str(value)
                .map_err(|e| format!("无效请求头值 {}: {}", name, e))?;
            request = request.header(header_name, header_value);
        }

        if use_api_key_upstream {
            let Some(api_key) = api_key else {
                return Err("API Key 账号缺少 openai_api_key".to_string());
            };
            request = request.header(AUTHORIZATION, format!("Bearer {}", api_key));
        } else {
            request = request.header(
                AUTHORIZATION,
                format!("Bearer {}", account.tokens.access_token.trim()),
            );
        }
        if !headers.contains_key("user-agent") {
            request = request.header(USER_AGENT, DEFAULT_CODEX_USER_AGENT);
        }
        if !headers.contains_key("originator") {
            request = request.header("Originator", DEFAULT_CODEX_ORIGINATOR);
        }
        if !use_api_key_upstream {
            if let Some(account_id) = resolve_upstream_account_id(account) {
                request = request.header("ChatGPT-Account-Id", account_id);
            }
        }
        if !headers.contains_key("accept") {
            request = request.header(
                ACCEPT,
                if is_stream_request(headers, body) {
                    "text/event-stream"
                } else {
                    "application/json"
                },
            );
        }
        request = request.header("Connection", "Keep-Alive");
        if !headers.contains_key("content-type") && !body.is_empty() {
            request = request.header(CONTENT_TYPE, "application/json");
        }
        if !body.is_empty() {
            request = request.body(body.to_vec());
        }

        match request.send().await {
            Ok(response) => return Ok(response),
            Err(error) => {
                let should_retry = retry_attempt < UPSTREAM_SEND_RETRY_ATTEMPTS
                    && should_retry_upstream_send_error(&error);
                if !should_retry {
                    return Err(format!("请求 Codex 上游失败: {}", error));
                }
                tokio::time::sleep(upstream_send_retry_delay(retry_attempt + 1)).await;
            }
        }
    }

    Err("请求 Codex 上游失败: 未知错误".to_string())
}

async fn proxy_request_with_account_pool(
    request: &ParsedRequest,
    collection: &CodexLocalAccessCollection,
) -> Result<ProxyDispatchSuccess, ProxyDispatchError> {
    let mut routing_account_ids = build_routing_pool_account_ids(collection);

    let upstream_target =
        resolve_upstream_target(&request.target).map_err(|err| ProxyDispatchError {
            status: 400,
            message: err,
            account_id: None,
            account_email: None,
            retry_after: None,
            defer_until_pool_available: false,
        })?;
    let routing_hint = build_request_routing_hint(request);
    let mut health_registry =
        load_health_registry_from_disk().map_err(|err| ProxyDispatchError {
            status: 503,
            message: format!("API 服务健康状态不可用，请手动检查后重试: {}", err),
            account_id: None,
            account_email: None,
            retry_after: None,
            defer_until_pool_available: false,
        })?;
    let now = now_ms();
    let sticky_pruned = prune_process_sticky_binding(
        &mut health_registry,
        &routing_account_ids,
        Some(&routing_hint.model_key),
        now,
    );
    let request_affinity_pruned =
        prune_persisted_request_affinity_bindings(&mut health_registry, None, now);
    if sticky_pruned || request_affinity_pruned {
        if let Err(err) = save_health_registry_to_disk(&health_registry) {
            log_health_registry_update_error(&err);
        }
    }
    let active_stream_affinity_account_id = active_stream_affinity_account_for_request(request);
    let response_affinity_account_id = match routing_hint.previous_response_id.as_deref() {
        Some(previous_response_id) => resolve_affinity_account(previous_response_id)
            .await
            .or_else(|| {
                infer_single_account_continuation_affinity(
                    Some(previous_response_id),
                    &routing_account_ids,
                )
            }),
        None => None,
    };
    let request_affinity_account_id = resolve_request_affinity_account_from_runtime(request)
        .await
        .or_else(|| request_affinity_account_from_registry(&health_registry, request, now));
    let hard_affinity_account_id = active_stream_affinity_account_id
        .clone()
        .or(response_affinity_account_id.clone())
        .or(request_affinity_account_id.clone());
    let affinity_account_id = hard_affinity_account_id.clone();
    let hard_affinity_source = if active_stream_affinity_account_id.is_some() {
        "active_stream"
    } else if response_affinity_account_id.is_some() {
        "previous_response_id"
    } else if request_affinity_account_id.is_some() {
        "codex_turn_state"
    } else {
        "none"
    };
    let request_affinity_mode = if request_affinity_account_id.is_some() {
        if active_stream_affinity_account_id.is_some() {
            "active_stream_hard"
        } else {
            "turn_state_hard"
        }
    } else {
        "absent"
    };
    if let Some(account_id) = affinity_account_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        if !routing_account_ids
            .iter()
            .any(|candidate| candidate == account_id)
            && !health_registry_account_has_hard_block(&health_registry, account_id)
        {
            routing_account_ids.push(account_id.to_string());
        }
    }

    if routing_account_ids.is_empty() {
        return Err(ProxyDispatchError {
            status: 503,
            message: "本地接入集合暂无账号".to_string(),
            account_id: None,
            account_email: None,
            retry_after: None,
            defer_until_pool_available: false,
        });
    }

    let total = routing_account_ids.len();
    let max_credential_attempts = total
        .min(retry_failover_account_attempt_limit(
            &collection.safety_config,
        ))
        .max(1);
    let max_retry_attempts = retry_failover_max_retries(&collection.safety_config);
    let mut last_status = 503u16;
    let mut last_error = "本地接入集合暂无可用账号".to_string();
    let mut last_account_id: Option<String> = None;
    let mut last_account_email: Option<String> = None;
    let mut attempts = 0usize;
    let mut retry_round = 0usize;
    let mut earliest_cooldown_wait: Option<Duration>;

    loop {
        let start = next_routing_start_index(collection);
        let ordered_account_ids =
            build_ordered_account_ids(&routing_account_ids, start, affinity_account_id.as_deref());
        let mut strategy_account_ids =
            apply_collection_routing_strategy(&ordered_account_ids, collection);
        let selector_now = now_ms();
        sort_account_ids_by_health_estimate(
            &mut strategy_account_ids,
            &health_registry,
            selector_now,
        );
        let process_sticky_account_id = process_sticky_account_id(
            &health_registry,
            &strategy_account_ids,
            Some(&routing_hint.model_key),
            selector_now,
        );
        let strategy_account_ids =
            pin_account_to_front(strategy_account_ids, process_sticky_account_id.as_deref());
        let strategy_account_ids =
            pin_account_to_front(strategy_account_ids, affinity_account_id.as_deref());
        let strategy_account_ids = constrain_previous_response_affinity(
            strategy_account_ids,
            hard_affinity_account_id.as_deref(),
        );
        let selector_audit_summary = build_selector_audit_summary(
            &strategy_account_ids,
            &health_registry,
            Some(&routing_hint.model_key),
            selector_now,
            max_credential_attempts,
            affinity_account_id.as_deref(),
            sticky_pruned,
        );
        let mut attempted_in_round = false;
        let mut round_cooldown_wait: Option<Duration> = None;

        for account_id in strategy_account_ids {
            if attempts >= max_credential_attempts {
                break;
            }

            let now = now_ms();
            let is_hard_affinity_continuation =
                hard_affinity_account_id.as_deref() == Some(account_id.as_str());
            let has_hard_block =
                health_registry_account_has_hard_block(&health_registry, &account_id);
            // A bound continuation may still be accepted upstream even while
            // new admissions for the same account/model are cooling down.
            if let Some(wait) = health_registry_account_cooldown_wait(
                &health_registry,
                &account_id,
                Some(&routing_hint.model_key),
                now,
            ) {
                if !is_hard_affinity_continuation || has_hard_block {
                    round_cooldown_wait = min_cooldown_wait(round_cooldown_wait, wait);
                    continue;
                }
            }

            if (!is_hard_affinity_continuation || has_hard_block)
                && !health_registry_account_is_schedulable(
                    &health_registry,
                    &account_id,
                    Some(&routing_hint.model_key),
                    now,
                )
            {
                continue;
            }

            if let Some(wait) = get_model_cooldown_wait(&account_id, &routing_hint.model_key).await
            {
                if !is_hard_affinity_continuation || has_hard_block {
                    round_cooldown_wait = Some(match round_cooldown_wait {
                        Some(current) if current <= wait => current,
                        _ => wait,
                    });
                    continue;
                }
            }

            attempted_in_round = true;
            attempts += 1;

            let mut account = match get_prepared_account(&account_id).await {
                Ok(account) => {
                    let context = build_audit_context(request, Some(account_id.as_str()));
                    record_audit_event_from_context(
                        &context,
                        "auth_projection",
                        None,
                        None,
                        None,
                        Some("prepared"),
                        BTreeMap::from([("source".to_string(), "prepared_account".to_string())]),
                    );
                    account
                }
                Err(err) => {
                    let context = build_audit_context(request, Some(account_id.as_str()));
                    record_audit_event_from_context(
                        &context,
                        "auth_projection",
                        None,
                        Some("auth_projection_error"),
                        None,
                        Some("prepare_failed"),
                        BTreeMap::from([(
                            "reason".to_string(),
                            "prepare_account_failed".to_string(),
                        )]),
                    );
                    invalidate_prepared_account(&account_id).await;
                    log_codex_api_failure(
                        None,
                        Some(request),
                        None,
                        Some(account_id.as_str()),
                        None,
                        None,
                        format!("账号预处理失败: {}", err).as_str(),
                    );
                    last_error = err;
                    continue;
                }
            };

            if collection.restrict_free_accounts && is_free_plan_type(account.plan_type.as_deref())
            {
                log_codex_api_failure(
                    None,
                    Some(request),
                    None,
                    Some(account.id.as_str()),
                    Some(account.email.as_str()),
                    None,
                    "Free 账号不支持加入本地接入",
                );
                last_error = "Free 账号不支持加入本地接入".to_string();
                continue;
            }

            last_account_id = Some(account.id.clone());
            last_account_email = Some(account.email.clone());

            let mut single_account_status_retry_attempt = 0usize;
            loop {
                let admission_context = build_audit_context(request, Some(account.id.as_str()));
                record_audit_event_from_context(
                    &admission_context,
                    "admission_attempt",
                    None,
                    None,
                    None,
                    Some("started"),
                    BTreeMap::from([
                        ("attempt".to_string(), attempts.to_string()),
                        ("model_key".to_string(), routing_hint.model_key.clone()),
                    ]),
                );
                record_audit_event_from_context(
                    &admission_context,
                    "upstream_forward",
                    None,
                    None,
                    None,
                    Some("send_started"),
                    BTreeMap::from([("model_key".to_string(), routing_hint.model_key.clone())]),
                );
                let first_response = send_upstream_request(
                    &request.method,
                    &upstream_target,
                    &request.headers,
                    &request.body,
                    &account,
                )
                .await;

                let mut response = match first_response {
                    Ok(response) => {
                        record_audit_event_from_context(
                            &admission_context,
                            "upstream_forward",
                            Some(response.status().as_u16()),
                            None,
                            None,
                            Some("response_received"),
                            BTreeMap::from([(
                                "model_key".to_string(),
                                routing_hint.model_key.clone(),
                            )]),
                        );
                        response
                    }
                    Err(err) => {
                        record_audit_event_from_context(
                            &admission_context,
                            "upstream_forward",
                            None,
                            Some("network_error"),
                            None,
                            Some("send_failed"),
                            BTreeMap::from([("reason".to_string(), "send_error".to_string())]),
                        );
                        log_codex_api_failure(
                            None,
                            Some(request),
                            None,
                            Some(account.id.as_str()),
                            Some(account.email.as_str()),
                            None,
                            format!("上游请求失败: {}", err).as_str(),
                        );
                        last_error = err;
                        break;
                    }
                };

                if response.status() == StatusCode::UNAUTHORIZED {
                    match force_refresh_gateway_account(&account_id).await {
                        Ok(refreshed_account) => {
                            account = refreshed_account;
                            let context = build_audit_context(request, Some(account.id.as_str()));
                            record_audit_event_from_context(
                                &context,
                                "auth_projection",
                                Some(StatusCode::UNAUTHORIZED.as_u16()),
                                None,
                                None,
                                Some("refreshed"),
                                BTreeMap::from([(
                                    "source".to_string(),
                                    "force_refresh".to_string(),
                                )]),
                            );
                            record_audit_event_from_context(
                                &context,
                                "upstream_forward",
                                None,
                                None,
                                None,
                                Some("send_started_after_refresh"),
                                BTreeMap::from([(
                                    "model_key".to_string(),
                                    routing_hint.model_key.clone(),
                                )]),
                            );
                            response = match send_upstream_request(
                                &request.method,
                                &upstream_target,
                                &request.headers,
                                &request.body,
                                &account,
                            )
                            .await
                            {
                                Ok(response) => {
                                    record_audit_event_from_context(
                                        &context,
                                        "upstream_forward",
                                        Some(response.status().as_u16()),
                                        None,
                                        None,
                                        Some("response_received_after_refresh"),
                                        BTreeMap::from([(
                                            "model_key".to_string(),
                                            routing_hint.model_key.clone(),
                                        )]),
                                    );
                                    response
                                }
                                Err(err) => {
                                    record_audit_event_from_context(
                                        &context,
                                        "upstream_forward",
                                        None,
                                        Some("network_error"),
                                        None,
                                        Some("send_failed_after_refresh"),
                                        BTreeMap::from([(
                                            "reason".to_string(),
                                            "send_error".to_string(),
                                        )]),
                                    );
                                    log_codex_api_failure(
                                        None,
                                        Some(request),
                                        None,
                                        Some(account.id.as_str()),
                                        Some(account.email.as_str()),
                                        None,
                                        format!("刷新后重试上游失败: {}", err).as_str(),
                                    );
                                    last_error = err;
                                    break;
                                }
                            };

                            if response.status() == StatusCode::UNAUTHORIZED {
                                let auth_failure = classify_codex_upstream_error(
                                    StatusCode::UNAUTHORIZED,
                                    None,
                                    "",
                                );
                                persist_health_registry_with_audit(
                                    &account.id,
                                    Some(&routing_hint.model_key),
                                    request,
                                    &auth_failure,
                                );
                                invalidate_prepared_account(&account_id).await;
                                log_codex_api_failure(
                                    None,
                                    Some(request),
                                    Some(StatusCode::UNAUTHORIZED.as_u16()),
                                    Some(account.id.as_str()),
                                    Some(account.email.as_str()),
                                    None,
                                    auth_failure.safe_message.as_str(),
                                );
                                return Err(ProxyDispatchError {
                                    status: StatusCode::UNAUTHORIZED.as_u16(),
                                    message: auth_failure.safe_message,
                                    account_id: Some(account.id.clone()),
                                    account_email: Some(account.email.clone()),
                                    retry_after: None,
                                    defer_until_pool_available: false,
                                });
                            }
                        }
                        Err(err) => {
                            invalidate_prepared_account(&account_id).await;
                            let auth_failure =
                                classify_codex_upstream_error(StatusCode::UNAUTHORIZED, None, "");
                            persist_health_registry_with_audit(
                                &account.id,
                                Some(&routing_hint.model_key),
                                request,
                                &auth_failure,
                            );
                            log_codex_api_failure(
                                None,
                                Some(request),
                                Some(StatusCode::UNAUTHORIZED.as_u16()),
                                Some(account.id.as_str()),
                                Some(account.email.as_str()),
                                None,
                                format!("账号刷新失败: {}", err).as_str(),
                            );
                            return Err(ProxyDispatchError {
                                status: StatusCode::UNAUTHORIZED.as_u16(),
                                message: auth_failure.safe_message,
                                account_id: Some(account.id.clone()),
                                account_email: Some(account.email.clone()),
                                retry_after: None,
                                defer_until_pool_available: false,
                            });
                        }
                    }
                }

                if response.status().is_success() {
                    clear_model_cooldown(&account.id, &routing_hint.model_key).await;
                    bind_request_affinity(request, &account.id).await;
                    persist_successful_routing_state(
                        &account.id,
                        request,
                        affinity_account_id.is_none(),
                    );
                    let context = build_audit_context(request, Some(account.id.as_str()));
                    record_audit_event_from_context(
                        &context,
                        "upstream_admitted",
                        Some(response.status().as_u16()),
                        None,
                        None,
                        Some("admitted"),
                        BTreeMap::from([("model_key".to_string(), routing_hint.model_key.clone())]),
                    );
                    let selected_reason = selector_selected_reason(
                        account.id.as_str(),
                        active_stream_affinity_account_id.as_deref(),
                        response_affinity_account_id.as_deref(),
                        request_affinity_account_id.as_deref(),
                        process_sticky_account_id.as_deref(),
                    );
                    let mut selector_detail = selector_audit_detail(
                        &selector_audit_summary,
                        selected_reason,
                        &routing_hint.model_key,
                    );
                    selector_detail.insert(
                        "hard_affinity_bound".to_string(),
                        hard_affinity_account_id.is_some().to_string(),
                    );
                    selector_detail.insert(
                        "active_stream_affinity_present".to_string(),
                        active_stream_affinity_account_id.is_some().to_string(),
                    );
                    selector_detail.insert(
                        "previous_response_affinity_present".to_string(),
                        response_affinity_account_id.is_some().to_string(),
                    );
                    selector_detail.insert(
                        "request_affinity_present".to_string(),
                        request_affinity_account_id.is_some().to_string(),
                    );
                    selector_detail.insert(
                        "request_affinity_mode".to_string(),
                        request_affinity_mode.to_string(),
                    );
                    selector_detail.insert(
                        "hard_affinity_source".to_string(),
                        hard_affinity_source.to_string(),
                    );
                    record_audit_event_from_context(
                        &context,
                        "routing_decision",
                        Some(response.status().as_u16()),
                        None,
                        None,
                        Some("selected"),
                        selector_detail.clone(),
                    );
                    record_audit_event_from_context(
                        &context,
                        "selector",
                        Some(response.status().as_u16()),
                        None,
                        None,
                        Some("selected"),
                        selector_detail,
                    );
                    return Ok(ProxyDispatchSuccess {
                        upstream: response,
                        account_id: account.id.clone(),
                        account_email: account.email.clone(),
                    });
                }

                let status = response.status();
                let upstream_headers = response.headers().clone();
                let body = response.text().await.unwrap_or_default();
                let classified =
                    classify_codex_upstream_error(status, Some(&upstream_headers), &body);
                let request_id = health_registry_request_id_from_request(request);
                update_health_registry_from_classified_error(
                    &mut health_registry,
                    &account.id,
                    Some(&routing_hint.model_key),
                    request_id.as_deref(),
                    &classified,
                    now_ms(),
                );
                persist_account_quota_exhaustion_with_audit(
                    &account.id,
                    request,
                    &classified,
                    &body,
                );
                persist_health_registry_with_audit(
                    &account.id,
                    Some(&routing_hint.model_key),
                    request,
                    &classified,
                );
                let message = classified.safe_message.clone();
                log_codex_api_failure(
                    None,
                    Some(request),
                    Some(status.as_u16()),
                    Some(account.id.as_str()),
                    Some(account.email.as_str()),
                    None,
                    format!(
                        "上游返回失败: {} error_type={}",
                        message,
                        classified.error_type.as_str()
                    )
                    .as_str(),
                );

                if let Some(retry_after) = classified.retry_after {
                    set_model_cooldown(&account.id, &routing_hint.model_key, retry_after).await;
                    round_cooldown_wait = Some(match round_cooldown_wait {
                        Some(current) if current <= retry_after => current,
                        _ => retry_after,
                    });
                }

                let can_retry_single_account = total == 1
                    && single_account_status_retry_attempt < max_retry_attempts
                    && should_retry_single_account_upstream_status(status);
                if can_retry_single_account {
                    single_account_status_retry_attempt += 1;
                    tokio::time::sleep(single_account_status_retry_delay(
                        single_account_status_retry_attempt,
                    ))
                    .await;
                    continue;
                }

                if classified.safe_for_request_failover() {
                    let context = build_audit_context(request, Some(account.id.as_str()));
                    let mut detail = classified_audit_detail(&classified);
                    detail.insert("attempt".to_string(), attempts.to_string());
                    detail.insert(
                        "max_credential_attempts".to_string(),
                        max_credential_attempts.to_string(),
                    );
                    let hard_affinity_bound = hard_affinity_account_id.is_some();
                    let hard_affinity_active =
                        hard_affinity_account_id.as_deref() == Some(account.id.as_str());
                    let hard_affinity_retry_wait_limit =
                        hard_affinity_inline_retry_wait_limit(&collection.safety_config);
                    detail.insert(
                        "hard_affinity_bound".to_string(),
                        hard_affinity_bound.to_string(),
                    );
                    detail.insert(
                        "hard_affinity_source".to_string(),
                        hard_affinity_source.to_string(),
                    );
                    detail.insert(
                        "request_affinity_mode".to_string(),
                        request_affinity_mode.to_string(),
                    );
                    if hard_affinity_bound {
                        detail.insert(
                            "max_hard_affinity_inline_retry_wait_ms".to_string(),
                            hard_affinity_retry_wait_limit.as_millis().to_string(),
                        );
                        detail.insert(
                            "hard_affinity_inline_retry_wait_limit_ms".to_string(),
                            hard_affinity_retry_wait_limit.as_millis().to_string(),
                        );
                    }
                    let can_try_next_account =
                        !hard_affinity_bound && attempts < max_credential_attempts;
                    record_audit_event_from_context(
                        &context,
                        if can_try_next_account {
                            "fallback_selected"
                        } else {
                            "fallback_blocked"
                        },
                        Some(status.as_u16()),
                        Some(classified.error_type.as_str()),
                        None,
                        Some(if can_try_next_account {
                            "next_account"
                        } else if hard_affinity_bound {
                            "hard_affinity"
                        } else {
                            "attempt_limit"
                        }),
                        detail,
                    );
                    if let Some(wait) = should_retry_hard_affinity_upstream_failure(
                        hard_affinity_active,
                        &classified,
                        single_account_status_retry_attempt,
                        max_retry_attempts,
                        hard_affinity_retry_wait_limit,
                    ) {
                        single_account_status_retry_attempt += 1;
                        record_hard_affinity_retry_wait_audit(
                            request,
                            account.id.as_str(),
                            status,
                            &classified,
                            wait,
                            hard_affinity_retry_wait_limit,
                            "sleeping",
                        );
                        tokio::time::sleep(wait).await;
                        record_hard_affinity_retry_wait_audit(
                            request,
                            account.id.as_str(),
                            status,
                            &classified,
                            wait,
                            hard_affinity_retry_wait_limit,
                            "retrying",
                        );
                        continue;
                    }
                    if hard_affinity_bound {
                        return Err(ProxyDispatchError {
                            status: status.as_u16(),
                            message,
                            account_id: Some(account.id.clone()),
                            account_email: Some(account.email.clone()),
                            retry_after: classified.retry_after,
                            defer_until_pool_available: false,
                        });
                    }
                    last_status = status.as_u16();
                    last_error = if can_try_next_account {
                        format!("账号 {} 当前不可用，已尝试轮转: {}", account.email, message)
                    } else {
                        format!(
                            "账号 {} 当前不可用，已达到本次请求切号上限: {}",
                            account.email, message
                        )
                    };
                    break;
                }

                return Err(ProxyDispatchError {
                    status: status.as_u16(),
                    message,
                    account_id: Some(account.id.clone()),
                    account_email: Some(account.email.clone()),
                    retry_after: None,
                    defer_until_pool_available: false,
                });
            }
        }

        earliest_cooldown_wait = round_cooldown_wait;
        let Some(wait) = earliest_cooldown_wait else {
            break;
        };
        if !attempted_in_round {
            let pool_summary = summarize_pool_unavailability(
                &health_registry,
                &routing_account_ids,
                Some(&routing_hint.model_key),
                now_ms(),
            );
            let use_pool_unavailable_summary = should_use_pool_unavailable_summary(&pool_summary);
            return Err(ProxyDispatchError {
                status: if use_pool_unavailable_summary {
                    status_for_pool_unavailable(&pool_summary)
                } else {
                    StatusCode::SERVICE_UNAVAILABLE.as_u16()
                },
                message: if use_pool_unavailable_summary {
                    build_pool_unavailable_message(&routing_hint.model_key, &pool_summary)
                } else {
                    build_cooldown_unavailable_message(&routing_hint.model_key, wait)
                },
                account_id: affinity_account_id.clone(),
                account_email: None,
                retry_after: Some(wait),
                defer_until_pool_available: true,
            });
        }
        if attempts >= max_credential_attempts
            || retry_round >= max_retry_attempts
            || wait > MAX_INLINE_ACCOUNT_RETRY_WAIT
        {
            break;
        }

        tokio::time::sleep(wait).await;
        retry_round += 1;
    }

    let pool_summary = summarize_pool_unavailability(
        &health_registry,
        &routing_account_ids,
        Some(&routing_hint.model_key),
        now_ms(),
    );
    let use_pool_unavailable_summary = should_use_pool_unavailable_summary(&pool_summary);
    let pool_retry_after = if use_pool_unavailable_summary {
        pool_summary.nearest_wait
    } else {
        None
    };

    Err(ProxyDispatchError {
        status: if use_pool_unavailable_summary {
            status_for_pool_unavailable(&pool_summary)
        } else if last_status == 503 {
            earliest_cooldown_wait
                .map(|_| StatusCode::TOO_MANY_REQUESTS.as_u16())
                .unwrap_or(last_status)
        } else {
            last_status
        },
        message: if use_pool_unavailable_summary {
            build_pool_unavailable_message(&routing_hint.model_key, &pool_summary)
        } else if matches!(last_status, 429 | 503) {
            earliest_cooldown_wait
                .map(|wait| build_cooldown_unavailable_message(&routing_hint.model_key, wait))
                .unwrap_or(last_error)
        } else {
            last_error
        },
        account_id: last_account_id,
        account_email: last_account_email,
        retry_after: earliest_cooldown_wait.or(pool_retry_after),
        defer_until_pool_available: use_pool_unavailable_summary
            && should_defer_pool_unavailable(&pool_summary),
    })
}

async fn pool_unavailable_from_snapshot(
    request: &ParsedRequest,
    collection: &CodexLocalAccessCollection,
) -> Option<ProxyDispatchError> {
    let routing_account_ids = build_routing_pool_account_ids(collection);
    if routing_account_ids.is_empty() {
        return None;
    }

    // Continuation turns must reach the affinity router; pre-admission pool
    // snapshots are only allowed to reject genuinely new Responses requests.
    if build_request_routing_hint(request)
        .previous_response_id
        .is_some()
        || resolve_request_affinity_account(request).await.is_some()
    {
        return None;
    }

    let routing_hint = build_request_routing_hint(request);
    let health_registry = load_health_registry_from_disk().ok()?;
    let pool_summary = summarize_pool_unavailability(
        &health_registry,
        &routing_account_ids,
        Some(&routing_hint.model_key),
        now_ms(),
    );
    if !should_use_pool_unavailable_summary(&pool_summary) {
        return None;
    }
    let defer_until_pool_available = should_defer_pool_unavailable(&pool_summary);

    Some(ProxyDispatchError {
        status: status_for_pool_unavailable(&pool_summary),
        message: build_pool_unavailable_message(&routing_hint.model_key, &pool_summary),
        account_id: None,
        account_email: None,
        retry_after: pool_summary.nearest_wait,
        defer_until_pool_available,
    })
}

async fn deferred_pool_unavailable_from_snapshot(
    request: &ParsedRequest,
    collection: &CodexLocalAccessCollection,
) -> Option<ProxyDispatchError> {
    pool_unavailable_from_snapshot(request, collection)
        .await
        .filter(|error| error.defer_until_pool_available && error.retry_after.is_some())
}

fn record_hard_affinity_retry_wait_audit(
    request: &ParsedRequest,
    account_id: &str,
    status: StatusCode,
    classified: &ClassifiedCodexUpstreamError,
    wait: Duration,
    retry_wait_limit: Duration,
    outcome: &str,
) {
    let context = build_audit_context(request, Some(account_id));
    record_audit_event_from_context(
        &context,
        "pool_wait",
        Some(status.as_u16()),
        Some(classified.error_type.as_str()),
        None,
        Some(outcome),
        BTreeMap::from([
            (
                "reason".to_string(),
                "hard_affinity_same_account_retry".to_string(),
            ),
            ("retry_after_ms".to_string(), wait.as_millis().to_string()),
            (
                "max_inline_wait_ms".to_string(),
                retry_wait_limit.as_millis().to_string(),
            ),
            (
                "inline_wait_limit_ms".to_string(),
                retry_wait_limit.as_millis().to_string(),
            ),
            (
                "message".to_string(),
                safe_log_field(Some(classified.safe_message.as_str()), 512),
            ),
        ]),
    );
}

fn record_pool_wait_audit(
    request: &ParsedRequest,
    error: &ProxyDispatchError,
    wait: Duration,
    remaining_timeout: Duration,
    outcome: &str,
) {
    let context = build_audit_context(request, error.account_id.as_deref());
    let error_type = classify_codex_api_failure(Some(error.status), error.message.as_str());
    record_audit_event_from_context(
        &context,
        "pool_wait",
        Some(error.status),
        Some(error_type),
        None,
        Some(outcome),
        BTreeMap::from([
            ("retry_after_ms".to_string(), wait.as_millis().to_string()),
            (
                "remaining_timeout_ms".to_string(),
                remaining_timeout.as_millis().to_string(),
            ),
            (
                "message".to_string(),
                safe_log_field(Some(error.message.as_str()), 512),
            ),
        ]),
    );
}

fn is_pool_unavailable_dispatch_error(error: &ProxyDispatchError) -> bool {
    classify_codex_api_failure(Some(error.status), error.message.as_str()) == "pool_unavailable"
}

fn request_has_codex_sticky_routing_boundary(request: &ParsedRequest) -> bool {
    official_codex_sticky_routing_boundary(request).is_some()
}

async fn request_has_resolved_hard_affinity(request: &ParsedRequest) -> bool {
    if active_stream_affinity_account_for_request(request).is_some() {
        return true;
    }

    let routing_hint = build_request_routing_hint(request);
    if let Some(previous_response_id) = routing_hint.previous_response_id.as_deref() {
        return resolve_affinity_account(previous_response_id)
            .await
            .is_some();
    }

    resolve_request_affinity_account(request).await.is_some()
}

fn should_write_in_band_pool_unavailable_response(
    request: &ParsedRequest,
    response_adapter: &GatewayResponseAdapter,
    error: &ProxyDispatchError,
) -> bool {
    // Only independent /v1/responses requests may be closed with a local
    // completed Responses payload. Official Codex sticky boundaries must keep
    // the transport/error contract so a continuation is not mistaken for an
    // upstream-completed turn.
    is_pool_unavailable_dispatch_error(error)
        && is_responses_request(&request.target)
        && matches!(response_adapter, GatewayResponseAdapter::Passthrough { .. })
        && !request_has_codex_sticky_routing_boundary(request)
}

fn build_synthetic_pool_unavailable_text(error: &ProxyDispatchError) -> String {
    format!(
        "Cockpit API Service pool_unavailable: {}\nrecover_action=retry_after_cooldown_or_start_new_task",
        error.message.trim()
    )
}

fn build_synthetic_pool_unavailable_responses_payload(
    request: &ParsedRequest,
    error: &ProxyDispatchError,
    include_visible_text: bool,
) -> Value {
    let routing_hint = build_request_routing_hint(request);
    let model = routing_hint
        .model_key
        .trim()
        .is_empty()
        .then(|| "unknown".to_string())
        .unwrap_or(routing_hint.model_key);
    let created_at = chrono::Utc::now().timestamp();
    let response_id = format!("resp_cockpit_pool_unavailable_{}", now_ms());
    let message_id = format!("msg_cockpit_pool_unavailable_{}", now_ms());
    let text = if include_visible_text {
        build_synthetic_pool_unavailable_text(error)
    } else {
        String::new()
    };

    json!({
        "id": response_id,
        "object": "response",
        "created_at": created_at,
        "completed_at": created_at,
        "status": "completed",
        "model": model,
        "output": [{
            "id": message_id,
            "type": "message",
            "status": "completed",
            "role": "assistant",
            "content": [{
                "type": "output_text",
                "text": text,
                "annotations": []
            }]
        }],
        "error": null,
        "incomplete_details": null,
        "parallel_tool_calls": true,
        "previous_response_id": null,
        "reasoning": {
            "effort": null,
            "summary": null
        },
        "store": false,
        "text": {
            "format": {
                "type": "text"
            }
        },
        "tool_choice": "auto",
        "tools": [],
        "truncation": "disabled",
        "usage": {
            "input_tokens": 0,
            "input_tokens_details": {
                "cached_tokens": 0
            },
            "output_tokens": 0,
            "output_tokens_details": {
                "reasoning_tokens": 0
            },
            "total_tokens": 0
        },
        "user": null,
        "metadata": {
            "cockpit_local_closure": "pool_unavailable",
            "cockpit_completion_mode": "synthetic_pool_unavailable_notice",
            "cockpit_completion_contract": "openai_codex_response_completed_with_visible_assistant_message",
            "cockpit_recover_action": "retry_after_cooldown_or_start_new_task"
        }
    })
}

fn build_in_progress_pool_unavailable_response(payload: &Value) -> Value {
    let mut response = payload.clone();
    if let Some(object) = response.as_object_mut() {
        object.insert("status".to_string(), json!("in_progress"));
        object.insert("completed_at".to_string(), Value::Null);
        object.insert("output".to_string(), json!([]));
        object.insert("usage".to_string(), Value::Null);
    }
    response
}

fn build_completed_pool_unavailable_sse(payload: &Value) -> Vec<u8> {
    let mut stream_body = String::new();
    let response = payload.clone();
    let in_progress_response = build_in_progress_pool_unavailable_response(payload);
    let output_item = payload
        .get("output")
        .and_then(Value::as_array)
        .and_then(|output| output.first())
        .cloned()
        .unwrap_or_else(|| {
            json!({
                "id": format!("msg_cockpit_pool_unavailable_{}", now_ms()),
                "type": "message",
                "status": "completed",
                "role": "assistant",
                "content": []
            })
        });
    let item_id = output_item
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("msg_cockpit_pool_unavailable");
    let text = output_item
        .get("content")
        .and_then(Value::as_array)
        .and_then(|content| content.first())
        .and_then(|part| part.get("text"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let in_progress_item = json!({
        "id": item_id,
        "type": "message",
        "status": "in_progress",
        "role": "assistant",
        "content": []
    });
    push_named_sse_payload(
        &mut stream_body,
        "response.created",
        json!({
            "type": "response.created",
            "response": in_progress_response
        }),
    );
    push_named_sse_payload(
        &mut stream_body,
        "response.in_progress",
        json!({
            "type": "response.in_progress",
            "response": build_in_progress_pool_unavailable_response(payload)
        }),
    );
    push_named_sse_payload(
        &mut stream_body,
        "response.output_item.added",
        json!({
            "type": "response.output_item.added",
            "output_index": 0,
            "item": in_progress_item
        }),
    );
    push_named_sse_payload(
        &mut stream_body,
        "response.content_part.added",
        json!({
            "type": "response.content_part.added",
            "item_id": item_id,
            "output_index": 0,
            "content_index": 0,
            "part": {
                "type": "output_text",
                "text": "",
                "annotations": []
            }
        }),
    );
    push_named_sse_payload(
        &mut stream_body,
        "response.output_text.delta",
        json!({
            "type": "response.output_text.delta",
            "item_id": item_id,
            "output_index": 0,
            "content_index": 0,
            "delta": text
        }),
    );
    push_named_sse_payload(
        &mut stream_body,
        "response.output_text.done",
        json!({
            "type": "response.output_text.done",
            "item_id": item_id,
            "output_index": 0,
            "content_index": 0,
            "text": text
        }),
    );
    push_named_sse_payload(
        &mut stream_body,
        "response.content_part.done",
        json!({
            "type": "response.content_part.done",
            "item_id": item_id,
            "output_index": 0,
            "content_index": 0,
            "part": {
                "type": "output_text",
                "text": text,
                "annotations": []
            }
        }),
    );
    push_named_sse_payload(
        &mut stream_body,
        "response.output_item.done",
        json!({
            "type": "response.output_item.done",
            "output_index": 0,
            "item": output_item
        }),
    );
    push_named_sse_payload(
        &mut stream_body,
        "response.completed",
        json!({
            "type": "response.completed",
            "response": response
        }),
    );
    stream_body.push_str("data: [DONE]\n\n");
    stream_body.into_bytes()
}

fn build_responses_upstream_stream_error_sse(message: &str) -> Vec<u8> {
    let created_at = chrono::Utc::now().timestamp();
    let response_id = format!("resp_cockpit_upstream_stream_error_{}", now_ms());
    let safe_message = safe_log_field(Some(message), 512);
    let mut stream_body = String::from("\n\n");
    push_named_sse_payload(
        &mut stream_body,
        "response.failed",
        json!({
            "type": "response.failed",
            "response": {
                "id": response_id,
                "object": "response",
                "created_at": created_at,
                "status": "failed",
                "model": "unknown",
                "output": [],
                "error": {
                    "type": "server_error",
                    "code": "cockpit_upstream_stream_error",
                    "message": safe_message
                },
                "incomplete_details": null,
                "usage": null,
                "metadata": {
                    "cockpit_terminal_origin": "upstream_stream_error",
                    "cockpit_completion_contract": "openai_codex_response_failed_with_done"
                }
            }
        }),
    );
    stream_body.push_str("data: [DONE]\n\n");
    stream_body.into_bytes()
}

async fn write_in_band_pool_unavailable_response(
    stream: &mut TcpStream,
    request: &ParsedRequest,
    response_adapter: &GatewayResponseAdapter,
    error: &ProxyDispatchError,
    latency_ms: u64,
) -> Result<(), String> {
    let is_stream_response = matches!(
        response_adapter,
        GatewayResponseAdapter::Passthrough {
            request_is_stream: true
        }
    );
    let payload = build_synthetic_pool_unavailable_responses_payload(request, error, true);
    let response_id = extract_response_id(&payload);
    let context = build_audit_context(request, error.account_id.as_deref());
    let mut detail = BTreeMap::from([
        ("latency_ms".to_string(), latency_ms.to_string()),
        ("transport_status".to_string(), "200".to_string()),
        ("original_status".to_string(), error.status.to_string()),
        (
            "codex_facing_terminal_contract".to_string(),
            if is_stream_response {
                "response.completed_local_pool_unavailable".to_string()
            } else {
                "json_completed_local_pool_unavailable".to_string()
            },
        ),
        (
            "recover_action".to_string(),
            "retry_after_cooldown_or_start_new_task".to_string(),
        ),
        (
            "terminal_origin".to_string(),
            "local_pool_unavailable".to_string(),
        ),
        (
            "message".to_string(),
            safe_log_field(Some(error.message.as_str()), 512),
        ),
    ]);
    if let Some(retry_after) = error.retry_after {
        detail.insert(
            "retry_after_ms".to_string(),
            retry_after.as_millis().to_string(),
        );
    }
    if let Some(response_id_hash) = response_id
        .as_deref()
        .and_then(|value| hashed_request_correlation_id("response", value))
    {
        detail.insert("upstream_response_id_hash".to_string(), response_id_hash);
    }
    record_audit_event_from_context(
        &context,
        "final_response",
        Some(StatusCode::OK.as_u16()),
        Some("pool_unavailable"),
        Some(if is_stream_response {
            "completed"
        } else {
            "json_completed"
        }),
        Some(if is_stream_response {
            "in_band_local_completion"
        } else {
            "in_band_json_local_completion"
        }),
        detail,
    );

    match response_adapter {
        GatewayResponseAdapter::Passthrough {
            request_is_stream: true,
        } => {
            write_chunked_response_headers(
                stream,
                StatusCode::OK,
                "OK",
                "text/event-stream; charset=utf-8",
                &HeaderMap::new(),
                &[],
            )
            .await?;
            write_chunked_response_chunk(stream, &build_completed_pool_unavailable_sse(&payload))
                .await?;
            finish_chunked_response(stream).await?;
        }
        GatewayResponseAdapter::Passthrough {
            request_is_stream: false,
        } => {
            let payload_bytes = serde_json::to_vec(&payload)
                .map_err(|e| format!("序列化 synthetic pool_unavailable 响应失败: {}", e))?;
            write_http_response(
                stream,
                StatusCode::OK.as_u16(),
                "OK",
                "application/json; charset=utf-8",
                &payload_bytes,
            )
            .await?;
        }
        _ => {
            return Err("in-band pool_unavailable 仅支持 responses passthrough 请求".to_string());
        }
    }

    if let Err(err) = record_request_stats(None, None, false, latency_ms, None).await {
        logger::log_codex_api_warn(&format!("[CodexLocalAccess] 写入失败统计失败: {}", err));
    }

    Ok(())
}

async fn record_successful_proxy_stats(
    account_id: &str,
    account_email: &str,
    started_at: Instant,
    response_capture: &ResponseCapture,
) {
    let latency_ms = started_at.elapsed().as_millis() as u64;
    if let Err(err) = record_request_stats(
        Some(account_id),
        Some(account_email),
        true,
        latency_ms,
        response_capture.usage.clone(),
    )
    .await
    {
        logger::log_codex_api_warn(&format!("[CodexLocalAccess] 写入请求统计失败: {}", err));
    }
}

async fn write_proxy_dispatch_error_response(
    stream: &mut TcpStream,
    addr: &std::net::SocketAddr,
    request: &ParsedRequest,
    error: ProxyDispatchError,
    latency_ms: u64,
) -> Result<(), String> {
    let ProxyDispatchError {
        status,
        message,
        account_id,
        account_email,
        retry_after,
        defer_until_pool_available: _,
    } = error;
    log_codex_api_failure(
        Some(addr),
        Some(request),
        Some(status),
        account_id.as_deref(),
        account_email.as_deref(),
        Some(latency_ms),
        message.as_str(),
    );
    let context = build_audit_context(request, account_id.as_deref());
    let detail = proxy_dispatch_final_error_detail(
        request,
        status,
        message.as_str(),
        retry_after,
        latency_ms,
    );
    record_audit_event_from_context(
        &context,
        "final_response",
        Some(status),
        Some(proxy_dispatch_final_error_type(
            request,
            status,
            message.as_str(),
        )),
        None,
        Some("error"),
        detail,
    );
    let status_text = match status {
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        405 => "Method Not Allowed",
        429 => "Too Many Requests",
        502 => "Bad Gateway",
        _ => "Service Unavailable",
    };
    let response = json_response_with_retry_after(
        status,
        status_text,
        &build_proxy_dispatch_error_body(request, status, message.as_str(), retry_after),
        retry_after,
    );
    let write_result = stream
        .write_all(&response)
        .await
        .map_err(|e| format!("写入错误响应失败: {}", e));
    if let Err(err) = record_request_stats(
        account_id.as_deref(),
        account_email.as_deref(),
        false,
        latency_ms,
        None,
    )
    .await
    {
        logger::log_codex_api_warn(&format!("[CodexLocalAccess] 写入失败统计失败: {}", err));
    }
    write_result
}

async fn handle_connection(
    mut stream: TcpStream,
    addr: std::net::SocketAddr,
) -> Result<(), String> {
    let raw_request = read_http_request(&mut stream).await?;
    let mut parsed = parse_http_request(&raw_request)?;

    if parsed.method.eq_ignore_ascii_case("OPTIONS") {
        stream
            .write_all(&options_response())
            .await
            .map_err(|e| format!("写入 OPTIONS 响应失败: {}", e))?;
        return Ok(());
    }

    if !parsed.method.eq_ignore_ascii_case("GET") && !parsed.method.eq_ignore_ascii_case("POST") {
        write_json_error_response(
            &mut stream,
            Some(&addr),
            Some(&parsed),
            405,
            "Method Not Allowed",
            "Only GET and POST are allowed",
            None,
            None,
            None,
        )
        .await?;
        return Ok(());
    }

    parsed.target = normalize_proxy_target(&parsed.target)?;
    if !parsed.target.starts_with("/v1/") {
        write_json_error_response(
            &mut stream,
            Some(&addr),
            Some(&parsed),
            404,
            "Not Found",
            "Not Found",
            None,
            None,
            None,
        )
        .await?;
        return Ok(());
    }

    let Some(api_key) = extract_local_api_key(&parsed.headers) else {
        write_json_error_response(
            &mut stream,
            Some(&addr),
            Some(&parsed),
            401,
            "Unauthorized",
            "缺少 Authorization Bearer 或 X-API-Key",
            None,
            None,
            None,
        )
        .await?;
        return Ok(());
    };

    let state = {
        let runtime = gateway_runtime().lock().await;
        build_state_snapshot(&runtime)
    };
    let Some(collection) = state.collection else {
        write_json_error_response(
            &mut stream,
            Some(&addr),
            Some(&parsed),
            503,
            "Service Unavailable",
            "本地接入集合尚未创建",
            None,
            None,
            None,
        )
        .await?;
        return Ok(());
    };

    if !collection.enabled || !state.running {
        write_json_error_response(
            &mut stream,
            Some(&addr),
            Some(&parsed),
            503,
            "Service Unavailable",
            "本地接入服务未启用",
            None,
            None,
            None,
        )
        .await?;
        return Ok(());
    }

    if api_key != collection.api_key {
        write_json_error_response(
            &mut stream,
            Some(&addr),
            Some(&parsed),
            401,
            "Unauthorized",
            "本地访问秘钥无效",
            None,
            None,
            None,
        )
        .await?;
        return Ok(());
    }

    if is_local_models_request(&parsed.target) {
        if collection.account_ids.is_empty() {
            write_json_error_response(
                &mut stream,
                Some(&addr),
                Some(&parsed),
                503,
                "Service Unavailable",
                "本地接入集合暂无账号",
                None,
                None,
                None,
            )
            .await?;
            return Ok(());
        }

        let response = json_response(200, "OK", &build_local_models_response());
        stream
            .write_all(&response)
            .await
            .map_err(|e| format!("写入模型响应失败: {}", e))?;
        return Ok(());
    }

    let client_route = failure_log_route(Some(&parsed));
    let started_at = Instant::now();
    let (prepared_request, response_adapter) = match prepare_gateway_request(parsed) {
        Ok(prepared) => prepared,
        Err(err) => {
            write_json_error_response(
                &mut stream,
                Some(&addr),
                None,
                400,
                "Bad Request",
                err.as_str(),
                None,
                None,
                Some(started_at.elapsed().as_millis() as u64),
            )
            .await?;
            return Ok(());
        }
    };
    let response_adapter_kind = response_adapter.audit_kind();
    let request_audit_context = build_audit_context(&prepared_request, None);
    record_audit_event_from_context(
        &request_audit_context,
        "listener",
        None,
        None,
        None,
        Some("accepted"),
        BTreeMap::from([
            ("method".to_string(), prepared_request.method.clone()),
            ("client_route".to_string(), client_route.clone()),
            (
                "response_adapter".to_string(),
                response_adapter_kind.to_string(),
            ),
        ]),
    );

    if is_responses_websocket_upgrade_request(&prepared_request) {
        let latency_ms = started_at.elapsed().as_millis() as u64;
        record_audit_event_from_context(
            &request_audit_context,
            "websocket_unsupported",
            Some(StatusCode::BAD_REQUEST.as_u16()),
            Some("unsupported_websocket"),
            None,
            Some("fallback_required"),
            BTreeMap::from([("fallback".to_string(), "responses_http_sse".to_string())]),
        );
        write_json_error_response(
            &mut stream,
            Some(&addr),
            Some(&prepared_request),
            StatusCode::BAD_REQUEST.as_u16(),
            "Bad Request",
            "Responses WebSocket is not supported by Cockpit local API service; retry with HTTP/SSE fallback",
            None,
            None,
            Some(latency_ms),
        )
        .await?;
        return Ok(());
    }

    let normal_request_timeout =
        Duration::from_secs(collection.safety_config.request_timeout_seconds.max(1));
    let hard_affinity_wait_limit = hard_affinity_inline_retry_wait_limit(&collection.safety_config);
    let hard_affinity_continuity = request_has_resolved_hard_affinity(&prepared_request).await;
    let request_timeout = if hard_affinity_continuity {
        normal_request_timeout.max(hard_affinity_wait_limit.saturating_add(Duration::from_secs(1)))
    } else {
        normal_request_timeout
    };
    let mut request_trace_detail = BTreeMap::from([
        ("method".to_string(), prepared_request.method.clone()),
        ("client_route".to_string(), client_route.clone()),
        (
            "response_adapter".to_string(),
            response_adapter_kind.to_string(),
        ),
        (
            "body_bytes".to_string(),
            prepared_request.body.len().to_string(),
        ),
        (
            "normal_request_timeout_ms".to_string(),
            normal_request_timeout.as_millis().to_string(),
        ),
        (
            "request_timeout_ms".to_string(),
            request_timeout.as_millis().to_string(),
        ),
        (
            "hard_affinity_continuity".to_string(),
            hard_affinity_continuity.to_string(),
        ),
        (
            "hard_affinity_wait_limit_ms".to_string(),
            hard_affinity_wait_limit.as_millis().to_string(),
        ),
        (
            "timeout_extended".to_string(),
            (request_timeout > normal_request_timeout).to_string(),
        ),
    ]);
    if let Some(boundary) = official_codex_sticky_routing_boundary(&prepared_request) {
        request_trace_detail.insert("sticky_boundary".to_string(), boundary.reason().to_string());
    }
    record_audit_event_from_context(
        &request_audit_context,
        "request_trace",
        None,
        None,
        None,
        Some("prepared"),
        request_trace_detail,
    );
    let dispatch_started_at = Instant::now();
    let dispatch_result = match timeout(request_timeout, async {
        loop {
            if let Some(error) =
                deferred_pool_unavailable_from_snapshot(&prepared_request, &collection).await
            {
                let Some(wait) = error.retry_after else {
                    return Err(error);
                };
                let elapsed = dispatch_started_at.elapsed();
                if !pool_wait_fits_request_budget(wait, elapsed, request_timeout) {
                    return Err(error);
                }
                record_pool_wait_audit(
                    &prepared_request,
                    &error,
                    wait,
                    request_timeout.saturating_sub(elapsed),
                    "sleeping",
                );
                tokio::time::sleep(wait).await;
                record_pool_wait_audit(
                    &prepared_request,
                    &error,
                    wait,
                    request_timeout.saturating_sub(dispatch_started_at.elapsed()),
                    "retrying",
                );
                continue;
            }

            let mut backpressure_permit =
                if let Some(reason) = local_backpressure_bypass_reason(&prepared_request).await {
                    record_audit_event_from_context(
                        &request_audit_context,
                        "local_backpressure",
                        None,
                        None,
                        None,
                        Some("bypassed"),
                        BTreeMap::from([("reason".to_string(), reason.to_string())]),
                    );
                    LocalApiBackpressurePermit { released: true }
                } else {
                    let Some(queue_wait) =
                        backpressure_wait_budget(dispatch_started_at.elapsed(), request_timeout)
                    else {
                        return Err(ProxyDispatchError {
                            status: StatusCode::SERVICE_UNAVAILABLE.as_u16(),
                            message: "本地接入请求排队超出本次请求超时预算，请稍后重试".to_string(),
                            account_id: None,
                            account_email: None,
                            retry_after: Some(Duration::from_secs(1)),
                            defer_until_pool_available: false,
                        });
                    };
                    acquire_local_api_backpressure_with_wait(&collection.safety_config, queue_wait)
                        .await?
                };
            match proxy_request_with_account_pool(&prepared_request, &collection).await {
                Ok(success) => {
                    backpressure_permit.release();
                    return Ok(success);
                }
                Err(error) if error.defer_until_pool_available => {
                    let Some(wait) = error.retry_after else {
                        return Err(error);
                    };
                    let elapsed = dispatch_started_at.elapsed();
                    if !pool_wait_fits_request_budget(wait, elapsed, request_timeout) {
                        return Err(error);
                    }
                    let remaining = request_timeout.saturating_sub(elapsed);
                    record_pool_wait_audit(&prepared_request, &error, wait, remaining, "sleeping");
                    backpressure_permit.release();
                    tokio::time::sleep(wait).await;
                    record_pool_wait_audit(
                        &prepared_request,
                        &error,
                        wait,
                        request_timeout.saturating_sub(dispatch_started_at.elapsed()),
                        "retrying",
                    );
                }
                Err(error) => return Err(error),
            }
        }
    })
    .await
    {
        Ok(result) => result,
        Err(_) => Err(ProxyDispatchError {
            status: 503,
            message: "本地接入请求超时，请稍后重试".to_string(),
            account_id: None,
            account_email: None,
            retry_after: Some(Duration::from_secs(1)),
            defer_until_pool_available: false,
        }),
    };

    match dispatch_result {
        Ok(success) => {
            let response_audit_context =
                build_audit_context(&prepared_request, Some(success.account_id.as_str()));
            let response_turn_state = codex_turn_state_response_header(
                &prepared_request,
                success.upstream.headers(),
                success.upstream.status(),
            );
            if let Some(header) = response_turn_state.as_ref() {
                if let Some(request_id) = codex_turn_state_request_id_from_value(&header.value) {
                    bind_request_affinity_key(request_id.clone(), &success.account_id).await;
                    persist_request_affinity_key(
                        &success.account_id,
                        &prepared_request,
                        &request_id,
                    );
                }
            }
            let mut response_headers = Vec::new();
            if let Some(header) = response_turn_state {
                response_headers.push(header);
            }
            let mut active_lease = grant_active_stream_lease_for_request(
                &response_audit_context,
                &success.account_id,
                &prepared_request,
            );
            let response_capture = match write_gateway_response(
                &mut stream,
                success.upstream,
                response_adapter,
                &response_headers,
                Some(&response_audit_context),
            )
            .await
            {
                Ok(response_capture) => {
                    active_lease.release(ActiveStreamTerminal::Completed);
                    response_capture
                }
                Err(err) => {
                    active_lease.release(classify_active_stream_terminal_error(&err));
                    return Err(err);
                }
            };
            if let Some(response_id) = response_capture.response_id.as_deref() {
                bind_response_affinity(response_id, &success.account_id).await;
                if let Some(response_id_hash) =
                    hashed_request_correlation_id("response", response_id)
                {
                    record_audit_event_from_context(
                        &response_audit_context,
                        "response_affinity_bound",
                        None,
                        None,
                        None,
                        Some("bound"),
                        BTreeMap::from([(
                            "upstream_response_id_hash".to_string(),
                            response_id_hash,
                        )]),
                    );
                }
            }
            record_successful_proxy_stats(
                &success.account_id,
                &success.account_email,
                started_at,
                &response_capture,
            )
            .await;
            Ok(())
        }
        Err(error) => {
            let latency_ms = started_at.elapsed().as_millis() as u64;
            if should_write_in_band_pool_unavailable_response(
                &prepared_request,
                &response_adapter,
                &error,
            ) {
                write_in_band_pool_unavailable_response(
                    &mut stream,
                    &prepared_request,
                    &response_adapter,
                    &error,
                    latency_ms,
                )
                .await
            } else {
                write_proxy_dispatch_error_response(
                    &mut stream,
                    &addr,
                    &prepared_request,
                    error,
                    latency_ms,
                )
                .await
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        acquire_local_api_backpressure, acquire_local_api_backpressure_with_wait,
        active_stream_affinity_account_for_request, active_stream_lease_count_for_account,
        append_audit_event_to_path, apply_audit_trail_status_to_health_summary,
        apply_collection_routing_strategy, apply_local_api_safety_preset_to_collection,
        backpressure_wait_budget, bind_response_affinity, build_audit_context, build_audit_event,
        build_chat_completion_payload, build_chat_completion_stream_body,
        build_codex_api_failure_log, build_effective_local_access_account_ids,
        build_effective_local_access_account_ids_from_registry, build_health_summary_from_registry,
        build_health_summary_from_registry_for_accounts, build_images_api_payload,
        build_local_models_response, build_ordered_account_ids, build_pool_unavailable_message,
        build_projection_seed_local_access_account_ids, build_proxy_dispatch_error_body,
        build_request_routing_hint, build_responses_upstream_stream_error_sse,
        build_routing_pool_account_ids, build_runtime_account, build_runtime_mode_state,
        build_selector_audit_summary, cache_prepared_account, classified_audit_detail,
        classify_active_stream_terminal_error, classify_codex_api_failure,
        classify_codex_upstream_error, constrain_previous_response_affinity, empty_health_registry,
        extract_usage_capture, filter_local_access_account_ids, first_audit_timestamp_from_path,
        first_stable_local_access_port, gateway_runtime, grant_active_stream_lease,
        grant_active_stream_lease_for_request, handle_connection,
        hard_affinity_inline_retry_wait_limit, health_registry_account_cooldown_wait,
        health_registry_account_is_schedulable, health_registry_model_key,
        is_responses_completion_event, is_responses_websocket_upgrade_request,
        is_websocket_upgrade_request, json_response_with_retry_after,
        load_health_registry_from_path, load_runtime_mode_state,
        local_api_safety_config_for_preset, local_backpressure_wait_duration,
        next_routing_start_index, normalize_health_registry, normalize_local_api_safety_config,
        now_ms, official_codex_sticky_routing_boundary, parse_codex_retry_after,
        parse_http_request, parse_responses_payload_from_upstream, parse_retry_after_header_value,
        pause_health_registry_account, pin_process_sticky_account, pool_wait_fits_request_budget,
        prepare_gateway_request, proxy_dispatch_final_error_detail,
        proxy_dispatch_final_error_type, proxy_request_with_account_pool,
        prune_process_sticky_binding, record_manual_pause_audit_event,
        record_manual_recovery_audit_event, recover_health_registry_account,
        request_affinity_account_from_registry, request_affinity_key,
        request_lineage_id_with_source, reset_active_stream_leases_for_tests,
        reset_local_api_backpressure_for_tests, resolve_supported_model_alias,
        retry_failover_account_attempt_limit, retry_failover_max_retries,
        save_health_registry_to_path, selector_audit_detail, selector_selected_reason,
        set_runtime_integration_mode, should_block_direct_projection_change,
        should_block_runtime_projection_change, should_defer_pool_unavailable,
        should_restore_direct_projection_before_app_exit,
        should_retry_hard_affinity_upstream_failure, should_retry_single_account_upstream_status,
        should_sync_local_access_collection_on_account_switch, should_treat_response_as_stream,
        should_try_next_account, should_use_pool_unavailable_summary,
        should_write_in_band_pool_unavailable_response, sort_account_ids_by_health_estimate,
        sort_account_ids_by_health_estimate_with_quota_hints, status_for_pool_unavailable,
        summarize_pool_unavailability, update_health_registry_from_classified_error,
        upsert_process_sticky_binding, upsert_request_affinity_binding,
        upsert_successful_account_health, AccountQuotaSortHint, ActiveStreamTerminal, AuditContext,
        AuditTrailStatus, CodexLocalAccessErrorType, GatewayResponseAdapter,
        LocalApiBackpressureState, OfficialCodexStickyRoutingBoundary, ParsedRequest,
        ProxyDispatchError, ResponseUsageCollector, RuntimeProjectionContinuityRisk,
        StreamWriteState, CODEX_LOCAL_ACCESS_RUNTIME_PROVIDER_ID,
        CODEX_LOCAL_ACCESS_RUNTIME_PROVIDER_NAME, DAY_WINDOW_MS, MAX_HTTP_REQUEST_BYTES,
        PREFERRED_CODEX_LOCAL_ACCESS_PORTS, X_CODEX_TURN_METADATA_HEADER,
        X_CODEX_TURN_STATE_HEADER,
    };
    use crate::models::codex::{CodexAccount, CodexApiProviderMode, CodexQuota, CodexTokens};
    use crate::models::codex_local_access::{
        CodexLocalAccessAccountHealth, CodexLocalAccessAccountHealthStatus,
        CodexLocalAccessCollection, CodexLocalAccessHealthSummary, CodexLocalAccessModelCooldown,
        CodexLocalAccessRoutingStrategy, CodexLocalApiFallbackMode, CodexLocalApiSafetyConfig,
        CodexLocalApiSafetyPresetId, CodexRuntimeAccountKind, CodexRuntimeIntegrationMode,
    };
    use reqwest::header::{HeaderValue, RETRY_AFTER};
    use reqwest::StatusCode;
    use serde_json::{json, Value};
    use std::collections::{BTreeMap, HashMap, HashSet};
    use std::fs;
    use std::path::PathBuf;
    use std::sync::{LazyLock, Mutex};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};
    use tokio::time::Duration;

    static LOCAL_BACKPRESSURE_TEST_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));
    static LOCAL_ACCESS_ENV_TEST_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

    fn test_codex_account(id: &str) -> CodexAccount {
        CodexAccount::new(
            id.to_string(),
            format!("{}@example.com", id),
            CodexTokens {
                id_token: String::new(),
                access_token: String::new(),
                refresh_token: None,
            },
        )
    }

    fn test_concurrency_audit_event(
        timestamp: i64,
        phase: &str,
        error_type: Option<&str>,
    ) -> super::CodexLocalAccessAuditEvent {
        super::CodexLocalAccessAuditEvent {
            schema_version: super::CODEX_LOCAL_ACCESS_AUDIT_SCHEMA_VERSION,
            timestamp,
            request_id: format!("req-{}", timestamp),
            phase: phase.to_string(),
            route: "/v1/responses".to_string(),
            model: "gpt-5.5".to_string(),
            account_hash: "hash".to_string(),
            status: None,
            error_type: error_type.map(str::to_string),
            stream_state: None,
            outcome: Some("test".to_string()),
            detail: BTreeMap::new(),
        }
    }

    #[test]
    fn local_access_file_paths_honor_data_root_env_override() {
        let _guard = LOCAL_ACCESS_ENV_TEST_LOCK
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let previous = std::env::var_os(super::CODEX_LOCAL_ACCESS_DATA_ROOT_ENV);
        let root = std::env::temp_dir().join(format!(
            "cockpit-local-access-env-test-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);

        std::env::set_var(super::CODEX_LOCAL_ACCESS_DATA_ROOT_ENV, &root);

        assert_eq!(
            super::local_access_file_path().expect("local access path should resolve"),
            root.join(super::CODEX_LOCAL_ACCESS_FILE)
        );
        assert_eq!(
            super::local_access_health_file_path().expect("health path should resolve"),
            root.join(super::CODEX_LOCAL_ACCESS_HEALTH_FILE)
        );
        assert_eq!(
            super::local_access_audit_file_path().expect("audit path should resolve"),
            root.join(super::CODEX_LOCAL_ACCESS_AUDIT_FILE)
        );
        assert_eq!(
            super::runtime_mode_file_path().expect("runtime mode path should resolve"),
            root.join(super::CODEX_RUNTIME_MODE_FILE)
        );

        match previous {
            Some(value) => std::env::set_var(super::CODEX_LOCAL_ACCESS_DATA_ROOT_ENV, value),
            None => std::env::remove_var(super::CODEX_LOCAL_ACCESS_DATA_ROOT_ENV),
        }
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn concurrency_diagnostics_reports_configured_capacity_without_audit() {
        let _env_guard = LOCAL_ACCESS_ENV_TEST_LOCK
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let _backpressure_guard = LOCAL_BACKPRESSURE_TEST_LOCK
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let previous = std::env::var_os(super::CODEX_LOCAL_ACCESS_DATA_ROOT_ENV);
        let root = std::env::temp_dir().join(format!(
            "cockpit-local-access-concurrency-empty-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        std::env::set_var(super::CODEX_LOCAL_ACCESS_DATA_ROOT_ENV, &root);
        reset_local_api_backpressure_for_tests();
        reset_active_stream_leases_for_tests();

        let mut runtime = super::GatewayRuntime::default();
        runtime.collection = Some(CodexLocalAccessCollection {
            enabled: true,
            port: 45335,
            api_key: "ck-test".to_string(),
            safety_config: CodexLocalApiSafetyConfig {
                max_concurrent_requests: 2,
                min_request_interval_seconds: 3,
                max_queue_wait_seconds: 4,
                ..CodexLocalApiSafetyConfig::default()
            },
            routing_strategy: CodexLocalAccessRoutingStrategy::Auto,
            restrict_free_accounts: false,
            follow_current_account: false,
            account_ids: vec!["acc-a".to_string()],
            created_at: 1,
            updated_at: 2,
        });

        let diagnostics = super::build_concurrency_diagnostics(&runtime);

        assert_eq!(diagnostics.max_concurrent_requests, 2);
        assert_eq!(diagnostics.active_request_count, 0);
        assert_eq!(diagnostics.request_capacity, 2);
        assert_eq!(diagnostics.min_request_interval_seconds, 3);
        assert_eq!(diagnostics.max_queue_wait_seconds, 4);
        assert_eq!(
            diagnostics.audit_window_ms,
            super::CONCURRENCY_DIAGNOSTICS_AUDIT_WINDOW_MS
        );
        assert_eq!(diagnostics.recent_audit_event_count, 0);
        assert_eq!(diagnostics.recent_request_count, 0);
        assert!(diagnostics.audit_load_error.is_none());

        match previous {
            Some(value) => std::env::set_var(super::CODEX_LOCAL_ACCESS_DATA_ROOT_ENV, value),
            None => std::env::remove_var(super::CODEX_LOCAL_ACCESS_DATA_ROOT_ENV),
        }
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn concurrency_diagnostics_classifies_recent_audit_pressure() {
        let _env_guard = LOCAL_ACCESS_ENV_TEST_LOCK
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let previous = std::env::var_os(super::CODEX_LOCAL_ACCESS_DATA_ROOT_ENV);
        let root = std::env::temp_dir().join(format!(
            "cockpit-local-access-concurrency-audit-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("test root should be created");
        std::env::set_var(super::CODEX_LOCAL_ACCESS_DATA_ROOT_ENV, &root);

        let now = now_ms();
        let events = [
            test_concurrency_audit_event(now - 5_000, "listener", None),
            test_concurrency_audit_event(now - 4_000, "local_backpressure", None),
            test_concurrency_audit_event(now - 3_000, "pool_wait", None),
            test_concurrency_audit_event(
                now - 2_000,
                "final_response",
                Some("usage_limit_reached"),
            ),
            test_concurrency_audit_event(
                now - 1_000,
                "stream_error",
                Some("upstream_stream_error"),
            ),
            test_concurrency_audit_event(
                now - super::CONCURRENCY_DIAGNOSTICS_AUDIT_WINDOW_MS - 1_000,
                "local_backpressure",
                None,
            ),
        ];
        let mut content = String::new();
        for event in events {
            content.push_str(&serde_json::to_string(&event).expect("audit event should serialize"));
            content.push('\n');
        }
        fs::write(root.join(super::CODEX_LOCAL_ACCESS_AUDIT_FILE), content)
            .expect("audit fixture should be written");

        let mut runtime = super::GatewayRuntime::default();
        runtime.collection = Some(CodexLocalAccessCollection {
            enabled: true,
            port: 45335,
            api_key: "ck-test".to_string(),
            safety_config: CodexLocalApiSafetyConfig::default(),
            routing_strategy: CodexLocalAccessRoutingStrategy::Auto,
            restrict_free_accounts: false,
            follow_current_account: false,
            account_ids: vec!["acc-a".to_string()],
            created_at: 1,
            updated_at: 2,
        });

        let diagnostics = super::build_concurrency_diagnostics(&runtime);

        assert_eq!(diagnostics.recent_audit_event_count, 5);
        assert_eq!(diagnostics.recent_request_count, 1);
        assert_eq!(diagnostics.recent_local_backpressure_count, 1);
        assert_eq!(diagnostics.recent_pool_wait_count, 1);
        assert_eq!(diagnostics.recent_upstream_limit_count, 1);
        assert_eq!(diagnostics.recent_stream_error_count, 1);
        assert_eq!(
            diagnostics.last_problem_kind.as_deref(),
            Some("stream_error")
        );
        assert!(diagnostics.last_problem_at_ms.is_some());
        assert!(diagnostics.audit_load_error.is_none());

        match previous {
            Some(value) => std::env::set_var(super::CODEX_LOCAL_ACCESS_DATA_ROOT_ENV, value),
            None => std::env::remove_var(super::CODEX_LOCAL_ACCESS_DATA_ROOT_ENV),
        }
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn extracts_usage_from_codex_response_completed_payload() {
        let payload = json!({
            "type": "response.completed",
            "response": {
                "usage": {
                    "input_tokens": 16,
                    "input_tokens_details": {
                        "cached_tokens": 3
                    },
                    "output_tokens": 5,
                    "output_tokens_details": {
                        "reasoning_tokens": 2
                    },
                    "total_tokens": 21
                }
            }
        });

        let usage = extract_usage_capture(&payload).expect("usage should be parsed");
        assert_eq!(usage.input_tokens, 16);
        assert_eq!(usage.output_tokens, 5);
        assert_eq!(usage.cached_tokens, 3);
        assert_eq!(usage.reasoning_tokens, 2);
        assert_eq!(usage.total_tokens, 21);
    }

    #[test]
    fn extracts_usage_from_codex_response_done_payload() {
        assert!(is_responses_completion_event("response.done"));

        let payload = json!({
            "type": "response.done",
            "response": {
                "id": "resp_123",
                "usage": {
                    "input_tokens": 32,
                    "input_tokens_details": {
                        "cached_tokens": 9
                    },
                    "output_tokens": 6,
                    "output_tokens_details": {
                        "reasoning_tokens": 3
                    },
                    "total_tokens": 41
                }
            }
        });

        let usage = extract_usage_capture(&payload).expect("usage should be parsed");
        assert_eq!(usage.input_tokens, 32);
        assert_eq!(usage.output_tokens, 6);
        assert_eq!(usage.cached_tokens, 9);
        assert_eq!(usage.reasoning_tokens, 3);
        assert_eq!(usage.total_tokens, 41);
    }

    #[test]
    fn extracts_usage_from_openai_prompt_and_completion_details() {
        let payload = json!({
            "usage": {
                "prompt_tokens": 8,
                "prompt_tokens_details": {
                    "cached_tokens": 1
                },
                "completion_tokens": 4,
                "completion_tokens_details": {
                    "reasoning_tokens": 2
                }
            }
        });

        let usage = extract_usage_capture(&payload).expect("usage should be parsed");
        assert_eq!(usage.input_tokens, 8);
        assert_eq!(usage.output_tokens, 4);
        assert_eq!(usage.cached_tokens, 1);
        assert_eq!(usage.reasoning_tokens, 2);
        assert_eq!(usage.total_tokens, 14);
    }

    #[test]
    fn parses_sse_usage_when_request_is_stream_even_if_content_type_is_json() {
        assert!(should_treat_response_as_stream(
            "application/json; charset=utf-8",
            true
        ));

        let mut collector = ResponseUsageCollector::new(true);
        collector.feed(
            br#"event: response.completed
data: {"type":"response.completed","response":{"id":"resp_123","usage":{"input_tokens":16,"input_tokens_details":{"cached_tokens":0},"output_tokens":5,"output_tokens_details":{"reasoning_tokens":0},"total_tokens":21}}}

"#,
        );

        let capture = collector.finish();
        let usage = capture.usage.expect("stream usage should be parsed");
        assert_eq!(usage.input_tokens, 16);
        assert_eq!(usage.output_tokens, 5);
        assert_eq!(usage.total_tokens, 21);
        assert_eq!(capture.response_id.as_deref(), Some("resp_123"));
    }

    #[test]
    fn parses_codex_retry_after_from_usage_limit_payload() {
        let wait = parse_codex_retry_after(
            StatusCode::TOO_MANY_REQUESTS,
            r#"{"error":{"type":"usage_limit_reached","resets_in_seconds":12}}"#,
        )
        .expect("retry after should be parsed");

        assert_eq!(wait, Duration::from_secs(12));
    }

    #[test]
    fn parses_codex_retry_after_from_reset_after_aliases() {
        let wait = parse_codex_retry_after(
            StatusCode::TOO_MANY_REQUESTS,
            r#"{"error":{"type":"usage_limit_reached","reset_after_seconds":"45"}}"#,
        )
        .expect("retry after should be parsed from reset_after_seconds");

        assert_eq!(wait, Duration::from_secs(45));
    }

    #[test]
    fn parses_codex_retry_after_from_insufficient_quota_payload() {
        let wait = parse_codex_retry_after(
            StatusCode::TOO_MANY_REQUESTS,
            r#"{"error":{"type":"insufficient_quota","reset_after_seconds":"90"}}"#,
        )
        .expect("retry after should be parsed from account quota exhaustion");

        assert_eq!(wait, Duration::from_secs(90));
    }

    #[test]
    fn parses_codex_retry_after_from_generic_rate_limit_payload() {
        let wait = parse_codex_retry_after(
            StatusCode::TOO_MANY_REQUESTS,
            r#"{"error":{"type":"rate_limit_exceeded","retry_after":30}}"#,
        )
        .expect("retry after should be parsed from generic upstream rate limit");

        assert_eq!(wait, Duration::from_secs(30));
    }

    #[test]
    fn hard_affinity_retry_allows_only_short_reset_window() {
        let classified = classify_codex_upstream_error(
            StatusCode::TOO_MANY_REQUESTS,
            None,
            r#"{"error":{"type":"usage_limit_reached","reset_after_seconds":2}}"#,
        );

        assert_eq!(classified.retry_after, Some(Duration::from_secs(2)));
        assert_eq!(
            should_retry_hard_affinity_upstream_failure(
                true,
                &classified,
                0,
                1,
                hard_affinity_inline_retry_wait_limit(&CodexLocalApiSafetyConfig::default())
            ),
            Some(Duration::from_secs(2))
        );
    }

    #[test]
    fn hard_affinity_retry_refuses_weekly_usage_limit_reset_wait() {
        let classified = classify_codex_upstream_error(
            StatusCode::TOO_MANY_REQUESTS,
            None,
            r#"{"error":{"type":"usage_limit_reached","reset_after_seconds":604282}}"#,
        );

        let reset_wait = Duration::from_secs(604_282);
        let retry_limit =
            hard_affinity_inline_retry_wait_limit(&CodexLocalApiSafetyConfig::default());
        assert_eq!(classified.retry_after, Some(reset_wait));
        assert!(
            retry_limit < reset_wait,
            "hard-affinity continuation must not stall until a long quota reset; limit={:?}, reset_wait={:?}",
            retry_limit,
            reset_wait
        );
        assert_eq!(
            should_retry_hard_affinity_upstream_failure(true, &classified, 0, 1, retry_limit),
            None
        );
    }

    #[test]
    fn sticky_429_dispatch_error_body_uses_official_usage_limit_shape() {
        let request = ParsedRequest {
            method: "POST".to_string(),
            target: "/v1/responses".to_string(),
            headers: HashMap::from([(
                X_CODEX_TURN_STATE_HEADER.to_string(),
                "turn-state-token".to_string(),
            )]),
            body: br#"{"model":"gpt-5.5","input":"continue"}"#.to_vec(),
            gateway_request_id: "gw-test-sticky-429-body".to_string(),
        };

        let body = build_proxy_dispatch_error_body(
            &request,
            StatusCode::TOO_MANY_REQUESTS.as_u16(),
            "sticky task waiting for old account quota reset",
            Some(Duration::from_secs(60)),
        );

        assert_eq!(
            body.get("error")
                .and_then(|error| error.get("type"))
                .and_then(Value::as_str),
            Some("usage_limit_reached")
        );
        assert_eq!(
            body.get("error")
                .and_then(|error| error.get("code"))
                .and_then(Value::as_str),
            Some("usage_limit_reached")
        );
        assert!(
            body.get("error")
                .and_then(|error| error.get("resets_at"))
                .and_then(Value::as_i64)
                .is_some(),
            "official Codex maps 429 to UsageLimitReached only when the body keeps the structured error object"
        );
    }

    #[test]
    fn hard_affinity_retry_wait_limit_does_not_raise_short_budget_to_day() {
        let mut config = CodexLocalApiSafetyConfig::default();
        config.request_timeout_seconds = 1;

        assert_eq!(
            hard_affinity_inline_retry_wait_limit(&config),
            Duration::from_secs(3)
        );
    }

    #[test]
    fn classifier_prefers_retry_after_headers_over_body_resets() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("retry-after-ms", HeaderValue::from_static("2500"));
        headers.insert(RETRY_AFTER, HeaderValue::from_static("9"));

        let classified = classify_codex_upstream_error(
            StatusCode::TOO_MANY_REQUESTS,
            Some(&headers),
            r#"{"error":{"type":"usage_limit_reached","resets_in_seconds":12}}"#,
        );

        assert_eq!(
            classified.error_type,
            CodexLocalAccessErrorType::UsageLimitReached
        );
        assert_eq!(classified.retry_after, Some(Duration::from_millis(2500)));
        assert_eq!(
            classified.provider_code.as_deref(),
            Some("usage_limit_reached")
        );
        assert_eq!(
            classified.log_fields.get("error_type").map(String::as_str),
            Some("usage_limit_reached")
        );
    }

    #[test]
    fn classifier_records_safe_plan_and_quota_metadata_without_raw_prompt_text() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("x-codex-plan-type", HeaderValue::from_static("free"));
        headers.insert("x-codex-active-limit", HeaderValue::from_static("weekly"));
        headers.insert(
            "x-codex-rate-limit-reached-type",
            HeaderValue::from_static("weekly"),
        );
        headers.insert(
            "x-codex-promo-message",
            HeaderValue::from_static("upgrade raw-prompt user@example.com"),
        );
        let body = r#"{"error":{"type":"usage_limit_reached","plan_type":"free","resets_at":1700000360000,"resets_in_seconds":604282,"promo_message":"upgrade raw prompt user@example.com","message":"raw prompt text sk-secret user@example.com"}}"#;

        let classified =
            classify_codex_upstream_error(StatusCode::TOO_MANY_REQUESTS, Some(&headers), body);
        let audit_detail = classified_audit_detail(&classified);

        assert_eq!(
            classified.log_fields.get("plan_type").map(String::as_str),
            Some("free")
        );
        assert_eq!(
            classified
                .log_fields
                .get("provider_plan_type")
                .map(String::as_str),
            Some("free")
        );
        assert_eq!(
            classified.log_fields.get("reset_at").map(String::as_str),
            Some("1700000360")
        );
        assert_eq!(
            classified
                .log_fields
                .get("reset_after_seconds")
                .map(String::as_str),
            Some("604282")
        );
        assert_eq!(
            classified
                .log_fields
                .get("active_limit")
                .map(String::as_str),
            Some("weekly")
        );
        assert_eq!(
            classified
                .log_fields
                .get("rate_limit_reached_type")
                .map(String::as_str),
            Some("weekly")
        );
        assert_eq!(
            classified
                .log_fields
                .get("promo_message_present")
                .map(String::as_str),
            Some("true")
        );
        assert_eq!(
            audit_detail.get("provider_plan_type").map(String::as_str),
            Some("free")
        );

        let serialized = serde_json::to_string(&audit_detail).expect("detail serializes");
        for secret in ["raw prompt", "raw-prompt", "sk-secret", "user@example.com"] {
            assert!(
                !serialized.contains(secret),
                "classified audit detail leaked {secret}"
            );
        }
    }

    #[test]
    fn api_service_usage_limit_writes_zero_quota_snapshot_with_reset_hint() {
        let now_ms = 1_700_000_000_000;
        let mut account = test_codex_account("api-service-quota");
        account.quota = Some(CodexQuota {
            hourly_percentage: 64,
            hourly_reset_time: Some(111),
            hourly_window_minutes: Some(300),
            hourly_window_present: Some(true),
            weekly_percentage: 27,
            weekly_reset_time: Some(222),
            weekly_window_minutes: Some(10080),
            weekly_window_present: Some(true),
            raw_data: None,
        });
        let body = r#"{"error":{"type":"usage_limit_reached","resets_at":1700000360,"resets_in_seconds":360,"message":"raw prompt text sk-secret user@example.com"}}"#;
        let classified = classify_codex_upstream_error(StatusCode::TOO_MANY_REQUESTS, None, body);

        assert!(super::apply_account_quota_exhaustion_snapshot(
            &mut account,
            &classified,
            body,
            now_ms,
        ));

        let quota = account.quota.as_ref().expect("quota snapshot");
        assert_eq!(quota.hourly_percentage, 0);
        assert_eq!(quota.weekly_percentage, 0);
        assert_eq!(quota.hourly_reset_time, Some(1_700_000_360));
        assert_eq!(quota.weekly_reset_time, Some(1_700_000_360));
        assert_eq!(account.usage_updated_at, Some(1_700_000_000));
        assert_eq!(
            account
                .quota_error
                .as_ref()
                .and_then(|error| error.code.as_deref()),
            Some("usage_limit_reached")
        );
        assert_eq!(
            quota
                .raw_data
                .as_ref()
                .and_then(|value| value.get("source"))
                .and_then(Value::as_str),
            Some("codex_local_access_upstream_error")
        );
        assert_eq!(
            quota
                .raw_data
                .as_ref()
                .and_then(|value| value.get("reset_after_seconds"))
                .and_then(Value::as_i64),
            Some(360)
        );

        let serialized = serde_json::to_string(&account).expect("account should serialize");
        for secret in ["raw prompt text", "sk-secret", "user@example.com"] {
            assert!(
                !serialized.contains(secret),
                "quota snapshot leaked {secret}"
            );
        }
    }

    #[test]
    fn api_service_unknown_rate_limit_does_not_zero_account_quota() {
        let now_ms = 1_700_000_000_000;
        let mut account = test_codex_account("api-service-rate-limit");
        account.quota = Some(CodexQuota {
            hourly_percentage: 64,
            hourly_reset_time: Some(111),
            hourly_window_minutes: Some(300),
            hourly_window_present: Some(true),
            weekly_percentage: 27,
            weekly_reset_time: Some(222),
            weekly_window_minutes: Some(10080),
            weekly_window_present: Some(true),
            raw_data: None,
        });
        let body = r#"{"error":{"type":"rate_limit_exceeded","message":"slow down"}}"#;
        let classified = classify_codex_upstream_error(StatusCode::TOO_MANY_REQUESTS, None, body);

        assert!(!super::apply_account_quota_exhaustion_snapshot(
            &mut account,
            &classified,
            body,
            now_ms,
        ));

        let quota = account.quota.as_ref().expect("quota should stay");
        assert_eq!(quota.hourly_percentage, 64);
        assert_eq!(quota.weekly_percentage, 27);
        assert!(account.quota_error.is_none());
        assert!(account.usage_updated_at.is_none());
    }

    #[test]
    fn retry_after_http_date_parser_uses_supplied_now() {
        let now = chrono::DateTime::parse_from_rfc2822("Sun, 17 May 2026 00:00:00 GMT")
            .expect("valid now")
            .with_timezone(&chrono::Utc);
        let wait = parse_retry_after_header_value("Sun, 17 May 2026 00:00:05 GMT", now)
            .expect("future http date should parse");

        assert_eq!(wait, Duration::from_secs(5));
        assert!(parse_retry_after_header_value("Sun, 17 May 2025 00:00:00 GMT", now).is_none());
    }

    #[test]
    fn classifier_blocks_auth_and_captcha_request_failover() {
        let auth = classify_codex_upstream_error(StatusCode::UNAUTHORIZED, None, "");
        assert_eq!(auth.error_type, CodexLocalAccessErrorType::AuthError);
        assert!(auth.manual_required);
        assert!(!auth.safe_for_request_failover());
        assert!(!should_try_next_account(StatusCode::UNAUTHORIZED, ""));

        let captcha = classify_codex_upstream_error(
            StatusCode::FORBIDDEN,
            None,
            r#"{"error":{"type":"captcha_required","message":"captcha token raw prompt text"}}"#,
        );
        assert_eq!(
            captcha.error_type,
            CodexLocalAccessErrorType::CaptchaOrSuspicious
        );
        assert!(captcha.manual_required);
        assert!(!captcha.safe_for_request_failover());
        assert!(!should_try_next_account(
            StatusCode::FORBIDDEN,
            r#"{"error":{"type":"captcha_required","message":"captcha token raw prompt text"}}"#
        ));
        assert!(!captcha.safe_message.contains("raw prompt text"));
        assert!(captcha
            .log_fields
            .values()
            .all(|value| !value.contains("raw prompt text")));
    }

    #[test]
    fn classifier_keeps_unknown_429_on_current_account() {
        let upstream_rate_limit = classify_codex_upstream_error(
            StatusCode::TOO_MANY_REQUESTS,
            None,
            r#"{"error":{"type":"rate_limit_exceeded","message":"raw prompt text"}}"#,
        );

        assert_eq!(
            upstream_rate_limit.error_type,
            CodexLocalAccessErrorType::UpstreamRateLimit
        );
        assert!(!upstream_rate_limit.manual_required);
        assert!(!upstream_rate_limit.safe_for_request_failover());
        assert!(!should_try_next_account(
            StatusCode::TOO_MANY_REQUESTS,
            r#"{"error":{"type":"rate_limit_exceeded","message":"raw prompt text"}}"#
        ));
        assert!(!upstream_rate_limit.safe_message.contains("raw prompt text"));
    }

    #[test]
    fn health_registry_marks_usage_limit_model_cooldown_without_sensitive_fields() {
        let mut registry = empty_health_registry(1_700_000_000_000);
        let classified = classify_codex_upstream_error(
            StatusCode::TOO_MANY_REQUESTS,
            None,
            r#"{"error":{"type":"usage_limit_reached","resets_in_seconds":60,"message":"raw prompt text sk-secret user@example.com"}}"#,
        );

        update_health_registry_from_classified_error(
            &mut registry,
            "account-1",
            Some("gpt-5.5"),
            Some("req-1"),
            &classified,
            1_700_000_000_000,
        );

        let account = registry
            .accounts
            .get("account-1")
            .expect("account health should be recorded");
        assert_eq!(account.status, CodexLocalAccessAccountHealthStatus::Healthy);
        assert_eq!(
            account.last_error_type.as_deref(),
            Some("usage_limit_reached")
        );
        assert_eq!(account.last_request_id.as_deref(), Some("req-1"));
        assert_eq!(account.cooldown_until_ms, None);
        assert_eq!(account.exhausted_at_ms, Some(1_700_000_000_000));
        assert_eq!(account.estimated_reset_at_ms, Some(1_700_000_060_000));
        assert_eq!(account.estimated_remaining_percentage, Some(0));
        assert_eq!(account.last_observed_remaining_percentage, Some(0));
        assert_eq!(account.last_quota_exhausted_at_ms, Some(1_700_000_000_000));
        assert!(!health_registry_account_is_schedulable(
            &registry,
            "account-1",
            Some("gpt-5.5"),
            1_700_000_000_000
        ));
        assert!(health_registry_account_is_schedulable(
            &registry,
            "account-1",
            Some("gpt-5.5-mini"),
            1_700_000_000_000
        ));

        let serialized = serde_json::to_string(&registry).expect("registry should serialize");
        for secret in ["raw prompt text", "sk-secret", "user@example.com"] {
            assert!(!serialized.contains(secret), "registry leaked {secret}");
        }
    }

    #[test]
    fn model_scoped_usage_limit_does_not_block_other_models() {
        let now = 1_700_000_000_000;
        let mut registry = empty_health_registry(now);
        let classified = classify_codex_upstream_error(
            StatusCode::TOO_MANY_REQUESTS,
            None,
            r#"{"error":{"type":"usage_limit_reached","resets_in_seconds":60}}"#,
        );

        update_health_registry_from_classified_error(
            &mut registry,
            "account-model-scope",
            Some("gpt-5.4-mini"),
            Some("req-mini-limit"),
            &classified,
            now,
        );

        let account = registry
            .accounts
            .get("account-model-scope")
            .expect("account health should be recorded");
        assert_eq!(account.status, CodexLocalAccessAccountHealthStatus::Healthy);
        assert!(!health_registry_account_is_schedulable(
            &registry,
            "account-model-scope",
            Some("gpt-5.4-mini"),
            now
        ));
        assert!(health_registry_account_is_schedulable(
            &registry,
            "account-model-scope",
            Some("gpt-5.5"),
            now
        ));
        assert_eq!(
            health_registry_account_cooldown_wait(
                &registry,
                "account-model-scope",
                Some("gpt-5.4-mini"),
                now
            ),
            Some(Duration::from_secs(60))
        );
    }

    #[test]
    fn health_registry_model_cooldown_wait_is_exposed_for_scheduler() {
        let now = 1_700_000_000_000;
        let mut registry = empty_health_registry(now);
        registry.accounts.insert(
            "account-model-cooldown".to_string(),
            CodexLocalAccessAccountHealth {
                status: CodexLocalAccessAccountHealthStatus::Healthy,
                updated_at: now,
                ..CodexLocalAccessAccountHealth::default()
            },
        );
        registry.model_cooldowns.insert(
            health_registry_model_key("account-model-cooldown", "gpt-5.4"),
            CodexLocalAccessModelCooldown {
                account_id: "account-model-cooldown".to_string(),
                model: "gpt-5.4".to_string(),
                cooldown_until_ms: now + 60_000,
                updated_at: now,
                ..CodexLocalAccessModelCooldown::default()
            },
        );

        assert_eq!(
            health_registry_account_cooldown_wait(
                &registry,
                "account-model-cooldown",
                Some("gpt-5.4"),
                now
            ),
            Some(Duration::from_secs(60))
        );
        assert!(!health_registry_account_is_schedulable(
            &registry,
            "account-model-cooldown",
            Some("gpt-5.4"),
            now
        ));
    }

    #[test]
    fn health_registry_unknown_429_is_cooling_not_exhausted() {
        let mut registry = empty_health_registry(1_700_000_000_000);
        let classified = classify_codex_upstream_error(
            StatusCode::TOO_MANY_REQUESTS,
            None,
            r#"{"error":{"type":"rate_limit_exceeded","message":"slow down"}}"#,
        );

        update_health_registry_from_classified_error(
            &mut registry,
            "account-2",
            Some("gpt-5.5"),
            Some("req-2"),
            &classified,
            1_700_000_000_000,
        );

        let account = registry
            .accounts
            .get("account-2")
            .expect("account health should be recorded");
        assert_eq!(
            account.status,
            CodexLocalAccessAccountHealthStatus::CoolingDown
        );
        assert_ne!(
            account.status,
            CodexLocalAccessAccountHealthStatus::Exhausted
        );
        assert_eq!(
            account.last_error_type.as_deref(),
            Some("upstream_rate_limit")
        );
    }

    #[test]
    fn health_registry_estimates_quota_reset_after_recorded_reset_time() {
        let now = 1_700_000_000_000;
        let mut registry = empty_health_registry(now);
        let classified = classify_codex_upstream_error(
            StatusCode::TOO_MANY_REQUESTS,
            None,
            r#"{"error":{"type":"usage_limit_reached","resets_in_seconds":60}}"#,
        );

        update_health_registry_from_classified_error(
            &mut registry,
            "account-reset",
            None,
            Some("req-reset"),
            &classified,
            now,
        );

        let before_reset = registry
            .accounts
            .get("account-reset")
            .expect("account should be tracked");
        assert_eq!(before_reset.last_observed_remaining_percentage, Some(0));
        assert_eq!(before_reset.estimated_remaining_percentage, Some(0));
        assert_eq!(before_reset.confidence.as_deref(), Some("confirmed"));
        assert_eq!(
            before_reset.reset_source.as_deref(),
            Some("upstream_reset_hint")
        );
        assert!(!health_registry_account_is_schedulable(
            &registry,
            "account-reset",
            Some("gpt-5.5"),
            now
        ));

        let normalized = normalize_health_registry(registry, now + 61_000);
        let after_reset = normalized
            .accounts
            .get("account-reset")
            .expect("account should still be tracked");
        assert_eq!(
            after_reset.status,
            CodexLocalAccessAccountHealthStatus::EstimatedAvailable
        );
        assert_eq!(after_reset.estimated_remaining_percentage, Some(100));
        assert_eq!(after_reset.confidence.as_deref(), Some("estimated"));
        assert!(health_registry_account_is_schedulable(
            &normalized,
            "account-reset",
            Some("gpt-5.5"),
            now + 61_000
        ));
    }

    #[test]
    fn health_registry_insufficient_quota_uses_reset_hint_for_recovery() {
        let now = 1_700_000_000_000;
        let mut registry = empty_health_registry(now);
        let classified = classify_codex_upstream_error(
            StatusCode::TOO_MANY_REQUESTS,
            None,
            r#"{"error":{"type":"insufficient_quota","reset_after_seconds":90}}"#,
        );

        assert_eq!(
            classified.error_type,
            CodexLocalAccessErrorType::InsufficientQuota
        );
        assert_eq!(classified.retry_after, Some(Duration::from_secs(90)));

        update_health_registry_from_classified_error(
            &mut registry,
            "account-insufficient",
            Some("gpt-5.5"),
            Some("req-insufficient"),
            &classified,
            now,
        );

        let account = registry
            .accounts
            .get("account-insufficient")
            .expect("account should be tracked");
        assert_eq!(
            account.status,
            CodexLocalAccessAccountHealthStatus::Exhausted
        );
        assert_eq!(account.cooldown_until_ms, Some(now + 90_000));
        assert_eq!(account.estimated_reset_at_ms, Some(now + 90_000));
        assert_eq!(account.estimated_remaining_percentage, Some(0));
        assert_eq!(account.last_observed_remaining_percentage, Some(0));
        assert_eq!(account.reset_source.as_deref(), Some("upstream_reset_hint"));
        assert!(!health_registry_account_is_schedulable(
            &registry,
            "account-insufficient",
            Some("gpt-5.5"),
            now
        ));

        let normalized = normalize_health_registry(registry, now + 91_000);
        let recovered = normalized
            .accounts
            .get("account-insufficient")
            .expect("account should still be tracked");
        assert_eq!(
            recovered.status,
            CodexLocalAccessAccountHealthStatus::EstimatedAvailable
        );
        assert_eq!(recovered.estimated_remaining_percentage, Some(100));
    }

    #[test]
    fn health_registry_generic_rate_limit_uses_reset_hint_without_zero_quota() {
        let now = 1_700_000_000_000;
        let mut registry = empty_health_registry(now);
        let classified = classify_codex_upstream_error(
            StatusCode::TOO_MANY_REQUESTS,
            None,
            r#"{"error":{"type":"rate_limit_exceeded","reset_after_seconds":30}}"#,
        );

        assert_eq!(
            classified.error_type,
            CodexLocalAccessErrorType::UpstreamRateLimit
        );
        assert_eq!(classified.retry_after, Some(Duration::from_secs(30)));

        update_health_registry_from_classified_error(
            &mut registry,
            "account-rate-limit",
            Some("gpt-5.5"),
            Some("req-rate-limit"),
            &classified,
            now,
        );

        let account = registry
            .accounts
            .get("account-rate-limit")
            .expect("account should be tracked");
        assert_eq!(
            account.status,
            CodexLocalAccessAccountHealthStatus::CoolingDown
        );
        assert_eq!(account.cooldown_until_ms, Some(now + 30_000));
        assert_eq!(account.estimated_remaining_percentage, None);
        assert_eq!(account.last_observed_remaining_percentage, None);
        assert_eq!(account.reset_source.as_deref(), Some("upstream_reset_hint"));
    }

    #[test]
    fn normalize_demotes_legacy_model_scoped_usage_limit_account_cooldown() {
        let now = 1_700_000_000_000;
        let mut registry = empty_health_registry(now);
        registry.accounts.insert(
            "legacy-account".to_string(),
            CodexLocalAccessAccountHealth {
                status: CodexLocalAccessAccountHealthStatus::CoolingDown,
                cooldown_until_ms: Some(now + 60_000),
                exhausted_at_ms: Some(now - 1),
                estimated_reset_at_ms: Some(now + 60_000),
                estimated_remaining_percentage: Some(0),
                last_observed_remaining_percentage: Some(0),
                reset_source: Some("upstream_reset_hint".to_string()),
                confidence: Some("confirmed".to_string()),
                last_error_type: Some("usage_limit_reached".to_string()),
                updated_at: now - 1,
                ..CodexLocalAccessAccountHealth::default()
            },
        );
        registry.model_cooldowns.insert(
            health_registry_model_key("legacy-account", "gpt-5.4-mini"),
            CodexLocalAccessModelCooldown {
                account_id: "legacy-account".to_string(),
                model: "gpt-5.4-mini".to_string(),
                cooldown_until_ms: now + 60_000,
                last_error_type: Some("usage_limit_reached".to_string()),
                updated_at: now - 1,
                ..CodexLocalAccessModelCooldown::default()
            },
        );

        let normalized = normalize_health_registry(registry, now);
        let account = normalized
            .accounts
            .get("legacy-account")
            .expect("legacy account should remain tracked");
        assert_eq!(account.status, CodexLocalAccessAccountHealthStatus::Healthy);
        assert_eq!(account.cooldown_until_ms, None);
        assert!(health_registry_account_is_schedulable(
            &normalized,
            "legacy-account",
            Some("gpt-5.5"),
            now
        ));
        assert!(!health_registry_account_is_schedulable(
            &normalized,
            "legacy-account",
            Some("gpt-5.4-mini"),
            now
        ));
    }

    #[test]
    fn health_estimate_sorting_prioritizes_confirmed_then_estimated_accounts() {
        let now = 1_700_000_000_000;
        let mut registry = empty_health_registry(now);
        registry.accounts.insert(
            "estimated".to_string(),
            CodexLocalAccessAccountHealth {
                status: CodexLocalAccessAccountHealthStatus::EstimatedAvailable,
                estimated_remaining_percentage: Some(100),
                estimated_reset_at_ms: Some(now - 1),
                updated_at: now,
                ..CodexLocalAccessAccountHealth::default()
            },
        );
        registry.accounts.insert(
            "confirmed".to_string(),
            CodexLocalAccessAccountHealth {
                status: CodexLocalAccessAccountHealthStatus::Healthy,
                estimated_remaining_percentage: Some(80),
                updated_at: now,
                ..CodexLocalAccessAccountHealth::default()
            },
        );
        registry.accounts.insert(
            "cooling".to_string(),
            CodexLocalAccessAccountHealth {
                status: CodexLocalAccessAccountHealthStatus::CoolingDown,
                cooldown_until_ms: Some(now + 60_000),
                last_observed_remaining_percentage: Some(0),
                updated_at: now,
                ..CodexLocalAccessAccountHealth::default()
            },
        );

        let mut account_ids = vec![
            "cooling".to_string(),
            "estimated".to_string(),
            "unknown".to_string(),
            "confirmed".to_string(),
        ];
        sort_account_ids_by_health_estimate(&mut account_ids, &registry, now);

        assert_eq!(
            account_ids,
            vec![
                "confirmed".to_string(),
                "estimated".to_string(),
                "unknown".to_string(),
                "cooling".to_string(),
            ]
        );
    }

    #[test]
    fn health_estimate_sorting_preserves_recent_success_continuity() {
        let now = 1_700_000_000_000;
        let mut registry = empty_health_registry(now);
        for (account_id, last_success_at_ms) in
            [("used-older", now - 60_000), ("used-recent", now - 1_000)]
        {
            registry.accounts.insert(
                account_id.to_string(),
                CodexLocalAccessAccountHealth {
                    status: CodexLocalAccessAccountHealthStatus::Healthy,
                    estimated_remaining_percentage: Some(80),
                    last_selected_at_ms: Some(last_success_at_ms),
                    last_success_at_ms: Some(last_success_at_ms),
                    api_service_success_count: 1,
                    updated_at: last_success_at_ms,
                    ..CodexLocalAccessAccountHealth::default()
                },
            );
        }

        let mut account_ids = vec![
            "new-reserve".to_string(),
            "used-older".to_string(),
            "used-recent".to_string(),
        ];
        sort_account_ids_by_health_estimate(&mut account_ids, &registry, now);

        assert_eq!(
            account_ids,
            vec![
                "used-recent".to_string(),
                "used-older".to_string(),
                "new-reserve".to_string(),
            ]
        );
    }

    #[test]
    fn health_estimate_sorting_prefers_used_recovered_accounts_over_new_reserve() {
        let now = 1_700_000_000_000;
        const TEST_WEEK_MS: i64 = 7 * 24 * 60 * 60 * 1000;
        let mut registry = empty_health_registry(now);
        registry.accounts.insert(
            "used-recovered".to_string(),
            CodexLocalAccessAccountHealth {
                status: CodexLocalAccessAccountHealthStatus::EstimatedAvailable,
                estimated_remaining_percentage: Some(100),
                estimated_reset_at_ms: Some(now - 1),
                last_success_at_ms: Some(now - TEST_WEEK_MS),
                last_quota_exhausted_at_ms: Some(now - 60_000),
                api_service_success_count: 3,
                updated_at: now,
                ..CodexLocalAccessAccountHealth::default()
            },
        );

        let mut account_ids = vec!["new-reserve".to_string(), "used-recovered".to_string()];
        sort_account_ids_by_health_estimate(&mut account_ids, &registry, now);

        assert_eq!(
            account_ids,
            vec!["used-recovered".to_string(), "new-reserve".to_string()]
        );
    }

    #[test]
    fn health_estimate_sorting_uses_account_quota_hints_for_visible_order() {
        let now = 1_700_000_000_000;
        let registry = empty_health_registry(now);
        let mut account_ids = vec![
            "weekly-0".to_string(),
            "weekly-97".to_string(),
            "weekly-30".to_string(),
        ];
        let quota_hints = HashMap::from([
            (
                "weekly-0".to_string(),
                AccountQuotaSortHint {
                    remaining_percentage: Some(0),
                    earliest_reset_at_ms: Some(now + 1_000),
                },
            ),
            (
                "weekly-97".to_string(),
                AccountQuotaSortHint {
                    remaining_percentage: Some(97),
                    earliest_reset_at_ms: Some(now + 9_000),
                },
            ),
            (
                "weekly-30".to_string(),
                AccountQuotaSortHint {
                    remaining_percentage: Some(30),
                    earliest_reset_at_ms: Some(now + 100),
                },
            ),
        ]);

        sort_account_ids_by_health_estimate_with_quota_hints(
            &mut account_ids,
            &registry,
            now,
            &quota_hints,
        );

        assert_eq!(
            account_ids,
            vec![
                "weekly-97".to_string(),
                "weekly-30".to_string(),
                "weekly-0".to_string(),
            ]
        );
    }

    #[test]
    fn health_registry_manual_required_blocks_schedulability() {
        let mut registry = empty_health_registry(1_700_000_000_000);
        let classified = classify_codex_upstream_error(StatusCode::UNAUTHORIZED, None, "");

        update_health_registry_from_classified_error(
            &mut registry,
            "account-3",
            None,
            Some("req-3"),
            &classified,
            1_700_000_000_000,
        );

        let account = registry
            .accounts
            .get("account-3")
            .expect("account health should be recorded");
        assert_eq!(
            account.status,
            CodexLocalAccessAccountHealthStatus::ManualRequired
        );
        assert!(account.manual_required);
        assert!(!health_registry_account_is_schedulable(
            &registry,
            "account-3",
            None,
            1_700_000_000_001
        ));
    }

    #[test]
    fn pool_unavailable_summary_reports_blocking_reason() {
        let now = 1_700_000_000_000;
        let mut registry = empty_health_registry(now);
        registry.accounts.insert(
            "cooling-account".to_string(),
            CodexLocalAccessAccountHealth {
                status: CodexLocalAccessAccountHealthStatus::CoolingDown,
                cooldown_until_ms: Some(now + 30_000),
                updated_at: now,
                ..CodexLocalAccessAccountHealth::default()
            },
        );
        registry.accounts.insert(
            "manual-account".to_string(),
            CodexLocalAccessAccountHealth {
                status: CodexLocalAccessAccountHealthStatus::ManualRequired,
                manual_required: true,
                updated_at: now,
                ..CodexLocalAccessAccountHealth::default()
            },
        );

        let summary = summarize_pool_unavailability(
            &registry,
            &["cooling-account".to_string(), "manual-account".to_string()],
            Some("gpt-5.5"),
            now,
        );

        assert_eq!(summary.total_count, 2);
        assert_eq!(summary.schedulable_count, 0);
        assert_eq!(summary.cooling_count, 1);
        assert_eq!(summary.manual_required_count, 1);
        assert_eq!(summary.nearest_wait, Some(Duration::from_secs(30)));
        assert_eq!(
            status_for_pool_unavailable(&summary),
            StatusCode::SERVICE_UNAVAILABLE.as_u16()
        );

        let message = build_pool_unavailable_message("gpt-5.5", &summary);
        assert!(message.contains("冷却中 1 个"));
        assert!(message.contains("需人工处理 1 个"));
    }

    #[test]
    fn pool_unavailable_with_reset_hint_is_deferred_inside_gateway() {
        let now = 1_700_000_000_000;
        let mut registry = empty_health_registry(now);
        for account_id in ["exhausted-a", "exhausted-b"] {
            registry.accounts.insert(
                account_id.to_string(),
                CodexLocalAccessAccountHealth {
                    status: CodexLocalAccessAccountHealthStatus::Exhausted,
                    estimated_reset_at_ms: Some(now + 60_000),
                    updated_at: now,
                    ..CodexLocalAccessAccountHealth::default()
                },
            );
        }

        let summary = summarize_pool_unavailability(
            &registry,
            &["exhausted-a".to_string(), "exhausted-b".to_string()],
            Some("gpt-5.5"),
            now,
        );

        assert!(should_use_pool_unavailable_summary(&summary));
        assert_eq!(summary.nearest_wait, Some(Duration::from_secs(60)));
        assert!(should_defer_pool_unavailable(&summary));
    }

    #[test]
    fn pool_unavailable_without_reset_hint_is_not_deferred() {
        let now = 1_700_000_000_000;
        let mut registry = empty_health_registry(now);
        registry.accounts.insert(
            "manual-a".to_string(),
            CodexLocalAccessAccountHealth {
                status: CodexLocalAccessAccountHealthStatus::ManualRequired,
                manual_required: true,
                updated_at: now,
                ..CodexLocalAccessAccountHealth::default()
            },
        );

        let summary = summarize_pool_unavailability(
            &registry,
            &["manual-a".to_string()],
            Some("gpt-5.5"),
            now,
        );

        assert!(should_use_pool_unavailable_summary(&summary));
        assert_eq!(summary.nearest_wait, None);
        assert!(!should_defer_pool_unavailable(&summary));
    }

    #[test]
    fn pool_wait_must_fit_inside_request_timeout_budget() {
        assert!(pool_wait_fits_request_budget(
            Duration::from_secs(2),
            Duration::from_secs(0),
            Duration::from_secs(120),
        ));
        assert!(pool_wait_fits_request_budget(
            Duration::from_secs(3),
            Duration::from_secs(0),
            Duration::from_secs(120),
        ));
        assert!(!pool_wait_fits_request_budget(
            Duration::from_secs(4),
            Duration::from_secs(0),
            Duration::from_secs(600),
        ));
        assert!(!pool_wait_fits_request_budget(
            Duration::from_secs(30),
            Duration::from_secs(10),
            Duration::from_secs(120),
        ));
        assert!(!pool_wait_fits_request_budget(
            Duration::from_secs(119),
            Duration::from_secs(0),
            Duration::from_secs(120),
        ));
        assert!(!pool_wait_fits_request_budget(
            Duration::from_secs(1),
            Duration::from_secs(119),
            Duration::from_secs(120),
        ));
    }

    #[test]
    fn pool_unavailable_stream_does_not_park_when_retry_after_exceeds_request_budget() {
        let error = ProxyDispatchError {
            status: StatusCode::SERVICE_UNAVAILABLE.as_u16(),
            message: "模型 gpt-5.5 的API 服务号池暂无可调度账号（冷却中 2 个）；请刷新配额、恢复账号或调整号池后重试"
                .to_string(),
            account_id: None,
            account_email: None,
            retry_after: Some(Duration::from_secs(7 * 24 * 60 * 60)),
            defer_until_pool_available: true,
        };

        assert_eq!(
            classify_codex_api_failure(Some(error.status), error.message.as_str()),
            "pool_unavailable"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn exhausted_responses_stream_returns_local_completion_when_wait_exceeds_budget() {
        let _env_guard = LOCAL_ACCESS_ENV_TEST_LOCK
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let _backpressure_guard = LOCAL_BACKPRESSURE_TEST_LOCK
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let previous_root = std::env::var_os(super::CODEX_LOCAL_ACCESS_DATA_ROOT_ENV);
        let root = std::env::temp_dir().join(format!(
            "cockpit-local-access-pool-terminal-stream-test-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let _ = fs::remove_dir_all(&root);
        std::env::set_var(super::CODEX_LOCAL_ACCESS_DATA_ROOT_ENV, &root);

        reset_local_api_backpressure_for_tests();
        reset_active_stream_leases_for_tests();

        let now = now_ms();
        let mut registry = empty_health_registry(now);
        registry.model_cooldowns.insert(
            health_registry_model_key("api-exhausted", "gpt-5.5"),
            CodexLocalAccessModelCooldown {
                account_id: "api-exhausted".to_string(),
                model: "gpt-5.5".to_string(),
                cooldown_until_ms: now + (7 * 24 * 60 * 60 * 1000),
                last_error_type: Some("usage_limit_reached".to_string()),
                last_request_id: Some("req-exhausted-local".to_string()),
                updated_at: now,
            },
        );
        save_health_registry_to_path(&root.join(super::CODEX_LOCAL_ACCESS_HEALTH_FILE), &registry)
            .expect("health registry should be written to isolated test root");

        let listener = TcpListener::bind((super::CODEX_LOCAL_ACCESS_BIND_HOST, 0))
            .await
            .expect("test listener should bind");
        let addr = listener
            .local_addr()
            .expect("listener should have local addr");
        let collection: CodexLocalAccessCollection = serde_json::from_value(json!({
            "enabled": true,
            "port": addr.port(),
            "apiKey": "test-local-key",
            "safetyConfig": {
                "schemaVersion": 1,
                "hardenedLocalMode": true,
                "maxConcurrentRequests": 1,
                "minRequestIntervalSeconds": 0,
                "maxQueueWaitSeconds": 1,
                "requestTimeoutSeconds": 5,
                "maxRequestBodyMb": 64,
                "maxRetries": 1,
                "maxRetryAccounts": 1,
                "fallbackMode": "disabled",
                "logging": {
                    "redactSensitiveValues": true,
                    "includeRequestId": true,
                    "includeAccountHash": true,
                    "includeRoute": true,
                    "includeModel": true,
                    "includeLatency": true,
                    "includePromptResponse": false,
                    "includeRawUpstreamBody": false
                }
            },
            "routingStrategy": "auto",
            "restrictFreeAccounts": false,
            "followCurrentAccount": false,
            "accountIds": ["api-exhausted"],
            "createdAt": now,
            "updatedAt": now
        }))
        .expect("collection fixture should deserialize");
        {
            let mut runtime = gateway_runtime().lock().await;
            *runtime = super::GatewayRuntime::default();
            runtime.loaded = true;
            runtime.collection = Some(collection);
            runtime.running = true;
            runtime.actual_port = Some(addr.port());
        }

        let server = tokio::spawn(async move {
            let (stream, peer) = listener.accept().await.expect("server should accept");
            handle_connection(stream, peer).await
        });
        let mut client = TcpStream::connect(addr)
            .await
            .expect("client should connect");
        let body = br#"{"model":"gpt-5.5","input":"hello"}"#;
        let request = format!(
            "POST /v1/responses HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer test-local-key\r\nAccept: text/event-stream\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            String::from_utf8_lossy(body)
        );
        client
            .write_all(request.as_bytes())
            .await
            .expect("request should be written");

        let mut response = Vec::new();
        tokio::time::timeout(Duration::from_secs(1), client.read_to_end(&mut response))
            .await
            .expect("exhausted stream should terminate promptly instead of silently waiting")
            .expect("response read should succeed");
        let response_text = String::from_utf8_lossy(&response);
        assert!(
            response_text.contains("HTTP/1.1 200 OK"),
            "expected protocol-shaped SSE response, got: {}",
            response_text
        );
        assert!(
            response_text.contains("text/event-stream"),
            "expected event-stream response, got: {}",
            response_text
        );
        assert!(
            response_text.contains("response.completed"),
            "expected local pool_unavailable SSE to complete gracefully, got: {}",
            response_text
        );
        assert!(
            !response_text.contains("response.failed"),
            "Codex-facing local pool_unavailable must not emit fatal response.failed: {}",
            response_text
        );
        assert!(
            response_text.contains("Cockpit API Service pool_unavailable")
                && response_text.contains("recover_action=retry_after_cooldown_or_start_new_task"),
            "streaming local pool_unavailable completion must include visible assistant text so Codex does not treat it as a silent successful turn: {}",
            response_text
        );
        assert!(
            response_text.contains("synthetic_pool_unavailable_notice")
                && response_text
                    .contains("openai_codex_response_completed_with_visible_assistant_message"),
            "expected local completion metadata to explain the Cockpit closure mode, got: {}",
            response_text
        );
        assert!(
            response_text.contains("response.output_item.added")
                && response_text.contains("response.content_part.added")
                && response_text.contains("response.output_text.delta")
                && response_text.contains("response.output_text.done")
                && response_text.contains("response.content_part.done")
                && response_text.contains("response.output_item.done"),
            "expected complete Responses streaming event sequence, got: {}",
            response_text
        );
        assert!(
            response_text.contains("[DONE]"),
            "expected completed SSE to close the stream, got: {}",
            response_text
        );
        assert!(
            !response_text.contains("cockpit_pool_wait"),
            "long exhausted waits must not keep the Codex turn silently parked: {}",
            response_text
        );

        let audit = fs::read_to_string(root.join(super::CODEX_LOCAL_ACCESS_AUDIT_FILE))
            .expect("audit should be written to isolated root");
        assert!(audit.contains("\"phase\":\"final_response\""));
        assert!(audit.contains("\"streamState\":\"completed\""));
        assert!(audit.contains("\"outcome\":\"in_band_local_completion\""));
        assert!(audit.contains("\"errorType\":\"pool_unavailable\""));
        assert!(audit.contains(
            "\"codex_facing_terminal_contract\":\"response.completed_local_pool_unavailable\""
        ));
        assert!(audit.contains("\"recover_action\":\"retry_after_cooldown_or_start_new_task\""));
        assert!(!audit.contains("\"streamState\":\"failed\""));
        assert!(!audit.contains("\"streamState\":\"heartbeat\""));

        let server_result = tokio::time::timeout(Duration::from_secs(1), server)
            .await
            .expect("server task should finish")
            .expect("server task should not panic");
        assert!(server_result.is_ok(), "server error: {:?}", server_result);
        {
            let mut runtime = gateway_runtime().lock().await;
            *runtime = super::GatewayRuntime::default();
        }
        reset_local_api_backpressure_for_tests();
        reset_active_stream_leases_for_tests();
        match previous_root {
            Some(value) => std::env::set_var(super::CODEX_LOCAL_ACCESS_DATA_ROOT_ENV, value),
            None => std::env::remove_var(super::CODEX_LOCAL_ACCESS_DATA_ROOT_ENV),
        }
        let _ = fs::remove_dir_all(&root);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn exhausted_responses_stream_completes_locally_preserving_active_stream() {
        let _env_guard = LOCAL_ACCESS_ENV_TEST_LOCK
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let _backpressure_guard = LOCAL_BACKPRESSURE_TEST_LOCK
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let previous_root = std::env::var_os(super::CODEX_LOCAL_ACCESS_DATA_ROOT_ENV);
        let root = std::env::temp_dir().join(format!(
            "cockpit-local-access-pool-unavailable-stream-test-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let _ = fs::remove_dir_all(&root);
        std::env::set_var(super::CODEX_LOCAL_ACCESS_DATA_ROOT_ENV, &root);

        reset_local_api_backpressure_for_tests();
        reset_active_stream_leases_for_tests();

        let active_request = ParsedRequest {
            method: "POST".to_string(),
            target: "/v1/responses".to_string(),
            headers: HashMap::new(),
            body: Vec::new(),
            gateway_request_id: "gw-test-3".to_string(),
        };
        let active_context = build_audit_context(&active_request, Some("active-a"));
        let mut active_lease = grant_active_stream_lease(&active_context, "active-a");
        assert_eq!(active_stream_lease_count_for_account("active-a"), 1);

        let upstream_listener = TcpListener::bind((super::CODEX_LOCAL_ACCESS_BIND_HOST, 0))
            .await
            .expect("fake upstream should bind");
        let upstream_addr = upstream_listener
            .local_addr()
            .expect("fake upstream should have addr");
        let upstream_server = tokio::spawn(async move {
            let (mut socket, _) = upstream_listener
                .accept()
                .await
                .expect("fake upstream should accept");
            let mut request = Vec::new();
            let mut chunk = [0u8; 1024];
            loop {
                let read = socket
                    .read(&mut chunk)
                    .await
                    .expect("fake upstream should read request");
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&chunk[..read]);
                if request.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            let body = concat!(
                "event: response.created\n",
                "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_recovered_after_long_wait\",\"model\":\"gpt-5.5\",\"status\":\"in_progress\",\"output\":[]}}\n\n",
                "event: response.output_text.delta\n",
                "data: {\"type\":\"response.output_text.delta\",\"item_id\":\"msg_1\",\"output_index\":0,\"content_index\":0,\"delta\":\"OK\"}\n\n",
                "event: response.completed\n",
                "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_recovered_after_long_wait\",\"model\":\"gpt-5.5\",\"status\":\"completed\",\"output\":[],\"usage\":{\"input_tokens\":1,\"output_tokens\":1,\"total_tokens\":2}}}\n\n",
                "data: [DONE]\n\n"
            );
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.as_bytes().len(),
                body
            );
            socket
                .write_all(response.as_bytes())
                .await
                .expect("fake upstream should write response");
        });

        let now = now_ms();
        let mut registry = empty_health_registry(now);
        registry.model_cooldowns.insert(
            health_registry_model_key("api-recovered", "gpt-5.5"),
            CodexLocalAccessModelCooldown {
                account_id: "api-recovered".to_string(),
                model: "gpt-5.5".to_string(),
                cooldown_until_ms: now + (7 * 24 * 60 * 60 * 1000),
                last_error_type: Some("usage_limit_reached".to_string()),
                last_request_id: Some("req-park-local".to_string()),
                updated_at: now,
            },
        );
        save_health_registry_to_path(&root.join(super::CODEX_LOCAL_ACCESS_HEALTH_FILE), &registry)
            .expect("health registry should be written to isolated test root");

        let listener = TcpListener::bind((super::CODEX_LOCAL_ACCESS_BIND_HOST, 0))
            .await
            .expect("test listener should bind");
        let addr = listener
            .local_addr()
            .expect("listener should have local addr");
        let collection: CodexLocalAccessCollection = serde_json::from_value(json!({
            "enabled": true,
            "port": addr.port(),
            "apiKey": "test-local-key",
            "safetyConfig": {
                "schemaVersion": 1,
                "hardenedLocalMode": true,
                "maxConcurrentRequests": 1,
                "minRequestIntervalSeconds": 1,
                "maxQueueWaitSeconds": 1,
                "requestTimeoutSeconds": 5,
                "maxRequestBodyMb": 64,
                "maxRetries": 1,
                "maxRetryAccounts": 2,
                "fallbackMode": "disabled",
                "logging": {
                    "redactSensitiveValues": true,
                    "includeRequestId": true,
                    "includeAccountHash": true,
                    "includeRoute": true,
                    "includeModel": true,
                    "includeLatency": true,
                    "includePromptResponse": false,
                    "includeRawUpstreamBody": false
                }
            },
            "routingStrategy": "auto",
            "restrictFreeAccounts": false,
            "followCurrentAccount": false,
            "accountIds": ["api-recovered"],
            "createdAt": now,
            "updatedAt": now
        }))
        .expect("collection fixture should deserialize");
        {
            let mut runtime = gateway_runtime().lock().await;
            *runtime = super::GatewayRuntime::default();
            runtime.loaded = true;
            runtime.collection = Some(collection);
            runtime.running = true;
            runtime.actual_port = Some(addr.port());
        }
        let account = CodexAccount::new_api_key(
            "api-recovered".to_string(),
            "api-recovered@example.com".to_string(),
            "sk-local-test".to_string(),
            CodexApiProviderMode::Custom,
            Some(format!("http://127.0.0.1:{}/v1", upstream_addr.port())),
            None,
            None,
        );
        cache_prepared_account(&account).await;

        let server = tokio::spawn(async move {
            let (stream, peer) = listener.accept().await.expect("server should accept");
            handle_connection(stream, peer).await
        });
        let mut client = TcpStream::connect(addr)
            .await
            .expect("client should connect");
        let body = br#"{"model":"gpt-5.5","input":"hello"}"#;
        let request = format!(
            "POST /v1/responses HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer test-local-key\r\nAccept: text/event-stream\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            String::from_utf8_lossy(body)
        );
        client
            .write_all(request.as_bytes())
            .await
            .expect("request should be written");

        let mut response = Vec::new();
        tokio::time::timeout(Duration::from_secs(1), client.read_to_end(&mut response))
            .await
            .expect(
                "exhausted stream should terminate promptly instead of waiting on active streams",
            )
            .expect("response read should succeed");
        let response_text = String::from_utf8_lossy(&response);
        assert!(
            response_text.contains("HTTP/1.1 200 OK"),
            "expected Codex protocol SSE response, got: {}",
            response_text
        );
        assert!(
            response_text.contains("text/event-stream"),
            "expected event-stream response, got: {}",
            response_text
        );
        assert!(
            response_text.contains("response.completed"),
            "expected exhausted request to complete locally without fatal stream failure, got: {}",
            response_text
        );
        assert!(
            !response_text.contains("response.failed"),
            "Codex-facing exhausted request must not emit response.failed: {}",
            response_text
        );
        assert!(
            response_text.contains("Cockpit API Service pool_unavailable")
                && response_text.contains("recover_action=retry_after_cooldown_or_start_new_task"),
            "streaming local pool_unavailable completion must include visible assistant text so Codex does not treat it as a silent successful turn: {}",
            response_text
        );
        assert!(
            response_text.contains("synthetic_pool_unavailable_notice")
                && response_text
                    .contains("openai_codex_response_completed_with_visible_assistant_message"),
            "expected local completion metadata to explain the Cockpit closure mode, got: {}",
            response_text
        );
        assert!(
            response_text.contains("[DONE]"),
            "expected completed SSE to close the stream, got: {}",
            response_text
        );
        assert!(
            !response_text.contains(": cockpit_pool_wait"),
            "long exhausted pool wait must not silently park the Codex turn: {}",
            response_text
        );
        assert!(
            !response_text.contains("503 Service Unavailable"),
            "Codex-facing exhausted stream must not expose transport 503: {}",
            response_text
        );
        assert_eq!(
            active_stream_lease_count_for_account("active-a"),
            1,
            "local completion for the new request must not release unrelated active streams"
        );
        active_lease.release(ActiveStreamTerminal::Completed);
        assert_eq!(active_stream_lease_count_for_account("active-a"), 0);

        let audit = fs::read_to_string(root.join(super::CODEX_LOCAL_ACCESS_AUDIT_FILE))
            .expect("audit should be written to isolated root");
        assert!(audit.contains("\"phase\":\"final_response\""));
        assert!(audit.contains("\"errorType\":\"pool_unavailable\""));
        assert!(audit.contains("\"streamState\":\"completed\""));
        assert!(audit.contains("\"outcome\":\"in_band_local_completion\""));
        assert!(!audit.contains("\"streamState\":\"failed\""));
        assert!(!audit.contains("\"streamState\":\"heartbeat\""));
        assert!(!audit.contains("\"outcome\":\"parked\""));
        assert!(!audit.contains("\"outcome\":\"in_band_synthetic\""));

        let server_result = tokio::time::timeout(Duration::from_secs(1), server)
            .await
            .expect("server task should finish")
            .expect("server task should not panic");
        assert!(server_result.is_ok(), "server error: {:?}", server_result);
        upstream_server.abort();
        {
            let mut runtime = gateway_runtime().lock().await;
            *runtime = super::GatewayRuntime::default();
        }
        reset_local_api_backpressure_for_tests();
        reset_active_stream_leases_for_tests();
        match previous_root {
            Some(value) => std::env::set_var(super::CODEX_LOCAL_ACCESS_DATA_ROOT_ENV, value),
            None => std::env::remove_var(super::CODEX_LOCAL_ACCESS_DATA_ROOT_ENV),
        }
        let _ = fs::remove_dir_all(&root);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn responses_stream_forwards_real_upstream_after_short_pool_wait_recovery() {
        let _env_guard = LOCAL_ACCESS_ENV_TEST_LOCK
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let _backpressure_guard = LOCAL_BACKPRESSURE_TEST_LOCK
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let previous_root = std::env::var_os(super::CODEX_LOCAL_ACCESS_DATA_ROOT_ENV);
        let root = std::env::temp_dir().join(format!(
            "cockpit-local-access-pool-recovery-stream-test-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let _ = fs::remove_dir_all(&root);
        std::env::set_var(super::CODEX_LOCAL_ACCESS_DATA_ROOT_ENV, &root);

        reset_local_api_backpressure_for_tests();
        reset_active_stream_leases_for_tests();

        let upstream_listener = TcpListener::bind((super::CODEX_LOCAL_ACCESS_BIND_HOST, 0))
            .await
            .expect("fake upstream should bind");
        let upstream_addr = upstream_listener
            .local_addr()
            .expect("fake upstream should have addr");
        let upstream_server = tokio::spawn(async move {
            let (mut socket, _) = upstream_listener
                .accept()
                .await
                .expect("fake upstream should accept");
            let mut request = Vec::new();
            let mut chunk = [0u8; 1024];
            loop {
                let read = socket
                    .read(&mut chunk)
                    .await
                    .expect("fake upstream should read request");
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&chunk[..read]);
                if request.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            let body = concat!(
                "event: response.created\n",
                "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_recovered\",\"model\":\"gpt-5.5\",\"status\":\"in_progress\",\"output\":[]}}\n\n",
                "event: response.output_text.delta\n",
                "data: {\"type\":\"response.output_text.delta\",\"item_id\":\"msg_1\",\"output_index\":0,\"content_index\":0,\"delta\":\"OK\"}\n\n",
                "event: response.completed\n",
                "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_recovered\",\"model\":\"gpt-5.5\",\"status\":\"completed\",\"output\":[],\"usage\":{\"input_tokens\":1,\"output_tokens\":1,\"total_tokens\":2}}}\n\n",
                "data: [DONE]\n\n"
            );
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.as_bytes().len(),
                body
            );
            socket
                .write_all(response.as_bytes())
                .await
                .expect("fake upstream should write response");
        });

        let now = now_ms();
        let mut registry = empty_health_registry(now);
        registry.model_cooldowns.insert(
            health_registry_model_key("api-recovered", "gpt-5.5"),
            CodexLocalAccessModelCooldown {
                account_id: "api-recovered".to_string(),
                model: "gpt-5.5".to_string(),
                cooldown_until_ms: now + 1_500,
                last_error_type: Some("usage_limit_reached".to_string()),
                last_request_id: Some("req-recover-local".to_string()),
                updated_at: now,
            },
        );
        save_health_registry_to_path(&root.join(super::CODEX_LOCAL_ACCESS_HEALTH_FILE), &registry)
            .expect("health registry should be written to isolated test root");

        let listener = TcpListener::bind((super::CODEX_LOCAL_ACCESS_BIND_HOST, 0))
            .await
            .expect("test listener should bind");
        let addr = listener
            .local_addr()
            .expect("listener should have local addr");
        let collection: CodexLocalAccessCollection = serde_json::from_value(json!({
            "enabled": true,
            "port": addr.port(),
            "apiKey": "test-local-key",
            "safetyConfig": {
                "schemaVersion": 1,
                "hardenedLocalMode": true,
                "maxConcurrentRequests": 2,
                "minRequestIntervalSeconds": 0,
                "maxQueueWaitSeconds": 1,
                "requestTimeoutSeconds": 5,
                "maxRequestBodyMb": 64,
                "maxRetries": 1,
                "maxRetryAccounts": 1,
                "fallbackMode": "disabled",
                "logging": {
                    "redactSensitiveValues": true,
                    "includeRequestId": true,
                    "includeAccountHash": true,
                    "includeRoute": true,
                    "includeModel": true,
                    "includeLatency": true,
                    "includePromptResponse": false,
                    "includeRawUpstreamBody": false
                }
            },
            "routingStrategy": "auto",
            "restrictFreeAccounts": false,
            "followCurrentAccount": false,
            "accountIds": ["api-recovered"],
            "createdAt": now,
            "updatedAt": now
        }))
        .expect("collection fixture should deserialize");
        {
            let mut runtime = gateway_runtime().lock().await;
            *runtime = super::GatewayRuntime::default();
            runtime.loaded = true;
            runtime.collection = Some(collection);
            runtime.running = true;
            runtime.actual_port = Some(addr.port());
        }
        let account = CodexAccount::new_api_key(
            "api-recovered".to_string(),
            "api-recovered@example.com".to_string(),
            "sk-local-test".to_string(),
            CodexApiProviderMode::Custom,
            Some(format!("http://127.0.0.1:{}/v1", upstream_addr.port())),
            None,
            None,
        );
        cache_prepared_account(&account).await;

        let server = tokio::spawn(async move {
            let (stream, peer) = listener.accept().await.expect("server should accept");
            handle_connection(stream, peer).await
        });
        let mut client = TcpStream::connect(addr)
            .await
            .expect("client should connect");
        let body = br#"{"model":"gpt-5.5","input":"hello"}"#;
        let request = format!(
            "POST /v1/responses HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer test-local-key\r\nAccept: text/event-stream\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            String::from_utf8_lossy(body)
        );
        client
            .write_all(request.as_bytes())
            .await
            .expect("request should be written");

        let mut response = Vec::new();
        for _ in 0..160 {
            let mut chunk = [0u8; 1024];
            let read = tokio::time::timeout(Duration::from_secs(5), client.read(&mut chunk))
                .await
                .expect("recovered stream should keep producing SSE")
                .expect("response read should succeed");
            if read == 0 {
                break;
            }
            response.extend_from_slice(&chunk[..read]);
            let response_text = String::from_utf8_lossy(&response);
            if response_text.contains("[DONE]") {
                break;
            }
        }
        let response_text = String::from_utf8_lossy(&response);
        assert!(
            response_text.contains("HTTP/1.1 200 OK"),
            "expected successful SSE response, got: {}",
            response_text
        );
        assert!(
            !response_text.contains("cockpit_pool_wait"),
            "short pre-admission wait should not send heartbeat comments before upstream admission, got: {}",
            response_text
        );
        assert!(
            response_text.contains("response.completed"),
            "expected real upstream completion after recovery, got: {}",
            response_text
        );
        assert!(
            response_text.contains("OK"),
            "expected upstream response body to be forwarded, got: {}",
            response_text
        );
        assert!(
            !response_text.contains("Cockpit API Service pool_unavailable"),
            "stream recovery must not emit terminal synthetic assistant text: {}",
            response_text
        );

        let audit = fs::read_to_string(root.join(super::CODEX_LOCAL_ACCESS_AUDIT_FILE))
            .expect("audit should be written to isolated root");
        assert!(audit.contains("\"phase\":\"pool_wait\""));
        assert!(!audit.contains("\"streamState\":\"heartbeat\""));
        assert!(audit.contains("\"phase\":\"stream_completed\""));
        assert!(!audit.contains("\"streamState\":\"failed\""));
        assert!(!audit.contains("\"outcome\":\"in_band_synthetic\""));

        let server_result = server.await.expect("server task should join");
        assert!(server_result.is_ok(), "server failed: {:?}", server_result);
        upstream_server
            .await
            .expect("fake upstream task should join");
        {
            let mut runtime = gateway_runtime().lock().await;
            *runtime = super::GatewayRuntime::default();
        }
        reset_local_api_backpressure_for_tests();
        reset_active_stream_leases_for_tests();
        match previous_root {
            Some(value) => std::env::set_var(super::CODEX_LOCAL_ACCESS_DATA_ROOT_ENV, value),
            None => std::env::remove_var(super::CODEX_LOCAL_ACCESS_DATA_ROOT_ENV),
        }
        let _ = fs::remove_dir_all(&root);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn single_account_previous_response_continuation_bypasses_pool_snapshot_and_forwards_upstream(
    ) {
        let _env_guard = LOCAL_ACCESS_ENV_TEST_LOCK
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let _backpressure_guard = LOCAL_BACKPRESSURE_TEST_LOCK
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let previous_root = std::env::var_os(super::CODEX_LOCAL_ACCESS_DATA_ROOT_ENV);
        let root = std::env::temp_dir().join(format!(
            "cockpit-local-access-continuation-cooldown-test-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let _ = fs::remove_dir_all(&root);
        std::env::set_var(super::CODEX_LOCAL_ACCESS_DATA_ROOT_ENV, &root);

        reset_local_api_backpressure_for_tests();
        reset_active_stream_leases_for_tests();

        let upstream_listener = TcpListener::bind((super::CODEX_LOCAL_ACCESS_BIND_HOST, 0))
            .await
            .expect("fake upstream should bind");
        let upstream_addr = upstream_listener
            .local_addr()
            .expect("fake upstream should have addr");
        let upstream_server = tokio::spawn(async move {
            let (mut socket, _) = upstream_listener
                .accept()
                .await
                .expect("fake upstream should accept continuation");
            let mut request = Vec::new();
            let mut chunk = [0u8; 1024];
            loop {
                let read = socket
                    .read(&mut chunk)
                    .await
                    .expect("fake upstream should read continuation request");
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&chunk[..read]);
                if request.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            assert!(
                String::from_utf8_lossy(&request).contains("previous_response_id"),
                "continuation request should reach upstream"
            );
            let body = concat!(
                "event: response.created\n",
                "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_continuation_ok\",\"model\":\"gpt-5.5\",\"status\":\"in_progress\",\"output\":[]}}\n\n",
                "event: response.output_text.delta\n",
                "data: {\"type\":\"response.output_text.delta\",\"item_id\":\"msg_1\",\"output_index\":0,\"content_index\":0,\"delta\":\"OK\"}\n\n",
                "event: response.completed\n",
                "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_continuation_ok\",\"model\":\"gpt-5.5\",\"status\":\"completed\",\"output\":[],\"usage\":{\"input_tokens\":1,\"output_tokens\":1,\"total_tokens\":2}}}\n\n",
                "data: [DONE]\n\n"
            );
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.as_bytes().len(),
                body
            );
            socket
                .write_all(response.as_bytes())
                .await
                .expect("fake upstream should write continuation response");
        });

        let now = now_ms();
        let mut registry = empty_health_registry(now);
        registry.model_cooldowns.insert(
            health_registry_model_key("api-continuation", "gpt-5.5"),
            CodexLocalAccessModelCooldown {
                account_id: "api-continuation".to_string(),
                model: "gpt-5.5".to_string(),
                cooldown_until_ms: now + (7 * 24 * 60 * 60 * 1000),
                last_error_type: Some("usage_limit_reached".to_string()),
                last_request_id: Some("req-new-admission-exhausted".to_string()),
                updated_at: now,
            },
        );
        save_health_registry_to_path(&root.join(super::CODEX_LOCAL_ACCESS_HEALTH_FILE), &registry)
            .expect("health registry should be written to isolated test root");

        let listener = TcpListener::bind((super::CODEX_LOCAL_ACCESS_BIND_HOST, 0))
            .await
            .expect("test listener should bind");
        let addr = listener
            .local_addr()
            .expect("listener should have local addr");
        let collection: CodexLocalAccessCollection = serde_json::from_value(json!({
            "enabled": true,
            "port": addr.port(),
            "apiKey": "test-local-key",
            "safetyConfig": {
                "schemaVersion": 1,
                "hardenedLocalMode": true,
                "maxConcurrentRequests": 2,
                "minRequestIntervalSeconds": 0,
                "maxQueueWaitSeconds": 1,
                "requestTimeoutSeconds": 5,
                "maxRequestBodyMb": 64,
                "maxRetries": 1,
                "maxRetryAccounts": 1,
                "fallbackMode": "disabled",
                "logging": {
                    "redactSensitiveValues": true,
                    "includeRequestId": true,
                    "includeAccountHash": true,
                    "includeRoute": true,
                    "includeModel": true,
                    "includeLatency": true,
                    "includePromptResponse": false,
                    "includeRawUpstreamBody": false
                }
            },
            "routingStrategy": "auto",
            "restrictFreeAccounts": false,
            "followCurrentAccount": false,
            "accountIds": ["api-continuation"],
            "createdAt": now,
            "updatedAt": now
        }))
        .expect("collection fixture should deserialize");
        {
            let mut runtime = gateway_runtime().lock().await;
            *runtime = super::GatewayRuntime::default();
            runtime.loaded = true;
            runtime.collection = Some(collection);
            runtime.running = true;
            runtime.actual_port = Some(addr.port());
        }
        let account = CodexAccount::new_api_key(
            "api-continuation".to_string(),
            "api-continuation@example.com".to_string(),
            "sk-local-test".to_string(),
            CodexApiProviderMode::Custom,
            Some(format!("http://127.0.0.1:{}/v1", upstream_addr.port())),
            None,
            None,
        );
        cache_prepared_account(&account).await;

        let server = tokio::spawn(async move {
            let (stream, peer) = listener.accept().await.expect("server should accept");
            handle_connection(stream, peer).await
        });
        let mut client = TcpStream::connect(addr)
            .await
            .expect("client should connect");
        let body =
            br#"{"model":"gpt-5.5","previous_response_id":"resp_existing","input":"continue"}"#;
        let request = format!(
            "POST /v1/responses HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer test-local-key\r\nAccept: text/event-stream\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            String::from_utf8_lossy(body)
        );
        client
            .write_all(request.as_bytes())
            .await
            .expect("request should be written");

        let mut response = Vec::new();
        tokio::time::timeout(Duration::from_secs(2), client.read_to_end(&mut response))
            .await
            .expect("continuation stream should finish")
            .expect("response read should succeed");
        let response_text = String::from_utf8_lossy(&response);
        assert!(
            response_text.contains("HTTP/1.1 200 OK"),
            "expected upstream continuation response, got: {}",
            response_text
        );
        assert!(
            response_text.contains("resp_continuation_ok") && response_text.contains("OK"),
            "continuation must forward real upstream output instead of local pool text: {}",
            response_text
        );
        assert!(
            !response_text.contains("Cockpit API Service pool_unavailable"),
            "accepted continuation must not be converted to local pool_unavailable output: {}",
            response_text
        );

        let audit = fs::read_to_string(root.join(super::CODEX_LOCAL_ACCESS_AUDIT_FILE))
            .expect("audit should be written to isolated root");
        assert!(audit.contains("\"phase\":\"stream_completed\""));
        assert!(!audit.contains("\"outcome\":\"in_band_local_completion\""));
        assert!(!audit.contains("\"errorType\":\"pool_unavailable\""));

        let server_result = server.await.expect("server task should join");
        assert!(server_result.is_ok(), "server failed: {:?}", server_result);
        upstream_server
            .await
            .expect("fake upstream task should join");
        {
            let mut runtime = gateway_runtime().lock().await;
            *runtime = super::GatewayRuntime::default();
        }
        reset_local_api_backpressure_for_tests();
        reset_active_stream_leases_for_tests();
        match previous_root {
            Some(value) => std::env::set_var(super::CODEX_LOCAL_ACCESS_DATA_ROOT_ENV, value),
            None => std::env::remove_var(super::CODEX_LOCAL_ACCESS_DATA_ROOT_ENV),
        }
        let _ = fs::remove_dir_all(&root);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn codex_turn_metadata_lineage_does_not_pin_routing_account() {
        let _env_guard = LOCAL_ACCESS_ENV_TEST_LOCK
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let _backpressure_guard = LOCAL_BACKPRESSURE_TEST_LOCK
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let previous_root = std::env::var_os(super::CODEX_LOCAL_ACCESS_DATA_ROOT_ENV);
        let previous_accounts_root = std::env::var_os("COCKPIT_TOOLS_TEST_DATA_DIR");
        let root = std::env::temp_dir().join(format!(
            "cockpit-local-access-request-affinity-test-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let _ = fs::remove_dir_all(&root);
        std::env::set_var(super::CODEX_LOCAL_ACCESS_DATA_ROOT_ENV, &root);
        std::env::set_var("COCKPIT_TOOLS_TEST_DATA_DIR", &root);

        reset_local_api_backpressure_for_tests();
        reset_active_stream_leases_for_tests();

        let upstream_listener = TcpListener::bind((super::CODEX_LOCAL_ACCESS_BIND_HOST, 0))
            .await
            .expect("fake upstream should bind");
        let upstream_addr = upstream_listener
            .local_addr()
            .expect("fake upstream should have addr");
        let upstream_server = tokio::spawn(async move {
            let (mut socket, _) = upstream_listener
                .accept()
                .await
                .expect("fake upstream should accept metadata-only request");
            let mut request = Vec::new();
            let mut chunk = [0u8; 1024];
            loop {
                let read = socket
                    .read(&mut chunk)
                    .await
                    .expect("fake upstream should read continuation request");
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&chunk[..read]);
                if request.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            let request_text = String::from_utf8_lossy(&request);
            assert!(
                request_text.contains("X-Client-Request-Id: req-active-task")
                    || request_text.contains("x-client-request-id: req-active-task"),
                "request should preserve the thread request id: {}",
                request_text
            );
            assert!(
                request_text
                    .to_ascii_lowercase()
                    .contains("x-codex-turn-metadata: {\"turn_id\":\"turn-active\"}"),
                "request should preserve the Codex turn metadata: {}",
                request_text
            );
            assert!(
                !request_text.contains("previous_response_id"),
                "this regression covers turn metadata without previous_response_id: {}",
                request_text
            );
            let body = concat!(
                "event: response.created\n",
                "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_request_affinity_ok\",\"model\":\"gpt-5.5\",\"status\":\"in_progress\",\"output\":[]}}\n\n",
                "event: response.output_text.delta\n",
                "data: {\"type\":\"response.output_text.delta\",\"item_id\":\"msg_1\",\"output_index\":0,\"content_index\":0,\"delta\":\"OK\"}\n\n",
                "event: response.completed\n",
                "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_request_affinity_ok\",\"model\":\"gpt-5.5\",\"status\":\"completed\",\"output\":[],\"usage\":{\"input_tokens\":1,\"output_tokens\":1,\"total_tokens\":2}}}\n\n",
                "data: [DONE]\n\n"
            );
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.as_bytes().len(),
                body
            );
            socket
                .write_all(response.as_bytes())
                .await
                .expect("fake upstream should write response");
        });

        let now = now_ms();
        let registry = empty_health_registry(now);
        save_health_registry_to_path(&root.join(super::CODEX_LOCAL_ACCESS_HEALTH_FILE), &registry)
            .expect("health registry should be written to isolated test root");

        let listener = TcpListener::bind((super::CODEX_LOCAL_ACCESS_BIND_HOST, 0))
            .await
            .expect("test listener should bind");
        let addr = listener
            .local_addr()
            .expect("listener should have local addr");
        let collection: CodexLocalAccessCollection = serde_json::from_value(json!({
            "enabled": true,
            "port": addr.port(),
            "apiKey": "test-local-key",
            "safetyConfig": {
                "schemaVersion": 1,
                "hardenedLocalMode": true,
                "maxConcurrentRequests": 1,
                "minRequestIntervalSeconds": 1,
                "maxQueueWaitSeconds": 1,
                "requestTimeoutSeconds": 3,
                "maxRequestBodyMb": 64,
                "maxRetries": 1,
                "maxRetryAccounts": 1,
                "fallbackMode": "next_request_only",
                "logging": {
                    "redactSensitiveValues": true,
                    "includeRequestId": true,
                    "includeAccountHash": true,
                    "includeRoute": true,
                    "includeModel": true,
                    "includeLatency": true,
                    "includePromptResponse": false,
                    "includeRawUpstreamBody": false
                }
            },
            "routingStrategy": "auto",
            "restrictFreeAccounts": false,
            "followCurrentAccount": false,
            "accountIds": ["api-replacement"],
            "createdAt": now,
            "updatedAt": now
        }))
        .expect("collection fixture should deserialize");
        {
            let mut runtime = gateway_runtime().lock().await;
            *runtime = super::GatewayRuntime::default();
            runtime.loaded = true;
            runtime.collection = Some(collection);
            runtime.running = true;
            runtime.actual_port = Some(addr.port());
        }

        let account = CodexAccount::new_api_key(
            "api-replacement".to_string(),
            "api-replacement@example.com".to_string(),
            "sk-local-test".to_string(),
            CodexApiProviderMode::Custom,
            Some(format!("http://127.0.0.1:{}/v1", upstream_addr.port())),
            None,
            None,
        );
        crate::modules::codex_account::save_account(&account)
            .expect("pool account should be persisted");
        cache_prepared_account(&account).await;

        let server = tokio::spawn(async move {
            let (stream, peer) = listener.accept().await.expect("server should accept");
            handle_connection(stream, peer).await
        });
        let mut client = TcpStream::connect(addr)
            .await
            .expect("client should connect");
        let body = br#"{"model":"gpt-5.5","input":"continue task"}"#;
        let request = format!(
            "POST /v1/responses HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer test-local-key\r\nAccept: text/event-stream\r\nX-Client-Request-Id: req-active-task\r\nX-Codex-Turn-Metadata: {{\"turn_id\":\"turn-active\"}}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            String::from_utf8_lossy(body)
        );
        client
            .write_all(request.as_bytes())
            .await
            .expect("request should be written");

        let mut response = Vec::new();
        tokio::time::timeout(Duration::from_secs(2), client.read_to_end(&mut response))
            .await
            .expect("metadata-only request stream should finish")
            .expect("response read should succeed");
        let response_text = String::from_utf8_lossy(&response);
        assert!(
            response_text.contains("resp_request_affinity_ok") && response_text.contains("OK"),
            "metadata-only request must forward upstream output instead of local pool text: {}",
            response_text
        );
        assert!(
            !response_text.contains("Cockpit API Service pool_unavailable"),
            "metadata-only request must not be converted to local pool_unavailable output: {}",
            response_text
        );

        let audit = fs::read_to_string(root.join(super::CODEX_LOCAL_ACCESS_AUDIT_FILE))
            .expect("audit should be written to isolated root");
        assert!(audit.contains("\"request_id_source\":\"codex_turn_metadata_turn_id\""));
        assert!(audit.contains("\"phase\":\"stream_completed\""));
        assert!(!audit.contains("\"phase\":\"fallback_blocked\""));
        assert!(!audit.contains("\"outcome\":\"in_band_local_completion\""));
        assert!(!audit.contains("\"errorType\":\"pool_unavailable\""));

        let server_result = server.await.expect("server task should join");
        assert!(server_result.is_ok(), "server failed: {:?}", server_result);
        upstream_server
            .await
            .expect("fake upstream task should join");
        {
            let mut runtime = gateway_runtime().lock().await;
            *runtime = super::GatewayRuntime::default();
        }
        reset_local_api_backpressure_for_tests();
        reset_active_stream_leases_for_tests();
        match previous_root {
            Some(value) => std::env::set_var(super::CODEX_LOCAL_ACCESS_DATA_ROOT_ENV, value),
            None => std::env::remove_var(super::CODEX_LOCAL_ACCESS_DATA_ROOT_ENV),
        }
        match previous_accounts_root {
            Some(value) => std::env::set_var("COCKPIT_TOOLS_TEST_DATA_DIR", value),
            None => std::env::remove_var("COCKPIT_TOOLS_TEST_DATA_DIR"),
        }
        let _ = fs::remove_dir_all(&root);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn same_client_request_id_does_not_block_independent_fallback_after_usage_limit() {
        let _env_guard = LOCAL_ACCESS_ENV_TEST_LOCK
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let _backpressure_guard = LOCAL_BACKPRESSURE_TEST_LOCK
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let previous_root = std::env::var_os(super::CODEX_LOCAL_ACCESS_DATA_ROOT_ENV);
        let previous_accounts_root = std::env::var_os("COCKPIT_TOOLS_TEST_DATA_DIR");
        let root = std::env::temp_dir().join(format!(
            "cockpit-local-access-request-affinity-fallback-test-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let _ = fs::remove_dir_all(&root);
        std::env::set_var(super::CODEX_LOCAL_ACCESS_DATA_ROOT_ENV, &root);
        std::env::set_var("COCKPIT_TOOLS_TEST_DATA_DIR", &root);

        reset_local_api_backpressure_for_tests();
        reset_active_stream_leases_for_tests();

        let upstream_listener = TcpListener::bind((super::CODEX_LOCAL_ACCESS_BIND_HOST, 0))
            .await
            .expect("fake upstream should bind");
        let upstream_addr = upstream_listener
            .local_addr()
            .expect("fake upstream should have addr");
        let upstream_server = tokio::spawn(async move {
            let mut seen = Vec::new();
            let (mut socket, _) = upstream_listener
                .accept()
                .await
                .expect("fake upstream should accept affinity request");
            let mut request = Vec::new();
            let mut chunk = [0u8; 1024];
            loop {
                let read = socket
                    .read(&mut chunk)
                    .await
                    .expect("fake upstream should read request");
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&chunk[..read]);
                if request.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            let request_text = String::from_utf8_lossy(&request).to_string();
            seen.push(request_text.clone());
            assert!(
                request_text.contains("Authorization: Bearer sk-local-current")
                    || request_text.contains("authorization: Bearer sk-local-current"),
                "thread-scoped request id alone should not pin a new request to the old account: {}",
                request_text
            );
            assert!(
                !request_text.contains("Authorization: Bearer sk-local-old")
                    && !request_text.contains("authorization: Bearer sk-local-old"),
                "thread-scoped request id alone must allow the replacement account: {}",
                request_text
            );
            let body = concat!(
                "event: response.created\n",
                "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_request_affinity_fallback_ok\",\"model\":\"gpt-5.5\",\"status\":\"in_progress\",\"output\":[]}}\n\n",
                "event: response.output_text.delta\n",
                "data: {\"type\":\"response.output_text.delta\",\"item_id\":\"msg_1\",\"output_index\":0,\"content_index\":0,\"delta\":\"CURRENT\"}\n\n",
                "event: response.completed\n",
                "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_request_affinity_fallback_ok\",\"model\":\"gpt-5.5\",\"status\":\"completed\",\"output\":[],\"usage\":{\"input_tokens\":1,\"output_tokens\":1,\"total_tokens\":2}}}\n\n",
                "data: [DONE]\n\n"
            );
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.as_bytes().len(),
                body
            );
            socket
                .write_all(response.as_bytes())
                .await
                .expect("fake upstream should write response");
            seen
        });

        let now = now_ms();
        let registry = empty_health_registry(now);
        save_health_registry_to_path(&root.join(super::CODEX_LOCAL_ACCESS_HEALTH_FILE), &registry)
            .expect("health registry should be written to isolated test root");

        let listener = TcpListener::bind((super::CODEX_LOCAL_ACCESS_BIND_HOST, 0))
            .await
            .expect("test listener should bind");
        let addr = listener
            .local_addr()
            .expect("listener should have local addr");
        let collection: CodexLocalAccessCollection = serde_json::from_value(json!({
            "enabled": true,
            "port": addr.port(),
            "apiKey": "test-local-key",
            "safetyConfig": {
                "schemaVersion": 1,
                "hardenedLocalMode": true,
                "maxConcurrentRequests": 1,
                "minRequestIntervalSeconds": 1,
                "maxQueueWaitSeconds": 1,
                "requestTimeoutSeconds": 5,
                "maxRequestBodyMb": 64,
                "maxRetries": 1,
                "maxRetryAccounts": 2,
                "fallbackMode": "next_request_only",
                "logging": {
                    "redactSensitiveValues": true,
                    "includeRequestId": true,
                    "includeAccountHash": true,
                    "includeRoute": true,
                    "includeModel": true,
                    "includeLatency": true,
                    "includePromptResponse": false,
                    "includeRawUpstreamBody": false
                }
            },
            "routingStrategy": "auto",
            "restrictFreeAccounts": false,
            "followCurrentAccount": false,
            "accountIds": ["api-current"],
            "createdAt": now,
            "updatedAt": now
        }))
        .expect("collection fixture should deserialize");
        {
            let mut runtime = gateway_runtime().lock().await;
            *runtime = super::GatewayRuntime::default();
            runtime.loaded = true;
            runtime.collection = Some(collection);
            runtime.running = true;
            runtime.actual_port = Some(addr.port());
        }

        let old_account = CodexAccount::new_api_key(
            "api-old".to_string(),
            "api-old@example.com".to_string(),
            "sk-local-old".to_string(),
            CodexApiProviderMode::Custom,
            Some(format!("http://127.0.0.1:{}/v1", upstream_addr.port())),
            None,
            None,
        );
        let current_account = CodexAccount::new_api_key(
            "api-current".to_string(),
            "api-current@example.com".to_string(),
            "sk-local-current".to_string(),
            CodexApiProviderMode::Custom,
            Some(format!("http://127.0.0.1:{}/v1", upstream_addr.port())),
            None,
            None,
        );
        crate::modules::codex_account::save_account(&old_account)
            .expect("old affinity account should be persisted");
        crate::modules::codex_account::save_account(&current_account)
            .expect("current pool account should be persisted");

        let affinity_request = ParsedRequest {
            method: "POST".to_string(),
            target: "/v1/responses".to_string(),
            headers: HashMap::from([(
                "x-client-request-id".to_string(),
                "req-active-task-fallback".to_string(),
            )]),
            body: br#"{"model":"gpt-5.5","input":"previous successful task step"}"#.to_vec(),
            gateway_request_id: "gw-test-5".to_string(),
        };
        let mut persisted_registry =
            load_health_registry_from_path(&root.join(super::CODEX_LOCAL_ACCESS_HEALTH_FILE))
                .expect("health registry should load from isolated test root");
        assert_eq!(
            request_affinity_key(&affinity_request),
            None,
            "x-client-request-id is thread-scoped and must not become a hard turn affinity key"
        );
        assert!(!upsert_request_affinity_binding(
            &mut persisted_registry,
            &affinity_request,
            "api-old",
            now_ms(),
        ));
        save_health_registry_to_path(
            &root.join(super::CODEX_LOCAL_ACCESS_HEALTH_FILE),
            &persisted_registry,
        )
        .expect("persistent request affinity should be written");

        let server = tokio::spawn(async move {
            let (stream, peer) = listener.accept().await.expect("server should accept");
            handle_connection(stream, peer).await
        });
        let mut client = TcpStream::connect(addr)
            .await
            .expect("client should connect");
        let body = br#"{"model":"gpt-5.5","input":"continue task"}"#;
        let request = format!(
            "POST /v1/responses HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer test-local-key\r\nAccept: text/event-stream\r\nX-Client-Request-Id: req-active-task-fallback\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            String::from_utf8_lossy(body)
        );
        client
            .write_all(request.as_bytes())
            .await
            .expect("request should be written");

        let mut response = Vec::new();
        tokio::time::timeout(Duration::from_secs(3), client.read_to_end(&mut response))
            .await
            .expect("fallback stream should finish")
            .expect("response read should succeed");
        let response_text = String::from_utf8_lossy(&response);
        assert!(
            response_text.contains("resp_request_affinity_fallback_ok")
                && response_text.contains("CURRENT"),
            "independent request should immediately use the replacement account: {}",
            response_text
        );
        assert!(
            !response_text.contains("resp_cockpit_pool_unavailable_"),
            "thread-scoped request id must not trigger local pool_unavailable completion: {}",
            response_text
        );

        let audit = fs::read_to_string(root.join(super::CODEX_LOCAL_ACCESS_AUDIT_FILE))
            .expect("audit should be written to isolated root");
        assert!(audit.contains("\"request_id_source\":\"client_request_id\""));
        assert!(!audit.contains("\"phase\":\"fallback_blocked\""));
        assert!(!audit.contains("\"outcome\":\"hard_affinity\""));
        assert!(!audit.contains("\"outcome\":\"in_band_local_completion\""));
        assert!(audit.contains("\"phase\":\"stream_completed\""));

        let server_result = server.await.expect("server task should join");
        assert!(server_result.is_ok(), "server failed: {:?}", server_result);
        let seen = upstream_server
            .await
            .expect("fake upstream task should join");
        assert_eq!(seen.len(), 1);
        {
            let mut runtime = gateway_runtime().lock().await;
            *runtime = super::GatewayRuntime::default();
        }
        reset_local_api_backpressure_for_tests();
        reset_active_stream_leases_for_tests();
        match previous_root {
            Some(value) => std::env::set_var(super::CODEX_LOCAL_ACCESS_DATA_ROOT_ENV, value),
            None => std::env::remove_var(super::CODEX_LOCAL_ACCESS_DATA_ROOT_ENV),
        }
        match previous_accounts_root {
            Some(value) => std::env::set_var("COCKPIT_TOOLS_TEST_DATA_DIR", value),
            None => std::env::remove_var("COCKPIT_TOOLS_TEST_DATA_DIR"),
        }
        let _ = fs::remove_dir_all(&root);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn codex_turn_metadata_only_request_falls_back_after_old_account_429() {
        let _env_guard = LOCAL_ACCESS_ENV_TEST_LOCK
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let _backpressure_guard = LOCAL_BACKPRESSURE_TEST_LOCK
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let previous_root = std::env::var_os(super::CODEX_LOCAL_ACCESS_DATA_ROOT_ENV);
        let previous_accounts_root = std::env::var_os("COCKPIT_TOOLS_TEST_DATA_DIR");
        let root = std::env::temp_dir().join(format!(
            "cockpit-local-access-metadata-fallback-test-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let _ = fs::remove_dir_all(&root);
        std::env::set_var(super::CODEX_LOCAL_ACCESS_DATA_ROOT_ENV, &root);
        std::env::set_var("COCKPIT_TOOLS_TEST_DATA_DIR", &root);

        reset_local_api_backpressure_for_tests();
        reset_active_stream_leases_for_tests();

        let upstream_seen = std::sync::Arc::new(tokio::sync::Mutex::new(Vec::<String>::new()));
        let upstream_listener = TcpListener::bind((super::CODEX_LOCAL_ACCESS_BIND_HOST, 0))
            .await
            .expect("fake upstream should bind");
        let upstream_addr = upstream_listener
            .local_addr()
            .expect("fake upstream should have addr");
        let upstream_seen_task = std::sync::Arc::clone(&upstream_seen);
        let upstream_server = tokio::spawn(async move {
            for _ in 0..2 {
                let Ok(Ok((mut socket, _))) =
                    tokio::time::timeout(Duration::from_secs(2), upstream_listener.accept()).await
                else {
                    break;
                };
                let mut request = Vec::new();
                let mut chunk = [0u8; 1024];
                loop {
                    let read = socket
                        .read(&mut chunk)
                        .await
                        .expect("fake upstream should read request");
                    if read == 0 {
                        break;
                    }
                    request.extend_from_slice(&chunk[..read]);
                    if request.windows(4).any(|window| window == b"\r\n\r\n") {
                        break;
                    }
                }
                let request_text = String::from_utf8_lossy(&request).to_string();
                upstream_seen_task.lock().await.push(request_text.clone());

                assert!(
                    request_text
                        .to_ascii_lowercase()
                        .contains("x-codex-turn-metadata: {\"turn_id\":\"turn-live-metadata\"}"),
                    "metadata header should be forwarded unchanged: {}",
                    request_text
                );
                assert!(
                    !request_text
                        .to_ascii_lowercase()
                        .contains("x-codex-turn-state:"),
                    "fixture intentionally has metadata lineage without turn-state: {}",
                    request_text
                );

                if request_text.contains("Bearer sk-local-old") {
                    let body =
                        r#"{"error":{"type":"usage_limit_reached","code":"usage_limit_reached"}}"#;
                    let response = format!(
                        "HTTP/1.1 429 Too Many Requests\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.as_bytes().len(),
                        body
                    );
                    socket
                        .write_all(response.as_bytes())
                        .await
                        .expect("fake upstream should write old-account 429");
                    continue;
                }

                let body = concat!(
                    "event: response.created\n",
                    "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_metadata_fallback_ok\",\"model\":\"gpt-5.5\",\"status\":\"in_progress\",\"output\":[]}}\n\n",
                    "event: response.output_text.delta\n",
                    "data: {\"type\":\"response.output_text.delta\",\"item_id\":\"msg_1\",\"output_index\":0,\"content_index\":0,\"delta\":\"CURRENT\"}\n\n",
                    "event: response.completed\n",
                    "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_metadata_fallback_ok\",\"model\":\"gpt-5.5\",\"status\":\"completed\",\"output\":[],\"usage\":{\"input_tokens\":1,\"output_tokens\":1,\"total_tokens\":2}}}\n\n",
                    "data: [DONE]\n\n"
                );
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.as_bytes().len(),
                    body
                );
                socket
                    .write_all(response.as_bytes())
                    .await
                    .expect("fake upstream should write current-account response");
            }
        });

        let now = now_ms();
        let mut registry = empty_health_registry(now);
        assert!(upsert_process_sticky_binding(&mut registry, "api-old", now));
        save_health_registry_to_path(&root.join(super::CODEX_LOCAL_ACCESS_HEALTH_FILE), &registry)
            .expect("health registry should be written to isolated test root");

        let collection: CodexLocalAccessCollection = serde_json::from_value(json!({
            "enabled": true,
            "port": 0,
            "apiKey": "test-local-key",
            "safetyConfig": {
                "schemaVersion": 1,
                "hardenedLocalMode": true,
                "maxConcurrentRequests": 1,
                "minRequestIntervalSeconds": 0,
                "maxQueueWaitSeconds": 1,
                "requestTimeoutSeconds": 5,
                "maxRequestBodyMb": 64,
                "maxRetries": 1,
                "maxRetryAccounts": 2,
                "fallbackMode": "next_request_only",
                "logging": {
                    "redactSensitiveValues": true,
                    "includeRequestId": true,
                    "includeAccountHash": true,
                    "includeRoute": true,
                    "includeModel": true,
                    "includeLatency": true,
                    "includePromptResponse": false,
                    "includeRawUpstreamBody": false
                }
            },
            "routingStrategy": "auto",
            "restrictFreeAccounts": false,
            "followCurrentAccount": false,
            "accountIds": ["api-old", "api-current"],
            "createdAt": now,
            "updatedAt": now
        }))
        .expect("collection fixture should deserialize");
        {
            let mut runtime = gateway_runtime().lock().await;
            *runtime = super::GatewayRuntime::default();
            runtime.loaded = true;
            runtime.collection = Some(collection.clone());
            runtime.running = true;
        }

        let old_account = CodexAccount::new_api_key(
            "api-old".to_string(),
            "api-old@example.com".to_string(),
            "sk-local-old".to_string(),
            CodexApiProviderMode::Custom,
            Some(format!("http://127.0.0.1:{}/v1", upstream_addr.port())),
            None,
            None,
        );
        let current_account = CodexAccount::new_api_key(
            "api-current".to_string(),
            "api-current@example.com".to_string(),
            "sk-local-current".to_string(),
            CodexApiProviderMode::Custom,
            Some(format!("http://127.0.0.1:{}/v1", upstream_addr.port())),
            None,
            None,
        );
        crate::modules::codex_account::save_account(&old_account)
            .expect("old account should be persisted");
        crate::modules::codex_account::save_account(&current_account)
            .expect("current account should be persisted");

        let request = ParsedRequest {
            method: "POST".to_string(),
            target: "/v1/responses".to_string(),
            headers: HashMap::from([
                ("accept".to_string(), "text/event-stream".to_string()),
                ("content-type".to_string(), "application/json".to_string()),
                (
                    "x-client-request-id".to_string(),
                    "thread-request-id".to_string(),
                ),
                (
                    "x-codex-turn-metadata".to_string(),
                    r#"{"turn_id":"turn-live-metadata"}"#.to_string(),
                ),
            ]),
            body: br#"{"model":"gpt-5.5","input":"metadata-only continuation"}"#.to_vec(),
            gateway_request_id: "gw-test-5b".to_string(),
        };
        assert_eq!(
            request_affinity_key(&request),
            None,
            "metadata-only lineage must not activate hard affinity"
        );

        let success = proxy_request_with_account_pool(&request, &collection)
            .await
            .expect("metadata-only request should fail over to healthy account");
        assert_eq!(success.account_id, "api-current");
        let response_text = success
            .upstream
            .text()
            .await
            .expect("current-account response should be readable");

        let seen = upstream_seen.lock().await.clone();
        let seen_text = seen.join("\n--- request ---\n");
        let upstream_result = upstream_server.await;
        assert!(
            upstream_result.is_ok(),
            "fake upstream task should join: {:?}",
            upstream_result
        );

        {
            let mut runtime = gateway_runtime().lock().await;
            *runtime = super::GatewayRuntime::default();
        }
        reset_local_api_backpressure_for_tests();
        reset_active_stream_leases_for_tests();
        match previous_root {
            Some(value) => std::env::set_var(super::CODEX_LOCAL_ACCESS_DATA_ROOT_ENV, value),
            None => std::env::remove_var(super::CODEX_LOCAL_ACCESS_DATA_ROOT_ENV),
        }
        match previous_accounts_root {
            Some(value) => std::env::set_var("COCKPIT_TOOLS_TEST_DATA_DIR", value),
            None => std::env::remove_var("COCKPIT_TOOLS_TEST_DATA_DIR"),
        }

        assert!(
            seen.iter()
                .any(|request| request.contains("Bearer sk-local-old")),
            "first attempt should hit old account: {}",
            seen_text
        );
        assert!(
            seen.iter()
                .any(|request| request.contains("Bearer sk-local-current")),
            "fallback attempt should hit current account: {}",
            seen_text
        );
        assert!(
            response_text.contains("resp_metadata_fallback_ok")
                && response_text.contains("CURRENT"),
            "metadata-only fallback should return current-account response: {}",
            response_text
        );

        let audit = fs::read_to_string(root.join(super::CODEX_LOCAL_ACCESS_AUDIT_FILE))
            .expect("audit should be written to isolated root");
        assert!(audit.contains("\"request_id_source\":\"codex_turn_metadata_turn_id\""));
        assert!(audit.contains("\"phase\":\"fallback_selected\""));
        assert!(audit.contains("\"outcome\":\"next_account\""));
        assert!(!audit.contains("\"phase\":\"fallback_blocked\""));
        assert!(!audit.contains("\"outcome\":\"hard_affinity\""));
        assert!(!audit.contains("\"outcome\":\"in_band_local_completion\""));
        let _ = fs::remove_dir_all(&root);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn gateway_generated_turn_state_remains_hard_affinity_across_turn_requests() {
        let _env_guard = LOCAL_ACCESS_ENV_TEST_LOCK
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let _backpressure_guard = LOCAL_BACKPRESSURE_TEST_LOCK
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let previous_root = std::env::var_os(super::CODEX_LOCAL_ACCESS_DATA_ROOT_ENV);
        let previous_accounts_root = std::env::var_os("COCKPIT_TOOLS_TEST_DATA_DIR");
        let root = std::env::temp_dir().join(format!(
            "cockpit-local-access-generated-turn-state-test-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let _ = fs::remove_dir_all(&root);
        std::env::set_var(super::CODEX_LOCAL_ACCESS_DATA_ROOT_ENV, &root);
        std::env::set_var("COCKPIT_TOOLS_TEST_DATA_DIR", &root);

        reset_local_api_backpressure_for_tests();
        reset_active_stream_leases_for_tests();

        let upstream_seen = std::sync::Arc::new(tokio::sync::Mutex::new(Vec::<String>::new()));
        let upstream_listener = TcpListener::bind((super::CODEX_LOCAL_ACCESS_BIND_HOST, 0))
            .await
            .expect("fake upstream should bind");
        let upstream_addr = upstream_listener
            .local_addr()
            .expect("fake upstream should have addr");
        let upstream_seen_task = std::sync::Arc::clone(&upstream_seen);
        let upstream_server = tokio::spawn(async move {
            for _ in 0..2 {
                let (mut socket, _) = upstream_listener
                    .accept()
                    .await
                    .expect("fake upstream should accept");
                let mut request = Vec::new();
                let mut chunk = [0u8; 1024];
                loop {
                    let read = socket
                        .read(&mut chunk)
                        .await
                        .expect("fake upstream should read request");
                    if read == 0 {
                        break;
                    }
                    request.extend_from_slice(&chunk[..read]);
                    if request.windows(4).any(|window| window == b"\r\n\r\n") {
                        break;
                    }
                }
                let request_text = String::from_utf8_lossy(&request).to_string();
                upstream_seen_task.lock().await.push(request_text.clone());

                if request_text.contains("Bearer sk-local-old") {
                    if request_text
                        .to_ascii_lowercase()
                        .contains("x-codex-turn-state:")
                    {
                        let body = r#"{"error":{"type":"usage_limit_reached","code":"usage_limit_reached"}}"#;
                        let response = format!(
                            "HTTP/1.1 429 Too Many Requests\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                            body.as_bytes().len(),
                            body
                        );
                        socket
                            .write_all(response.as_bytes())
                            .await
                            .expect("fake upstream should write old-account followup 429");
                        continue;
                    }

                    let body = concat!(
                        "event: response.created\n",
                        "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_initial_generated_state\",\"model\":\"gpt-5.5\",\"status\":\"in_progress\",\"output\":[]}}\n\n",
                        "event: response.output_text.delta\n",
                        "data: {\"type\":\"response.output_text.delta\",\"item_id\":\"msg_1\",\"output_index\":0,\"content_index\":0,\"delta\":\"OLD-INITIAL\"}\n\n",
                        "event: response.completed\n",
                        "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_initial_generated_state\",\"model\":\"gpt-5.5\",\"status\":\"completed\",\"output\":[],\"usage\":{\"input_tokens\":1,\"output_tokens\":1,\"total_tokens\":2}}}\n\n",
                        "data: [DONE]\n\n"
                    );
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.as_bytes().len(),
                        body
                    );
                    socket
                        .write_all(response.as_bytes())
                        .await
                        .expect("fake upstream should write initial response without turn-state");
                    continue;
                }

                let body = concat!(
                    "event: response.created\n",
                    "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_current_after_soft_affinity\",\"model\":\"gpt-5.5\",\"status\":\"in_progress\",\"output\":[]}}\n\n",
                    "event: response.output_text.delta\n",
                    "data: {\"type\":\"response.output_text.delta\",\"item_id\":\"msg_1\",\"output_index\":0,\"content_index\":0,\"delta\":\"CURRENT\"}\n\n",
                    "event: response.completed\n",
                    "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_current_after_soft_affinity\",\"model\":\"gpt-5.5\",\"status\":\"completed\",\"output\":[],\"usage\":{\"input_tokens\":1,\"output_tokens\":1,\"total_tokens\":2}}}\n\n",
                    "data: [DONE]\n\n"
                );
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.as_bytes().len(),
                    body
                );
                socket
                    .write_all(response.as_bytes())
                    .await
                    .expect("fake upstream should write current-account response");
            }
        });

        let now = now_ms();
        let registry = empty_health_registry(now);
        save_health_registry_to_path(&root.join(super::CODEX_LOCAL_ACCESS_HEALTH_FILE), &registry)
            .expect("health registry should be written to isolated test root");

        let listener = TcpListener::bind((super::CODEX_LOCAL_ACCESS_BIND_HOST, 0))
            .await
            .expect("test listener should bind");
        let addr = listener
            .local_addr()
            .expect("listener should have local addr");
        let collection: CodexLocalAccessCollection = serde_json::from_value(json!({
            "enabled": true,
            "port": addr.port(),
            "apiKey": "test-local-key",
            "safetyConfig": {
                "schemaVersion": 1,
                "hardenedLocalMode": true,
                "maxConcurrentRequests": 2,
                "minRequestIntervalSeconds": 0,
                "maxQueueWaitSeconds": 1,
                "requestTimeoutSeconds": 5,
                "maxRequestBodyMb": 64,
                "maxRetries": 1,
                "maxRetryAccounts": 2,
                "fallbackMode": "next_request_only",
                "logging": {
                    "redactSensitiveValues": true,
                    "includeRequestId": true,
                    "includeAccountHash": true,
                    "includeRoute": true,
                    "includeModel": true,
                    "includeLatency": true,
                    "includePromptResponse": false,
                    "includeRawUpstreamBody": false
                }
            },
            "routingStrategy": "auto",
            "restrictFreeAccounts": false,
            "followCurrentAccount": false,
            "accountIds": ["api-old"],
            "createdAt": now,
            "updatedAt": now
        }))
        .expect("collection fixture should deserialize");
        {
            let mut runtime = gateway_runtime().lock().await;
            *runtime = super::GatewayRuntime::default();
            runtime.loaded = true;
            runtime.collection = Some(collection);
            runtime.running = true;
            runtime.actual_port = Some(addr.port());
        }

        let old_account = CodexAccount::new_api_key(
            "api-old".to_string(),
            "api-old@example.com".to_string(),
            "sk-local-old".to_string(),
            CodexApiProviderMode::Custom,
            Some(format!("http://127.0.0.1:{}/v1", upstream_addr.port())),
            None,
            None,
        );
        let current_account = CodexAccount::new_api_key(
            "api-current".to_string(),
            "api-current@example.com".to_string(),
            "sk-local-current".to_string(),
            CodexApiProviderMode::Custom,
            Some(format!("http://127.0.0.1:{}/v1", upstream_addr.port())),
            None,
            None,
        );
        crate::modules::codex_account::save_account(&old_account)
            .expect("old account should be persisted");
        crate::modules::codex_account::save_account(&current_account)
            .expect("current account should be persisted");

        let server = tokio::spawn(async move {
            let mut handlers = Vec::new();
            for _ in 0..2 {
                let (stream, peer) = listener.accept().await.expect("server should accept");
                handlers.push(tokio::spawn(async move {
                    handle_connection(stream, peer).await
                }));
            }
            for handler in handlers {
                handler.await.expect("server connection task should join")?;
            }
            Ok::<(), String>(())
        });

        let mut first_client = TcpStream::connect(addr)
            .await
            .expect("first client should connect");
        let first_body = br#"{"model":"gpt-5.5","input":"start turn"}"#;
        let first_request = format!(
            "POST /v1/responses HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer test-local-key\r\nAccept: text/event-stream\r\nX-Client-Request-Id: thread-generated-state\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            first_body.len(),
            String::from_utf8_lossy(first_body)
        );
        first_client
            .write_all(first_request.as_bytes())
            .await
            .expect("first request should be written");
        let mut first_response = Vec::new();
        tokio::time::timeout(
            Duration::from_secs(3),
            first_client.read_to_end(&mut first_response),
        )
        .await
        .expect("first response should finish")
        .expect("first response read should succeed");
        let first_response_text = String::from_utf8_lossy(&first_response);
        assert!(
            first_response_text.contains("resp_initial_generated_state"),
            "initial response should be forwarded: {}",
            first_response_text
        );
        let parsed_first = parse_http_request(&first_response)
            .expect("gateway response headers should parse like an HTTP message");
        let turn_state = parsed_first
            .headers
            .get("x-codex-turn-state")
            .cloned()
            .expect("gateway must synthesize x-codex-turn-state for Codex clients");
        assert!(
            !turn_state.trim().is_empty(),
            "generated turn-state must be non-empty"
        );

        {
            let mut runtime = gateway_runtime().lock().await;
            let collection = runtime
                .collection
                .as_mut()
                .expect("runtime collection should still be available");
            if !collection
                .account_ids
                .iter()
                .any(|account_id| account_id == "api-current")
            {
                collection.account_ids.push("api-current".to_string());
            }
        }

        let mut second_client = TcpStream::connect(addr)
            .await
            .expect("second client should connect");
        let second_body = br#"{"model":"gpt-5.5","input":"continue same turn"}"#;
        let second_request = format!(
            "POST /v1/responses HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer test-local-key\r\nAccept: text/event-stream\r\nX-Client-Request-Id: thread-generated-state\r\nX-Codex-Turn-State: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            turn_state,
            second_body.len(),
            String::from_utf8_lossy(second_body)
        );
        second_client
            .write_all(second_request.as_bytes())
            .await
            .expect("second request should be written");
        let mut second_response = Vec::new();
        tokio::time::timeout(
            Duration::from_secs(3),
            second_client.read_to_end(&mut second_response),
        )
        .await
        .expect("second response should finish")
        .expect("second response read should succeed");
        let second_response_text = String::from_utf8_lossy(&second_response);

        let server_result = server.await.expect("server task should join");
        assert!(server_result.is_ok(), "server failed: {:?}", server_result);
        upstream_server
            .await
            .expect("fake upstream task should join");

        let seen = upstream_seen.lock().await.clone();
        let seen_text = seen.join("\n--- request ---\n");
        let audit =
            fs::read_to_string(root.join(super::CODEX_LOCAL_ACCESS_AUDIT_FILE)).unwrap_or_default();
        {
            let mut runtime = gateway_runtime().lock().await;
            *runtime = super::GatewayRuntime::default();
        }
        reset_local_api_backpressure_for_tests();
        reset_active_stream_leases_for_tests();
        match previous_root {
            Some(value) => std::env::set_var(super::CODEX_LOCAL_ACCESS_DATA_ROOT_ENV, value),
            None => std::env::remove_var(super::CODEX_LOCAL_ACCESS_DATA_ROOT_ENV),
        }
        match previous_accounts_root {
            Some(value) => std::env::set_var("COCKPIT_TOOLS_TEST_DATA_DIR", value),
            None => std::env::remove_var("COCKPIT_TOOLS_TEST_DATA_DIR"),
        }
        let _ = fs::remove_dir_all(&root);

        assert!(
            second_response_text.contains("HTTP/1.1 429 Too Many Requests")
                && second_response_text.contains("usage_limit_reached"),
            "same-turn followup must preserve original-account quota failure instead of consuming a replacement account: {}",
            second_response_text
        );
        assert!(
            seen.iter()
                .any(|request| request.contains("Bearer sk-local-old")
                    && !request.to_ascii_lowercase().contains("x-codex-turn-state:")),
            "initial request should use old account without incoming turn-state: {}",
            seen_text
        );
        assert!(
            seen.iter()
                .any(|request| request.contains("Bearer sk-local-old")
                    && request.to_ascii_lowercase().contains("x-codex-turn-state:")),
            "same-turn followup should use the original account: {}",
            seen_text
        );
        assert!(
            !seen
                .iter()
                .any(|request| request.contains("Bearer sk-local-current")),
            "same-turn followup must not consume the replacement account: {}",
            seen_text
        );

        assert!(audit.contains("\"phase\":\"fallback_blocked\""));
        assert!(audit.contains("\"outcome\":\"hard_affinity\""));
        assert!(audit.contains("\"hard_affinity_bound\":\"true\""));
        assert!(!audit.contains("\"phase\":\"fallback_selected\""));
        assert!(!audit.contains("\"outcome\":\"in_band_local_completion\""));
        assert!(
            !audit.contains(turn_state.as_str()),
            "raw generated turn-state must not be written to audit"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn previous_response_id_hard_affinity_blocks_fallback_after_usage_limit() {
        let _env_guard = LOCAL_ACCESS_ENV_TEST_LOCK
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let _backpressure_guard = LOCAL_BACKPRESSURE_TEST_LOCK
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let previous_root = std::env::var_os(super::CODEX_LOCAL_ACCESS_DATA_ROOT_ENV);
        let previous_accounts_root = std::env::var_os("COCKPIT_TOOLS_TEST_DATA_DIR");
        let root = std::env::temp_dir().join(format!(
            "cockpit-local-access-previous-response-fallback-test-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let _ = fs::remove_dir_all(&root);
        std::env::set_var(super::CODEX_LOCAL_ACCESS_DATA_ROOT_ENV, &root);
        std::env::set_var("COCKPIT_TOOLS_TEST_DATA_DIR", &root);

        reset_local_api_backpressure_for_tests();
        reset_active_stream_leases_for_tests();

        let upstream_listener = TcpListener::bind((super::CODEX_LOCAL_ACCESS_BIND_HOST, 0))
            .await
            .expect("fake upstream should bind");
        let upstream_addr = upstream_listener
            .local_addr()
            .expect("fake upstream should have addr");
        let upstream_server = tokio::spawn(async move {
            let (mut socket, _) = upstream_listener
                .accept()
                .await
                .expect("fake upstream should accept continuation request");
            let mut request = Vec::new();
            let mut chunk = [0u8; 1024];
            loop {
                let read = socket
                    .read(&mut chunk)
                    .await
                    .expect("fake upstream should read request");
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&chunk[..read]);
                if request.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            let request_text = String::from_utf8_lossy(&request).to_string();
            assert!(
                request_text.contains("Authorization: Bearer sk-local-old")
                    || request_text.contains("authorization: Bearer sk-local-old"),
                "previous_response_id affinity should use the original account: {}",
                request_text
            );
            assert!(
                !request_text.contains("Authorization: Bearer sk-local-current")
                    && !request_text.contains("authorization: Bearer sk-local-current"),
                "previous_response_id affinity must not switch to the current pool account: {}",
                request_text
            );
            assert!(
                request_text.contains("\"previous_response_id\":\"resp-prev-hard\""),
                "previous_response_id must be forwarded to upstream body: {}",
                request_text
            );
            let body = r#"{"error":{"type":"usage_limit_reached","code":"usage_limit_reached"}}"#;
            let response = format!(
                "HTTP/1.1 429 Too Many Requests\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.as_bytes().len(),
                body
            );
            socket
                .write_all(response.as_bytes())
                .await
                .expect("fake upstream should write 429");
            request_text
        });

        let now = now_ms();
        let registry = empty_health_registry(now);
        save_health_registry_to_path(&root.join(super::CODEX_LOCAL_ACCESS_HEALTH_FILE), &registry)
            .expect("health registry should be written to isolated test root");

        let collection: CodexLocalAccessCollection = serde_json::from_value(json!({
            "enabled": true,
            "port": 0,
            "apiKey": "test-local-key",
            "safetyConfig": {
                "schemaVersion": 1,
                "hardenedLocalMode": true,
                "maxConcurrentRequests": 1,
                "minRequestIntervalSeconds": 1,
                "maxQueueWaitSeconds": 1,
                "requestTimeoutSeconds": 5,
                "maxRequestBodyMb": 64,
                "maxRetries": 1,
                "maxRetryAccounts": 2,
                "fallbackMode": "next_request_only",
                "logging": {
                    "redactSensitiveValues": true,
                    "includeRequestId": true,
                    "includeAccountHash": true,
                    "includeRoute": true,
                    "includeModel": true,
                    "includeLatency": true,
                    "includePromptResponse": false,
                    "includeRawUpstreamBody": false
                }
            },
            "routingStrategy": "auto",
            "restrictFreeAccounts": false,
            "followCurrentAccount": false,
            "accountIds": ["api-current"],
            "createdAt": now,
            "updatedAt": now
        }))
        .expect("collection fixture should deserialize");
        {
            let mut runtime = gateway_runtime().lock().await;
            *runtime = super::GatewayRuntime::default();
            runtime.loaded = true;
            runtime.collection = Some(collection.clone());
            runtime.running = true;
        }

        let old_account = CodexAccount::new_api_key(
            "api-old".to_string(),
            "api-old@example.com".to_string(),
            "sk-local-old".to_string(),
            CodexApiProviderMode::Custom,
            Some(format!("http://127.0.0.1:{}/v1", upstream_addr.port())),
            None,
            None,
        );
        let current_account = CodexAccount::new_api_key(
            "api-current".to_string(),
            "api-current@example.com".to_string(),
            "sk-local-current".to_string(),
            CodexApiProviderMode::Custom,
            Some(format!("http://127.0.0.1:{}/v1", upstream_addr.port())),
            None,
            None,
        );
        crate::modules::codex_account::save_account(&old_account)
            .expect("old affinity account should be persisted");
        crate::modules::codex_account::save_account(&current_account)
            .expect("current pool account should be persisted");
        cache_prepared_account(&old_account).await;

        let previous_response_id = "resp-prev-hard";
        bind_response_affinity(previous_response_id, "api-old").await;

        let request = ParsedRequest {
            method: "POST".to_string(),
            target: "/v1/responses".to_string(),
            headers: HashMap::from([
                ("accept".to_string(), "text/event-stream".to_string()),
                ("content-type".to_string(), "application/json".to_string()),
            ]),
            body: format!(
                r#"{{"model":"gpt-5.5","previous_response_id":"{}","input":"continue previous response"}}"#,
                previous_response_id
            )
            .into_bytes(),
            gateway_request_id: "gw-test-7".to_string(),
        };
        let err = proxy_request_with_account_pool(&request, &collection)
            .await
            .expect_err(
                "previous_response_id hard affinity should not fall back to replacement account",
            );
        assert_eq!(err.account_id.as_deref(), Some("api-old"));
        assert_eq!(err.status, StatusCode::TOO_MANY_REQUESTS.as_u16());
        assert_eq!(
            proxy_dispatch_final_error_type(&request, err.status, err.message.as_str()),
            "usage_limit_reached"
        );
        let final_detail = proxy_dispatch_final_error_detail(
            &request,
            err.status,
            err.message.as_str(),
            err.retry_after,
            123,
        );
        assert_eq!(
            final_detail.get("provider_code").map(String::as_str),
            Some("usage_limit_reached")
        );
        assert_eq!(
            final_detail.get("terminal_origin").map(String::as_str),
            Some("upstream_quota_error")
        );
        assert_eq!(
            final_detail.get("sticky_boundary").map(String::as_str),
            Some("previous_response_id")
        );

        let seen = upstream_server
            .await
            .expect("fake upstream task should join");
        assert!(seen.contains("sk-local-old"));

        let audit = fs::read_to_string(root.join(super::CODEX_LOCAL_ACCESS_AUDIT_FILE))
            .expect("audit should be written to isolated root");
        assert!(audit.contains("\"phase\":\"fallback_blocked\""));
        assert!(audit.contains("\"outcome\":\"hard_affinity\""));
        assert!(audit.contains("\"previous_response_id_hash\":\"response:sha256:"));
        assert!(
            !audit.contains(previous_response_id),
            "raw previous_response_id must not be written to audit"
        );
        assert!(!audit.contains("\"phase\":\"fallback_selected\""));

        {
            let mut runtime = gateway_runtime().lock().await;
            *runtime = super::GatewayRuntime::default();
        }
        reset_local_api_backpressure_for_tests();
        reset_active_stream_leases_for_tests();
        match previous_root {
            Some(value) => std::env::set_var(super::CODEX_LOCAL_ACCESS_DATA_ROOT_ENV, value),
            None => std::env::remove_var(super::CODEX_LOCAL_ACCESS_DATA_ROOT_ENV),
        }
        match previous_accounts_root {
            Some(value) => std::env::set_var("COCKPIT_TOOLS_TEST_DATA_DIR", value),
            None => std::env::remove_var("COCKPIT_TOOLS_TEST_DATA_DIR"),
        }
        let _ = fs::remove_dir_all(&root);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn previous_response_id_short_retry_after_retries_original_account_to_completion() {
        let _env_guard = LOCAL_ACCESS_ENV_TEST_LOCK
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let _backpressure_guard = LOCAL_BACKPRESSURE_TEST_LOCK
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let previous_root = std::env::var_os(super::CODEX_LOCAL_ACCESS_DATA_ROOT_ENV);
        let previous_accounts_root = std::env::var_os("COCKPIT_TOOLS_TEST_DATA_DIR");
        let root = std::env::temp_dir().join(format!(
            "cockpit-local-access-previous-response-retry-test-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let _ = fs::remove_dir_all(&root);
        std::env::set_var(super::CODEX_LOCAL_ACCESS_DATA_ROOT_ENV, &root);
        std::env::set_var("COCKPIT_TOOLS_TEST_DATA_DIR", &root);

        reset_local_api_backpressure_for_tests();
        reset_active_stream_leases_for_tests();

        let upstream_listener = TcpListener::bind((super::CODEX_LOCAL_ACCESS_BIND_HOST, 0))
            .await
            .expect("fake upstream should bind");
        let upstream_addr = upstream_listener
            .local_addr()
            .expect("fake upstream should have addr");
        let upstream_server = tokio::spawn(async move {
            let mut seen = Vec::new();
            for attempt in 0..2 {
                let (mut socket, _) = upstream_listener
                    .accept()
                    .await
                    .expect("fake upstream should accept retry request");
                let mut request = Vec::new();
                let mut chunk = [0u8; 1024];
                loop {
                    let read = socket
                        .read(&mut chunk)
                        .await
                        .expect("fake upstream should read request");
                    if read == 0 {
                        break;
                    }
                    request.extend_from_slice(&chunk[..read]);
                    if request.windows(4).any(|window| window == b"\r\n\r\n") {
                        break;
                    }
                }
                let request_text = String::from_utf8_lossy(&request).to_string();
                seen.push(request_text);

                if attempt == 0 {
                    let body =
                        r#"{"error":{"type":"usage_limit_reached","code":"usage_limit_reached"}}"#;
                    let response = format!(
                        "HTTP/1.1 429 Too Many Requests\r\nContent-Type: application/json\r\nRetry-After-Ms: 1\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.as_bytes().len(),
                        body
                    );
                    socket
                        .write_all(response.as_bytes())
                        .await
                        .expect("fake upstream should write retryable 429");
                } else {
                    let body = concat!(
                        "event: response.created\r\n",
                        "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_retry_ok\"}}\r\n\r\n",
                        "event: response.completed\r\n",
                        "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_retry_ok\",\"usage\":null}}\r\n\r\n",
                        "data: [DONE]\r\n\r\n"
                    );
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.as_bytes().len(),
                        body
                    );
                    socket
                        .write_all(response.as_bytes())
                        .await
                        .expect("fake upstream should write completed stream");
                }
            }
            seen
        });

        let now = now_ms();
        let registry = empty_health_registry(now);
        save_health_registry_to_path(&root.join(super::CODEX_LOCAL_ACCESS_HEALTH_FILE), &registry)
            .expect("health registry should be written to isolated test root");

        let collection: CodexLocalAccessCollection = serde_json::from_value(json!({
            "enabled": true,
            "port": 0,
            "apiKey": "test-local-key",
            "safetyConfig": {
                "schemaVersion": 1,
                "hardenedLocalMode": true,
                "maxConcurrentRequests": 1,
                "minRequestIntervalSeconds": 1,
                "maxQueueWaitSeconds": 1,
                "requestTimeoutSeconds": 5,
                "maxRequestBodyMb": 64,
                "maxRetries": 1,
                "maxRetryAccounts": 2,
                "fallbackMode": "next_request_only",
                "logging": {
                    "redactSensitiveValues": true,
                    "includeRequestId": true,
                    "includeAccountHash": true,
                    "includeRoute": true,
                    "includeModel": true,
                    "includeLatency": true,
                    "includePromptResponse": false,
                    "includeRawUpstreamBody": false
                }
            },
            "routingStrategy": "auto",
            "restrictFreeAccounts": false,
            "followCurrentAccount": false,
            "accountIds": ["api-current"],
            "createdAt": now,
            "updatedAt": now
        }))
        .expect("collection fixture should deserialize");
        {
            let mut runtime = gateway_runtime().lock().await;
            *runtime = super::GatewayRuntime::default();
            runtime.loaded = true;
            runtime.collection = Some(collection.clone());
            runtime.running = true;
        }

        let old_account = CodexAccount::new_api_key(
            "api-old".to_string(),
            "api-old@example.com".to_string(),
            "sk-local-old".to_string(),
            CodexApiProviderMode::Custom,
            Some(format!("http://127.0.0.1:{}/v1", upstream_addr.port())),
            None,
            None,
        );
        let current_account = CodexAccount::new_api_key(
            "api-current".to_string(),
            "api-current@example.com".to_string(),
            "sk-local-current".to_string(),
            CodexApiProviderMode::Custom,
            Some(format!("http://127.0.0.1:{}/v1", upstream_addr.port())),
            None,
            None,
        );
        crate::modules::codex_account::save_account(&old_account)
            .expect("old affinity account should be persisted");
        crate::modules::codex_account::save_account(&current_account)
            .expect("current pool account should be persisted");
        cache_prepared_account(&old_account).await;

        let previous_response_id = "resp-prev-retry";
        bind_response_affinity(previous_response_id, "api-old").await;

        let request = ParsedRequest {
            method: "POST".to_string(),
            target: "/v1/responses".to_string(),
            headers: HashMap::from([
                ("accept".to_string(), "text/event-stream".to_string()),
                ("content-type".to_string(), "application/json".to_string()),
            ]),
            body: format!(
                r#"{{"model":"gpt-5.5","previous_response_id":"{}","input":"continue previous response"}}"#,
                previous_response_id
            )
            .into_bytes(),
            gateway_request_id: "gw-test-7b".to_string(),
        };
        let success = proxy_request_with_account_pool(&request, &collection)
            .await
            .expect(
                "previous_response_id hard affinity should retry the original account after a short reset",
            );
        assert_eq!(success.account_id, "api-old");
        let response_text = success
            .upstream
            .text()
            .await
            .expect("completed upstream response should be readable");
        assert!(
            response_text.contains("response.completed") && response_text.contains("resp_retry_ok"),
            "short hard-affinity reset should recover to a real upstream completion: {}",
            response_text
        );

        let seen = upstream_server
            .await
            .expect("fake upstream task should join");
        let seen_text = seen.join("\n--- request ---\n");
        assert_eq!(seen.len(), 2);
        assert!(
            seen.iter()
                .all(|request| request.contains("Bearer sk-local-old")),
            "both attempts must stay on the original account: {}",
            seen_text
        );
        assert!(
            !seen_text.contains("Bearer sk-local-current"),
            "hard-affinity retry must not consume the replacement account: {}",
            seen_text
        );

        let audit = fs::read_to_string(root.join(super::CODEX_LOCAL_ACCESS_AUDIT_FILE))
            .expect("audit should be written to isolated root");
        assert!(audit.contains("\"phase\":\"fallback_blocked\""));
        assert!(audit.contains("\"outcome\":\"hard_affinity\""));
        assert!(audit.contains("\"phase\":\"pool_wait\""));
        assert!(audit.contains("\"reason\":\"hard_affinity_same_account_retry\""));
        assert!(audit.contains("\"outcome\":\"sleeping\""));
        assert!(audit.contains("\"outcome\":\"retrying\""));
        assert!(!audit.contains("\"phase\":\"fallback_selected\""));

        {
            let mut runtime = gateway_runtime().lock().await;
            *runtime = super::GatewayRuntime::default();
        }
        reset_local_api_backpressure_for_tests();
        reset_active_stream_leases_for_tests();
        match previous_root {
            Some(value) => std::env::set_var(super::CODEX_LOCAL_ACCESS_DATA_ROOT_ENV, value),
            None => std::env::remove_var(super::CODEX_LOCAL_ACCESS_DATA_ROOT_ENV),
        }
        match previous_accounts_root {
            Some(value) => std::env::set_var("COCKPIT_TOOLS_TEST_DATA_DIR", value),
            None => std::env::remove_var("COCKPIT_TOOLS_TEST_DATA_DIR"),
        }
        let _ = fs::remove_dir_all(&root);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn hard_affinity_followup_retries_short_reset_on_original_account() {
        let _env_guard = LOCAL_ACCESS_ENV_TEST_LOCK
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let _backpressure_guard = LOCAL_BACKPRESSURE_TEST_LOCK
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let previous_root = std::env::var_os(super::CODEX_LOCAL_ACCESS_DATA_ROOT_ENV);
        let previous_accounts_root = std::env::var_os("COCKPIT_TOOLS_TEST_DATA_DIR");
        let root = std::env::temp_dir().join(format!(
            "cockpit-local-access-hard-affinity-timeout-test-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let _ = fs::remove_dir_all(&root);
        std::env::set_var(super::CODEX_LOCAL_ACCESS_DATA_ROOT_ENV, &root);
        std::env::set_var("COCKPIT_TOOLS_TEST_DATA_DIR", &root);

        reset_local_api_backpressure_for_tests();
        reset_active_stream_leases_for_tests();

        let upstream_listener = TcpListener::bind((super::CODEX_LOCAL_ACCESS_BIND_HOST, 0))
            .await
            .expect("fake upstream should bind");
        let upstream_addr = upstream_listener
            .local_addr()
            .expect("fake upstream should have addr");
        let upstream_server = tokio::spawn(async move {
            let mut seen = Vec::new();
            for attempt in 0..2 {
                let accepted = if attempt == 0 {
                    Some(
                        upstream_listener
                            .accept()
                            .await
                            .expect("fake upstream should accept first request"),
                    )
                } else {
                    match tokio::time::timeout(Duration::from_secs(2), upstream_listener.accept())
                        .await
                    {
                        Ok(Ok(pair)) => Some(pair),
                        _ => None,
                    }
                };
                let Some((mut socket, _)) = accepted else {
                    break;
                };
                let mut request = Vec::new();
                let mut chunk = [0u8; 1024];
                loop {
                    let read = socket
                        .read(&mut chunk)
                        .await
                        .expect("fake upstream should read request");
                    if read == 0 {
                        break;
                    }
                    request.extend_from_slice(&chunk[..read]);
                    if request.windows(4).any(|window| window == b"\r\n\r\n") {
                        break;
                    }
                }
                let request_text = String::from_utf8_lossy(&request).to_string();
                assert!(
                    request_text.contains("Bearer sk-local-old"),
                    "hard-affinity followup must use original account: {}",
                    request_text
                );
                assert!(
                    !request_text.contains("Bearer sk-local-current"),
                    "hard-affinity followup must not switch accounts: {}",
                    request_text
                );
                assert!(
                    request_text.contains("\"previous_response_id\":\"resp-prev-wait\""),
                    "previous_response_id must be preserved in forwarded body: {}",
                    request_text
                );
                seen.push(request_text);

                if attempt == 0 {
                    let body =
                        r#"{"error":{"type":"usage_limit_reached","code":"usage_limit_reached"}}"#;
                    let response = format!(
                        "HTTP/1.1 429 Too Many Requests\r\nContent-Type: application/json\r\nRetry-After-Ms: 1100\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.as_bytes().len(),
                        body
                    );
                    socket
                        .write_all(response.as_bytes())
                        .await
                        .expect("fake upstream should write retryable 429");
                } else {
                    let body = concat!(
                        "event: response.created\r\n",
                        "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_long_wait_ok\"}}\r\n\r\n",
                        "event: response.output_item.done\r\n",
                        "data: {\"type\":\"response.output_item.done\",\"item\":{\"type\":\"compaction_summary\",\"encrypted_content\":\"compact-ok\"}}\r\n\r\n",
                        "event: response.completed\r\n",
                        "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_long_wait_ok\",\"usage\":null}}\r\n\r\n",
                        "data: [DONE]\r\n\r\n"
                    );
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.as_bytes().len(),
                        body
                    );
                    socket
                        .write_all(response.as_bytes())
                        .await
                        .expect("fake upstream should write completed stream");
                }
            }
            seen
        });

        let now = now_ms();
        let registry = empty_health_registry(now);
        save_health_registry_to_path(&root.join(super::CODEX_LOCAL_ACCESS_HEALTH_FILE), &registry)
            .expect("health registry should be written to isolated test root");

        let listener = TcpListener::bind((super::CODEX_LOCAL_ACCESS_BIND_HOST, 0))
            .await
            .expect("test listener should bind");
        let addr = listener
            .local_addr()
            .expect("listener should have local addr");
        let collection: CodexLocalAccessCollection = serde_json::from_value(json!({
            "enabled": true,
            "port": addr.port(),
            "apiKey": "test-local-key",
            "safetyConfig": {
                "schemaVersion": 1,
                "hardenedLocalMode": true,
                "maxConcurrentRequests": 2,
                "minRequestIntervalSeconds": 0,
                "maxQueueWaitSeconds": 1,
                "requestTimeoutSeconds": 1,
                "maxRequestBodyMb": 64,
                "maxRetries": 1,
                "maxRetryAccounts": 2,
                "fallbackMode": "next_request_only",
                "logging": {
                    "redactSensitiveValues": true,
                    "includeRequestId": true,
                    "includeAccountHash": true,
                    "includeRoute": true,
                    "includeModel": true,
                    "includeLatency": true,
                    "includePromptResponse": false,
                    "includeRawUpstreamBody": false
                }
            },
            "routingStrategy": "auto",
            "restrictFreeAccounts": false,
            "followCurrentAccount": false,
            "accountIds": ["api-current"],
            "createdAt": now,
            "updatedAt": now
        }))
        .expect("collection fixture should deserialize");
        {
            let mut runtime = gateway_runtime().lock().await;
            *runtime = super::GatewayRuntime::default();
            runtime.loaded = true;
            runtime.collection = Some(collection);
            runtime.running = true;
            runtime.actual_port = Some(addr.port());
        }

        let old_account = CodexAccount::new_api_key(
            "api-old".to_string(),
            "api-old@example.com".to_string(),
            "sk-local-old".to_string(),
            CodexApiProviderMode::Custom,
            Some(format!("http://127.0.0.1:{}/v1", upstream_addr.port())),
            None,
            None,
        );
        let current_account = CodexAccount::new_api_key(
            "api-current".to_string(),
            "api-current@example.com".to_string(),
            "sk-local-current".to_string(),
            CodexApiProviderMode::Custom,
            Some(format!("http://127.0.0.1:{}/v1", upstream_addr.port())),
            None,
            None,
        );
        crate::modules::codex_account::save_account(&old_account)
            .expect("old affinity account should be persisted");
        crate::modules::codex_account::save_account(&current_account)
            .expect("current pool account should be persisted");
        cache_prepared_account(&old_account).await;

        let previous_response_id = "resp-prev-wait";
        bind_response_affinity(previous_response_id, "api-old").await;

        let server = tokio::spawn(async move {
            let (stream, peer) = listener.accept().await.expect("server should accept");
            handle_connection(stream, peer).await
        });

        let mut client = TcpStream::connect(addr)
            .await
            .expect("client should connect");
        let body = format!(
            r#"{{"model":"gpt-5.5","previous_response_id":"{}","input":"continue previous response"}}"#,
            previous_response_id
        );
        let request = format!(
            "POST /v1/responses HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer test-local-key\r\nAccept: text/event-stream\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            body.as_bytes().len(),
            body
        );
        client
            .write_all(request.as_bytes())
            .await
            .expect("request should be written");
        let mut response = Vec::new();
        tokio::time::timeout(Duration::from_secs(4), client.read_to_end(&mut response))
            .await
            .expect("response should finish")
            .expect("response read should succeed");
        let response_text = String::from_utf8_lossy(&response).to_string();

        let server_result = tokio::time::timeout(Duration::from_secs(1), server)
            .await
            .expect("server task should finish")
            .expect("server task should join");
        let seen = tokio::time::timeout(Duration::from_secs(3), upstream_server)
            .await
            .expect("fake upstream task should finish")
            .expect("fake upstream task should join");
        let audit =
            fs::read_to_string(root.join(super::CODEX_LOCAL_ACCESS_AUDIT_FILE)).unwrap_or_default();

        {
            let mut runtime = gateway_runtime().lock().await;
            *runtime = super::GatewayRuntime::default();
        }
        reset_local_api_backpressure_for_tests();
        reset_active_stream_leases_for_tests();
        match previous_root {
            Some(value) => std::env::set_var(super::CODEX_LOCAL_ACCESS_DATA_ROOT_ENV, value),
            None => std::env::remove_var(super::CODEX_LOCAL_ACCESS_DATA_ROOT_ENV),
        }
        match previous_accounts_root {
            Some(value) => std::env::set_var("COCKPIT_TOOLS_TEST_DATA_DIR", value),
            None => std::env::remove_var("COCKPIT_TOOLS_TEST_DATA_DIR"),
        }
        let _ = fs::remove_dir_all(&root);

        assert!(server_result.is_ok(), "server failed: {:?}", server_result);
        assert!(
            response_text.contains("HTTP/1.1 200 OK")
                && response_text.contains("resp_long_wait_ok")
                && response_text.contains("compaction_summary")
                && response_text.contains("response.completed"),
            "hard-affinity followup should wait and finish with a real compaction-capable upstream stream: {}",
            response_text
        );
        assert!(
            !response_text.contains("429 Too Many Requests")
                && !response_text.contains("Service Unavailable"),
            "hard-affinity followup must not leak terminal local/upstream errors: {}",
            response_text
        );
        assert_eq!(
            seen.len(),
            2,
            "gateway should retry the same original account after cooldown"
        );
        assert!(seen
            .iter()
            .all(|request| request.contains("Bearer sk-local-old")));
        assert!(audit.contains("\"reason\":\"hard_affinity_same_account_retry\""));
        assert!(audit.contains("\"phase\":\"pool_wait\""));
        assert!(audit.contains("\"phase\":\"request_trace\""));
        assert!(audit.contains("\"hard_affinity_continuity\":\"true\""));
        assert!(audit.contains("\"phase\":\"quota_classification\""));
        assert!(
            audit.contains("\"reset_source\":\"upstream_reset_hint\""),
            "audit missing reset source: {}",
            audit
        );
        assert!(audit.contains("\"retry_after_ms\":\"1100\""));
        assert!(audit.contains("\"phase\":\"routing_decision\""));
        assert!(audit.contains("\"phase\":\"stream_terminal\""));
        assert!(audit.contains("\"response_completed_seen\":\"true\""));
        assert!(audit.contains("\"compaction_summary_seen\":\"true\""));
        assert!(!audit.contains("\"phase\":\"final_response\""));
        assert!(!audit.contains("sk-local-old"));
        assert!(!audit.contains(previous_response_id));
    }

    #[test]
    fn codex_turn_metadata_is_lineage_only_not_hard_affinity() {
        let now = 1_700_000_000_000;
        let old_turn = ParsedRequest {
            method: "POST".to_string(),
            target: "/v1/responses".to_string(),
            headers: HashMap::from([
                (
                    "x-client-request-id".to_string(),
                    "thread-request-id".to_string(),
                ),
                (
                    "x-codex-turn-metadata".to_string(),
                    r#"{"turn_id":"turn-old"}"#.to_string(),
                ),
            ]),
            body: br#"{"model":"gpt-5.5","input":"old turn"}"#.to_vec(),
            gateway_request_id: "gw-test-8".to_string(),
        };
        let new_turn = ParsedRequest {
            method: "POST".to_string(),
            target: "/v1/responses".to_string(),
            headers: HashMap::from([
                (
                    "x-client-request-id".to_string(),
                    "thread-request-id".to_string(),
                ),
                (
                    "x-codex-turn-metadata".to_string(),
                    r#"{"turn_id":"turn-new"}"#.to_string(),
                ),
            ]),
            body: br#"{"model":"gpt-5.5","input":"new turn"}"#.to_vec(),
            gateway_request_id: "gw-test-9".to_string(),
        };

        let (old_lineage, old_lineage_source) = request_lineage_id_with_source(&old_turn);
        let (new_lineage, new_lineage_source) = request_lineage_id_with_source(&new_turn);
        let old_lineage = old_lineage.expect("old turn should have audit lineage");
        let new_lineage = new_lineage.expect("new turn should have audit lineage");
        assert_eq!(old_lineage_source, Some("codex_turn_metadata_turn_id"));
        assert_eq!(new_lineage_source, Some("codex_turn_metadata_turn_id"));
        assert!(old_lineage.starts_with("x-codex-turn-metadata.turn_id:sha256:"));
        assert!(new_lineage.starts_with("x-codex-turn-metadata.turn_id:sha256:"));
        assert_ne!(
            old_lineage, new_lineage,
            "Codex x-client-request-id is thread-scoped; turn metadata should remain useful for audit lineage"
        );
        assert_eq!(
            request_affinity_key(&old_turn),
            None,
            "x-codex-turn-metadata is observability metadata, not a hard routing key"
        );
        assert_eq!(
            request_affinity_key(&new_turn),
            None,
            "x-codex-turn-metadata must not block independent account admission"
        );
        assert_eq!(
            official_codex_sticky_routing_boundary(&old_turn),
            None,
            "official Codex treats metadata as observability lineage, not sticky routing"
        );

        let mut registry = empty_health_registry(now);
        assert!(!upsert_request_affinity_binding(
            &mut registry,
            &old_turn,
            "api-old",
            now,
        ));
        assert_eq!(
            request_affinity_account_from_registry(&registry, &old_turn, now),
            None
        );
        assert_eq!(
            request_affinity_account_from_registry(&registry, &new_turn, now),
            None,
            "Codex metadata-only turns must be admitted independently"
        );
    }

    #[test]
    fn official_codex_sticky_boundary_matches_reference_semantics() {
        let turn_state_request = ParsedRequest {
            method: "POST".to_string(),
            target: "/v1/responses".to_string(),
            headers: HashMap::from([(
                X_CODEX_TURN_STATE_HEADER.to_string(),
                "turn-state-token".to_string(),
            )]),
            body: br#"{"model":"gpt-5.5","input":"same turn"}"#.to_vec(),
            gateway_request_id: "gw-test-official-boundary-1".to_string(),
        };
        let turn_state_boundary = official_codex_sticky_routing_boundary(&turn_state_request)
            .expect("x-codex-turn-state should be sticky");
        assert_eq!(turn_state_boundary.reason(), "codex_turn_state");
        match turn_state_boundary {
            OfficialCodexStickyRoutingBoundary::TurnState { affinity_key } => {
                assert!(affinity_key.starts_with("x-codex-turn-state:sha256:"));
            }
            other => panic!("unexpected boundary: {:?}", other),
        }

        let continuation_request = ParsedRequest {
            method: "POST".to_string(),
            target: "/v1/responses".to_string(),
            headers: HashMap::new(),
            body: br#"{"model":"gpt-5.5","previous_response_id":"resp-1","input":"continue"}"#
                .to_vec(),
            gateway_request_id: "gw-test-official-boundary-2".to_string(),
        };
        let continuation_boundary = official_codex_sticky_routing_boundary(&continuation_request)
            .expect("previous_response_id should bind continuation");
        assert_eq!(continuation_boundary.reason(), "previous_response_id");
        match continuation_boundary {
            OfficialCodexStickyRoutingBoundary::PreviousResponseId { response_id } => {
                assert_eq!(response_id, "resp-1");
            }
            other => panic!("unexpected boundary: {:?}", other),
        }

        let metadata_request = ParsedRequest {
            method: "POST".to_string(),
            target: "/v1/responses".to_string(),
            headers: HashMap::from([(
                X_CODEX_TURN_METADATA_HEADER.to_string(),
                r#"{"turn_id":"turn-1"}"#.to_string(),
            )]),
            body: br#"{"model":"gpt-5.5","input":"metadata only"}"#.to_vec(),
            gateway_request_id: "gw-test-official-boundary-3".to_string(),
        };
        assert_eq!(
            official_codex_sticky_routing_boundary(&metadata_request),
            None
        );
        assert!(request_lineage_id_with_source(&metadata_request)
            .0
            .is_some());
    }

    #[test]
    fn active_stream_lease_uses_turn_state_affinity_even_with_client_request_id() {
        let _env_guard = LOCAL_ACCESS_ENV_TEST_LOCK
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let previous_root = std::env::var_os(super::CODEX_LOCAL_ACCESS_DATA_ROOT_ENV);
        let root = std::env::temp_dir().join(format!(
            "cockpit-local-access-turn-state-active-lease-test-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let _ = fs::remove_dir_all(&root);
        std::env::set_var(super::CODEX_LOCAL_ACCESS_DATA_ROOT_ENV, &root);
        reset_active_stream_leases_for_tests();

        let request = ParsedRequest {
            method: "POST".to_string(),
            target: "/v1/responses".to_string(),
            headers: HashMap::from([
                (
                    "x-client-request-id".to_string(),
                    "thread-request-id".to_string(),
                ),
                (
                    "x-codex-turn-state".to_string(),
                    "turn-secret-state".to_string(),
                ),
            ]),
            body: br#"{"model":"gpt-5.5","input":"active turn"}"#.to_vec(),
            gateway_request_id: "gw-test-10".to_string(),
        };
        let context = build_audit_context(&request, Some("api-old"));
        let mut lease = grant_active_stream_lease_for_request(&context, "api-old", &request);

        assert_eq!(
            active_stream_affinity_account_for_request(&request).as_deref(),
            Some("api-old"),
            "active stream affinity must use the Codex turn-state key, not the thread request id"
        );

        lease.release(ActiveStreamTerminal::Completed);
        reset_active_stream_leases_for_tests();
        match previous_root {
            Some(value) => std::env::set_var(super::CODEX_LOCAL_ACCESS_DATA_ROOT_ENV, value),
            None => std::env::remove_var(super::CODEX_LOCAL_ACCESS_DATA_ROOT_ENV),
        }
        let _ = fs::remove_dir_all(&root);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn active_stream_request_affinity_blocks_old_task_fallback_but_allows_new_task() {
        let _env_guard = LOCAL_ACCESS_ENV_TEST_LOCK
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let _backpressure_guard = LOCAL_BACKPRESSURE_TEST_LOCK
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let previous_root = std::env::var_os(super::CODEX_LOCAL_ACCESS_DATA_ROOT_ENV);
        let previous_accounts_root = std::env::var_os("COCKPIT_TOOLS_TEST_DATA_DIR");
        let root = std::env::temp_dir().join(format!(
            "cockpit-local-access-active-affinity-fallback-test-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let _ = fs::remove_dir_all(&root);
        std::env::set_var(super::CODEX_LOCAL_ACCESS_DATA_ROOT_ENV, &root);
        std::env::set_var("COCKPIT_TOOLS_TEST_DATA_DIR", &root);

        reset_local_api_backpressure_for_tests();
        reset_active_stream_leases_for_tests();

        let upstream_seen = std::sync::Arc::new(tokio::sync::Mutex::new(Vec::<String>::new()));
        let upstream_listener = TcpListener::bind((super::CODEX_LOCAL_ACCESS_BIND_HOST, 0))
            .await
            .expect("fake upstream should bind");
        let upstream_addr = upstream_listener
            .local_addr()
            .expect("fake upstream should have addr");
        let upstream_seen_task = std::sync::Arc::clone(&upstream_seen);
        let upstream_server = tokio::spawn(async move {
            loop {
                let accept =
                    tokio::time::timeout(Duration::from_secs(5), upstream_listener.accept()).await;
                let Ok(Ok((mut socket, _))) = accept else {
                    break;
                };
                let mut request = Vec::new();
                let mut chunk = [0u8; 1024];
                loop {
                    let read = socket
                        .read(&mut chunk)
                        .await
                        .expect("fake upstream should read request");
                    if read == 0 {
                        break;
                    }
                    request.extend_from_slice(&chunk[..read]);
                    if request.windows(4).any(|window| window == b"\r\n\r\n") {
                        break;
                    }
                }
                let request_text = String::from_utf8_lossy(&request).to_string();
                upstream_seen_task.lock().await.push(request_text.clone());

                if request_text.contains("Bearer sk-local-old") {
                    let body =
                        r#"{"error":{"type":"usage_limit_reached","code":"usage_limit_reached"}}"#;
                    let response = format!(
                        "HTTP/1.1 429 Too Many Requests\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.as_bytes().len(),
                        body
                    );
                    socket
                        .write_all(response.as_bytes())
                        .await
                        .expect("fake upstream should write old-account 429");
                    continue;
                }

                let body = concat!(
                    "event: response.created\n",
                    "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_current_ok\",\"model\":\"gpt-5.5\",\"status\":\"in_progress\",\"output\":[]}}\n\n",
                    "event: response.output_text.delta\n",
                    "data: {\"type\":\"response.output_text.delta\",\"item_id\":\"msg_1\",\"output_index\":0,\"content_index\":0,\"delta\":\"CURRENT\"}\n\n",
                    "event: response.completed\n",
                    "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_current_ok\",\"model\":\"gpt-5.5\",\"status\":\"completed\",\"output\":[],\"usage\":{\"input_tokens\":1,\"output_tokens\":1,\"total_tokens\":2}}}\n\n",
                    "data: [DONE]\n\n"
                );
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.as_bytes().len(),
                    body
                );
                socket
                    .write_all(response.as_bytes())
                    .await
                    .expect("fake upstream should write current-account response");
            }
        });

        let now = now_ms();
        let registry = empty_health_registry(now);
        save_health_registry_to_path(&root.join(super::CODEX_LOCAL_ACCESS_HEALTH_FILE), &registry)
            .expect("health registry should be written to isolated test root");

        let listener = TcpListener::bind((super::CODEX_LOCAL_ACCESS_BIND_HOST, 0))
            .await
            .expect("test listener should bind");
        let addr = listener
            .local_addr()
            .expect("listener should have local addr");
        let collection: CodexLocalAccessCollection = serde_json::from_value(json!({
            "enabled": true,
            "port": addr.port(),
            "apiKey": "test-local-key",
            "safetyConfig": {
                "schemaVersion": 1,
                "hardenedLocalMode": true,
                "maxConcurrentRequests": 1,
                "minRequestIntervalSeconds": 0,
                "maxQueueWaitSeconds": 1,
                "requestTimeoutSeconds": 5,
                "maxRequestBodyMb": 64,
                "maxRetries": 1,
                "maxRetryAccounts": 2,
                "fallbackMode": "next_request_only",
                "logging": {
                    "redactSensitiveValues": true,
                    "includeRequestId": true,
                    "includeAccountHash": true,
                    "includeRoute": true,
                    "includeModel": true,
                    "includeLatency": true,
                    "includePromptResponse": false,
                    "includeRawUpstreamBody": false
                }
            },
            "routingStrategy": "auto",
            "restrictFreeAccounts": false,
            "followCurrentAccount": false,
            "accountIds": ["api-current"],
            "createdAt": now,
            "updatedAt": now
        }))
        .expect("collection fixture should deserialize");
        {
            let mut runtime = gateway_runtime().lock().await;
            *runtime = super::GatewayRuntime::default();
            runtime.loaded = true;
            runtime.collection = Some(collection);
            runtime.running = true;
            runtime.actual_port = Some(addr.port());
        }

        let old_account = CodexAccount::new_api_key(
            "api-old".to_string(),
            "api-old@example.com".to_string(),
            "sk-local-old".to_string(),
            CodexApiProviderMode::Custom,
            Some(format!("http://127.0.0.1:{}/v1", upstream_addr.port())),
            None,
            None,
        );
        let current_account = CodexAccount::new_api_key(
            "api-current".to_string(),
            "api-current@example.com".to_string(),
            "sk-local-current".to_string(),
            CodexApiProviderMode::Custom,
            Some(format!("http://127.0.0.1:{}/v1", upstream_addr.port())),
            None,
            None,
        );
        crate::modules::codex_account::save_account(&old_account)
            .expect("old active account should be persisted");
        crate::modules::codex_account::save_account(&current_account)
            .expect("current pool account should be persisted");

        let active_request = ParsedRequest {
            method: "POST".to_string(),
            target: "/v1/responses".to_string(),
            headers: HashMap::from([
                (
                    "x-client-request-id".to_string(),
                    "thread-request-id".to_string(),
                ),
                (
                    "x-codex-turn-metadata".to_string(),
                    r#"{"turn_id":"turn-old"}"#.to_string(),
                ),
                (
                    "x-codex-turn-state".to_string(),
                    "turn-old-state".to_string(),
                ),
            ]),
            body: br#"{"model":"gpt-5.5","input":"already running"}"#.to_vec(),
            gateway_request_id: "gw-test-11".to_string(),
        };
        let active_context = build_audit_context(&active_request, Some("api-old"));
        let mut active_lease =
            grant_active_stream_lease_for_request(&active_context, "api-old", &active_request);
        assert_eq!(active_stream_lease_count_for_account("api-old"), 1);

        let mut persisted_registry =
            load_health_registry_from_path(&root.join(super::CODEX_LOCAL_ACCESS_HEALTH_FILE))
                .expect("health registry should load from isolated test root");
        assert!(upsert_request_affinity_binding(
            &mut persisted_registry,
            &active_request,
            "api-old",
            now_ms(),
        ));
        save_health_registry_to_path(
            &root.join(super::CODEX_LOCAL_ACCESS_HEALTH_FILE),
            &persisted_registry,
        )
        .expect("persistent request affinity should be written");

        let server = tokio::spawn(async move {
            let mut handlers = Vec::new();
            for _ in 0..2 {
                let (stream, peer) = listener.accept().await.expect("server should accept");
                handlers.push(tokio::spawn(async move {
                    handle_connection(stream, peer).await
                }));
            }
            for handler in handlers {
                handler.await.expect("server connection task should join")?;
            }
            Ok::<(), String>(())
        });

        let mut active_client = TcpStream::connect(addr)
            .await
            .expect("active client should connect");
        let active_body = br#"{"model":"gpt-5.5","input":"continue old task"}"#;
        let active_http = format!(
            "POST /v1/responses HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer test-local-key\r\nAccept: text/event-stream\r\nX-Client-Request-Id: thread-request-id\r\nX-Codex-Turn-Metadata: {{\"turn_id\":\"turn-old\"}}\r\nX-Codex-Turn-State: turn-old-state\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            active_body.len(),
            String::from_utf8_lossy(active_body)
        );
        active_client
            .write_all(active_http.as_bytes())
            .await
            .expect("active request should be written");
        let mut active_response = Vec::new();
        tokio::time::timeout(
            Duration::from_secs(3),
            active_client.read_to_end(&mut active_response),
        )
        .await
        .expect("active request response should finish")
        .expect("active response read should succeed");

        let mut new_client = TcpStream::connect(addr)
            .await
            .expect("new client should connect");
        let new_body = br#"{"model":"gpt-5.5","input":"new task"}"#;
        let new_http = format!(
            "POST /v1/responses HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer test-local-key\r\nAccept: text/event-stream\r\nX-Client-Request-Id: thread-request-id\r\nX-Codex-Turn-Metadata: {{\"turn_id\":\"turn-new\"}}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            new_body.len(),
            String::from_utf8_lossy(new_body)
        );
        new_client
            .write_all(new_http.as_bytes())
            .await
            .expect("new request should be written");
        let mut new_response = Vec::new();
        tokio::time::timeout(
            Duration::from_secs(3),
            new_client.read_to_end(&mut new_response),
        )
        .await
        .expect("new request response should finish")
        .expect("new response read should succeed");

        let server_result = server.await.expect("server task should join");
        assert!(server_result.is_ok(), "server failed: {:?}", server_result);
        tokio::time::sleep(Duration::from_millis(100)).await;
        upstream_server.abort();

        let active_response_text = String::from_utf8_lossy(&active_response);
        let new_response_text = String::from_utf8_lossy(&new_response);
        let seen = upstream_seen.lock().await.clone();
        let seen_text = seen.join("\n--- request ---\n");

        active_lease.release(ActiveStreamTerminal::Completed);
        assert_eq!(active_stream_lease_count_for_account("api-old"), 0);
        {
            let mut runtime = gateway_runtime().lock().await;
            *runtime = super::GatewayRuntime::default();
        }
        reset_local_api_backpressure_for_tests();
        reset_active_stream_leases_for_tests();
        match previous_root {
            Some(value) => std::env::set_var(super::CODEX_LOCAL_ACCESS_DATA_ROOT_ENV, value),
            None => std::env::remove_var(super::CODEX_LOCAL_ACCESS_DATA_ROOT_ENV),
        }
        match previous_accounts_root {
            Some(value) => std::env::set_var("COCKPIT_TOOLS_TEST_DATA_DIR", value),
            None => std::env::remove_var("COCKPIT_TOOLS_TEST_DATA_DIR"),
        }
        let _ = fs::remove_dir_all(&root);

        assert!(
            !active_response_text.contains("resp_current_ok")
                && !active_response_text.contains("CURRENT"),
            "active task must not be replayed on the replacement account: {}",
            active_response_text
        );
        assert!(
            new_response_text.contains("resp_current_ok") && new_response_text.contains("CURRENT"),
            "new task should immediately use the current replacement account: {}",
            new_response_text
        );
        assert!(
            seen.iter()
                .any(|request| request.contains("\"turn_id\":\"turn-old\"")
                    && request.contains("Bearer sk-local-old")),
            "active task should reach the old account: {}",
            seen_text
        );
        assert!(
            !seen
                .iter()
                .any(|request| request.contains("\"turn_id\":\"turn-old\"")
                    && request.contains("Bearer sk-local-current")),
            "active task must not be sent to the replacement account: {}",
            seen_text
        );
        assert!(
            seen.iter()
                .any(|request| request.contains("\"turn_id\":\"turn-new\"")
                    && request.contains("Bearer sk-local-current")),
            "new task should reach the replacement account: {}",
            seen_text
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn in_flight_stream_finishes_on_original_account_while_new_task_uses_replacement() {
        let _env_guard = LOCAL_ACCESS_ENV_TEST_LOCK
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let _backpressure_guard = LOCAL_BACKPRESSURE_TEST_LOCK
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let previous_root = std::env::var_os(super::CODEX_LOCAL_ACCESS_DATA_ROOT_ENV);
        let previous_accounts_root = std::env::var_os("COCKPIT_TOOLS_TEST_DATA_DIR");
        let root = std::env::temp_dir().join(format!(
            "cockpit-local-access-inflight-stream-switch-test-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let _ = fs::remove_dir_all(&root);
        std::env::set_var(super::CODEX_LOCAL_ACCESS_DATA_ROOT_ENV, &root);
        std::env::set_var("COCKPIT_TOOLS_TEST_DATA_DIR", &root);

        reset_local_api_backpressure_for_tests();
        reset_active_stream_leases_for_tests();

        let upstream_seen = std::sync::Arc::new(tokio::sync::Mutex::new(Vec::<String>::new()));
        let old_stream_started = std::sync::Arc::new(tokio::sync::Notify::new());
        let finish_old_stream = std::sync::Arc::new(tokio::sync::Notify::new());
        let upstream_listener = TcpListener::bind((super::CODEX_LOCAL_ACCESS_BIND_HOST, 0))
            .await
            .expect("fake upstream should bind");
        let upstream_addr = upstream_listener
            .local_addr()
            .expect("fake upstream should have addr");
        let upstream_seen_task = std::sync::Arc::clone(&upstream_seen);
        let old_stream_started_task = std::sync::Arc::clone(&old_stream_started);
        let finish_old_stream_task = std::sync::Arc::clone(&finish_old_stream);
        let upstream_server = tokio::spawn(async move {
            let mut handlers = Vec::new();
            for _ in 0..2 {
                let (mut socket, _) = upstream_listener
                    .accept()
                    .await
                    .expect("fake upstream should accept");
                let upstream_seen_task = std::sync::Arc::clone(&upstream_seen_task);
                let old_stream_started_task = std::sync::Arc::clone(&old_stream_started_task);
                let finish_old_stream_task = std::sync::Arc::clone(&finish_old_stream_task);
                handlers.push(tokio::spawn(async move {
                    let mut request = Vec::new();
                    let mut chunk = [0u8; 1024];
                    loop {
                        let read = socket
                            .read(&mut chunk)
                            .await
                            .expect("fake upstream should read request");
                        if read == 0 {
                            break;
                        }
                        request.extend_from_slice(&chunk[..read]);
                        if request.windows(4).any(|window| window == b"\r\n\r\n") {
                            break;
                        }
                    }
                    let request_text = String::from_utf8_lossy(&request).to_string();
                    upstream_seen_task.lock().await.push(request_text.clone());

                    if request_text.contains("Bearer sk-local-old") {
                        let old_first = concat!(
                            "event: response.created\n",
                            "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_old_stream\",\"model\":\"gpt-5.5\",\"status\":\"in_progress\",\"output\":[]}}\n\n",
                            "event: response.output_text.delta\n",
                            "data: {\"type\":\"response.output_text.delta\",\"item_id\":\"msg_1\",\"output_index\":0,\"content_index\":0,\"delta\":\"OLD-START\"}\n\n"
                        );
                        let old_rest = concat!(
                            "event: response.output_text.delta\n",
                            "data: {\"type\":\"response.output_text.delta\",\"item_id\":\"msg_1\",\"output_index\":0,\"content_index\":0,\"delta\":\"OLD-DONE\"}\n\n",
                            "event: response.completed\n",
                            "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_old_stream\",\"model\":\"gpt-5.5\",\"status\":\"completed\",\"output\":[],\"usage\":{\"input_tokens\":1,\"output_tokens\":2,\"total_tokens\":3}}}\n\n",
                            "data: [DONE]\n\n"
                        );
                        let content_length =
                            old_first.as_bytes().len() + old_rest.as_bytes().len();
                        let response_head = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            content_length
                        );
                        socket
                            .write_all(response_head.as_bytes())
                            .await
                            .expect("fake upstream should write old response headers");
                        socket
                            .write_all(old_first.as_bytes())
                            .await
                            .expect("fake upstream should write old first chunk");
                        old_stream_started_task.notify_waiters();
                        finish_old_stream_task.notified().await;
                        socket
                            .write_all(old_rest.as_bytes())
                            .await
                            .expect("fake upstream should finish old stream");
                        return;
                    }

                    let body = concat!(
                        "event: response.created\n",
                        "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_current_stream\",\"model\":\"gpt-5.5\",\"status\":\"in_progress\",\"output\":[]}}\n\n",
                        "event: response.output_text.delta\n",
                        "data: {\"type\":\"response.output_text.delta\",\"item_id\":\"msg_1\",\"output_index\":0,\"content_index\":0,\"delta\":\"CURRENT\"}\n\n",
                        "event: response.completed\n",
                        "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_current_stream\",\"model\":\"gpt-5.5\",\"status\":\"completed\",\"output\":[],\"usage\":{\"input_tokens\":1,\"output_tokens\":1,\"total_tokens\":2}}}\n\n",
                        "data: [DONE]\n\n"
                    );
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.as_bytes().len(),
                        body
                    );
                    socket
                        .write_all(response.as_bytes())
                        .await
                        .expect("fake upstream should write current-account response");
                }));
            }
            for handler in handlers {
                handler
                    .await
                    .expect("fake upstream connection task should join");
            }
        });

        let now = now_ms();
        let registry = empty_health_registry(now);
        save_health_registry_to_path(&root.join(super::CODEX_LOCAL_ACCESS_HEALTH_FILE), &registry)
            .expect("health registry should be written to isolated test root");

        let listener = TcpListener::bind((super::CODEX_LOCAL_ACCESS_BIND_HOST, 0))
            .await
            .expect("test listener should bind");
        let addr = listener
            .local_addr()
            .expect("listener should have local addr");
        let collection: CodexLocalAccessCollection = serde_json::from_value(json!({
            "enabled": true,
            "port": addr.port(),
            "apiKey": "test-local-key",
            "safetyConfig": {
                "schemaVersion": 1,
                "hardenedLocalMode": true,
                "maxConcurrentRequests": 1,
                "minRequestIntervalSeconds": 0,
                "maxQueueWaitSeconds": 1,
                "requestTimeoutSeconds": 5,
                "maxRequestBodyMb": 64,
                "maxRetries": 1,
                "maxRetryAccounts": 2,
                "fallbackMode": "next_request_only",
                "logging": {
                    "redactSensitiveValues": true,
                    "includeRequestId": true,
                    "includeAccountHash": true,
                    "includeRoute": true,
                    "includeModel": true,
                    "includeLatency": true,
                    "includePromptResponse": false,
                    "includeRawUpstreamBody": false
                }
            },
            "routingStrategy": "auto",
            "restrictFreeAccounts": false,
            "followCurrentAccount": false,
            "accountIds": ["api-old", "api-current"],
            "createdAt": now,
            "updatedAt": now
        }))
        .expect("collection fixture should deserialize");
        {
            let mut runtime = gateway_runtime().lock().await;
            *runtime = super::GatewayRuntime::default();
            runtime.loaded = true;
            runtime.collection = Some(collection);
            runtime.running = true;
            runtime.actual_port = Some(addr.port());
        }

        let old_account = CodexAccount::new_api_key(
            "api-old".to_string(),
            "api-old@example.com".to_string(),
            "sk-local-old".to_string(),
            CodexApiProviderMode::Custom,
            Some(format!("http://127.0.0.1:{}/v1", upstream_addr.port())),
            None,
            None,
        );
        let current_account = CodexAccount::new_api_key(
            "api-current".to_string(),
            "api-current@example.com".to_string(),
            "sk-local-current".to_string(),
            CodexApiProviderMode::Custom,
            Some(format!("http://127.0.0.1:{}/v1", upstream_addr.port())),
            None,
            None,
        );
        crate::modules::codex_account::save_account(&old_account)
            .expect("old account should be persisted");
        crate::modules::codex_account::save_account(&current_account)
            .expect("current account should be persisted");

        let old_affinity_request = ParsedRequest {
            method: "POST".to_string(),
            target: "/v1/responses".to_string(),
            headers: HashMap::from([
                (
                    "x-client-request-id".to_string(),
                    "thread-request-id".to_string(),
                ),
                (
                    "x-codex-turn-metadata".to_string(),
                    r#"{"turn_id":"turn-old-stream"}"#.to_string(),
                ),
                (
                    "x-codex-turn-state".to_string(),
                    "turn-old-stream-state".to_string(),
                ),
            ]),
            body: br#"{"model":"gpt-5.5","input":"old stream start"}"#.to_vec(),
            gateway_request_id: "gw-test-12".to_string(),
        };
        let mut persisted_registry =
            load_health_registry_from_path(&root.join(super::CODEX_LOCAL_ACCESS_HEALTH_FILE))
                .expect("health registry should load from isolated test root");
        assert!(upsert_request_affinity_binding(
            &mut persisted_registry,
            &old_affinity_request,
            "api-old",
            now_ms(),
        ));
        save_health_registry_to_path(
            &root.join(super::CODEX_LOCAL_ACCESS_HEALTH_FILE),
            &persisted_registry,
        )
        .expect("persistent request affinity should be written");

        let server = tokio::spawn(async move {
            let mut handlers = Vec::new();
            for _ in 0..2 {
                let (stream, peer) = listener.accept().await.expect("server should accept");
                handlers.push(tokio::spawn(async move {
                    handle_connection(stream, peer).await
                }));
            }
            for handler in handlers {
                handler.await.expect("server connection task should join")?;
            }
            Ok::<(), String>(())
        });

        let old_client_task = tokio::spawn(async move {
            let mut client = TcpStream::connect(addr)
                .await
                .expect("old client should connect");
            let body = br#"{"model":"gpt-5.5","input":"old stream"}"#;
            let request = format!(
                "POST /v1/responses HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer test-local-key\r\nAccept: text/event-stream\r\nX-Client-Request-Id: thread-request-id\r\nX-Codex-Turn-Metadata: {{\"turn_id\":\"turn-old-stream\"}}\r\nX-Codex-Turn-State: turn-old-stream-state\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                body.len(),
                String::from_utf8_lossy(body)
            );
            client
                .write_all(request.as_bytes())
                .await
                .expect("old request should be written");
            let mut response = Vec::new();
            client
                .read_to_end(&mut response)
                .await
                .expect("old response read should succeed");
            response
        });

        tokio::time::timeout(Duration::from_secs(2), old_stream_started.notified())
            .await
            .expect("old upstream stream should start");
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if active_stream_lease_count_for_account("api-old") == 1 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("old active stream lease should be granted");

        let mut registry =
            load_health_registry_from_path(&root.join(super::CODEX_LOCAL_ACCESS_HEALTH_FILE))
                .expect("health registry should load before simulated exhaustion");
        let exhausted = classify_codex_upstream_error(
            StatusCode::TOO_MANY_REQUESTS,
            None,
            r#"{"error":{"type":"usage_limit_reached","code":"usage_limit_reached"}}"#,
        );
        update_health_registry_from_classified_error(
            &mut registry,
            "api-old",
            Some("gpt-5.5"),
            Some("req-inflight-old-stream"),
            &exhausted,
            now_ms(),
        );
        save_health_registry_to_path(&root.join(super::CODEX_LOCAL_ACCESS_HEALTH_FILE), &registry)
            .expect("simulated quota exhaustion should be persisted");

        let mut new_client = TcpStream::connect(addr)
            .await
            .expect("new client should connect");
        let new_body = br#"{"model":"gpt-5.5","input":"new task after exhaustion"}"#;
        let new_request = format!(
            "POST /v1/responses HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer test-local-key\r\nAccept: text/event-stream\r\nX-Client-Request-Id: thread-request-id\r\nX-Codex-Turn-Metadata: {{\"turn_id\":\"turn-new-after-exhausted\"}}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            new_body.len(),
            String::from_utf8_lossy(new_body)
        );
        new_client
            .write_all(new_request.as_bytes())
            .await
            .expect("new request should be written");
        let mut new_response = Vec::new();
        tokio::time::timeout(
            Duration::from_secs(2),
            new_client.read_to_end(&mut new_response),
        )
        .await
        .expect("new request response should finish")
        .expect("new response read should succeed");
        finish_old_stream.notify_waiters();
        let old_response = tokio::time::timeout(Duration::from_secs(2), old_client_task)
            .await
            .expect("old client task should finish")
            .expect("old client task should not panic");

        let server_result = server.await.expect("server task should join");
        assert!(server_result.is_ok(), "server failed: {:?}", server_result);
        let upstream_result = upstream_server.await;
        assert!(
            upstream_result.is_ok(),
            "fake upstream task should join: {:?}",
            upstream_result
        );

        let old_response_text = String::from_utf8_lossy(&old_response);
        let new_response_text = String::from_utf8_lossy(&new_response);
        let seen = upstream_seen.lock().await.clone();
        let seen_text = seen.join("\n--- request ---\n");

        {
            let mut runtime = gateway_runtime().lock().await;
            *runtime = super::GatewayRuntime::default();
        }
        reset_local_api_backpressure_for_tests();
        reset_active_stream_leases_for_tests();
        match previous_root {
            Some(value) => std::env::set_var(super::CODEX_LOCAL_ACCESS_DATA_ROOT_ENV, value),
            None => std::env::remove_var(super::CODEX_LOCAL_ACCESS_DATA_ROOT_ENV),
        }
        match previous_accounts_root {
            Some(value) => std::env::set_var("COCKPIT_TOOLS_TEST_DATA_DIR", value),
            None => std::env::remove_var("COCKPIT_TOOLS_TEST_DATA_DIR"),
        }
        let _ = fs::remove_dir_all(&root);

        assert!(
            old_response_text.contains("OLD-START")
                && old_response_text.contains("OLD-DONE")
                && old_response_text.contains("resp_old_stream"),
            "old stream should complete from the original account: {}",
            old_response_text
        );
        assert!(
            !old_response_text.contains("CURRENT"),
            "old stream must not be replaced by the new account response: {}",
            old_response_text
        );
        assert!(
            new_response_text.contains("CURRENT")
                && new_response_text.contains("resp_current_stream"),
            "new task should immediately use the replacement account: {}",
            new_response_text
        );
        assert!(
            seen.iter().any(
                |request| request.contains("\"turn_id\":\"turn-old-stream\"")
                    && request.contains("Bearer sk-local-old")
            ),
            "old stream should reach the original account: {}",
            seen_text
        );
        assert!(
            !seen.iter().any(
                |request| request.contains("\"turn_id\":\"turn-old-stream\"")
                    && request.contains("Bearer sk-local-current")
            ),
            "old stream must never be replayed on the replacement account: {}",
            seen_text
        );
        assert!(
            seen.iter().any(
                |request| request.contains("\"turn_id\":\"turn-new-after-exhausted\"")
                    && request.contains("Bearer sk-local-current")
            ),
            "new task should reach the replacement account: {}",
            seen_text
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn previous_response_continuation_bypasses_local_backpressure_start_interval() {
        let _env_guard = LOCAL_ACCESS_ENV_TEST_LOCK
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let _backpressure_guard = LOCAL_BACKPRESSURE_TEST_LOCK
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let previous_root = std::env::var_os(super::CODEX_LOCAL_ACCESS_DATA_ROOT_ENV);
        let root = std::env::temp_dir().join(format!(
            "cockpit-local-access-continuation-backpressure-test-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let _ = fs::remove_dir_all(&root);
        std::env::set_var(super::CODEX_LOCAL_ACCESS_DATA_ROOT_ENV, &root);

        reset_local_api_backpressure_for_tests();
        reset_active_stream_leases_for_tests();

        let upstream_listener = TcpListener::bind((super::CODEX_LOCAL_ACCESS_BIND_HOST, 0))
            .await
            .expect("fake upstream should bind");
        let upstream_addr = upstream_listener
            .local_addr()
            .expect("fake upstream should have addr");
        let upstream_server = tokio::spawn(async move {
            let (mut socket, _) = upstream_listener
                .accept()
                .await
                .expect("fake upstream should accept continuation");
            let mut request = Vec::new();
            let mut chunk = [0u8; 1024];
            loop {
                let read = socket
                    .read(&mut chunk)
                    .await
                    .expect("fake upstream should read request");
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&chunk[..read]);
                if request.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            assert!(
                String::from_utf8_lossy(&request).contains("previous_response_id"),
                "continuation request should reach upstream"
            );
            let body = concat!(
                "event: response.created\n",
                "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_backpressure_bypass\",\"model\":\"gpt-5.5\",\"status\":\"in_progress\",\"output\":[]}}\n\n",
                "event: response.output_text.delta\n",
                "data: {\"type\":\"response.output_text.delta\",\"item_id\":\"msg_1\",\"output_index\":0,\"content_index\":0,\"delta\":\"OK\"}\n\n",
                "event: response.completed\n",
                "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_backpressure_bypass\",\"model\":\"gpt-5.5\",\"status\":\"completed\",\"output\":[],\"usage\":{\"input_tokens\":1,\"output_tokens\":1,\"total_tokens\":2}}}\n\n",
                "data: [DONE]\n\n"
            );
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.as_bytes().len(),
                body
            );
            socket
                .write_all(response.as_bytes())
                .await
                .expect("fake upstream should write response");
        });

        let now = now_ms();
        let listener = TcpListener::bind((super::CODEX_LOCAL_ACCESS_BIND_HOST, 0))
            .await
            .expect("test listener should bind");
        let addr = listener
            .local_addr()
            .expect("test listener should have addr");
        let collection: CodexLocalAccessCollection = serde_json::from_value(json!({
            "enabled": true,
            "port": addr.port(),
            "apiKey": "test-local-key",
            "safetyConfig": {
                "schemaVersion": 1,
                "hardenedLocalMode": true,
                "maxConcurrentRequests": 1,
                "minRequestIntervalSeconds": 30,
                "maxQueueWaitSeconds": 1,
                "requestTimeoutSeconds": 5,
                "maxRequestBodyMb": 64,
                "maxRetries": 1,
                "maxRetryAccounts": 1,
                "fallbackMode": "disabled",
                "logging": {
                    "redactSensitiveValues": true,
                    "includeRequestId": true,
                    "includeAccountHash": true,
                    "includeRoute": true,
                    "includeModel": true,
                    "includeLatency": true,
                    "includePromptResponse": false,
                    "includeRawUpstreamBody": false
                }
            },
            "routingStrategy": "auto",
            "restrictFreeAccounts": false,
            "followCurrentAccount": false,
            "accountIds": ["api-continuation"],
            "createdAt": now,
            "updatedAt": now
        }))
        .expect("collection fixture should deserialize");
        let backpressure_config = collection.safety_config.clone();
        {
            let mut runtime = gateway_runtime().lock().await;
            *runtime = super::GatewayRuntime::default();
            runtime.loaded = true;
            runtime.collection = Some(collection);
            runtime.running = true;
            runtime.actual_port = Some(addr.port());
        }
        let account = CodexAccount::new_api_key(
            "api-continuation".to_string(),
            "api-continuation@example.com".to_string(),
            "sk-local-test".to_string(),
            CodexApiProviderMode::Custom,
            Some(format!("http://127.0.0.1:{}/v1", upstream_addr.port())),
            None,
            None,
        );
        cache_prepared_account(&account).await;

        let stale_permit = acquire_local_api_backpressure(&backpressure_config)
            .await
            .expect("initial request should acquire backpressure slot");
        drop(stale_permit);

        let server = tokio::spawn(async move {
            let (stream, peer) = listener.accept().await.expect("server should accept");
            handle_connection(stream, peer).await
        });
        let mut client = TcpStream::connect(addr)
            .await
            .expect("client should connect");
        let body =
            br#"{"model":"gpt-5.5","previous_response_id":"resp_existing","input":"continue"}"#;
        let request = format!(
            "POST /v1/responses HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer test-local-key\r\nAccept: text/event-stream\r\nX-Client-Request-Id: req-continuation-backpressure\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            String::from_utf8_lossy(body)
        );
        client
            .write_all(request.as_bytes())
            .await
            .expect("request should be written");

        let mut response = Vec::new();
        tokio::time::timeout(Duration::from_secs(1), client.read_to_end(&mut response))
            .await
            .expect("continuation must not wait behind the new-request start interval")
            .expect("response read should succeed");
        let response_text = String::from_utf8_lossy(&response);
        assert!(
            response_text.contains("HTTP/1.1 200 OK"),
            "expected upstream continuation response, got: {}",
            response_text
        );
        assert!(
            response_text.contains("resp_backpressure_bypass") && response_text.contains("OK"),
            "continuation must be forwarded instead of timing out in local backpressure: {}",
            response_text
        );
        assert!(
            !response_text.contains("本地接入队列等待超时"),
            "continuation must not expose local backpressure timeout: {}",
            response_text
        );

        let audit = fs::read_to_string(root.join(super::CODEX_LOCAL_ACCESS_AUDIT_FILE))
            .expect("audit should be written to isolated root");
        assert!(audit.contains("\"phase\":\"local_backpressure\""));
        assert!(audit.contains("\"outcome\":\"bypassed\""));
        assert!(audit.contains("\"reason\":\"previous_response_id\""));
        assert!(audit.contains("\"phase\":\"stream_completed\""));
        assert!(!audit.contains("\"phase\":\"client_aborted\""));

        let server_result = server.await.expect("server task should join");
        assert!(server_result.is_ok(), "server failed: {:?}", server_result);
        upstream_server
            .await
            .expect("fake upstream task should join");
        {
            let mut runtime = gateway_runtime().lock().await;
            *runtime = super::GatewayRuntime::default();
        }
        reset_local_api_backpressure_for_tests();
        reset_active_stream_leases_for_tests();
        match previous_root {
            Some(value) => std::env::set_var(super::CODEX_LOCAL_ACCESS_DATA_ROOT_ENV, value),
            None => std::env::remove_var(super::CODEX_LOCAL_ACCESS_DATA_ROOT_ENV),
        }
        let _ = fs::remove_dir_all(&root);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn exhausted_responses_non_stream_returns_in_band_completed_payload() {
        let _env_guard = LOCAL_ACCESS_ENV_TEST_LOCK
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let previous_root = std::env::var_os(super::CODEX_LOCAL_ACCESS_DATA_ROOT_ENV);
        let root = std::env::temp_dir().join(format!(
            "cockpit-local-access-pool-unavailable-non-stream-test-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let _ = fs::remove_dir_all(&root);
        std::env::set_var(super::CODEX_LOCAL_ACCESS_DATA_ROOT_ENV, &root);

        reset_local_api_backpressure_for_tests();
        reset_active_stream_leases_for_tests();

        let now = now_ms();
        let mut registry = empty_health_registry(now);
        for account_id in ["cooling-a", "cooling-b"] {
            registry.model_cooldowns.insert(
                health_registry_model_key(account_id, "gpt-5.5"),
                CodexLocalAccessModelCooldown {
                    account_id: account_id.to_string(),
                    model: "gpt-5.5".to_string(),
                    cooldown_until_ms: now + (7 * 24 * 60 * 60 * 1000),
                    last_error_type: Some("usage_limit_reached".to_string()),
                    last_request_id: Some("req-json-local".to_string()),
                    updated_at: now,
                },
            );
        }
        save_health_registry_to_path(&root.join(super::CODEX_LOCAL_ACCESS_HEALTH_FILE), &registry)
            .expect("health registry should be written to isolated test root");

        let listener = TcpListener::bind((super::CODEX_LOCAL_ACCESS_BIND_HOST, 0))
            .await
            .expect("test listener should bind");
        let addr = listener
            .local_addr()
            .expect("listener should have local addr");
        let collection: CodexLocalAccessCollection = serde_json::from_value(json!({
            "enabled": true,
            "port": addr.port(),
            "apiKey": "test-local-key",
            "safetyConfig": {
                "schemaVersion": 1,
                "hardenedLocalMode": true,
                "maxConcurrentRequests": 1,
                "minRequestIntervalSeconds": 1,
                "maxQueueWaitSeconds": 1,
                "requestTimeoutSeconds": 3,
                "maxRequestBodyMb": 64,
                "maxRetries": 1,
                "maxRetryAccounts": 2,
                "fallbackMode": "disabled",
                "logging": {
                    "redactSensitiveValues": true,
                    "includeRequestId": true,
                    "includeAccountHash": true,
                    "includeRoute": true,
                    "includeModel": true,
                    "includeLatency": true,
                    "includePromptResponse": false,
                    "includeRawUpstreamBody": false
                }
            },
            "routingStrategy": "auto",
            "restrictFreeAccounts": false,
            "followCurrentAccount": false,
            "accountIds": ["cooling-a", "cooling-b"],
            "createdAt": now,
            "updatedAt": now
        }))
        .expect("collection fixture should deserialize");
        {
            let mut runtime = gateway_runtime().lock().await;
            *runtime = super::GatewayRuntime::default();
            runtime.loaded = true;
            runtime.collection = Some(collection);
            runtime.running = true;
            runtime.actual_port = Some(addr.port());
        }

        let server = tokio::spawn(async move {
            let (stream, peer) = listener.accept().await.expect("server should accept");
            handle_connection(stream, peer).await
        });
        let mut client = TcpStream::connect(addr)
            .await
            .expect("client should connect");
        let body = br#"{"model":"gpt-5.5","input":"hello","stream":false}"#;
        let request = format!(
            "POST /v1/responses HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer test-local-key\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            String::from_utf8_lossy(body)
        );
        client
            .write_all(request.as_bytes())
            .await
            .expect("request should be written");

        let mut response = Vec::new();
        tokio::time::timeout(Duration::from_secs(1), client.read_to_end(&mut response))
            .await
            .expect("pool-unavailable JSON response should finish promptly")
            .expect("response read should succeed");
        let response_text = String::from_utf8_lossy(&response);
        assert!(
            response_text.contains("HTTP/1.1 200 OK"),
            "expected exhausted non-stream responses request to return protocol-shaped in-band JSON, got: {}",
            response_text
        );
        assert!(
            response_text.contains("application/json"),
            "expected JSON response, got: {}",
            response_text
        );
        assert!(
            response_text.contains("\"status\":\"completed\""),
            "expected completed pool_unavailable response payload, got: {}",
            response_text
        );
        assert!(
            response_text.contains("\"error\":null"),
            "expected completed local response without fatal error object, got: {}",
            response_text
        );
        assert!(
            response_text.contains("\"output\"") && response_text.contains("\"output_text\""),
            "expected local assistant output text, got: {}",
            response_text
        );
        assert!(
            response_text.contains("pool_unavailable")
                || response_text.contains("API 服务号池")
                || response_text.contains("暂无可调度账号"),
            "expected pool_unavailable explanation, got: {}",
            response_text
        );
        assert!(
            response_text.contains("synthetic_pool_unavailable_notice")
                && response_text.contains("retry_after_cooldown_or_start_new_task"),
            "expected local completion metadata to explain the Cockpit closure mode, got: {}",
            response_text
        );
        assert!(
            !response_text.contains("503 Service Unavailable"),
            "Codex-facing exhausted non-stream request must not expose transport 503: {}",
            response_text
        );

        let audit = fs::read_to_string(root.join(super::CODEX_LOCAL_ACCESS_AUDIT_FILE))
            .expect("audit should be written to isolated root");
        assert!(audit.contains("\"phase\":\"final_response\""));
        assert!(audit.contains("\"status\":200"));
        assert!(audit.contains("\"errorType\":\"pool_unavailable\""));
        assert!(audit.contains("\"streamState\":\"json_completed\""));
        assert!(audit.contains("\"outcome\":\"in_band_json_local_completion\""));
        assert!(!audit.contains("\"streamState\":\"json_failed\""));
        assert!(!audit.contains("\"outcome\":\"in_band_synthetic\""));

        let server_result = tokio::time::timeout(Duration::from_secs(1), server)
            .await
            .expect("server task should finish")
            .expect("server task should not panic");
        assert!(server_result.is_ok(), "server error: {:?}", server_result);
        {
            let mut runtime = gateway_runtime().lock().await;
            *runtime = super::GatewayRuntime::default();
        }
        reset_local_api_backpressure_for_tests();
        reset_active_stream_leases_for_tests();
        match previous_root {
            Some(value) => std::env::set_var(super::CODEX_LOCAL_ACCESS_DATA_ROOT_ENV, value),
            None => std::env::remove_var(super::CODEX_LOCAL_ACCESS_DATA_ROOT_ENV),
        }
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn pool_unavailable_non_responses_keeps_http_error_contract() {
        let error = ProxyDispatchError {
            status: StatusCode::SERVICE_UNAVAILABLE.as_u16(),
            message: "API 服务号池暂无可调度账号".to_string(),
            account_id: None,
            account_email: None,
            retry_after: None,
            defer_until_pool_available: false,
        };
        assert_eq!(
            classify_codex_api_failure(Some(error.status), error.message.as_str()),
            "pool_unavailable"
        );
        let request = ParsedRequest {
            method: "GET".to_string(),
            target: "/v1/models".to_string(),
            headers: HashMap::new(),
            body: Vec::new(),
            gateway_request_id: "gw-test-13".to_string(),
        };
        assert!(!should_write_in_band_pool_unavailable_response(
            &request,
            &GatewayResponseAdapter::Passthrough {
                request_is_stream: false
            },
            &error
        ));
    }

    #[test]
    fn pool_unavailable_sticky_responses_keeps_http_error_contract() {
        let error = ProxyDispatchError {
            status: StatusCode::SERVICE_UNAVAILABLE.as_u16(),
            message: "API 服务号池暂无可调度账号".to_string(),
            account_id: None,
            account_email: None,
            retry_after: None,
            defer_until_pool_available: false,
        };

        let turn_state_request = ParsedRequest {
            method: "POST".to_string(),
            target: "/v1/responses".to_string(),
            headers: HashMap::from([(
                "x-codex-turn-state".to_string(),
                "turn-state-token".to_string(),
            )]),
            body: br#"{"model":"gpt-5.5","input":"continue"}"#.to_vec(),
            gateway_request_id: "gw-test-sticky-pool-1".to_string(),
        };
        assert!(!should_write_in_band_pool_unavailable_response(
            &turn_state_request,
            &GatewayResponseAdapter::Passthrough {
                request_is_stream: true
            },
            &error
        ));

        let continuation_request = ParsedRequest {
            method: "POST".to_string(),
            target: "/v1/responses".to_string(),
            headers: HashMap::new(),
            body: br#"{"model":"gpt-5.5","previous_response_id":"resp-1","input":"continue"}"#
                .to_vec(),
            gateway_request_id: "gw-test-sticky-pool-2".to_string(),
        };
        assert!(!should_write_in_band_pool_unavailable_response(
            &continuation_request,
            &GatewayResponseAdapter::Passthrough {
                request_is_stream: false
            },
            &error
        ));

        let metadata_only_request = ParsedRequest {
            method: "POST".to_string(),
            target: "/v1/responses".to_string(),
            headers: HashMap::from([(
                "x-codex-turn-metadata".to_string(),
                r#"{"turn_id":"turn-observable-only"}"#.to_string(),
            )]),
            body: br#"{"model":"gpt-5.5","input":"new independent request"}"#.to_vec(),
            gateway_request_id: "gw-test-sticky-pool-3".to_string(),
        };
        assert!(
            should_write_in_band_pool_unavailable_response(
                &metadata_only_request,
                &GatewayResponseAdapter::Passthrough {
                    request_is_stream: true
                },
                &error
            ),
            "x-codex-turn-metadata is official observability lineage, not a hard-affinity boundary"
        );
    }

    #[test]
    fn pool_unavailable_message_reports_all_exhausted_without_reset_hint() {
        let now = 1_700_000_000_000;
        let mut registry = empty_health_registry(now);
        for account_id in ["exhausted-a", "exhausted-b"] {
            registry.accounts.insert(
                account_id.to_string(),
                CodexLocalAccessAccountHealth {
                    status: CodexLocalAccessAccountHealthStatus::Exhausted,
                    updated_at: now,
                    ..CodexLocalAccessAccountHealth::default()
                },
            );
        }

        let summary = summarize_pool_unavailability(
            &registry,
            &["exhausted-a".to_string(), "exhausted-b".to_string()],
            Some("gpt-5.5"),
            now,
        );

        assert_eq!(summary.total_count, 2);
        assert_eq!(summary.schedulable_count, 0);
        assert_eq!(summary.exhausted_count, 2);
        assert_eq!(summary.nearest_wait, None);
        assert!(should_use_pool_unavailable_summary(&summary));
        assert_eq!(
            status_for_pool_unavailable(&summary),
            StatusCode::SERVICE_UNAVAILABLE.as_u16()
        );

        let message = build_pool_unavailable_message("gpt-5.5", &summary);
        assert!(message.contains("额度均已耗尽"));
        assert!(message.contains("未拿到上游 reset 时间"));
        assert!(message.contains("调整号池"));
        assert!(!message.contains("切换账号"));
    }

    #[test]
    fn pool_unavailable_message_reports_manual_required_accounts() {
        let now = 1_700_000_000_000;
        let mut registry = empty_health_registry(now);
        for account_id in ["manual-a", "manual-b"] {
            registry.accounts.insert(
                account_id.to_string(),
                CodexLocalAccessAccountHealth {
                    status: CodexLocalAccessAccountHealthStatus::ManualRequired,
                    manual_required: true,
                    updated_at: now,
                    ..CodexLocalAccessAccountHealth::default()
                },
            );
        }

        let summary = summarize_pool_unavailability(
            &registry,
            &["manual-a".to_string(), "manual-b".to_string()],
            Some("gpt-5.5"),
            now,
        );

        assert_eq!(summary.total_count, 2);
        assert_eq!(summary.schedulable_count, 0);
        assert_eq!(summary.manual_required_count, 2);
        assert!(should_use_pool_unavailable_summary(&summary));
        assert_eq!(
            status_for_pool_unavailable(&summary),
            StatusCode::UNAUTHORIZED.as_u16()
        );

        let message = build_pool_unavailable_message("gpt-5.5", &summary);
        assert!(message.contains("均需人工处理"));
        assert!(message.contains("重新登录或风控验证"));
        assert_eq!(
            classify_codex_api_failure(Some(status_for_pool_unavailable(&summary)), &message),
            "auth_failed"
        );

        let error = ProxyDispatchError {
            status: status_for_pool_unavailable(&summary),
            message,
            account_id: None,
            account_email: None,
            retry_after: None,
            defer_until_pool_available: false,
        };
        let request = ParsedRequest {
            method: "POST".to_string(),
            target: "/v1/responses".to_string(),
            headers: HashMap::new(),
            body: Vec::new(),
            gateway_request_id: "gw-test-14".to_string(),
        };
        assert!(!should_write_in_band_pool_unavailable_response(
            &request,
            &GatewayResponseAdapter::Passthrough {
                request_is_stream: true,
            },
            &error
        ));
    }

    #[test]
    fn pool_unavailable_summary_uses_in_request_failover_health_updates() {
        let now = 1_700_000_000_000;
        let mut registry = empty_health_registry(now);
        let classified = classify_codex_upstream_error(
            StatusCode::TOO_MANY_REQUESTS,
            None,
            r#"{"error":{"type":"insufficient_quota"}}"#,
        );

        for account_id in ["failover-a", "failover-b"] {
            update_health_registry_from_classified_error(
                &mut registry,
                account_id,
                Some("gpt-5.5"),
                Some("req-pool"),
                &classified,
                now,
            );
        }

        let summary = summarize_pool_unavailability(
            &registry,
            &["failover-a".to_string(), "failover-b".to_string()],
            Some("gpt-5.5"),
            now,
        );

        assert_eq!(summary.schedulable_count, 0);
        assert_eq!(summary.exhausted_count, 2);
        assert!(should_use_pool_unavailable_summary(&summary));
        assert!(build_pool_unavailable_message("gpt-5.5", &summary).contains("额度均已耗尽"));
    }

    #[test]
    fn manual_recovery_clears_account_and_model_cooldowns() {
        let now = 1_700_000_000_000;
        let mut registry = empty_health_registry(now);
        registry.accounts.insert(
            "account-cooling".to_string(),
            CodexLocalAccessAccountHealth {
                status: CodexLocalAccessAccountHealthStatus::CoolingDown,
                cooldown_until_ms: Some(now + 60_000),
                exhausted_at_ms: Some(now - 1),
                estimated_reset_at_ms: Some(now + 60_000),
                estimated_remaining_percentage: Some(0),
                last_observed_remaining_percentage: Some(0),
                reset_source: Some("upstream_reset_hint".to_string()),
                confidence: Some("confirmed".to_string()),
                manual_required: true,
                last_status: Some(429),
                last_error_type: Some("usage_limit_reached".to_string()),
                last_request_id: Some("req-cooling".to_string()),
                updated_at: now,
                ..CodexLocalAccessAccountHealth::default()
            },
        );
        registry.model_cooldowns.insert(
            health_registry_model_key("account-cooling", "gpt-5.5"),
            CodexLocalAccessModelCooldown {
                account_id: "account-cooling".to_string(),
                model: "gpt-5.5".to_string(),
                cooldown_until_ms: now + 60_000,
                last_error_type: Some("model_capacity".to_string()),
                last_request_id: Some("req-model".to_string()),
                updated_at: now,
            },
        );

        assert!(recover_health_registry_account(
            &mut registry,
            "account-cooling",
            None,
            now + 1
        ));

        let account = registry
            .accounts
            .get("account-cooling")
            .expect("account health should still exist");
        assert_eq!(
            account.status,
            CodexLocalAccessAccountHealthStatus::EstimatedAvailable
        );
        assert!(!account.manual_required);
        assert_eq!(account.cooldown_until_ms, None);
        assert_eq!(account.exhausted_at_ms, None);
        assert_eq!(account.estimated_reset_at_ms, None);
        assert_eq!(account.estimated_remaining_percentage, Some(100));
        assert_eq!(account.reset_source.as_deref(), Some("manual_recovery"));
        assert_eq!(account.confidence.as_deref(), Some("manual"));
        assert!(registry.model_cooldowns.is_empty());
        assert!(health_registry_account_is_schedulable(
            &registry,
            "account-cooling",
            Some("gpt-5.5"),
            now + 1
        ));
    }

    #[test]
    fn manual_recovery_clears_only_selected_model_cooldown() {
        let now = 1_700_000_000_000;
        let mut registry = empty_health_registry(now);
        registry.accounts.insert(
            "account-model".to_string(),
            CodexLocalAccessAccountHealth {
                status: CodexLocalAccessAccountHealthStatus::Healthy,
                updated_at: now,
                ..CodexLocalAccessAccountHealth::default()
            },
        );
        for model in ["gpt-5.5", "gpt-5.4"] {
            registry.model_cooldowns.insert(
                health_registry_model_key("account-model", model),
                CodexLocalAccessModelCooldown {
                    account_id: "account-model".to_string(),
                    model: model.to_string(),
                    cooldown_until_ms: now + 60_000,
                    last_error_type: Some("upstream_rate_limit".to_string()),
                    last_request_id: Some(format!("req-{model}")),
                    updated_at: now,
                },
            );
        }

        assert!(recover_health_registry_account(
            &mut registry,
            "account-model",
            Some("gpt-5.5"),
            now + 1
        ));

        assert!(!registry
            .model_cooldowns
            .contains_key(&health_registry_model_key("account-model", "gpt-5.5")));
        assert!(registry
            .model_cooldowns
            .contains_key(&health_registry_model_key("account-model", "gpt-5.4")));
        assert!(!health_registry_account_is_schedulable(
            &registry,
            "account-model",
            Some("gpt-5.4"),
            now + 1
        ));
        assert!(health_registry_account_is_schedulable(
            &registry,
            "account-model",
            Some("gpt-5.5"),
            now + 1
        ));
    }

    #[test]
    fn manual_pause_marks_account_disabled_and_prunes_affinity() {
        let now = 1_700_000_000_000;
        let mut registry = empty_health_registry(now);
        registry.accounts.insert(
            "account-paused".to_string(),
            CodexLocalAccessAccountHealth {
                status: CodexLocalAccessAccountHealthStatus::Healthy,
                last_success_at_ms: Some(now - 1),
                api_service_success_count: 3,
                updated_at: now,
                ..CodexLocalAccessAccountHealth::default()
            },
        );
        upsert_process_sticky_binding(&mut registry, "account-paused", now);
        let request = ParsedRequest {
            method: "POST".to_string(),
            target: "/v1/responses".to_string(),
            headers: HashMap::from([
                (
                    "x-client-request-id".to_string(),
                    "thread-request-id".to_string(),
                ),
                (
                    "x-codex-turn-metadata".to_string(),
                    r#"{"turn_id":"turn-manual-pause"}"#.to_string(),
                ),
                (
                    "x-codex-turn-state".to_string(),
                    "turn-manual-pause-state".to_string(),
                ),
            ]),
            body: Vec::new(),
            gateway_request_id: "gw-test-15".to_string(),
        };
        assert!(upsert_request_affinity_binding(
            &mut registry,
            &request,
            "account-paused",
            now
        ));
        registry.model_cooldowns.insert(
            health_registry_model_key("account-paused", "gpt-5.5"),
            CodexLocalAccessModelCooldown {
                account_id: "account-paused".to_string(),
                model: "gpt-5.5".to_string(),
                cooldown_until_ms: now + 60_000,
                last_error_type: Some("model_capacity".to_string()),
                last_request_id: Some("req-model-pause".to_string()),
                updated_at: now,
            },
        );

        assert!(pause_health_registry_account(
            &mut registry,
            "account-paused",
            now + 1
        ));

        let account = registry
            .accounts
            .get("account-paused")
            .expect("paused account health should remain visible");
        assert_eq!(
            account.status,
            CodexLocalAccessAccountHealthStatus::Disabled
        );
        assert_eq!(account.reset_source.as_deref(), Some("manual_pause"));
        assert_eq!(account.confidence.as_deref(), Some("manual"));
        assert_eq!(account.last_error_type.as_deref(), Some("manual_paused"));
        assert!(!health_registry_account_is_schedulable(
            &registry,
            "account-paused",
            Some("gpt-5.5"),
            now + 1
        ));
        assert!(registry.sticky_bindings.is_empty());
        assert!(registry.request_affinity.is_empty());
        assert!(registry.model_cooldowns.is_empty());
    }

    #[test]
    fn health_summary_exposes_scoped_account_health_views() {
        let now = 1_700_000_000_000;
        let mut registry = empty_health_registry(now);
        registry.accounts.insert(
            "account-auth".to_string(),
            CodexLocalAccessAccountHealth {
                status: CodexLocalAccessAccountHealthStatus::ManualRequired,
                manual_required: true,
                last_status: Some(401),
                last_error_type: Some("auth_error".to_string()),
                last_provider_code: Some("invalid_token".to_string()),
                last_request_id: Some("req-auth-secret".to_string()),
                updated_at: now,
                ..CodexLocalAccessAccountHealth::default()
            },
        );
        registry.accounts.insert(
            "account-outside".to_string(),
            CodexLocalAccessAccountHealth {
                status: CodexLocalAccessAccountHealthStatus::ManualRequired,
                manual_required: true,
                last_status: Some(401),
                updated_at: now,
                ..CodexLocalAccessAccountHealth::default()
            },
        );
        registry.model_cooldowns.insert(
            health_registry_model_key("account-auth", "gpt-5.5"),
            CodexLocalAccessModelCooldown {
                account_id: "account-auth".to_string(),
                model: "gpt-5.5".to_string(),
                cooldown_until_ms: now + 60_000,
                last_error_type: Some("model_capacity".to_string()),
                last_request_id: Some("req-model".to_string()),
                updated_at: now,
            },
        );

        let account_ids = vec!["account-auth".to_string(), "account-missing".to_string()];
        let summary = build_health_summary_from_registry_for_accounts(&registry, now, &account_ids);

        assert_eq!(summary.manual_required_count, 1);
        assert_eq!(summary.healthy_count, 1);
        assert_eq!(summary.active_model_cooldown_count, 1);
        assert_eq!(summary.accounts.len(), 2);
        assert!(summary
            .accounts
            .iter()
            .all(|view| view.account_id != "account-outside"));

        let auth = summary
            .accounts
            .iter()
            .find(|view| view.account_id == "account-auth")
            .expect("auth account health should be exposed");
        assert_eq!(
            auth.status,
            CodexLocalAccessAccountHealthStatus::ManualRequired
        );
        assert!(auth.manual_required);
        assert_eq!(auth.last_status, Some(401));
        assert_eq!(auth.last_error_type.as_deref(), Some("auth_error"));
        assert_eq!(auth.last_provider_code.as_deref(), Some("invalid_token"));
        assert_eq!(auth.active_model_cooldown_count, 1);
        assert_eq!(auth.nearest_model_cooldown_until_ms, Some(now + 60_000));

        let missing = summary
            .accounts
            .iter()
            .find(|view| view.account_id == "account-missing")
            .expect("missing scoped account should default to healthy");
        assert_eq!(missing.status, CodexLocalAccessAccountHealthStatus::Healthy);
        assert!(!missing.manual_required);
    }

    #[test]
    fn manual_recovery_restores_manually_paused_account_locally() {
        let now = 1_700_000_000_000;
        let mut registry = empty_health_registry(now);

        assert!(pause_health_registry_account(
            &mut registry,
            "account-paused",
            now
        ));
        assert!(recover_health_registry_account(
            &mut registry,
            "account-paused",
            None,
            now + 1
        ));

        let account = registry
            .accounts
            .get("account-paused")
            .expect("recovered account health should remain visible");
        assert_eq!(
            account.status,
            CodexLocalAccessAccountHealthStatus::EstimatedAvailable
        );
        assert_eq!(account.reset_source.as_deref(), Some("manual_recovery"));
        assert_eq!(account.confidence.as_deref(), Some("manual"));
        assert_eq!(account.last_error_type, None);
        assert!(health_registry_account_is_schedulable(
            &registry,
            "account-paused",
            Some("gpt-5.5"),
            now + 1
        ));
    }

    #[test]
    fn manual_pause_and_recovery_audit_events_are_redacted() {
        let _guard = LOCAL_ACCESS_ENV_TEST_LOCK
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let previous = std::env::var_os(super::CODEX_LOCAL_ACCESS_DATA_ROOT_ENV);
        let root = std::env::temp_dir().join(format!(
            "cockpit-local-access-manual-audit-test-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let _ = fs::remove_dir_all(&root);
        std::env::set_var(super::CODEX_LOCAL_ACCESS_DATA_ROOT_ENV, &root);

        record_manual_pause_audit_event("account-secret-user@example.com", true);
        record_manual_recovery_audit_event(
            "account-secret-user@example.com",
            Some("gpt-5.5"),
            true,
        );

        let audit = fs::read_to_string(root.join(super::CODEX_LOCAL_ACCESS_AUDIT_FILE))
            .expect("manual audit log should be written");
        assert!(audit.contains("\"phase\":\"manual_pause\""));
        assert!(audit.contains("\"phase\":\"manual_recovery\""));
        assert!(audit.contains("\"errorType\":\"manual_paused\""));
        assert!(audit.contains("\"accountHash\":\"sha256:"));
        for secret in ["account-secret-user@example.com", "@", "sk-"] {
            assert!(!audit.contains(secret), "manual audit leaked {secret}");
        }

        match previous {
            Some(value) => std::env::set_var(super::CODEX_LOCAL_ACCESS_DATA_ROOT_ENV, value),
            None => std::env::remove_var(super::CODEX_LOCAL_ACCESS_DATA_ROOT_ENV),
        }
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn health_summary_counts_statuses_and_redacts_sticky_account() {
        let now = 1_700_000_000_000;
        let mut registry = empty_health_registry(now);
        registry.accounts.insert(
            "account-secret-user@example.com".to_string(),
            CodexLocalAccessAccountHealth {
                status: CodexLocalAccessAccountHealthStatus::Healthy,
                updated_at: now,
                ..CodexLocalAccessAccountHealth::default()
            },
        );
        registry.accounts.insert(
            "cooling-account".to_string(),
            CodexLocalAccessAccountHealth {
                status: CodexLocalAccessAccountHealthStatus::CoolingDown,
                cooldown_until_ms: Some(now + 60_000),
                last_error_type: Some("usage_limit_reached".to_string()),
                last_status: Some(429),
                last_request_id: Some("req-health".to_string()),
                updated_at: now + 1,
                ..CodexLocalAccessAccountHealth::default()
            },
        );
        registry.accounts.insert(
            "manual-account".to_string(),
            CodexLocalAccessAccountHealth {
                status: CodexLocalAccessAccountHealthStatus::ManualRequired,
                manual_required: true,
                updated_at: now,
                ..CodexLocalAccessAccountHealth::default()
            },
        );
        upsert_process_sticky_binding(&mut registry, "account-secret-user@example.com", now);

        let summary = build_health_summary_from_registry(&registry, now);

        assert_eq!(summary.healthy_count, 1);
        assert_eq!(summary.cooling_count, 1);
        assert_eq!(summary.manual_required_count, 1);
        assert_eq!(summary.active_model_cooldown_count, 0);
        assert_eq!(
            summary.last_error_type.as_deref(),
            Some("usage_limit_reached")
        );
        assert_eq!(summary.last_status, Some(429));
        assert_eq!(summary.last_request_id.as_deref(), Some("req-health"));
        assert_eq!(summary.nearest_cooldown_until_ms, Some(now + 60_000));
        assert!(summary
            .sticky_account_hash
            .as_deref()
            .unwrap_or_default()
            .starts_with("sha256:"));

        let serialized = serde_json::to_string(&summary).expect("summary should serialize");
        for secret in [
            "account-secret-user@example.com",
            "cooling-account",
            "manual-account",
        ] {
            assert!(
                !serialized.contains(secret),
                "health summary leaked {secret}"
            );
        }
    }

    #[test]
    fn scoped_health_summary_ignores_accounts_outside_current_pool() {
        let now = 1_700_000_000_000;
        let mut registry = empty_health_registry(now);
        registry.accounts.insert(
            "current-healthy".to_string(),
            CodexLocalAccessAccountHealth {
                status: CodexLocalAccessAccountHealthStatus::Healthy,
                updated_at: now,
                ..CodexLocalAccessAccountHealth::default()
            },
        );
        registry.accounts.insert(
            "old-cooling".to_string(),
            CodexLocalAccessAccountHealth {
                status: CodexLocalAccessAccountHealthStatus::CoolingDown,
                cooldown_until_ms: Some(now + 60_000),
                last_error_type: Some("usage_limit_reached".to_string()),
                last_status: Some(429),
                last_request_id: Some("old-req".to_string()),
                updated_at: now + 1,
                ..CodexLocalAccessAccountHealth::default()
            },
        );
        registry.model_cooldowns.insert(
            health_registry_model_key("old-cooling", "gpt-5.5"),
            CodexLocalAccessModelCooldown {
                account_id: "old-cooling".to_string(),
                model: "gpt-5.5".to_string(),
                cooldown_until_ms: now + 60_000,
                last_error_type: Some("usage_limit_reached".to_string()),
                last_request_id: Some("old-req".to_string()),
                updated_at: now + 1,
                ..CodexLocalAccessModelCooldown::default()
            },
        );
        upsert_process_sticky_binding(&mut registry, "old-cooling", now);

        let account_ids = vec![
            "current-healthy".to_string(),
            "current-no-record".to_string(),
        ];
        let summary = build_health_summary_from_registry_for_accounts(&registry, now, &account_ids);

        assert_eq!(summary.healthy_count, 2);
        assert_eq!(summary.estimated_available_count, 0);
        assert_eq!(summary.cooling_count, 0);
        assert_eq!(summary.active_model_cooldown_count, 0);
        assert_eq!(summary.nearest_cooldown_until_ms, None);
        assert_eq!(summary.last_error_type, None);
        assert_eq!(summary.last_status, None);
        assert_eq!(summary.last_request_id, None);
        assert_eq!(summary.sticky_account_hash, None);
    }

    #[test]
    fn health_registry_parse_error_is_fail_closed() {
        let path = temp_health_registry_path("corrupt");
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("temp dir should be created");
        }
        fs::write(&path, "{not valid json").expect("corrupt health file should be written");

        let err = load_health_registry_from_path(&path)
            .expect_err("corrupt health registry must not silently open scheduling");
        assert!(err.contains("解析 API 服务健康状态失败"));
    }

    #[test]
    fn health_registry_save_and_load_roundtrips_atomically() {
        let path = temp_health_registry_path("roundtrip");
        let mut registry = empty_health_registry(1_700_000_000_000);
        let classified = classify_codex_upstream_error(
            StatusCode::TOO_MANY_REQUESTS,
            None,
            r#"{"error":{"type":"usage_limit_reached","resets_in_seconds":30}}"#,
        );
        update_health_registry_from_classified_error(
            &mut registry,
            "account-4",
            Some("gpt-5.5"),
            Some("req-4"),
            &classified,
            1_700_000_000_000,
        );

        save_health_registry_to_path(&path, &registry).expect("health registry should save");
        let loaded = load_health_registry_from_path(&path).expect("health registry should load");

        assert_eq!(loaded.schema_version, registry.schema_version);
        assert!(loaded.accounts.contains_key("account-4"));
        assert!(!path.with_extension("json.tmp").exists());
    }

    #[test]
    fn audit_event_uses_redacted_structured_metadata_only() {
        let request = ParsedRequest {
            method: "POST".to_string(),
            target: "/v1/chat/completions?api_key=sk-query-secret".to_string(),
            headers: HashMap::from([
                ("x-client-request-id".to_string(), "req-audit-1".to_string()),
                ("authorization".to_string(), "Bearer raw-secret".to_string()),
            ]),
            body: br#"{"messages":[{"role":"user","content":"raw prompt"}],"model":"gpt-5.5"}"#
                .to_vec(),
            gateway_request_id: "gw-test-16".to_string(),
        };
        let context = build_audit_context(&request, Some("account-secret-user@example.com"));
        let event = build_audit_event(
            1_700_000_000_000,
            &context,
            "classifier",
            Some(429),
            Some("usage_limit_reached"),
            None,
            Some("cooldown"),
            BTreeMap::from([
                ("raw_body".to_string(), "raw prompt sk-secret".to_string()),
                ("authorization".to_string(), "Bearer raw-secret".to_string()),
                ("retry_after_ms".to_string(), "60000".to_string()),
            ]),
        );

        assert_eq!(event.request_id, "req-audit-1");
        assert_eq!(event.route, "/v1/chat/completions");
        assert_eq!(event.model, "gpt-5.5");
        assert!(event.account_hash.starts_with("sha256:"));
        assert_eq!(
            event.detail.get("request_id_source").map(String::as_str),
            Some("client_request_id")
        );
        assert_eq!(
            event.detail.get("gateway_request_id").map(String::as_str),
            Some("gw-test-16")
        );
        assert_eq!(
            event.detail.get("retry_after_ms").map(String::as_str),
            Some("60000")
        );

        let serialized = serde_json::to_string(&event).expect("audit event should serialize");
        for secret in [
            "raw prompt",
            "sk-secret",
            "raw-secret",
            "user@example.com",
            "account-secret-user@example.com",
        ] {
            assert!(!serialized.contains(secret), "audit event leaked {secret}");
        }
    }

    #[test]
    fn audit_context_marks_turn_lineage_continuation_and_compaction_without_raw_ids() {
        let metadata = r#"{"turn_id":"turn-compact"}"#;
        let request = ParsedRequest {
            method: "POST".to_string(),
            target: "/v1/responses".to_string(),
            headers: HashMap::from([("x-codex-turn-metadata".to_string(), metadata.to_string())]),
            body: br#"{"model":"gpt-5.5","previous_response_id":"resp_secret_previous","input":[{"type":"message","role":"user","content":[{"type":"input_text","text":"hello"}]},{"type":"compaction_trigger"}]}"#.to_vec(),
            gateway_request_id: "gw-lineage-1".to_string(),
        };

        let context = build_audit_context(&request, Some("account-lineage@example.com"));
        let event = build_audit_event(
            1_700_000_000_001,
            &context,
            "listener",
            None,
            None,
            None,
            Some("accepted"),
            BTreeMap::new(),
        );

        assert_eq!(
            event.detail.get("gateway_request_id").map(String::as_str),
            Some("gw-lineage-1")
        );
        assert_eq!(
            event.detail.get("turn_lineage_source").map(String::as_str),
            Some("codex_turn_metadata_turn_id")
        );
        assert!(event
            .detail
            .get("turn_lineage_id")
            .is_some_and(|value| value.starts_with("x-codex-turn-metadata.turn_id:sha256:")));
        assert!(event
            .detail
            .get("previous_response_id_hash")
            .is_some_and(|value| value.starts_with("response:sha256:")));
        assert_eq!(
            event.detail.get("is_continuation").map(String::as_str),
            Some("true")
        );
        assert_eq!(
            event
                .detail
                .get("is_auto_compact_candidate")
                .map(String::as_str),
            Some("true")
        );

        let serialized = serde_json::to_string(&event).expect("audit event should serialize");
        assert!(!serialized.contains("turn-compact"));
        assert!(!serialized.contains("resp_secret_previous"));
        assert!(!serialized.contains("hello"));
        assert!(!serialized.contains("account-lineage@example.com"));
    }

    #[test]
    fn audit_event_append_writes_jsonl_and_rotates_by_size() {
        let path = temp_audit_path("rotate");
        let context = AuditContext {
            request_id: "req-rotate".to_string(),
            request_id_source: "test".to_string(),
            route: "/v1/responses".to_string(),
            model: "gpt-5.5".to_string(),
            account_hash: "sha256:abc123abc123".to_string(),
            gateway_request_id: "gw-context-test-5".to_string(),
            turn_lineage_id: None,
            turn_lineage_source: None,
            previous_response_id_hash: None,
            is_continuation: false,
            is_auto_compact_candidate: false,
        };
        let first = build_audit_event(
            1,
            &context,
            "listener",
            None,
            None,
            None,
            Some("accepted"),
            BTreeMap::new(),
        );
        let second = build_audit_event(
            2,
            &context,
            "stream_write",
            Some(200),
            None,
            Some("headers_written"),
            Some("ok"),
            BTreeMap::new(),
        );

        append_audit_event_to_path(&path, &first, usize::MAX).expect("first append should work");
        append_audit_event_to_path(&path, &second, 1).expect("second append should rotate");

        let active = fs::read_to_string(&path).expect("active audit log should exist");
        assert!(active.contains("\"phase\":\"stream_write\""));
        assert!(!active.contains("\"phase\":\"listener\""));

        let rotated =
            fs::read_to_string(path.with_extension("jsonl.1")).expect("rotated audit log exists");
        assert!(rotated.contains("\"phase\":\"listener\""));
    }

    #[test]
    fn audit_event_append_rotates_when_event_day_changes() {
        let path = temp_audit_path("rotate-day");
        let context = AuditContext {
            request_id: "req-rotate-day".to_string(),
            request_id_source: "test".to_string(),
            route: "/v1/responses".to_string(),
            model: "gpt-5.5".to_string(),
            account_hash: "sha256:day123day123".to_string(),
            gateway_request_id: "gw-context-test-6".to_string(),
            turn_lineage_id: None,
            turn_lineage_source: None,
            previous_response_id_hash: None,
            is_continuation: false,
            is_auto_compact_candidate: false,
        };
        let first = build_audit_event(
            DAY_WINDOW_MS,
            &context,
            "listener",
            None,
            None,
            None,
            Some("accepted"),
            BTreeMap::new(),
        );
        let second = build_audit_event(
            DAY_WINDOW_MS * 2,
            &context,
            "stream_write",
            Some(200),
            None,
            Some("headers_written"),
            Some("ok"),
            BTreeMap::new(),
        );

        append_audit_event_to_path(&path, &first, usize::MAX).expect("first append should work");
        append_audit_event_to_path(&path, &second, usize::MAX)
            .expect("second append should rotate by day");

        let active = fs::read_to_string(&path).expect("active audit log should exist");
        assert!(active.contains("\"phase\":\"stream_write\""));
        assert!(!active.contains("\"phase\":\"listener\""));

        let rotated =
            fs::read_to_string(path.with_extension("jsonl.1")).expect("rotated audit log exists");
        assert!(rotated.contains("\"phase\":\"listener\""));
    }

    #[test]
    fn audit_timestamp_reader_tolerates_concatenated_jsonl_objects() {
        let path = temp_audit_path("concatenated");
        let context = AuditContext {
            request_id: "req-concat".to_string(),
            request_id_source: "test".to_string(),
            route: "/v1/responses".to_string(),
            model: "gpt-5.5".to_string(),
            account_hash: "sha256:concat123".to_string(),
            gateway_request_id: "gw-context-test-7".to_string(),
            turn_lineage_id: None,
            turn_lineage_source: None,
            previous_response_id_hash: None,
            is_continuation: false,
            is_auto_compact_candidate: false,
        };
        let first = build_audit_event(
            DAY_WINDOW_MS,
            &context,
            "listener",
            None,
            None,
            None,
            Some("accepted"),
            BTreeMap::new(),
        );
        let second = build_audit_event(
            DAY_WINDOW_MS + 1,
            &context,
            "stream_write",
            Some(200),
            None,
            Some("headers_written"),
            Some("ok"),
            BTreeMap::new(),
        );
        let content = format!(
            "{}{}\n",
            serde_json::to_string(&first).expect("first event should serialize"),
            serde_json::to_string(&second).expect("second event should serialize")
        );
        fs::write(&path, content).expect("test audit log should be written");

        assert_eq!(
            first_audit_timestamp_from_path(&path).expect("timestamp should be readable"),
            Some(DAY_WINDOW_MS)
        );

        let _ = fs::remove_file(path);
    }

    #[test]
    fn audit_status_is_exposed_in_health_summary() {
        let mut summary = CodexLocalAccessHealthSummary::default();
        let status = AuditTrailStatus {
            degraded: true,
            error: Some("audit path unavailable".to_string()),
            degraded_at_ms: Some(1_700_000_000_000),
        };

        apply_audit_trail_status_to_health_summary(&mut summary, &status);

        assert!(summary.audit_degraded);
        assert_eq!(
            summary.audit_error.as_deref(),
            Some("audit path unavailable")
        );
        assert_eq!(summary.audit_degraded_at_ms, Some(1_700_000_000_000));
    }

    #[test]
    fn audit_stream_write_events_have_boundary_states() {
        let context = AuditContext {
            request_id: "req-stream".to_string(),
            request_id_source: "test".to_string(),
            route: "/v1/responses".to_string(),
            model: "gpt-5.5".to_string(),
            account_hash: "sha256:stream123456".to_string(),
            gateway_request_id: "gw-context-test-8".to_string(),
            turn_lineage_id: None,
            turn_lineage_source: None,
            previous_response_id_hash: None,
            is_continuation: false,
            is_auto_compact_candidate: false,
        };
        let headers = build_audit_event(
            1,
            &context,
            "stream_write",
            Some(200),
            None,
            Some("headers_written"),
            Some("ok"),
            BTreeMap::new(),
        );
        let first_chunk = build_audit_event(
            2,
            &context,
            "stream_write",
            Some(200),
            None,
            Some("first_chunk_written"),
            Some("ok"),
            BTreeMap::new(),
        );

        assert_eq!(headers.stream_state.as_deref(), Some("headers_written"));
        assert_eq!(
            first_chunk.stream_state.as_deref(),
            Some("first_chunk_written")
        );
    }

    fn temp_health_registry_path(name: &str) -> PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "cockpit-local-health-{name}-{}-{}.json",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        path
    }

    fn temp_audit_path(name: &str) -> PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "cockpit-local-audit-{name}-{}-{}.jsonl",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        path
    }

    #[test]
    fn retries_next_account_for_transient_upstream_status() {
        assert!(should_try_next_account(
            StatusCode::SERVICE_UNAVAILABLE,
            "upstream temporarily unavailable"
        ));
        assert!(should_try_next_account(
            StatusCode::BAD_GATEWAY,
            "gateway error"
        ));
    }

    #[test]
    fn retries_single_account_for_transient_upstream_status() {
        assert!(should_retry_single_account_upstream_status(
            StatusCode::SERVICE_UNAVAILABLE
        ));
        assert!(should_retry_single_account_upstream_status(
            StatusCode::GATEWAY_TIMEOUT
        ));
        assert!(!should_retry_single_account_upstream_status(
            StatusCode::TOO_MANY_REQUESTS
        ));
        assert!(!should_retry_single_account_upstream_status(
            StatusCode::FORBIDDEN
        ));
    }

    #[test]
    fn codex_api_failure_log_omits_sensitive_context() {
        let request = ParsedRequest {
            method: "POST".to_string(),
            target: "/v1/responses?api_key=sk-query-secret-1234567890".to_string(),
            headers: HashMap::from([
                (
                    "authorization".to_string(),
                    "Bearer sk-header-secret-1234567890".to_string(),
                ),
                (
                    "x-client-request-id".to_string(),
                    "req-test-123".to_string(),
                ),
            ]),
            body: br#"{"model":"gpt-5.5","prompt":"raw prompt text","content":"raw response text","access_token":"oauth-token-1234567890"}"#.to_vec(),
            gateway_request_id: "gw-test-17".to_string(),
        };

        let line = build_codex_api_failure_log(
            Some(&request),
            Some(429),
            Some("account-secret-id-1234567890"),
            Some(128),
            r#"upstream body {"error":{"message":"raw upstream text"},"email":"user.name@example.com","api_key":"sk-body-secret-1234567890"}"#,
        );

        assert!(line.contains("status=429"));
        assert!(line.contains("route=/v1/responses"));
        assert!(line.contains("model=gpt-5.5"));
        assert!(line.contains("request_id=req-test-123"));
        assert!(line.contains("account_hash="));

        for secret in [
            "sk-query-secret-1234567890",
            "sk-header-secret-1234567890",
            "sk-body-secret-1234567890",
            "oauth-token-1234567890",
            "account-secret-id-1234567890",
            "user.name@example.com",
            "raw prompt text",
            "raw response text",
            "raw upstream text",
        ] {
            assert!(
                !line.contains(secret),
                "failure log should not contain {secret}: {line}"
            );
        }
    }

    #[test]
    fn local_api_safety_config_defaults_for_legacy_collection() {
        let mut collection: CodexLocalAccessCollection = serde_json::from_value(json!({
            "enabled": true,
            "port": 45335,
            "apiKey": "ck-test",
            "routingStrategy": "auto",
            "restrictFreeAccounts": false,
            "followCurrentAccount": false,
            "accountIds": [],
            "createdAt": 1,
            "updatedAt": 2
        }))
        .expect("legacy collection should deserialize");

        let changed = normalize_local_api_safety_config(&mut collection);

        assert!(changed);
        assert_eq!(collection.safety_config.schema_version, 1);
        assert!(collection.safety_config.hardened_local_mode);
        assert_eq!(collection.safety_config.max_concurrent_requests, 1);
        assert_eq!(collection.safety_config.min_request_interval_seconds, 20);
        assert_eq!(collection.safety_config.max_queue_wait_seconds, 21);
        assert_eq!(collection.safety_config.request_timeout_seconds, 600);
        assert_eq!(
            collection.safety_config.max_request_body_mb,
            (MAX_HTTP_REQUEST_BYTES / (1024 * 1024)) as u32
        );
        assert_eq!(collection.safety_config.max_retries, 1);
        assert_eq!(collection.safety_config.max_retry_accounts, 2);
        assert_eq!(collection.safety_config.fallback_mode.as_str(), "disabled");
        assert!(collection.safety_config.logging.redact_sensitive_values);
        assert!(!collection.safety_config.logging.include_prompt_response);
        assert!(!collection.safety_config.logging.include_raw_upstream_body);
    }

    #[test]
    fn local_api_safety_config_future_schema_fails_closed() {
        let mut collection: CodexLocalAccessCollection = serde_json::from_value(json!({
            "enabled": true,
            "port": 45335,
            "apiKey": "ck-test",
            "routingStrategy": "auto",
            "restrictFreeAccounts": false,
            "followCurrentAccount": false,
            "accountIds": [],
            "createdAt": 1,
            "updatedAt": 2,
            "safetyConfig": {
                "schemaVersion": 999,
                "hardenedLocalMode": false,
                "maxConcurrentRequests": 999,
                "minRequestIntervalSeconds": 0,
                "maxQueueWaitSeconds": 0,
                "requestTimeoutSeconds": 1,
                "maxRequestBodyMb": 999,
                "maxRetries": 999,
                "maxRetryAccounts": 999,
                "fallbackMode": "aggressive",
                "logging": {
                    "redactSensitiveValues": false,
                    "includePromptResponse": true,
                    "includeRawUpstreamBody": true
                }
            }
        }))
        .expect("future collection should deserialize");

        let changed = normalize_local_api_safety_config(&mut collection);

        assert!(changed);
        assert_eq!(collection.safety_config.schema_version, 1);
        assert!(collection.safety_config.hardened_local_mode);
        assert_eq!(collection.safety_config.max_concurrent_requests, 1);
        assert_eq!(collection.safety_config.max_retry_accounts, 2);
        assert_eq!(collection.safety_config.fallback_mode.as_str(), "disabled");
        assert!(collection.safety_config.logging.redact_sensitive_values);
        assert!(!collection.safety_config.logging.include_prompt_response);
        assert!(!collection.safety_config.logging.include_raw_upstream_body);
    }

    #[test]
    fn local_api_safety_presets_expand_to_safe_contracts() {
        let maximum_safety =
            local_api_safety_config_for_preset(CodexLocalApiSafetyPresetId::MaximumSafety);
        assert!(maximum_safety.hardened_local_mode);
        assert_eq!(maximum_safety.max_concurrent_requests, 1);
        assert_eq!(maximum_safety.min_request_interval_seconds, 60);
        assert_eq!(maximum_safety.max_queue_wait_seconds, 61);
        assert_eq!(maximum_safety.max_retry_accounts, 2);
        assert_eq!(maximum_safety.fallback_mode.as_str(), "disabled");
        assert!(maximum_safety.logging.redact_sensitive_values);
        assert!(!maximum_safety.logging.include_prompt_response);
        assert!(!maximum_safety.logging.include_raw_upstream_body);

        let balanced =
            local_api_safety_config_for_preset(CodexLocalApiSafetyPresetId::BalancedSelfUse);
        assert_eq!(balanced.max_concurrent_requests, 1);
        assert_eq!(balanced.min_request_interval_seconds, 20);
        assert_eq!(balanced.max_queue_wait_seconds, 21);
        assert_eq!(balanced.max_retry_accounts, 2);
        assert_eq!(balanced.fallback_mode.as_str(), "disabled");

        let quota_drain =
            local_api_safety_config_for_preset(CodexLocalApiSafetyPresetId::QuotaDrainCareful);
        assert_eq!(quota_drain.max_concurrent_requests, 1);
        assert_eq!(quota_drain.min_request_interval_seconds, 30);
        assert_eq!(quota_drain.max_queue_wait_seconds, 31);
        assert_eq!(quota_drain.max_retry_accounts, 2);
        assert_eq!(quota_drain.fallback_mode.as_str(), "next_request_only");
    }

    #[test]
    fn applying_safety_preset_resets_collection_to_hardened_fill_first() {
        let mut collection = CodexLocalAccessCollection {
            enabled: true,
            port: 45335,
            api_key: "ck-test".to_string(),
            safety_config: CodexLocalApiSafetyConfig {
                hardened_local_mode: false,
                max_concurrent_requests: 4,
                min_request_interval_seconds: 1,
                max_retry_accounts: 2,
                fallback_mode: CodexLocalApiFallbackMode::NextRequestOnly,
                ..CodexLocalApiSafetyConfig::default()
            },
            routing_strategy: CodexLocalAccessRoutingStrategy::PlanHighFirst,
            restrict_free_accounts: false,
            follow_current_account: false,
            account_ids: vec!["acc-a".to_string(), "acc-b".to_string()],
            created_at: 1,
            updated_at: 2,
        };

        let changed = apply_local_api_safety_preset_to_collection(
            &mut collection,
            CodexLocalApiSafetyPresetId::MaximumSafety,
        );

        assert!(changed);
        assert_eq!(
            collection.routing_strategy,
            CodexLocalAccessRoutingStrategy::Auto
        );
        assert!(collection.safety_config.hardened_local_mode);
        assert_eq!(collection.safety_config.max_concurrent_requests, 1);
        assert_eq!(collection.safety_config.min_request_interval_seconds, 60);
        assert_eq!(collection.safety_config.max_queue_wait_seconds, 61);
        assert_eq!(collection.safety_config.max_retry_accounts, 2);
        assert_eq!(collection.safety_config.fallback_mode.as_str(), "disabled");
    }

    #[test]
    fn normalizes_queue_wait_to_cover_start_interval() {
        let mut collection = CodexLocalAccessCollection {
            enabled: true,
            port: 45335,
            api_key: "ck-test".to_string(),
            safety_config: CodexLocalApiSafetyConfig {
                min_request_interval_seconds: 20,
                max_queue_wait_seconds: 10,
                ..CodexLocalApiSafetyConfig::default()
            },
            routing_strategy: CodexLocalAccessRoutingStrategy::Auto,
            restrict_free_accounts: false,
            follow_current_account: false,
            account_ids: vec!["acc-a".to_string()],
            created_at: 1,
            updated_at: 2,
        };

        assert!(normalize_local_api_safety_config(&mut collection));
        assert_eq!(collection.safety_config.max_queue_wait_seconds, 21);
    }

    #[test]
    fn local_backpressure_wait_duration_respects_min_start_interval() {
        let mut config = CodexLocalApiSafetyConfig::default();
        config.max_concurrent_requests = 1;
        config.min_request_interval_seconds = 1;

        let now = std::time::Instant::now();
        let state = LocalApiBackpressureState {
            active_requests: 0,
            last_started_at: Some(now - Duration::from_millis(250)),
        };

        let wait = local_backpressure_wait_duration(&state, &config, now)
            .expect("recent request should enforce start interval");

        assert!(wait > Duration::from_millis(0));
        assert!(wait <= Duration::from_secs(1));
    }

    #[tokio::test]
    async fn local_backpressure_rejects_when_single_permit_is_busy_past_queue_wait() {
        let _guard = LOCAL_BACKPRESSURE_TEST_LOCK
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        reset_local_api_backpressure_for_tests();
        let mut config = CodexLocalApiSafetyConfig::default();
        config.max_concurrent_requests = 1;
        config.min_request_interval_seconds = 1;
        config.max_queue_wait_seconds = 1;

        let first = acquire_local_api_backpressure(&config)
            .await
            .expect("first request should acquire permit");
        let err = acquire_local_api_backpressure(&config)
            .await
            .expect_err("second request should time out while permit is busy");

        assert_eq!(err.status, StatusCode::TOO_MANY_REQUESTS.as_u16());
        assert!(err.message.contains("本地接入队列"));
        assert_eq!(err.retry_after, Some(Duration::from_secs(1)));
        drop(first);
        reset_local_api_backpressure_for_tests();
    }

    #[tokio::test]
    async fn extended_local_backpressure_wait_can_outlive_default_queue_wait() {
        let _guard = LOCAL_BACKPRESSURE_TEST_LOCK
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        reset_local_api_backpressure_for_tests();
        let mut config = CodexLocalApiSafetyConfig::default();
        config.max_concurrent_requests = 1;
        config.min_request_interval_seconds = 1;
        config.max_queue_wait_seconds = 1;

        let first = acquire_local_api_backpressure(&config)
            .await
            .expect("first request should acquire permit");
        let waiter_config = config.clone();
        let waiter = tokio::spawn(async move {
            acquire_local_api_backpressure_with_wait(&waiter_config, Duration::from_secs(3)).await
        });

        tokio::time::sleep(Duration::from_millis(1200)).await;
        drop(first);
        let second = waiter
            .await
            .expect("waiter task should not panic")
            .expect("extended request-timeout budget should allow waiting past max_queue_wait");
        drop(second);
        reset_local_api_backpressure_for_tests();
    }

    #[test]
    fn backpressure_wait_budget_reserves_request_timeout_guard() {
        assert_eq!(
            backpressure_wait_budget(Duration::from_secs(10), Duration::from_secs(60)),
            Some(Duration::from_secs(48))
        );
        assert_eq!(
            backpressure_wait_budget(Duration::from_secs(59), Duration::from_secs(60)),
            None
        );
    }

    #[tokio::test]
    async fn local_backpressure_permit_drop_releases_capacity() {
        let _guard = LOCAL_BACKPRESSURE_TEST_LOCK
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        reset_local_api_backpressure_for_tests();
        let mut config = CodexLocalApiSafetyConfig::default();
        config.max_concurrent_requests = 1;
        config.min_request_interval_seconds = 0;
        config.max_queue_wait_seconds = 1;

        let first = acquire_local_api_backpressure(&config)
            .await
            .expect("first request should acquire permit");
        drop(first);
        let second = acquire_local_api_backpressure(&config)
            .await
            .expect("dropped permit should release capacity");

        drop(second);
        reset_local_api_backpressure_for_tests();
    }

    #[tokio::test]
    async fn active_stream_admission_release_allows_next_request_without_waiting_for_stream() {
        let _guard = LOCAL_BACKPRESSURE_TEST_LOCK
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        reset_local_api_backpressure_for_tests();
        let mut config = CodexLocalApiSafetyConfig::default();
        config.max_concurrent_requests = 1;
        config.min_request_interval_seconds = 0;
        config.max_queue_wait_seconds = 1;

        let mut first = acquire_local_api_backpressure(&config)
            .await
            .expect("first admission should acquire permit");
        first.release();

        let second = acquire_local_api_backpressure(&config)
            .await
            .expect("released admission permit should not wait for active stream body");

        drop(second);
        reset_local_api_backpressure_for_tests();
    }

    #[tokio::test]
    async fn local_backpressure_admission_queue_is_fifo_across_start_interval() {
        let _guard = LOCAL_BACKPRESSURE_TEST_LOCK
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        reset_local_api_backpressure_for_tests();
        let mut config = CodexLocalApiSafetyConfig::default();
        config.max_concurrent_requests = 1;
        config.min_request_interval_seconds = 2;
        config.max_queue_wait_seconds = 3;

        let first = acquire_local_api_backpressure(&config)
            .await
            .expect("initial request should acquire permit");
        drop(first);

        let first_waiter_config = config.clone();
        let first_waiter = tokio::spawn(async move {
            let permit = acquire_local_api_backpressure(&first_waiter_config)
                .await
                .expect("first queued waiter should acquire next start slot");
            drop(permit);
            std::time::Instant::now()
        });

        tokio::time::sleep(Duration::from_millis(250)).await;

        let second_waiter_config = config.clone();
        let second_waiter = tokio::spawn(async move {
            let permit = acquire_local_api_backpressure(&second_waiter_config)
                .await
                .expect("later queued waiter should not time out behind the first waiter");
            drop(permit);
            std::time::Instant::now()
        });

        let first_started = tokio::time::timeout(Duration::from_secs(6), first_waiter)
            .await
            .expect("first queued waiter should not hang")
            .expect("first queued waiter task should not panic");
        let second_started = tokio::time::timeout(Duration::from_secs(6), second_waiter)
            .await
            .expect("second queued waiter should not hang")
            .expect("second queued waiter task should not panic");

        assert!(second_started >= first_started);
        reset_local_api_backpressure_for_tests();
    }

    #[test]
    fn active_stream_lease_survives_cooldown_until_terminal_release() {
        let _env_guard = LOCAL_ACCESS_ENV_TEST_LOCK
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let previous_root = std::env::var_os(super::CODEX_LOCAL_ACCESS_DATA_ROOT_ENV);
        let root = std::env::temp_dir().join(format!(
            "cockpit-local-access-active-stream-test-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let _ = fs::remove_dir_all(&root);
        std::env::set_var(super::CODEX_LOCAL_ACCESS_DATA_ROOT_ENV, &root);

        reset_active_stream_leases_for_tests();
        let now = 1_700_000_000_000;
        let context = AuditContext {
            request_id: "req-active".to_string(),
            request_id_source: "test".to_string(),
            route: "/v1/responses".to_string(),
            model: "gpt-5.5".to_string(),
            account_hash: "hash-active".to_string(),
            gateway_request_id: "gw-context-test-9".to_string(),
            turn_lineage_id: None,
            turn_lineage_source: None,
            previous_response_id_hash: None,
            is_continuation: false,
            is_auto_compact_candidate: false,
        };
        let mut lease = grant_active_stream_lease(&context, "acc-active");
        assert_eq!(active_stream_lease_count_for_account("acc-active"), 1);

        let mut registry = empty_health_registry(now);
        let classified = classify_codex_upstream_error(
            StatusCode::TOO_MANY_REQUESTS,
            None,
            r#"{"error":{"message":"rate limit exceeded"}}"#,
        );
        update_health_registry_from_classified_error(
            &mut registry,
            "acc-active",
            Some("gpt-5.5"),
            Some("req-followup"),
            &classified,
            now,
        );

        assert!(!health_registry_account_is_schedulable(
            &registry,
            "acc-active",
            Some("gpt-5.5"),
            now
        ));
        assert_eq!(active_stream_lease_count_for_account("acc-active"), 1);

        lease.release(ActiveStreamTerminal::Completed);
        assert_eq!(active_stream_lease_count_for_account("acc-active"), 0);
        reset_active_stream_leases_for_tests();
        match previous_root {
            Some(value) => std::env::set_var(super::CODEX_LOCAL_ACCESS_DATA_ROOT_ENV, value),
            None => std::env::remove_var(super::CODEX_LOCAL_ACCESS_DATA_ROOT_ENV),
        }
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn active_stream_error_classifier_distinguishes_client_abort_from_upstream_error() {
        assert_eq!(
            classify_active_stream_terminal_error("写入上游响应失败: broken pipe"),
            ActiveStreamTerminal::ClientAborted
        );
        assert_eq!(
            classify_active_stream_terminal_error("读取上游响应失败: timeout"),
            ActiveStreamTerminal::StreamError
        );
    }

    #[test]
    fn local_backpressure_error_response_includes_retry_after_header() {
        let raw = String::from_utf8(json_response_with_retry_after(
            429,
            "Too Many Requests",
            &json!({ "error": "本地接入队列等待超时，请稍后重试" }),
            Some(Duration::from_secs(3)),
        ))
        .expect("response should be utf8");

        assert!(raw.contains("\r\nRetry-After: 3\r\n"));
    }

    #[test]
    fn local_backpressure_failure_log_has_local_error_type() {
        let line = build_codex_api_failure_log(
            None,
            Some(429),
            None,
            Some(10),
            "本地接入队列等待超时，请稍后重试",
        );

        assert!(line.contains("error_type=local_backpressure"));
    }

    #[test]
    fn pool_unavailable_failure_log_has_local_error_type_even_with_quota_words() {
        let line = build_codex_api_failure_log(
            None,
            Some(StatusCode::SERVICE_UNAVAILABLE.as_u16()),
            None,
            Some(10),
            "模型 gpt-5.5 的API 服务号池账号额度均已耗尽，请 1 小时后重试",
        );

        assert!(line.contains("error_type=pool_unavailable"));
        assert!(!line.contains("error_type=rate_limited"));
    }

    #[test]
    fn does_not_retry_forbidden_without_quota_or_capacity_markers() {
        assert!(!should_try_next_account(
            StatusCode::FORBIDDEN,
            r#"{"error":"forbidden"}"#,
        ));
    }

    #[test]
    fn prefers_affinity_account_before_round_robin_order() {
        let ordered = build_ordered_account_ids(
            &[
                "acc-a".to_string(),
                "acc-b".to_string(),
                "acc-c".to_string(),
            ],
            1,
            Some("acc-c"),
        );

        assert_eq!(ordered, vec!["acc-c", "acc-b", "acc-a"]);
    }

    #[test]
    fn hardened_routing_uses_stable_fill_first_start_index() {
        let collection = CodexLocalAccessCollection {
            enabled: true,
            port: 2876,
            api_key: "agt_test".to_string(),
            safety_config: CodexLocalApiSafetyConfig::default(),
            routing_strategy: CodexLocalAccessRoutingStrategy::Auto,
            restrict_free_accounts: false,
            follow_current_account: false,
            account_ids: vec!["acc-a".to_string(), "acc-b".to_string()],
            created_at: 1,
            updated_at: 2,
        };

        assert_eq!(next_routing_start_index(&collection), 0);
        assert_eq!(next_routing_start_index(&collection), 0);
    }

    #[test]
    fn hardened_routing_pins_schedulable_process_sticky_account() {
        let now = 1_700_000_000_000;
        let mut registry = empty_health_registry(now);
        registry.accounts.insert(
            "acc-primary".to_string(),
            CodexLocalAccessAccountHealth {
                status: CodexLocalAccessAccountHealthStatus::Healthy,
                estimated_remaining_percentage: Some(10),
                updated_at: now,
                ..CodexLocalAccessAccountHealth::default()
            },
        );
        registry.accounts.insert(
            "acc-secondary".to_string(),
            CodexLocalAccessAccountHealth {
                status: CodexLocalAccessAccountHealthStatus::Healthy,
                estimated_remaining_percentage: Some(90),
                updated_at: now,
                ..CodexLocalAccessAccountHealth::default()
            },
        );
        upsert_process_sticky_binding(&mut registry, "acc-primary", now);

        let mut health_sorted = vec!["acc-primary".to_string(), "acc-secondary".to_string()];
        sort_account_ids_by_health_estimate(&mut health_sorted, &registry, now);
        assert_eq!(
            health_sorted,
            vec!["acc-secondary".to_string(), "acc-primary".to_string()]
        );

        let sticky_ordered =
            pin_process_sticky_account(health_sorted, &registry, Some("gpt-5.5"), now);
        assert_eq!(
            sticky_ordered,
            vec!["acc-primary".to_string(), "acc-secondary".to_string()]
        );
    }

    #[test]
    fn hardened_routing_prunes_process_sticky_account_when_model_cools_down() {
        let now = 1_700_000_000_000;
        let mut registry = empty_health_registry(now);
        upsert_process_sticky_binding(&mut registry, "acc-primary", now);
        registry.model_cooldowns.insert(
            health_registry_model_key("acc-primary", "gpt-5.5"),
            crate::models::codex_local_access::CodexLocalAccessModelCooldown {
                account_id: "acc-primary".to_string(),
                model: "gpt-5.5".to_string(),
                cooldown_until_ms: now + 60_000,
                updated_at: now,
                ..crate::models::codex_local_access::CodexLocalAccessModelCooldown::default()
            },
        );

        let account_ids = vec!["acc-primary".to_string(), "acc-secondary".to_string()];
        assert!(prune_process_sticky_binding(
            &mut registry,
            &account_ids,
            Some("gpt-5.5"),
            now
        ));
        assert!(registry.sticky_bindings.is_empty());
    }

    #[test]
    fn state_effective_pool_order_uses_health_and_recent_success_without_rewriting_config() {
        let now = 1_700_000_000_000;
        let collection = CodexLocalAccessCollection {
            enabled: true,
            port: 2876,
            api_key: "agt_test".to_string(),
            safety_config: CodexLocalApiSafetyConfig::default(),
            routing_strategy: CodexLocalAccessRoutingStrategy::Auto,
            restrict_free_accounts: false,
            follow_current_account: false,
            account_ids: vec![
                "acc-primary".to_string(),
                "acc-secondary".to_string(),
                "acc-third".to_string(),
            ],
            created_at: 1,
            updated_at: 2,
        };
        let mut registry = empty_health_registry(now);
        registry.accounts.insert(
            "acc-primary".to_string(),
            CodexLocalAccessAccountHealth {
                status: CodexLocalAccessAccountHealthStatus::CoolingDown,
                cooldown_until_ms: Some(now + 60_000),
                last_observed_remaining_percentage: Some(0),
                updated_at: now,
                ..CodexLocalAccessAccountHealth::default()
            },
        );

        assert!(upsert_process_sticky_binding(
            &mut registry,
            "acc-secondary",
            now
        ));
        assert!(upsert_successful_account_health(
            &mut registry,
            "acc-secondary",
            now + 1_000
        ));

        assert_eq!(
            build_effective_local_access_account_ids_from_registry(&collection, &registry, now),
            vec![
                "acc-secondary".to_string(),
                "acc-third".to_string(),
                "acc-primary".to_string(),
            ]
        );
        assert_eq!(
            collection.account_ids,
            vec![
                "acc-primary".to_string(),
                "acc-secondary".to_string(),
                "acc-third".to_string(),
            ]
        );

        let account = registry
            .accounts
            .get("acc-secondary")
            .expect("successful account health should be recorded");
        assert_eq!(account.status, CodexLocalAccessAccountHealthStatus::Healthy);
        assert_eq!(account.last_success_at_ms, Some(now + 1_000));
        assert_eq!(account.api_service_success_count, 1);
    }

    #[test]
    fn hardened_routing_considers_full_pool_but_keeps_single_attempt_cap() {
        let now = 1_700_000_000_000;
        let mut collection = CodexLocalAccessCollection {
            enabled: true,
            port: 2876,
            api_key: "agt_test".to_string(),
            safety_config: CodexLocalApiSafetyConfig::default(),
            routing_strategy: CodexLocalAccessRoutingStrategy::PlanHighFirst,
            restrict_free_accounts: false,
            follow_current_account: false,
            account_ids: (0..500).map(|index| format!("acc-{index:03}")).collect(),
            created_at: 1,
            updated_at: 2,
        };
        let mut registry = empty_health_registry(now);
        registry.accounts.insert(
            "acc-000".to_string(),
            CodexLocalAccessAccountHealth {
                status: CodexLocalAccessAccountHealthStatus::CoolingDown,
                cooldown_until_ms: Some(now + 60_000),
                updated_at: now,
                ..CodexLocalAccessAccountHealth::default()
            },
        );

        let routing_pool = build_routing_pool_account_ids(&collection);
        assert_eq!(routing_pool.len(), 500);
        assert_eq!(
            retry_failover_account_attempt_limit(&collection.safety_config),
            2
        );

        let mut candidate_ids = apply_collection_routing_strategy(&routing_pool, &collection);
        sort_account_ids_by_health_estimate(&mut candidate_ids, &registry, now);
        let next_schedulable = candidate_ids
            .iter()
            .find(|account_id| {
                health_registry_account_is_schedulable(&registry, account_id, Some("gpt-5.5"), now)
            })
            .map(String::as_str);

        assert_eq!(next_schedulable, Some("acc-001"));

        collection.safety_config.max_retry_accounts = 2;
        collection.safety_config.fallback_mode = CodexLocalApiFallbackMode::NextRequestOnly;
        assert_eq!(
            retry_failover_account_attempt_limit(&collection.safety_config),
            2
        );
        assert_eq!(build_routing_pool_account_ids(&collection).len(), 500);
    }

    #[test]
    fn selector_audit_summary_records_redacted_counts_and_selected_reason() {
        let now = 1_700_000_000_000;
        let mut registry = empty_health_registry(now);
        registry.accounts.insert(
            "acc-sticky".to_string(),
            CodexLocalAccessAccountHealth {
                status: CodexLocalAccessAccountHealthStatus::Healthy,
                estimated_remaining_percentage: Some(10),
                updated_at: now,
                ..CodexLocalAccessAccountHealth::default()
            },
        );
        registry.accounts.insert(
            "acc-cooling".to_string(),
            CodexLocalAccessAccountHealth {
                status: CodexLocalAccessAccountHealthStatus::CoolingDown,
                cooldown_until_ms: Some(now + 60_000),
                updated_at: now,
                ..CodexLocalAccessAccountHealth::default()
            },
        );
        registry.accounts.insert(
            "acc-disabled".to_string(),
            CodexLocalAccessAccountHealth {
                status: CodexLocalAccessAccountHealthStatus::Disabled,
                updated_at: now,
                ..CodexLocalAccessAccountHealth::default()
            },
        );
        upsert_process_sticky_binding(&mut registry, "acc-sticky", now);

        let candidate_ids = vec![
            "acc-cooling".to_string(),
            "acc-sticky".to_string(),
            "acc-new".to_string(),
            "acc-disabled".to_string(),
        ];
        let summary = build_selector_audit_summary(
            &candidate_ids,
            &registry,
            Some("gpt-5.5"),
            now,
            3,
            None,
            false,
        );
        let selected_reason =
            selector_selected_reason("acc-sticky", None, None, None, Some("acc-sticky"));
        let detail = selector_audit_detail(&summary, selected_reason, "gpt-5.5");

        assert_eq!(detail.get("candidate_count").map(String::as_str), Some("4"));
        assert_eq!(detail.get("eligible_count").map(String::as_str), Some("2"));
        assert_eq!(
            detail.get("selected_reason").map(String::as_str),
            Some("sticky_selected")
        );
        assert_eq!(detail.get("cap_applied").map(String::as_str), Some("true"));
        assert_eq!(detail.get("cap_limit").map(String::as_str), Some("3"));

        let skipped: Value = serde_json::from_str(
            detail
                .get("skipped_counts_by_reason")
                .expect("skip counts should be serialized"),
        )
        .expect("skip counts should parse");
        assert_eq!(skipped["health_skipped"], json!(1));
        assert_eq!(skipped["cap_truncated"], json!(1));

        let serialized = serde_json::to_string(&detail).expect("detail should serialize");
        for secret in ["acc-sticky", "acc-cooling", "acc-disabled", "@", "sk-"] {
            assert!(
                !serialized.contains(secret),
                "selector audit detail leaked sensitive value {secret}: {serialized}"
            );
        }
    }

    #[test]
    fn selector_audit_reason_distinguishes_previous_response_and_sticky_clear() {
        let now = 1_700_000_000_000;
        let registry = empty_health_registry(now);
        let candidate_ids = vec!["acc-prev".to_string()];
        let summary = build_selector_audit_summary(
            &candidate_ids,
            &registry,
            Some("gpt-5.5"),
            now,
            2,
            Some("acc-prev"),
            true,
        );
        let selected_reason =
            selector_selected_reason("acc-prev", None, Some("acc-prev"), None, None);
        let detail = selector_audit_detail(&summary, selected_reason, "gpt-5.5");

        assert_eq!(
            detail.get("selected_reason").map(String::as_str),
            Some("previous_response_affinity_selected")
        );
        assert_eq!(
            detail.get("sticky_cleared").map(String::as_str),
            Some("true")
        );

        let skipped: Value = serde_json::from_str(
            detail
                .get("skipped_counts_by_reason")
                .expect("skip counts should be serialized"),
        )
        .expect("skip counts should parse");
        assert_eq!(skipped["sticky_cleared"], json!(1));
        assert_eq!(
            selector_selected_reason("acc-fill", None, None, None, None),
            "fill_first_selected"
        );
    }

    #[test]
    fn selector_audit_summary_handles_large_pool_in_milliseconds() {
        let now = 1_700_000_000_000;
        let mut registry = empty_health_registry(now);
        for index in 0..10 {
            registry.accounts.insert(
                format!("acc-{index:03}"),
                CodexLocalAccessAccountHealth {
                    status: CodexLocalAccessAccountHealthStatus::CoolingDown,
                    cooldown_until_ms: Some(now + 60_000),
                    updated_at: now,
                    ..CodexLocalAccessAccountHealth::default()
                },
            );
        }
        let candidate_ids: Vec<String> = (0..600).map(|index| format!("acc-{index:03}")).collect();

        let started = std::time::Instant::now();
        let summary = build_selector_audit_summary(
            &candidate_ids,
            &registry,
            Some("gpt-5.5"),
            now,
            24,
            None,
            false,
        );

        assert!(
            started.elapsed() < std::time::Duration::from_millis(100),
            "selector audit summary should remain a pure millisecond-scale pass"
        );
        assert_eq!(summary.candidate_count, 600);
        assert_eq!(summary.eligible_count, 14);
        assert!(summary.cap_applied);
        assert_eq!(summary.cap_limit, 24);
        assert_eq!(
            summary.skipped_counts_by_reason.get("health_skipped"),
            Some(&10)
        );
        assert_eq!(
            summary.skipped_counts_by_reason.get("cap_truncated"),
            Some(&576)
        );
    }

    #[test]
    fn previous_response_affinity_disallows_direct_cross_account_reuse() {
        let candidates = vec![
            "acc-primary".to_string(),
            "acc-secondary".to_string(),
            "acc-third".to_string(),
        ];

        assert_eq!(
            constrain_previous_response_affinity(candidates.clone(), Some("acc-secondary")),
            vec!["acc-secondary".to_string()]
        );
        assert_eq!(
            constrain_previous_response_affinity(candidates.clone(), Some("missing-affinity")),
            Vec::<String>::new()
        );
        assert_eq!(
            constrain_previous_response_affinity(candidates.clone(), None),
            candidates
        );
    }

    #[test]
    fn hardened_routing_fill_first_does_not_preserve_configured_order_after_health_sort() {
        let now = 1_700_000_000_000;
        let collection = CodexLocalAccessCollection {
            enabled: true,
            port: 2876,
            api_key: "agt_test".to_string(),
            safety_config: CodexLocalApiSafetyConfig::default(),
            routing_strategy: CodexLocalAccessRoutingStrategy::PlanHighFirst,
            restrict_free_accounts: false,
            follow_current_account: false,
            account_ids: vec!["acc-b".to_string(), "acc-a".to_string()],
            created_at: 1,
            updated_at: 2,
        };

        let mut candidate_ids =
            apply_collection_routing_strategy(&collection.account_ids, &collection);
        sort_account_ids_by_health_estimate(&mut candidate_ids, &empty_health_registry(now), now);

        assert_eq!(
            candidate_ids,
            vec!["acc-a".to_string(), "acc-b".to_string()]
        );
    }

    #[test]
    fn effective_local_access_pool_matches_configured_members() {
        let collection = CodexLocalAccessCollection {
            enabled: true,
            port: 2876,
            api_key: "agt_test".to_string(),
            safety_config: CodexLocalApiSafetyConfig::default(),
            routing_strategy: CodexLocalAccessRoutingStrategy::Auto,
            restrict_free_accounts: false,
            follow_current_account: false,
            account_ids: vec![
                "acc-third".to_string(),
                "acc-primary".to_string(),
                "acc-secondary".to_string(),
            ],
            created_at: 1,
            updated_at: 2,
        };

        assert_eq!(
            build_effective_local_access_account_ids(&collection),
            vec![
                "acc-primary".to_string(),
                "acc-secondary".to_string(),
                "acc-third".to_string()
            ]
        );
    }

    #[test]
    fn retry_failover_defaults_to_one_retry_and_current_request_failover_account() {
        let config = CodexLocalApiSafetyConfig::default();

        assert_eq!(retry_failover_max_retries(&config), 1);
        assert_eq!(retry_failover_account_attempt_limit(&config), 2);
    }

    #[test]
    fn retry_failover_account_limit_supports_three_account_acceptance() {
        let config = CodexLocalApiSafetyConfig {
            max_retry_accounts: 3,
            fallback_mode: CodexLocalApiFallbackMode::Disabled,
            ..CodexLocalApiSafetyConfig::default()
        };

        assert_eq!(retry_failover_account_attempt_limit(&config), 3);
    }

    #[test]
    fn local_api_safety_config_keeps_three_retry_accounts_for_acceptance() {
        let mut collection = CodexLocalAccessCollection {
            enabled: true,
            port: 45335,
            api_key: "ck-test".to_string(),
            safety_config: CodexLocalApiSafetyConfig {
                max_retry_accounts: 3,
                fallback_mode: CodexLocalApiFallbackMode::NextRequestOnly,
                ..CodexLocalApiSafetyConfig::default()
            },
            routing_strategy: CodexLocalAccessRoutingStrategy::Auto,
            restrict_free_accounts: false,
            follow_current_account: false,
            account_ids: vec![
                "acc-primary".to_string(),
                "acc-secondary".to_string(),
                "acc-third".to_string(),
            ],
            created_at: 1,
            updated_at: 2,
        };

        let changed = normalize_local_api_safety_config(&mut collection);

        assert!(!changed);
        assert_eq!(collection.safety_config.max_retry_accounts, 3);
    }

    #[test]
    fn effective_local_access_pool_ignores_retry_account_limit() {
        let mut collection = CodexLocalAccessCollection {
            enabled: true,
            port: 2876,
            api_key: "agt_test".to_string(),
            safety_config: CodexLocalApiSafetyConfig {
                max_retry_accounts: 2,
                fallback_mode: CodexLocalApiFallbackMode::Disabled,
                ..CodexLocalApiSafetyConfig::default()
            },
            routing_strategy: CodexLocalAccessRoutingStrategy::Auto,
            restrict_free_accounts: false,
            follow_current_account: false,
            account_ids: vec![
                "acc-primary".to_string(),
                "acc-secondary".to_string(),
                "acc-third".to_string(),
            ],
            created_at: 1,
            updated_at: 2,
        };

        assert_eq!(
            build_effective_local_access_account_ids(&collection),
            vec![
                "acc-primary".to_string(),
                "acc-secondary".to_string(),
                "acc-third".to_string()
            ]
        );

        collection.safety_config.fallback_mode = CodexLocalApiFallbackMode::NextRequestOnly;

        assert_eq!(
            build_effective_local_access_account_ids(&collection),
            vec![
                "acc-primary".to_string(),
                "acc-secondary".to_string(),
                "acc-third".to_string()
            ]
        );
    }

    #[test]
    fn retry_failover_account_limit_lifts_legacy_enabled_fallback_to_second_account() {
        let config = CodexLocalApiSafetyConfig {
            max_retry_accounts: 1,
            fallback_mode: CodexLocalApiFallbackMode::NextRequestOnly,
            ..CodexLocalApiSafetyConfig::default()
        };

        assert_eq!(retry_failover_account_attempt_limit(&config), 2);
    }

    #[test]
    fn local_access_account_filter_keeps_membership_without_preserving_input_order() {
        let valid_account_ids = HashSet::from([
            "acc-primary".to_string(),
            "acc-secondary".to_string(),
            "acc-third".to_string(),
        ]);

        let filtered = filter_local_access_account_ids(
            vec![
                "acc-third".to_string(),
                "missing".to_string(),
                "acc-secondary".to_string(),
                "acc-third".to_string(),
                "acc-primary".to_string(),
            ],
            &valid_account_ids,
        );

        assert_eq!(
            filtered,
            vec![
                "acc-primary".to_string(),
                "acc-secondary".to_string(),
                "acc-third".to_string(),
            ]
        );
    }

    #[test]
    fn stream_write_state_blocks_fallback_after_headers_or_first_chunk() {
        let mut state = StreamWriteState::default();
        assert!(state.can_attempt_account_fallback());

        state.mark_headers_written();
        assert!(!state.can_attempt_account_fallback());

        let mut state = StreamWriteState::default();
        state.mark_first_chunk_written();
        assert!(!state.can_attempt_account_fallback());
    }

    #[test]
    fn upstream_stream_error_sse_has_explicit_failed_terminal_event() {
        let body = build_responses_upstream_stream_error_sse(
            "读取上游响应失败: stream closed before response.completed",
        );
        let text = String::from_utf8(body).expect("failure SSE should be UTF-8");

        assert!(text.contains("event: response.failed"));
        assert!(text.contains("\"code\":\"cockpit_upstream_stream_error\""));
        assert!(text.contains("openai_codex_response_failed_with_done"));
        assert!(text.ends_with("data: [DONE]\n\n"));

        let mut collector = ResponseUsageCollector::new(true);
        collector.feed(text.as_bytes());
        let capture = collector.finish();
        assert!(
            !capture.response_completed_seen,
            "response.failed must not be reported as a successful completion"
        );
    }

    #[test]
    fn cockpit_api_service_account_switch_preserves_pool_without_follow_toggle() {
        let collection = CodexLocalAccessCollection {
            enabled: true,
            port: 45335,
            api_key: "agt_test".to_string(),
            safety_config: CodexLocalApiSafetyConfig::default(),
            routing_strategy: CodexLocalAccessRoutingStrategy::Auto,
            restrict_free_accounts: false,
            follow_current_account: false,
            account_ids: vec!["acc-old".to_string()],
            created_at: 1,
            updated_at: 2,
        };

        assert!(!should_sync_local_access_collection_on_account_switch(
            CodexRuntimeIntegrationMode::CockpitApiService,
            &collection
        ));
    }

    #[test]
    fn legacy_follow_current_account_switch_is_ignored() {
        let collection = CodexLocalAccessCollection {
            enabled: true,
            port: 45335,
            api_key: "agt_test".to_string(),
            safety_config: CodexLocalApiSafetyConfig::default(),
            routing_strategy: CodexLocalAccessRoutingStrategy::Auto,
            restrict_free_accounts: false,
            follow_current_account: true,
            account_ids: vec![
                "acc-old".to_string(),
                "acc-third".to_string(),
                "acc-new".to_string(),
            ],
            created_at: 1,
            updated_at: 2,
        };

        assert!(
            !should_sync_local_access_collection_on_account_switch(
                CodexRuntimeIntegrationMode::DirectProjection,
                &collection
            ),
            "legacy follow-current flag must not mutate the API service account pool"
        );
    }

    #[test]
    fn cockpit_api_service_projection_seeds_only_missing_collection() {
        let collection = CodexLocalAccessCollection {
            enabled: true,
            port: 45335,
            api_key: "agt_test".to_string(),
            safety_config: CodexLocalApiSafetyConfig::default(),
            routing_strategy: CodexLocalAccessRoutingStrategy::Auto,
            restrict_free_accounts: false,
            follow_current_account: false,
            account_ids: vec!["acc-old".to_string(), "acc-third".to_string()],
            created_at: 1,
            updated_at: 2,
        };

        assert_eq!(
            build_projection_seed_local_access_account_ids("acc-current", None),
            Some(vec!["acc-current".to_string()])
        );
        assert_eq!(
            build_projection_seed_local_access_account_ids("acc-current", Some(&collection)),
            None
        );
    }

    #[test]
    fn direct_projection_account_switch_does_not_replace_pool_without_follow_toggle() {
        let collection = CodexLocalAccessCollection {
            enabled: true,
            port: 45335,
            api_key: "agt_test".to_string(),
            safety_config: CodexLocalApiSafetyConfig::default(),
            routing_strategy: CodexLocalAccessRoutingStrategy::Auto,
            restrict_free_accounts: false,
            follow_current_account: false,
            account_ids: vec!["acc-old".to_string()],
            created_at: 1,
            updated_at: 2,
        };

        assert!(!should_sync_local_access_collection_on_account_switch(
            CodexRuntimeIntegrationMode::DirectProjection,
            &collection
        ));
    }

    #[test]
    fn app_exit_projection_guard_only_targets_cockpit_api_service() {
        assert!(should_restore_direct_projection_before_app_exit(
            CodexRuntimeIntegrationMode::CockpitApiService
        ));
        assert!(!should_restore_direct_projection_before_app_exit(
            CodexRuntimeIntegrationMode::DirectProjection
        ));
    }

    fn continuity_risk_for_test(
        active_stream_count: usize,
        codex_app_process_count: usize,
        recent_audit_activity: bool,
    ) -> RuntimeProjectionContinuityRisk {
        RuntimeProjectionContinuityRisk {
            active_stream_count,
            codex_app_process_count,
            recent_audit_activity,
            audit_last_modified_age_ms: if recent_audit_activity {
                Some(1_000)
            } else {
                None
            },
        }
    }

    #[test]
    fn direct_projection_change_blocks_when_continuity_risk_exists() {
        let risk = continuity_risk_for_test(1, 0, false);

        assert!(should_block_direct_projection_change(
            CodexRuntimeIntegrationMode::CockpitApiService,
            CodexRuntimeIntegrationMode::DirectProjection,
            false,
            &risk,
        ));
    }

    #[test]
    fn runtime_projection_change_blocks_both_auth_replacing_directions() {
        let risk = continuity_risk_for_test(1, 0, false);

        assert!(should_block_runtime_projection_change(
            CodexRuntimeIntegrationMode::CockpitApiService,
            CodexRuntimeIntegrationMode::DirectProjection,
            false,
            &risk,
        ));
        assert!(should_block_runtime_projection_change(
            CodexRuntimeIntegrationMode::DirectProjection,
            CodexRuntimeIntegrationMode::CockpitApiService,
            false,
            &risk,
        ));
    }

    #[test]
    fn runtime_projection_change_allows_force_no_risk_or_same_mode() {
        let risk = continuity_risk_for_test(2, 1, true);
        let no_risk = continuity_risk_for_test(0, 0, false);

        assert!(!should_block_runtime_projection_change(
            CodexRuntimeIntegrationMode::DirectProjection,
            CodexRuntimeIntegrationMode::CockpitApiService,
            true,
            &risk,
        ));
        assert!(!should_block_runtime_projection_change(
            CodexRuntimeIntegrationMode::DirectProjection,
            CodexRuntimeIntegrationMode::CockpitApiService,
            false,
            &no_risk,
        ));
        assert!(!should_block_runtime_projection_change(
            CodexRuntimeIntegrationMode::CockpitApiService,
            CodexRuntimeIntegrationMode::CockpitApiService,
            false,
            &risk,
        ));
    }

    #[test]
    fn direct_projection_change_blocks_for_codex_app_or_recent_audit() {
        for risk in [
            continuity_risk_for_test(0, 1, false),
            continuity_risk_for_test(0, 0, true),
        ] {
            assert!(should_block_direct_projection_change(
                CodexRuntimeIntegrationMode::CockpitApiService,
                CodexRuntimeIntegrationMode::DirectProjection,
                false,
                &risk,
            ));
        }
    }

    #[test]
    fn direct_projection_change_allows_force_or_no_risk() {
        let risk = continuity_risk_for_test(2, 1, true);
        let no_risk = continuity_risk_for_test(0, 0, false);

        assert!(!should_block_direct_projection_change(
            CodexRuntimeIntegrationMode::CockpitApiService,
            CodexRuntimeIntegrationMode::DirectProjection,
            true,
            &risk,
        ));
        assert!(!should_block_direct_projection_change(
            CodexRuntimeIntegrationMode::CockpitApiService,
            CodexRuntimeIntegrationMode::DirectProjection,
            false,
            &no_risk,
        ));
    }

    #[test]
    fn direct_projection_guard_only_blocks_cockpit_to_direct_transition() {
        let risk = continuity_risk_for_test(1, 1, true);

        assert!(!should_block_direct_projection_change(
            CodexRuntimeIntegrationMode::DirectProjection,
            CodexRuntimeIntegrationMode::DirectProjection,
            false,
            &risk,
        ));
        assert!(!should_block_direct_projection_change(
            CodexRuntimeIntegrationMode::CockpitApiService,
            CodexRuntimeIntegrationMode::CockpitApiService,
            false,
            &risk,
        ));
    }

    #[tokio::test]
    #[ignore = "live test mutates local Codex/Cockpit projection; set COCKPIT_TOOLS_LIVE_RUNTIME_SWITCH=1"]
    async fn live_runtime_mode_roundtrip_direct_and_api_service() {
        assert_eq!(
            std::env::var("COCKPIT_TOOLS_LIVE_RUNTIME_SWITCH").as_deref(),
            Ok("1"),
            "set COCKPIT_TOOLS_LIVE_RUNTIME_SWITCH=1 to run this live projection test"
        );

        let direct = set_runtime_integration_mode(CodexRuntimeIntegrationMode::DirectProjection)
            .await
            .expect("switch to direct projection");
        assert_eq!(direct.mode, CodexRuntimeIntegrationMode::DirectProjection);

        let gateway = set_runtime_integration_mode(CodexRuntimeIntegrationMode::CockpitApiService)
            .await
            .expect("switch back to cockpit api service");
        assert_eq!(gateway.mode, CodexRuntimeIntegrationMode::CockpitApiService);
        assert_eq!(
            load_runtime_mode_state().expect("runtime mode").mode,
            CodexRuntimeIntegrationMode::CockpitApiService
        );
    }

    #[test]
    fn builds_routing_hint_from_previous_response_id_and_model() {
        let request = ParsedRequest {
            method: "POST".to_string(),
            target: "/v1/responses".to_string(),
            headers: HashMap::new(),
            body: br#"{"model":"GPT-5.4-mini","previous_response_id":"resp_prev"}"#.to_vec(),
            gateway_request_id: "gw-test-18".to_string(),
        };

        let hint = build_request_routing_hint(&request);
        assert_eq!(hint.model_key, "gpt-5.4-mini");
        assert_eq!(hint.previous_response_id.as_deref(), Some("resp_prev"));
    }

    #[test]
    fn runtime_mode_renames_gateway_litellm_to_cockpit_api_service() {
        let legacy: crate::models::codex_local_access::CodexRuntimeModeState =
            serde_json::from_value(json!({
                "mode": "gateway_litellm",
                "accountKind": "api",
                "currentAccountId": "codex-test",
                "updatedAt": 1
            }))
            .expect("legacy runtime mode should deserialize");
        assert_eq!(legacy.mode, CodexRuntimeIntegrationMode::CockpitApiService);

        let serialized = serde_json::to_value(build_runtime_mode_state(
            CodexRuntimeIntegrationMode::CockpitApiService,
        ))
        .expect("serialize runtime mode");
        assert_eq!(serialized["mode"], "cockpit_api_service");
    }

    #[test]
    fn runtime_account_kind_serializes_oauth_without_acronym_split() {
        let legacy: CodexRuntimeAccountKind =
            serde_json::from_value(json!("o_auth")).expect("legacy oauth kind");
        assert_eq!(legacy, CodexRuntimeAccountKind::OAuth);
        assert_eq!(
            serde_json::to_value(CodexRuntimeAccountKind::OAuth).expect("serialize oauth kind"),
            json!("oauth")
        );
    }

    #[test]
    fn stable_local_access_port_prefers_first_available_fixed_port() {
        let expected = PREFERRED_CODEX_LOCAL_ACCESS_PORTS[0];
        assert_eq!(
            first_stable_local_access_port(None, |port| port == expected),
            Some(expected)
        );
    }

    #[test]
    fn stable_local_access_port_skips_occupied_fixed_port() {
        let occupied_port = PREFERRED_CODEX_LOCAL_ACCESS_PORTS[0];
        let expected = PREFERRED_CODEX_LOCAL_ACCESS_PORTS[1];

        assert_eq!(
            first_stable_local_access_port(Some(occupied_port), |port| {
                port == occupied_port || port == expected
            }),
            Some(expected)
        );
    }

    #[test]
    fn maps_snapshot_model_ids_to_supported_aliases() {
        assert_eq!(
            resolve_supported_model_alias("gpt-5.4-2026-03-05"),
            "gpt-5.4"
        );
        assert_eq!(
            resolve_supported_model_alias("GPT-5.4-Mini-2026-03-05"),
            "gpt-5.4-mini"
        );
        assert_eq!(
            resolve_supported_model_alias("custom-model-2026-03-05"),
            "custom-model-2026-03-05"
        );
    }

    #[test]
    fn local_models_include_codex_image_model() {
        let response = build_local_models_response();
        let has_image_model = response
            .get("data")
            .and_then(Value::as_array)
            .map(|models| {
                models
                    .iter()
                    .any(|model| model.get("id").and_then(Value::as_str) == Some("gpt-image-2"))
            })
            .unwrap_or(false);

        assert!(has_image_model);
    }

    #[test]
    fn runtime_account_uses_dedicated_local_access_provider_bucket() {
        let source = CodexAccount::new_api_key(
            "codex_apikey_source".to_string(),
            "api@example.com".to_string(),
            "sk-upstream".to_string(),
            CodexApiProviderMode::Custom,
            Some("http://35.213.82.91:8003/v1".to_string()),
            Some("cmp_1778165666417_1".to_string()),
            Some("35.213.82.91".to_string()),
        );

        let runtime = build_runtime_account(
            "http://127.0.0.1:2876/v1".to_string(),
            "agt_codex_local".to_string(),
            &source,
        );

        assert_eq!(
            runtime.api_base_url.as_deref(),
            Some("http://127.0.0.1:2876/v1")
        );
        assert_eq!(
            runtime.api_provider_id.as_deref(),
            Some(CODEX_LOCAL_ACCESS_RUNTIME_PROVIDER_ID)
        );
        assert_eq!(
            runtime.api_provider_name.as_deref(),
            Some(CODEX_LOCAL_ACCESS_RUNTIME_PROVIDER_NAME)
        );
        assert_eq!(runtime.openai_api_key.as_deref(), Some("agt_codex_local"));
    }

    #[test]
    fn prepares_chat_completions_request_for_responses_proxy() {
        let request = ParsedRequest {
            method: "POST".to_string(),
            target: "/v1/chat/completions".to_string(),
            headers: HashMap::new(),
            body: br#"{"model":"GPT-5.4","stream":true,"messages":[{"role":"user","content":"hello"}]}"#
                .to_vec(),
            gateway_request_id: "gw-test-19".to_string(),
        };

        let (prepared, adapter) = prepare_gateway_request(request).expect("request should map");
        assert_eq!(prepared.target, "/v1/responses");
        let mapped_body: Value =
            serde_json::from_slice(&prepared.body).expect("mapped body should be json");
        assert_eq!(
            mapped_body.get("model").and_then(Value::as_str),
            Some("gpt-5.4")
        );
        assert!(mapped_body.get("input").is_some());
        assert_eq!(mapped_body.get("store"), Some(&Value::Bool(false)));
        assert_eq!(mapped_body.get("stream"), Some(&Value::Bool(true)));
        assert_eq!(
            mapped_body.get("instructions").and_then(Value::as_str),
            Some("")
        );
        assert_eq!(
            mapped_body
                .get("parallel_tool_calls")
                .and_then(Value::as_bool),
            Some(true)
        );
        assert_eq!(
            mapped_body
                .get("reasoning")
                .and_then(|reasoning| reasoning.get("effort"))
                .and_then(Value::as_str),
            Some("medium")
        );
        assert!(mapped_body
            .get("tools")
            .and_then(Value::as_array)
            .map(|tools| tools.iter().any(|tool| {
                tool.get("type").and_then(Value::as_str) == Some("image_generation")
            }))
            .unwrap_or(false));

        match adapter {
            GatewayResponseAdapter::ChatCompletions {
                stream,
                requested_model,
                original_request_body: _,
            } => {
                assert!(stream);
                assert_eq!(requested_model, "gpt-5.4");
            }
            _ => panic!("expected chat completions adapter"),
        }
    }

    #[test]
    fn prepares_images_generation_request_for_responses_proxy() {
        let request = ParsedRequest {
            method: "POST".to_string(),
            target: "/v1/images/generations".to_string(),
            headers: HashMap::new(),
            body: br#"{"model":"gpt-image-2","prompt":"draw a clean icon","size":"1024x1024","response_format":"b64_json"}"#.to_vec(),
            gateway_request_id: "gw-test-20".to_string(),
        };

        let (prepared, adapter) = prepare_gateway_request(request).expect("request should map");
        assert_eq!(prepared.target, "/v1/responses");
        let mapped_body: Value =
            serde_json::from_slice(&prepared.body).expect("mapped body should be json");
        assert_eq!(
            mapped_body.get("model").and_then(Value::as_str),
            Some("gpt-5.4-mini")
        );
        assert_eq!(
            mapped_body
                .get("tool_choice")
                .and_then(|choice| choice.get("type"))
                .and_then(Value::as_str),
            Some("image_generation")
        );
        assert_eq!(
            mapped_body
                .get("tools")
                .and_then(Value::as_array)
                .and_then(|tools| tools.first())
                .and_then(|tool| tool.get("model"))
                .and_then(Value::as_str),
            Some("gpt-image-2")
        );
        assert_eq!(
            mapped_body
                .get("tools")
                .and_then(Value::as_array)
                .and_then(|tools| tools.first())
                .and_then(|tool| tool.get("size"))
                .and_then(Value::as_str),
            Some("1024x1024")
        );

        match adapter {
            GatewayResponseAdapter::Images {
                stream,
                response_format,
                stream_prefix,
            } => {
                assert!(!stream);
                assert_eq!(response_format, "b64_json");
                assert_eq!(stream_prefix, "image_generation");
            }
            _ => panic!("expected images adapter"),
        }
    }

    #[test]
    fn rejects_unsupported_images_model() {
        let request = ParsedRequest {
            method: "POST".to_string(),
            target: "/v1/images/generations".to_string(),
            headers: HashMap::new(),
            body: br#"{"model":"gpt-image-1.5","prompt":"draw"}"#.to_vec(),
            gateway_request_id: "gw-test-21".to_string(),
        };

        let err = prepare_gateway_request(request).expect_err("model should be rejected");
        assert!(err.contains("Use gpt-image-2"));
    }

    #[test]
    fn prepares_multipart_images_edit_request_for_responses_proxy() {
        let boundary = "test-boundary";
        let mut body = Vec::new();
        body.extend_from_slice(b"--test-boundary\r\n");
        body.extend_from_slice(b"Content-Disposition: form-data; name=\"model\"\r\n\r\n");
        body.extend_from_slice(b"gpt-image-2\r\n");
        body.extend_from_slice(b"--test-boundary\r\n");
        body.extend_from_slice(b"Content-Disposition: form-data; name=\"prompt\"\r\n\r\n");
        body.extend_from_slice(b"make it brighter\r\n");
        body.extend_from_slice(b"--test-boundary\r\n");
        body.extend_from_slice(
            b"Content-Disposition: form-data; name=\"image\"; filename=\"a.png\"\r\n",
        );
        body.extend_from_slice(b"Content-Type: image/png\r\n\r\n");
        body.extend_from_slice(b"\x89PNG\r\n\x1a\nabc\r\n");
        body.extend_from_slice(b"--test-boundary--\r\n");
        let mut headers = HashMap::new();
        headers.insert(
            "content-type".to_string(),
            format!("multipart/form-data; boundary={}", boundary),
        );
        let request = ParsedRequest {
            method: "POST".to_string(),
            target: "/v1/images/edits".to_string(),
            headers,
            body,
            gateway_request_id: "gw-test-22".to_string(),
        };

        let (prepared, adapter) = prepare_gateway_request(request).expect("request should map");
        assert_eq!(prepared.target, "/v1/responses");
        let mapped_body: Value =
            serde_json::from_slice(&prepared.body).expect("mapped body should be json");
        assert_eq!(
            mapped_body
                .get("tools")
                .and_then(Value::as_array)
                .and_then(|tools| tools.first())
                .and_then(|tool| tool.get("action"))
                .and_then(Value::as_str),
            Some("edit")
        );
        let has_input_image = mapped_body
            .get("input")
            .and_then(Value::as_array)
            .and_then(|items| items.first())
            .and_then(|item| item.get("content"))
            .and_then(Value::as_array)
            .map(|content| {
                content.iter().any(|part| {
                    part.get("type").and_then(Value::as_str) == Some("input_image")
                        && part
                            .get("image_url")
                            .and_then(Value::as_str)
                            .map(|url| url.starts_with("data:image/png;base64,"))
                            .unwrap_or(false)
                })
            })
            .unwrap_or(false);
        assert!(has_input_image);

        match adapter {
            GatewayResponseAdapter::Images { stream_prefix, .. } => {
                assert_eq!(stream_prefix, "image_edit");
            }
            _ => panic!("expected images adapter"),
        }
    }

    #[test]
    fn builds_images_api_payload_from_responses_output() {
        let response = json!({
            "response": {
                "created_at": 123,
                "output": [{
                    "type": "image_generation_call",
                    "result": "aGVsbG8=",
                    "output_format": "png",
                    "revised_prompt": "draw a clean icon"
                }],
                "tool_usage": {
                    "image_gen": {
                        "input_images": 0,
                        "output_images": 1
                    }
                }
            }
        });

        let payload =
            build_images_api_payload(&response, "b64_json").expect("payload should build");
        assert_eq!(payload.get("created").and_then(Value::as_i64), Some(123));
        assert_eq!(
            payload
                .get("data")
                .and_then(Value::as_array)
                .and_then(|items| items.first())
                .and_then(|item| item.get("b64_json"))
                .and_then(Value::as_str),
            Some("aGVsbG8=")
        );
        assert_eq!(
            payload
                .get("data")
                .and_then(Value::as_array)
                .and_then(|items| items.first())
                .and_then(|item| item.get("revised_prompt"))
                .and_then(Value::as_str),
            Some("draw a clean icon")
        );
    }

    #[test]
    fn rewrites_snapshot_model_ids_for_passthrough_requests() {
        let request = ParsedRequest {
            method: "POST".to_string(),
            target: "/v1/responses".to_string(),
            headers: HashMap::new(),
            body: br#"{"model":"gpt-5.4-2026-03-05","input":"hello"}"#.to_vec(),
            gateway_request_id: "gw-test-23".to_string(),
        };

        let (prepared, adapter) = prepare_gateway_request(request).expect("request should map");
        let mapped_body: Value =
            serde_json::from_slice(&prepared.body).expect("mapped body should be json");
        assert_eq!(
            mapped_body.get("model").and_then(Value::as_str),
            Some("gpt-5.4")
        );

        match adapter {
            GatewayResponseAdapter::Passthrough { request_is_stream } => {
                assert!(!request_is_stream);
            }
            _ => panic!("expected passthrough adapter"),
        }
    }

    #[test]
    fn responses_stream_requests_stay_passthrough() {
        let request = ParsedRequest {
            method: "POST".to_string(),
            target: "/v1/responses".to_string(),
            headers: HashMap::from([("accept".to_string(), "text/event-stream".to_string())]),
            body: br#"{"model":"gpt-5.4","stream":true,"input":"hello"}"#.to_vec(),
            gateway_request_id: "gw-test-24".to_string(),
        };

        let (prepared, adapter) = prepare_gateway_request(request).expect("request should map");
        assert_eq!(prepared.target, "/v1/responses");

        match adapter {
            GatewayResponseAdapter::Passthrough { request_is_stream } => {
                assert!(request_is_stream);
            }
            _ => panic!("expected responses stream passthrough adapter"),
        }
    }

    #[test]
    fn detects_responses_websocket_upgrade_probe() {
        let request = ParsedRequest {
            method: "GET".to_string(),
            target: "/v1/responses".to_string(),
            headers: HashMap::from([
                ("connection".to_string(), "keep-alive, Upgrade".to_string()),
                ("upgrade".to_string(), "websocket".to_string()),
                ("sec-websocket-key".to_string(), "test-key".to_string()),
            ]),
            body: Vec::new(),
            gateway_request_id: "gw-test-25".to_string(),
        };

        assert!(is_websocket_upgrade_request(&request.headers));
        assert!(is_responses_websocket_upgrade_request(&request));
    }

    #[test]
    fn websocket_upgrade_probe_must_not_match_regular_responses_stream() {
        let request = ParsedRequest {
            method: "POST".to_string(),
            target: "/v1/responses".to_string(),
            headers: HashMap::from([("accept".to_string(), "text/event-stream".to_string())]),
            body: br#"{"model":"gpt-5.4","stream":true,"input":"hello"}"#.to_vec(),
            gateway_request_id: "gw-test-26".to_string(),
        };

        assert!(!is_websocket_upgrade_request(&request.headers));
        assert!(!is_responses_websocket_upgrade_request(&request));
    }

    #[test]
    fn injects_image_generation_tool_for_responses_requests() {
        let request = ParsedRequest {
            method: "POST".to_string(),
            target: "/v1/responses".to_string(),
            headers: HashMap::new(),
            body: br#"{"model":"gpt-5.4","input":"draw an icon"}"#.to_vec(),
            gateway_request_id: "gw-test-27".to_string(),
        };

        let (prepared, adapter) = prepare_gateway_request(request).expect("request should map");
        let mapped_body: Value =
            serde_json::from_slice(&prepared.body).expect("mapped body should be json");
        assert!(mapped_body
            .get("tools")
            .and_then(Value::as_array)
            .map(|tools| tools.iter().any(|tool| {
                tool.get("type").and_then(Value::as_str) == Some("image_generation")
                    && tool.get("output_format").and_then(Value::as_str) == Some("png")
            }))
            .unwrap_or(false));

        match adapter {
            GatewayResponseAdapter::Passthrough { request_is_stream } => {
                assert!(!request_is_stream);
            }
            _ => panic!("expected passthrough adapter"),
        }
    }

    #[test]
    fn rewrites_snapshot_model_ids_for_chat_completions_requests() {
        let request = ParsedRequest {
            method: "POST".to_string(),
            target: "/v1/chat/completions".to_string(),
            headers: HashMap::new(),
            body:
                br#"{"model":"gpt-5.4-2026-03-05","messages":[{"role":"user","content":"hello"}]}"#
                    .to_vec(),
            gateway_request_id: "gw-test-28".to_string(),
        };

        let (prepared, adapter) = prepare_gateway_request(request).expect("request should map");
        let mapped_body: Value =
            serde_json::from_slice(&prepared.body).expect("mapped body should be json");
        assert_eq!(
            mapped_body.get("model").and_then(Value::as_str),
            Some("gpt-5.4")
        );

        match adapter {
            GatewayResponseAdapter::ChatCompletions {
                requested_model, ..
            } => {
                assert_eq!(requested_model, "gpt-5.4");
            }
            _ => panic!("expected chat completions adapter"),
        }
    }

    #[test]
    fn drops_unsupported_sampling_params_for_responses_proxy() {
        let request = ParsedRequest {
            method: "POST".to_string(),
            target: "/v1/chat/completions".to_string(),
            headers: HashMap::new(),
            body: br#"{"model":"gpt-5.4","temperature":0.2,"top_p":0.7,"messages":[{"role":"user","content":"hello"}]}"#
                .to_vec(),
            gateway_request_id: "gw-test-29".to_string(),
        };

        let (prepared, _) = prepare_gateway_request(request).expect("request should map");
        let mapped_body: Value =
            serde_json::from_slice(&prepared.body).expect("mapped body should be json");
        assert!(mapped_body.get("temperature").is_none());
        assert!(mapped_body.get("top_p").is_none());
    }

    #[test]
    fn normalizes_text_content_parts_for_responses_proxy() {
        let request = ParsedRequest {
            method: "POST".to_string(),
            target: "/v1/chat/completions".to_string(),
            headers: HashMap::new(),
            body: br#"{"model":"gpt-5.4","messages":[{"role":"user","content":[{"type":"text","text":"hello"}]}]}"#
                .to_vec(),
            gateway_request_id: "gw-test-30".to_string(),
        };

        let (prepared, _) = prepare_gateway_request(request).expect("request should map");
        let mapped_body: Value =
            serde_json::from_slice(&prepared.body).expect("mapped body should be json");
        let first_type = mapped_body
            .get("input")
            .and_then(Value::as_array)
            .and_then(|messages| messages.first())
            .and_then(|message| message.get("content"))
            .and_then(Value::as_array)
            .and_then(|parts| parts.first())
            .and_then(|part| part.get("type"))
            .and_then(Value::as_str);
        assert_eq!(first_type, Some("input_text"));
    }

    #[test]
    fn normalizes_function_tools_for_responses_proxy() {
        let request = ParsedRequest {
            method: "POST".to_string(),
            target: "/v1/chat/completions".to_string(),
            headers: HashMap::new(),
            body: br#"{"model":"gpt-5.4","messages":[{"role":"user","content":"hello"}],"tools":[{"type":"function","function":{"name":"get_weather","description":"Get weather","parameters":{"type":"object","properties":{"location":{"type":"string"}}},"strict":true}}],"tool_choice":{"type":"function","function":{"name":"get_weather"}}}"#
                .to_vec(),
            gateway_request_id: "gw-test-31".to_string(),
        };

        let (prepared, _) = prepare_gateway_request(request).expect("request should map");
        let mapped_body: Value =
            serde_json::from_slice(&prepared.body).expect("mapped body should be json");
        assert_eq!(
            mapped_body
                .get("tools")
                .and_then(Value::as_array)
                .and_then(|tools| tools.first())
                .and_then(|tool| tool.get("name"))
                .and_then(Value::as_str),
            Some("get_weather")
        );
        assert_eq!(
            mapped_body
                .get("tool_choice")
                .and_then(|choice| choice.get("name"))
                .and_then(Value::as_str),
            Some("get_weather")
        );
        assert_eq!(
            mapped_body
                .get("tools")
                .and_then(Value::as_array)
                .and_then(|tools| tools.first())
                .and_then(|tool| tool.get("strict"))
                .and_then(Value::as_bool),
            Some(true)
        );
    }

    #[test]
    fn normalizes_tool_history_messages_for_responses_proxy() {
        let request = ParsedRequest {
            method: "POST".to_string(),
            target: "/v1/chat/completions".to_string(),
            headers: HashMap::new(),
            body: br#"{"model":"gpt-5.4","messages":[{"role":"user","content":"weather?"},{"role":"assistant","content":null,"tool_calls":[{"id":"call_1","type":"function","function":{"name":"get_weather","arguments":"{\"location\":\"Paris\"}"}}]},{"role":"tool","tool_call_id":"call_1","content":"{\"temperature_c\":18}"}]}"#
                .to_vec(),
            gateway_request_id: "gw-test-32".to_string(),
        };

        let (prepared, _) = prepare_gateway_request(request).expect("request should map");
        let mapped_body: Value =
            serde_json::from_slice(&prepared.body).expect("mapped body should be json");
        let input = mapped_body
            .get("input")
            .and_then(Value::as_array)
            .expect("input should be array");
        assert_eq!(
            input
                .first()
                .and_then(|item| item.get("role"))
                .and_then(Value::as_str),
            Some("user")
        );
        assert!(input.iter().any(|item| {
            item.get("type").and_then(Value::as_str) == Some("function_call")
                && item.get("name").and_then(Value::as_str) == Some("get_weather")
        }));
        assert!(input.iter().any(|item| {
            item.get("type").and_then(Value::as_str) == Some("function_call_output")
                && item.get("call_id").and_then(Value::as_str) == Some("call_1")
        }));
    }

    #[test]
    fn skips_spurious_empty_assistant_message_for_tool_calls() {
        let request = ParsedRequest {
            method: "POST".to_string(),
            target: "/v1/chat/completions".to_string(),
            headers: HashMap::new(),
            body: br#"{"model":"gpt-5.4","messages":[{"role":"user","content":"weather?"},{"role":"assistant","content":null,"tool_calls":[{"id":"call_1","type":"function","function":{"name":"get_weather","arguments":"{\"location\":\"Paris\"}"}}]},{"role":"tool","tool_call_id":"call_1","content":"{\"temperature_c\":18}"}]}"#
                .to_vec(),
            gateway_request_id: "gw-test-33".to_string(),
        };

        let (prepared, _) = prepare_gateway_request(request).expect("request should map");
        let mapped_body: Value =
            serde_json::from_slice(&prepared.body).expect("mapped body should be json");
        let input = mapped_body
            .get("input")
            .and_then(Value::as_array)
            .expect("input should be array");
        assert_eq!(input.len(), 3);
        assert_eq!(
            input
                .first()
                .and_then(|item| item.get("type"))
                .and_then(Value::as_str),
            Some("message")
        );
        assert_eq!(
            input
                .get(1)
                .and_then(|item| item.get("type"))
                .and_then(Value::as_str),
            Some("function_call")
        );
        assert_eq!(
            input
                .get(2)
                .and_then(|item| item.get("type"))
                .and_then(Value::as_str),
            Some("function_call_output")
        );
    }

    #[test]
    fn builds_chat_completion_payload_from_responses_output() {
        let responses_payload = json!({
            "id": "resp_123",
            "model": "gpt-5.4",
            "output": [{
                "type": "message",
                "role": "assistant",
                "content": [{
                    "type": "output_text",
                    "text": "hello world"
                }]
            }],
            "usage": {
                "input_tokens": 7,
                "output_tokens": 3,
                "total_tokens": 10
            }
        });

        let chat_payload = build_chat_completion_payload(&responses_payload, "gpt-5.4", br#"{}"#);
        assert_eq!(
            chat_payload.get("object").and_then(Value::as_str),
            Some("chat.completion")
        );
        assert_eq!(
            chat_payload
                .get("choices")
                .and_then(Value::as_array)
                .and_then(|choices| choices.first())
                .and_then(|choice| choice.get("message"))
                .and_then(|message| message.get("content"))
                .and_then(Value::as_str),
            Some("hello world")
        );
        assert_eq!(
            chat_payload
                .get("usage")
                .and_then(|usage| usage.get("total_tokens"))
                .and_then(Value::as_u64),
            Some(10)
        );
    }

    #[test]
    fn builds_chat_completion_payload_from_function_call_output() {
        let responses_payload = json!({
            "id": "resp_tool_1",
            "model": "gpt-5.4",
            "status": "completed",
            "output": [{
                "type": "function_call",
                "call_id": "call_abc",
                "name": "get_weather",
                "arguments": "{\"location\":\"Paris\"}"
            }]
        });

        let chat_payload = build_chat_completion_payload(&responses_payload, "gpt-5.4", br#"{}"#);
        assert_eq!(
            chat_payload
                .get("choices")
                .and_then(Value::as_array)
                .and_then(|choices| choices.first())
                .and_then(|choice| choice.get("finish_reason"))
                .and_then(Value::as_str),
            Some("tool_calls")
        );
        assert_eq!(
            chat_payload
                .get("choices")
                .and_then(Value::as_array)
                .and_then(|choices| choices.first())
                .and_then(|choice| choice.get("message"))
                .and_then(|message| message.get("tool_calls"))
                .and_then(Value::as_array)
                .and_then(|tool_calls| tool_calls.first())
                .and_then(|tool_call| tool_call.get("function"))
                .and_then(|function| function.get("name"))
                .and_then(Value::as_str),
            Some("get_weather")
        );
    }

    #[test]
    fn restores_shortened_tool_name_in_chat_payload() {
        let original_request = br#"{
            "model":"gpt-5.4",
            "messages":[{"role":"user","content":"run tool"}],
            "tools":[{
                "type":"function",
                "function":{
                    "name":"mcp__very_long_namespace_segment__very_long_server_name__super_long_tool_name_that_needs_shortening",
                    "description":"Long name",
                    "parameters":{"type":"object","properties":{}}
                }
            }]
        }"#;
        let responses_payload = json!({
            "id": "resp_tool_2",
            "model": "gpt-5.4",
            "status": "completed",
            "output": [{
                "type": "function_call",
                "call_id": "call_long",
                "name": "mcp__super_long_tool_name_that_needs_shortening",
                "arguments": "{}"
            }]
        });

        let chat_payload =
            build_chat_completion_payload(&responses_payload, "gpt-5.4", original_request);
        assert_eq!(
            chat_payload
                .get("choices")
                .and_then(Value::as_array)
                .and_then(|choices| choices.first())
                .and_then(|choice| choice.get("message"))
                .and_then(|message| message.get("tool_calls"))
                .and_then(Value::as_array)
                .and_then(|tool_calls| tool_calls.first())
                .and_then(|tool_call| tool_call.get("function"))
                .and_then(|function| function.get("name"))
                .and_then(Value::as_str),
            Some(
                "mcp__very_long_namespace_segment__very_long_server_name__super_long_tool_name_that_needs_shortening"
            )
        );
    }

    #[test]
    fn builds_chat_completion_stream_body_with_done_marker() {
        let upstream_sse = br#"data: {"type":"response.created","response":{"id":"resp_1","created_at":123,"model":"gpt-5.4"}}

data: {"type":"response.output_text.delta","delta":"stream-body"}

event: response.done
data: {"response":{"id":"resp_1","created_at":123,"model":"gpt-5.4","status":"completed","usage":{"input_tokens":1,"input_tokens_details":{"cached_tokens":1},"output_tokens":1,"total_tokens":2}}}

"#;

        let stream_body = build_chat_completion_stream_body(upstream_sse, br#"{}"#, "gpt-5.4");
        assert!(stream_body.contains("chat.completion.chunk"));
        assert!(stream_body.contains("stream-body"));
        assert!(stream_body.contains("\"cached_tokens\":1"));
        assert!(stream_body.contains("data: [DONE]"));
    }

    #[test]
    fn parses_responses_sse_payload_to_json() {
        let sse = br#"event: response.output_text.delta
data: {"type":"response.output_text.delta","delta":"hello "}

event: response.output_text.delta
data: {"type":"response.output_text.delta","delta":"world"}

event: response.completed
data: {"type":"response.completed","response":{"id":"resp_1","model":"gpt-5.4","status":"completed","usage":{"input_tokens":2,"output_tokens":2,"total_tokens":4}}}

data: [DONE]

"#;

        let parsed = parse_responses_payload_from_upstream(sse).expect("sse should be parsed");
        assert_eq!(
            parsed
                .get("response")
                .and_then(|value| value.get("id"))
                .and_then(Value::as_str),
            Some("resp_1")
        );
        assert_eq!(
            parsed
                .get("response")
                .and_then(|value| value.get("output_text"))
                .and_then(Value::as_str),
            Some("hello world")
        );
    }

    #[test]
    fn parses_response_done_sse_payload_to_json() {
        let sse = br#"event: response.output_text.delta
data: {"type":"response.output_text.delta","delta":"done body"}

event: response.done
data: {"response":{"id":"resp_done","model":"gpt-5.4","status":"completed","usage":{"input_tokens":3,"input_tokens_details":{"cached_tokens":2},"output_tokens":1,"total_tokens":4}}}

"#;

        let parsed = parse_responses_payload_from_upstream(sse).expect("sse should be parsed");
        assert_eq!(
            parsed
                .get("response")
                .and_then(|value| value.get("id"))
                .and_then(Value::as_str),
            Some("resp_done")
        );
        assert_eq!(
            parsed
                .get("response")
                .and_then(|value| value.get("usage"))
                .and_then(|value| value.get("input_tokens_details"))
                .and_then(|value| value.get("cached_tokens"))
                .and_then(Value::as_u64),
            Some(2)
        );
    }
}
