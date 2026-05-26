use crate::models::codex::{CodexAccount, CodexQuota, CodexQuotaErrorInfo};
use crate::modules::{codex_account, logger};
use chrono::{DateTime, Utc};
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION};
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

// 使用 wham/usage 端点（Quotio 使用的）
const USAGE_URL: &str = "https://chatgpt.com/backend-api/wham/usage";
const COCKPIT_API_PROVIDER_ID: &str = "cockpit_api";
const LEGACY_NEW_API_PROVIDER_ID: &str = "new_api";
const COCKPIT_API_PLAN_TYPE: &str = "Cockpit Api";
const LEGACY_NEW_API_EXCLUSIVE_PLAN_TYPE: &str = "NEW_API_EXCLUSIVE";
const COCKPIT_API_BASE_URL: &str = "https://chongcodex.cn/v1";
const WEEKLY_WINDOW_MINUTES_THRESHOLD: i64 = 6 * 24 * 60;
const DIRECT_SESSION_SCAN_MAX_FILES: usize = 32;
const DIRECT_SESSION_SCAN_MAX_BYTES_PER_FILE: u64 = 256 * 1024;
const DIRECT_CODEX_LOG_SCAN_MAX_ROWS: i64 = 20_000;
const DIRECT_CODEX_LOG_INCREMENTAL_BOOTSTRAP_ROWS: i64 = 5_000;
const DIRECT_CODEX_LOG_INCREMENTAL_SCAN_MAX_ROWS: i64 = 5_000;
const DIRECT_ERROR_RESET_BACKOFF_MAX_SECONDS: i64 = 60 * 60;

fn get_header_value(headers: &HeaderMap, name: &str) -> String {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("-")
        .to_string()
}

fn extract_detail_code_from_body(body: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;

    if let Some(code) = value
        .get("detail")
        .and_then(|detail| detail.get("code"))
        .and_then(|code| code.as_str())
    {
        return Some(code.to_string());
    }

    if let Some(code) = value
        .get("error")
        .and_then(|error| error.get("code"))
        .and_then(|code| code.as_str())
    {
        return Some(code.to_string());
    }

    if let Some(code) = value.get("code").and_then(|code| code.as_str()) {
        return Some(code.to_string());
    }

    None
}

fn extract_error_code_from_message(message: &str) -> Option<String> {
    let marker = "[error_code:";
    if let Some(start) = message.find(marker) {
        let code_start = start + marker.len();
        let end = message[code_start..].find(']')?;
        return Some(message[code_start..code_start + end].to_string());
    }

    let marker = "error_code=";
    let start = message.find(marker)?;
    let code_start = start + marker.len();
    let tail = &message[code_start..];
    let end = tail
        .find(|ch: char| ch == ',' || ch == ']' || ch.is_whitespace())
        .unwrap_or(tail.len());
    let code = tail[..end].trim();
    if code.is_empty() {
        None
    } else {
        Some(code.to_string())
    }
}

fn extract_i64_marker_from_message(message: &str, marker: &str) -> Option<i64> {
    let start = message.find(marker)?;
    let value_start = start + marker.len();
    let end = message[value_start..].find(']')?;
    message[value_start..value_start + end].parse::<i64>().ok()
}

fn normalize_unix_timestamp_seconds(value: i64) -> i64 {
    if value > 1_000_000_000_000 {
        value / 1000
    } else {
        value
    }
}

fn first_i64_field<'a>(
    value: &'a serde_json::Value,
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

fn extract_quota_reset_hint_from_body(body: &str, now: i64) -> Option<(Option<i64>, Option<i64>)> {
    let root: serde_json::Value = serde_json::from_str(body).ok()?;
    let candidates = [
        Some(&root),
        root.get("error"),
        root.get("detail"),
        root.get("data"),
    ];

    for candidate in candidates.into_iter().flatten() {
        let reset_at = first_i64_field(candidate, ["reset_at", "resets_at"])
            .map(normalize_unix_timestamp_seconds);
        let reset_after_seconds = first_i64_field(
            candidate,
            [
                "reset_after_seconds",
                "resets_in_seconds",
                "resetAfterSeconds",
                "resetsInSeconds",
                "retry_after_seconds",
                "retry_after",
            ],
        )
        .filter(|seconds| *seconds >= 0);

        if reset_at.is_some() || reset_after_seconds.is_some() {
            let computed_reset_at =
                reset_at.or_else(|| reset_after_seconds.map(|seconds| now.saturating_add(seconds)));
            return Some((computed_reset_at, reset_after_seconds));
        }
    }

    None
}

fn write_quota_error(account: &mut CodexAccount, message: String) {
    account.quota_error = Some(CodexQuotaErrorInfo {
        code: extract_error_code_from_message(&message),
        message,
        timestamp: chrono::Utc::now().timestamp(),
    });
}

fn is_quota_exhaustion_error(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("usage_limit_reached")
        || lower.contains("insufficient_quota")
        || lower.contains("usage_limit")
        || lower.contains("usage limit")
        || lower.contains("limit_reached")
        || lower.contains("limit reached")
        || (lower.contains("quota")
            && (lower.contains("exceed") || lower.contains("limit") || lower.contains("exhaust")))
}

fn build_exhausted_quota_snapshot_with_hint(
    account: &CodexAccount,
    source: &str,
    reset_at: Option<i64>,
    reset_after_seconds: Option<i64>,
    now: i64,
) -> CodexQuota {
    let previous = account.quota.as_ref();
    let hourly_reset_time = reset_at.or_else(|| previous.and_then(|quota| quota.hourly_reset_time));
    let weekly_reset_time = reset_at.or_else(|| previous.and_then(|quota| quota.weekly_reset_time));
    CodexQuota {
        hourly_percentage: 0,
        hourly_reset_time,
        hourly_window_minutes: previous.and_then(|quota| quota.hourly_window_minutes),
        hourly_window_present: previous
            .and_then(|quota| quota.hourly_window_present)
            .or(Some(true)),
        weekly_percentage: 0,
        weekly_reset_time,
        weekly_window_minutes: previous.and_then(|quota| quota.weekly_window_minutes),
        weekly_window_present: previous
            .and_then(|quota| quota.weekly_window_present)
            .or(Some(true)),
        raw_data: Some(json!({
            "source": source,
            "quota_exhausted": true,
            "exhausted_at": now,
            "reset_at": reset_at,
            "reset_after_seconds": reset_after_seconds,
        })),
    }
}

fn build_exhausted_quota_snapshot(account: &CodexAccount, message: &str) -> CodexQuota {
    let now = chrono::Utc::now().timestamp();
    let reset_at = extract_i64_marker_from_message(message, "[reset_at:")
        .map(normalize_unix_timestamp_seconds);
    let reset_after_seconds = extract_i64_marker_from_message(message, "[reset_after_seconds:");
    build_exhausted_quota_snapshot_with_hint(
        account,
        "quota_refresh_error",
        reset_at,
        reset_after_seconds,
        now,
    )
}

fn write_quota_fetch_error(account: &mut CodexAccount, message: String) {
    let quota_exhausted = is_quota_exhaustion_error(&message);
    write_quota_error(account, message);
    if quota_exhausted {
        let message = account
            .quota_error
            .as_ref()
            .map(|error| error.message.as_str())
            .unwrap_or_default();
        account.quota = Some(build_exhausted_quota_snapshot(account, message));
        account.usage_updated_at = Some(chrono::Utc::now().timestamp());
    }
}

/// 使用率窗口（5小时/周）
#[derive(Debug, Clone, Serialize, Deserialize)]
struct WindowInfo {
    #[serde(rename = "used_percent")]
    used_percent: Option<i32>,
    #[serde(rename = "limit_window_seconds")]
    limit_window_seconds: Option<i64>,
    #[serde(rename = "reset_after_seconds")]
    reset_after_seconds: Option<i64>,
    #[serde(rename = "reset_at")]
    reset_at: Option<i64>,
}

/// 速率限制信息
#[derive(Debug, Clone, Serialize, Deserialize)]
struct RateLimitInfo {
    allowed: Option<bool>,
    #[serde(rename = "limit_reached")]
    limit_reached: Option<bool>,
    #[serde(rename = "primary_window")]
    primary_window: Option<WindowInfo>,
    #[serde(rename = "secondary_window")]
    secondary_window: Option<WindowInfo>,
}

/// 使用率响应
#[derive(Debug, Clone, Serialize, Deserialize)]
struct UsageResponse {
    #[serde(rename = "plan_type")]
    plan_type: Option<String>,
    #[serde(rename = "rate_limit")]
    rate_limit: Option<RateLimitInfo>,
    #[serde(rename = "code_review_rate_limit")]
    code_review_rate_limit: Option<RateLimitInfo>,
    #[serde(rename = "rate_limit_reached_type")]
    rate_limit_reached_type: Option<serde_json::Value>,
}

fn normalize_remaining_percentage(window: &WindowInfo) -> i32 {
    let used = window.used_percent.unwrap_or(0).clamp(0, 100);
    100 - used
}

fn normalize_window_minutes(window: &WindowInfo) -> Option<i64> {
    let seconds = window.limit_window_seconds?;
    if seconds <= 0 {
        return None;
    }
    Some((seconds + 59) / 60)
}

fn is_weekly_window(window: &WindowInfo) -> bool {
    normalize_window_minutes(window)
        .map(|minutes| minutes >= WEEKLY_WINDOW_MINUTES_THRESHOLD)
        .unwrap_or(false)
}

fn normalize_reset_time(window: &WindowInfo) -> Option<i64> {
    if let Some(reset_at) = window.reset_at {
        return Some(normalize_unix_timestamp_seconds(reset_at));
    }

    let reset_after_seconds = window.reset_after_seconds?;
    if reset_after_seconds < 0 {
        return None;
    }

    Some(chrono::Utc::now().timestamp() + reset_after_seconds)
}

fn read_rate_limit_reached_type(value: &serde_json::Value) -> Option<String> {
    if let Some(kind) = value.as_str() {
        return Some(kind.trim().to_ascii_lowercase());
    }

    value
        .get("type")
        .or_else(|| value.get("kind"))
        .and_then(|item| item.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_ascii_lowercase)
}

fn is_exhausted_rate_limit_reached_type(value: Option<&serde_json::Value>) -> bool {
    let Some(kind) = value.and_then(read_rate_limit_reached_type) else {
        return false;
    };

    matches!(
        kind.as_str(),
        "rate_limit_reached"
            | "workspace_owner_credits_depleted"
            | "workspace_member_credits_depleted"
            | "workspace_owner_usage_limit_reached"
            | "workspace_member_usage_limit_reached"
            | "usage_limit_reached"
            | "credits_depleted"
    )
}

fn rate_limit_marks_exhausted(rate_limit: Option<&RateLimitInfo>) -> bool {
    let Some(rate_limit) = rate_limit else {
        return false;
    };

    rate_limit.limit_reached == Some(true) || rate_limit.allowed == Some(false)
}

fn apply_window_to_quota_slots(
    window: &WindowInfo,
    exhausted: bool,
    hourly: &mut (i32, Option<i64>, Option<i64>, bool),
    weekly: &mut (i32, Option<i64>, Option<i64>, bool),
) {
    let remaining = if exhausted {
        0
    } else {
        normalize_remaining_percentage(window)
    };
    let reset_time = normalize_reset_time(window);
    let window_minutes = normalize_window_minutes(window);
    let target = if is_weekly_window(window) {
        weekly
    } else {
        hourly
    };

    if !target.3 || remaining < target.0 {
        target.0 = remaining;
        target.1 = reset_time;
        target.2 = window_minutes;
        target.3 = true;
    }
}

/// 配额查询结果（包含 plan_type）
pub struct FetchQuotaResult {
    pub quota: CodexQuota,
    pub plan_type: Option<String>,
}

async fn refresh_account_tokens(account: &mut CodexAccount, reason: &str) -> Result<(), String> {
    logger::log_info(&format!(
        "Codex 账号 {} 触发强制 Token 刷新: {}",
        account.email, reason
    ));

    let refreshed = codex_account::force_refresh_managed_account(&account.id, reason)
        .await
        .map_err(|e| format!("{}，刷新 Token 失败: {}", reason, e))?;
    *account = refreshed;
    Ok(())
}

/// 查询单个账号的配额
pub async fn fetch_quota(account: &CodexAccount) -> Result<FetchQuotaResult, String> {
    let client = reqwest::Client::new();

    let mut headers = HeaderMap::new();
    headers.insert(
        AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {}", account.tokens.access_token))
            .map_err(|e| format!("构建 Authorization 头失败: {}", e))?,
    );
    headers.insert(ACCEPT, HeaderValue::from_static("application/json"));

    // 添加 ChatGPT-Account-Id 头（关键！）
    let account_id = account.account_id.clone().or_else(|| {
        codex_account::extract_chatgpt_account_id_from_access_token(&account.tokens.access_token)
    });

    if let Some(ref acc_id) = account_id {
        if !acc_id.is_empty() {
            headers.insert(
                "ChatGPT-Account-Id",
                HeaderValue::from_str(acc_id)
                    .map_err(|e| format!("构建 Account-Id 头失败: {}", e))?,
            );
        }
    }

    logger::log_info(&format!(
        "Codex 配额请求: {} (account_id: {:?})",
        USAGE_URL, account_id
    ));

    let response = client
        .get(USAGE_URL)
        .headers(headers)
        .send()
        .await
        .map_err(|e| format!("请求失败: {}", e))?;

    let status = response.status();
    let headers = response.headers().clone();
    let body = response
        .text()
        .await
        .map_err(|e| format!("读取响应失败: {}", e))?;

    let request_id = get_header_value(&headers, "request-id");
    let x_request_id = get_header_value(&headers, "x-request-id");
    let cf_ray = get_header_value(&headers, "cf-ray");
    let body_len = body.len();

    logger::log_info(&format!(
        "Codex 配额响应元信息: url={}, status={}, request-id={}, x-request-id={}, cf-ray={}, body_len={}",
        USAGE_URL, status, request_id, x_request_id, cf_ray, body_len
    ));

    if !status.is_success() {
        let detail_code = extract_detail_code_from_body(&body);
        let quota_reset_hint =
            extract_quota_reset_hint_from_body(&body, chrono::Utc::now().timestamp());

        logger::log_error(&format!(
            "Codex 配额接口返回非成功状态: url={}, status={}, request-id={}, x-request-id={}, cf-ray={}, detail_code={:?}, body_len={}",
            USAGE_URL,
            status,
            request_id,
            x_request_id,
            cf_ray,
            detail_code,
            body_len
        ));

        let mut error_message = format!("API 返回错误 {}", status);
        if let Some(code) = detail_code {
            error_message.push_str(&format!(" [error_code:{}]", code));
        }
        if let Some((reset_at, reset_after_seconds)) = quota_reset_hint {
            if let Some(reset_at) = reset_at {
                error_message.push_str(&format!(" [reset_at:{}]", reset_at));
            }
            if let Some(reset_after_seconds) = reset_after_seconds {
                error_message.push_str(&format!(" [reset_after_seconds:{}]", reset_after_seconds));
            }
        }
        error_message.push_str(&format!(" [body_len:{}]", body_len));
        return Err(error_message);
    }

    // 解析响应
    let usage: UsageResponse =
        serde_json::from_str(&body).map_err(|e| format!("解析 JSON 失败: {}", e))?;

    let quota = parse_quota_from_usage(&usage, &body)?;
    let plan_type = usage.plan_type.clone();

    Ok(FetchQuotaResult { quota, plan_type })
}

/// 从使用率响应中解析配额信息
fn parse_quota_from_usage(usage: &UsageResponse, raw_body: &str) -> Result<CodexQuota, String> {
    let rate_limit = usage.rate_limit.as_ref();
    let primary_window = rate_limit.and_then(|r| r.primary_window.as_ref());
    let secondary_window = rate_limit.and_then(|r| r.secondary_window.as_ref());
    let exhausted = rate_limit_marks_exhausted(rate_limit)
        || is_exhausted_rate_limit_reached_type(usage.rate_limit_reached_type.as_ref());

    let mut hourly = (100, None, None, false);
    let mut weekly = (100, None, None, false);

    for window in [primary_window, secondary_window].into_iter().flatten() {
        apply_window_to_quota_slots(window, exhausted, &mut hourly, &mut weekly);
    }

    // 保存原始响应
    let raw_data: Option<serde_json::Value> = serde_json::from_str(raw_body).ok();

    Ok(CodexQuota {
        hourly_percentage: hourly.0,
        hourly_reset_time: hourly.1,
        hourly_window_minutes: hourly.2,
        hourly_window_present: Some(hourly.3),
        weekly_percentage: weekly.0,
        weekly_reset_time: weekly.1,
        weekly_window_minutes: weekly.2,
        weekly_window_present: Some(weekly.3),
        raw_data,
    })
}

fn quota_reset_time_for_preservation(quota: &CodexQuota) -> Option<i64> {
    [quota.hourly_reset_time, quota.weekly_reset_time]
        .into_iter()
        .flatten()
        .max()
}

fn should_keep_existing_quota_exhaustion(account: &CodexAccount, now: i64) -> bool {
    let Some(error) = account.quota_error.as_ref() else {
        return false;
    };
    let code_or_message = error.code.as_deref().unwrap_or(error.message.as_str());
    if !is_quota_exhaustion_error(code_or_message) && !is_quota_exhaustion_error(&error.message) {
        return false;
    }

    let Some(quota) = account.quota.as_ref() else {
        return false;
    };
    if quota.hourly_percentage > 0 || quota.weekly_percentage > 0 {
        return false;
    }

    quota_reset_time_for_preservation(quota)
        .map(|reset_at| reset_at > now)
        .unwrap_or(false)
}

#[derive(Debug, Clone)]
struct DirectQuotaObservation {
    observed_at: i64,
    reset_at: Option<i64>,
    reset_after_seconds: Option<i64>,
    error_type: Option<String>,
    source: String,
    quota: Option<CodexQuota>,
    exhausted: bool,
}

#[derive(Debug, Clone)]
struct DirectCodexLogRow {
    id: i64,
    ts: i64,
    thread_id: Option<String>,
    body: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct DirectCodexLogSyncState {
    #[serde(default)]
    last_log_id: i64,
    #[serde(default)]
    last_synced_at_ms: i64,
}

fn parse_session_timestamp_seconds(value: &serde_json::Value) -> Option<i64> {
    let raw = value.get("timestamp").and_then(|item| item.as_str())?;
    DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|parsed| parsed.with_timezone(&Utc).timestamp())
}

fn read_f64_field(value: &serde_json::Value, key: &str) -> Option<f64> {
    value.get(key).and_then(|item| {
        item.as_f64().or_else(|| {
            item.as_str()
                .and_then(|text| text.trim().parse::<f64>().ok())
        })
    })
}

fn direct_window_reset_at(window: &serde_json::Value, observed_at: i64) -> Option<i64> {
    if let Some(reset_at) = first_i64_field(
        window,
        [
            "resets_at",
            "reset_at",
            "resetAt",
            "reset_at_seconds",
            "reset_timestamp",
        ],
    )
    .map(normalize_unix_timestamp_seconds)
    {
        return Some(reset_at);
    }

    first_i64_field(
        window,
        [
            "resets_in_seconds",
            "reset_after_seconds",
            "resetAfterSeconds",
            "retry_after_seconds",
            "retry_after",
        ],
    )
    .filter(|seconds| *seconds >= 0)
    .map(|seconds| observed_at + seconds)
}

fn direct_window_minutes(window: &serde_json::Value) -> Option<i64> {
    first_i64_field(
        window,
        [
            "window_minutes",
            "limit_window_minutes",
            "limitWindowMinutes",
        ],
    )
    .filter(|minutes| *minutes > 0)
    .or_else(|| {
        first_i64_field(
            window,
            [
                "limit_window_seconds",
                "window_seconds",
                "limitWindowSeconds",
            ],
        )
        .filter(|seconds| *seconds > 0)
        .map(|seconds| (seconds + 59) / 60)
    })
}

fn direct_window_is_weekly(window: &serde_json::Value) -> bool {
    direct_window_minutes(window)
        .map(|minutes| minutes >= WEEKLY_WINDOW_MINUTES_THRESHOLD)
        .unwrap_or(false)
}

fn direct_window_remaining_percentage(window: &serde_json::Value, exhausted: bool) -> Option<i32> {
    if exhausted {
        return Some(0);
    }

    let used = read_f64_field(window, "used_percent")?.clamp(0.0, 100.0);
    Some((100.0 - used).round().clamp(0.0, 100.0) as i32)
}

fn apply_direct_window_to_quota_slots(
    window: &serde_json::Value,
    exhausted: bool,
    observed_at: i64,
    hourly: &mut (i32, Option<i64>, Option<i64>, bool),
    weekly: &mut (i32, Option<i64>, Option<i64>, bool),
) {
    let Some(remaining) = direct_window_remaining_percentage(window, exhausted) else {
        return;
    };
    let reset_time = direct_window_reset_at(window, observed_at);
    let window_minutes = direct_window_minutes(window);
    let target = if direct_window_is_weekly(window) {
        weekly
    } else {
        hourly
    };

    if !target.3 || remaining < target.0 {
        target.0 = remaining;
        target.1 = reset_time;
        target.2 = window_minutes;
        target.3 = true;
    }
}

fn parse_direct_rate_limits_observation(
    rate_limits: &serde_json::Value,
    observed_at: i64,
) -> Option<DirectQuotaObservation> {
    let reached_type = rate_limits
        .get("rate_limit_reached_type")
        .and_then(read_rate_limit_reached_type);
    let reached_type_exhausted = rate_limits
        .get("rate_limit_reached_type")
        .is_some_and(|value| is_exhausted_rate_limit_reached_type(Some(value)));
    let limit_reached_exhausted = rate_limits
        .get("limit_reached")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
        || rate_limits
            .get("allowed")
            .and_then(|value| value.as_bool())
            .is_some_and(|allowed| !allowed);

    let primary = rate_limits.get("primary");
    let secondary = rate_limits.get("secondary");
    let windows: Vec<&serde_json::Value> = [primary, secondary].into_iter().flatten().collect();
    if windows.is_empty() {
        return None;
    }

    let exhausted_window = windows.iter().copied().find(|window| {
        read_f64_field(window, "used_percent")
            .map(|used| used >= 100.0)
            .unwrap_or(false)
    });
    let fallback_window = windows
        .iter()
        .copied()
        .find(|window| direct_window_reset_at(window, observed_at).is_some());

    let exhausted = reached_type_exhausted || limit_reached_exhausted || exhausted_window.is_some();
    let mut hourly = (100, None, None, false);
    let mut weekly = (100, None, None, false);
    for window in &windows {
        apply_direct_window_to_quota_slots(
            window,
            exhausted,
            observed_at,
            &mut hourly,
            &mut weekly,
        );
    }

    if !hourly.3 && !weekly.3 {
        return None;
    }

    let reset_at = exhausted_window
        .and_then(|window| direct_window_reset_at(window, observed_at))
        .or_else(|| fallback_window.and_then(|window| direct_window_reset_at(window, observed_at)));
    let error_type = if exhausted {
        Some(reached_type.unwrap_or_else(|| "usage_limit_reached".to_string()))
    } else {
        reached_type
    };
    let quota_reset_at = [hourly.1, weekly.1].into_iter().flatten().max();
    let quota = CodexQuota {
        hourly_percentage: hourly.0,
        hourly_reset_time: hourly.1,
        hourly_window_minutes: hourly.2,
        hourly_window_present: Some(hourly.3),
        weekly_percentage: weekly.0,
        weekly_reset_time: weekly.1,
        weekly_window_minutes: weekly.2,
        weekly_window_present: Some(weekly.3),
        raw_data: Some(json!({
            "source": "codex_session_rate_limits",
            "observed_at": observed_at,
            "quota_exhausted": exhausted,
            "rate_limit_reached_type": error_type.as_deref(),
            "plan_type": rate_limits.get("plan_type").and_then(|value| value.as_str()),
        })),
    };

    Some(DirectQuotaObservation {
        observed_at,
        reset_at: reset_at.or(quota_reset_at),
        reset_after_seconds: reset_at
            .or(quota_reset_at)
            .map(|reset| reset.saturating_sub(observed_at).max(0)),
        error_type,
        source: "codex_session_rate_limits".to_string(),
        quota: Some(quota),
        exhausted,
    })
}

fn value_has_usage_limit_marker(value: &serde_json::Value) -> bool {
    let Some(text) = value.as_str() else {
        return false;
    };
    let lower = text.to_ascii_lowercase();
    matches!(
        lower.as_str(),
        "usage_limit_reached"
            | "insufficient_quota"
            | "workspace_owner_credits_depleted"
            | "workspace_member_credits_depleted"
            | "workspace_owner_usage_limit_reached"
            | "workspace_member_usage_limit_reached"
            | "credits_depleted"
    )
}

fn find_usage_limit_object(value: &serde_json::Value) -> Option<&serde_json::Value> {
    let object = value.as_object()?;
    if object
        .get("type")
        .or_else(|| object.get("code"))
        .is_some_and(value_has_usage_limit_marker)
    {
        return Some(value);
    }

    for item in object.values() {
        if item.is_object() {
            if let Some(found) = find_usage_limit_object(item) {
                return Some(found);
            }
        } else if let Some(items) = item.as_array() {
            for child in items {
                if let Some(found) = find_usage_limit_object(child) {
                    return Some(found);
                }
            }
        }
    }

    None
}

fn parse_direct_error_observation(
    root: &serde_json::Value,
    observed_at: i64,
) -> Option<DirectQuotaObservation> {
    let error = find_usage_limit_object(root)?;
    let error_type = error
        .get("type")
        .or_else(|| error.get("code"))
        .and_then(|item| item.as_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_else(|| "usage_limit_reached".to_string());
    let reset_at = first_i64_field(
        error,
        [
            "resets_at",
            "reset_at",
            "resetAt",
            "reset_at_seconds",
            "reset_timestamp",
        ],
    )
    .map(normalize_unix_timestamp_seconds);
    let reset_after_seconds = first_i64_field(
        error,
        [
            "resets_in_seconds",
            "reset_after_seconds",
            "resetAfterSeconds",
            "retry_after_seconds",
            "retry_after",
        ],
    )
    .filter(|seconds| *seconds >= 0);
    let reset_at = reset_at.or_else(|| reset_after_seconds.map(|seconds| observed_at + seconds));

    Some(DirectQuotaObservation {
        observed_at,
        reset_at,
        reset_after_seconds,
        error_type: Some(error_type),
        source: "codex_session_error".to_string(),
        quota: None,
        exhausted: true,
    })
}

fn parse_direct_quota_observation_from_session_line(line: &str) -> Option<DirectQuotaObservation> {
    if !line.contains("rate_limits")
        && !line.contains("usage_limit_reached")
        && !line.contains("insufficient_quota")
        && !line.contains("credits_depleted")
    {
        return None;
    }

    let root: serde_json::Value = serde_json::from_str(line).ok()?;
    let observed_at = parse_session_timestamp_seconds(&root)?;
    if root.get("type").and_then(|item| item.as_str()) != Some("event_msg") {
        return None;
    }

    let payload = root.get("payload")?;
    let payload_type = payload.get("type").and_then(|item| item.as_str());
    if payload_type == Some("token_count") {
        let rate_limits = payload.get("rate_limits")?;
        if let Some(observation) = parse_direct_rate_limits_observation(rate_limits, observed_at) {
            return Some(observation);
        }
    }

    if payload_type == Some("error") {
        return parse_direct_error_observation(payload, observed_at);
    }

    None
}

fn collect_session_jsonl_files(root: &Path) -> Vec<PathBuf> {
    let sessions_root = root.join("sessions");
    let mut stack = vec![sessions_root];
    let mut files = Vec::new();

    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().and_then(|ext| ext.to_str()) == Some("jsonl") {
                files.push(path);
            }
        }
    }

    files.sort_by_key(|path| {
        fs::metadata(path)
            .and_then(|metadata| metadata.modified())
            .ok()
    });
    files.reverse();
    files.truncate(DIRECT_SESSION_SCAN_MAX_FILES);
    files
}

fn read_recent_session_text(path: &Path) -> Option<String> {
    let mut file = fs::File::open(path).ok()?;
    let size = file.metadata().ok()?.len();
    let start = size.saturating_sub(DIRECT_SESSION_SCAN_MAX_BYTES_PER_FILE);
    if start > 0 {
        file.seek(SeekFrom::Start(start)).ok()?;
    }

    let mut content = String::new();
    file.read_to_string(&mut content).ok()?;
    if start == 0 {
        return Some(content);
    }

    let newline = content.find('\n')?;
    Some(content[newline + 1..].to_string())
}

fn latest_direct_quota_observation_from_sessions(
    codex_home: &Path,
    since_seconds: i64,
) -> Option<DirectQuotaObservation> {
    let mut best: Option<DirectQuotaObservation> = None;
    for path in collect_session_jsonl_files(codex_home) {
        let Some(content) = read_recent_session_text(&path) else {
            continue;
        };
        for line in content.lines().rev() {
            let Some(observation) = parse_direct_quota_observation_from_session_line(line) else {
                continue;
            };
            if observation.observed_at < since_seconds {
                continue;
            }
            if is_better_direct_quota_observation(&observation, best.as_ref()) {
                best = Some(observation);
            }
        }
    }

    best
}

fn direct_identity_key(kind: &str, value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    Some(format!("{}:{}", kind, value.to_ascii_lowercase()))
}

fn insert_direct_identity_mapping(
    index: &mut HashMap<String, Option<String>>,
    key: Option<String>,
    account_id: &str,
) {
    let Some(key) = key else {
        return;
    };
    match index.get_mut(&key) {
        Some(existing) if existing.as_deref() == Some(account_id) => {}
        Some(existing) => {
            *existing = None;
        }
        None => {
            index.insert(key, Some(account_id.to_string()));
        }
    }
}

fn direct_account_identity_index(accounts: &[CodexAccount]) -> HashMap<String, String> {
    let mut index: HashMap<String, Option<String>> = HashMap::new();
    for account in accounts {
        insert_direct_identity_mapping(
            &mut index,
            account
                .account_id
                .as_deref()
                .and_then(|value| direct_identity_key("account_id", value)),
            &account.id,
        );
        insert_direct_identity_mapping(
            &mut index,
            direct_identity_key("email", &account.email),
            &account.id,
        );
        insert_direct_identity_mapping(
            &mut index,
            account
                .user_id
                .as_deref()
                .and_then(|value| direct_identity_key("user_id", value)),
            &account.id,
        );
    }
    index
        .into_iter()
        .filter_map(|(key, value)| value.map(|account_id| (key, account_id)))
        .collect()
}

fn extract_quoted_log_attribute(body: &str, name: &str) -> Option<String> {
    let marker = format!("{}=\"", name);
    let start = body.find(&marker)? + marker.len();
    let tail = &body[start..];
    let end = tail.find('"')?;
    let value = tail[..end].trim();
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

fn account_id_from_codex_log_identity(
    body: &str,
    identity_index: &HashMap<String, String>,
) -> Option<String> {
    [
        ("account_id", "user.account_id"),
        ("email", "user.email"),
        ("user_id", "user.id"),
    ]
    .into_iter()
    .filter_map(|(kind, attr)| {
        extract_quoted_log_attribute(body, attr).and_then(|value| direct_identity_key(kind, &value))
    })
    .find_map(|key| identity_index.get(&key).cloned())
}

fn extract_websocket_event_value(body: &str) -> Option<serde_json::Value> {
    let marker = "websocket event:";
    let start = body.find(marker)? + marker.len();
    let tail = body[start..].trim_start();
    let mut stream = serde_json::Deserializer::from_str(tail).into_iter::<serde_json::Value>();
    stream.next()?.ok()
}

fn set_direct_observation_source(observation: &mut DirectQuotaObservation, source: &str) {
    observation.source = source.to_string();
    if let Some(quota) = observation.quota.as_mut() {
        if let Some(raw_data) = quota
            .raw_data
            .as_mut()
            .and_then(|value| value.as_object_mut())
        {
            raw_data.insert(
                "source".to_string(),
                serde_json::Value::String(source.to_string()),
            );
        }
    }
}

fn parse_direct_quota_observation_from_websocket_event(
    event: &serde_json::Value,
    observed_at: i64,
) -> Option<DirectQuotaObservation> {
    let event_type = event.get("type").and_then(|value| value.as_str())?;
    if event_type == "codex.rate_limits" {
        let mut rate_limits = event.get("rate_limits")?.clone();
        if let Some(object) = rate_limits.as_object_mut() {
            for key in ["plan_type", "rate_limit_reached_type"] {
                if !object.contains_key(key) {
                    if let Some(value) = event.get(key) {
                        object.insert(key.to_string(), value.clone());
                    }
                }
            }
        }
        let mut observation = parse_direct_rate_limits_observation(&rate_limits, observed_at)?;
        set_direct_observation_source(&mut observation, "codex_official_websocket_rate_limits");
        return Some(observation);
    }

    let lower_type = event_type.to_ascii_lowercase();
    let error_like = lower_type.contains("error")
        || lower_type.contains("failed")
        || event.get("error").is_some()
        || event
            .get("response")
            .and_then(|response| response.get("error"))
            .is_some();
    if error_like {
        let mut observation = parse_direct_error_observation(event, observed_at)?;
        set_direct_observation_source(&mut observation, "codex_official_websocket_error");
        return Some(observation);
    }

    None
}

fn direct_observation_quota_reset_hint(observation: &DirectQuotaObservation) -> Option<i64> {
    observation
        .quota
        .as_ref()
        .and_then(quota_reset_time_for_preservation)
        .or(observation.reset_at)
}

fn direct_error_reset_looks_like_short_backoff(observation: &DirectQuotaObservation) -> bool {
    if observation.source != "codex_official_websocket_error" {
        return false;
    }

    observation
        .reset_after_seconds
        .map(|seconds| seconds <= DIRECT_ERROR_RESET_BACKOFF_MAX_SECONDS)
        .unwrap_or_else(|| {
            observation
                .reset_at
                .map(|reset_at| {
                    reset_at <= observation.observed_at + DIRECT_ERROR_RESET_BACKOFF_MAX_SECONDS
                })
                .unwrap_or(false)
        })
}

fn apply_direct_observation_reset_hint(observation: &mut DirectQuotaObservation, reset_at: i64) {
    observation.reset_at = Some(reset_at);
    observation.reset_after_seconds = Some(reset_at.saturating_sub(observation.observed_at).max(0));
    if let Some(quota) = observation.quota.as_mut() {
        if quota.hourly_window_present.unwrap_or(false) && quota.hourly_percentage <= 0 {
            quota.hourly_reset_time = Some(reset_at);
        }
        if quota.weekly_window_present.unwrap_or(false) && quota.weekly_percentage <= 0 {
            quota.weekly_reset_time = Some(reset_at);
        }
        if let Some(raw_data) = quota
            .raw_data
            .as_mut()
            .and_then(|value| value.as_object_mut())
        {
            raw_data.insert(
                "reset_at".to_string(),
                serde_json::Value::Number(reset_at.into()),
            );
            raw_data.insert(
                "reset_source".to_string(),
                serde_json::Value::String("codex_official_websocket_rate_limits".to_string()),
            );
        }
    }
}

fn direct_observation_since_seconds(account: &CodexAccount) -> i64 {
    account
        .last_used
        .max(account.usage_updated_at.unwrap_or(0))
        .saturating_sub(3600)
}

fn latest_direct_quota_observations_from_codex_log_rows(
    accounts: &[CodexAccount],
    mut rows: Vec<DirectCodexLogRow>,
) -> HashMap<String, DirectQuotaObservation> {
    rows.sort_by_key(|row| row.id);
    let identity_index = direct_account_identity_index(accounts);
    let account_since: HashMap<&str, i64> = accounts
        .iter()
        .map(|account| {
            (
                account.id.as_str(),
                direct_observation_since_seconds(account),
            )
        })
        .collect();
    let mut thread_accounts: HashMap<String, String> = HashMap::new();
    let mut observations: HashMap<String, DirectQuotaObservation> = HashMap::new();
    let mut reset_hints: HashMap<String, i64> = HashMap::new();

    // 官方 Codex 日志里身份事件可能早于或晚于 quota 事件；先按 thread 建索引，避免顺序差异漏配账号。
    for row in &rows {
        if let Some(thread_id) = row.thread_id.as_deref() {
            if let Some(account_id) = account_id_from_codex_log_identity(&row.body, &identity_index)
            {
                thread_accounts.insert(thread_id.to_string(), account_id);
            }
        }
    }

    for row in rows {
        if let Some(thread_id) = row.thread_id.as_deref() {
            if let Some(account_id) = account_id_from_codex_log_identity(&row.body, &identity_index)
            {
                thread_accounts.insert(thread_id.to_string(), account_id);
            }

            let Some(account_id) = thread_accounts.get(thread_id) else {
                continue;
            };
            let Some(event) = extract_websocket_event_value(&row.body) else {
                continue;
            };
            let Some(observation) =
                parse_direct_quota_observation_from_websocket_event(&event, row.ts)
            else {
                continue;
            };
            if observation.observed_at
                < account_since
                    .get(account_id.as_str())
                    .copied()
                    .unwrap_or_default()
            {
                continue;
            }
            if let Some(reset_at) = direct_observation_quota_reset_hint(&observation) {
                if reset_at > observation.observed_at {
                    reset_hints
                        .entry(account_id.clone())
                        .and_modify(|existing| *existing = (*existing).max(reset_at))
                        .or_insert(reset_at);
                }
            }
            if is_better_direct_quota_observation(&observation, observations.get(account_id)) {
                observations.insert(account_id.clone(), observation);
            }
        }
    }

    for (account_id, observation) in observations.iter_mut() {
        if !observation.exhausted || !direct_error_reset_looks_like_short_backoff(observation) {
            continue;
        }
        let Some(reset_at) = reset_hints.get(account_id).copied() else {
            continue;
        };
        if observation
            .reset_at
            .map(|current| reset_at > current)
            .unwrap_or(true)
        {
            apply_direct_observation_reset_hint(observation, reset_at);
        }
    }

    observations
}

fn map_direct_codex_log_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<DirectCodexLogRow> {
    Ok(DirectCodexLogRow {
        id: row.get(0)?,
        ts: row.get(1)?,
        thread_id: row.get(2)?,
        body: row.get(3)?,
    })
}

fn direct_codex_log_body_may_contain_quota_observation(body: &str) -> bool {
    body.contains("websocket event:")
        && (body.contains(r#""type":"codex.rate_limits""#)
            || body.contains("usage_limit_reached")
            || body.contains("insufficient_quota")
            || body.contains("credits_depleted"))
}

fn append_direct_codex_log_identity_rows(
    connection: &Connection,
    result: &mut Vec<DirectCodexLogRow>,
    thread_ids: Vec<String>,
) -> Result<(), String> {
    if thread_ids.is_empty() {
        return Ok(());
    }

    let mut identity_statement = connection
        .prepare(
            r#"
            SELECT id, ts, thread_id, feedback_log_body
            FROM logs
            WHERE thread_id = ?1
              AND (
                    feedback_log_body LIKE '%user.account_id=%'
                    OR feedback_log_body LIKE '%user.email=%'
              )
            ORDER BY id DESC
            LIMIT 64
            "#,
        )
        .map_err(|error| format!("读取 Codex Desktop 身份日志 schema 失败: {}", error))?;

    for thread_id in thread_ids {
        let mut identity_rows = identity_statement
            .query_map([thread_id.as_str()], map_direct_codex_log_row)
            .map_err(|error| format!("查询 Codex Desktop 身份日志失败: {}", error))?;
        while let Some(row) = identity_rows
            .next()
            .transpose()
            .map_err(|error| format!("解析 Codex Desktop 身份日志失败: {}", error))?
        {
            result.push(row);
        }
    }
    Ok(())
}

fn direct_codex_log_thread_ids_for_identity_backfill(rows: &[DirectCodexLogRow]) -> Vec<String> {
    rows.iter()
        .filter(|row| direct_codex_log_body_may_contain_quota_observation(&row.body))
        .filter_map(|row| row.thread_id.clone())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect()
}

fn read_recent_codex_log_rows(codex_home: &Path) -> Result<Vec<DirectCodexLogRow>, String> {
    let path = codex_home.join("logs_2.sqlite");
    if !path.exists() {
        return Ok(Vec::new());
    }

    let connection = Connection::open_with_flags(
        &path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|error| format!("打开 Codex Desktop 日志失败: {}", error))?;
    let mut event_statement = connection
        .prepare(
            r#"
            SELECT id, ts, thread_id, feedback_log_body
            FROM logs
            WHERE feedback_log_body LIKE '%websocket event: {"type":"codex.rate_limits"%'
               OR (
                    feedback_log_body LIKE '%websocket event:%'
                    AND (
                        feedback_log_body LIKE '%usage_limit_reached%'
                        OR feedback_log_body LIKE '%insufficient_quota%'
                        OR feedback_log_body LIKE '%credits_depleted%'
                    )
               )
            ORDER BY id DESC
            LIMIT ?1
            "#,
        )
        .map_err(|error| format!("读取 Codex Desktop 日志 schema 失败: {}", error))?;
    let mut event_rows = event_statement
        .query_map([DIRECT_CODEX_LOG_SCAN_MAX_ROWS], map_direct_codex_log_row)
        .map_err(|error| format!("查询 Codex Desktop 日志失败: {}", error))?;

    let mut result = Vec::new();
    while let Some(row) = event_rows
        .next()
        .transpose()
        .map_err(|error| format!("解析 Codex Desktop 日志失败: {}", error))?
    {
        result.push(row);
    }

    let thread_ids = direct_codex_log_thread_ids_for_identity_backfill(&result);
    append_direct_codex_log_identity_rows(&connection, &mut result, thread_ids)?;
    Ok(result)
}

fn load_direct_codex_log_sync_state(path: &Path) -> DirectCodexLogSyncState {
    let Ok(content) = fs::read_to_string(path) else {
        return DirectCodexLogSyncState::default();
    };
    serde_json::from_str::<DirectCodexLogSyncState>(&content).unwrap_or_default()
}

fn save_direct_codex_log_sync_state(
    path: &Path,
    state: &DirectCodexLogSyncState,
) -> Result<(), String> {
    let content = serde_json::to_string_pretty(state)
        .map_err(|error| format!("序列化 Direct OAuth 日志同步游标失败: {}", error))?;
    crate::modules::atomic_write::write_string_atomic(path, &content)
        .map_err(|error| format!("写入 Direct OAuth 日志同步游标失败: {}", error))
}

fn read_incremental_codex_log_rows(
    codex_home: &Path,
    last_log_id: i64,
) -> Result<(Vec<DirectCodexLogRow>, i64), String> {
    let path = codex_home.join("logs_2.sqlite");
    if !path.exists() {
        return Ok((Vec::new(), last_log_id.max(0)));
    }

    let connection = Connection::open_with_flags(
        &path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|error| format!("打开 Codex Desktop 日志失败: {}", error))?;
    let max_log_id = connection
        .query_row("SELECT COALESCE(MAX(id), 0) FROM logs", [], |row| {
            row.get::<_, i64>(0)
        })
        .map_err(|error| format!("读取 Codex Desktop 日志游标失败: {}", error))?;
    if max_log_id <= last_log_id {
        return Ok((Vec::new(), last_log_id.max(max_log_id)));
    }

    let effective_last_log_id = if last_log_id > 0 {
        last_log_id
    } else {
        max_log_id.saturating_sub(DIRECT_CODEX_LOG_INCREMENTAL_BOOTSTRAP_ROWS)
    };
    let mut statement = connection
        .prepare(
            r#"
            SELECT id, ts, thread_id, feedback_log_body
            FROM logs
            WHERE id > ?1
            ORDER BY id ASC
            LIMIT ?2
            "#,
        )
        .map_err(|error| format!("读取 Codex Desktop 增量日志 schema 失败: {}", error))?;
    let mut rows = statement
        .query_map(
            [
                effective_last_log_id,
                DIRECT_CODEX_LOG_INCREMENTAL_SCAN_MAX_ROWS,
            ],
            map_direct_codex_log_row,
        )
        .map_err(|error| format!("查询 Codex Desktop 增量日志失败: {}", error))?;

    let mut result = Vec::new();
    while let Some(row) = rows
        .next()
        .transpose()
        .map_err(|error| format!("解析 Codex Desktop 增量日志失败: {}", error))?
    {
        result.push(row);
    }

    let scanned_to_log_id = result
        .last()
        .map(|row| row.id)
        .unwrap_or(effective_last_log_id);
    let next_log_id = if result.len() < DIRECT_CODEX_LOG_INCREMENTAL_SCAN_MAX_ROWS as usize {
        max_log_id
    } else {
        scanned_to_log_id
    };

    let thread_ids = direct_codex_log_thread_ids_for_identity_backfill(&result);
    append_direct_codex_log_identity_rows(&connection, &mut result, thread_ids)?;
    Ok((result, next_log_id.max(last_log_id)))
}

fn is_better_direct_quota_observation(
    candidate: &DirectQuotaObservation,
    current: Option<&DirectQuotaObservation>,
) -> bool {
    let Some(current) = current else {
        return true;
    };
    if candidate.exhausted != current.exhausted {
        return candidate.exhausted;
    }
    if candidate.exhausted {
        return candidate.observed_at > current.observed_at;
    }

    let candidate_remaining = candidate
        .quota
        .as_ref()
        .and_then(quota_remaining_for_direct_observation);
    let current_remaining = current
        .quota
        .as_ref()
        .and_then(quota_remaining_for_direct_observation);

    match (candidate_remaining, current_remaining) {
        (Some(left), Some(right)) if left != right => left < right,
        (Some(_), None) => true,
        (None, Some(_)) => false,
        _ => candidate.observed_at > current.observed_at,
    }
}

fn apply_direct_quota_observation(
    account: &mut CodexAccount,
    observation: &DirectQuotaObservation,
) {
    let reset_at = observation.reset_at;
    if let Some(quota) = observation.quota.clone() {
        account.quota = Some(quota);
    } else {
        account.quota = Some(build_exhausted_quota_snapshot_with_hint(
            account,
            &observation.source,
            reset_at,
            observation.reset_after_seconds,
            observation.observed_at,
        ));
    }

    if observation.exhausted {
        let error_type = observation
            .error_type
            .clone()
            .unwrap_or_else(|| "usage_limit_reached".to_string());
        account.quota_error = Some(CodexQuotaErrorInfo {
            code: Some(error_type.clone()),
            message: format!(
                "Codex Direct OAuth upstream quota exhausted: status=429, error_type={}, reset_at={}",
                error_type,
                reset_at
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "-".to_string())
            ),
            timestamp: observation.observed_at,
        });
    } else if account
        .quota_error
        .as_ref()
        .map(|error| {
            error.timestamp <= observation.observed_at
                && (error
                    .code
                    .as_deref()
                    .map(is_quota_exhaustion_error)
                    .unwrap_or(false)
                    || is_quota_exhaustion_error(&error.message))
        })
        .unwrap_or(false)
    {
        account.quota_error = None;
    }
    account.usage_updated_at = Some(observation.observed_at);
}

fn quota_remaining_for_direct_observation(quota: &CodexQuota) -> Option<i32> {
    let mut quota = quota.clone();
    quota.normalize_window_slots();

    let mut percentages = Vec::new();
    if quota.hourly_window_present.unwrap_or(true) {
        percentages.push(quota.hourly_percentage.clamp(0, 100));
    }
    if quota.weekly_window_present.unwrap_or(true) {
        percentages.push(quota.weekly_percentage.clamp(0, 100));
    }
    percentages.into_iter().min()
}

fn should_apply_direct_quota_observation(
    account: &CodexAccount,
    observation: &DirectQuotaObservation,
) -> bool {
    if observation.exhausted {
        if observation
            .reset_at
            .map(|reset_at| reset_at <= chrono::Utc::now().timestamp())
            .unwrap_or(false)
        {
            return false;
        }
        return true;
    }

    let Some(observed_quota) = observation.quota.as_ref() else {
        return true;
    };
    let Some(observed_remaining) = quota_remaining_for_direct_observation(observed_quota) else {
        return false;
    };
    let Some(current_remaining) = account
        .quota
        .as_ref()
        .and_then(quota_remaining_for_direct_observation)
    else {
        return true;
    };

    observed_remaining < current_remaining
}

fn account_has_direct_quota_exhaustion_snapshot(account: &CodexAccount) -> bool {
    let Some(error) = account.quota_error.as_ref() else {
        return false;
    };
    let code_or_message = error.code.as_deref().unwrap_or(error.message.as_str());
    if !is_quota_exhaustion_error(code_or_message) && !is_quota_exhaustion_error(&error.message) {
        return false;
    }

    account
        .quota
        .as_ref()
        .and_then(quota_remaining_for_direct_observation)
        == Some(0)
}

fn repair_exhausted_account_reset_from_direct_observation(
    account: &mut CodexAccount,
    observation: &DirectQuotaObservation,
) -> bool {
    if observation.exhausted || !account_has_direct_quota_exhaustion_snapshot(account) {
        return false;
    }
    let Some(reset_at) = direct_observation_quota_reset_hint(observation) else {
        return false;
    };
    if reset_at <= chrono::Utc::now().timestamp() {
        return false;
    }

    let current_reset = account
        .quota
        .as_ref()
        .and_then(quota_reset_time_for_preservation);
    if current_reset
        .map(|current| current >= reset_at)
        .unwrap_or(false)
    {
        return false;
    }

    let Some(quota) = account.quota.as_mut() else {
        return false;
    };
    let mut changed = false;
    if quota.hourly_percentage <= 0 && quota.hourly_window_present.unwrap_or(false) {
        if quota.hourly_reset_time != Some(reset_at) {
            quota.hourly_reset_time = Some(reset_at);
            changed = true;
        }
    }
    if quota.weekly_percentage <= 0 && quota.weekly_window_present.unwrap_or(false) {
        if quota.weekly_reset_time != Some(reset_at) {
            quota.weekly_reset_time = Some(reset_at);
            changed = true;
        }
    }
    if !changed && quota.weekly_percentage <= 0 {
        quota.weekly_reset_time = Some(reset_at);
        quota.weekly_window_present = Some(true);
        changed = true;
    }
    if changed {
        if let Some(raw_data) = quota
            .raw_data
            .as_mut()
            .and_then(|value| value.as_object_mut())
        {
            raw_data.insert(
                "reset_at".to_string(),
                serde_json::Value::Number(reset_at.into()),
            );
            raw_data.insert(
                "reset_source".to_string(),
                serde_json::Value::String(observation.source.clone()),
            );
        }
        account.usage_updated_at = Some(observation.observed_at);
    }

    changed
}

pub(crate) fn apply_latest_direct_oauth_observation_if_current(
    account: &mut CodexAccount,
) -> Result<bool, String> {
    let Ok(runtime_mode) = crate::modules::codex_local_access::load_runtime_mode_state() else {
        return Ok(false);
    };
    if runtime_mode.mode
        != crate::models::codex_local_access::CodexRuntimeIntegrationMode::DirectProjection
        || runtime_mode.current_account_id.as_deref() != Some(account.id.as_str())
    {
        return Ok(false);
    }

    let since_seconds = account
        .last_used
        .max(runtime_mode.updated_at.div_euclid(1000));
    let Some(observation) = latest_direct_quota_observation_from_sessions(
        &codex_account::get_codex_home(),
        since_seconds,
    ) else {
        return Ok(false);
    };
    if !observation.exhausted {
        return Ok(false);
    }

    if should_keep_existing_quota_exhaustion(account, observation.observed_at)
        && account
            .quota_error
            .as_ref()
            .map(|error| error.timestamp >= observation.observed_at)
            .unwrap_or(false)
    {
        return Ok(false);
    }
    if !should_apply_direct_quota_observation(account, &observation) {
        return Ok(false);
    }

    apply_direct_quota_observation(account, &observation);
    codex_account::save_account(account)?;
    if observation.exhausted {
        logger::log_warn(&format!(
            "Codex Direct OAuth 配额耗尽已从官方 session 记录同步: account_id={}, source={}, reset_at={:?}",
            account.id, observation.source, observation.reset_at
        ));
    } else {
        logger::log_info(&format!(
            "Codex Direct OAuth 配额快照已从官方 session 记录同步: account_id={}, source={}, remaining={:?}, reset_at={:?}",
            account.id,
            observation.source,
            account
                .quota
                .as_ref()
                .and_then(quota_remaining_for_direct_observation),
            observation.reset_at
        ));
    }
    Ok(true)
}

pub(crate) fn repair_direct_oauth_observations_for_accounts_incremental(
    accounts: &mut [CodexAccount],
    state_path: &Path,
) -> usize {
    let codex_home = codex_account::get_codex_home();
    let mut state = load_direct_codex_log_sync_state(state_path);
    let (rows, next_log_id) = match read_incremental_codex_log_rows(&codex_home, state.last_log_id)
    {
        Ok(value) => value,
        Err(error) => {
            logger::log_warn(&format!(
                "[Codex Direct OAuth][QuotaRepair] 读取官方 Codex Desktop 增量日志失败: {}",
                error
            ));
            return 0;
        }
    };

    let observations = latest_direct_quota_observations_from_codex_log_rows(accounts, rows);
    let repaired = repair_direct_oauth_observations_from_map(accounts, &observations);
    if next_log_id > state.last_log_id {
        state.last_log_id = next_log_id;
        state.last_synced_at_ms = chrono::Utc::now().timestamp_millis();
        if let Err(error) = save_direct_codex_log_sync_state(state_path, &state) {
            logger::log_warn(&format!(
                "[Codex Direct OAuth][QuotaRepair] 写入官方日志增量游标失败: {}",
                error
            ));
        }
    }
    repaired
}

fn repair_direct_oauth_observations_from_map(
    accounts: &mut [CodexAccount],
    observations: &HashMap<String, DirectQuotaObservation>,
) -> usize {
    let mut repaired = 0usize;
    for account in accounts.iter_mut() {
        if account.is_api_key_auth() {
            continue;
        }
        let Some(observation) = observations.get(&account.id) else {
            continue;
        };
        if repair_exhausted_account_reset_from_direct_observation(account, observation) {
            match codex_account::save_account(account) {
                Ok(()) => {
                    repaired += 1;
                    logger::log_warn(&format!(
                        "[Codex Direct OAuth][QuotaRepair] 已根据官方 Codex Desktop rate_limits 日志修复耗尽账号 reset 时间: account_id={}, source={}, reset_at={:?}",
                        account.id, observation.source, direct_observation_quota_reset_hint(observation)
                    ));
                }
                Err(error) => logger::log_warn(&format!(
                    "[Codex Direct OAuth][QuotaRepair] 写回 quota reset 时间失败: account_id={}, error={}",
                    account.id, error
                )),
            }
            continue;
        }
        if should_keep_existing_quota_exhaustion(account, observation.observed_at)
            && account
                .quota_error
                .as_ref()
                .map(|error| error.timestamp >= observation.observed_at)
                .unwrap_or(false)
        {
            continue;
        }
        if !should_apply_direct_quota_observation(account, observation) {
            continue;
        }

        apply_direct_quota_observation(account, observation);
        match codex_account::save_account(account) {
            Ok(()) => {
                repaired += 1;
                logger::log_warn(&format!(
                    "[Codex Direct OAuth][QuotaRepair] 已根据官方 Codex Desktop websocket 日志修复 quota 缓存: account_id={}, source={}, exhausted={}, reset_at={:?}",
                    account.id, observation.source, observation.exhausted, observation.reset_at
                ));
            }
            Err(error) => logger::log_warn(&format!(
                "[Codex Direct OAuth][QuotaRepair] 写回 quota 缓存失败: account_id={}, error={}",
                account.id, error
            )),
        }
    }

    repaired
}

fn is_new_api_account(account: &CodexAccount) -> bool {
    account
        .api_provider_id
        .as_deref()
        .map(|value| {
            let value = value.trim();
            value.eq_ignore_ascii_case(COCKPIT_API_PROVIDER_ID)
                || value.eq_ignore_ascii_case(LEGACY_NEW_API_PROVIDER_ID)
        })
        .unwrap_or(false)
        || is_cockpit_api_base_url(account.api_base_url.as_deref())
        || account
            .plan_type
            .as_deref()
            .map(|value| {
                let value = value.trim();
                value.eq_ignore_ascii_case(COCKPIT_API_PLAN_TYPE)
                    || value.eq_ignore_ascii_case(LEGACY_NEW_API_EXCLUSIVE_PLAN_TYPE)
            })
            .unwrap_or(false)
}

fn normalize_api_base_url_for_match(raw: Option<&str>) -> Option<String> {
    let parsed = reqwest::Url::parse(raw?.trim()).ok()?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return None;
    }
    let host = parsed.host_str()?;
    let port = parsed
        .port()
        .map(|value| format!(":{}", value))
        .unwrap_or_default();
    let path = parsed.path().trim_end_matches('/');
    Some(format!("{}://{}{}{}", parsed.scheme(), host, port, path).to_ascii_lowercase())
}

fn is_cockpit_api_base_url(raw: Option<&str>) -> bool {
    let Some(actual) = normalize_api_base_url_for_match(raw) else {
        return false;
    };
    let Some(expected) = normalize_api_base_url_for_match(Some(COCKPIT_API_BASE_URL)) else {
        return false;
    };
    actual == expected
}

fn build_new_api_profile_url(account: &CodexAccount) -> Result<String, String> {
    let base_url = account
        .api_base_url
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or("Cockpit Api 账号缺少 Base URL")?;
    let mut parsed = reqwest::Url::parse(base_url)
        .map_err(|err| format!("Cockpit Api Base URL 无效: {}", err))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err("Cockpit Api Base URL 仅支持 http/https".to_string());
    }
    parsed.set_path("/api/cockpit-tools/token-profile");
    parsed.set_query(None);
    parsed.set_fragment(None);
    Ok(parsed.to_string())
}

fn read_i64(value: &serde_json::Value, key: &str) -> i64 {
    value
        .get(key)
        .and_then(|item| {
            item.as_i64()
                .or_else(|| item.as_u64().and_then(|raw| i64::try_from(raw).ok()))
        })
        .unwrap_or(0)
}

fn read_bool(value: &serde_json::Value, key: &str) -> bool {
    value
        .get(key)
        .and_then(|item| item.as_bool())
        .unwrap_or(false)
}

fn new_api_percentage(available: i64, total: i64, unlimited: bool) -> i32 {
    if unlimited {
        return 100;
    }
    if total <= 0 {
        return 0;
    }
    let percentage = (available.max(0) as f64 / total.max(1) as f64) * 100.0;
    percentage.round().clamp(0.0, 100.0) as i32
}

async fn fetch_new_api_quota(account: &CodexAccount) -> Result<FetchQuotaResult, String> {
    let api_key = account
        .openai_api_key
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or("Cockpit Api 账号缺少 OPENAI_API_KEY")?;
    let profile_url = build_new_api_profile_url(account)?;
    let client = reqwest::Client::new();
    let response = client
        .get(&profile_url)
        .bearer_auth(api_key)
        .header(ACCEPT, "application/json")
        .send()
        .await
        .map_err(|err| format!("请求 Cockpit Api 额度失败: {}", err))?;
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|err| format!("读取 Cockpit Api 额度响应失败: {}", err))?;
    if !status.is_success() {
        return Err(format!("Cockpit Api 额度接口返回 HTTP {}", status.as_u16()));
    }

    let root: serde_json::Value = serde_json::from_str(&body)
        .map_err(|err| format!("解析 Cockpit Api 额度 JSON 失败: {}", err))?;
    if root.get("success").and_then(|item| item.as_bool()) == Some(false) {
        let message = root
            .get("message")
            .and_then(|item| item.as_str())
            .unwrap_or("Cockpit Api 额度接口返回失败");
        return Err(message.to_string());
    }
    let data = root.get("data").unwrap_or(&root);
    let usage = data.get("usage").ok_or("Cockpit Api 额度响应缺少 usage")?;
    let total = read_i64(usage, "total_granted");
    let used = read_i64(usage, "total_used");
    let available = read_i64(usage, "total_available");
    let unlimited = read_bool(usage, "unlimited_quota");
    let percentage = new_api_percentage(available, total, unlimited);
    let expires_at = read_i64(usage, "expires_at");
    let reset_time = if expires_at > 0 {
        Some(expires_at)
    } else {
        None
    };

    Ok(FetchQuotaResult {
        quota: CodexQuota {
            hourly_percentage: percentage,
            hourly_reset_time: reset_time,
            hourly_window_minutes: None,
            hourly_window_present: Some(true),
            weekly_percentage: 0,
            weekly_reset_time: None,
            weekly_window_minutes: None,
            weekly_window_present: Some(false),
            raw_data: Some(json!({
                "provider": "cockpit-api",
                "object": "codex_cockpit_api_quota",
                "profile": data,
                "usage": usage,
                "total_granted": total,
                "total_used": used,
                "total_available": available,
                "unlimited_quota": unlimited
            })),
        },
        plan_type: Some(
            data.get("plan_type")
                .and_then(|item| item.as_str())
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| COCKPIT_API_PLAN_TYPE.to_string()),
        ),
    })
}

/// 从 id_token 中提取订阅标识并同步更新账号和索引
fn sync_subscription_from_token(
    account: &mut CodexAccount,
    plan_type: Option<String>,
    subscription_active_until: Option<String>,
) {
    let mut changed = false;
    if let Some(ref new_plan) = plan_type {
        let old_plan = account.plan_type.clone();
        if account.plan_type.as_deref() != Some(new_plan) {
            logger::log_info(&format!(
                "Codex 账号 {} 订阅标识已更新: {:?} -> {:?}",
                account.email, old_plan, plan_type
            ));
            account.plan_type = plan_type;
            changed = true;
        }
    }

    if let Some(ref next_expiry) = subscription_active_until {
        if account.subscription_active_until.as_deref() != Some(next_expiry) {
            account.subscription_active_until = Some(next_expiry.clone());
            changed = true;
        }
    }

    if changed {
        if let Err(e) = codex_account::update_account_plan_type_in_index(
            &account.id,
            &account.plan_type,
            &account.subscription_active_until,
        ) {
            logger::log_warn(&format!("更新索引 plan_type 失败: {}", e));
        }
    }
}

fn sync_subscription_expiry_from_current_id_token(account: &mut CodexAccount) {
    if let Ok((_, _, _, subscription_active_until, _, _)) =
        codex_account::extract_user_info(&account.tokens.id_token)
    {
        sync_subscription_from_token(account, None, subscription_active_until);
    }
}

/// 刷新账号配额并保存（包含 token 自动刷新）
async fn refresh_account_quota_once(account_id: &str) -> Result<CodexQuota, String> {
    let mut account = codex_account::prepare_account_for_injection(account_id).await?;
    if account.is_api_key_auth() {
        if is_new_api_account(&account) {
            let result = match fetch_new_api_quota(&account).await {
                Ok(result) => result,
                Err(e) => {
                    write_quota_fetch_error(&mut account, e.clone());
                    if let Err(save_err) = codex_account::save_account(&account) {
                        logger::log_warn(&format!("写入 Cockpit Api 配额错误失败: {}", save_err));
                    }
                    return Err(e);
                }
            };
            if result.plan_type.is_some() {
                sync_subscription_from_token(&mut account, result.plan_type, None);
            }
            account.quota = Some(result.quota.clone());
            account.quota_error = None;
            account.usage_updated_at = Some(chrono::Utc::now().timestamp());
            codex_account::save_account(&account)?;
            return Ok(result.quota);
        }
        account.quota = None;
        account.quota_error = None;
        account.usage_updated_at = None;
        let _ = codex_account::save_account(&account);
        return Err("API Key 账号不支持刷新配额，请在网页端查看。".to_string());
    }

    // 检查 token 是否过期，如果过期则刷新
    if crate::modules::codex_oauth::is_token_expired(&account.tokens.access_token) {
        match refresh_account_tokens(&mut account, "Token 已过期").await {
            Ok(()) => {
                logger::log_info(&format!("账号 {} 的 Token 刷新成功", account.email));

                sync_subscription_expiry_from_current_id_token(&mut account);

                codex_account::save_account(&account)?;
            }
            Err(e) => {
                logger::log_error(&format!("账号 {} Token 刷新失败: {}", account.email, e));
                let message = e;
                write_quota_error(&mut account, message.clone());
                if let Err(save_err) = codex_account::save_account(&account) {
                    logger::log_warn(&format!("写入 Codex 配额错误失败: {}", save_err));
                }
                return Err(message);
            }
        }
    }

    if apply_latest_direct_oauth_observation_if_current(&mut account)? {
        if let Some(quota) = account.quota.clone() {
            return Ok(quota);
        }
    }
    if should_keep_existing_quota_exhaustion(&account, chrono::Utc::now().timestamp()) {
        if let Some(quota) = account.quota.clone() {
            logger::log_info(&format!(
                "Codex 账号 {} 仍处于已记录的配额冷却期，跳过本次 wham/usage 刷新以避免覆盖 Direct OAuth 429 快照",
                account.email
            ));
            return Ok(quota);
        }
    }

    let result = match fetch_quota(&account).await {
        Ok(result) => result,
        Err(e) => {
            write_quota_fetch_error(&mut account, e.clone());
            if let Err(save_err) = codex_account::save_account(&account) {
                logger::log_warn(&format!("写入 Codex 配额错误失败: {}", save_err));
            }
            return Err(e);
        }
    };

    // 从 usage 响应中的 plan_type 更新订阅标识
    if result.plan_type.is_some() {
        sync_subscription_from_token(&mut account, result.plan_type, None);
    }

    account.quota = Some(result.quota.clone());
    account.quota_error = None;
    account.usage_updated_at = Some(chrono::Utc::now().timestamp());
    codex_account::save_account(&account)?;

    Ok(result.quota)
}

pub async fn refresh_account_quota(account_id: &str) -> Result<CodexQuota, String> {
    refresh_account_quota_once(account_id).await
}

/// 刷新所有账号配额
pub async fn refresh_all_quotas() -> Result<Vec<(String, Result<CodexQuota, String>)>, String> {
    use futures::future::join_all;
    use std::sync::Arc;
    use tokio::sync::Semaphore;

    const MAX_CONCURRENT: usize = 5;
    let accounts: Vec<_> = codex_account::list_accounts()
        .into_iter()
        .filter(|account| !account.is_api_key_auth() || is_new_api_account(account))
        .collect();

    let semaphore = Arc::new(Semaphore::new(MAX_CONCURRENT));
    let tasks: Vec<_> = accounts
        .into_iter()
        .map(|account| {
            let account_id = account.id;
            let semaphore = semaphore.clone();
            async move {
                let _permit = semaphore
                    .acquire_owned()
                    .await
                    .map_err(|e| format!("获取 Codex 刷新并发许可失败: {}", e))?;
                let result = refresh_account_quota(&account_id).await;
                Ok::<(String, Result<CodexQuota, String>), String>((account_id, result))
            }
        })
        .collect();

    let mut results = Vec::with_capacity(tasks.len());
    for task in join_all(tasks).await {
        match task {
            Ok(item) => results.push(item),
            Err(err) => return Err(err),
        }
    }

    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::codex::CodexTokens;

    fn test_account() -> CodexAccount {
        CodexAccount::new(
            "codex_test".to_string(),
            "user@example.com".to_string(),
            CodexTokens {
                id_token: String::new(),
                access_token: String::new(),
                refresh_token: None,
            },
        )
    }

    #[test]
    fn quota_fetch_error_sets_exhausted_quota_to_zero() {
        let mut account = test_account();
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

        write_quota_fetch_error(
            &mut account,
            "API 返回错误 429 [error_code:usage_limit_reached] [body_len:42]".to_string(),
        );

        let quota = account.quota.as_ref().expect("quota snapshot");
        assert_eq!(quota.hourly_percentage, 0);
        assert_eq!(quota.weekly_percentage, 0);
        assert_eq!(quota.hourly_reset_time, Some(111));
        assert_eq!(quota.weekly_reset_time, Some(222));
        assert_eq!(
            account
                .quota_error
                .as_ref()
                .and_then(|error| error.code.as_deref()),
            Some("usage_limit_reached")
        );
        assert!(account.usage_updated_at.is_some());
    }

    #[test]
    fn quota_fetch_error_uses_reset_hint_for_exhausted_snapshot() {
        let mut account = test_account();
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

        write_quota_fetch_error(
            &mut account,
            "API 返回错误 429 [error_code:usage_limit_reached] [reset_at:333] [reset_after_seconds:60] [body_len:42]".to_string(),
        );

        let quota = account.quota.as_ref().expect("quota snapshot");
        assert_eq!(quota.hourly_percentage, 0);
        assert_eq!(quota.weekly_percentage, 0);
        assert_eq!(quota.hourly_reset_time, Some(333));
        assert_eq!(quota.weekly_reset_time, Some(333));
        assert_eq!(
            quota
                .raw_data
                .as_ref()
                .and_then(|value| value.get("reset_at"))
                .and_then(|value| value.as_i64()),
            Some(333)
        );
    }

    #[test]
    fn quota_reset_hint_body_computes_reset_at_from_reset_after_seconds() {
        let hint = extract_quota_reset_hint_from_body(
            r#"{"error":{"code":"usage_limit_reached","resets_in_seconds":60}}"#,
            1_700_000_000,
        )
        .expect("reset hint should parse");

        assert_eq!(hint, (Some(1_700_000_060), Some(60)));
    }

    #[test]
    fn quota_reset_hint_body_accepts_string_retry_after_alias() {
        let hint = extract_quota_reset_hint_from_body(
            r#"{"error":{"code":"insufficient_quota","retry_after":"45"}}"#,
            1_700_000_000,
        )
        .expect("reset hint should parse string retry_after alias");

        assert_eq!(hint, (Some(1_700_000_045), Some(45)));
    }

    #[test]
    fn direct_codex_log_error_observation_maps_identity_and_zeroes_quota() {
        let mut account = test_account();
        account.id = "codex_direct_log_error".to_string();
        account.email = "direct@example.com".to_string();
        account.account_id = Some("acc-direct".to_string());
        account.last_used = 1_700_000_000;
        account.usage_updated_at = Some(1_700_000_000);
        account.quota = Some(CodexQuota {
            hourly_percentage: 100,
            hourly_reset_time: None,
            hourly_window_minutes: None,
            hourly_window_present: Some(false),
            weekly_percentage: 97,
            weekly_reset_time: Some(1_700_010_000),
            weekly_window_minutes: Some(WEEKLY_WINDOW_MINUTES_THRESHOLD),
            weekly_window_present: Some(true),
            raw_data: None,
        });
        let rows = vec![
            DirectCodexLogRow {
                id: 1,
                ts: 1_700_000_010,
                thread_id: Some("thread-a".to_string()),
                body: r#"codex_otel.log_only user.account_id="acc-direct" user.email="direct@example.com""#
                    .to_string(),
            },
            DirectCodexLogRow {
                id: 2,
                ts: 1_700_000_020,
                thread_id: Some("thread-a".to_string()),
                body: r#"websocket.stream_request: websocket event: {"type":"response.failed","response":{"error":{"type":"usage_limit_reached","reset_after_seconds":120}}}"#
                    .to_string(),
            },
        ];

        let observations =
            latest_direct_quota_observations_from_codex_log_rows(&[account.clone()], rows);
        let observation = observations
            .get("codex_direct_log_error")
            .expect("direct websocket error should map to account");

        assert!(observation.exhausted);
        assert_eq!(observation.source, "codex_official_websocket_error");
        assert_eq!(observation.reset_at, Some(1_700_000_140));
        apply_direct_quota_observation(&mut account, observation);
        let quota = account.quota.as_ref().expect("quota should be repaired");
        assert_eq!(quota.hourly_percentage, 0);
        assert_eq!(quota.weekly_percentage, 0);
        assert_eq!(quota.hourly_reset_time, Some(1_700_000_140));
        assert_eq!(quota.weekly_reset_time, Some(1_700_000_140));
        assert_eq!(
            account
                .quota_error
                .as_ref()
                .and_then(|error| error.code.as_deref()),
            Some("usage_limit_reached")
        );
    }

    #[test]
    fn direct_codex_log_rate_limit_observation_maps_identity_and_zeroes_weekly_quota() {
        let mut account = test_account();
        account.id = "codex_direct_log_rate_limits".to_string();
        account.email = "direct@example.com".to_string();
        account.account_id = Some("acc-direct".to_string());
        account.last_used = 1_700_000_000;
        account.usage_updated_at = Some(1_700_000_000);
        account.quota = Some(CodexQuota {
            hourly_percentage: 100,
            hourly_reset_time: None,
            hourly_window_minutes: None,
            hourly_window_present: Some(false),
            weekly_percentage: 97,
            weekly_reset_time: Some(1_700_010_000),
            weekly_window_minutes: Some(WEEKLY_WINDOW_MINUTES_THRESHOLD),
            weekly_window_present: Some(true),
            raw_data: None,
        });
        let rows = vec![
            DirectCodexLogRow {
                id: 1,
                ts: 1_700_000_010,
                thread_id: Some("thread-a".to_string()),
                body: r#"codex_otel.log_only user.account_id="acc-direct" user.email="direct@example.com""#
                    .to_string(),
            },
            DirectCodexLogRow {
                id: 2,
                ts: 1_700_000_020,
                thread_id: Some("thread-a".to_string()),
                body: r#"websocket.stream_request: websocket event: {"type":"codex.rate_limits","plan_type":"free","rate_limits":{"allowed":false,"limit_reached":true,"primary":{"used_percent":3,"window_minutes":10080,"reset_after_seconds":120,"reset_at":1700000140},"secondary":null}}"#
                    .to_string(),
            },
        ];

        let observations =
            latest_direct_quota_observations_from_codex_log_rows(&[account.clone()], rows);
        let observation = observations
            .get("codex_direct_log_rate_limits")
            .expect("direct websocket rate_limits should map to account");

        assert!(observation.exhausted);
        assert_eq!(observation.source, "codex_official_websocket_rate_limits");
        assert_eq!(observation.reset_at, Some(1_700_000_140));
        apply_direct_quota_observation(&mut account, observation);
        let quota = account.quota.as_ref().expect("quota should be repaired");
        assert_eq!(quota.weekly_percentage, 0);
        assert_eq!(quota.weekly_reset_time, Some(1_700_000_140));
        assert_eq!(quota.weekly_window_present, Some(true));
        assert_eq!(
            quota
                .raw_data
                .as_ref()
                .and_then(|value| value.get("source"))
                .and_then(|value| value.as_str()),
            Some("codex_official_websocket_rate_limits")
        );
        assert_eq!(
            account
                .quota_error
                .as_ref()
                .and_then(|error| error.code.as_deref()),
            Some("usage_limit_reached")
        );
    }

    #[test]
    fn direct_codex_log_sqlite_scan_keeps_quota_events_after_noisy_websocket_delta() {
        let root = std::env::temp_dir().join(format!(
            "cockpit-direct-codex-log-sqlite-noise-test-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("codex home fixture should be created");
        let db_path = root.join("logs_2.sqlite");
        let connection = Connection::open(&db_path).expect("sqlite fixture should open");
        connection
            .execute_batch(
                r#"
                CREATE TABLE logs (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    ts INTEGER NOT NULL,
                    ts_nanos INTEGER NOT NULL,
                    level TEXT NOT NULL,
                    target TEXT NOT NULL,
                    feedback_log_body TEXT,
                    thread_id TEXT,
                    estimated_bytes INTEGER NOT NULL DEFAULT 0
                );
                "#,
            )
            .expect("logs table should be created");
        connection
            .execute(
                "INSERT INTO logs (ts, ts_nanos, level, target, feedback_log_body, thread_id) VALUES (?1, 0, 'INFO', 'codex_otel.log_only', ?2, 'thread-noisy')",
                (
                    1_700_000_010i64,
                    r#"codex_otel.log_only user.account_id="acc-direct" user.email="direct@example.com""#,
                ),
            )
            .expect("identity row should be inserted");
        connection
            .execute(
                "INSERT INTO logs (ts, ts_nanos, level, target, feedback_log_body, thread_id) VALUES (?1, 0, 'INFO', 'codex_api::endpoint::responses_websocket', ?2, 'thread-noisy')",
                (
                    1_700_000_020i64,
                    r#"websocket.stream_request: websocket event: {"type":"codex.rate_limits","plan_type":"free","rate_limits":{"allowed":false,"limit_reached":true,"primary":{"used_percent":3,"window_minutes":10080,"reset_after_seconds":120,"reset_at":1700000140},"secondary":null}}"#,
                ),
            )
            .expect("quota row should be inserted");
        let noise_body = r#"websocket.stream_request: websocket event: {"type":"response.output_text.delta","delta":"noise"}"#;
        let transaction = connection
            .unchecked_transaction()
            .expect("noise transaction should start");
        {
            let mut statement = transaction
                .prepare(
                    "INSERT INTO logs (ts, ts_nanos, level, target, feedback_log_body, thread_id) VALUES (?1, 0, 'INFO', 'codex_api::endpoint::responses_websocket', ?2, 'thread-other')",
                )
                .expect("noise insert should prepare");
            for offset in 0..=DIRECT_CODEX_LOG_SCAN_MAX_ROWS {
                statement
                    .execute((1_700_000_100i64 + offset, noise_body))
                    .expect("noise row should be inserted");
            }
        }
        transaction
            .commit()
            .expect("noise transaction should commit");

        let mut account = test_account();
        account.id = "codex_direct_log_sqlite_noise".to_string();
        account.email = "direct@example.com".to_string();
        account.account_id = Some("acc-direct".to_string());
        account.last_used = 1_700_000_000;
        account.usage_updated_at = Some(1_700_000_000);

        let rows = read_recent_codex_log_rows(&root).expect("sqlite logs should be read");
        let observations = latest_direct_quota_observations_from_codex_log_rows(&[account], rows);

        let _ = fs::remove_dir_all(&root);
        let observation = observations
            .get("codex_direct_log_sqlite_noise")
            .expect("quota event must survive noisy websocket delta rows");
        assert!(observation.exhausted);
    }

    #[test]
    fn direct_codex_log_error_uses_rate_limit_reset_over_short_retry_backoff() {
        let mut account = test_account();
        account.id = "codex_direct_log_error_with_window_reset".to_string();
        account.email = "direct@example.com".to_string();
        account.account_id = Some("acc-direct".to_string());
        account.last_used = 1_700_000_000;
        let rows = vec![
            DirectCodexLogRow {
                id: 1,
                ts: 1_700_000_010,
                thread_id: Some("thread-a".to_string()),
                body: r#"codex_otel.log_only user.account_id="acc-direct" user.email="direct@example.com""#
                    .to_string(),
            },
            DirectCodexLogRow {
                id: 2,
                ts: 1_700_000_020,
                thread_id: Some("thread-a".to_string()),
                body: r#"websocket.stream_request: websocket event: {"type":"codex.rate_limits","plan_type":"free","rate_limits":{"allowed":true,"limit_reached":false,"primary":{"used_percent":88,"window_minutes":10080,"reset_after_seconds":604800,"reset_at":1700604820},"secondary":null}}"#
                    .to_string(),
            },
            DirectCodexLogRow {
                id: 3,
                ts: 1_700_000_030,
                thread_id: Some("thread-a".to_string()),
                body: r#"websocket.stream_request: websocket event: {"type":"response.failed","response":{"error":{"type":"usage_limit_reached","reset_after_seconds":120}}}"#
                    .to_string(),
            },
        ];

        let observations = latest_direct_quota_observations_from_codex_log_rows(&[account], rows);
        let observation = observations
            .get("codex_direct_log_error_with_window_reset")
            .expect("direct websocket error should map to account");

        assert!(observation.exhausted);
        assert_eq!(observation.source, "codex_official_websocket_error");
        assert_eq!(
            observation.reset_at,
            Some(1_700_604_820),
            "short websocket error retry/backoff must not replace the quota window reset"
        );
        assert_eq!(
            observation.reset_after_seconds,
            Some(604_790),
            "reset_after_seconds should be recomputed from the selected quota window reset"
        );
    }

    #[test]
    fn direct_codex_log_parser_ignores_request_body_usage_limit_text_without_websocket_event() {
        let mut account = test_account();
        account.id = "codex_direct_log_false_positive".to_string();
        account.email = "direct@example.com".to_string();
        account.account_id = Some("acc-direct".to_string());
        account.last_used = 1_700_000_000;
        let rows = vec![
            DirectCodexLogRow {
                id: 1,
                ts: 1_700_000_010,
                thread_id: Some("thread-a".to_string()),
                body: r#"codex_otel.log_only user.account_id="acc-direct" user.email="direct@example.com""#
                    .to_string(),
            },
            DirectCodexLogRow {
                id: 2,
                ts: 1_700_000_020,
                thread_id: Some("thread-a".to_string()),
                body: r#"request_body={"prompt":"请检查 usage_limit_reached 是否出现"}"#.to_string(),
            },
            DirectCodexLogRow {
                id: 3,
                ts: 1_700_000_030,
                thread_id: Some("thread-a".to_string()),
                body: r#"websocket.stream_request: websocket event: {"type":"response.output_text.delta","delta":"usage_limit_reached"}"#
                    .to_string(),
            },
        ];

        let observations = latest_direct_quota_observations_from_codex_log_rows(&[account], rows);

        assert!(
            observations.is_empty(),
            "non-error websocket deltas and request bodies must not zero quota"
        );
    }

    #[test]
    fn direct_codex_log_parser_ignores_ambiguous_email_identity() {
        let mut first = test_account();
        first.id = "codex_direct_log_first".to_string();
        first.email = "shared@example.com".to_string();
        first.last_used = 1_700_000_000;
        let mut second = test_account();
        second.id = "codex_direct_log_second".to_string();
        second.email = "shared@example.com".to_string();
        second.last_used = 1_700_000_000;
        let rows = vec![
            DirectCodexLogRow {
                id: 1,
                ts: 1_700_000_010,
                thread_id: Some("thread-a".to_string()),
                body: r#"codex_otel.log_only user.email="shared@example.com""#.to_string(),
            },
            DirectCodexLogRow {
                id: 2,
                ts: 1_700_000_020,
                thread_id: Some("thread-a".to_string()),
                body: r#"websocket.stream_request: websocket event: {"type":"response.failed","response":{"error":{"type":"usage_limit_reached","reset_after_seconds":120}}}"#
                    .to_string(),
            },
        ];

        let observations =
            latest_direct_quota_observations_from_codex_log_rows(&[first, second], rows);

        assert!(
            observations.is_empty(),
            "ambiguous email-only identity must not zero an arbitrary account"
        );
    }

    #[test]
    fn quota_usage_reset_at_normalizes_millisecond_timestamp() {
        let usage = UsageResponse {
            plan_type: Some("pro".to_string()),
            rate_limit: Some(RateLimitInfo {
                allowed: Some(true),
                limit_reached: Some(false),
                primary_window: Some(WindowInfo {
                    used_percent: Some(40),
                    limit_window_seconds: Some(18_000),
                    reset_after_seconds: None,
                    reset_at: Some(1_700_000_360_000),
                }),
                secondary_window: None,
            }),
            code_review_rate_limit: None,
            rate_limit_reached_type: None,
        };

        let quota = parse_quota_from_usage(&usage, "{}").expect("quota should parse");

        assert_eq!(quota.hourly_percentage, 60);
        assert_eq!(quota.hourly_reset_time, Some(1_700_000_360));
    }

    #[test]
    fn quota_usage_primary_weekly_window_maps_to_weekly_quota() {
        let usage = UsageResponse {
            plan_type: Some("free".to_string()),
            rate_limit: Some(RateLimitInfo {
                allowed: Some(false),
                limit_reached: Some(true),
                primary_window: Some(WindowInfo {
                    used_percent: Some(100),
                    limit_window_seconds: Some(604_800),
                    reset_after_seconds: None,
                    reset_at: Some(1_700_604_800),
                }),
                secondary_window: None,
            }),
            code_review_rate_limit: None,
            rate_limit_reached_type: None,
        };

        let quota = parse_quota_from_usage(&usage, "{}").expect("quota should parse");

        assert_eq!(quota.hourly_percentage, 100);
        assert_eq!(quota.hourly_window_present, Some(false));
        assert_eq!(quota.weekly_percentage, 0);
        assert_eq!(quota.weekly_reset_time, Some(1_700_604_800));
        assert_eq!(quota.weekly_window_present, Some(true));
    }

    #[test]
    fn persisted_weekly_window_in_hourly_slot_normalizes_to_weekly() {
        let mut quota = CodexQuota {
            hourly_percentage: 97,
            hourly_reset_time: Some(1_780_310_638),
            hourly_window_minutes: Some(10_080),
            hourly_window_present: Some(true),
            weekly_percentage: 100,
            weekly_reset_time: None,
            weekly_window_minutes: None,
            weekly_window_present: Some(false),
            raw_data: None,
        };

        assert!(quota.normalize_window_slots());
        assert_eq!(quota.hourly_percentage, 100);
        assert_eq!(quota.hourly_reset_time, None);
        assert_eq!(quota.hourly_window_present, Some(false));
        assert_eq!(quota.weekly_percentage, 97);
        assert_eq!(quota.weekly_reset_time, Some(1_780_310_638));
        assert_eq!(quota.weekly_window_minutes, Some(10_080));
        assert_eq!(quota.weekly_window_present, Some(true));
    }

    #[test]
    fn quota_usage_reached_type_zeroes_lagging_usage_payload() {
        let usage: UsageResponse = serde_json::from_str(
            r#"{
              "plan_type":"free",
              "rate_limit":{
                "allowed":true,
                "limit_reached":false,
                "primary_window":{
                  "used_percent":3,
                  "limit_window_seconds":604800,
                  "reset_at":1700604800
                }
              },
              "rate_limit_reached_type":{"type":"workspace_member_usage_limit_reached"}
            }"#,
        )
        .expect("usage payload should parse");

        let quota = parse_quota_from_usage(&usage, "{}").expect("quota should parse");

        assert_eq!(quota.weekly_percentage, 0);
        assert_eq!(quota.weekly_reset_time, Some(1_700_604_800));
    }

    #[test]
    fn active_direct_quota_exhaustion_snapshot_blocks_stale_success_overwrite() {
        let mut account = test_account();
        account.quota_error = Some(CodexQuotaErrorInfo {
            code: Some("usage_limit_reached".to_string()),
            message: "Codex Direct OAuth upstream quota exhausted: status=429, error_type=usage_limit_reached, reset_at=1700003600".to_string(),
            timestamp: 1_700_000_000,
        });
        account.quota = Some(CodexQuota {
            hourly_percentage: 0,
            hourly_reset_time: Some(1_700_003_600),
            hourly_window_minutes: Some(300),
            hourly_window_present: Some(true),
            weekly_percentage: 0,
            weekly_reset_time: Some(1_700_003_600),
            weekly_window_minutes: Some(10080),
            weekly_window_present: Some(true),
            raw_data: Some(json!({
                "source": "codex_direct_oauth_upstream_error",
                "quota_exhausted": true,
                "reset_at": 1_700_003_600
            })),
        });

        assert!(should_keep_existing_quota_exhaustion(
            &account,
            1_700_000_100
        ));
    }

    #[test]
    fn direct_session_parser_ignores_non_token_count_rate_limit_payloads() {
        let line = serde_json::json!({
            "timestamp": "2026-05-25T11:42:31Z",
            "type": "event_msg",
            "payload": {
                "type": "function_call_output",
                "rate_limits": {
                    "primary": {
                        "used_percent": 100,
                        "window_minutes": 10080,
                        "resets_at": 1780314131
                    },
                    "rate_limit_reached_type": "usage_limit_reached"
                }
            }
        })
        .to_string();

        assert!(parse_direct_quota_observation_from_session_line(&line).is_none());
    }

    #[test]
    fn direct_session_parser_ignores_non_error_usage_limit_payloads() {
        let line = serde_json::json!({
            "timestamp": "2026-05-25T11:42:31Z",
            "type": "event_msg",
            "payload": {
                "type": "function_call_output",
                "error": {
                    "type": "usage_limit_reached",
                    "reset_after_seconds": 120
                }
            }
        })
        .to_string();

        assert!(parse_direct_quota_observation_from_session_line(&line).is_none());
    }

    #[test]
    fn direct_session_rate_limit_event_builds_quota_observation() {
        let line = serde_json::json!({
            "timestamp": "2026-05-25T11:42:31.809Z",
            "type": "event_msg",
            "payload": {
                "type": "token_count",
                "rate_limits": {
                    "limit_id": "codex",
                    "primary": {
                        "used_percent": 100.0,
                        "window_minutes": 10080,
                        "resets_at": 1780314131
                    },
                    "secondary": null,
                    "plan_type": "free",
                    "rate_limit_reached_type": "workspace_member_usage_limit_reached"
                }
            }
        })
        .to_string();

        let observation =
            parse_direct_quota_observation_from_session_line(&line).expect("observation");

        assert_eq!(observation.reset_at, Some(1_780_314_131));
        assert_eq!(
            observation.error_type.as_deref(),
            Some("workspace_member_usage_limit_reached")
        );
        assert_eq!(observation.source, "codex_session_rate_limits");
        assert!(observation.exhausted);
        assert_eq!(
            observation
                .quota
                .as_ref()
                .map(|quota| quota.weekly_percentage),
            Some(0)
        );
    }

    #[test]
    fn direct_session_rate_limit_event_computes_relative_reset_hint() {
        let line = serde_json::json!({
            "timestamp": "2026-05-25T11:42:31Z",
            "type": "event_msg",
            "payload": {
                "type": "token_count",
                "rate_limits": {
                    "primary": {
                        "used_percent": "100",
                        "window_minutes": 300,
                        "reset_after_seconds": "60"
                    }
                }
            }
        })
        .to_string();

        let observation =
            parse_direct_quota_observation_from_session_line(&line).expect("observation");

        assert_eq!(observation.reset_after_seconds, Some(60));
        assert_eq!(observation.reset_at, Some(observation.observed_at + 60));
    }

    #[test]
    fn direct_session_rate_limit_event_updates_partial_remaining_quota() {
        let line = serde_json::json!({
            "timestamp": "2026-05-25T12:41:39Z",
            "type": "event_msg",
            "payload": {
                "type": "token_count",
                "rate_limits": {
                    "primary": {
                        "used_percent": 56,
                        "window_minutes": 10080,
                        "resets_at": 1780317510
                    },
                    "secondary": null,
                    "plan_type": "free",
                    "rate_limit_reached_type": null
                }
            }
        })
        .to_string();

        let observation =
            parse_direct_quota_observation_from_session_line(&line).expect("observation");
        let quota = observation.quota.as_ref().expect("quota snapshot");

        assert!(!observation.exhausted);
        assert_eq!(observation.error_type, None);
        assert_eq!(quota.hourly_window_present, Some(false));
        assert_eq!(quota.weekly_percentage, 44);
        assert_eq!(quota.weekly_reset_time, Some(1_780_317_510));
        assert_eq!(quota.weekly_window_minutes, Some(10_080));
    }

    #[test]
    fn direct_session_partial_quota_only_applies_when_more_constrained() {
        let mut account = test_account();
        account.quota = Some(CodexQuota {
            hourly_percentage: 100,
            hourly_reset_time: None,
            hourly_window_minutes: None,
            hourly_window_present: Some(false),
            weekly_percentage: 97,
            weekly_reset_time: Some(1_780_317_510),
            weekly_window_minutes: Some(10_080),
            weekly_window_present: Some(true),
            raw_data: None,
        });
        let observation = DirectQuotaObservation {
            observed_at: 1_779_712_100,
            reset_at: Some(1_780_317_510),
            reset_after_seconds: Some(60),
            error_type: None,
            source: "codex_session_rate_limits".to_string(),
            exhausted: false,
            quota: Some(CodexQuota {
                hourly_percentage: 100,
                hourly_reset_time: None,
                hourly_window_minutes: None,
                hourly_window_present: Some(false),
                weekly_percentage: 44,
                weekly_reset_time: Some(1_780_317_510),
                weekly_window_minutes: Some(10_080),
                weekly_window_present: Some(true),
                raw_data: None,
            }),
        };

        assert!(should_apply_direct_quota_observation(
            &account,
            &observation
        ));

        account.quota.as_mut().unwrap().weekly_percentage = 20;
        assert!(!should_apply_direct_quota_observation(
            &account,
            &observation
        ));

        let expired_exhaustion = DirectQuotaObservation {
            observed_at: 1,
            reset_at: Some(1),
            reset_after_seconds: Some(0),
            error_type: Some("usage_limit_reached".to_string()),
            source: "codex_session_error".to_string(),
            exhausted: true,
            quota: None,
        };
        assert!(!should_apply_direct_quota_observation(
            &account,
            &expired_exhaustion
        ));
    }

    #[test]
    fn direct_session_observation_selection_prefers_more_constrained_snapshot() {
        let older_low = DirectQuotaObservation {
            observed_at: 20,
            reset_at: Some(100),
            reset_after_seconds: Some(80),
            error_type: None,
            source: "codex_session_rate_limits".to_string(),
            exhausted: false,
            quota: Some(CodexQuota {
                hourly_percentage: 100,
                hourly_reset_time: None,
                hourly_window_minutes: None,
                hourly_window_present: Some(false),
                weekly_percentage: 44,
                weekly_reset_time: Some(100),
                weekly_window_minutes: Some(10_080),
                weekly_window_present: Some(true),
                raw_data: None,
            }),
        };
        let newer_high = DirectQuotaObservation {
            observed_at: 30,
            reset_at: Some(110),
            reset_after_seconds: Some(80),
            error_type: None,
            source: "codex_session_rate_limits".to_string(),
            exhausted: false,
            quota: Some(CodexQuota {
                hourly_percentage: 100,
                hourly_reset_time: None,
                hourly_window_minutes: None,
                hourly_window_present: Some(false),
                weekly_percentage: 97,
                weekly_reset_time: Some(110),
                weekly_window_minutes: Some(10_080),
                weekly_window_present: Some(true),
                raw_data: None,
            }),
        };

        assert!(is_better_direct_quota_observation(
            &older_low,
            Some(&newer_high)
        ));
        assert!(!is_better_direct_quota_observation(
            &newer_high,
            Some(&older_low)
        ));
    }

    #[test]
    fn direct_session_scan_ignores_large_file_prefix_outside_tail_window() {
        let root = std::env::temp_dir().join(format!(
            "cockpit-direct-session-tail-window-test-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        let sessions_dir = root.join("sessions").join("2026").join("05").join("26");
        fs::create_dir_all(&sessions_dir).expect("sessions dir should be created");
        let session_path = sessions_dir.join("rollover.jsonl");
        let stale_prefix_observation = serde_json::json!({
            "timestamp": "2026-05-25T11:42:31Z",
            "type": "event_msg",
            "payload": {
                "type": "error",
                "status": 429,
                "error": {
                    "type": "usage_limit_reached",
                    "reset_after_seconds": 120
                }
            }
        })
        .to_string();
        let filler = " ".repeat((256 * 1024) + 128);
        fs::write(
            &session_path,
            format!("{}\n{}", stale_prefix_observation, filler),
        )
        .expect("large session fixture should be written");

        let observation = latest_direct_quota_observation_from_sessions(&root, 0);

        let _ = fs::remove_dir_all(&root);
        assert!(
            observation.is_none(),
            "startup repair must not scan large session file prefixes"
        );
    }

    #[test]
    fn direct_session_error_event_parses_reset_hint() {
        let line = serde_json::json!({
            "timestamp": "2026-05-25T11:42:31Z",
            "type": "event_msg",
            "payload": {
                "type": "error",
                "status": 429,
                "error": {
                    "type": "usage_limit_reached",
                    "message": "The usage limit has been reached",
                    "resets_in_seconds": "120"
                }
            }
        })
        .to_string();

        let observation =
            parse_direct_quota_observation_from_session_line(&line).expect("observation");

        assert_eq!(observation.reset_after_seconds, Some(120));
        assert_eq!(observation.reset_at, Some(observation.observed_at + 120));
        assert_eq!(
            observation.error_type.as_deref(),
            Some("usage_limit_reached")
        );
    }

    #[test]
    fn direct_session_error_observation_applies_zero_quota_snapshot_with_reset() {
        let line = serde_json::json!({
            "timestamp": "2026-05-25T11:42:31Z",
            "type": "event_msg",
            "payload": {
                "type": "error",
                "status": 429,
                "error": {
                    "type": "usage_limit_reached",
                    "message": "The usage limit has been reached",
                    "reset_after_seconds": 120
                }
            }
        })
        .to_string();
        let observation =
            parse_direct_quota_observation_from_session_line(&line).expect("observation");
        let mut account = test_account();
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

        apply_direct_quota_observation(&mut account, &observation);

        let quota = account.quota.as_ref().expect("quota snapshot");
        assert_eq!(quota.hourly_percentage, 0);
        assert_eq!(quota.weekly_percentage, 0);
        assert_eq!(quota.hourly_reset_time, observation.reset_at);
        assert_eq!(quota.weekly_reset_time, observation.reset_at);
        assert_eq!(
            quota
                .raw_data
                .as_ref()
                .and_then(|value| value.get("reset_after_seconds"))
                .and_then(|value| value.as_i64()),
            Some(120)
        );
        assert_eq!(account.usage_updated_at, Some(observation.observed_at));
        assert_eq!(
            account
                .quota_error
                .as_ref()
                .and_then(|error| error.code.as_deref()),
            Some("usage_limit_reached")
        );
    }

    #[test]
    fn direct_session_error_event_parses_insufficient_quota_without_rate_limits() {
        let line = serde_json::json!({
            "timestamp": "2026-05-25T11:42:31Z",
            "type": "event_msg",
            "payload": {
                "type": "error",
                "status": 429,
                "error": {
                    "type": "insufficient_quota",
                    "message": "Quota exceeded",
                    "reset_after_seconds": 45
                }
            }
        })
        .to_string();

        let observation =
            parse_direct_quota_observation_from_session_line(&line).expect("observation");

        assert_eq!(observation.reset_after_seconds, Some(45));
        assert_eq!(
            observation.error_type.as_deref(),
            Some("insufficient_quota")
        );
    }

    #[test]
    fn quota_fetch_error_does_not_zero_generic_rate_limit() {
        let mut account = test_account();
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

        write_quota_fetch_error(
            &mut account,
            "API 返回错误 429 [error_code:rate_limit_exceeded] [reset_after_seconds:60] [body_len:42]".to_string(),
        );

        let quota = account.quota.as_ref().expect("quota should stay unchanged");
        assert_eq!(quota.hourly_percentage, 64);
        assert_eq!(quota.weekly_percentage, 27);
        assert_eq!(
            account
                .quota_error
                .as_ref()
                .and_then(|error| error.code.as_deref()),
            Some("rate_limit_exceeded")
        );
        assert!(account.usage_updated_at.is_none());
    }

    #[test]
    fn token_refresh_error_is_not_quota_exhaustion() {
        assert!(!is_quota_exhaustion_error("Token 已过期且刷新失败"));
    }
}
