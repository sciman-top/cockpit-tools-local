# New Gateway Hard-Affinity Real Probe - 2026-05-27

## Result
- Overall: pass
- Model: gpt-5.5
- Gateway exe SHA256: 8513C253B76E2EE3EFDBC251340A0E46A996DB9CC0A0C3518F3839B2E4164587
- Selected account hashes: sha256:0a7db35b2db7, sha256:2c1f79315627
- First sticky account hash: sha256:0a7db35b2db7

## Real Quota Evidence
- Initial sticky request: status=200, timedOut=False
- Real usage limit observed: True
- Quota account hash: sha256:0a7db35b2db7
- Retry-After ms: 275811000
- Usage-limit event count: 19
- Fallback selected count: 1

## Hard-Affinity Evidence
- Same-turn hard-affinity probe attempted: True
- Same-turn probe timed out client-side within bounded window: True
- No immediate generic 429 within client window: True
- Request timeout ms: 691201000
- Normal timeout ms: 600000
- Timeout extended: true
- Hard-affinity wait limit ms: 691200000
- Pool wait reason: hard_affinity_same_account_retry
- Pool wait retry-after ms: 275809000
- Final 429 count for same turn: 0

## Notes
- This probe used an isolated data root under %TEMP% and did not modify live Codex/Cockpit config.
- The sidecar checkpoint from the run was produced before the client_route/response_adapter monitor refinement, so its responses-503 verdict may over-attribute chat adapter traffic. New monitor fixtures cover the refined behavior.
