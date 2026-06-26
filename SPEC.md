# Spec: Lattice — a config-driven MCP shell for APIs & CLIs

> Status: **Phase 1 (Specify)** — awaiting human review before Phase 2 (Plan).
> Last updated: 2026-06-25

## Objective

**Lattice turns any existing REST API or CLI tool into an MCP server using a single declarative config file — no code.**

You write a YAML/JSON config that (a) declares a set of MCP tools with hand-written
JSON Schemas, and (b) maps each tool to an HTTP request or a CLI invocation. Lattice
exposes those tools over MCP (`tools/list`, `tools/call`), translates an incoming tool
call into the underlying request, executes it, filters the response down to what the
harness should see, and returns it.

**Primary user:** a developer who has an API or CLI they want an LLM/agent to drive,
and wants it available as an MCP server in minutes instead of writing a bespoke server.

**Success looks like:** point lattice at a config and a real third-party API becomes a
working MCP server that Claude Code/Desktop can call, with no Rust written by the user.

### Core capabilities (from the brief)

1. **Config-mapped tools** — YAML or JSON maps MCP tools → API/CLI calls.
2. **Nested-body mapping** — flat MCP input (`firstName: "Bob"`) maps into a nested
   request body (`user.name.first`).
3. **Path/URL variable substitution** — `/user/{userId}/update` filled from input.
4. **Response field filtering** — include-list or exclude-list to limit what returns
   to the harness.
5. **Standard MCP surface** — `tools/list`, `tools/call` (+ optional richer detail view).
6. **CLI tools too** — same config pattern maps tools to argv/stdin/env/cwd.
7. **Passed values as variables** — e.g. `userId` from input used in URL and body.

## Tech Stack

- **Language:** Rust (edition 2021/2024), toolchain 1.94.
- **MCP:** [`rmcp`](https://crates.io/crates/rmcp) v1.8 — official Rust MCP SDK
  (JSON-RPC, `initialize`/`tools/list`/`tools/call`, stdio + Streamable HTTP transports).
- **Async runtime:** `tokio`.
- **HTTP client:** `reqwest` (rustls, json).
- **CLI exec:** `tokio::process`.
- **Config:** `serde` + `serde_json` + a maintained YAML crate (`serde_norway`;
  `serde_yaml` is deprecated/archived — decide in Plan).
- **Templating:** `minijinja` (the `Template` value variant; behind a path that lets
  us keep it lightweight).
- **Schema:** JSON Schema authored by the user, passed verbatim to the harness;
  optional input validation via `jsonschema` crate.
- **CLI args:** `clap`.
- **Errors/logging:** `thiserror` + `anyhow`; `tracing` + `tracing-subscriber`
  (**logs to stderr only** — stdout is the JSON-RPC channel in stdio mode).

## Commands

```
Build:        cargo build --release
Run (stdio):  lattice --config tools.yaml
Run (+HTTP):  lattice --config tools.yaml --http 127.0.0.1:8080
Validate:     lattice check --config tools.yaml   # parse + env + schema, no server
Test:         cargo test
Lint:         cargo clippy --all-targets -- -D warnings
Format:       cargo fmt
```

## Project Structure

```
lattice/
  Cargo.toml
  SPEC.md                  # this document (living)
  README.md
  src/
    main.rs                # clap entry; transport selection; check mode
    config/
      mod.rs               # Config/Server/Tool/Http/Cli/Auth/Response types (serde)
      interpolate.rs       # ${ENV} interpolation over loaded config
      load.rs              # load + parse (yaml|json by extension) + validate
    engine/
      value.rs             # ValueExpr resolution: InputRef | Env | Literal | Template
      request.rs           # build HTTP request (url, path vars, query, headers, body)
      body.rs              # dotted-path nested body builder
      command.rs           # build CLI argv/stdin/env/cwd
      response.rs          # include/exclude dotted-path filtering + parse modes
    exec/
      http.rs              # reqwest execution
      cli.rs               # process execution
      auth.rs              # bearer/basic/api-key + oauth2 client-credentials token cache
    mcp/
      server.rs            # rmcp ServerHandler: list_tools / call_tool / (view)
      result.rs            # engine output -> MCP CallToolResult content
    error.rs
  tests/
    http_integration.rs    # wiremock-backed request/response translation
    cli_integration.rs     # real process exec
    mcp_roundtrip.rs       # in-process rmcp client drives the server
  examples/
    httpbin.yaml           # HTTP example (nesting, path vars, filtering)
    github.yaml            # real API w/ bearer auth
    ls.yaml                # CLI example
```

## Config Model (draft)

One config file = one MCP server. `defaults` are inherited by all tools; each tool
has **exactly one** of `http:` or `cli:`.

```yaml
server:
  name: example-api
  version: 0.1.0
  instructions: "Wraps the Example API."     # optional, surfaced to the harness

defaults:
  base_url: https://api.example.com
  headers: { Accept: application/json }
  auth:
    type: oauth2                              # bearer | basic | api_key | oauth2
    token_url: https://auth.example.com/token
    client_id: ${EXAMPLE_CLIENT_ID}
    client_secret: ${EXAMPLE_CLIENT_SECRET}
    scopes: [read, write]

tools:
  - name: update_user
    description: Update a user's profile.
    inputSchema:                              # hand-written JSON Schema, verbatim
      type: object
      properties:
        userId:    { type: string }
        firstName: { type: string }
        lastName:  { type: string }
      required: [userId, firstName]
    http:
      method: POST
      path: /user/{userId}/update             # {userId} <- input
      query:   { notify: "true" }             # name -> value expression
      headers: { X-Source: lattice }
      body:                                    # dotted target path -> value expression
        user.name.first: $firstName
        user.name.last:  $lastName
        source:          lattice               # literal
        fullName:        "{{ input.firstName }} {{ input.lastName }}"   # template
      response:
        include: [id, user.name, updatedAt]    # OR exclude: [audit, _internal]

  - name: list_dir
    description: List a directory.
    inputSchema:
      type: object
      properties: { dir: { type: string } }
      required: [dir]
    cli:
      command: ls
      args: ["-la", "$dir"]                    # value expressions; arrays flatten
      # stdin:  <value expr>     env: { K: ${ENV} }     cwd: <value expr>
      parse: raw                               # raw | json | lines  (default raw)
      response: { }                            # filtering applies when parse=json
```

### Expose modes (small vs. large APIs)

The MCP surface is config-driven via a server-level `expose:` field, because a
standard harness only ever sees `tools/list` + the server `instructions` — there is no
on-demand schema fetch a server can offer. So the route catalog and any multi-step
workflow must live in those two places.

- **`expose: tools`** (default) — every route is a first-class MCP tool; its authored
  JSON Schema appears in `tools/list`. Best for small APIs; maximum upfront validation.
- **`expose: dispatcher`** — `tools/list` exposes exactly two tools, `describe_route`
  and `call_route`. The **lightweight route catalog** (route name + one-line when-to-use,
  **no schemas**) is embedded into the auto-generated server `instructions` and the
  `call_route` description. Flow: model reads catalog → *optionally* `describe_route(name)`
  for exact params → `call_route(name, params)`. `describe_route` is the optional
  "zoom-in" step; a confident model may call `call_route` directly.

```yaml
server:
  name: example-api
  expose: dispatcher          # tools (default) | dispatcher
```

Lattice **auto-generates** the dispatcher `instructions` and tool descriptions from the
config so the two-step workflow is explicit to the model (author may override
`server.instructions`). The translation engine is identical across modes — only the MCP
surface differs.

**Future (not v1): `expose: mixed`** — promote selected hot routes to first-class tools
while the long tail stays behind the dispatcher in the same server.

### Value-expression model (the templating-ready layer)

Every leaf in `path`/`query`/`headers`/`body`/`args`/`stdin`/`env` is a **value
expression**, resolved against `{ input: <call args>, env: <process env> }`:

| Form                      | Meaning                                            |
|---------------------------|----------------------------------------------------|
| `$firstName`, `$a.b.c`    | **InputRef** — dotted lookup into the call's args  |
| `${ENV_VAR}`              | **Env** — environment variable                     |
| `{{ ... }}`               | **Template** — minijinja over `input`/`env`        |
| `{userId}` (in `path`)    | path-var sugar, equivalent to `$userId`            |
| anything else             | **Literal** (YAML keeps native type)               |

Internally this is one `enum ValueExpr { InputRef, Template, Literal }` resolved by a
single pipeline — so "anticipating templates" is satisfied structurally, and basic
`{{ }}` templating works in v1. Richer template features (filters, conditionals across
fields) extend the `Template` variant without touching callers.

> **Implemented refinement (T6):** there is no `Env` variant in the engine. `${ENV}` is
> resolved at **load time** (T4) across every value leaf, so by the time the engine runs
> no `${...}` remains and the resolution context is just `input`. A `${ENV}` inside a
> `{{ ... }}` template is therefore substituted *before* the template renders. The engine
> `ValueExpr` is `{ InputRef, Template, Literal }`.

### Nested body builder

`body` keys are **dotted target paths**; values are value expressions. Keys sharing a
prefix merge into nested objects (`user.name.first` + `user.name.last` →
`{user:{name:{first,last}}}`). Optional `body_from: $ref` sends a referenced value as
the entire body (passthrough). Default content type `application/json`
(`form`/`raw` deferred).

### Response filtering

`response.include` keeps only listed dotted paths; `response.exclude` drops them;
neither → whole response. Returned to the harness as JSON text content (and optionally
`structuredContent`). Non-2xx HTTP / non-zero CLI exit → `isError: true` result whose
body is still filtered, so the model can react.

### Auth

`bearer` (token), `basic` (user/pass), `api_key` (header or query placement), and
`oauth2` client-credentials (token fetched from `token_url`, cached, refreshed on
expiry/401). Secrets come via `${ENV}`; resolved tokens are **never logged** (redacted).

## Code Style

```rust
/// Resolve a single value expression against the active call context.
fn resolve(expr: &ValueExpr, ctx: &Ctx<'_>) -> Result<Value, EngineError> {
    match expr {
        ValueExpr::Literal(v) => Ok(v.clone()),
        ValueExpr::InputRef(path) => ctx
            .input
            .get_path(path)
            .cloned()
            .ok_or_else(|| EngineError::MissingInput(path.clone())),
        ValueExpr::Env(name) => std::env::var(name)
            .map(Value::String)
            .map_err(|_| EngineError::MissingEnv(name.clone())),
        ValueExpr::Template(t) => t.render(ctx).map(Value::String),
    }
}
```

- `snake_case` items, `CamelCase` types; modules small and single-purpose.
- Errors are typed (`thiserror`) at module boundaries; `anyhow` only at the binary edge.
- No `unwrap()`/`expect()` on runtime paths; secrets redacted in `Debug`/logs.
- Public engine functions are pure where possible (config + input → request) to keep
  them unit-testable without I/O.

## Testing Strategy

- **Unit:** value resolution, dotted nested-body building, path-var substitution,
  `${ENV}` interpolation, include/exclude filtering, auth header construction,
  oauth2 token cache expiry logic.
- **Integration (HTTP):** `wiremock` mock server asserts lattice emits the correct
  method/url/headers/body and filters the response correctly.
- **Integration (CLI):** drive real `echo`/test scripts; assert argv/stdin/env/parse.
- **MCP roundtrip:** in-process `rmcp` client over an in-memory duplex transport calls
  `tools/list` (schema verbatim) and `tools/call` (end-to-end translation).
- **Coverage:** core engine modules (`value`, `request`, `body`, `response`, `auth`)
  ~90%; meaningful coverage elsewhere. Tests live in `tests/` + `#[cfg(test)]` modules.

## Boundaries

- **Always:** log to **stderr** only in stdio mode; redact secrets; validate config in
  `check` before serving; run `cargo test` + `clippy -D warnings` before commits.
- **Ask first:** adding dependencies beyond those listed; changing the config schema
  shape after Plan is approved; adding a new transport or auth type; binding the HTTP
  listener to a non-loopback address by default.
- **Never:** write secrets to the config or logs; print non-protocol bytes to stdout in
  stdio mode; execute CLI commands not declared in the config; commit/push without an
  explicit request.

## Success Criteria

1. `lattice check --config examples/httpbin.yaml` validates config + required env and
   reports actionable errors (missing env, bad schema, unknown value ref).
2. Spawned over stdio, an MCP client receives `tools/list` with the **verbatim**
   authored JSON Schemas.
3. A `tools/call` to an HTTP tool: flat input → nested body, `{userId}` path var filled,
   auth applied, response filtered to the configured include/exclude set.
4. The same works for a CLI tool (argv/stdin/env, `parse: json` filtered).
5. OAuth2 client-credentials: token fetched once, cached, auto-refreshed after
   expiry/401 — verified against a mock token endpoint.
6. `--http 127.0.0.1:8080` serves the same tools over Streamable HTTP.
6b. With `expose: dispatcher`, `tools/list` returns exactly `describe_route` +
   `call_route`, the server `instructions` contain the route catalog, and a
   `call_route` invocation translates + executes identically to `tools` mode.
7. Non-2xx / non-zero exit returns an `isError` result with the (filtered) body.
8. `cargo test` and `cargo clippy -- -D warnings` are green.

## Open Questions

1. **Value sigils** — confirm `$ref` / `${ENV}` / `{{ tpl }}` / `{path}`-sugar. Low
   stakes, easy to change; flagging since it's user-facing config syntax.
2. **Result shape** — JSON text content only, or also populate MCP `structuredContent`?
   (Lean: text by default, `structuredContent` opt-in per tool.)
3. **HTTP listener auth** — should the `--http` listener require a bearer token by
   default, or bind loopback + unauthenticated for v1? (Lean: loopback + unauth in v1,
   document clearly.)
4. ~~`tools/view`~~ **Resolved:** no custom method (invisible to standard clients).
   Instead, config-driven `expose: tools | dispatcher` (see *Expose modes*).
   `expose: dispatcher` exposes `describe_route` + `call_route` with an auto-generated
   embedded catalog. `mixed` mode deferred past v1.
5. **CLI safety** — exec via direct argv (no shell) is the default; do we ever need a
   shell mode? (Lean: never; argv-only avoids injection.)
```
