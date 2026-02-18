//! subseq_tracing
//!
//! Client-first diagnostics capture designed for readysetapp ingestion.
//! The API is intentionally Sentry-like:
//! - `init`
//! - `configure_scope`
//! - `with_scope`
//! - `capture_message` / `capture_error`
//! - panic hook support
//! - tracing subscriber layer for warning/error events
//! - workload JWT transport options for readysetapp ingest

mod client;
mod diagnostics;
mod layer;
mod oauth;
pub mod readysetapp;
mod scope;
mod transport;

pub use client::{
    ClientInitGuard, DiagnosticsClient, DiagnosticsOptions, capture_error, capture_error_message,
    capture_message, capture_message_with_fields, capture_warning, configure_scope, current_client,
    flush, init,
};
pub use diagnostics::{
    DiagnosticEvent, DiagnosticKind, DiagnosticLevel, DiagnosticPackage, ScopeSnapshot,
    TraceContext, UserContext,
};
pub use layer::DiagnosticsLayer;
pub use oauth::{
    CognitoClientCredentialsJwtProvider, OAuthClientCredentialsJwtProvider,
    OAuthClientCredentialsJwtProviderOptions,
};
pub use scope::{Scope, with_scope, with_scope_async};
pub use transport::{
    DiagnosticsTransport, HttpTransport, HttpTransportOptions, MemoryTransport, NoopTransport,
    TransportError, WorkloadJwtProvider,
};

/// Create a tracing layer that forwards warn/error events into diagnostics capture.
pub fn diagnostics_layer() -> DiagnosticsLayer {
    DiagnosticsLayer::new(DiagnosticLevel::Warning)
}

/// Create a tracing layer with a custom minimum diagnostic level.
pub fn diagnostics_layer_with_min_level(min_level: DiagnosticLevel) -> DiagnosticsLayer {
    DiagnosticsLayer::new(min_level)
}
