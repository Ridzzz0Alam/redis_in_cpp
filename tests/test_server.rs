//! Port of `tests/test_server.cpp`: integration tests against a real Server
//! over TCP.

use std::io::Read;
use std::net::TcpStream;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use redis_rust::net::{call, connect_tcp, read_response, write_all};
use redis_rust::protocol::{encode_request, Value};
use redis_rust::server::{Server, ServerOptions, StopHandle};
use redis_rust::Buffer;

struct TestServer {
    port: u16,
    stop: StopHandle,
    handle: Option<JoinHandle<std::io::Result<()>>>,
}

impl TestServer {
    fn start() -> Self {
        Self::start_with(ServerOptions {
            port: 0,
            verbose: false,
            idle_timeout_ms: 500,
            worker_threads: 4,
            ..ServerOptions::default()
        })
    }

    fn start_with(opts: ServerOptions) -> Self {
        let mut server = Server::new(opts).expect("start server");
        let port = server.port();
        let stop = server.stop_handle().expect("stop handle");
        let handle = thread::spawn(move || server.run());
        Self {
            port,
            stop,
            handle: Some(handle),
        }
    }

    fn connect(&self) -> TcpStream {
        connect_tcp("127.0.0.1", self.port).expect("connect")
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.stop.stop();
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn cmd(parts: &[&str]) -> Vec<Vec<u8>> {
    parts.iter().map(|s| s.as_bytes().to_vec()).collect()
}

#[test]
fn request_response() {
    let server = TestServer::start();
    let mut stream = server.connect();
    assert_eq!(
        call(&mut stream, &cmd(&["set", "k", "v"])).unwrap(),
        Value::Nil
    );
    assert_eq!(
        call(&mut stream, &cmd(&["get", "k"])).unwrap(),
        Value::Str(b"v".to_vec())
    );
}

#[test]
fn many_clients_share_one_key_space() {
    let server = TestServer::start();
    let mut streams: Vec<TcpStream> = (0..20).map(|_| server.connect()).collect();
    for (i, stream) in streams.iter_mut().enumerate() {
        let key = format!("k{i}");
        assert!(call(stream, &cmd(&["set", &key, "v"])).is_ok());
    }
    match call(&mut streams[0], &cmd(&["keys"])).unwrap() {
        Value::Arr(items) => assert_eq!(items.len(), 20),
        other => panic!("expected array, got {other:?}"),
    }
}

// Many requests in a single write; responses must come back complete and in order.
#[test]
fn pipelining() {
    let server = TestServer::start();
    let mut stream = server.connect();
    const N: usize = 2000;
    let mut batch: Buffer = Buffer::new();
    for i in 0..N {
        let req = encode_request(&cmd(&["zadd", "z", &i.to_string(), &format!("m{i}")]));
        batch.extend_from_slice(&req);
    }
    assert!(write_all(&mut stream, &batch));
    for i in 0..N {
        let value = read_response(&mut stream).expect("response");
        assert_eq!(value, Value::Int(1), "response {i}");
    }
}

#[test]
fn large_value_spans_many_loop_iterations() {
    let server = TestServer::start();
    let mut stream = server.connect();
    let big = "x".repeat(8 << 20); // 8 MiB > socket buffers
    assert!(call(&mut stream, &cmd(&["set", "big", &big])).is_ok());
    match call(&mut stream, &cmd(&["get", "big"])).unwrap() {
        Value::Str(value) => assert_eq!(value.len(), big.len()),
        other => panic!("expected string, got {other:?}"),
    }
}

#[test]
fn malformed_request_closes_connection() {
    let server = TestServer::start();
    let mut stream = server.connect();
    let garbage: Buffer = vec![5, 0, 0, 0, 1, 2, 3, 4, 5]; // length 5, invalid body
    assert!(write_all(&mut stream, &garbage));
    assert!(read_response(&mut stream).is_err()); // EOF

    // the server is still healthy for other clients
    let mut stream2 = server.connect();
    assert!(call(&mut stream2, &cmd(&["get", "x"])).is_ok());
}

#[test]
fn idle_connections_are_closed() {
    let server = TestServer::start();
    let mut stream = server.connect();
    // Block until the server closes us (no fixed sleep to race against); the
    // timeout only bounds a broken server.
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let mut byte = [0u8; 1];
    assert_eq!(stream.read(&mut byte).unwrap(), 0); // EOF: server closed us
}

// A client that pipelines many large replies must be throttled, not buffered
// in full, and every held-back request must still be answered once it reads.
#[test]
fn pending_output_is_bounded_and_resumes() {
    let server = TestServer::start_with(ServerOptions {
        port: 0,
        verbose: false,
        max_pending_output: 1 << 20,
        ..ServerOptions::default()
    });
    let mut stream = server.connect();
    let value = "v".repeat(256 << 10);
    assert!(call(&mut stream, &cmd(&["set", "k", &value])).is_ok());

    const N: usize = 64; // 16 MiB of replies for ~1 KiB of requests
    let mut batch: Buffer = Buffer::new();
    for _ in 0..N {
        batch.extend_from_slice(&encode_request(&cmd(&["get", "k"])));
    }
    assert!(write_all(&mut stream, &batch));
    thread::sleep(Duration::from_millis(200)); // let the server hit the limit
    for i in 0..N {
        match read_response(&mut stream).expect("response") {
            Value::Str(v) => assert_eq!(v.len(), value.len(), "response {i}"),
            other => panic!("response {i}: {other:?}"),
        }
    }
    // The connection is still usable afterwards.
    assert_eq!(
        call(&mut stream, &cmd(&["del", "k"])).unwrap(),
        Value::Int(1)
    );
}

// Replies queued before a protocol error are delivered before the close.
#[test]
fn replies_before_a_bad_request_are_flushed() {
    let server = TestServer::start();
    let mut stream = server.connect();
    let mut batch = encode_request(&cmd(&["set", "a", "1"]));
    batch.extend_from_slice(&encode_request(&cmd(&["get", "a"])));
    batch.extend_from_slice(&[5, 0, 0, 0, 1, 2, 3, 4, 5]); // malformed
    assert!(write_all(&mut stream, &batch));
    assert_eq!(read_response(&mut stream).unwrap(), Value::Nil);
    assert_eq!(
        read_response(&mut stream).unwrap(),
        Value::Str(b"1".to_vec())
    );
    assert!(read_response(&mut stream).is_err()); // then EOF
}

#[test]
fn max_clients_is_enforced() {
    let server = TestServer::start_with(ServerOptions {
        port: 0,
        verbose: false,
        max_clients: 2,
        ..ServerOptions::default()
    });
    let mut a = server.connect();
    let mut b = server.connect();
    assert!(call(&mut a, &cmd(&["get", "x"])).is_ok());
    assert!(call(&mut b, &cmd(&["get", "x"])).is_ok());
    let mut c = server.connect(); // TCP handshake succeeds via the backlog...
    assert!(call(&mut c, &cmd(&["get", "x"])).is_err()); // ...then it is closed
    drop(a);
    thread::sleep(Duration::from_millis(100));
    let mut d = server.connect(); // a slot is free again
    assert!(call(&mut d, &cmd(&["get", "x"])).is_ok());
}

#[test]
fn default_bind_is_loopback() {
    assert!(ServerOptions::default().bind_addr.is_loopback());
}

#[test]
fn keys_expire_in_the_background() {
    let server = TestServer::start();
    let mut stream = server.connect();
    assert!(call(&mut stream, &cmd(&["set", "k", "v"])).is_ok());
    assert!(call(&mut stream, &cmd(&["pexpire", "k", "50"])).is_ok());
    thread::sleep(Duration::from_millis(150));
    assert_eq!(call(&mut stream, &cmd(&["get", "k"])).unwrap(), Value::Nil);
}
