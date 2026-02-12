use std::error::Error;
use std::panic::PanicHookInfo;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use once_cell::sync::Lazy;
use opentelemetry::trace::TraceContextExt;
use serde_json::{Map, Value};
use tracing_opentelemetry::OpenTelemetrySpanExt;
use uuid::Uuid;

use crate::diagnostics::{
    DiagnosticEvent, DiagnosticKind, DiagnosticLevel, DiagnosticPackage, ScopeSnapshot,
    TraceContext,
};
use crate::scope::{Scope, current_local_scope};
use crate::transport::{DiagnosticsTransport, NoopTransport, TransportError};

static GLOBAL_CLIENT: Lazy<RwLock<Option<Arc<DiagnosticsClient>>>> =
    Lazy::new(|| RwLock::new(None));
static PANIC_HOOK_INSTALLED: AtomicBool = AtomicBool::new(false);

thread_local! {
    static PANIC_CAPTURE_GUARD: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[derive(Clone)]
pub struct DiagnosticsOptions {
    pub project_slug: String,
    pub service_name: String,
    pub environment: String,
    pub release: Option<String>,
    pub max_events_per_package: usize,
    pub max_buffered_events: usize,
    pub auto_flush_on_error: bool,
    pub install_panic_hook: bool,
    pub transport: Arc<dyn DiagnosticsTransport>,
}

impl std::fmt::Debug for DiagnosticsOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DiagnosticsOptions")
            .field("project_slug", &self.project_slug)
            .field("service_name", &self.service_name)
            .field("environment", &self.environment)
            .field("release", &self.release)
            .field("max_events_per_package", &self.max_events_per_package)
            .field("max_buffered_events", &self.max_buffered_events)
            .field("auto_flush_on_error", &self.auto_flush_on_error)
            .field("install_panic_hook", &self.install_panic_hook)
            .field("transport", &"<dyn DiagnosticsTransport>")
            .finish()
    }
}

impl Default for DiagnosticsOptions {
    fn default() -> Self {
        Self {
            project_slug: "unknown-project".to_string(),
            service_name: "unknown-service".to_string(),
            environment: "development".to_string(),
            release: None,
            max_events_per_package: 50,
            max_buffered_events: 500,
            auto_flush_on_error: true,
            install_panic_hook: true,
            transport: Arc::new(NoopTransport),
        }
    }
}

pub struct ClientInitGuard {
    flush_on_drop: bool,
}

impl Drop for ClientInitGuard {
    fn drop(&mut self) {
        if self.flush_on_drop {
            let _ = flush();
        }
    }
}

#[derive(Debug)]
pub struct DiagnosticsClient {
    options: DiagnosticsOptions,
    global_scope: RwLock<Scope>,
    queue: Mutex<Vec<DiagnosticEvent>>,
    dropped_events: AtomicU64,
}

impl DiagnosticsClient {
    fn new(options: DiagnosticsOptions) -> Self {
        Self {
            options,
            global_scope: RwLock::new(Scope::default()),
            queue: Mutex::new(Vec::new()),
            dropped_events: AtomicU64::new(0),
        }
    }

    pub fn options(&self) -> &DiagnosticsOptions {
        &self.options
    }

    pub fn configure_scope(&self, callback: impl FnOnce(&mut Scope)) {
        let mut scope = self
            .global_scope
            .write()
            .expect("global diagnostics scope poisoned");
        callback(&mut scope);
    }

    pub fn dropped_events(&self) -> u64 {
        self.dropped_events.load(Ordering::Relaxed)
    }

    fn snapshot_scope(&self) -> ScopeSnapshot {
        let global_scope = self
            .global_scope
            .read()
            .expect("global diagnostics scope poisoned")
            .clone();
        let local_scope = current_local_scope();

        let mut snapshot = ScopeSnapshot::default();
        global_scope.merge_into(&mut snapshot);
        if let Some(local_scope) = local_scope {
            local_scope.merge_into(&mut snapshot);
        }

        snapshot
    }

    fn capture(&self, mut event: DiagnosticEvent) -> Uuid {
        event.logger = self.options.service_name.clone();
        let event_id = event.id;

        let should_flush = {
            let mut queue = self.queue.lock().expect("diagnostics queue poisoned");
            if queue.len() >= self.options.max_buffered_events {
                queue.remove(0);
                self.dropped_events.fetch_add(1, Ordering::Relaxed);
            }
            queue.push(event.clone());

            queue.len() >= self.options.max_events_per_package
                || (self.options.auto_flush_on_error
                    && matches!(event.level, DiagnosticLevel::Error | DiagnosticLevel::Fatal))
        };

        if should_flush {
            let _ = self.flush();
        }

        event_id
    }

    pub fn flush(&self) -> Result<usize, TransportError> {
        let drained = {
            let mut queue = self.queue.lock().expect("diagnostics queue poisoned");
            if queue.is_empty() {
                return Ok(0);
            }
            std::mem::take(&mut *queue)
        };

        let sent_count = drained.len();
        let package = DiagnosticPackage {
            package_id: Uuid::new_v4(),
            generated_at: chrono::Utc::now(),
            project_slug: self.options.project_slug.clone(),
            service_name: self.options.service_name.clone(),
            environment: self.options.environment.clone(),
            release: self.options.release.clone(),
            event_count: sent_count,
            events: drained.clone(),
        };

        match self.options.transport.send(package) {
            Ok(()) => Ok(sent_count),
            Err(err) => {
                let mut queue = self.queue.lock().expect("diagnostics queue poisoned");
                let mut combined = drained;
                combined.append(&mut *queue);

                if combined.len() > self.options.max_buffered_events {
                    let overflow = combined.len() - self.options.max_buffered_events;
                    combined.drain(0..overflow);
                    self.dropped_events
                        .fetch_add(overflow as u64, Ordering::Relaxed);
                }

                *queue = combined;
                Err(err)
            }
        }
    }
}

pub fn init(options: DiagnosticsOptions) -> ClientInitGuard {
    let install_panic_hook = options.install_panic_hook;
    let client = Arc::new(DiagnosticsClient::new(options));

    {
        let mut global = GLOBAL_CLIENT.write().expect("global client lock poisoned");
        *global = Some(client);
    }

    if install_panic_hook {
        install_global_panic_hook();
    }

    ClientInitGuard {
        flush_on_drop: true,
    }
}

pub fn current_client() -> Option<Arc<DiagnosticsClient>> {
    GLOBAL_CLIENT
        .read()
        .expect("global client lock poisoned")
        .as_ref()
        .cloned()
}

pub fn configure_scope(callback: impl FnOnce(&mut Scope)) {
    if let Some(client) = current_client() {
        client.configure_scope(callback);
    }
}

pub fn capture_message(level: DiagnosticLevel, message: impl Into<String>) -> Uuid {
    capture_message_with_fields(level, DiagnosticKind::Message, message, Map::new())
}

pub fn capture_warning(message: impl Into<String>) -> Uuid {
    capture_message(DiagnosticLevel::Warning, message)
}

pub fn capture_error_message(message: impl Into<String>) -> Uuid {
    capture_message(DiagnosticLevel::Error, message)
}

pub fn capture_error(error: &(dyn Error + 'static)) -> Uuid {
    let mut fields = Map::new();
    fields.insert("errorChain".to_string(), Value::String(error_chain(error)));

    capture_message_with_fields(
        DiagnosticLevel::Error,
        DiagnosticKind::Error,
        error.to_string(),
        fields,
    )
}

fn error_chain(error: &(dyn Error + 'static)) -> String {
    let mut parts = Vec::new();
    parts.push(error.to_string());

    let mut current = error.source();
    while let Some(source) = current {
        parts.push(source.to_string());
        current = source.source();
    }

    parts.join(": ")
}

pub fn capture_message_with_fields(
    level: DiagnosticLevel,
    kind: DiagnosticKind,
    message: impl Into<String>,
    fields: Map<String, Value>,
) -> Uuid {
    let message = message.into();

    let Some(client) = current_client() else {
        return Uuid::nil();
    };

    let scope = client.snapshot_scope();
    let trace = current_trace_context();

    let event = DiagnosticEvent::new(
        level,
        kind,
        client.options.service_name.clone(),
        message,
        scope,
    )
    .with_trace(trace)
    .with_fields(fields);

    client.capture(event)
}

pub fn flush() -> Result<(), TransportError> {
    if let Some(client) = current_client() {
        client.flush()?;
    }
    Ok(())
}

fn install_global_panic_hook() {
    if PANIC_HOOK_INSTALLED
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return;
    }

    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        PANIC_CAPTURE_GUARD.with(|guard| {
            if guard.get() {
                previous_hook(panic_info);
                return;
            }

            guard.set(true);
            capture_panic(panic_info);
            guard.set(false);
            previous_hook(panic_info);
        });
    }));
}

fn capture_panic(panic_info: &PanicHookInfo<'_>) {
    let Some(client) = current_client() else {
        return;
    };

    let message = extract_panic_message(panic_info);
    let mut fields = Map::new();

    if let Some(location) = panic_info.location() {
        fields.insert(
            "panicLocation".to_string(),
            Value::String(format!(
                "{}:{}:{}",
                location.file(),
                location.line(),
                location.column()
            )),
        );
    }

    fields.insert(
        "backtrace".to_string(),
        Value::String(std::backtrace::Backtrace::force_capture().to_string()),
    );

    let scope = client.snapshot_scope();
    let trace = current_trace_context();
    let event = DiagnosticEvent::new(
        DiagnosticLevel::Fatal,
        DiagnosticKind::Panic,
        client.options.service_name.clone(),
        message,
        scope,
    )
    .with_trace(trace)
    .with_fields(fields);

    client.capture(event);
    let _ = client.flush();
}

fn extract_panic_message(panic_info: &PanicHookInfo<'_>) -> String {
    if let Some(message) = panic_info.payload().downcast_ref::<&str>() {
        return (*message).to_string();
    }
    if let Some(message) = panic_info.payload().downcast_ref::<String>() {
        return message.clone();
    }
    "panic captured".to_string()
}

fn current_trace_context() -> Option<TraceContext> {
    let span = tracing::Span::current();
    if span.id().is_none() {
        return None;
    }

    let context = span.context();
    let span_ref = context.span();
    let span_context = span_ref.span_context();

    if !span_context.is_valid() {
        return None;
    }

    Some(TraceContext {
        trace_id: span_context.trace_id().to_string(),
        span_id: span_context.span_id().to_string(),
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::diagnostics::DiagnosticLevel;
    use crate::transport::MemoryTransport;

    use super::{DiagnosticsOptions, capture_message, configure_scope, flush, init};

    #[test]
    fn captures_scoped_messages_into_memory_transport() {
        let memory = MemoryTransport::default();
        let options = DiagnosticsOptions {
            project_slug: "readysetapp".to_string(),
            service_name: "api".to_string(),
            transport: Arc::new(memory.clone()),
            ..DiagnosticsOptions::default()
        };

        let _guard = init(options);
        configure_scope(|scope| {
            scope.set_tag("projectId", "proj_123");
        });

        let _event_id = capture_message(DiagnosticLevel::Warning, "disk is near capacity");
        flush().expect("flush should succeed");

        let packages = memory.packages();
        assert_eq!(packages.len(), 1);
        assert_eq!(packages[0].events.len(), 1);
        assert_eq!(
            packages[0].events[0].scope.tags.get("projectId"),
            Some(&"proj_123".to_string())
        );
        assert_eq!(packages[0].events[0].message, "disk is near capacity");
    }
}
