# Plan: Lattice implementation

> Status: **Phase 2 (Plan)** — awaiting human review before Phase 3 (Tasks).
> Derived from SPEC.md. Last updated: 2026-06-25.

## Components & dependency graph

```
Foundations          Engine (pure)             Execution (I/O)        MCP surface
-----------          -------------             ---------------        -----------
C0 scaffold   ─┐
C1 config     ─┼─► C4 value-expr ─┬─► C5 body ─► C6 http-req ─► C9 http-exec ─┐
C2 env-interp ─┘                  ├─► C7 cli-cmd ─────────────► C11 cli-exec ─┼─► C12 server (tools)
C3 check ◄─────────────────────────┘                          C10 auth ◄─────┘     + C13 stdio  (MVP)
                                   └─► C8 response-filter ───────────────────────► C14 dispatcher
                                                                                    C15 http-transport
Polish: C16 examples/README · C17 jsonschema validation
```

| ID | Component | Depends on | Pure? |
|----|-----------|-----------|-------|
| C0 | Cargo scaffold, deps, module skeleton, clap entry, tracing→stderr, error types | — | — |
| C1 | Config serde model + YAML/JSON load + defaults merge | C0 | ✓ |
| C2 | `${ENV}` interpolation over loaded config | C1 | ✓ |
| C3 | `check` mode (parse, env, one-of http/cli, valid JSON Schema, value-ref sanity) | C1,C2 | — |
| C4 | Value expressions: `$ref` / `${ENV}` / `{{tpl}}` / `{path}` / literal | C1 | ✓ |
| C5 | Nested body builder (dotted target paths → nested JSON, `body_from`) | C4 | ✓ |
| C6 | HTTP request builder (method, path-vars, query, headers, body) | C4,C5 | ✓ |
| C7 | CLI command builder (argv/stdin/env/cwd) | C4 | ✓ |
| C8 | Response filter (include/exclude dotted paths, parse raw/json/lines) | C1 | ✓ |
| C9 | HTTP executor (reqwest; status+body; isError on non-2xx) | C6 | — |
| C10| Auth: bearer/basic/api_key + oauth2 client-credentials token cache/refresh | C9 | — |
| C11| CLI executor (tokio::process; stdout/stderr/exit; isError on non-zero) | C7 | — |
| C12| MCP server, tools mode: list_tools (verbatim schema) + call_tool dispatch | C8,C9,C10,C11 | — |
| C13| stdio transport wiring (stdout reserved) | C12 | — |
| C14| Dispatcher mode: describe_route/call_route + auto-gen instructions/catalog | C12,C17 | — |
| C15| Streamable HTTP transport (`--http`, loopback default) | C12 | — |
| C16| Examples (httpbin/github/ls) + README | C13 | — |
| C17| Input validation against authored JSON Schema (`jsonschema`) | C1 | ✓ |

## Build order

- **Phase A — Foundations (sequential):** C0 → C1 → C2 → C3. Checkpoint: `check` loads a fixture config and reports good/bad correctly.
- **Phase 0 spike (de-risk, runs alongside A):** minimal rmcp stdio server with ONE hardcoded tool to pin the rmcp 1.8 API before investing in the engine.
- **Phase B — Engine (parallelizable after C4):** C4 first, then C5/C6/C7/C8 in parallel. Checkpoint: engine unit tests green, zero I/O.
- **Phase C — Execution:** C9 and C11 in parallel; C10 after C9. Checkpoint: wiremock + real-process integration tests.
- **Phase D — MCP surface:** C12+C13 = **MVP milestone** (config-driven HTTP tool callable over stdio). Then C14, then C15.
- **Phase E — Polish:** C16, C17 (C17 also feeds C3 and C14 validation).

## Parallel vs. sequential

- Sequential spine: C0→C1→C4→C6→C9→C12→C13 (the shortest path to a working server).
- Parallel branches once C4 exists: body/request vs. cli vs. response-filter; http-exec vs. cli-exec.
- C10 (oauth) and C15 (http transport) are independent leaves — can be done last in any order.

## Verification checkpoints

1. After C3 — `lattice check` unit + fixture tests pass/fail correctly.
2. After Phase B — engine unit tests (value, body, request, cli, response) green.
3. After C9/C10/C11 — wiremock HTTP + mock oauth token endpoint + real process exec integration tests.
4. **MVP after C12/C13** — in-process rmcp client roundtrip: `tools/list` verbatim schema + `tools/call` end-to-end vs wiremock.
5. After C14 — dispatcher roundtrip: exactly 2 tools, catalog in instructions, `call_route` executes + validates.
6. After C15 — HTTP transport smoke test.

## Risks & mitigations

1. **rmcp 1.8 API shape** (biggest unknown) — spike a hardcoded-tool stdio server first (Phase 0) to pin trait/transport signatures; lock exact version.
2. **stdout contamination in stdio** — tracing→stderr from C0; test asserting stdout is only framed JSON-RPC; ban `println!` in review.
3. **`serde_yaml` deprecated** — use maintained `serde_norway`; verify round-trip early; fallback `serde_yaml_ng`.
4. **OAuth2 lifecycle** (skew/refresh races/401) — client-credentials only; single-flight refresh; expiry safety margin; mock-token tests; sequenced late so MVP doesn't depend on it.
5. **Value-expr ambiguity** — sigils disambiguate; `check` flags `$refs` absent from the schema; documented.
6. **Dispatcher param gap** — no per-route schema in tools/list, so validate params server-side (C17) before executing; clear isError.
7. **Scope creep** (templating/mixed/extra body+auth types) — held by SPEC Boundaries; deferred per Open Questions.

## Recommended first move

A vertical **tracer bullet**: C0 scaffold → minimal rmcp stdio server exposing one hardcoded tool → confirm an rmcp client can `list` + `call` it. Pins the rmcp API (risk #1) before the engine exists, then we backfill config + engine and swap the hardcoded tool for config-driven ones.
