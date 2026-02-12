use serde_json::{Map, Value};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::{
    Layer,
    layer::{Context, Filter},
    registry::LookupSpan,
};

use crate::{
    capture_message_with_fields,
    diagnostics::{DiagnosticKind, DiagnosticLevel},
};

#[derive(Debug, Clone)]
pub struct DiagnosticsLayer {
    min_level: DiagnosticLevel,
}

impl DiagnosticsLayer {
    pub fn new(min_level: DiagnosticLevel) -> Self {
        Self { min_level }
    }
}

impl<S> Layer<S> for DiagnosticsLayer
where
    S: Subscriber + for<'span> LookupSpan<'span>,
{
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let Some(level) = map_level(*event.metadata().level()) else {
            return;
        };

        if level_rank(level) < level_rank(self.min_level) {
            return;
        }

        let mut visitor = JsonFieldVisitor::default();
        event.record(&mut visitor);

        let message = visitor
            .message
            .unwrap_or_else(|| event.metadata().name().to_string());

        visitor.fields.insert(
            "target".to_string(),
            Value::String(event.metadata().target().to_string()),
        );

        capture_message_with_fields(level, DiagnosticKind::Tracing, message, visitor.fields);
    }
}

impl<S> Filter<S> for DiagnosticsLayer
where
    S: Subscriber + for<'span> LookupSpan<'span>,
{
    fn enabled(&self, metadata: &tracing::Metadata<'_>, _ctx: &Context<'_, S>) -> bool {
        map_level(*metadata.level()).is_some()
    }
}

#[derive(Debug, Default)]
struct JsonFieldVisitor {
    message: Option<String>,
    fields: Map<String, Value>,
}

impl tracing::field::Visit for JsonFieldVisitor {
    fn record_bool(&mut self, field: &tracing::field::Field, value: bool) {
        self.fields
            .insert(field.name().to_string(), Value::Bool(value));
    }

    fn record_i64(&mut self, field: &tracing::field::Field, value: i64) {
        self.fields
            .insert(field.name().to_string(), Value::Number(value.into()));
    }

    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        self.fields
            .insert(field.name().to_string(), Value::Number(value.into()));
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "message" {
            self.message = Some(value.to_string());
        } else {
            self.fields
                .insert(field.name().to_string(), Value::String(value.to_string()));
        }
    }

    fn record_error(
        &mut self,
        field: &tracing::field::Field,
        value: &(dyn std::error::Error + 'static),
    ) {
        self.fields.insert(
            field.name().to_string(),
            Value::String(format!("{value:#}")),
        );
    }

    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        self.fields.insert(
            field.name().to_string(),
            Value::String(format!("{value:?}")),
        );
    }
}

fn map_level(level: Level) -> Option<DiagnosticLevel> {
    match level {
        Level::TRACE | Level::DEBUG => Some(DiagnosticLevel::Debug),
        Level::INFO => Some(DiagnosticLevel::Info),
        Level::WARN => Some(DiagnosticLevel::Warning),
        Level::ERROR => Some(DiagnosticLevel::Error),
    }
}

fn level_rank(level: DiagnosticLevel) -> u8 {
    match level {
        DiagnosticLevel::Debug => 10,
        DiagnosticLevel::Info => 20,
        DiagnosticLevel::Warning => 30,
        DiagnosticLevel::Error => 40,
        DiagnosticLevel::Fatal => 50,
    }
}
