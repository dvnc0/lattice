# Lattice Config Reference

A lattice config file describes **one MCP server** and the **tools** it exposes. The
file may be **YAML or JSON** — the format is chosen by extension (`.yaml`/`.yml` vs
`.json`) and the schema is identical. Examples below are JSON; the YAML equivalent
uses the same keys.

```
lattice --config tools.yaml            # serve over stdio
lattice --config tools.json --http 127.0.0.1:8080   # serve over HTTP instead of stdio
lattice check --config tools.yaml      # validate without serving
```

> **Implementation status.** The full pipeline is implemented (T3–T18): parse/validate,
> `${ENV}`, the engine, HTTP/CLI execution with auth, both expose modes, and the stdio
> and Streamable HTTP transports. See the [status table](#implementation-status) at the
> end.

---

## Top-level structure

```json
{
  "server":   { "...": "identity + MCP surface" },
  "defaults": { "...": "shared HTTP settings (optional)" },
  "tools":    [ { "...": "one per exposed tool" } ]
}
```

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `server` | object | **yes** | Server identity and expose mode. |
| `defaults` | object | no | Settings inherited by every HTTP tool. |
| `tools` | array | no | The tools this server exposes (may be empty). |

Unknown top-level keys are rejected (every object uses strict field checking, so typos
like `tool:` instead of `tools:` fail fast).

---

## `server`

```json
"server": {
  "name": "example-api",
  "version": "0.1.0",
  "instructions": "Wraps the Example API.",
  "expose": "tools"
}
```

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `name` | string | **yes** | — | Server name reported to the harness. |
| `version` | string | no | — | Optional server version. |
| `instructions` | string | no | — | Human-readable usage notes surfaced at MCP init. In `dispatcher` mode lattice auto-generates these if omitted. |
| `expose` | enum | no | `tools` | `tools` or `dispatcher` — see [Expose modes](#expose-modes). |

---

## `defaults`

Shared settings merged into every **HTTP** tool. CLI tools ignore `defaults`.

```json
"defaults": {
  "base_url": "https://api.example.com",
  "headers": { "Accept": "application/json" },
  "auth": { "type": "bearer", "token": "${API_TOKEN}" }
}
```

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `base_url` | string | no | Prefixed to each HTTP tool's `path` unless the tool sets its own `base_url`. |
| `headers` | map<string,value> | no | Applied to every request; a tool's own header with the same key **overrides** the default. |
| `auth` | object | no | Applied to every request unless the tool sets its own `auth`. See [Auth](#auth). |

**Merge rules:** a tool's `base_url` and `auth` are used when present, otherwise the
default fills in. Headers merge per-key, with the tool winning ties.

---

## Auth

Selected by a `type` discriminator. Used under `defaults.auth` or a tool's `http.auth`.
Secret-bearing values should always come from the environment via `${ENV}`. **Prefer the
`auth` block for credentials** — its secret fields are redacted from logs, whereas a
secret placed directly in a header/query/body value is not. Never put a literal secret
in the config file.

### `bearer`
```json
{ "type": "bearer", "token": "${API_TOKEN}" }
```
Sends `Authorization: Bearer <token>`.

### `basic`
```json
{ "type": "basic", "username": "${USER}", "password": "${PASS}" }
```
HTTP Basic auth.

### `api_key`
```json
{ "type": "api_key", "in": "header", "name": "X-API-Key", "value": "${API_KEY}" }
```
| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `in` | enum | no | `header` | `header` or `query` — where to place the key. |
| `name` | string | **yes** | — | Header or query-parameter name. |
| `value` | string | **yes** | — | The key (use `${ENV}`). |

### `oauth2`
```json
{
  "type": "oauth2",
  "token_url": "https://auth.example.com/token",
  "client_id": "${CLIENT_ID}",
  "client_secret": "${CLIENT_SECRET}",
  "scopes": ["read", "write"]
}
```
OAuth2 **client-credentials** grant. lattice fetches the token, caches it, and refreshes
on expiry/401.
| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `token_url` | string | **yes** | Token endpoint. |
| `client_id` | string | **yes** | Client identifier. |
| `client_secret` | string | **yes** | Client secret (use `${ENV}`). |
| `scopes` | string[] | no | Requested scopes. |

---

## `tools`

Each tool is one MCP tool. It must declare **exactly one** of `http` or `cli`.

```json
{
  "name": "update_user",
  "description": "Update a user's profile.",
  "inputSchema": { "type": "object", "properties": { "...": {} } },
  "http": { "...": {} }
}
```

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `name` | string | **yes** | Tool name shown to the harness. |
| `description` | string | no | What the tool does / when to use it. |
| `inputSchema` | JSON Schema | no | Hand-written JSON Schema, passed **verbatim** to `tools/list`. This is what the harness validates the model's arguments against. |
| `http` | object | one of | HTTP backing — see [`http`](#tool-http). |
| `cli` | object | one of | CLI backing — see [`cli`](#tool-cli). |

> `inputSchema` is authored by you (full control over types, enums, `required`, nested
> objects). Lattice does not generate it.

---

## Tool `http`

```json
"http": {
  "method": "POST",
  "path": "/user/{userId}/update",
  "base_url": "https://api.example.com",
  "query":   { "notify": "true" },
  "headers": { "X-Source": "lattice" },
  "body": {
    "user.name.first": "$firstName",
    "user.name.last":  "$lastName",
    "source": "lattice"
  },
  "response": { "include": ["id", "user.name", "updatedAt"] }
}
```

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `method` | string | **yes** | — | HTTP method (`GET`, `POST`, …). |
| `path` | string | **yes** | — | Request path; `{var}` placeholders are filled from input. Joined onto `base_url`. |
| `base_url` | string | no | from `defaults` | Per-tool base URL override. |
| `query` | map<string,value> | no | — | Query parameters: name → [value expression](#value-expressions). |
| `headers` | map<string,value> | no | merged w/ `defaults` | Headers: name → value expression. |
| `body` | map<string,value> | no | — | Request body: **dotted target path** → value expression. See [Nested bodies](#nested-bodies). |
| `body_from` | value | no | — | Send a single referenced value as the **entire** body (instead of `body`). |
| `auth` | object | no | from `defaults` | Per-tool [auth](#auth) override. |
| `response` | object | no | — | [Response filtering](#response-filtering). |

Default body content type is `application/json`.

---

## Tool `cli`

```json
"cli": {
  "command": "ls",
  "args": ["-la", "$dir"],
  "stdin": "$payload",
  "env": { "TOKEN": "${TOKEN}" },
  "cwd": "$workdir",
  "parse": "json",
  "response": { "exclude": ["permissions"] }
}
```

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `command` | string | **yes** | — | Program to run. Executed **directly (argv), never via a shell** — no shell injection. |
| `args` | value[] | no | — | Ordered argument list of [value expressions](#value-expressions). |
| `stdin` | value | no | — | Value piped to the command's standard input. |
| `env` | map<string,value> | no | — | Extra environment variables: name → value expression. |
| `cwd` | value | no | — | Working directory. |
| `parse` | enum | no | `raw` | How to interpret stdout: `raw` (text), `json` (parse one JSON value), `lines` (array of lines). |
| `response` | object | no | — | [Response filtering](#response-filtering) (applies when `parse` yields JSON). |

---

## Value expressions

Every leaf in `path`, `query`, `headers`, `body`, `args`, `stdin`, `env`, and `cwd` is a
**value expression**, resolved against the call's `input` (the arguments the model sent)
and the process `env`:

| Form | Meaning | Example |
|------|---------|---------|
| `$name`, `$a.b.c` | **Input reference** — dotted lookup into the call's arguments | `"$firstName"`, `"$user.address.city"` |
| `${VAR}` | **Environment variable** (resolved at load time, anywhere in the config) | `"${API_TOKEN}"` |
| `{{ ... }}` | **Template** (minijinja) over `input` | `"{{ input.first }} {{ input.last }}"` |
| `{name}` (in `path`) | Path-var sugar, equivalent to `$name` | `"/user/{userId}"` |
| anything else | **Literal** (keeps its native JSON/YAML type) | `"lattice"`, `42`, `true` |

So a model that calls a tool with `{ "firstName": "Bob" }` can populate a nested API
body field `user.name.first` via `"user.name.first": "$firstName"`.

---

## Nested bodies

`body` keys are **dotted target paths** describing where each value goes in the request
JSON. Keys that share a prefix merge into nested objects:

```json
"body": {
  "user.name.first": "$firstName",
  "user.name.last":  "$lastName",
  "user.active":     true
}
```
produces
```json
{ "user": { "name": { "first": "Bob", "last": "Lee" }, "active": true } }
```

To send a value as the whole body instead, use `body_from` (e.g. `"body_from": "$payload"`).

---

## Response filtering

Trim what the underlying response returns to the harness. Provide **one** of:

```json
"response": { "include": ["id", "user.name", "updatedAt"] }
```
```json
"response": { "exclude": ["audit", "_internal"] }
```

| Field | Type | Description |
|-------|------|-------------|
| `include` | string[] | Keep **only** these dotted field paths. |
| `exclude` | string[] | Drop these dotted field paths. |

With neither, the full response is returned. A non-2xx HTTP status or non-zero CLI exit
returns an error result (so the model can react) with the body still filtered.

---

## Expose modes

`server.expose` controls how routes appear in `tools/list`:

- **`tools`** (default) — every tool is a first-class MCP tool with its `inputSchema`
  in `tools/list`. Best for small APIs; maximum up-front validation.
- **`dispatcher`** — for large APIs, `tools/list` shows just `describe_route` and
  `call_route`, and the lightweight route catalog (names + descriptions, no schemas)
  is embedded into the server `instructions`. The model reads the catalog →
  optionally `describe_route(name)` for exact params → `call_route(name, params)`.

The translation engine is identical in both modes; only the MCP surface differs.

---

## Full examples

### HTTP tool (nested body, path var, filtering)

```json
{
  "server": { "name": "example-api" },
  "defaults": {
    "base_url": "https://api.example.com",
    "headers": { "Accept": "application/json" },
    "auth": { "type": "bearer", "token": "${API_TOKEN}" }
  },
  "tools": [
    {
      "name": "update_user",
      "description": "Update a user's profile.",
      "inputSchema": {
        "type": "object",
        "properties": {
          "userId":    { "type": "string" },
          "firstName": { "type": "string" },
          "lastName":  { "type": "string" }
        },
        "required": ["userId", "firstName"]
      },
      "http": {
        "method": "POST",
        "path": "/user/{userId}/update",
        "body": {
          "user.name.first": "$firstName",
          "user.name.last":  "$lastName"
        },
        "response": { "include": ["id", "user.name", "updatedAt"] }
      }
    }
  ]
}
```

### CLI tool

```json
{
  "server": { "name": "shell-tools" },
  "tools": [
    {
      "name": "list_dir",
      "description": "List a directory in long format.",
      "inputSchema": {
        "type": "object",
        "properties": { "dir": { "type": "string" } },
        "required": ["dir"]
      },
      "cli": {
        "command": "ls",
        "args": ["-la", "$dir"],
        "parse": "lines"
      }
    }
  ]
}
```

---

## Implementation status

All capabilities are implemented (see `TASKS.md` for the per-task history):

| Capability | Status |
|------------|--------|
| Parse + validate config (YAML/JSON), defaults-merge, exactly-one-of http/cli | ✅ T3 |
| `${ENV}` interpolation | ✅ T4 |
| `check` (env presence, JSON-Schema validity, include/exclude exclusivity, `body`+`body_from` conflict, per-variant auth keys) | ✅ T5 |
| Value expressions (`$ref` / `${ENV}` / `{{ template }}` / `{path}`) | ✅ T6 |
| Nested body building | ✅ T7 |
| HTTP request build + execution | ✅ T8, T11 |
| CLI command build + execution | ✅ T9, T13 |
| Response filtering | ✅ T10 |
| Auth (bearer/basic/api_key) + OAuth2 | ✅ T12 |
| Runtime `inputSchema` validation | ✅ T17 |
| `tools` mode serving | ✅ T14–T15 |
| `dispatcher` mode | ✅ T16 |
| Streamable HTTP transport (`--http`) | ✅ T18 |
