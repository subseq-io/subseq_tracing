# subseq_tracing

`subseq_tracing` is a client-first diagnostics crate for Subsequent services.

It captures:
- scoped diagnostics events (tags, user, extras)
- panic events via a global panic hook
- warn/error `tracing` events through a subscriber layer
- OpenTelemetry trace/span IDs when available on the current span

The output format is a typed `DiagnosticPackage` that can be sent to readysetapp ingest APIs.

## Sentry-style DX

```rust
use std::sync::Arc;

use subseq_tracing::{
    capture_error, capture_message, configure_scope, diagnostics_layer,
    with_scope, with_scope_async, DiagnosticLevel, DiagnosticsOptions, MemoryTransport,
};

let transport = MemoryTransport::default();
let _guard = subseq_tracing::init(DiagnosticsOptions {
    project_slug: "readysetapp".to_string(),
    service_name: "my-service".to_string(),
    environment: "production".to_string(),
    transport: Arc::new(transport.clone()),
    ..DiagnosticsOptions::default()
});

configure_scope(|scope| {
    scope.set_tag("projectId", "proj_abc");
    scope.set_tag("deployment", "canary");
});

with_scope(
    |scope| scope.set_tag("requestId", "req_123"),
    || {
        capture_message(DiagnosticLevel::Warning, "cache miss ratio increased");
    },
);

// Prefer `with_scope_async` when you need the scope to persist across `.await`.
// (This must run inside an `async fn`.)
with_scope_async(|scope| scope.set_tag("requestId", "req_456"), async {
    capture_message(DiagnosticLevel::Info, "async capture");
})
.await;

let err = anyhow::anyhow!("database timeout");
capture_error(err.as_ref());

let subscriber = tracing_subscriber::registry().with(diagnostics_layer());
```

## Integration Notes

- `init` installs a panic hook by default (`install_panic_hook = true`).
- `diagnostics_layer()` captures warn/error events.
- `diagnostics_layer_with_min_level(..)` can be used to include info/debug.
- `flush()` sends buffered events via the configured transport.

## Readysetapp Env Bootstrap

For service workloads that send diagnostics to readysetapp, use the env bootstrap helpers:

```rust
use subseq_tracing::readysetapp;

let layer = readysetapp::diagnostics_layer_from_env()?;
let _guard = readysetapp::init_from_env()?;
```

Supported env:
- `READYSETAPP_DIAGNOSTICS_ENDPOINT` (set to enable diagnostics)
- `READYSETAPP_PROJECT_SLUG`
- `READYSETAPP_SERVICE_NAME` (optional; defaults to project slug)
- `READYSETAPP_ENVIRONMENT`
- `READYSETAPP_RELEASE` (optional)
- `READYSETAPP_WORKLOAD_TOKEN_URL`
- `READYSETAPP_WORKLOAD_CLIENT_ID`
- `READYSETAPP_WORKLOAD_CLIENT_SECRET`
- `READYSETAPP_WORKLOAD_SCOPES` (optional, comma/whitespace-separated)
- `READYSETAPP_CA_CERT_PEM_PATH` (optional)
- `READYSETAPP_DIAGNOSTICS_LOG_PUMP_MIN_LEVEL` (optional: `debug|info|warning|warn|error|fatal|critical`, default `warning`)

## Workload JWT Pattern (readysetapp ingest)

- Use the readysetapp endpoint: `/api/v1/monitoring/ingest/diagnostics`.
- Configure `HttpTransportOptions.workload_jwt` (or `workload_jwt_provider`) for machine ingest auth.
- Register workload identities in readysetapp with selector combinations that match JWT claims:
  - required: `issuer` + (`clientId` or `subject`)
  - optional: `audience`
- For Cognito-style rotation, prefer `workload_jwt_provider` so each send can fetch a fresh token.

### Cognito Client-Credentials Provider

```rust
use std::sync::Arc;
use std::time::Duration;

use subseq_tracing::{
    CognitoClientCredentialsJwtProvider, HttpTransport, HttpTransportOptions,
    OAuthClientCredentialsJwtProviderOptions,
};

let provider = CognitoClientCredentialsJwtProvider::new(OAuthClientCredentialsJwtProviderOptions {
    token_url: "https://<pool-domain>.auth.<region>.amazoncognito.com/oauth2/token".to_string(),
    client_id: "<client_id>".to_string(),
    client_secret: "<client_secret>".to_string(),
    scopes: vec![],
    timeout: Duration::from_secs(5),
    ..OAuthClientCredentialsJwtProviderOptions::default()
})?;

let transport = HttpTransport::new(HttpTransportOptions {
    endpoint: "https://ingest.readysetapp.com/api/v1/monitoring/ingest/diagnostics".to_string(),
    workload_jwt_provider: Some(Arc::new(provider)),
    ..HttpTransportOptions::default()
})?;
# Ok::<(), subseq_tracing::TransportError>(())
```

### Custom Root CA (in-cluster dev HTTPS)

When readysetapp ingest is signed by a cluster-local cert-manager CA, add the CA bundle to the
HTTP transport root store:

```rust
use std::path::PathBuf;

use subseq_tracing::{HttpTransport, HttpTransportOptions};

let transport = HttpTransport::new(HttpTransportOptions {
    endpoint: "https://readysetapp-ingest.readysetapp.svc.cluster.local/api/v1/monitoring/ingest/diagnostics".to_string(),
    ca_cert_pem_path: Some(PathBuf::from("/var/run/subseq/cluster-ca.crt")),
    ..HttpTransportOptions::default()
})?;
# Ok::<(), subseq_tracing::TransportError>(())
```

## Transport

Implement `DiagnosticsTransport` to send packages to a backend.
Use `MemoryTransport` in tests.
