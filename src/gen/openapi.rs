//! OpenAPI 3.0.x document parsing → [`GeneratorInput`].
//!
//! Uses `serde_json::Value` walking so no extra dependency is required.
//! Only `#/components/...` (same-document) `$ref`s are resolved; external refs
//! produce a warning and fall back to `{ "type": "object" }`.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use serde_json::Value;

use crate::config::ApiKeyLocation;

// ── Intermediate representation ──────────────────────────────────────────────

/// Everything the emitter needs, extracted from a raw OpenAPI document.
pub struct GeneratorInput {
    pub title: String,
    pub version: Option<String>,
    pub description: Option<String>,
    pub base_url: Option<String>,
    pub auth: Option<DetectedAuth>,
    pub env_prefix: String,
    pub operations: Vec<OperationInput>,
}

/// A single operation (path + method pair) mapped to a future Lattice tool.
pub struct OperationInput {
    /// Resolved, deduplicated tool name (snake_case).
    pub name: String,
    pub description: Option<String>,
    /// HTTP method in uppercase (e.g. `"GET"`).
    pub method: String,
    /// OpenAPI path string (e.g. `"/pets/{petId}"`).
    pub path: String,
    pub path_params: Vec<ParamInput>,
    pub query_params: Vec<ParamInput>,
    pub body: Option<RequestBodyInput>,
}

/// A single parameter (path or query).
#[derive(Clone)]
pub struct ParamInput {
    pub name: String,
    pub schema: Value,
    pub required: bool,
}

/// The parsed `requestBody` for an operation.
pub struct RequestBodyInput {
    pub required: bool,
    pub kind: RequestBodyKind,
}

pub enum RequestBodyKind {
    /// Root is a plain object — map fields individually.
    FlatObject { properties: Vec<ParamInput> },
    /// Root is not a plain object (array, `oneOf`, etc.) — use `body_from` passthrough.
    Passthrough { schema: Value },
}

/// An auth scheme detected from the spec's `components.securitySchemes`.
pub enum DetectedAuth {
    Bearer {
        env_prefix: String,
    },
    Basic {
        env_prefix: String,
    },
    ApiKey {
        location: ApiKeyLocation,
        param_name: String,
        env_prefix: String,
    },
    Oauth2 {
        token_url: String,
        env_prefix: String,
        scopes: Vec<String>,
    },
}

// ── Parse errors ─────────────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error("failed to read spec file {path}: {source}")]
    Read {
        path: String,
        source: std::io::Error,
    },
    #[error("unsupported spec extension for {path} (use .yaml, .yml, or .json)")]
    UnknownFormat { path: String },
    #[error("YAML parse error: {0}")]
    Yaml(#[from] serde_norway::Error),
    #[error("JSON parse error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("spec is missing required field `info.title`")]
    MissingTitle,
}

// ── Public entry point ────────────────────────────────────────────────────────

/// Parse an OpenAPI 3.0.x spec file and return a [`GeneratorInput`] plus any
/// non-fatal warnings.
pub fn parse(path: &Path) -> Result<(GeneratorInput, Vec<String>), ParseError> {
    let doc = load_document(path)?;
    let mut warnings = Vec::new();

    let title = extract_title(&doc)?;
    let version = extract_str(&doc, &["info", "version"]).map(str::to_owned);
    let description = extract_str(&doc, &["info", "description"]).map(|s| {
        let s = s.trim();
        if s.len() > 500 {
            s[..500].to_owned()
        } else {
            s.to_owned()
        }
    });
    let base_url = extract_base_url(&doc, &mut warnings);
    let env_prefix = make_env_prefix(&title);
    let auth = extract_auth(&doc, &env_prefix, &mut warnings);
    let operations = extract_operations(&doc, &mut warnings);

    Ok((
        GeneratorInput {
            title,
            version,
            description,
            base_url,
            auth,
            env_prefix,
            operations,
        },
        warnings,
    ))
}

// ── Document loading ──────────────────────────────────────────────────────────

fn load_document(path: &Path) -> Result<Value, ParseError> {
    let text = std::fs::read_to_string(path).map_err(|source| ParseError::Read {
        path: path.display().to_string(),
        source,
    })?;
    match path.extension().and_then(|e| e.to_str()) {
        Some("yaml") | Some("yml") => Ok(serde_norway::from_str(&text)?),
        Some("json") => Ok(serde_json::from_str(&text)?),
        _ => Err(ParseError::UnknownFormat {
            path: path.display().to_string(),
        }),
    }
}

// ── Metadata extraction ───────────────────────────────────────────────────────

fn extract_title(doc: &Value) -> Result<String, ParseError> {
    extract_str(doc, &["info", "title"])
        .map(str::to_owned)
        .ok_or(ParseError::MissingTitle)
}

fn extract_base_url(doc: &Value, warnings: &mut Vec<String>) -> Option<String> {
    // OpenAPI 3.0: servers[0].url
    if let Some(servers) = doc.get("servers").and_then(Value::as_array) {
        if servers.len() > 1 {
            warnings.push(format!(
                "spec declares {} servers; using servers[0] as base_url",
                servers.len()
            ));
        }
        if let Some(url) = servers.first().and_then(|s| s.get("url")).and_then(Value::as_str) {
            return Some(url.trim_end_matches('/').to_owned());
        }
    }

    // Swagger 2.0: schemes[0] + "://" + host + basePath
    let host = doc.get("host").and_then(Value::as_str)?;
    let base_path = doc.get("basePath").and_then(Value::as_str).unwrap_or("");
    let scheme = doc
        .get("schemes")
        .and_then(Value::as_array)
        .and_then(|arr| arr.first())
        .and_then(Value::as_str)
        .unwrap_or("https");
    Some(format!(
        "{scheme}://{host}{}",
        base_path.trim_end_matches('/')
    ))
}

/// Derive a SCREAMING_SNAKE env prefix from the API title.
pub fn make_env_prefix(title: &str) -> String {
    let upper = title.to_uppercase();
    let mut out = String::with_capacity(upper.len());
    let mut prev_underscore = true;
    for ch in upper.chars() {
        if ch.is_alphanumeric() {
            out.push(ch);
            prev_underscore = false;
        } else if !prev_underscore {
            out.push('_');
            prev_underscore = true;
        }
    }
    out.trim_end_matches('_').to_owned()
}

// ── Auth extraction ───────────────────────────────────────────────────────────

/// Extract the first usable auth scheme from `components.securitySchemes` (OAS 3.0)
/// or `securityDefinitions` (Swagger 2.0).
pub fn extract_auth(
    doc: &Value,
    env_prefix: &str,
    warnings: &mut Vec<String>,
) -> Option<DetectedAuth> {
    let schemes = doc
        .get("components")
        .and_then(|c| c.get("securitySchemes"))
        .and_then(Value::as_object)
        .or_else(|| doc.get("securityDefinitions").and_then(Value::as_object))?;

    let mut first: Option<DetectedAuth> = None;
    let mut skipped: Vec<String> = Vec::new();

    for (name, scheme) in schemes {
        match try_parse_auth_scheme(scheme, env_prefix) {
            Some(auth) if first.is_none() => first = Some(auth),
            Some(_) => skipped.push(name.clone()),
            None => {
                let kind = scheme
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown");
                let flow = scheme
                    .get("flows")
                    .and_then(|f| f.as_object())
                    .map(|f| f.keys().map(String::as_str).collect::<Vec<_>>().join(", "))
                    .unwrap_or_default();
                if flow.is_empty() {
                    warnings.push(format!(
                        "skipping unsupported security scheme '{name}' (type: {kind})"
                    ));
                } else {
                    warnings.push(format!(
                        "skipping unsupported security scheme '{name}' (type: {kind}, flows: {flow})"
                    ));
                }
            }
        }
    }

    if !skipped.is_empty() {
        warnings.push(format!(
            "multiple supported auth schemes found; using first, skipping: {}",
            skipped.join(", ")
        ));
    }

    first
}

fn try_parse_auth_scheme(scheme: &Value, env_prefix: &str) -> Option<DetectedAuth> {
    let kind = scheme.get("type").and_then(Value::as_str)?;
    match kind {
        "http" => {
            let scheme_name = scheme.get("scheme").and_then(Value::as_str)?;
            match scheme_name {
                "bearer" => Some(DetectedAuth::Bearer {
                    env_prefix: env_prefix.to_owned(),
                }),
                "basic" => Some(DetectedAuth::Basic {
                    env_prefix: env_prefix.to_owned(),
                }),
                _ => None,
            }
        }
        "apiKey" => {
            let param_name = scheme.get("name").and_then(Value::as_str)?.to_owned();
            let location = match scheme.get("in").and_then(Value::as_str)? {
                "header" => ApiKeyLocation::Header,
                "query" => ApiKeyLocation::Query,
                _ => return None,
            };
            Some(DetectedAuth::ApiKey {
                location,
                param_name,
                env_prefix: env_prefix.to_owned(),
            })
        }
        "oauth2" => {
            let flows = scheme.get("flows")?.as_object()?;
            let cc = flows.get("clientCredentials")?;
            let token_url = cc.get("tokenUrl").and_then(Value::as_str)?.to_owned();
            let scopes = cc
                .get("scopes")
                .and_then(Value::as_object)
                .map(|s| s.keys().cloned().collect())
                .unwrap_or_default();
            Some(DetectedAuth::Oauth2 {
                token_url,
                env_prefix: env_prefix.to_owned(),
                scopes,
            })
        }
        _ => None,
    }
}

// ── $ref resolution ───────────────────────────────────────────────────────────

/// Resolve a `$ref` value within the document, following chains recursively.
/// Cycles and external refs produce a warning and return `{ "type": "object" }`.
fn resolve_ref<'a>(
    doc: &'a Value,
    reference: &str,
    seen: &mut HashSet<String>,
    warnings: &mut Vec<String>,
) -> std::borrow::Cow<'a, Value> {
    if !reference.starts_with("#/") {
        warnings.push(format!(
            "external $ref '{reference}' is not supported; substituting {{\"type\":\"object\"}}"
        ));
        return std::borrow::Cow::Owned(object_schema());
    }
    if !seen.insert(reference.to_owned()) {
        warnings.push(format!(
            "circular $ref '{reference}' detected; substituting {{\"type\":\"object\"}}"
        ));
        return std::borrow::Cow::Owned(object_schema());
    }

    let resolved = follow_json_pointer(doc, &reference[1..]);
    match resolved {
        None => {
            warnings.push(format!(
                "unresolvable $ref '{reference}'; substituting {{\"type\":\"object\"}}"
            ));
            std::borrow::Cow::Owned(object_schema())
        }
        Some(target) => {
            if let Some(next_ref) = target.get("$ref").and_then(Value::as_str) {
                let next_ref = next_ref.to_owned();
                resolve_ref(doc, &next_ref, seen, warnings)
            } else {
                std::borrow::Cow::Borrowed(target)
            }
        }
    }
}

/// Walk a JSON Pointer path (e.g. `/components/schemas/Foo`) through `doc`.
fn follow_json_pointer<'a>(doc: &'a Value, pointer: &str) -> Option<&'a Value> {
    let mut current = doc;
    for segment in pointer.split('/').filter(|s| !s.is_empty()) {
        let key = segment.replace("~1", "/").replace("~0", "~");
        current = current.get(&key)?;
    }
    Some(current)
}

/// Resolve a schema value: if it contains `$ref`, follow it; otherwise return it as-is.
fn resolve_schema<'a>(
    doc: &'a Value,
    schema: &'a Value,
    seen: &mut HashSet<String>,
    warnings: &mut Vec<String>,
) -> std::borrow::Cow<'a, Value> {
    if let Some(ref_str) = schema.get("$ref").and_then(Value::as_str) {
        let ref_str = ref_str.to_owned();
        resolve_ref(doc, &ref_str, seen, warnings)
    } else {
        std::borrow::Cow::Borrowed(schema)
    }
}

fn object_schema() -> Value {
    serde_json::json!({ "type": "object" })
}

// ── Operation extraction ──────────────────────────────────────────────────────

pub fn extract_operations(doc: &Value, warnings: &mut Vec<String>) -> Vec<OperationInput> {
    let Some(paths) = doc.get("paths").and_then(Value::as_object) else {
        return Vec::new();
    };

    const METHODS: &[&str] = &["get", "post", "put", "patch", "delete", "head", "options"];

    let mut raw: Vec<(String, OperationInput)> = Vec::new();

    for (path, path_item) in paths {
        // Collect path-level parameters (may be overridden per-operation).
        let path_level_params = path_item.get("parameters");

        for method in METHODS {
            let Some(op) = path_item.get(*method) else {
                continue;
            };

            let raw_name = op
                .get("operationId")
                .and_then(Value::as_str)
                .map(to_snake_case)
                .unwrap_or_else(|| fallback_name(method, path));

            let description = op
                .get("summary")
                .or_else(|| op.get("description"))
                .and_then(Value::as_str)
                .map(str::to_owned);

            // Merge path-level + operation-level parameters (op wins on same name+in).
            let merged_params =
                merge_parameters(path_level_params, op.get("parameters"), doc, warnings);
            let path_params: Vec<ParamInput> = merged_params
                .iter()
                .filter(|(loc, _)| *loc == "path")
                .map(|(_, p)| p.clone())
                .collect();
            let query_params: Vec<ParamInput> = merged_params
                .iter()
                .filter(|(loc, _)| *loc == "query")
                .map(|(_, p)| p.clone())
                .collect();

            // OAS 3.0 requestBody takes precedence; fall back to Swagger 2.0
            // formData / body parameters when absent.
            let body = extract_request_body(op, doc, warnings)
                .or_else(|| swagger2_body_from_params(&merged_params, op, doc, warnings));

            raw.push((
                raw_name,
                OperationInput {
                    name: String::new(), // filled in during deduplication
                    description,
                    method: method.to_uppercase(),
                    path: path.clone(),
                    path_params,
                    query_params,
                    body,
                },
            ));
        }
    }

    deduplicate_names(raw, warnings)
}

/// Merge path-level and operation-level `parameters` arrays. Operation params
/// take precedence over path-level params with the same (`name`, `in`) pair.
fn merge_parameters(
    path_level: Option<&Value>,
    op_level: Option<&Value>,
    doc: &Value,
    warnings: &mut Vec<String>,
) -> Vec<(String, ParamInput)> {
    let mut by_key: HashMap<(String, String), (String, ParamInput)> = HashMap::new();

    let mut process = |arr: &Vec<Value>| {
        for p in arr {
            let mut seen = HashSet::new();
            let resolved = if let Some(r) = p.get("$ref").and_then(Value::as_str) {
                let r = r.to_owned();
                resolve_ref(doc, &r, &mut seen, warnings).into_owned()
            } else {
                p.clone()
            };
            let name = match resolved.get("name").and_then(Value::as_str) {
                Some(n) => n.to_owned(),
                None => continue,
            };
            let location = match resolved.get("in").and_then(Value::as_str) {
                Some(l) => l.to_owned(),
                None => continue,
            };
            // Skip header and cookie parameters.
            if location == "header" || location == "cookie" {
                continue;
            }
            let required = resolved
                .get("required")
                .and_then(Value::as_bool)
                .unwrap_or(location == "path");
            // OAS 3.0 wraps the type in a `schema` object; Swagger 2.0 puts
            // `type` (and optionally `enum`, `format`, etc.) directly on the param.
            let schema = if let Some(s) = resolved.get("schema") {
                let mut seen2 = HashSet::new();
                resolve_schema(doc, s, &mut seen2, warnings).into_owned()
            } else if resolved.get("type").is_some() {
                // Swagger 2.0 inline type — lift it into a schema object.
                let mut s = serde_json::Map::new();
                for key in &["type", "format", "enum", "minimum", "maximum", "items"] {
                    if let Some(v) = resolved.get(*key) {
                        s.insert((*key).to_owned(), v.clone());
                    }
                }
                Value::Object(s)
            } else {
                serde_json::json!({ "type": "string" })
            };

            by_key.insert(
                (name.clone(), location.clone()),
                (
                    location,
                    ParamInput {
                        name,
                        schema,
                        required,
                    },
                ),
            );
        }
    };

    if let Some(arr) = path_level.and_then(Value::as_array) {
        process(arr);
    }
    if let Some(arr) = op_level.and_then(Value::as_array) {
        process(arr);
    }

    by_key.into_values().collect()
}

fn extract_request_body(
    op: &Value,
    doc: &Value,
    warnings: &mut Vec<String>,
) -> Option<RequestBodyInput> {
    let body_val = op.get("requestBody")?;
    let mut seen = HashSet::new();
    let resolved = if let Some(r) = body_val.get("$ref").and_then(Value::as_str) {
        let r = r.to_owned();
        resolve_ref(doc, &r, &mut seen, warnings).into_owned()
    } else {
        body_val.clone()
    };

    let required = resolved
        .get("required")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let schema = resolved
        .get("content")
        .and_then(|c| c.get("application/json"))
        .and_then(|m| m.get("schema"))?;

    let mut seen2 = HashSet::new();
    let resolved_schema = resolve_schema(doc, schema, &mut seen2, warnings).into_owned();

    let kind = if resolved_schema.get("type").and_then(Value::as_str) == Some("object") {
        let props = resolved_schema.get("properties").and_then(Value::as_object);
        let required_fields: Vec<&str> = resolved_schema
            .get("required")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();

        if let Some(props) = props {
            let properties = props
                .iter()
                .map(|(name, prop_schema)| {
                    let mut seen3 = HashSet::new();
                    let prop_resolved =
                        resolve_schema(doc, prop_schema, &mut seen3, warnings).into_owned();
                    ParamInput {
                        name: name.clone(),
                        schema: prop_resolved,
                        required: required_fields.contains(&name.as_str()),
                    }
                })
                .collect();
            RequestBodyKind::FlatObject { properties }
        } else {
            RequestBodyKind::Passthrough {
                schema: resolved_schema,
            }
        }
    } else {
        RequestBodyKind::Passthrough {
            schema: resolved_schema,
        }
    };

    Some(RequestBodyInput { required, kind })
}

/// Swagger 2.0 fallback: build a body from `in: formData` params or a single
/// `in: body` param when the operation has no OAS 3.0 `requestBody`.
fn swagger2_body_from_params(
    merged: &[(String, ParamInput)],
    op: &Value,
    doc: &Value,
    warnings: &mut Vec<String>,
) -> Option<RequestBodyInput> {
    // Prefer formData params (maps to FlatObject).
    let form_params: Vec<ParamInput> = merged
        .iter()
        .filter(|(loc, _)| loc == "formData")
        .map(|(_, p)| p.clone())
        .collect();

    if !form_params.is_empty() {
        return Some(RequestBodyInput {
            required: form_params.iter().any(|p| p.required),
            kind: RequestBodyKind::FlatObject {
                properties: form_params,
            },
        });
    }

    // Fall back to a single `in: body` parameter.
    let body_param = op
        .get("parameters")
        .and_then(Value::as_array)
        .and_then(|arr| arr.iter().find(|p| p.get("in").and_then(Value::as_str) == Some("body")))?;

    let required = body_param
        .get("required")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let schema = body_param.get("schema")?;
    let mut seen = HashSet::new();
    let resolved = resolve_schema(doc, schema, &mut seen, warnings).into_owned();

    let kind = if resolved.get("type").and_then(Value::as_str) == Some("object") {
        let props = resolved.get("properties").and_then(Value::as_object);
        let required_fields: Vec<&str> = resolved
            .get("required")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();

        if let Some(props) = props {
            let properties = props
                .iter()
                .map(|(name, prop_schema)| {
                    let mut seen2 = HashSet::new();
                    let resolved_prop =
                        resolve_schema(doc, prop_schema, &mut seen2, warnings).into_owned();
                    ParamInput {
                        name: name.clone(),
                        schema: resolved_prop,
                        required: required_fields.contains(&name.as_str()),
                    }
                })
                .collect();
            RequestBodyKind::FlatObject { properties }
        } else {
            RequestBodyKind::Passthrough { schema: resolved }
        }
    } else {
        RequestBodyKind::Passthrough { schema: resolved }
    };

    Some(RequestBodyInput { required, kind })
}

// ── Name helpers ──────────────────────────────────────────────────────────────

pub fn to_snake_case(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_underscore = true;
    for ch in s.chars() {
        if ch.is_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            prev_underscore = false;
        } else if !prev_underscore {
            out.push('_');
            prev_underscore = true;
        }
    }
    out.trim_end_matches('_').to_owned()
}

fn fallback_name(method: &str, path: &str) -> String {
    let path_part = to_snake_case(path);
    format!("{}_{}", method, path_part)
}

/// Resolve duplicate tool names using `{name}_{method}` suffix, then counter.
fn deduplicate_names(
    raw: Vec<(String, OperationInput)>,
    warnings: &mut Vec<String>,
) -> Vec<OperationInput> {
    // Count how many times each raw name appears. Use owned keys so `raw` can be
    // moved in the next step without a lifetime conflict.
    let mut counts: HashMap<String, usize> = HashMap::new();
    for (name, _) in &raw {
        *counts.entry(name.clone()).or_insert(0) += 1;
    }

    // For duplicates, try appending `_{method}`.
    let mut with_candidates: Vec<(String, OperationInput)> = raw
        .into_iter()
        .map(|(name, op)| {
            let candidate = if counts.get(&name).copied().unwrap_or(0) > 1 {
                format!("{}_{}", name, op.method.to_lowercase())
            } else {
                name
            };
            (candidate, op)
        })
        .collect();

    // Recount — still-duplicate pairs get a numeric suffix.
    let mut seen_final: HashMap<String, usize> = HashMap::new();
    let mut result = Vec::with_capacity(with_candidates.len());

    for (candidate, mut op) in with_candidates.drain(..) {
        let count = seen_final.entry(candidate.clone()).or_insert(0);
        let final_name = if *count == 0 {
            candidate.clone()
        } else {
            let suffixed = format!("{}_{}", candidate, *count + 1);
            warnings.push(format!(
                "duplicate tool name '{candidate}' renamed to '{suffixed}'"
            ));
            suffixed
        };
        *count += 1;
        op.name = final_name;
        result.push(op);
    }

    result
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn extract_str<'a>(doc: &'a Value, path: &[&str]) -> Option<&'a str> {
    let mut cur = doc;
    for key in path {
        cur = cur.get(key)?;
    }
    cur.as_str()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(yaml: &str) -> Value {
        serde_norway::from_str(yaml).expect("test yaml")
    }

    // ── load ──────────────────────────────────────────────────────────────────

    #[test]
    fn load_yaml_file() {
        let tmp = tempfile("server:\n  name: t\ntools: []\n", "yaml");
        let v = load_document(tmp.path()).unwrap();
        assert_eq!(v["server"]["name"], "t");
    }

    #[test]
    fn load_json_file() {
        let tmp = tempfile(r#"{"server":{"name":"t"},"tools":[]}"#, "json");
        let v = load_document(tmp.path()).unwrap();
        assert_eq!(v["server"]["name"], "t");
    }

    #[test]
    fn load_bad_extension_errors() {
        let tmp = tempfile("server:\n  name: t\n", "toml");
        assert!(matches!(
            load_document(tmp.path()),
            Err(ParseError::UnknownFormat { .. })
        ));
    }

    #[test]
    fn missing_title_errors() {
        let v = doc("info:\n  version: '1'\n");
        assert!(matches!(extract_title(&v), Err(ParseError::MissingTitle)));
    }

    // ── auth ──────────────────────────────────────────────────────────────────

    #[test]
    fn auth_bearer() {
        let v = doc(
            "components:\n  securitySchemes:\n    bearerAuth:\n      type: http\n      scheme: bearer\n",
        );
        let mut w = Vec::new();
        let auth = extract_auth(&v, "MY_API", &mut w).unwrap();
        assert!(matches!(auth, DetectedAuth::Bearer { .. }));
        assert!(w.is_empty());
    }

    #[test]
    fn auth_basic() {
        let v = doc(
            "components:\n  securitySchemes:\n    basicAuth:\n      type: http\n      scheme: basic\n",
        );
        let mut w = Vec::new();
        let auth = extract_auth(&v, "MY_API", &mut w).unwrap();
        assert!(matches!(auth, DetectedAuth::Basic { .. }));
    }

    #[test]
    fn auth_api_key_header() {
        let v = doc(
            "components:\n  securitySchemes:\n    apiKey:\n      type: apiKey\n      in: header\n      name: X-Api-Key\n",
        );
        let mut w = Vec::new();
        let auth = extract_auth(&v, "MY_API", &mut w).unwrap();
        assert!(matches!(
            auth,
            DetectedAuth::ApiKey {
                location: ApiKeyLocation::Header,
                ..
            }
        ));
    }

    #[test]
    fn auth_api_key_query() {
        let v = doc(
            "components:\n  securitySchemes:\n    apiKey:\n      type: apiKey\n      in: query\n      name: api_key\n",
        );
        let mut w = Vec::new();
        let auth = extract_auth(&v, "MY_API", &mut w).unwrap();
        assert!(matches!(
            auth,
            DetectedAuth::ApiKey {
                location: ApiKeyLocation::Query,
                ..
            }
        ));
    }

    #[test]
    fn auth_oauth2_client_credentials() {
        let v = doc(
            "components:\n  securitySchemes:\n    oauth:\n      type: oauth2\n      flows:\n        clientCredentials:\n          tokenUrl: https://auth.example.com/token\n          scopes:\n            read: read access\n            write: write access\n",
        );
        let mut w = Vec::new();
        let auth = extract_auth(&v, "MY_API", &mut w).unwrap();
        let DetectedAuth::Oauth2 {
            token_url, scopes, ..
        } = auth
        else {
            panic!("expected Oauth2");
        };
        assert_eq!(token_url, "https://auth.example.com/token");
        assert_eq!(scopes.len(), 2);
    }

    #[test]
    fn auth_unsupported_scheme_warns_and_returns_none() {
        let v = doc(
            "components:\n  securitySchemes:\n    openId:\n      type: openIdConnect\n      openIdConnectUrl: https://example.com/.well-known\n",
        );
        let mut w = Vec::new();
        let auth = extract_auth(&v, "MY_API", &mut w);
        assert!(auth.is_none());
        assert!(!w.is_empty());
    }

    // ── $ref resolution ───────────────────────────────────────────────────────

    #[test]
    fn ref_direct_resolution() {
        let v = doc(
            "components:\n  schemas:\n    Pet:\n      type: object\n      properties:\n        name:\n          type: string\n",
        );
        let mut seen = HashSet::new();
        let mut w = Vec::new();
        let resolved = resolve_ref(&v, "#/components/schemas/Pet", &mut seen, &mut w);
        assert_eq!(resolved["type"], "object");
        assert!(w.is_empty());
    }

    #[test]
    fn ref_chain_resolution() {
        let v = doc(
            "components:\n  schemas:\n    PetAlias:\n      $ref: '#/components/schemas/Pet'\n    Pet:\n      type: string\n",
        );
        let mut seen = HashSet::new();
        let mut w = Vec::new();
        let resolved = resolve_ref(&v, "#/components/schemas/PetAlias", &mut seen, &mut w);
        assert_eq!(resolved["type"], "string");
    }

    #[test]
    fn ref_cycle_falls_back() {
        let v = doc("components:\n  schemas:\n    A:\n      $ref: '#/components/schemas/A'\n");
        let mut seen = HashSet::new();
        let mut w = Vec::new();
        let resolved = resolve_ref(&v, "#/components/schemas/A", &mut seen, &mut w);
        assert_eq!(resolved["type"], "object");
        assert!(!w.is_empty());
    }

    #[test]
    fn ref_external_falls_back() {
        let v = serde_json::json!({});
        let mut seen = HashSet::new();
        let mut w = Vec::new();
        let resolved = resolve_ref(&v, "other.yaml#/Foo", &mut seen, &mut w);
        assert_eq!(resolved["type"], "object");
        assert!(!w.is_empty());
    }

    // ── name helpers ──────────────────────────────────────────────────────────

    #[test]
    fn snake_case_from_camel() {
        assert_eq!(to_snake_case("listPets"), "listpets");
    }

    #[test]
    fn snake_case_from_kebab() {
        assert_eq!(to_snake_case("list-pets"), "list_pets");
    }

    #[test]
    fn snake_case_strips_leading_trailing() {
        // Leading non-alphanum is skipped; trailing underscore is trimmed.
        assert_eq!(to_snake_case("_listPets_"), "listpets");
    }

    #[test]
    fn fallback_name_from_method_path() {
        // `{petId}` → `petid`; trailing `}` is trimmed before the format.
        assert_eq!(fallback_name("get", "/pets/{petId}"), "get_pets_petid");
    }

    #[test]
    fn env_prefix_from_title() {
        assert_eq!(make_env_prefix("Petstore API"), "PETSTORE_API");
        assert_eq!(make_env_prefix("My-Cool API v2"), "MY_COOL_API_V2");
    }

    // ── deduplication ─────────────────────────────────────────────────────────

    #[test]
    fn dedup_adds_method_suffix_for_collision() {
        let ops = vec![
            ("list_pets".to_owned(), make_op("GET", "/pets")),
            ("list_pets".to_owned(), make_op("POST", "/pets")),
        ];
        let mut w = Vec::new();
        let result = deduplicate_names(ops, &mut w);
        let names: Vec<&str> = result.iter().map(|o| o.name.as_str()).collect();
        assert!(names.contains(&"list_pets_get"), "{names:?}");
        assert!(names.contains(&"list_pets_post"), "{names:?}");
    }

    #[test]
    fn dedup_numeric_suffix_for_remaining_collision() {
        // Both ops have raw name "foo_get" and method GET → both get `_get` suffix
        // → "foo_get_get" × 2 → second gets numeric suffix "foo_get_get_2".
        let ops = vec![
            ("foo_get".to_owned(), make_op("GET", "/a")),
            ("foo_get".to_owned(), make_op("GET", "/b")),
        ];
        let mut w = Vec::new();
        let result = deduplicate_names(ops, &mut w);
        let names: Vec<&str> = result.iter().map(|o| o.name.as_str()).collect();
        assert!(names.contains(&"foo_get_get"), "{names:?}");
        assert!(names.contains(&"foo_get_get_2"), "{names:?}");
        assert!(!w.is_empty());
    }

    // ── helpers ───────────────────────────────────────────────────────────────

    fn make_op(method: &str, path: &str) -> OperationInput {
        OperationInput {
            name: String::new(),
            description: None,
            method: method.to_owned(),
            path: path.to_owned(),
            path_params: vec![],
            query_params: vec![],
            body: None,
        }
    }

    /// Write `content` to a temporary file with the given extension and return a
    /// handle that deletes it on drop.
    fn tempfile(content: &str, ext: &str) -> TempFile {
        let path = std::env::temp_dir().join(format!(
            "lattice_gen_test_{}.{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos(),
            ext
        ));
        std::fs::write(&path, content).unwrap();
        TempFile { path }
    }

    struct TempFile {
        path: std::path::PathBuf,
    }
    impl TempFile {
        fn path(&self) -> &std::path::Path {
            &self.path
        }
    }
    impl Drop for TempFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}
