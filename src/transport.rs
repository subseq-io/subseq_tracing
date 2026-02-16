use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use thiserror::Error;

use crate::diagnostics::DiagnosticPackage;

#[derive(Debug, Error)]
pub enum TransportError {
    #[error("transport error: {0}")]
    Message(String),
}

pub trait DiagnosticsTransport: Send + Sync {
    fn send(&self, package: DiagnosticPackage) -> Result<(), TransportError>;
}

pub trait WorkloadJwtProvider: Send + Sync {
    fn workload_jwt(&self) -> Result<String, TransportError>;
}

impl<F> WorkloadJwtProvider for F
where
    F: Fn() -> Result<String, TransportError> + Send + Sync,
{
    fn workload_jwt(&self) -> Result<String, TransportError> {
        self()
    }
}

#[derive(Debug, Default)]
pub struct NoopTransport;

impl DiagnosticsTransport for NoopTransport {
    fn send(&self, _package: DiagnosticPackage) -> Result<(), TransportError> {
        Ok(())
    }
}

#[derive(Debug, Default, Clone)]
pub struct MemoryTransport {
    packages: Arc<Mutex<Vec<DiagnosticPackage>>>,
}

impl MemoryTransport {
    pub fn packages(&self) -> Vec<DiagnosticPackage> {
        self.packages
            .lock()
            .expect("memory transport poisoned")
            .clone()
    }

    pub fn clear(&self) {
        self.packages
            .lock()
            .expect("memory transport poisoned")
            .clear();
    }
}

impl DiagnosticsTransport for MemoryTransport {
    fn send(&self, package: DiagnosticPackage) -> Result<(), TransportError> {
        self.packages
            .lock()
            .map_err(|_| TransportError::Message("memory transport poisoned".to_string()))?
            .push(package);
        Ok(())
    }
}

#[derive(Clone)]
pub struct HttpTransportOptions {
    pub endpoint: String,
    pub workload_jwt: Option<String>,
    pub workload_jwt_provider: Option<Arc<dyn WorkloadJwtProvider>>,
    // Backward compatibility with older callers. Prefer workload_jwt.
    pub bearer_token: Option<String>,
    /// Optional root CA bundle for HTTPS endpoints (PEM encoded).
    ///
    /// This is primarily intended for in-cluster dev where readysetapp uses a cert-manager
    /// cluster CA, and workloads must trust that CA when sending diagnostics over HTTPS.
    pub ca_cert_pem_path: Option<PathBuf>,
    pub timeout: Duration,
    pub user_agent: String,
    pub headers: BTreeMap<String, String>,
}

impl std::fmt::Debug for HttpTransportOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpTransportOptions")
            .field("endpoint", &self.endpoint)
            .field(
                "workload_jwt",
                &self
                    .workload_jwt
                    .as_ref()
                    .map(|_| "<redacted workload jwt>"),
            )
            .field(
                "workload_jwt_provider",
                &self
                    .workload_jwt_provider
                    .as_ref()
                    .map(|_| "<dyn WorkloadJwtProvider>"),
            )
            .field(
                "bearer_token",
                &self
                    .bearer_token
                    .as_ref()
                    .map(|_| "<redacted bearer token>"),
            )
            .field("ca_cert_pem_path", &self.ca_cert_pem_path)
            .field("timeout", &self.timeout)
            .field("user_agent", &self.user_agent)
            .field("headers", &self.headers)
            .finish()
    }
}

impl Default for HttpTransportOptions {
    fn default() -> Self {
        Self {
            endpoint: "http://localhost:3000/api/v1/monitoring/ingest/diagnostics".to_string(),
            workload_jwt: None,
            workload_jwt_provider: None,
            bearer_token: None,
            ca_cert_pem_path: None,
            timeout: Duration::from_secs(5),
            user_agent: format!("subseq_tracing/{}", env!("CARGO_PKG_VERSION")),
            headers: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct HttpTransport {
    options: HttpTransportOptions,
    agent: ureq::Agent,
}

impl HttpTransport {
    pub fn new(options: HttpTransportOptions) -> Result<Self, TransportError> {
        if options.endpoint.trim().is_empty() {
            return Err(TransportError::Message(
                "http transport endpoint must be non-empty".to_string(),
            ));
        }
        if !(options.endpoint.starts_with("http://") || options.endpoint.starts_with("https://")) {
            return Err(TransportError::Message(
                "http transport endpoint must start with http:// or https://".to_string(),
            ));
        }
        if options.workload_jwt_provider.is_some()
            && (options.workload_jwt.is_some() || options.bearer_token.is_some())
        {
            return Err(TransportError::Message(
                "http transport workload_jwt_provider cannot be combined with workload_jwt/bearer_token".to_string(),
            ));
        }

        let configured_static_token = options
            .workload_jwt
            .as_deref()
            .or(options.bearer_token.as_deref());
        if let Some(token) = configured_static_token {
            if token.trim().is_empty() {
                return Err(TransportError::Message(
                    "http transport workload jwt must be non-empty".to_string(),
                ));
            }
        }

        let mut agent_builder = ureq::AgentBuilder::new().timeout(options.timeout);
        if options.endpoint.starts_with("https://") {
            if let Some(ca_path) = options.ca_cert_pem_path.as_ref() {
                let mut root_store = rustls::RootCertStore {
                    roots: webpki_roots::TLS_SERVER_ROOTS.iter().cloned().collect(),
                };

                let pem = std::fs::read(ca_path).map_err(|err| {
                    TransportError::Message(format!(
                        "failed to read ca cert pem at {}: {err}",
                        ca_path.display()
                    ))
                })?;

                let certs = rustls_pemfile::certs(&mut &pem[..])
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|err| {
                        TransportError::Message(format!(
                            "invalid ca cert pem at {}: {err}",
                            ca_path.display()
                        ))
                    })?;

                if certs.is_empty() {
                    return Err(TransportError::Message(format!(
                        "ca cert pem at {} did not contain any certificates",
                        ca_path.display()
                    )));
                }

                for cert in certs {
                    root_store.add(cert).map_err(|err| {
                        TransportError::Message(format!(
                            "invalid ca cert in {}: {err}",
                            ca_path.display()
                        ))
                    })?;
                }

                let tls_config = rustls::ClientConfig::builder()
                    .with_root_certificates(root_store)
                    .with_no_client_auth();
                agent_builder = agent_builder.tls_config(Arc::new(tls_config));
            }
        }

        let agent = agent_builder.build();

        Ok(Self { options, agent })
    }

    fn resolve_workload_jwt(&self) -> Result<Option<String>, TransportError> {
        let token = if let Some(provider) = &self.options.workload_jwt_provider {
            Some(provider.workload_jwt()?)
        } else {
            self.options
                .workload_jwt
                .clone()
                .or_else(|| self.options.bearer_token.clone())
        };

        match token {
            Some(value) => {
                let trimmed = value.trim();
                if trimmed.is_empty() {
                    return Err(TransportError::Message(
                        "http transport workload jwt must be non-empty".to_string(),
                    ));
                }
                Ok(Some(trimmed.to_string()))
            }
            None => Ok(None),
        }
    }
}

impl DiagnosticsTransport for HttpTransport {
    fn send(&self, package: DiagnosticPackage) -> Result<(), TransportError> {
        let mut request = self
            .agent
            .post(&self.options.endpoint)
            .set("content-type", "application/json")
            .set("accept", "application/json")
            .set("user-agent", &self.options.user_agent);

        if let Some(token) = self.resolve_workload_jwt()? {
            request = request.set("authorization", &format!("Bearer {token}"));
        }

        for (name, value) in &self.options.headers {
            request = request.set(name, value);
        }

        match request.send_json(&package) {
            Ok(_) => Ok(()),
            Err(ureq::Error::Status(status, response)) => {
                let body = response.into_string().unwrap_or_default();
                Err(TransportError::Message(format!(
                    "http status {status} from diagnostics endpoint: {body}"
                )))
            }
            Err(ureq::Error::Transport(error)) => Err(TransportError::Message(format!(
                "http transport failure: {error}"
            ))),
        }
    }
}
