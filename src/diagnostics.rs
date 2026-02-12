use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticLevel {
    Debug,
    Info,
    Warning,
    Error,
    Fatal,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticKind {
    Message,
    Error,
    Panic,
    Tracing,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TraceContext {
    pub trace_id: String,
    pub span_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct UserContext {
    pub id: Option<String>,
    pub email: Option<String>,
    pub username: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ScopeSnapshot {
    #[serde(default)]
    pub tags: BTreeMap<String, String>,
    #[serde(default)]
    pub extras: Map<String, Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user: Option<UserContext>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticEvent {
    pub id: Uuid,
    pub occurred_at: DateTime<Utc>,
    pub level: DiagnosticLevel,
    pub kind: DiagnosticKind,
    pub logger: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace: Option<TraceContext>,
    pub scope: ScopeSnapshot,
    #[serde(default)]
    pub fields: Map<String, Value>,
}

impl DiagnosticEvent {
    pub fn new(
        level: DiagnosticLevel,
        kind: DiagnosticKind,
        logger: impl Into<String>,
        message: impl Into<String>,
        scope: ScopeSnapshot,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            occurred_at: Utc::now(),
            level,
            kind,
            logger: logger.into(),
            message: message.into(),
            trace: None,
            scope,
            fields: Map::new(),
        }
    }

    pub fn with_trace(mut self, trace: Option<TraceContext>) -> Self {
        self.trace = trace;
        self
    }

    pub fn with_fields(mut self, fields: Map<String, Value>) -> Self {
        self.fields = fields;
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticPackage {
    pub package_id: Uuid,
    pub generated_at: DateTime<Utc>,
    pub project_slug: String,
    pub service_name: String,
    pub environment: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub release: Option<String>,
    pub event_count: usize,
    pub events: Vec<DiagnosticEvent>,
}
