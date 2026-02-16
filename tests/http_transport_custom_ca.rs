use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use rcgen::{CertifiedKey, generate_simple_self_signed};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use subseq_tracing::{
    DiagnosticEvent, DiagnosticKind, DiagnosticLevel, DiagnosticPackage, DiagnosticsTransport,
    HttpTransport, HttpTransportOptions, ScopeSnapshot,
};
use uuid::Uuid;

fn write_temp_pem(prefix: &str, pem: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!("{prefix}-{}.pem", Uuid::new_v4()));
    std::fs::write(&path, pem).expect("write temp pem");
    path
}

fn spawn_https_server() -> (String, std::path::PathBuf) {
    let CertifiedKey { cert, signing_key } =
        generate_simple_self_signed(vec!["localhost".to_string()]).expect("cert");

    let cert_chain = vec![CertificateDer::from(cert.der().to_vec())];
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(signing_key.serialize_der()));

    let tls_config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(cert_chain, key)
        .expect("server tls config");

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");

    let ca_pem_path = write_temp_pem("subseq-tracing-ca", &cert.pem());

    let tls_config = Arc::new(tls_config);
    std::thread::spawn(move || {
        let (stream, _) = listener.accept().expect("accept");
        let _ = stream.set_read_timeout(Some(Duration::from_secs(3)));
        let _ = stream.set_write_timeout(Some(Duration::from_secs(3)));

        let conn = match rustls::ServerConnection::new(tls_config) {
            Ok(conn) => conn,
            Err(_) => return,
        };
        let mut tls = rustls::StreamOwned::new(conn, stream);

        // Read until we have all headers + body (ureq uses Content-Length).
        let mut raw = Vec::with_capacity(8192);
        let mut buf = [0_u8; 2048];
        let header_end = loop {
            let read = match tls.read(&mut buf) {
                Ok(0) => return,
                Ok(n) => n,
                Err(_) => return,
            };
            raw.extend_from_slice(&buf[..read]);

            if raw.len() > 1024 * 1024 {
                // Defensive: avoid unbounded growth in tests.
                return;
            }

            if let Some(idx) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
                break idx + 4;
            }
        };

        let headers = match std::str::from_utf8(&raw[..header_end]) {
            Ok(value) => value,
            Err(_) => return,
        };

        let mut content_length: usize = 0;
        for line in headers.lines() {
            let lower = line.to_ascii_lowercase();
            if let Some(value) = lower.strip_prefix("content-length:") {
                content_length = value.trim().parse().unwrap_or(0);
                break;
            }
        }

        while raw.len() < header_end + content_length {
            let read = match tls.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(_) => break,
            };
            raw.extend_from_slice(&buf[..read]);
        }

        let body = "{}";
        let response = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        let _ = tls.write_all(response.as_bytes());
        let _ = tls.flush();
    });

    let endpoint = format!(
        "https://localhost:{}/api/v1/monitoring/ingest/diagnostics",
        addr.port()
    );
    (endpoint, ca_pem_path)
}

fn sample_package() -> DiagnosticPackage {
    let scope = ScopeSnapshot::default();
    let event = DiagnosticEvent::new(
        DiagnosticLevel::Warning,
        DiagnosticKind::Message,
        "subseq-tracing-test",
        "hello from test",
        scope,
    );

    DiagnosticPackage {
        package_id: Uuid::new_v4(),
        generated_at: Utc::now(),
        project_slug: "test-project".to_string(),
        service_name: "test-service".to_string(),
        environment: "dev".to_string(),
        release: None,
        event_count: 1,
        events: vec![event],
    }
}

#[test]
fn http_transport_accepts_custom_ca_for_https() {
    // Sanity: without the custom CA, this should fail for our ephemeral CA.
    let (endpoint, ca_path) = spawn_https_server();
    let no_ca = HttpTransport::new(HttpTransportOptions {
        endpoint,
        timeout: Duration::from_secs(2),
        ..HttpTransportOptions::default()
    })
    .expect("transport");
    assert!(no_ca.send(sample_package()).is_err());
    let _ = std::fs::remove_file(&ca_path);

    let (endpoint, ca_path) = spawn_https_server();
    let with_ca = HttpTransport::new(HttpTransportOptions {
        endpoint,
        timeout: Duration::from_secs(2),
        ca_cert_pem_path: Some(ca_path.clone()),
        ..HttpTransportOptions::default()
    })
    .expect("transport");

    with_ca.send(sample_package()).expect("send over https");

    // Clean up (best-effort).
    let _ = std::fs::remove_file(ca_path);
}
