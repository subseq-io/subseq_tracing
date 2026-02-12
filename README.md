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
    with_scope, DiagnosticLevel, DiagnosticsOptions, MemoryTransport,
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

let err = anyhow::anyhow!("database timeout");
capture_error(err.as_ref());

let subscriber = tracing_subscriber::registry().with(diagnostics_layer());
```

## Integration Notes

- `init` installs a panic hook by default (`install_panic_hook = true`).
- `diagnostics_layer()` captures warn/error events.
- `diagnostics_layer_with_min_level(..)` can be used to include info/debug.
- `flush()` sends buffered events via the configured transport.

## Transport

Implement `DiagnosticsTransport` to send packages to a backend.
Use `MemoryTransport` in tests.
