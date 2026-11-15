//! End-to-end checks of the host binary: it answers over the pipe with the
//! token, and gives nothing to anyone else.

use std::{
    io::{BufRead, BufReader, Write},
    process::{Child, Command, Stdio},
    time::Duration,
};

use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::windows::named_pipe::{ClientOptions, NamedPipeClient, ServerOptions},
};

struct Host {
    child: Child,
    pipe: String,
    token: [u8; 32],
}

impl Host {
    fn start() -> Self {
        let pipe = format!(r"\\.\pipe\shinatama-moli-test-{:032x}", rand_u128());
        let token: [u8; 32] = std::array::from_fn(|i| (rand_u128() >> (i % 16)) as u8 ^ i as u8);
        let hex: String = token.iter().map(|b| format!("{b:02x}")).collect();
        let mut child = Command::new(env!("CARGO_BIN_EXE_shinatama-moli"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("host starts");
        let stdin = child.stdin.as_mut().unwrap();
        writeln!(stdin, r#"{{"pipe":{},"token":"{hex}","args":["--layout"]}}"#, serde_json::to_string(&pipe).unwrap()).unwrap();
        stdin.flush().unwrap();
        let mut ready = String::new();
        BufReader::new(child.stdout.take().unwrap()).read_line(&mut ready).unwrap();
        assert_eq!(ready.trim(), r#"{"ready":true}"#);
        Self { child, pipe, token }
    }

    async fn client(&self) -> NamedPipeClient {
        for _ in 0..50 {
            match ClientOptions::new().open(&self.pipe) {
                Ok(client) => return client,
                Err(_) => tokio::time::sleep(Duration::from_millis(20)).await,
            }
        }
        panic!("pipe never became connectable");
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn rand_u128() -> u128 {
    use std::hash::{BuildHasher, RandomState};
    let a = RandomState::new().hash_one(std::time::SystemTime::now()) as u128;
    let b = RandomState::new().hash_one(std::process::id()) as u128;
    (a << 64) | b
}

const VERSION_REQUEST: &[u8] = b"GET /json/version HTTP/1.1\r\nHost: 127.0.0.1:9222\r\nConnection: close\r\n\r\n";

async fn read_all(client: &mut NamedPipeClient, within: Duration) -> Vec<u8> {
    let mut out = Vec::new();
    let _ = tokio::time::timeout(within, client.read_to_end(&mut out)).await;
    out
}

#[tokio::test]
async fn version_over_pipe() {
    let host = Host::start();
    let mut client = host.client().await;
    client.write_all(&host.token).await.unwrap();
    client.write_all(VERSION_REQUEST).await.unwrap();
    let response = String::from_utf8_lossy(&read_all(&mut client, Duration::from_secs(10)).await).into_owned();
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert!(response.contains("\"Browser\""), "{response}");
    assert!(response.contains("webSocketDebuggerUrl"), "{response}");
}

#[tokio::test]
async fn foreign_client_gets_no_bytes() {
    let host = Host::start();
    let mut client = host.client().await;
    client.write_all(&[0x55; 32]).await.unwrap();
    let _ = client.write_all(VERSION_REQUEST).await;
    assert!(read_all(&mut client, Duration::from_secs(4)).await.is_empty());
}

#[tokio::test]
async fn short_token_is_dropped_and_the_next_client_works() {
    let host = Host::start();
    let mut first = host.client().await;
    first.write_all(&host.token[..31]).await.unwrap();
    drop(first);
    let mut second = host.client().await;
    second.write_all(&host.token).await.unwrap();
    second.write_all(VERSION_REQUEST).await.unwrap();
    let response = read_all(&mut second, Duration::from_secs(10)).await;
    assert!(response.starts_with(b"HTTP/1.1 200"));
}

#[tokio::test]
async fn second_server_instance_is_refused() {
    let host = Host::start();
    let squatter = ServerOptions::new().first_pipe_instance(true).create(&host.pipe);
    assert!(squatter.is_err(), "a second first instance must fail");
}

#[tokio::test]
async fn silent_client_is_dropped_after_the_handshake_timeout() {
    let host = Host::start();
    let mut silent = host.client().await;
    let started = std::time::Instant::now();
    let read = read_all(&mut silent, Duration::from_secs(6)).await;
    assert!(read.is_empty());
    assert!(started.elapsed() < Duration::from_secs(5), "dropped after {:?}", started.elapsed());
}

#[test]
fn no_tcp_listener() {
    let host = Host::start();
    let pid = host.child.id();
    let output = Command::new("netstat").args(["-ano", "-p", "TCP"]).output().unwrap();
    let listing = String::from_utf8_lossy(&output.stdout);
    let owned: Vec<_> = listing
        .lines()
        .filter(|line| line.split_whitespace().last() == Some(&pid.to_string()))
        .collect();
    assert!(owned.is_empty(), "host owns TCP sockets: {owned:?}");
}
