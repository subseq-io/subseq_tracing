use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serial_test::serial;
use subseq_tracing::{
    DiagnosticKind, DiagnosticLevel, DiagnosticPackage, DiagnosticsOptions, DiagnosticsTransport,
    MemoryTransport, ScopeSnapshot, TransportError, capture_error, capture_error_message,
    capture_warning, configure_scope, current_client, flush, init, with_scope,
};

#[derive(Default)]
struct FlakyTransport {
    fail_next: AtomicBool,
    packages: Mutex<Vec<DiagnosticPackage>>,
}

impl FlakyTransport {
    fn fail_once() -> Self {
        Self {
            fail_next: AtomicBool::new(true),
            packages: Mutex::new(Vec::new()),
        }
    }

    fn package_count(&self) -> usize {
        self.packages
            .lock()
            .expect("flaky transport poisoned")
            .len()
    }
}

impl DiagnosticsTransport for FlakyTransport {
    fn send(&self, package: DiagnosticPackage) -> Result<(), TransportError> {
        if self.fail_next.swap(false, Ordering::SeqCst) {
            return Err(TransportError::Message(
                "simulated transport failure".to_string(),
            ));
        }

        self.packages
            .lock()
            .map_err(|_| TransportError::Message("flaky transport poisoned".to_string()))?
            .push(package);
        Ok(())
    }
}

fn base_options(transport: Arc<dyn DiagnosticsTransport>) -> DiagnosticsOptions {
    DiagnosticsOptions {
        project_slug: "readysetapp".to_string(),
        service_name: "api".to_string(),
        environment: "test".to_string(),
        release: Some("2026.02.12".to_string()),
        max_events_per_package: 100,
        max_buffered_events: 500,
        auto_flush_on_error: true,
        install_panic_hook: false,
        transport,
    }
}

#[test]
#[serial]
fn scoped_tags_override_global_scope_for_capture() {
    let memory = MemoryTransport::default();
    let _guard = init(base_options(Arc::new(memory.clone())));

    configure_scope(|scope| {
        scope.set_tag("projectId", "global_project");
        scope.set_extra("globalHint", "visible");
    });

    with_scope(
        |scope| {
            scope.set_tag("projectId", "request_project");
            scope.set_tag("requestId", "req_123");
        },
        || {
            capture_warning("request warning");
        },
    );

    flush().expect("flush should succeed");

    let packages = memory.packages();
    assert_eq!(packages.len(), 1);
    assert_eq!(packages[0].events.len(), 1);

    let event = &packages[0].events[0];
    assert_eq!(
        event.scope.tags.get("projectId"),
        Some(&"request_project".to_string())
    );
    assert_eq!(
        event.scope.tags.get("requestId"),
        Some(&"req_123".to_string())
    );
    assert_eq!(
        event.scope.extras.get("globalHint"),
        Some(&serde_json::Value::String("visible".to_string()))
    );
}

#[test]
#[serial]
fn capture_error_includes_error_chain_and_kind() {
    let memory = MemoryTransport::default();
    let _guard = init(base_options(Arc::new(memory.clone())));

    let err = anyhow::anyhow!("database timeout")
        .context("saving dashboard")
        .context("request failed");

    let _id = capture_error(err.as_ref());
    flush().expect("flush should succeed");

    let packages = memory.packages();
    assert_eq!(packages.len(), 1);

    let event = &packages[0].events[0];
    assert_eq!(event.kind, DiagnosticKind::Error);
    assert_eq!(event.level, DiagnosticLevel::Error);

    let chain = event
        .fields
        .get("errorChain")
        .and_then(serde_json::Value::as_str)
        .expect("error chain should be present");
    assert!(chain.contains("request failed"));
    assert!(chain.contains("saving dashboard"));
    assert!(chain.contains("database timeout"));
}

#[test]
#[serial]
fn error_level_auto_flushes_without_explicit_flush_call() {
    let memory = MemoryTransport::default();
    let mut options = base_options(Arc::new(memory.clone()));
    options.max_events_per_package = 100;
    options.auto_flush_on_error = true;
    let _guard = init(options);

    capture_error_message("worker queue is unhealthy");

    let packages = memory.packages();
    assert_eq!(packages.len(), 1);
    assert_eq!(packages[0].event_count, 1);
    assert_eq!(packages[0].events[0].message, "worker queue is unhealthy");
}

#[test]
#[serial]
fn flush_requeues_events_after_transport_failure() {
    let flaky = Arc::new(FlakyTransport::fail_once());
    let mut options = base_options(flaky.clone());
    options.auto_flush_on_error = false;
    let _guard = init(options);

    capture_warning("first attempt should fail transport");

    let client = current_client().expect("client should be initialized");
    let first_flush = client.flush();
    assert!(first_flush.is_err());
    assert_eq!(flaky.package_count(), 0);

    let second_flush = client.flush().expect("second flush should succeed");
    assert_eq!(second_flush, 1);
    assert_eq!(flaky.package_count(), 1);
}

#[test]
#[serial]
fn ring_buffer_drops_oldest_events_when_full() {
    let memory = MemoryTransport::default();
    let mut options = base_options(Arc::new(memory.clone()));
    options.max_buffered_events = 2;
    options.max_events_per_package = 100;
    options.auto_flush_on_error = false;
    let _guard = init(options);

    capture_warning("one");
    capture_warning("two");
    capture_warning("three");

    let client = current_client().expect("client should be initialized");
    assert_eq!(client.dropped_events(), 1);

    flush().expect("flush should succeed");
    let packages = memory.packages();
    assert_eq!(packages.len(), 1);
    assert_eq!(packages[0].events.len(), 2);
    assert_eq!(packages[0].events[0].message, "two");
    assert_eq!(packages[0].events[1].message, "three");
}

#[test]
#[serial]
fn panic_hook_capture_emits_fatal_panic_event() {
    let memory = MemoryTransport::default();
    let mut options = base_options(Arc::new(memory.clone()));
    options.install_panic_hook = true;
    options.max_events_per_package = 1;
    let _guard = init(options);

    let _ = std::panic::catch_unwind(|| {
        panic!("panic-path smoke test");
    });

    let packages = memory.packages();
    assert!(!packages.is_empty());

    let has_panic = packages
        .iter()
        .flat_map(|package| package.events.iter())
        .any(|event| {
            event.kind == DiagnosticKind::Panic
                && event.level == DiagnosticLevel::Fatal
                && event.message.contains("panic-path smoke test")
        });

    assert!(has_panic);
}

#[test]
fn scope_snapshot_default_is_empty() {
    let snapshot = ScopeSnapshot::default();
    assert!(snapshot.tags.is_empty());
    assert!(snapshot.extras.is_empty());
    assert!(snapshot.user.is_none());
}
