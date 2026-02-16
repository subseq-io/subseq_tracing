use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::Engine as _;
use serde::Deserialize;

use crate::transport::{TransportError, WorkloadJwtProvider};

#[derive(Clone)]
pub struct OAuthClientCredentialsJwtProviderOptions {
    pub token_url: String,
    pub client_id: String,
    pub client_secret: String,
    pub scopes: Vec<String>,
    pub timeout: Duration,
    pub user_agent: String,
    pub refresh_margin: Duration,
}

impl std::fmt::Debug for OAuthClientCredentialsJwtProviderOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OAuthClientCredentialsJwtProviderOptions")
            .field("token_url", &self.token_url)
            .field("client_id", &self.client_id)
            .field("client_secret", &"<redacted>")
            .field("scopes", &self.scopes)
            .field("timeout", &self.timeout)
            .field("user_agent", &self.user_agent)
            .field("refresh_margin", &self.refresh_margin)
            .finish()
    }
}

impl Default for OAuthClientCredentialsJwtProviderOptions {
    fn default() -> Self {
        Self {
            token_url: "https://example.invalid/oauth2/token".to_string(),
            client_id: "client-id".to_string(),
            client_secret: "client-secret".to_string(),
            scopes: Vec::new(),
            timeout: Duration::from_secs(5),
            user_agent: format!("subseq_tracing/{}", env!("CARGO_PKG_VERSION")),
            refresh_margin: Duration::from_secs(30),
        }
    }
}

#[derive(Clone)]
pub struct OAuthClientCredentialsJwtProvider {
    options: OAuthClientCredentialsJwtProviderOptions,
    agent: ureq::Agent,
    cache: Arc<Mutex<Option<CachedToken>>>,
}

#[derive(Clone)]
struct CachedToken {
    token: String,
    expires_at: Instant,
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    expires_in: Option<u64>,
    #[serde(default)]
    token_type: Option<String>,
}

impl OAuthClientCredentialsJwtProvider {
    pub fn new(options: OAuthClientCredentialsJwtProviderOptions) -> Result<Self, TransportError> {
        if options.token_url.trim().is_empty() {
            return Err(TransportError::Message(
                "oauth token_url must be non-empty".to_string(),
            ));
        }
        if options.client_id.trim().is_empty() {
            return Err(TransportError::Message(
                "oauth client_id must be non-empty".to_string(),
            ));
        }
        if options.client_secret.trim().is_empty() {
            return Err(TransportError::Message(
                "oauth client_secret must be non-empty".to_string(),
            ));
        }

        let agent = ureq::AgentBuilder::new().timeout(options.timeout).build();
        Ok(Self {
            options,
            agent,
            cache: Arc::new(Mutex::new(None)),
        })
    }

    fn fetch_token(&self) -> Result<CachedToken, TransportError> {
        let scope = self.options.scopes.join(" ");
        let mut form = vec![("grant_type", "client_credentials")];
        if !scope.trim().is_empty() {
            form.push(("scope", scope.as_str()));
        }

        let request = self
            .agent
            .post(&self.options.token_url)
            .set("accept", "application/json")
            .set("user-agent", &self.options.user_agent)
            .set(
                "authorization",
                &format!(
                    "Basic {}",
                    base64::engine::general_purpose::STANDARD.encode(format!(
                        "{}:{}",
                        self.options.client_id, self.options.client_secret
                    ))
                ),
            );

        let response = request.send_form(&form).map_err(|err| match err {
            ureq::Error::Status(status, response) => {
                let body = response.into_string().unwrap_or_default();
                TransportError::Message(format!("oauth token endpoint http {status}: {body}"))
            }
            ureq::Error::Transport(error) => {
                TransportError::Message(format!("oauth token endpoint transport failure: {error}"))
            }
        })?;

        let parsed: TokenResponse = response.into_json().map_err(|err| {
            TransportError::Message(format!("oauth token endpoint returned invalid JSON: {err}"))
        })?;

        let token = parsed.access_token.trim().to_string();
        if token.is_empty() {
            return Err(TransportError::Message(
                "oauth token endpoint returned empty access_token".to_string(),
            ));
        }

        let expires_in = parsed.expires_in.unwrap_or(0);
        if expires_in == 0 {
            return Err(TransportError::Message(
                "oauth token endpoint returned missing/zero expires_in".to_string(),
            ));
        }

        if let Some(token_type) = parsed.token_type.as_deref() {
            if token_type != "Bearer" {
                return Err(TransportError::Message(format!(
                    "oauth token endpoint returned unsupported token_type: {token_type}"
                )));
            }
        }

        Ok(CachedToken {
            token,
            expires_at: Instant::now() + Duration::from_secs(expires_in),
        })
    }
}

impl WorkloadJwtProvider for OAuthClientCredentialsJwtProvider {
    fn workload_jwt(&self) -> Result<String, TransportError> {
        let mut cache = self
            .cache
            .lock()
            .map_err(|_| TransportError::Message("oauth token cache poisoned".to_string()))?;

        let now = Instant::now();
        if let Some(existing) = cache.as_ref() {
            if existing.expires_at > now + self.options.refresh_margin {
                return Ok(existing.token.clone());
            }
        }

        let refreshed = self.fetch_token()?;
        let token = refreshed.token.clone();
        *cache = Some(refreshed);
        Ok(token)
    }
}

/// Convenience alias for Cognito client-credentials workloads.
///
/// Cognito's `/oauth2/token` endpoint is standard OAuth2 client-credentials.
pub type CognitoClientCredentialsJwtProvider = OAuthClientCredentialsJwtProvider;
