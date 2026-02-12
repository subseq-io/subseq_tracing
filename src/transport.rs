use std::sync::{Arc, Mutex};

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
