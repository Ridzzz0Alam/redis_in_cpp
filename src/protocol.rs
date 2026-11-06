//! Wire protocol: request parser and tagged (TLV) response codec, ported from
//! `protocol.h` / `protocol.cpp`.
//!
//! Framing: every message is `| len:u32 | body... |`.
//! Request body: `| nstr:u32 | len:u32 | str1 | ... | len:u32 | strn |`.
//! Response body: one tagged value. Integers are in host byte order, exactly
//! like the C++ implementation.

use crate::buffer::{buf_append, buf_append_f64, buf_append_i64, buf_append_u32, Buffer};

pub const K_MAX_MSG: usize = 32 << 20; // 32 MiB
pub const K_MAX_ARGS: usize = 200_000;

// Error codes carried by TAG_ERR.
pub const ERR_UNKNOWN: u32 = 1;
pub const ERR_TOO_BIG: u32 = 2;
pub const ERR_BAD_TYP: u32 = 3;
pub const ERR_BAD_ARG: u32 = 4;

// Type tags of serialized values.
pub const TAG_NIL: u8 = 0;
pub const TAG_ERR: u8 = 1;
pub const TAG_STR: u8 = 2;
pub const TAG_INT: u8 = 3;
pub const TAG_DBL: u8 = 4;
pub const TAG_ARR: u8 = 5;

// ---------------------------------------------------------------------------
// requests
// ---------------------------------------------------------------------------

/// A decoded response value; mirrors the C++ `Value` variant.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Nil,
    Error { code: u32, msg: String },
    Str(Vec<u8>),
    Int(i64),
    Dbl(f64),
    Arr(Vec<Value>),
}

fn read_u32(data: &[u8], cur: &mut usize) -> Option<u32> {
    let end = cur.checked_add(4)?;
    if end > data.len() {
        return None;
    }
    let mut bytes = [0u8; 4];
    bytes.copy_from_slice(&data[*cur..end]);
    *cur = end;
    Some(u32::from_ne_bytes(bytes))
}

/// Parse a request body into its byte strings. Returns false on malformed input.
pub fn parse_req(req: &[u8], out: &mut Vec<Vec<u8>>) -> bool {
    let mut cur = 0usize;
    let Some(nstr) = read_u32(req, &mut cur) else {
        return false;
    };
    if nstr as usize > K_MAX_ARGS {
        return false; // safety limit
    }
    while out.len() < nstr as usize {
        let Some(len) = read_u32(req, &mut cur) else {
            return false;
        };
        let len = len as usize;
        let Some(end) = cur.checked_add(len) else {
            return false;
        };
        if end > req.len() {
            return false;
        }
        out.push(req[cur..end].to_vec());
        cur = end;
    }
    cur == req.len() // reject trailing garbage
}

/// Encode a full request message (length header + body).
pub fn encode_request(cmd: &[Vec<u8>]) -> Buffer {
    let mut len = 4usize;
    for s in cmd {
        len += 4 + s.len();
    }
    let mut buf = Buffer::with_capacity(4 + len);
    buf_append_u32(&mut buf, len as u32);
    buf_append_u32(&mut buf, cmd.len() as u32);
    for s in cmd {
        buf_append_u32(&mut buf, s.len() as u32);
        buf_append(&mut buf, s);
    }
    buf
}

// ---------------------------------------------------------------------------
// response encoding
// ---------------------------------------------------------------------------

pub fn out_nil(out: &mut Buffer) {
    out.push(TAG_NIL);
}

pub fn out_str(out: &mut Buffer, s: &[u8]) {
    out.push(TAG_STR);
    buf_append_u32(out, s.len() as u32);
    buf_append(out, s);
}

pub fn out_int(out: &mut Buffer, val: i64) {
    out.push(TAG_INT);
    buf_append_i64(out, val);
}

pub fn out_dbl(out: &mut Buffer, val: f64) {
    out.push(TAG_DBL);
    buf_append_f64(out, val);
}

pub fn out_err(out: &mut Buffer, code: u32, msg: &str) {
    out.push(TAG_ERR);
    buf_append_u32(out, code);
    buf_append_u32(out, msg.len() as u32);
    buf_append(out, msg.as_bytes());
}

pub fn out_arr(out: &mut Buffer, n: u32) {
    out.push(TAG_ARR);
    buf_append_u32(out, n);
}

/// Reserve an array header; returns the context passed to [`out_end_arr`].
pub fn out_begin_arr(out: &mut Buffer) -> usize {
    out.push(TAG_ARR);
    buf_append_u32(out, 0); // filled by out_end_arr()
    out.len() - 4
}

pub fn out_end_arr(out: &mut Buffer, ctx: usize, n: u32) {
    debug_assert_eq!(out[ctx - 1], TAG_ARR);
    out[ctx..ctx + 4].copy_from_slice(&n.to_ne_bytes());
}

/// Reserve the 4-byte message header; returns its position.
pub fn response_begin(out: &mut Buffer) -> usize {
    let header = out.len();
    buf_append_u32(out, 0);
    header
}

/// Backfill the header, replacing an oversized body with `ERR_TOO_BIG`.
pub fn response_end(out: &mut Buffer, header: usize) {
    let mut msg_size = out.len() - header - 4;
    if msg_size > K_MAX_MSG {
        out.truncate(header + 4);
        out_err(out, ERR_TOO_BIG, "response is too big.");
        msg_size = out.len() - header - 4;
    }
    let len = msg_size as u32;
    out[header..header + 4].copy_from_slice(&len.to_ne_bytes());
}

// ---------------------------------------------------------------------------
// response decoding
// ---------------------------------------------------------------------------

fn read_pod<const N: usize>(data: &[u8], pos: usize) -> Option<[u8; N]> {
    let end = pos.checked_add(N)?;
    if end > data.len() {
        return None;
    }
    let mut bytes = [0u8; N];
    bytes.copy_from_slice(&data[pos..end]);
    Some(bytes)
}

fn bad<T>() -> Result<T, String> {
    Err("bad response".to_string())
}

/// Maximum array nesting accepted by [`decode_value`]. The server never nests
/// arrays, so this only guards the client against a hostile or broken server:
/// a 5-byte array header per level would otherwise allow millions of levels
/// within one message and overflow the stack.
pub const K_MAX_DEPTH: usize = 64;

/// Decode one tagged value from the front of `data`, returning it and the
/// number of bytes consumed.
pub fn decode_value(data: &[u8]) -> Result<(Value, usize), String> {
    decode_value_at(data, 0)
}

fn decode_value_at(data: &[u8], depth: usize) -> Result<(Value, usize), String> {
    if depth > K_MAX_DEPTH {
        return Err("response nested too deeply".to_string());
    }
    let Some(&tag) = data.first() else {
        return bad();
    };
    match tag {
        TAG_NIL => Ok((Value::Nil, 1)),
        TAG_ERR => {
            let code = u32::from_ne_bytes(read_pod(data, 1).ok_or("bad response")?);
            let len = u32::from_ne_bytes(read_pod(data, 5).ok_or("bad response")?) as usize;
            let end = 9usize.checked_add(len).ok_or("bad response")?;
            if data.len() < end {
                return bad();
            }
            let msg = String::from_utf8_lossy(&data[9..end]).into_owned();
            Ok((Value::Error { code, msg }, end))
        }
        TAG_STR => {
            let len = u32::from_ne_bytes(read_pod(data, 1).ok_or("bad response")?) as usize;
            let end = 5usize.checked_add(len).ok_or("bad response")?;
            if data.len() < end {
                return bad();
            }
            Ok((Value::Str(data[5..end].to_vec()), end))
        }
        TAG_INT => {
            let bytes = read_pod(data, 1).ok_or("bad response")?;
            Ok((Value::Int(i64::from_ne_bytes(bytes)), 9))
        }
        TAG_DBL => {
            let bytes = read_pod(data, 1).ok_or("bad response")?;
            Ok((Value::Dbl(f64::from_ne_bytes(bytes)), 9))
        }
        TAG_ARR => {
            let n = u32::from_ne_bytes(read_pod(data, 1).ok_or("bad response")?);
            let mut items = Vec::new();
            let mut pos = 5usize;
            for _ in 0..n {
                let (value, consumed) = decode_value_at(&data[pos..], depth + 1)?;
                pos = pos.checked_add(consumed).ok_or("bad response")?;
                items.push(value);
            }
            Ok((Value::Arr(items), pos))
        }
        _ => bad(),
    }
}

/// Human-readable form used by the CLI.
pub fn format_value(value: &Value, out: &mut Vec<u8>) {
    match value {
        Value::Nil => out.extend_from_slice(b"(nil)\n"),
        Value::Error { code, msg } => {
            out.extend_from_slice(format!("(err) {code} {msg}\n").as_bytes());
        }
        Value::Str(s) => {
            out.extend_from_slice(b"(str) ");
            out.extend_from_slice(s);
            out.push(b'\n');
        }
        Value::Int(i) => out.extend_from_slice(format!("(int) {i}\n").as_bytes()),
        Value::Dbl(d) => out.extend_from_slice(format!("(dbl) {d}\n").as_bytes()),
        Value::Arr(items) => {
            out.extend_from_slice(format!("(arr) len={}\n", items.len()).as_bytes());
            for item in items {
                format_value(item, out);
            }
            out.extend_from_slice(b"(arr) end\n");
        }
    }
}

/// Helper used by the server: append one request's serialized response.
pub fn make_response(out: &mut Buffer, value: &Value) {
    let header = response_begin(out);
    encode_value(out, value);
    response_end(out, header);
}

fn encode_value(out: &mut Buffer, value: &Value) {
    match value {
        Value::Nil => out_nil(out),
        Value::Error { code, msg } => out_err(out, *code, msg),
        Value::Str(s) => out_str(out, s),
        Value::Int(i) => out_int(out, *i),
        Value::Dbl(d) => out_dbl(out, *d),
        Value::Arr(items) => {
            out_arr(out, items.len() as u32);
            for item in items {
                encode_value(out, item);
            }
        }
    }
}
