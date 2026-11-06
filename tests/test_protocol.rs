//! Port of `tests/test_protocol.cpp`.

use redis_rust::buffer::buf_append_u32;
use redis_rust::protocol::*;
use redis_rust::Buffer;

fn body_of(msg: &[u8]) -> &[u8] {
    &msg[4..] // skip the length header
}

fn roundtrip(encoded: &[u8]) -> Value {
    let (value, consumed) = decode_value(encoded).expect("decode");
    assert_eq!(consumed, encoded.len());
    value
}

#[test]
fn parse_req_round_trips_encode_request() {
    let cmd = vec![
        b"set".to_vec(),
        b"key".to_vec(),
        b"bin\0ary".to_vec(),
        b"".to_vec(),
    ];
    let msg = encode_request(&cmd);
    let len = u32::from_ne_bytes(msg[..4].try_into().unwrap()) as usize;
    assert_eq!(len, msg.len() - 4);

    let mut out = Vec::new();
    assert!(parse_req(body_of(&msg), &mut out));
    assert_eq!(out, cmd);
}

#[test]
fn parse_req_empty_command_list() {
    let msg = encode_request(&[]);
    let mut out = Vec::new();
    assert!(parse_req(body_of(&msg), &mut out));
    assert!(out.is_empty());
}

#[test]
fn parse_req_rejects_trailing_garbage() {
    let mut msg = encode_request(&[b"get".to_vec(), b"k".to_vec()]);
    msg.push(0xFF);
    let mut out = Vec::new();
    assert!(!parse_req(body_of(&msg), &mut out));
}

#[test]
fn parse_req_rejects_every_truncation() {
    let msg = encode_request(&[
        b"zadd".to_vec(),
        b"z".to_vec(),
        b"1.5".to_vec(),
        b"member".to_vec(),
    ]);
    let body = body_of(&msg);
    for n in 0..body.len() {
        let mut out = Vec::new();
        assert!(!parse_req(&body[..n], &mut out), "prefix length {n}");
    }
}

#[test]
fn parse_req_rejects_too_many_args() {
    let mut body = Vec::new();
    buf_append_u32(&mut body, K_MAX_ARGS as u32 + 1);
    let mut out = Vec::new();
    assert!(!parse_req(&body, &mut out));
}

#[test]
fn parse_req_rejects_huge_string_length() {
    let mut body = Vec::new();
    buf_append_u32(&mut body, 1);
    buf_append_u32(&mut body, 0xFFFF_FFFF); // claims 4 GiB
    let mut out = Vec::new();
    assert!(!parse_req(&body, &mut out));
}

#[test]
fn random_input_never_crashes() {
    let mut rng = 1234u64;
    for _ in 0..20_000 {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        let len = (rng % 64) as usize;
        let mut data = vec![0u8; len];
        // small values hit valid tags/lengths often
        data.fill_with(|| (rng % 8) as u8);
        let mut out = Vec::new();
        let _ = parse_req(&data, &mut out);
        let _ = decode_value(&data);
    }
}

#[test]
fn scalars_round_trip() {
    let mut b = Buffer::new();
    out_nil(&mut b);
    assert_eq!(roundtrip(&b), Value::Nil);

    b.clear();
    out_str(&mut b, b"hello");
    assert_eq!(roundtrip(&b), Value::Str(b"hello".to_vec()));

    b.clear();
    out_int(&mut b, -42);
    assert_eq!(roundtrip(&b), Value::Int(-42));

    b.clear();
    out_dbl(&mut b, 3.25);
    assert_eq!(roundtrip(&b), Value::Dbl(3.25));

    b.clear();
    out_err(&mut b, ERR_BAD_ARG, "nope");
    assert_eq!(
        roundtrip(&b),
        Value::Error {
            code: ERR_BAD_ARG,
            msg: "nope".to_string()
        }
    );
}

#[test]
fn nested_arrays_with_backfilled_length() {
    let mut b = Buffer::new();
    let outer = out_begin_arr(&mut b);
    out_str(&mut b, b"a");
    out_arr(&mut b, 2);
    out_int(&mut b, 1);
    out_nil(&mut b);
    out_end_arr(&mut b, outer, 2);

    let expected = Value::Arr(vec![
        Value::Str(b"a".to_vec()),
        Value::Arr(vec![Value::Int(1), Value::Nil]),
    ]);
    assert_eq!(roundtrip(&b), expected);
}

#[test]
fn response_framing_writes_length() {
    let mut b = Buffer::new();
    let header = response_begin(&mut b);
    out_str(&mut b, b"xyz");
    response_end(&mut b, header);
    let len = u32::from_ne_bytes(b[..4].try_into().unwrap()) as usize;
    assert_eq!(len, b.len() - 4);
}

#[test]
fn oversized_response_becomes_error() {
    let mut b = Buffer::new();
    let header = response_begin(&mut b);
    out_str(&mut b, &vec![b'x'; K_MAX_MSG]);
    response_end(&mut b, header);
    let (value, _) = decode_value(&b[4..]).expect("decode");
    match value {
        Value::Error { code, .. } => assert_eq!(code, ERR_TOO_BIG),
        other => panic!("expected error, got {other:?}"),
    }
}

#[test]
fn format_matches_cli_output() {
    let value = Value::Arr(vec![Value::Str(b"n1".to_vec()), Value::Dbl(1.1)]);
    let mut text = Vec::new();
    format_value(&value, &mut text);
    assert_eq!(text, b"(arr) len=2\n(str) n1\n(dbl) 1.1\n(arr) end\n");
}
