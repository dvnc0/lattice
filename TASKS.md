# Tasks: Lattice

> Status: **Phase 4 (Implement)** — in progress. T1–T2 complete; rmcp API pinned.
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

- [ ] **T3 — Config model + load**
  - Acceptance: serde types (Config/Server{name,version,instructions,expose}/Defaults{base_url,headers,auth}/Tool{name,description,inputSchema,http|cli,response}/Http/Cli/Auth/Response); same fixture parses identically from YAML (`serde_norway`) and JSON; defaults merge into tools.
  - Verify: `cargo test config_parse`.
  - Files: `src/config/mod.rs`, `src/config/load.rs`, `tests/config_parse.rs`, `tests/fixtures/example.{yaml,json}`.

- [ ] **T4 — `${ENV}` interpolation**
  - Acceptance: `${VAR}` in any string leaf replaced from env; all missing vars collected into one descriptive error; non-`${}` `$` left intact.
  - Verify: `cargo test interpolate`.
  - Files: `src/config/interpolate.rs`, in-module tests.

- [ ] **T5 — `check` mode**
  - Acceptance: `lattice check --config X` parses, interpolates (reports missing env), enforces exactly-one-of http/cli, compiles each `inputSchema` as valid JSON Schema, warns on `$ref`s absent from the schema; prints summary (N tools, expose mode) and exits nonzero on any error.
  - Verify: `cargo test check_mode` (1 good + several bad fixtures).
  - Files: `src/config/load.rs`, `src/main.rs`, `tests/check_mode.rs`.

## Phase B — Engine (pure; parallel after T6)

- [ ] **T6 — Value expressions**
  - Acceptance: `enum ValueExpr {InputRef,Env,Literal,Template}` parsed from config values; resolved against `{input,env}`; dotted input lookup; `{path}` sugar; minijinja render; missing input/env → typed error.
  - Verify: `cargo test value_expr`.
  - Files: `src/engine/mod.rs`, `src/engine/value.rs`.

- [ ] **T7 — Nested body builder**
  - Acceptance: dotted target-path keys build/merge nested JSON (`user.name.first`+`user.name.last`→`{user:{name:{first,last}}}`); `body_from:$ref` passthrough.
  - Verify: `cargo test body_builder`.
  - Files: `src/engine/body.rs`.

- [ ] **T8 — HTTP request builder (pure)**
  - Acceptance: `Tool.http`+input → `HttpRequestSpec{method,url,query,headers,body,content_type}`; path vars filled; missing path var → error.
  - Verify: `cargo test http_request_builder`.
  - Files: `src/engine/request.rs`.

- [ ] **T9 — CLI command builder (pure)**
  - Acceptance: `Tool.cli`+input → `CommandSpec{program,argv,stdin,env,cwd}`; value-expr substitution; array input flattens to multiple args; no shell.
  - Verify: `cargo test cli_command_builder`.
  - Files: `src/engine/command.rs`.

- [ ] **T10 — Response filter**
  - Acceptance: include keeps only listed dotted paths (nested), exclude drops them, neither → unchanged; parse modes raw/json/lines.
  - Verify: `cargo test response_filter`.
  - Files: `src/engine/response.rs`.

## Phase C — Execution (I/O)

- [ ] **T11 — HTTP executor**
  - Acceptance: runs `HttpRequestSpec` via reqwest against `wiremock`, asserts correct method/url/headers/body, returns filtered body; non-2xx → `isError` with filtered body.
  - Verify: `cargo test --test http_integration`.
  - Files: `src/exec/mod.rs`, `src/exec/http.rs`, `tests/http_integration.rs`.

- [ ] **T12 — Auth (bearer/basic/api_key + oauth2)**
  - Acceptance: each auth type adds correct header/query; oauth2 client-credentials fetches token from mock `token_url`, caches with expiry margin, single-flight refresh, refreshes on 401; secrets redacted in logs.
  - Verify: `cargo test --test http_integration auth_*`.
  - Files: `src/exec/auth.rs`, `src/exec/http.rs`, `tests/http_integration.rs`.

- [ ] **T13 — CLI executor**
  - Acceptance: runs `CommandSpec` via tokio::process against a real test script; captures stdout/stderr/exit; `parse:json` filtered; non-zero exit → `isError` with stderr.
  - Verify: `cargo test --test cli_integration`.
  - Files: `src/exec/cli.rs`, `tests/cli_integration.rs`, `tests/fixtures/script.sh`.

## Phase D — MCP surface

- [ ] **T14 — Server tools-mode + result mapping (MVP)**
  - Acceptance: replace tracer's hardcoded tool — `list_tools` returns config tools with **verbatim** inputSchema; `call_tool` dispatches name→engine→exec→filter→`CallToolResult` (text content; optional structuredContent); isError propagates. In-process rmcp client calls an HTTP tool end-to-end vs wiremock.
  - Verify: `cargo test --test mcp_roundtrip tools_mode`.
  - Files: `src/mcp/server.rs`, `src/mcp/result.rs`, `src/main.rs`, `tests/mcp_roundtrip.rs`.

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
