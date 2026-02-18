use std::env;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use thiserror::Error;

use crate::{
    ClientInitGuard, CognitoClientCredentialsJwtProvider, DiagnosticLevel, DiagnosticsLayer,
    DiagnosticsOptions, HttpTransport, HttpTransportOptions,
    OAuthClientCredentialsJwtProviderOptions, TransportError, init,
};

const ENV_DIAGNOSTICS_ENDPOINT: &str = "READYSETAPP_DIAGNOSTICS_ENDPOINT";
const ENV_PROJECT_SLUG: &str = "READYSETAPP_PROJECT_SLUG";
const ENV_SERVICE_NAME: &str = "READYSETAPP_SERVICE_NAME";
const ENV_ENVIRONMENT: &str = "READYSETAPP_ENVIRONMENT";
const ENV_RELEASE: &str = "READYSETAPP_RELEASE";
const ENV_WORKLOAD_TOKEN_URL: &str = "READYSETAPP_WORKLOAD_TOKEN_URL";
const ENV_WORKLOAD_CLIENT_ID: &str = "READYSETAPP_WORKLOAD_CLIENT_ID";
const ENV_WORKLOAD_CLIENT_SECRET: &str = "READYSETAPP_WORKLOAD_CLIENT_SECRET";
const ENV_WORKLOAD_SCOPES: &str = "READYSETAPP_WORKLOAD_SCOPES";
const ENV_CA_CERT_PEM_PATH: &str = "READYSETAPP_CA_CERT_PEM_PATH";
const ENV_LOG_PUMP_MIN_LEVEL: &str = "READYSETAPP_DIAGNOSTICS_LOG_PUMP_MIN_LEVEL";

const DEFAULT_HTTP_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Error)]
pub enum ReadysetappDiagnosticsEnvError {
    #[error("{0} contains non-unicode data")]
    NonUnicodeEnv(&'static str),
    #[error("{0} must be non-empty when set")]
    EmptyEnv(&'static str),
    #[error("{0} is required when {1} is set")]
    MissingRequiredEnv(&'static str, &'static str),
    #[error(
        "invalid {0} value '{1}'; expected one of debug|info|warning|warn|error|fatal|critical"
    )]
    InvalidEnumValue(&'static str, String),
    #[error("invalid readysetapp workload oauth config: {0}")]
    InvalidOAuth(#[source] TransportError),
    #[error("invalid readysetapp diagnostics transport config: {0}")]
    InvalidTransport(#[source] TransportError),
}

/// Build a diagnostics layer for tracing log pump capture.
///
/// Env:
/// - `READYSETAPP_DIAGNOSTICS_LOG_PUMP_MIN_LEVEL` (optional, default: `warning`)
pub fn diagnostics_layer_from_env() -> Result<DiagnosticsLayer, ReadysetappDiagnosticsEnvError> {
    let min_level = read_log_pump_min_level()?;
    Ok(DiagnosticsLayer::new(min_level))
}

/// Build diagnostics options from Readysetapp workload env.
///
/// Returns `Ok(None)` when diagnostics are disabled (no endpoint configured).
///
/// Required when endpoint is configured:
/// - `READYSETAPP_PROJECT_SLUG`
/// - `READYSETAPP_ENVIRONMENT`
/// - `READYSETAPP_WORKLOAD_TOKEN_URL`
/// - `READYSETAPP_WORKLOAD_CLIENT_ID`
/// - `READYSETAPP_WORKLOAD_CLIENT_SECRET`
///
/// Optional:
/// - `READYSETAPP_SERVICE_NAME` (defaults to project slug)
/// - `READYSETAPP_RELEASE`
/// - `READYSETAPP_WORKLOAD_SCOPES` (comma or whitespace-separated)
/// - `READYSETAPP_CA_CERT_PEM_PATH`
pub fn diagnostics_options_from_env()
-> Result<Option<DiagnosticsOptions>, ReadysetappDiagnosticsEnvError> {
    let endpoint = match read_non_empty_optional_env(ENV_DIAGNOSTICS_ENDPOINT)? {
        Some(value) => value,
        None => return Ok(None),
    };

    let project_slug = read_required_non_empty_env(ENV_PROJECT_SLUG)?;
    let service_name =
        read_non_empty_optional_env(ENV_SERVICE_NAME)?.unwrap_or_else(|| project_slug.clone());
    let environment = read_required_non_empty_env(ENV_ENVIRONMENT)?;
    let release = read_non_empty_optional_env(ENV_RELEASE)?;

    let token_url = read_required_non_empty_env(ENV_WORKLOAD_TOKEN_URL)?;
    let client_id = read_required_non_empty_env(ENV_WORKLOAD_CLIENT_ID)?;
    let client_secret = read_required_non_empty_env(ENV_WORKLOAD_CLIENT_SECRET)?;
    let scopes = parse_scopes(
        read_non_empty_optional_env(ENV_WORKLOAD_SCOPES)?
            .as_deref()
            .unwrap_or(""),
    );

    let ca_cert_pem_path = read_non_empty_optional_env(ENV_CA_CERT_PEM_PATH)?.map(PathBuf::from);

    let provider =
        CognitoClientCredentialsJwtProvider::new(OAuthClientCredentialsJwtProviderOptions {
            token_url,
            client_id,
            client_secret,
            scopes,
            timeout: DEFAULT_HTTP_TIMEOUT,
            ..OAuthClientCredentialsJwtProviderOptions::default()
        })
        .map_err(ReadysetappDiagnosticsEnvError::InvalidOAuth)?;

    let transport = HttpTransport::new(HttpTransportOptions {
        endpoint,
        workload_jwt_provider: Some(Arc::new(provider)),
        ca_cert_pem_path,
        timeout: DEFAULT_HTTP_TIMEOUT,
        ..HttpTransportOptions::default()
    })
    .map_err(ReadysetappDiagnosticsEnvError::InvalidTransport)?;

    Ok(Some(DiagnosticsOptions {
        project_slug,
        service_name,
        environment,
        release,
        transport: Arc::new(transport),
        ..DiagnosticsOptions::default()
    }))
}

/// Initialize the global diagnostics client from Readysetapp workload env.
///
/// Returns `Ok(None)` when diagnostics are disabled (no endpoint configured).
pub fn init_from_env() -> Result<Option<ClientInitGuard>, ReadysetappDiagnosticsEnvError> {
    let options = diagnostics_options_from_env()?;
    Ok(options.map(init))
}

fn read_log_pump_min_level() -> Result<DiagnosticLevel, ReadysetappDiagnosticsEnvError> {
    let Some(raw) = read_non_empty_optional_env(ENV_LOG_PUMP_MIN_LEVEL)? else {
        return Ok(DiagnosticLevel::Info);
    };
    parse_diagnostic_level(ENV_LOG_PUMP_MIN_LEVEL, &raw)
}

fn parse_diagnostic_level(
    key: &'static str,
    raw: &str,
) -> Result<DiagnosticLevel, ReadysetappDiagnosticsEnvError> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "debug" => Ok(DiagnosticLevel::Debug),
        "info" => Ok(DiagnosticLevel::Info),
        "warning" | "warn" => Ok(DiagnosticLevel::Warning),
        "error" => Ok(DiagnosticLevel::Error),
        "fatal" | "critical" => Ok(DiagnosticLevel::Fatal),
        _ => Err(ReadysetappDiagnosticsEnvError::InvalidEnumValue(
            key,
            raw.to_string(),
        )),
    }
}

fn parse_scopes(raw: &str) -> Vec<String> {
    raw.split(|c: char| c.is_whitespace() || c == ',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn read_required_non_empty_env(
    key: &'static str,
) -> Result<String, ReadysetappDiagnosticsEnvError> {
    read_non_empty_optional_env(key)?.ok_or(ReadysetappDiagnosticsEnvError::MissingRequiredEnv(
        key,
        ENV_DIAGNOSTICS_ENDPOINT,
    ))
}

fn read_non_empty_optional_env(
    key: &'static str,
) -> Result<Option<String>, ReadysetappDiagnosticsEnvError> {
    match env::var(key) {
        Ok(raw) => {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                return Err(ReadysetappDiagnosticsEnvError::EmptyEnv(key));
            }
            Ok(Some(trimmed.to_string()))
        }
        Err(env::VarError::NotPresent) => Ok(None),
        Err(env::VarError::NotUnicode(_)) => {
            Err(ReadysetappDiagnosticsEnvError::NonUnicodeEnv(key))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ENV_DIAGNOSTICS_ENDPOINT, ENV_ENVIRONMENT, ENV_LOG_PUMP_MIN_LEVEL, ENV_PROJECT_SLUG,
        ENV_WORKLOAD_CLIENT_ID, ENV_WORKLOAD_CLIENT_SECRET, ENV_WORKLOAD_TOKEN_URL,
        ReadysetappDiagnosticsEnvError, diagnostics_options_from_env, parse_diagnostic_level,
        parse_scopes, read_log_pump_min_level,
    };
    use serial_test::serial;

    struct EnvGuard {
        entries: Vec<(&'static str, Option<std::ffi::OsString>)>,
    }

    impl EnvGuard {
        fn set(overrides: &[(&'static str, Option<&str>)]) -> Self {
            let mut entries = Vec::with_capacity(overrides.len());
            for (key, value) in overrides {
                entries.push((*key, std::env::var_os(key)));
                match value {
                    Some(content) => {
                        // SAFETY: Tests are marked with `serial` so environment mutation is isolated.
                        unsafe { std::env::set_var(key, content) }
                    }
                    None => {
                        // SAFETY: Tests are marked with `serial` so environment mutation is isolated.
                        unsafe { std::env::remove_var(key) }
                    }
                }
            }
            Self { entries }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (key, value) in self.entries.drain(..).rev() {
                match value {
                    Some(content) => {
                        // SAFETY: Tests are marked with `serial` so environment mutation is isolated.
                        unsafe { std::env::set_var(key, content) }
                    }
                    None => {
                        // SAFETY: Tests are marked with `serial` so environment mutation is isolated.
                        unsafe { std::env::remove_var(key) }
                    }
                }
            }
        }
    }

    #[test]
    fn parse_scopes_handles_commas_and_whitespace() {
        let parsed = parse_scopes("a,b c\t d");
        assert_eq!(parsed, vec!["a", "b", "c", "d"]);
    }

    #[test]
    fn parse_diagnostic_level_accepts_aliases() {
        let warning = parse_diagnostic_level("LEVEL", "warn").expect("warn should parse");
        let critical = parse_diagnostic_level("LEVEL", "critical").expect("critical should parse");
        assert!(matches!(warning, super::DiagnosticLevel::Warning));
        assert!(matches!(critical, super::DiagnosticLevel::Fatal));
    }

    #[test]
    #[serial]
    fn options_are_disabled_when_endpoint_is_missing() {
        let _guard = EnvGuard::set(&[(ENV_DIAGNOSTICS_ENDPOINT, None)]);
        let options = diagnostics_options_from_env().expect("missing endpoint should not fail");
        assert!(options.is_none());
    }

    #[test]
    #[serial]
    fn options_require_project_slug_when_endpoint_is_present() {
        let _guard = EnvGuard::set(&[
            (
                ENV_DIAGNOSTICS_ENDPOINT,
                Some("http://localhost:3000/ingest"),
            ),
            (ENV_PROJECT_SLUG, None),
        ]);
        let err = diagnostics_options_from_env().expect_err("missing project slug should fail");
        assert!(matches!(
            err,
            ReadysetappDiagnosticsEnvError::MissingRequiredEnv(
                ENV_PROJECT_SLUG,
                ENV_DIAGNOSTICS_ENDPOINT
            )
        ));
    }

    #[test]
    #[serial]
    fn options_parse_required_readysetapp_env() {
        let _guard = EnvGuard::set(&[
            (ENV_DIAGNOSTICS_ENDPOINT, Some("https://example.com/ingest")),
            (ENV_PROJECT_SLUG, Some("local")),
            (ENV_ENVIRONMENT, Some("dev")),
            (
                ENV_WORKLOAD_TOKEN_URL,
                Some("https://example.com/oauth2/token"),
            ),
            (ENV_WORKLOAD_CLIENT_ID, Some("client-id")),
            (ENV_WORKLOAD_CLIENT_SECRET, Some("client-secret")),
        ]);

        let options = diagnostics_options_from_env()
            .expect("env should parse")
            .expect("endpoint enables diagnostics");
        assert_eq!(options.project_slug, "local");
        assert_eq!(options.service_name, "local");
        assert_eq!(options.environment, "dev");
        assert_eq!(options.release, None);
    }

    #[test]
    #[serial]
    fn log_pump_level_defaults_to_info() {
        let _guard = EnvGuard::set(&[(ENV_LOG_PUMP_MIN_LEVEL, None)]);
        let level = read_log_pump_min_level().expect("default level should parse");
        assert!(matches!(level, super::DiagnosticLevel::Info));
    }

    #[test]
    #[serial]
    fn log_pump_level_rejects_invalid_value() {
        let _guard = EnvGuard::set(&[(ENV_LOG_PUMP_MIN_LEVEL, Some("verbose"))]);
        let err = read_log_pump_min_level().expect_err("invalid level should fail");
        assert!(matches!(
            err,
            ReadysetappDiagnosticsEnvError::InvalidEnumValue(ENV_LOG_PUMP_MIN_LEVEL, _)
        ));
    }
}
