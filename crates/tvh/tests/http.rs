//! Talks to tiny local HTTP servers to check credential scoping and size limits.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::thread;

use tvh::{Auth, Client, Error};

/// Serves `body` to every request on `ip`; records whether each request carried Authorization.
/// `content_length: false` sends a close-delimited body (no length header).
fn serve(ip: &str, body: Vec<u8>, content_length: bool) -> (String, Arc<Mutex<Vec<bool>>>) {
    let listener = TcpListener::bind((ip, 0)).unwrap();
    let addr = listener.local_addr().unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            let mut auth = false;
            for line in BufReader::new(stream.try_clone().unwrap()).lines() {
                let line = line.unwrap();
                if line.to_ascii_lowercase().starts_with("authorization:") {
                    auth = true;
                }
                if line.is_empty() {
                    break;
                }
            }
            log.lock().unwrap().push(auth);
            let mut head = String::from("HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nConnection: close\r\n");
            if content_length {
                head.push_str(&format!("Content-Length: {}\r\n", body.len()));
            }
            head.push_str("\r\n");
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(&body);
        }
    });
    (format!("http://{addr}"), seen)
}

/// Behaves like Tvheadend: a POST without Content-Length gets the connection closed.
/// Answers everything else with a serverinfo-style JSON body.
fn serve_strict() -> String {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let addr = listener.local_addr().unwrap();
    thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let (mut post, mut length) = (false, None);
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 {
                    break;
                }
                let line = line.trim_end();
                if line.starts_with("POST ") {
                    post = true;
                }
                if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = v.trim().parse::<usize>().ok();
                }
                if line.is_empty() {
                    break;
                }
            }
            if post && length.is_none() {
                continue; // drop the connection without answering
            }
            let mut body = vec![0; length.unwrap_or(0)];
            let _ = std::io::Read::read_exact(&mut reader, &mut body);
            let json = br#"{"sw_version":"4.3-test","api_version":20,"name":"Tvheadend"}"#;
            let head = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", json.len());
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(json);
        }
    });
    format!("http://{addr}")
}

#[tokio::test]
async fn empty_form_posts_send_content_length() {
    let c = Client::new(&serve_strict(), Auth::None).unwrap();
    let info = c.server_info().await.expect("server_info against a strict server");
    assert_eq!(info.api_version, 20);
}

fn basic() -> Auth {
    Auth::Basic { user: "u".into(), pass: "secret".into() }
}

#[tokio::test]
async fn credentials_only_go_to_the_server() {
    let (server, server_seen) = serve("127.0.0.1", b"icon".to_vec(), true);
    // 127.0.0.2 is a different host on Linux loopback, standing in for an external icon CDN.
    let (foreign, foreign_seen) = serve("127.0.0.2", b"icon".to_vec(), true);
    let c = Client::new(&server, basic()).unwrap();

    let own = c.icon_url("imagecache/1").unwrap();
    assert_eq!(c.fetch(own).await.unwrap(), b"icon");
    let ext = c.icon_url(&format!("{foreign}/logo.png")).unwrap();
    assert_eq!(c.fetch(ext).await.unwrap(), b"icon");

    assert_eq!(*server_seen.lock().unwrap(), [true]);
    assert_eq!(*foreign_seen.lock().unwrap(), [false]);
}

#[tokio::test]
async fn oversized_fetch_is_refused() {
    let big = vec![0u8; 3 << 20];
    for content_length in [true, false] {
        let (server, _) = serve("127.0.0.1", big.clone(), content_length);
        let c = Client::new(&server, Auth::None).unwrap();
        let err = c.fetch(c.icon_url("imagecache/1").unwrap()).await.unwrap_err();
        assert!(matches!(err, Error::TooLarge(_)), "content_length={content_length}: {err}");
    }
}

/// Needs network access: `cargo test -p tvh -- --ignored`.
#[tokio::test]
#[ignore]
async fn https_with_system_roots() {
    let c = Client::new("https://apt.icecubelabs.xyz", Auth::None).unwrap();
    let key = c.fetch(c.icon_url("cablebox.gpg").unwrap()).await.unwrap();
    assert!(key.len() > 100, "got {} bytes", key.len());
}

/// Certificate verification must be on. Needs network access.
#[tokio::test]
#[ignore]
async fn https_rejects_bad_certificates() {
    for host in ["https://expired.badssl.com", "https://self-signed.badssl.com", "https://wrong.host.badssl.com"] {
        let c = Client::new(host, Auth::None).unwrap();
        let r = c.fetch(c.icon_url("index.html").unwrap()).await;
        assert!(matches!(r, Err(Error::Http(_))), "{host} was accepted");
    }
}
