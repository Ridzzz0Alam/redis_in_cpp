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
        let opts = ServerOptions {
            port: 0,
            verbose: false,
            idle_timeout_ms: 500,
            worker_threads: 4,
        };
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
    thread::sleep(Duration::from_millis(1200));
    let mut byte = [0u8; 1];
    assert_eq!(stream.read(&mut byte).unwrap(), 0); // EOF: server closed us
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
