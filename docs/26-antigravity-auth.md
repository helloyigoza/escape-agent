# 26 — Antigravity OAuth & Cloud Code Assist Gateway

Architecture and interface contract for `oc-antigravity`, the native Rust port of
`opencode-antigravity-auth` for `opencode-rust`.

## Goals

- Authenticate Google accounts via Antigravity OAuth 2.0 + PKCE (local loopback
  callback listener on `127.0.0.1:51121/oauth-callback` or manual code/URL
  completion) and resolve Cloud Code Assist managed project context
  (`loadCodeAssist` / `onboardUser`).
- Maintain both:
  - the SQLite `credential` table in `oc-store` (`integrationID = "google"`,
    `type = "oauth"`, `methodID = "antigravity"`, packed
    `refreshToken|projectId|managedProjectId`), and
  - the multi-account pool file `antigravity-accounts.json` (v4 format with
    v1–v3 migration, SHA-256 token hashes, per-family active cursors, quota
    cache, verification flags, and device fingerprints).
- Route `google/antigravity-*` models (and `google/*` models backed by an active
  Antigravity OAuth credential) through the Cloud Code Assist Unified Gateway
  (`/v1internal:streamGenerateContent?alt=sse`) using `oc-llm::LlmTransportHook`
  on `SharedLlmClient`, preserving standard API-key Gemini behavior when an API
  key (`GEMINI_API_KEY` or `key` credential) is used on non-Antigravity models.
- Expose Antigravity model definitions (Gemini 2.5 / 3 / 3.8 / 4 Argon &
  Hydrogen + Claude Sonnet/Opus 4.5–5.5 + GPT-OSS 120B defaults, grouped
  thinking tiers `low/medium/high/xhigh`, live
  `/v1internal:fetchAvailableModels` catalog discovery auto-refreshed on OAuth
  login and startup, and dynamic runtime catalog resolution via
  `SharedLlmClient::resolve_model`) and the `google_search` grounding tool.

## Crate Placement & Boundaries

- `crates/oc-antigravity` depends only on `oc-types`, `oc-store`, `oc-config`,
  `oc-llm`, `oc-tools`, and workspace HTTP/async/crypto utilities. It **must
  not** depend on `oc-runtime` or `oc-server`.
- `oc-llm` exposes:
  - `LlmTransportHook` trait (`handles`, `stream`, `resolve_model`,
    `dynamic_models`) and hook attachment on `SharedLlmClient` / `LlmClient`
    (`with_hook`, `set_hook`, `resolve_model`, `dynamic_models`).
  - `Catalog::upsert_provider_models` to overlay `antigravity-*` and discovered
    models onto `"google"`.
- `oc-server` exposes `AppState::antigravity()` and wires:
  - `POST/GET/DELETE /api/integration/google/connect/oauth*` (real PKCE OAuth
    attempts when `methodID = "antigravity"`, returning 501 only for engines not
    yet attached such as remote MCP OAuth).
  - `/api/antigravity/accounts*`, `/api/antigravity/quotas`, and
    `/api/antigravity/models/refresh`.
  - `AntigravityToolProvider` (`oc_runtime::ToolProvider`) providing
    `google_search`.
- `oc-cli` provides `opencode auth <login|list|status|quota|use|enable|disable|remove|models>`
  and `/antigravity` (`/ag`) in interactive chat, and wires `AntigravityService`
  during `bootstrap`.

## Multi-Account Rotation & Failover

- **Strategies**: `sticky`, `round-robin`, and `hybrid` (default: deterministic
  score combining `HealthScoreTracker`, `TokenBucketTracker`, LRU recency, and
  optional PID offset).
- **Per-family & Per-quota Rate Limits**: Tracks reset timestamps separately
  for `claude`, `gemini-antigravity[:model]`, and `gemini-cli[:model]`.
- **Dual Quota Pool (Gemini)**: Respects explicit `-antigravity` / `-cli`
  routing or `cli_first`, and fails over between Antigravity Hub
  (`daily-cloudcode-pa.googleapis.com`) and Gemini CLI
  (`cloudcode-pa.googleapis.com`) before blocking.
- **Retry & Recovery**:
  - `MODEL_CAPACITY_EXHAUSTED` / `SERVER_ERROR`: exponential backoff (1 s → 8 s,
    up to 3 attempts per endpoint) plus fingerprint regeneration.
  - `RATE_LIMIT_EXCEEDED` / `QUOTA_EXHAUSTED`: respects `Retry-After` /
    `google.rpc.RetryInfo`, honors `cache_first` waits up to
    `max_cache_first_wait_seconds`, otherwise switches accounts or falls back to
    alternate quota pool.
  - `403 validation_required`: extracts verification URL, flags the account as
    `verificationRequired` and disables it, then switches to the next healthy
    account.
  - Empty SSE streams / `thinking_block_order` 400 errors: retries or applies
    `close_tool_loop_for_thinking` recovery when enabled.

## Request & Stream Transformation Pipeline

1. **Model resolution**: Maps `antigravity-*` aliases, tier suffixes
   (`-low`/`-medium`/`-high`/`-minimal`), variants, and dynamic advertised IDs.
2. **Schema cleaning**: 4-phase JSON Schema sanitizer (`$ref` hint, `const` →
   `enum`, constraint hints, `allOf` merge, `anyOf`/`oneOf` flattening,
   type-array flattening, unsupported keyword stripping, empty-object
   `_placeholder`), plus `to_gemini_schema` uppercase types for Gemini models.
3. **Claude transforms**: `toolConfig.functionCallingConfig.mode = "VALIDATED"`,
   snake_case `thinking_config`, `maxOutputTokens >= 64000`, tool name
   sanitization, optional `claude_tool_hardening` parameter hints + system
   instruction, `functionCall` ID assignment & FIFO `functionResponse` pairing,
   cross-model signature sanitization, and thinking signature cache /
   `skip_thought_signature_validator` sentinel injection.
4. **Envelope & SSE Decoding**: Wraps into `{ project, model, request, userAgent,
   requestType, requestId }`, decodes `{ response: { candidates, usageMetadata }
   }` SSE frames into `oc_llm::LlmEvent`, deduplicates cumulative thinking
   deltas, and caches returned `thoughtSignature` values.

## Credential Key Compatibility

- OAuth credentials in `oc-store` follow OpenAPI's `Credential.OAuth` shape
  (`{ type: "oauth", methodID, refresh, access, expires, metadata? }`).
- Runtime, observer, and scribe `credential_key` resolution check `access`
  before legacy `access_token`, and session export sanitization strips `access`
  and `refresh` alongside `access_token` and `refresh_token`.
