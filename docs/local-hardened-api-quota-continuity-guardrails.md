# Local Hardened API Quota Continuity Guardrails

Last reviewed: 2026-05-30

## Verdict

The current implementation is considered broadly successful for ordinary admitted-stream continuity and new independent request failover after OAuth account exhaustion.

It is not acceptable to treat a hard-affinity continuation as locally successful when the original account is exhausted. A request carrying official sticky state must either complete upstream on the bound account, retry the same account only inside the short reset window, or return an explicit quota terminal error.

## Official Codex Compatibility Anchors

Reviewed against local official source mirror:

- `D:\CODE\external\_reference_gateway_sources\openai-codex`
- commit `8a827d6` (`Expose MCP server info as part of server status (#24698)`)

Relevant official source facts:

- `codex-rs/core/src/client.rs`: `ModelClientSession` is turn-scoped; `x-codex-turn-state` is captured and replayed only within the same turn.
- `codex-rs/core/src/client.rs`: `previous_response_id` is produced only from a completed upstream response and binds Responses continuation.
- `codex-rs/core/src/client.rs`: `x-codex-turn-metadata` is optional observability metadata, not a hard-affinity routing token.
- `codex-rs/core/src/session/turn.rs`: `ResponseEvent::Completed` records `completed_response_id`; only after this completion does Codex send `response_processed`.

## Non-Negotiable Behavior

- `x-codex-turn-state` and `previous_response_id` are official sticky boundaries.
- `x-codex-turn-metadata` and `x-codex-turn-metadata.turn_id` are lineage/observability only.
- A sticky boundary must not fall through to another account.
- A sticky boundary must not be closed with local `response.completed` / `in_band_local_completion` for `pool_unavailable`.
- An already admitted stream must keep its active lease and finish on the original account even if that account is marked exhausted while the stream is running.
- A new independent request may avoid exhausted/cooldown accounts and use a healthy replacement account.
- Independent `/v1/responses` requests may receive a local completed Responses payload for explicit `pool_unavailable`; this is a client-facing terminal contract, not proof of upstream completion.

## Historical Issues Now Guarded

- Retry-limit 429 surfaced as `exceeded retry limit, last status: 429 Too Many Requests` instead of structured quota/failover handling.
- Local backpressure queue wait did not always cover the request-start interval.
- `pool_unavailable` previously risked leaking as transport 503, `response.failed`, or SSE idle instead of an explicit terminal contract.
- Sticky turn/request affinity could be confused with metadata-only lineage.
- `previous_response_id` continuation risked cross-account reuse without a hard original-account boundary.
- Hard-affinity reset waits could be oversized or killed by the local request timeout.
- Live monitor evidence previously required manual JSON inspection to decide whether in-flight streams survived account exhaustion.

## Regression Gates

Use these focused checks before changing quota continuity, account-pool routing, or local monitor semantics:

```powershell
pwsh -NoProfile -ExecutionPolicy Bypass -File scripts/test-local-hardened-api-live-monitor.ps1
cargo test --manifest-path src-tauri/Cargo.toml --lib pool_unavailable_sticky_responses_keeps_http_error_contract
cargo test --manifest-path src-tauri/Cargo.toml --lib previous_response_id_hard_affinity_blocks_fallback_after_usage_limit
cargo test --manifest-path src-tauri/Cargo.toml --lib codex_turn_metadata_is_lineage_only_not_hard_affinity
node scripts/release/preflight.cjs --skip-typecheck --skip-build --skip-cargo --skip-cargo-test
```

For release-quality closure, keep the repository gate order:

```powershell
npm run build
cargo test --manifest-path src-tauri/Cargo.toml --lib
node scripts/release/preflight.cjs --skip-typecheck --skip-build --skip-cargo --skip-cargo-test
```

## Evidence From 2026-05-29 Runs

- `reports/local-hardened-api-realrun/manual-api-service-quota-drain-sidecar-20260529-230855/live-monitor-20260529-233014.json`: first account exhaustion; new independent requests avoided the exhausted account.
- `reports/local-hardened-api-realrun/manual-api-service-second-account-exhaustion-sidecar-20260529-233740/live-monitor-20260529-235644.json`: second account exhaustion; most admitted streams completed, but one hard-affinity request was locally completed and must remain a fail signal.
- `reports/local-hardened-api-realrun/manual-api-service-second-account-exhaustion-sidecar-20260529-233740/second-account-exhaustion-summary.json`: manual evidence summary; one in-flight stream completed after the second account's first 429, while one older gateway remained unresolved in the primary window.

