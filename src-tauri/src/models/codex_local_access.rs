use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CodexLocalAccessRoutingStrategy {
    Auto,
    QuotaHighFirst,
    QuotaLowFirst,
    PlanHighFirst,
    PlanLowFirst,
    ExpirySoonFirst,
}

impl Default for CodexLocalAccessRoutingStrategy {
    fn default() -> Self {
        Self::Auto
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CodexLocalApiSafetyPresetId {
    MaximumSafety,
    BalancedSelfUse,
    QuotaDrainCareful,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CodexRuntimeIntegrationMode {
    DirectProjection,
    #[serde(alias = "gateway_litellm")]
    CockpitApiService,
}

impl Default for CodexRuntimeIntegrationMode {
    fn default() -> Self {
        Self::DirectProjection
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CodexRuntimeAccountKind {
    #[serde(rename = "oauth", alias = "o_auth")]
    OAuth,
    Api,
    Unknown,
}

impl Default for CodexRuntimeAccountKind {
    fn default() -> Self {
        Self::Unknown
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexRuntimeModeState {
    #[serde(default)]
    pub mode: CodexRuntimeIntegrationMode,
    #[serde(default)]
    pub account_kind: CodexRuntimeAccountKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_account_id: Option<String>,
    #[serde(default)]
    pub updated_at: i64,
}

fn default_restrict_free_accounts() -> bool {
    false
}

fn default_follow_current_account() -> bool {
    false
}

pub const CODEX_LOCAL_API_SAFETY_SCHEMA_VERSION: u32 = 1;
pub const CODEX_LOCAL_API_DEFAULT_MAX_CONCURRENT_REQUESTS: u32 = 1;
pub const CODEX_LOCAL_API_DEFAULT_MIN_REQUEST_INTERVAL_SECONDS: u64 = 20;
pub const CODEX_LOCAL_API_DEFAULT_MAX_QUEUE_WAIT_SECONDS: u64 = 21;
pub const CODEX_LOCAL_API_DEFAULT_REQUEST_TIMEOUT_SECONDS: u64 = 600;
pub const CODEX_LOCAL_API_DEFAULT_MAX_REQUEST_BODY_MB: u32 = 64;
pub const CODEX_LOCAL_API_DEFAULT_MAX_RETRIES: u32 = 1;
pub const CODEX_LOCAL_API_DEFAULT_MAX_RETRY_ACCOUNTS: u32 = 2;
pub const CODEX_LOCAL_ACCESS_HEALTH_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CodexLocalApiFallbackMode {
    Disabled,
    NextRequestOnly,
    #[serde(other)]
    Unknown,
}

impl CodexLocalApiFallbackMode {
    #[cfg(test)]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::NextRequestOnly => "next_request_only",
            Self::Unknown => "unknown",
        }
    }
}

impl Default for CodexLocalApiFallbackMode {
    fn default() -> Self {
        Self::Disabled
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CodexLocalApiLoggingConfig {
    #[serde(default = "default_true")]
    pub redact_sensitive_values: bool,
    #[serde(default = "default_true")]
    pub include_request_id: bool,
    #[serde(default = "default_true")]
    pub include_account_hash: bool,
    #[serde(default = "default_true")]
    pub include_route: bool,
    #[serde(default = "default_true")]
    pub include_model: bool,
    #[serde(default = "default_true")]
    pub include_latency: bool,
    #[serde(default)]
    pub include_prompt_response: bool,
    #[serde(default)]
    pub include_raw_upstream_body: bool,
}

impl Default for CodexLocalApiLoggingConfig {
    fn default() -> Self {
        Self {
            redact_sensitive_values: true,
            include_request_id: true,
            include_account_hash: true,
            include_route: true,
            include_model: true,
            include_latency: true,
            include_prompt_response: false,
            include_raw_upstream_body: false,
        }
    }
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CodexLocalApiSafetyConfig {
    #[serde(default)]
    pub schema_version: u32,
    #[serde(default = "default_true")]
    pub hardened_local_mode: bool,
    #[serde(default)]
    pub max_concurrent_requests: u32,
    #[serde(default)]
    pub min_request_interval_seconds: u64,
    #[serde(default)]
    pub max_queue_wait_seconds: u64,
    #[serde(default)]
    pub request_timeout_seconds: u64,
    #[serde(default)]
    pub max_request_body_mb: u32,
    #[serde(default)]
    pub max_retries: u32,
    #[serde(default)]
    pub max_retry_accounts: u32,
    #[serde(default)]
    pub fallback_mode: CodexLocalApiFallbackMode,
    #[serde(default)]
    pub logging: CodexLocalApiLoggingConfig,
}

impl CodexLocalApiSafetyConfig {
    pub fn missing() -> Self {
        Self {
            schema_version: 0,
            ..Self::default()
        }
    }
}

impl Default for CodexLocalApiSafetyConfig {
    fn default() -> Self {
        Self {
            schema_version: CODEX_LOCAL_API_SAFETY_SCHEMA_VERSION,
            hardened_local_mode: true,
            max_concurrent_requests: CODEX_LOCAL_API_DEFAULT_MAX_CONCURRENT_REQUESTS,
            min_request_interval_seconds: CODEX_LOCAL_API_DEFAULT_MIN_REQUEST_INTERVAL_SECONDS,
            max_queue_wait_seconds: CODEX_LOCAL_API_DEFAULT_MAX_QUEUE_WAIT_SECONDS,
            request_timeout_seconds: CODEX_LOCAL_API_DEFAULT_REQUEST_TIMEOUT_SECONDS,
            max_request_body_mb: CODEX_LOCAL_API_DEFAULT_MAX_REQUEST_BODY_MB,
            max_retries: CODEX_LOCAL_API_DEFAULT_MAX_RETRIES,
            max_retry_accounts: CODEX_LOCAL_API_DEFAULT_MAX_RETRY_ACCOUNTS,
            fallback_mode: CodexLocalApiFallbackMode::default(),
            logging: CodexLocalApiLoggingConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexLocalAccessCollection {
    pub enabled: bool,
    pub port: u16,
    pub api_key: String,
    #[serde(default = "CodexLocalApiSafetyConfig::missing")]
    pub safety_config: CodexLocalApiSafetyConfig,
    #[serde(default)]
    pub routing_strategy: CodexLocalAccessRoutingStrategy,
    #[serde(default = "default_restrict_free_accounts")]
    pub restrict_free_accounts: bool,
    #[serde(default = "default_follow_current_account")]
    pub follow_current_account: bool,
    pub account_ids: Vec<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CodexLocalAccessAccountHealthStatus {
    Healthy,
    EstimatedAvailable,
    CoolingDown,
    Exhausted,
    AuthSuspect,
    ManualRequired,
    Disabled,
}

impl Default for CodexLocalAccessAccountHealthStatus {
    fn default() -> Self {
        Self::Healthy
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CodexLocalAccessAccountHealth {
    #[serde(default)]
    pub status: CodexLocalAccessAccountHealthStatus,
    #[serde(default)]
    pub cooldown_until_ms: Option<i64>,
    #[serde(default)]
    pub exhausted_at_ms: Option<i64>,
    #[serde(default)]
    pub estimated_reset_at_ms: Option<i64>,
    #[serde(default)]
    pub estimated_remaining_percentage: Option<i32>,
    #[serde(default)]
    pub last_observed_remaining_percentage: Option<i32>,
    #[serde(default)]
    pub reset_source: Option<String>,
    #[serde(default)]
    pub confidence: Option<String>,
    #[serde(default)]
    pub manual_required: bool,
    #[serde(default)]
    pub last_status: Option<u16>,
    #[serde(default)]
    pub last_error_type: Option<String>,
    #[serde(default)]
    pub last_provider_code: Option<String>,
    #[serde(default)]
    pub last_request_id: Option<String>,
    #[serde(default)]
    pub last_selected_at_ms: Option<i64>,
    #[serde(default)]
    pub last_success_at_ms: Option<i64>,
    #[serde(default)]
    pub last_quota_exhausted_at_ms: Option<i64>,
    #[serde(default)]
    pub api_service_success_count: u64,
    #[serde(default)]
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CodexLocalAccessModelCooldown {
    pub account_id: String,
    pub model: String,
    #[serde(default)]
    pub cooldown_until_ms: i64,
    #[serde(default)]
    pub last_error_type: Option<String>,
    #[serde(default)]
    pub last_request_id: Option<String>,
    #[serde(default)]
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CodexLocalAccessStickyBinding {
    pub binding_key: String,
    pub account_id: String,
    #[serde(default)]
    pub reason: String,
    #[serde(default)]
    pub expires_at_ms: i64,
    #[serde(default)]
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CodexLocalAccessGlobalError {
    pub error_type: String,
    #[serde(default)]
    pub status: Option<u16>,
    #[serde(default)]
    pub request_id: Option<String>,
    #[serde(default)]
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CodexLocalAccessHealthRegistry {
    #[serde(default)]
    pub schema_version: u32,
    #[serde(default)]
    pub updated_at: i64,
    #[serde(default)]
    pub accounts: BTreeMap<String, CodexLocalAccessAccountHealth>,
    #[serde(default)]
    pub model_cooldowns: BTreeMap<String, CodexLocalAccessModelCooldown>,
    #[serde(default)]
    pub sticky_bindings: BTreeMap<String, CodexLocalAccessStickyBinding>,
    #[serde(default)]
    pub request_affinity: BTreeMap<String, CodexLocalAccessStickyBinding>,
    #[serde(default)]
    pub last_global_error: Option<CodexLocalAccessGlobalError>,
}

impl Default for CodexLocalAccessHealthRegistry {
    fn default() -> Self {
        Self {
            schema_version: CODEX_LOCAL_ACCESS_HEALTH_SCHEMA_VERSION,
            updated_at: 0,
            accounts: BTreeMap::new(),
            model_cooldowns: BTreeMap::new(),
            sticky_bindings: BTreeMap::new(),
            request_affinity: BTreeMap::new(),
            last_global_error: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct CodexLocalAccessUsageStats {
    #[serde(default)]
    pub request_count: u64,
    #[serde(default)]
    pub success_count: u64,
    #[serde(default)]
    pub failure_count: u64,
    #[serde(default)]
    pub total_latency_ms: u64,
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub total_tokens: u64,
    #[serde(default)]
    pub cached_tokens: u64,
    #[serde(default)]
    pub reasoning_tokens: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct CodexLocalAccessAccountStats {
    pub account_id: String,
    pub email: String,
    #[serde(default)]
    pub usage: CodexLocalAccessUsageStats,
    #[serde(default)]
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct CodexLocalAccessStatsWindow {
    #[serde(default)]
    pub since: i64,
    #[serde(default)]
    pub updated_at: i64,
    #[serde(default)]
    pub totals: CodexLocalAccessUsageStats,
    #[serde(default)]
    pub accounts: Vec<CodexLocalAccessAccountStats>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct CodexLocalAccessUsageEvent {
    #[serde(default)]
    pub timestamp: i64,
    #[serde(default)]
    pub account_id: String,
    #[serde(default)]
    pub email: String,
    #[serde(default)]
    pub success: bool,
    #[serde(default)]
    pub latency_ms: u64,
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub total_tokens: u64,
    #[serde(default)]
    pub cached_tokens: u64,
    #[serde(default)]
    pub reasoning_tokens: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct CodexLocalAccessStats {
    #[serde(default)]
    pub since: i64,
    #[serde(default)]
    pub updated_at: i64,
    #[serde(default)]
    pub totals: CodexLocalAccessUsageStats,
    #[serde(default)]
    pub accounts: Vec<CodexLocalAccessAccountStats>,
    #[serde(default)]
    pub daily: CodexLocalAccessStatsWindow,
    #[serde(default)]
    pub weekly: CodexLocalAccessStatsWindow,
    #[serde(default)]
    pub monthly: CodexLocalAccessStatsWindow,
    #[serde(default)]
    pub events: Vec<CodexLocalAccessUsageEvent>,
}

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct CodexLocalAccessHealthSummary {
    pub schema_version: u32,
    pub updated_at: i64,
    pub unavailable: bool,
    pub load_error: Option<String>,
    #[serde(default)]
    pub accounts: Vec<CodexLocalAccessAccountHealthView>,
    pub healthy_count: usize,
    pub estimated_available_count: usize,
    pub cooling_count: usize,
    pub exhausted_count: usize,
    pub auth_suspect_count: usize,
    pub manual_required_count: usize,
    pub disabled_count: usize,
    pub active_model_cooldown_count: usize,
    pub sticky_account_hash: Option<String>,
    pub sticky_reason: Option<String>,
    pub sticky_expires_at_ms: Option<i64>,
    pub nearest_cooldown_until_ms: Option<i64>,
    pub last_error_type: Option<String>,
    pub last_status: Option<u16>,
    pub last_request_id: Option<String>,
    pub audit_degraded: bool,
    pub audit_error: Option<String>,
    pub audit_degraded_at_ms: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct CodexLocalAccessAccountHealthView {
    pub account_id: String,
    #[serde(default)]
    pub status: CodexLocalAccessAccountHealthStatus,
    #[serde(default)]
    pub manual_required: bool,
    #[serde(default)]
    pub cooldown_until_ms: Option<i64>,
    #[serde(default)]
    pub exhausted_at_ms: Option<i64>,
    #[serde(default)]
    pub estimated_reset_at_ms: Option<i64>,
    #[serde(default)]
    pub last_status: Option<u16>,
    #[serde(default)]
    pub last_error_type: Option<String>,
    #[serde(default)]
    pub last_provider_code: Option<String>,
    #[serde(default)]
    pub updated_at: i64,
    #[serde(default)]
    pub active_model_cooldown_count: usize,
    #[serde(default)]
    pub nearest_model_cooldown_until_ms: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexLocalAccessState {
    pub collection: Option<CodexLocalAccessCollection>,
    pub running: bool,
    pub api_port_url: Option<String>,
    pub base_url: Option<String>,
    pub lan_base_url: Option<String>,
    pub model_ids: Vec<String>,
    pub last_error: Option<String>,
    pub member_count: usize,
    pub effective_account_ids: Vec<String>,
    pub stats: CodexLocalAccessStats,
    pub health: CodexLocalAccessHealthSummary,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexLocalAccessPortCleanupResult {
    pub killed_count: u32,
    pub state: CodexLocalAccessState,
}
