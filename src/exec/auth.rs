//! Authentication for HTTP requests.
//!
//! Applies a tool's configured [`Auth`] to an outgoing `reqwest` request:
//! - **bearer / basic / api_key** are static — they add a header or query parameter with
//!   no I/O (reqwest does the `Basic` base64 / `Bearer` formatting).
//! - **oauth2** (client-credentials) fetches an access token from the `token_url`, caches
//!   it (with an expiry safety margin), serves concurrent callers from a single in-flight
//!   fetch, and refreshes once on a `401`.
//!
//! State lives in [`AuthState`], created once per tool and reused so the OAuth token cache
//! survives across calls. Secrets (tokens, client secret) are **never** logged and never
//! placed in an error message — token-fetch failures are scrubbed of their URL.

use std::time::{Duration, Instant};

use reqwest::{Client, RequestBuilder};
use serde::Deserialize;
use thiserror::Error;
use tokio::sync::Mutex;

use crate::config::{ApiKeyLocation, Auth};

/// Refresh a cached token this long *before* it actually expires, to cover clock skew and
/// in-flight requests.
const EXPIRY_MARGIN: Duration = Duration::from_secs(60);
/// Assumed lifetime when a token response omits `expires_in`.
const DEFAULT_EXPIRES_IN: Duration = Duration::from_secs(3600);
/// Cap on a cached token's lifetime (24h). Bounds the cache window and, crucially, keeps
/// `Instant::now() + lifetime` from overflowing on a hostile/huge `expires_in`.
const MAX_TOKEN_LIFETIME: Duration = Duration::from_secs(24 * 60 * 60);
/// Wall-clock cap on a token-endpoint request.
const TOKEN_TIMEOUT: Duration = Duration::from_secs(30);
/// Cap on the token-response body we will buffer (token responses are tiny; this guards
/// against a hostile/MITM'd token endpoint streaming an unbounded body — OOM).
const MAX_TOKEN_BYTES: usize = 64 * 1024;

/// Errors from obtaining credentials.
///
/// Messages are deliberately free of secrets: the OAuth client secret never appears, and
/// transport errors are stripped of their URL.
#[derive(Debug, Error)]
pub enum AuthError {
    /// The token endpoint could not be reached or returned a non-success status.
    #[error("OAuth2 token request failed: {0}")]
    TokenRequest(String),
    /// The token response could not be parsed (missing `access_token`, bad JSON, …).
    #[error("OAuth2 token response was invalid: {0}")]
    TokenResponse(String),
}

/// The outcome of applying auth to a request.
///
/// Carries the OAuth access token alongside the request so the executor can drive a single
/// `401` refresh; static schemes need no follow-up.
pub enum Applied {
    /// A static scheme was applied (or there was nothing to retry).
    Ready(RequestBuilder),
    /// An OAuth bearer token was applied; `token` is the value used, for refresh-on-401.
    OAuth {
        request: RequestBuilder,
        token: String,
    },
}

/// Per-tool authentication state: the configured scheme plus its OAuth token cache.
pub struct AuthState {
    auth: Auth,
    cache: OAuthCache,
}

impl AuthState {
    /// Create state for a tool's resolved [`Auth`].
    pub fn new(auth: Auth) -> Self {
        Self {
            auth,
            cache: OAuthCache::new(),
        }
    }

    /// Whether this scheme is OAuth2 (and therefore can refresh on a 401).
    pub fn is_oauth(&self) -> bool {
        matches!(self.auth, Auth::Oauth2 { .. })
    }

    /// Apply the configured auth to `request`, fetching an OAuth token if needed.
    pub async fn apply(
        &self,
        client: &Client,
        request: RequestBuilder,
    ) -> Result<Applied, AuthError> {
        match &self.auth {
            Auth::Bearer { token } => Ok(Applied::Ready(request.bearer_auth(token))),
            Auth::Basic { username, password } => {
                Ok(Applied::Ready(request.basic_auth(username, Some(password))))
            }
            Auth::ApiKey {
                location,
                name,
                value,
            } => Ok(Applied::Ready(apply_api_key(
                request, *location, name, value,
            ))),
            Auth::Oauth2 { .. } => {
                let token = self.cache.token(client, &self.auth).await?;
                Ok(Applied::OAuth {
                    request: request.bearer_auth(&token),
                    token,
                })
            }
        }
    }

    /// Re-apply OAuth auth after a `401`, forcing a token refresh if `stale` is still the
    /// cached value (so concurrent 401s share one refresh).
    pub async fn reauthorize(
        &self,
        client: &Client,
        request: RequestBuilder,
        stale: &str,
    ) -> Result<RequestBuilder, AuthError> {
        let token = self.cache.refresh(client, &self.auth, stale).await?;
        Ok(request.bearer_auth(token))
    }
}

/// Place an API key in a header or query parameter.
fn apply_api_key(
    request: RequestBuilder,
    location: ApiKeyLocation,
    name: &str,
    value: &str,
) -> RequestBuilder {
    match location {
        ApiKeyLocation::Header => request.header(name, value),
        ApiKeyLocation::Query => request.query(&[(name, value)]),
    }
}

/// A cached OAuth2 access token and when it expires.
struct CachedToken {
    access_token: String,
    expires_at: Instant,
}

/// A single-slot OAuth2 token cache with single-flight fetching.
///
/// The async mutex is held across the network fetch, so concurrent callers that find the
/// slot empty/expired queue up and the first one to acquire the lock does the only fetch;
/// the rest observe the freshly stored token.
struct OAuthCache {
    cached: Mutex<Option<CachedToken>>,
}

impl OAuthCache {
    fn new() -> Self {
        Self {
            cached: Mutex::new(None),
        }
    }

    /// Return a valid cached token, or fetch and cache a new one.
    async fn token(&self, client: &Client, auth: &Auth) -> Result<String, AuthError> {
        let mut guard = self.cached.lock().await;
        if let Some(token) = guard.as_ref() {
            if Instant::now() + EXPIRY_MARGIN < token.expires_at {
                return Ok(token.access_token.clone());
            }
        }
        let fresh = fetch_token(client, auth).await?;
        let access_token = fresh.access_token.clone();
        *guard = Some(fresh);
        Ok(access_token)
    }

    /// Refresh after a 401: fetch a new token only if `stale` is still cached (otherwise a
    /// concurrent caller already refreshed — return theirs).
    async fn refresh(
        &self,
        client: &Client,
        auth: &Auth,
        stale: &str,
    ) -> Result<String, AuthError> {
        let mut guard = self.cached.lock().await;
        if let Some(token) = guard.as_ref() {
            if token.access_token != stale {
                return Ok(token.access_token.clone());
            }
        }
        let fresh = fetch_token(client, auth).await?;
        let access_token = fresh.access_token.clone();
        *guard = Some(fresh);
        Ok(access_token)
    }
}

/// The subset of an OAuth2 token response we use.
#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    expires_in: Option<u64>,
}

/// Fetch a client-credentials access token from the configured token endpoint.
async fn fetch_token(client: &Client, auth: &Auth) -> Result<CachedToken, AuthError> {
    let Auth::Oauth2 {
        token_url,
        client_id,
        client_secret,
        scopes,
    } = auth
    else {
        // The cache is only ever constructed for an oauth2 auth; defensive.
        return Err(AuthError::TokenRequest(
            "not an oauth2 configuration".to_string(),
        ));
    };

    tracing::debug!("fetching oauth2 client-credentials token");

    let scope = scopes.join(" ");
    let mut params = vec![
        ("grant_type", "client_credentials"),
        ("client_id", client_id.as_str()),
        ("client_secret", client_secret.as_str()),
    ];
    if !scope.is_empty() {
        params.push(("scope", scope.as_str()));
    }

    let response = client
        .post(token_url.as_str())
        .form(&params)
        .timeout(TOKEN_TIMEOUT)
        .send()
        .await
        .map_err(|err| AuthError::TokenRequest(err.without_url().to_string()))?;

    let status = response.status();
    if !status.is_success() {
        return Err(AuthError::TokenRequest(format!(
            "token endpoint returned status {}",
            status.as_u16()
        )));
    }

    let body = read_token_body(response, MAX_TOKEN_BYTES).await?;
    let token: TokenResponse =
        serde_json::from_slice(&body).map_err(|err| AuthError::TokenResponse(err.to_string()))?;

    // Clamp the lifetime: an untrusted `expires_in` could otherwise overflow the `Instant`
    // addition (a panic) — and a 24h ceiling forces periodic re-auth anyway.
    let lifetime = token
        .expires_in
        .map(Duration::from_secs)
        .unwrap_or(DEFAULT_EXPIRES_IN)
        .min(MAX_TOKEN_LIFETIME);
    Ok(CachedToken {
        access_token: token.access_token,
        expires_at: Instant::now() + lifetime,
    })
}

/// Read a token-endpoint response body, aborting past `limit` bytes (token responses are
/// small; an unbounded body from a hostile endpoint must not OOM the process).
async fn read_token_body(
    mut response: reqwest::Response,
    limit: usize,
) -> Result<Vec<u8>, AuthError> {
    let mut buffer = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|err| AuthError::TokenRequest(err.without_url().to_string()))?
    {
        if buffer.len() + chunk.len() > limit {
            return Err(AuthError::TokenRequest(format!(
                "token response exceeded {limit} bytes"
            )));
        }
        buffer.extend_from_slice(&chunk);
    }
    Ok(buffer)
}
