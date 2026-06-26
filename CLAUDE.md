# Lattice — agent guide

Lattice is a **config-driven MCP shell**: it turns existing REST APIs and CLI tools
into Model Context Protocol servers from a single YAML/JSON config, with no bespoke
code. Built on the official [`rmcp`](https://crates.io/crates/rmcp) crate.

Planning docs are the source of truth — read them before non-trivial work:
- `SPEC.md` — what we're building and why (design, config model, expose modes).
- `PLAN.md` — components, build order, risks.
- `TASKS.md` — the dependency-ordered task breakdown **and current status**.
- `docs/config-reference.md` — every config option (the user-facing schema reference).

## Pre-commit workflow (required)

Before **every** commit:

1. **Review the diff with two subagents** and address their findings:
   - `security-and-hardening` skill — vulnerabilities / hardening.
   - `code-review-and-quality` skill — correctness, design, idiom, tests.
2. **Update `TASKS.md`** — check off completed tasks (`[ ]` → `[x]`) and reflect any
   scope changes.
3. **Green the gates:** `cargo test`, `cargo clippy --all-targets -- -D warnings`,
   `cargo fmt --check`, and `cargo audit` (dependency CVE scan) must all pass.

Only commit once 1–3 are done. Commit/push **only when the user asks**.

## Invariants (do not break)

- **stdout is the JSON-RPC channel** in stdio mode — never `println!` or otherwise
  write to stdout. All logs go to **stderr** (`init_tracing` in `src/main.rs` wires
  this up). There is a stdout-purity test guarding this.
- **Secrets** come from `${ENV}` interpolation and are **never** written to a config
  file or logged — redact in `Debug`/logs.
- **CLI execution is argv-only** — no shell interpolation (injection-safe).
- **Tool failures** surface as `CallToolResult { is_error: true }` so the model can
  react, not as transport/protocol errors.

## Commands

| Task | Command |
|------|---------|
| Build | `cargo build` |
| Test | `cargo test` |
| Lint | `cargo clippy --all-targets -- -D warnings` |
| Format | `cargo fmt` |
| Run (stdio) | `cargo run -- --config <file>` |
| Validate config | `cargo run -- check --config <file>` |

## Layout

- `src/mcp/` — `rmcp` `ServerHandler` (the MCP surface: `tools/list`, `tools/call`).
- `src/config/` — config model, loading, `${ENV}` interpolation (tasks T3–T5).
- `src/engine/` — **pure** translation: value expressions, nested body, HTTP request,
  CLI command, response filtering (tasks T6–T10).
- `src/exec/` — HTTP/CLI execution + auth, incl. OAuth2 (tasks T11–T13).
- `tests/` — integration + in-process MCP roundtrip tests.
