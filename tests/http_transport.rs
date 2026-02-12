use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread;
use std::time::Duration;

use chrono::Utc;
use serde_json::json;
use subseq_tracing::{
    DiagnosticEvent, DiagnosticKind, DiagnosticLevel, DiagnosticPackage, DiagnosticsTransport,
    HttpTransport, HttpTransportOptions, ScopeSnapshot, TransportError,
};
use uuid::Uuid;

#[derive(Debug)]
struct CapturedRequest {
    method: String,
    path: String,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
}

#[test]
fn http_transport_rejects_invalid_endpoints() {
    let mut empty_endpoint = HttpTransportOptions::default();
    empty_endpoint.endpoint = " ".to_string();
    let err = HttpTransport::new(empty_endpoint).expect_err("empty endpoint should fail");
    assert!(err.to_string().contains("must be non-empty"));

    let mut invalid_scheme = HttpTransportOptions::default();
    invalid_scheme.endpoint = "ftp://localhost:3000/ingest".to_string();
    let err = HttpTransport::new(invalid_scheme).expect_err("invalid scheme should fail");
    assert!(
        err.to_string()
            .contains("must start with http:// or https://")
    );
}

#[test]
fn http_transport_sends_expected_headers_and_payload() {
    let (endpoint, handle) = spawn_one_shot_server("200 OK", r#"{"ok":true}"#);

    let mut options = HttpTransportOptions {
        endpoint,
        bearer_token: Some("test-workload-token".to_string()),
        timeout: Duration::from_secs(2),
        user_agent: "subseq-tracing-test/1.0".to_string(),
        headers: BTreeMap::new(),
    };
    options
        .headers
        .insert("x-subseq-project".to_string(), "readysetapp".to_string());

    let transport = HttpTransport::new(options).expect("http transport should build");
    let package = fixture_package();

    transport
        .send(package.clone())
        .expect("send should succeed");

    let request = handle.join().expect("server thread should join");
    assert_eq!(request.method, "POST");
    assert_eq!(request.path, "/ingest/diagnostics");
    assert_eq!(
        request.headers.get("authorization"),
        Some(&"Bearer test-workload-token".to_string())
    );
    assert_eq!(
        request.headers.get("x-subseq-project"),
        Some(&"readysetapp".to_string())
    );
    assert_eq!(
        request.headers.get("user-agent"),
        Some(&"subseq-tracing-test/1.0".to_string())
    );
    assert_eq!(
        request.headers.get("content-type"),
        Some(&"application/json".to_string())
    );

    let decoded: DiagnosticPackage =
        serde_json::from_slice(&request.body).expect("payload should decode");
    assert_eq!(decoded, package);
}

#[test]
fn http_transport_returns_status_error_body() {
    let (endpoint, handle) = spawn_one_shot_server("401 Unauthorized", r#"{"error":"denied"}"#);

    let options = HttpTransportOptions {
        endpoint,
        timeout: Duration::from_secs(2),
        ..HttpTransportOptions::default()
    };
    let transport = HttpTransport::new(options).expect("http transport should build");

    let err = transport
        .send(fixture_package())
        .expect_err("status failures should return an error");

    match err {
        TransportError::Message(message) => {
            assert!(message.contains("http status 401"));
            assert!(message.contains("denied"));
        }
    }

    handle.join().expect("server thread should join");
}

#[test]
fn http_transport_returns_transport_error_when_unreachable() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("port allocation should succeed");
    let addr = listener
        .local_addr()
        .expect("listener should have local addr");
    drop(listener);

    let options = HttpTransportOptions {
        endpoint: format!("http://{addr}/ingest/diagnostics"),
        timeout: Duration::from_millis(200),
        ..HttpTransportOptions::default()
    };
    let transport = HttpTransport::new(options).expect("http transport should build");

    let err = transport
        .send(fixture_package())
        .expect_err("unreachable endpoint should fail");

    let message = err.to_string();
    assert!(message.contains("http transport failure"));
}

fn fixture_package() -> DiagnosticPackage {
    DiagnosticPackage {
        package_id: Uuid::new_v4(),
        generated_at: Utc::now(),
        project_slug: "readysetapp".to_string(),
        service_name: "api".to_string(),
        environment: "test".to_string(),
        release: Some("2026.02.12".to_string()),
        event_count: 1,
        events: vec![DiagnosticEvent {
            id: Uuid::new_v4(),
            occurred_at: Utc::now(),
            level: DiagnosticLevel::Error,
            kind: DiagnosticKind::Error,
            logger: "api".to_string(),
            message: "request failed".to_string(),
            trace: None,
            scope: ScopeSnapshot::default(),
            fields: serde_json::Map::from_iter([(
                "errorChain".to_string(),
                json!("request failed: database timeout"),
            )]),
        }],
    }
}

fn spawn_one_shot_server(
    status_line: &'static str,
    response_body: &'static str,
) -> (String, thread::JoinHandle<CapturedRequest>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("server bind should succeed");
    let addr = listener
        .local_addr()
        .expect("listener should have local addr");

    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("server should accept one client");
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .expect("read timeout should set");

        let mut raw = Vec::new();
        let mut chunk = [0_u8; 1024];
        let header_end = loop {
            let read = stream.read(&mut chunk).expect("server read should succeed");
            assert!(read > 0, "connection closed before headers were received");
            raw.extend_from_slice(&chunk[..read]);

            if let Some(idx) = find_subsequence(&raw, b"\r\n\r\n") {
                break idx + 4;
            }
        };

        let header_text =
            std::str::from_utf8(&raw[..header_end]).expect("headers should be valid utf-8");
        let mut lines = header_text.split("\r\n");
        let request_line = lines
            .next()
            .expect("request line should exist")
            .split_whitespace()
            .map(str::to_string)
            .collect::<Vec<_>>();

        let method = request_line
            .first()
            .cloned()
            .expect("request line should include method");
        let path = request_line
            .get(1)
            .cloned()
            .expect("request line should include path");

        let mut headers = BTreeMap::new();
        for line in lines {
            if line.is_empty() {
                continue;
            }
            if let Some((name, value)) = line.split_once(':') {
                headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_string());
            }
        }

        let content_length = headers
            .get("content-length")
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(0);

        let mut body = raw[header_end..].to_vec();
        while body.len() < content_length {
            let read = stream
                .read(&mut chunk)
                .expect("server body read should succeed");
            if read == 0 {
                break;
            }
            body.extend_from_slice(&chunk[..read]);
        }
        body.truncate(content_length);

        let response = format!(
            "HTTP/1.1 {status_line}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{response_body}",
            response_body.len()
        );
        stream
            .write_all(response.as_bytes())
            .expect("server write should succeed");

        CapturedRequest {
            method,
            path,
            headers,
            body,
        }
    });

    (format!("http://{addr}/ingest/diagnostics"), handle)
}

fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }

    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}
