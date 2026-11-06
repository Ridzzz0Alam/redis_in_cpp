//! Blocking client-side socket helpers, ported from `net.h` / `net.cpp`.

use std::io::{ErrorKind, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};

use crate::protocol::{decode_value, encode_request, Value, K_MAX_MSG};
use crate::Buffer;

/// Read exactly `n` bytes, looping over partial transfers.
pub fn read_full(stream: &mut TcpStream, buf: &mut [u8]) -> bool {
    let mut pos = 0usize;
    while pos < buf.len() {
        match stream.read(&mut buf[pos..]) {
            Ok(0) => return false, // EOF
            Ok(n) => pos += n,
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(_) => return false,
        }
    }
    true
}

/// Write all bytes.
pub fn write_all(stream: &mut TcpStream, buf: &[u8]) -> bool {
    stream.write_all(buf).is_ok()
}

/// Connect to an IPv4 address such as `127.0.0.1`.
pub fn connect_tcp(host: &str, port: u16) -> Result<TcpStream, String> {
    let ip: Ipv4Addr = host
        .parse()
        .map_err(|_| format!("invalid IPv4 address: {host}"))?;
    let addr = SocketAddr::from((ip, port));
    TcpStream::connect(addr).map_err(|e| format!("connect(): {e}"))
}

/// Read one framed response message and decode it.
pub fn read_response(stream: &mut TcpStream) -> Result<Value, String> {
    let mut header = [0u8; 4];
    if !read_full(stream, &mut header) {
        return Err("read error or EOF".to_string());
    }
    let len = u32::from_ne_bytes(header) as usize;
    if len > K_MAX_MSG {
        return Err("response too long".to_string());
    }
    let mut body: Buffer = vec![0u8; len];
    if !read_full(stream, &mut body) {
        return Err("read error or EOF".to_string());
    }
    let (value, consumed) = decode_value(&body)?;
    if consumed != len {
        return Err("trailing bytes in response".to_string());
    }
    Ok(value)
}

/// Send one request and wait for its response.
pub fn call(stream: &mut TcpStream, cmd: &[Vec<u8>]) -> Result<Value, String> {
    let req = encode_request(cmd);
    if req.len() - 4 > K_MAX_MSG {
        return Err("request too long".to_string());
    }
    if !write_all(stream, &req) {
        return Err("write error".to_string());
    }
    read_response(stream)
}
