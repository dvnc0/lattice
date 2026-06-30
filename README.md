# Lattice

**Turn existing REST APIs and CLI tools into [Model Context Protocol](https://modelcontextprotocol.io) servers from a single YAML/JSON config — no bespoke code.**

Lattice is a config-driven MCP *shell*. You describe an API (or a set of local
commands) declaratively; Lattice exposes each as an MCP tool, translates the model's
tool call into an HTTP request or a CLI invocation, runs it, filters the response, and
hands the result back. Built on the official [`rmcp`](https://crates.io/crates/rmcp)
crate.

```yaml
# weather.yaml
server:
  name: weather
defaults:
  base_url: "https://api.weather.example"
tools:
  - name: forecast
    description: "Get the forecast for a city."
    inputSchema:
      type: object
      properties:
        city: { type: string }
      required: ["city"]
    http:
      method: GET
      path: "/forecast"
      query:
        q: "$city"
      response:
        include: ["temp_c", "summary"]
```

```console
$ lattice --config weather.yaml      # serve over stdio; point an MCP client at it
```

That config is a complete MCP server. No glue code, no SDK calls.

---

## Why

A standard MCP integration means writing (and maintaining) a server per API: argument
schemas, request building, auth, response shaping, error mapping, transport. Lattice
makes that a config concern. The same declarative file works for any REST API or any
CLI tool, and the translation engine — value expressions, nested bodies, response
filters — is identical across both.

---

## Install

Lattice is a Rust binary. With a recent stable toolchain:

```console
$ git clone <repo> && cd lattice
$ cargo build --release
$ ./target/release/lattice --help
```

---

## Quickstart

1. **Write a config** (YAML or JSON — the schema is identical; the format is chosen by
   extension). Start from one of the [`examples/`](examples/).

2. **Validate it** without starting a server:

   ```console
   $ lattice check --config examples/httpbin.yaml
   examples/httpbin.yaml: 3 tool(s), expose = Tools
   OK
   ```

   `check` parses the config, resolves `${ENV}` references (reporting any that are
   unset), enforces exactly-one-of `http`/`cli`, and compiles every `inputSchema` as
   JSON Schema. It exits non-zero on any error — wire it into CI.

3. **Serve it.** Over stdio (the default MCP transport):

   ```console
   $ lattice --config examples/httpbin.yaml
   ```

   or over [Streamable HTTP](#transports):

   ```console
   $ lattice --config examples/httpbin.yaml --http 127.0.0.1:8080
   ```

4. **Point an MCP client at it** — e.g. Claude Code, or any harness that speaks MCP.
   For stdio, configure the client to launch `lattice --config <file>`.

---

## How a tool call flows

```
model tool call
   │  arguments (JSON)
   ▼
validate against inputSchema ──► reject with an error result if invalid (nothing runs)
   │
   ▼
value expressions resolve against the input  ($ref, {{ template }}, {path}, literals)
   │
   ▼
build an HTTP request  or  an argv-only CLI command
   │
   ▼
execute (reqwest / tokio::process)  ──► non-2xx / non-zero exit becomes an error result
   │
   ▼
response filter (include / exclude dotted paths)
   │
   ▼
CallToolResult  (text + structuredContent for objects)
```

Everything before "execute" is pure and unit-tested; the executor adds I/O, auth,
timeouts, and size caps.

---

## Config at a glance

A config is one `server`, optional `defaults`, and a list of `tools`. Each tool
declares **exactly one** of `http` or `cli`.

```yaml
server:
  name: example-api
  version: "0.1.0"
  expose: tools          # tools (default) | dispatcher

defaults:                # merged into every HTTP tool
  base_url: "https://api.example.com"
  headers: { Accept: "application/json" }
  auth: { type: bearer, token: "${API_TOKEN}" }

tools:
  - name: update_user
    description: "Update a user's profile."
    inputSchema:         # authored by you, passed verbatim to tools/list
      type: object
      properties:
        userId: { type: string }
        firstName: { type: string }
      required: ["userId", "firstName"]
    http:
      method: POST
      path: "/user/{userId}/update"     # {userId} filled from input
      body:
        user.name.first: "$firstName"   # dotted keys build a nested body
      response:
        include: ["id", "user.name"]    # trim the response
```

The **full schema reference** — every field, every option — is in
[`docs/config-reference.md`](docs/config-reference.md).

---

## Value expressions

Every leaf in `path`, `query`, `headers`, `body`, `args`, `stdin`, `env`, and `cwd` is
a *value expression*, resolved against the call's `input` (the model's arguments) and
the process environment:

| Form | Meaning | Example |
|------|---------|---------|
| `$name`, `$a.b.c` | **Input reference** — dotted lookup into the call's arguments | `"$firstName"`, `"$user.address.city"` |
| `${VAR}` | **Environment variable**, resolved at load time anywhere in the config | `"${API_TOKEN}"` |
| `{{ ... }}` | **Template** ([minijinja](https://crates.io/crates/minijinja)) over `input`, rendered to a string | `"{{ input.first }} {{ input.last }}"` |
| `{name}` (in `path`) | Path-variable sugar, equivalent to `$name` | `"/user/{userId}"` |
| anything else | **Literal**, keeping its native JSON/YAML type | `"lattice"`, `42`, `true` |

Body keys are **dotted target paths**: `user.name.first` + `user.name.last` build
`{ "user": { "name": { "first": …, "last": … } } }`. Use `body_from: "$payload"` to
send a single value as the entire body.

---

## Expose modes

`server.expose` controls how routes appear in `tools/list`:

- **`tools`** (default) — every tool is a first-class MCP tool with its `inputSchema`
  in `tools/list`. Best for small APIs; maximum up-front validation by the client.
- **`dispatcher`** — for large APIs, `tools/list` shows just **`describe_route`** and
  **`call_route`**, and a lightweight route catalog (names + one-line descriptions, no
  schemas) is embedded into the server `instructions`. The model reads the catalog →
  optionally `describe_route(name)` for the exact parameters → `call_route(name,
  params)`. Lattice auto-generates the instructions (override with
  `server.instructions`).

The translation engine is identical in both modes; only the MCP surface differs. See
[`examples/github.yaml`](examples/github.yaml) for a dispatcher config.

---

## Auth

HTTP tools support four auth schemes, set under `defaults.auth` (applied to every
tool) or a tool's `http.auth` (per-tool override). Selected by a `type` discriminator:

```yaml
auth: { type: bearer, token: "${API_TOKEN}" }
auth: { type: basic, username: "${USER}", password: "${PASS}" }
auth: { type: api_key, in: header, name: "X-API-Key", value: "${API_KEY}" }
auth:
  type: oauth2                       # client-credentials grant
  token_url: "https://auth.example.com/token"
  client_id: "${CLIENT_ID}"
  client_secret: "${CLIENT_SECRET}"
  scopes: ["read", "write"]
```

For `oauth2`, Lattice fetches the token, caches it (with an expiry margin), serves
concurrent callers from a single in-flight fetch, and refreshes once on a `401`.

> **Put credentials in the `auth` block, sourced from `${ENV}`.** The `auth` block's
> secret fields are redacted from logs; a secret placed directly in a header/query/body
> value is *not*. **Never commit a literal secret** to a config file.

---

## Transports

- **stdio** (default) — `lattice --config <file>`. The JSON-RPC stream owns stdout;
  all logs go to stderr (there is a test guarding stdout purity).
- **Streamable HTTP** — `lattice --config <file> --http 127.0.0.1:8080`. Mounted at
  `/mcp`. By default the server validates the inbound `Host` header against **loopback
  only** (DNS-rebinding protection), so it is safe for local development out of the box.

  > **Production HTTP** needs more than the default: terminate TLS and add
  > authentication in front of Lattice (e.g. a reverse proxy), and only then widen the
  > allowed hosts. Do not expose a Lattice HTTP endpoint directly to the internet.

---

## Security model

- **CLI execution is argv-only** — the `command` is a literal from config (model input
  can never choose which binary runs), and each argument is a distinct argv entry.
  There is no shell, so there is no shell-injection surface.
- **Input is validated** against the tool's `inputSchema` before anything runs; an
  invalid call is rejected with an error result and never reaches the API or shell.
  Regex `pattern` matching is backtrack-bounded (no ReDoS hang on hostile input).
- **Secrets** come from `${ENV}` interpolation and belong in the `auth` block; they are
  never written to a config file and never logged.
- **URL/header safety** — path variables are percent-encoded, header values reject
  CRLF/control characters, query parameters are percent-encoded.
- **Bounded I/O** — per-request timeouts and a 10 MiB response cap on both HTTP bodies
  and CLI output; HTTP redirects are disabled (a hostile upstream can't 302 to an
  internal host).
- **Tool failures are results, not crashes** — a non-2xx status or non-zero exit comes
  back as `CallToolResult { is_error: true }` (with the filtered body) so the model can
  react, rather than a transport/protocol error.

---

## Examples

| File | Shows |
|------|-------|
| [`examples/httpbin.yaml`](examples/httpbin.yaml) | HTTP tools-mode: query params, path var, nested body, a template, response filtering |
| [`examples/github.yaml`](examples/github.yaml) | Dispatcher mode over a real public API, shared defaults |
| [`examples/ls.yaml`](examples/ls.yaml) | CLI tools: argv args, stdin, `parse` modes |

All three pass `lattice check`.

---

## Development

| Task | Command |
|------|---------|
| Build | `cargo build` |
| Test | `cargo test` |
| Lint | `cargo clippy --all-targets -- -D warnings` |
| Format | `cargo fmt` |
| Audit deps | `cargo audit` |
| Validate a config | `cargo run -- check --config <file>` |

Design docs live alongside the code: `SPEC.md` (what and why), `PLAN.md` (components and
build order), `TASKS.md` (status), and `docs/config-reference.md` (the full schema).