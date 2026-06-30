# Tasks: Lattice

> Status: **Phase 4 (Implement)** — in progress. **Phases B + C complete** (T6–T13)
> and **T14 done**: the rmcp server now serves config-driven tools (tools mode) end to
> end — `list_tools` (verbatim schema) + `call_tool` → engine → exec → filter →
> `CallToolResult`, with the production `Client` builder (no redirects, connect timeout).
> Next: **T15 (stdio purity)**, then T16+. The **MVP** (config-driven HTTP tool callable
> over stdio) is effectively reached pending T15's stdout-purity guard.
> Derived from PLAN.md. Each task ≤5 files, single focused session, dependency-ordered.

## Phase A — Foundations

- [x] **T1 — Cargo scaffold** ✅
  - Acceptance: `cargo build` compiles; `lattice --help` shows `--config`, `--http`, and a `check` subcommand; `tracing` initialized to **stderr**; top-level error enum exists.
  - Verify: `cargo build`; `cargo run -- --help`.
  - Files: `Cargo.toml`, `src/main.rs`, `src/error.rs`, `src/lib.rs`, `.gitignore`.

- [x] **T2 — rmcp tracer bullet (de-risk)** ✅
  - Acceptance: minimal `rmcp` `ServerHandler` over stdio exposing ONE hardcoded `ping`→`pong` tool; an in-process rmcp client lists and calls it.
  - Verify: `cargo test mcp_tracer`.
  - Files: `src/mcp/mod.rs`, `src/mcp/server.rs`, `src/main.rs`, `tests/mcp_tracer.rs`.
  - Note: pins the rmcp 1.8 API before the engine exists (risk #1). Also verified
    stdout purity (logs→stderr) and server identifies as `lattice`. rmcp 1.8 API
    locked: `ServerHandler` async-fn methods, `Tool::new`, `ListToolsResult::with_all_items`,
    `CallToolResult::{success,error}`, `ServiceExt::serve(stdio())`.

- [x] **T3 — Config model + load** ✅
  - Acceptance: serde types (Config/Server{name,version,instructions,expose}/Defaults{base_url,headers,auth}/Tool{name,description,inputSchema,http|cli,response}/Http/Cli/Auth/Response); same fixture parses identically from YAML (`serde_norway`) and JSON; defaults merge into tools.
  - Verify: `cargo test config_parse`.
  - Files: `src/config/mod.rs`, `src/config/load.rs`, `tests/config_parse.rs`, `tests/fixtures/example.{yaml,json}`.
  - Note: `deny_unknown_fields` on structs (typo-catching); `Auth` internally tagged
    by `type`; exactly-one-of http/cli enforced in `validate`; defaults-merge in
    `apply_defaults`. Schema/env/include-exclude validation deferred to T4/T5/T8.

- [x] **T4 — `${ENV}` interpolation** ✅
  - Acceptance: `${VAR}` in any string leaf replaced from env; all missing vars collected into one descriptive error; non-`${}` `$` left intact.
  - Verify: `cargo test interpolate`.
  - Files: `src/config/interpolate.rs`, in-module tests.
  - Note: typed-tree walk (no Serialize round-trip); interpolates value-bearing leaves
    incl. auth, skips `inputSchema` (verbatim) and bare `$ref` (engine, T6). Env lookup
    injected for race-free tests. `ConfigError::MissingEnv` lists all unset vars; wired
    into `load_config` before defaults-merge.

- [x] **T5 — `check` mode** ✅
  - Acceptance: `lattice check --config X` parses, interpolates (reports missing env), enforces exactly-one-of http/cli, compiles each `inputSchema` as valid JSON Schema, warns on `$ref`s absent from the schema; prints summary (N tools, expose mode) and exits nonzero on any error.
  - Verify: `cargo test check_mode` (1 good + several bad fixtures).
  - Files: `src/config/load.rs`, `src/main.rs`, `tests/check_mode.rs`.
  - Note: `check`/`check_str` return a `CheckReport {errors, warnings, tool_count, expose}`;
    collects ALL issues (doesn't bail). Uses `jsonschema::validator_for` (default features
    off → SSRF-safe). Auth unknown-key check walks the raw document. 10 check_mode tests.
  - Also (from T3 review): validate `include`/`exclude` mutual exclusivity, reject
    `body` + `body_from` both-set, and per-variant `auth` known-key checking
    (internally-tagged `Auth` can't use `deny_unknown_fields`, so `scope:` vs
    `scopes:` is silently dropped today).
  - Also (from T4 review): warn on any residual `${...}`-shaped substring left after
    interpolation (catches malformed/invalid-name refs that pass through as literals).

## Phase B — Engine (pure; parallel after T6)

- [x] **T6 — Value expressions** ✅
  - Acceptance: `enum ValueExpr {InputRef,Env,Literal,Template}` parsed from config values; resolved against `{input,env}`; dotted input lookup; `{path}` sugar; minijinja render; missing input/env → typed error.
  - Verify: `cargo test value_expr`.
  - Files: `src/engine/mod.rs`, `src/engine/value.rs`.
  - Note (design reconciliation): **no `Env` variant** — `${ENV}` is resolved at load
    (T4), so the engine `ValueExpr` is `{InputRef, Template, Literal}` over `{input}`
    only. `$ref` is a whole-value ref (preserves type); `{{ }}` templates render to
    strings (lenient undefined); paths use `{name}` sugar via `resolve_path`. `resolve`
    recurses into arrays/objects.

- [x] **T7 — Nested body builder** ✅
  - Acceptance: dotted target-path keys build/merge nested JSON (`user.name.first`+`user.name.last`→`{user:{name:{first,last}}}`); `body_from:$ref` passthrough.
  - Verify: `cargo test body_builder`.
  - Files: `src/engine/body.rs`.
  - Note (from T6 review): use `value::resolve_optional` so a body entry whose `$ref`
    targets an absent optional input is **omitted** rather than erroring; present-but-null
    refs are kept.
  - Note: `build_body(body, body_from, ctx) -> Option<Value>` (decoupled from `HttpTarget`
    for testability). Empty / fully-omitted body → `None` (no body sent); `body_from` wins
    over `body` (mutually exclusive post-T5). Dotted insert detects the only reachable
    conflict — descending through a key already set as a leaf (`user` then `user.name`);
    the reverse can't occur because `BTreeMap` yields a prefix key before its extension.
    Content-type/serialization left to T8.

- [x] **T8 — HTTP request builder (pure)** ✅
  - Acceptance: `Tool.http`+input → `HttpRequestSpec{method,url,query,headers,body,content_type}`; path vars filled; missing path var → error.
  - Verify: `cargo test http_request_builder`.
  - Files: `src/engine/request.rs`.
  - Note (from T6 review): when resolving many template leaves per request, reuse a
    single minijinja `Environment` (T6's `render` builds one per call).
  - Note: addressed the env-reuse — `value::template_env()` is a process-wide `OnceLock`
    `Environment` (fuel is per-render, so sharing is safe). Builder consumes the
    defaults-merged `HttpTarget` (base_url/headers/auth already resolved by
    `apply_defaults`). query/headers use `resolve_optional` (omit absent), fan arrays into
    repeated pairs, drop `null`, error on objects; body via T7; content-type defaults to
    `application/json` unless the tool sets its own `Content-Type`. URL/query are **not**
    percent-encoded and header values **not** CRLF-checked here — left to reqwest in T11.
  - For T11: validate/encode at the boundary — reqwest must reject control chars in header
    values (CRLF injection) and percent-encode query pairs derived from model input.

- [x] **T9 — CLI command builder (pure)** ✅
  - Acceptance: `Tool.cli`+input → `CommandSpec{program,argv,stdin,env,cwd}`; value-expr substitution; array input flattens to multiple args; no shell.
  - Verify: `cargo test cli_command_builder`.
  - Files: `src/engine/command.rs`.
  - Note: `build_command(target, ctx)`. `program` is the literal config string (**never** a
    value expr) — model input can't choose the binary; args/env/stdin/cwd resolve via value
    exprs and become distinct argv/env entries (argv-only, no shell). args fan arrays out,
    omit absent/`null`; env omits absent/`null`, errors on objects; stdin pipes a string
    verbatim / other JSON compactly / none for absent-null; cwd is a single scalar or
    inherit. Shared `value::scalarize` helper (also refactored into T8's request builder).
  - For T13: env/stdin values may carry interpolated `${ENV}` secrets — redact at the log
    boundary; cap captured stdout size; note relative `program` + model-controlled `cwd`.

- [x] **T10 — Response filter** ✅
  - Acceptance: include keeps only listed dotted paths (nested), exclude drops them, neither → unchanged; parse modes raw/json/lines.
  - Verify: `cargo test response_filter`.
  - Files: `src/engine/response.rs`.
  - Note: `parse_output(text, mode)` (raw→string, json→parsed value w/ error, lines→array,
    `str::lines` handles `\r\n`) + `filter(value, spec)`. Dotted paths navigate **objects**
    only (array indices unaddressed — documented boundary); non-object values (arrays,
    scalars, raw/lines output) pass `filter` through unchanged. `include` rebuilds a pruned
    object, `exclude` removes in place, neither → as-is; `include` wins if both set.

## Phase C — Execution (I/O)

- [x] **T11 — HTTP executor** ✅
  - Acceptance: runs `HttpRequestSpec` via reqwest against `wiremock`, asserts correct method/url/headers/body, returns filtered body; non-2xx → `isError` with filtered body.
  - Verify: `cargo test --test http_integration`.
  - Files: `src/exec/mod.rs`, `src/exec/http.rs`, `tests/http_integration.rs`.
  - Note: `exec::http::execute(client, spec, response_spec) -> ToolOutcome {is_error, value}`.
    Non-2xx → `is_error: true` with the **filtered** body (a tool error the model sees);
    `ExecError` is reserved for genuine transport failures (no response). Response body is
    JSON when it parses, else the raw text as a string; then `response::filter` is applied.
    Added deps: `reqwest` (rustls, no native-tls) + `wiremock` (dev). 10 integration tests
    + 2 unit tests. Auth application is a clean seam (`to_reqwest_request`) for T12.
  - Hardening addressed (T4 + T7–T10 + T11 reviews, both subagents Approve):
    - **Path-var percent-encoding** at the correct layer — `value::resolve_path` strictly
      path-segment-encodes each substituted value (operator separators untouched) **and**
      rejects a lone `.`/`..` (`ValueError::UnsafePathVar`) which encoding can't neutralize.
      Round-trip integration test confirms `a/b c` → `/items/a%2Fb%20c` survives reqwest.
    - Headers built via `HeaderName`/`HeaderValue` (reject CRLF/controls); query pairs
      percent-encoded by reqwest's `.query()`. CRLF-rejection test asserts no value leak.
    - **Per-request 30s timeout** (defense-in-depth vs hanging upstream) and a **10 MiB
      response-body cap** (`read_body`, streamed via `chunk()`) → `ResponseTooLarge`.
    - No whole-spec logging — one curated `debug!` (method + status only). Transport errors
      scrubbed via `reqwest::Error::without_url()`; `InvalidHeader` carries only the name.
  - Deferred to T12/T14 (tracked):
    - At the production `Client` builder (T14): `redirect::Policy::none()` (a hostile
      upstream could 302 to an internal host; reqwest doesn't strip *custom* auth headers on
      cross-host redirects) + `connect_timeout`; make timeout/size limits configurable.
    - Map a model-input-triggered build failure (e.g. `InvalidHeader`) to an `is_error`
      result at the MCP wiring layer so the model can correct it, not a hard error.
    - T12: warn when an auth secret would be sent over cleartext `http://`.
    - Never `Debug`-log `HttpRequestSpec`/`CommandSpec` at call sites; scrub
      `ValueError::Template(..)` before surfacing in a logged request error.
    - Docs (T19): warn against putting a `{placeholder}` in a `base_url`-less authority.

- [x] **T12 — Auth (bearer/basic/api_key + oauth2)** ✅
  - Acceptance: each auth type adds correct header/query; oauth2 client-credentials fetches token from mock `token_url`, caches with expiry margin, single-flight refresh, refreshes on 401; secrets redacted in logs.
  - Verify: `cargo test --test http_integration auth_*`.
  - Files: `src/exec/auth.rs`, `src/exec/http.rs`, `tests/http_integration.rs`.
  - Note: `AuthState::new(Auth)` holds the scheme + an `OAuthCache`, created once per tool
    so the token cache persists. `execute` gained an `Option<&AuthState>`. Static schemes
    (bearer/basic/api_key) are applied via reqwest (`bearer_auth`/`basic_auth`/header|query).
    OAuth2 client-credentials: single-slot cache, **single-flight** (async mutex held across
    the fetch), 60s expiry margin; `execute` does one **refresh-and-retry on 401** (clones
    the request only for oauth). Secrets never logged; `AuthError` messages carry only a
    status code / URL-scrubbed transport error — never the token or client secret. 9
    `auth_*` integration tests (caching: 1 fetch / 2 calls; refresh: 2 fetches on 401;
    token-endpoint failure; missing `access_token`; expiry-margin refetch).
  - Review fixes (security + quality subagents, both Approve, no leaks): clamp token
    lifetime to ≤24h (a hostile `expires_in` would overflow `Instant +` → panic) and cap
    the token-response body at 64 KiB (`read_token_body`, mirroring the main-path guard).
  - Carried forward to T14 (Client builder): `redirect::Policy::none()` + `connect_timeout`;
    warn when an auth secret would ride cleartext `http://`; make timeouts configurable.

- [x] **T13 — CLI executor** ✅
  - Acceptance: runs `CommandSpec` via tokio::process against a real test script; captures stdout/stderr/exit; `parse:json` filtered; non-zero exit → `isError` with stderr.
  - Verify: `cargo test --test cli_integration`.
  - Files: `src/exec/cli.rs`, `tests/cli_integration.rs`, `tests/fixtures/script.sh`.
  - Note: `cli::execute(spec, parse, response_spec) -> ToolOutcome`. argv-only (no shell).
    Non-zero exit / signal → `is_error: true` with `{exit_code, stderr}`; `parse_output`
    (raw/json/lines) + `response::filter` on success; `parse: json` mismatch / spawn / I/O
    → `ExecError` (`Process`/`Parse`/`ResponseTooLarge`). stdin is written **concurrently**
    with draining stdout+stderr (`try_join!`) to avoid a pipe deadlock; each stream capped
    at 10 MiB; whole run has a 120s timeout (`kill_on_drop`). Log line carries only program
    + exit code — never argv/env/stdin/output (may hold `${ENV}` secrets). Added tokio
    `io-util`/`time` features. 11 integration tests (incl. signal-kill → null exit, and
    `parse:json` mismatch → `ExecError::Parse`) + 1 cap unit test.
  - Deferred hardening (from T13 security review, all rated Consider — proper fixes are
    config-model/design decisions, not executor one-liners):
    - **Env-secret inheritance:** CLI children inherit Lattice's full process env, which
      holds `${ENV}` secrets used for HTTP/OAuth auth — a tool that dumps env (or a
      model-controlled path reading `/proc/self/environ`) could surface them. Needs an
      env-passthrough policy (`.env_clear()` + allowlist, or scrub interpolation-consumed
      var names). Track for a config-model `cli.env` policy.
    - **Program/loader hijack:** a bare `command` + a model-influenced `env` `PATH`/
      `LD_PRELOAD`/`LD_*`/`DYLD_*` (or a relative program + model-controlled `cwd`) can
      redirect which binary/loader runs. Add `check`-mode validation: prefer an absolute
      `program`; reject loader-control keys in `env`.
    - **Process-group reaping:** `kill_on_drop` SIGKILLs only the direct child; a
      double-forking child orphans grandchildren (and can hold the pipes open until the
      timeout). Consider `setsid` via `pre_exec` + `killpg` (Unix, `unsafe`).
    - CLI timeout (120s) is fixed — make it operator-configurable (with the env policy).

## Phase D — MCP surface

- [x] **T14 — Server tools-mode + result mapping (MVP)** ✅
  - Acceptance: replace tracer's hardcoded tool — `list_tools` returns config tools with **verbatim** inputSchema; `call_tool` dispatches name→engine→exec→filter→`CallToolResult` (text content; optional structuredContent); isError propagates. In-process rmcp client calls an HTTP tool end-to-end vs wiremock.
  - Verify: `cargo test --test mcp_roundtrip tools_mode`.
  - Files: `src/mcp/server.rs`, `src/mcp/result.rs`, `src/main.rs`, `tests/mcp_roundtrip.rs`.
  - Note: `LatticeServer::new(Config)` prepares per-tool descriptors + (HTTP) `AuthState`
    **once** (so the OAuth token cache persists across calls) and a shared production
    `reqwest::Client`. `call_tool` builds input → `Ctx` → `target()` dispatch:
    `build_request`+`exec::http::execute` (with auth) / `build_command`+`exec::cli::execute`.
    `result::outcome_to_result` maps `ToolOutcome` → text content + (object-only)
    `structuredContent`, propagating `is_error`. **Every** model-actionable failure is an
    `isError` *result*, never a protocol error: unknown tool, engine build error, exec
    failure, non-2xx/non-zero. Tracer (`mcp_tracer.rs`) replaced by `mcp_roundtrip.rs`
    (HTTP vs wiremock + real-process CLI); `main.rs serve_stdio` now requires `--config`
    and loads it. 2 roundtrip + 7 unit tests (130 total).
  - Carried-forward hardening **landed here**: production `Client` builder with
    `redirect::Policy::none()` + `connect_timeout(10s)`; cleartext-`http://`-auth warning;
    model-input build failures (`RequestError`/`CommandError`) mapped to `isError` results;
    `ValueError::Template` messages scrubbed before surfacing/logging (could echo an
    interpolated secret). Still deferred: operator-configurable timeouts/size caps; the
    CLI env-passthrough/loader-hijack policy (T13 notes) — track for the config model.

- [ ] **T15 — stdio wiring + stdout purity**
  - Acceptance: `lattice --config X` serves over stdio; a test asserts stdout carries **only** framed JSON-RPC (no log/print leakage).
  - Verify: `cargo test stdio_purity`.
  - Files: `src/main.rs`, `src/mcp/mod.rs`, `tests/stdio_purity.rs`.

- [ ] **T16 — Dispatcher mode**
  - Acceptance: `expose: dispatcher` → `tools/list` returns exactly `describe_route`+`call_route`; auto-generated server `instructions` embed the route catalog (name+description, no schemas); `describe_route(name)`→schema/detail; `call_route(name,params)`→validate vs route schema→engine+exec; bad route/params → clear isError.
  - Verify: `cargo test --test mcp_roundtrip dispatcher`.
  - Files: `src/mcp/server.rs`, `src/mcp/dispatcher.rs`, `tests/mcp_roundtrip.rs`.

- [ ] **T17 — Runtime input-schema validation**
  - Acceptance: call params validated against the tool's compiled `inputSchema` before any request/command; violations → `isError` listing them, nothing executed. Reuses schemas compiled in T5.
  - Verify: `cargo test schema_validation`.
  - Files: `src/engine/validate.rs`, `src/mcp/server.rs`.
  - Also (from T5 review): `jsonschema` pulls `fancy-regex`; if a tool's `inputSchema`
    uses `pattern` and we match it against model-supplied (attacker-influenced) input,
    watch for ReDoS. Compile schemas once and reuse; consider bounding match effort.

- [ ] **T18 — Streamable HTTP transport**
  - Acceptance: `--http 127.0.0.1:8080` serves the same tools over Streamable HTTP (loopback default); stdio still works when the flag is absent; an rmcp HTTP client lists+calls a tool.
  - Verify: `cargo test --test http_transport`.
  - Files: `src/main.rs`, `src/mcp/mod.rs`, `tests/http_transport.rs`.

## Phase E — Polish

- [ ] **T19 — Examples, README, lint gate**
  - Acceptance: `examples/{httpbin,github,ls}.yaml` all pass `lattice check`; README covers quickstart, config reference, expose modes, auth, value expressions; `cargo clippy --all-targets -- -D warnings` and `cargo fmt --check` are green.
  - Verify: `lattice check --config examples/*.yaml`; `cargo clippy --all-targets -- -D warnings`; `cargo fmt --check`.
  - Files: `examples/httpbin.yaml`, `examples/github.yaml`, `examples/ls.yaml`, `README.md`.

## Parallelizable

- After **T6**: T7, T8, T9, T10 independent.
- **T11** ∥ **T13**; **T12** after T11.
- **T17** feeds T16; **T18** is an independent leaf.

## Milestones

1. **T2** — rmcp API pinned.
2. **T5** — configs validate (`check`).
3. **T10** — engine complete, fully unit-tested, zero I/O.
4. **T14/T15** — **MVP**: config-driven HTTP tool callable over stdio.
5. **T19** — shippable v1.
