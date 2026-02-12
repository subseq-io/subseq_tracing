use std::sync::Arc;

use serial_test::serial;
use subseq_tracing::{
    DiagnosticKind, DiagnosticLevel, DiagnosticsLayer, DiagnosticsOptions, MemoryTransport, flush,
    init,
};
use tracing_subscriber::{prelude::*, registry::Registry};

fn base_options(memory: &MemoryTransport) -> DiagnosticsOptions {
    DiagnosticsOptions {
        project_slug: "readysetapp".to_string(),
        service_name: "api".to_string(),
        environment: "test".to_string(),
        release: Some("2026.02.12".to_string()),
        max_events_per_package: 100,
        max_buffered_events: 500,
        auto_flush_on_error: true,
        install_panic_hook: false,
        transport: Arc::new(memory.clone()),
    }
}

#[test]
#[serial]
fn diagnostics_layer_captures_warning_fields_and_target() {
    let memory = MemoryTransport::default();
    let mut options = base_options(&memory);
    options.auto_flush_on_error = false;
    let _guard = init(options);

    let subscriber = Registry::default().with(DiagnosticsLayer::new(DiagnosticLevel::Warning));
    tracing::dispatcher::with_default(&tracing::Dispatch::new(subscriber), || {
        tracing::warn!(
            feature = "search",
            attempt = 3_u64,
            "query latency is elevated"
        );
    });

    flush().expect("flush should succeed");
    let packages = memory.packages();
    assert_eq!(packages.len(), 1);
    assert_eq!(packages[0].events.len(), 1);

    let event = &packages[0].events[0];
    assert_eq!(event.kind, DiagnosticKind::Tracing);
    assert_eq!(event.level, DiagnosticLevel::Warning);
    assert_eq!(event.message, "query latency is elevated");
    assert_eq!(
        event.fields.get("feature"),
        Some(&serde_json::Value::String("search".to_string()))
    );
    assert_eq!(
        event.fields.get("attempt"),
        Some(&serde_json::Value::Number(3_u64.into()))
    );
    assert!(event.fields.get("target").is_some());
}

#[test]
#[serial]
fn diagnostics_layer_respects_min_level() {
    let memory = MemoryTransport::default();
    let mut options = base_options(&memory);
    options.auto_flush_on_error = false;
    let _guard = init(options);

    let subscriber = Registry::default().with(DiagnosticsLayer::new(DiagnosticLevel::Error));
    tracing::dispatcher::with_default(&tracing::Dispatch::new(subscriber), || {
        tracing::warn!("warn should be filtered");
        tracing::error!("error should be captured");
    });

    flush().expect("flush should succeed");
    let packages = memory.packages();
    assert_eq!(packages.len(), 1);
    assert_eq!(packages[0].events.len(), 1);
    assert_eq!(packages[0].events[0].message, "error should be captured");
    assert_eq!(packages[0].events[0].level, DiagnosticLevel::Error);
}
