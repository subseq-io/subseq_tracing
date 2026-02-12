use std::collections::BTreeMap;
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

#[derive(Debug, Clone)]
pub struct HttpTransportOptions {
    pub endpoint: String,
    pub bearer_token: Option<String>,
    pub timeout: Duration,
    pub user_agent: String,
    pub headers: BTreeMap<String, String>,
}

impl Default for HttpTransportOptions {
    fn default() -> Self {
        Self {
            endpoint: "http://localhost:3000/api/v1/monitoring/ingest/diagnostics".to_string(),
            bearer_token: None,
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

        let agent = ureq::AgentBuilder::new().timeout(options.timeout).build();

        Ok(Self { options, agent })
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

        if let Some(token) = &self.options.bearer_token {
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
