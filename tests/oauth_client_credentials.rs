use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::Duration;

use subseq_tracing::{
    OAuthClientCredentialsJwtProvider, OAuthClientCredentialsJwtProviderOptions,
    WorkloadJwtProvider,
};

fn spawn_token_server(expires_in: u64, max_requests: usize) -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("token server bind");
    let addr = listener.local_addr().expect("token server addr");

    let calls = Arc::new(AtomicUsize::new(0));
    let calls_bg = calls.clone();

    thread::spawn(move || {
        for _ in 0..max_requests {
            let (mut stream, _) = listener.accept().expect("token server accept");
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .expect("read timeout");

            let mut raw = Vec::new();
            let mut buf = [0_u8; 1024];
            loop {
                let read = stream.read(&mut buf).expect("server read");
                if read == 0 {
                    break;
                }
                raw.extend_from_slice(&buf[..read]);
                if raw.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }

            let call_num = calls_bg.fetch_add(1, Ordering::SeqCst) + 1;
            let token = format!("token-{call_num}");
            let body = format!(
                "{{\"access_token\":\"{token}\",\"expires_in\":{expires_in},\"token_type\":\"Bearer\"}}"
            );
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(response.as_bytes()).expect("server write");
        }
    });

    (format!("http://{addr}/oauth2/token"), calls)
}

#[test]
fn oauth_provider_caches_tokens_until_expiry() {
    let (token_url, calls) = spawn_token_server(60, 1);

    let provider =
        OAuthClientCredentialsJwtProvider::new(OAuthClientCredentialsJwtProviderOptions {
            token_url,
            client_id: "client".to_string(),
            client_secret: "secret".to_string(),
            scopes: vec!["scope-a".to_string(), "scope-b".to_string()],
            timeout: Duration::from_secs(2),
            user_agent: "subseq-tracing-test/1.0".to_string(),
            refresh_margin: Duration::from_secs(0),
        })
        .expect("provider");

    let first = provider.workload_jwt().expect("first token");
    let second = provider.workload_jwt().expect("cached token");
    assert_eq!(first, second);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn oauth_provider_refreshes_after_expiry() {
    let (token_url, calls) = spawn_token_server(1, 2);

    let provider =
        OAuthClientCredentialsJwtProvider::new(OAuthClientCredentialsJwtProviderOptions {
            token_url,
            client_id: "client".to_string(),
            client_secret: "secret".to_string(),
            scopes: vec![],
            timeout: Duration::from_secs(2),
            user_agent: "subseq-tracing-test/1.0".to_string(),
            refresh_margin: Duration::from_secs(0),
        })
        .expect("provider");

    let first = provider.workload_jwt().expect("first token");
    std::thread::sleep(Duration::from_millis(1100));
    let second = provider.workload_jwt().expect("refreshed token");
    assert_ne!(first, second);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}
