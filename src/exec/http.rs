//! HTTP request execution via `reqwest`.
//!
//! Takes a pure [`HttpRequestSpec`] (from the T8 request builder) plus the tool's
//! [`ResponseSpec`], performs the request, and returns a response-filtered [`ToolOutcome`].
//! A non-success status is reported as `is_error: true` with the (filtered) body, not as an
//! [`ExecError`]; `ExecError` is reserved for requests that never produced a response.
//!
//! Hardening (per the T11 review notes): query pairs are percent-encoded by `reqwest`;
//! header names/values go through `HeaderName`/`HeaderValue`, which reject CRLF/control
//! characters; path-var values were already path-segment-encoded by the request builder;
//! and transport errors are scrubbed of their URL so an interpolated secret can't leak.

use std::time::Duration;

use reqwest::header::{HeaderName, HeaderValue, CONTENT_TYPE};
use reqwest::{Client, Method, RequestBuilder, Response, StatusCode};
use serde_json::Value;

use super::auth::{Applied, AuthState};
use super::{ExecError, ToolOutcome};
use crate::config::ResponseSpec;
use crate::engine::{response, HttpRequestSpec};

/// Per-request wall-clock cap, as defense-in-depth against a hanging upstream. The
/// production client should also set connect/overall timeouts (T14); this guards even a
/// client built without them. (Not yet operator-configurable — see TASKS.)
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Maximum response body we will buffer (10 MiB). An untrusted upstream returning an
/// unbounded body would otherwise OOM the process; a larger body anyway overflows the
/// model's context. Exceeding it is an [`ExecError::ResponseTooLarge`].
const MAX_RESPONSE_BYTES: usize = 10 * 1024 * 1024;

/// Execute an HTTP request spec, returning the response-filtered outcome.
///
/// `auth`, when present, is applied to the request; an OAuth2 tool that receives a `401`
/// gets one token-refresh-and-retry before the response is accepted.
pub async fn execute(
    client: &Client,
    spec: &HttpRequestSpec,
    response_spec: &ResponseSpec,
    auth: Option<&AuthState>,
) -> Result<ToolOutcome, ExecError> {
    let response = send(client, spec, auth).await?;

    let status = response.status();
    let text = read_body(response, MAX_RESPONSE_BYTES).await?;

    // Curated, secret-free log line — never the URL (query secrets), headers, or body.
    tracing::debug!(method = %spec.method, status = status.as_u16(), "http request complete");

    let body = parse_body(&text);
    Ok(ToolOutcome {
        is_error: !status.is_success(),
        value: response::filter(body, response_spec),
    })
}

/// Apply auth and send, with a single OAuth2 refresh-and-retry on `401`.
async fn send(
    client: &Client,
    spec: &HttpRequestSpec,
    auth: Option<&AuthState>,
) -> Result<Response, ExecError> {
    let base = to_reqwest_request(client, spec)?;
    let Some(auth) = auth else {
        return dispatch(base).await;
    };

    // Only OAuth2 can retry, so only clone the request (for the retry) in that case.
    // `try_clone` succeeds whenever the body is in memory, which it always is here (the
    // request builder sets a `Vec<u8>` body); the `None` arm below is purely defensive.
    let retry = auth.is_oauth().then(|| base.try_clone()).flatten();
    match auth.apply(client, base).await? {
        Applied::Ready(request) => dispatch(request).await,
        Applied::OAuth { request, token } => {
            let response = dispatch(request).await?;
            if response.status() != StatusCode::UNAUTHORIZED {
                return Ok(response);
            }
            // A 401: refresh the token once and retry (bounded to a single retry).
            match retry {
                Some(retry) => {
                    let retry = auth.reauthorize(client, retry, &token).await?;
                    dispatch(retry).await
                }
                None => Ok(response),
            }
        }
    }
}

/// Send a built request with the per-request timeout, mapping transport errors.
async fn dispatch(request: RequestBuilder) -> Result<Response, ExecError> {
    request
        .timeout(REQUEST_TIMEOUT)
        .send()
        .await
        .map_err(|err| ExecError::Request(scrub(err)))
}

/// Read a response body into a string, aborting once `limit` bytes have been buffered.
async fn read_body(mut response: reqwest::Response, limit: usize) -> Result<String, ExecError> {
    let mut buffer: Vec<u8> = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|err| ExecError::Request(scrub(err)))?
    {
        if buffer.len() + chunk.len() > limit {
            return Err(ExecError::ResponseTooLarge { limit });
        }
        buffer.extend_from_slice(&chunk);
    }
    // JSON/text APIs are UTF-8; lossily decode rather than reject on stray bytes.
    Ok(String::from_utf8_lossy(&buffer).into_owned())
}

/// Translate an [`HttpRequestSpec`] into a `reqwest` request builder.
fn to_reqwest_request(
    client: &Client,
    spec: &HttpRequestSpec,
) -> Result<RequestBuilder, ExecError> {
    let method = Method::from_bytes(spec.method.to_ascii_uppercase().as_bytes())
        .map_err(|_| ExecError::InvalidMethod(spec.method.clone()))?;

    let mut request = client.request(method, &spec.url);

    if !spec.query.is_empty() {
        request = request.query(&spec.query);
    }

    for (name, value) in &spec.headers {
        let header_name = HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| ExecError::InvalidHeader(name.clone()))?;
        let header_value =
            HeaderValue::from_str(value).map_err(|_| ExecError::InvalidHeader(name.clone()))?;
        request = request.header(header_name, header_value);
    }

    if let Some(body) = &spec.body {
        let bytes = serde_json::to_vec(body).map_err(|err| ExecError::Body(err.to_string()))?;
        request = request.body(bytes);
        // `content_type` is set only when the tool didn't supply its own Content-Type
        // header (which is already in `spec.headers`).
        if let Some(content_type) = &spec.content_type {
            let value = HeaderValue::from_str(content_type)
                .map_err(|_| ExecError::InvalidHeader(CONTENT_TYPE.as_str().to_string()))?;
            request = request.header(CONTENT_TYPE, value);
        }
    }

    Ok(request)
}

/// Parse an HTTP response body: a JSON value when it parses, else the raw text as a string.
fn parse_body(text: &str) -> Value {
    serde_json::from_str(text).unwrap_or_else(|_| Value::String(text.to_string()))
}

/// A `reqwest` error message with the URL stripped (it can carry query/path secrets).
fn scrub(err: reqwest::Error) -> String {
    err.without_url().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// Fetch `/` from a mock server returning `body`, then read it with `limit`.
    async fn read_with_limit(body: &str, limit: usize) -> Result<String, ExecError> {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string(body))
            .mount(&server)
            .await;
        let response = Client::new().get(server.uri()).send().await.unwrap();
        read_body(response, limit).await
    }

    #[tokio::test]
    async fn read_body_accepts_within_limit() {
        assert_eq!(
            read_with_limit("hello world", 1024).await.unwrap(),
            "hello world"
        );
    }

    #[tokio::test]
    async fn read_body_rejects_oversized() {
        let err = read_with_limit("hello world", 5).await.unwrap_err();
        assert!(matches!(err, ExecError::ResponseTooLarge { limit: 5 }));
    }
}
