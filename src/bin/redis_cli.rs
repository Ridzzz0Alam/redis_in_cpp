//! Command-line client. Ported from `apps/redis_cli.cpp` / the book's client.

use std::io::Write;
use std::os::unix::ffi::OsStringExt;
use std::process::ExitCode;

use redis_rust::net;
use redis_rust::protocol::{format_value, Value};

/// Returns `Ok(false)` when the server replied with an error value.
fn run() -> Result<bool, String> {
    let args: Vec<Vec<u8>> = std::env::args_os().skip(1).map(|s| s.into_vec()).collect();

    let mut host = String::from("127.0.0.1");
    let mut port: u16 = 1234;
    let mut i = 0usize;
    while i < args.len() {
        let flag = args[i].as_slice();
        if flag != b"--host" && flag != b"--port" {
            break;
        }
        let value = args
            .get(i + 1)
            .ok_or_else(|| format!("{} needs a value\n{USAGE}", String::from_utf8_lossy(flag)))?;
        if flag == b"--host" {
            host = String::from_utf8_lossy(value).into_owned();
        } else {
            port = String::from_utf8_lossy(value)
                .parse()
                .map_err(|_| "invalid port".to_string())?;
        }
        i += 2;
    }
    let cmd = args[i..].to_vec();
    if cmd.is_empty() {
        return Err(USAGE.to_string());
    }

    let mut stream = net::connect_tcp(&host, port)?;
    let value = net::call(&mut stream, &cmd)?;
    let mut text = Vec::new();
    format_value(&value, &mut text);
    let mut stdout = std::io::stdout().lock();
    stdout
        .write_all(&text)
        .and_then(|()| stdout.flush())
        .map_err(|e| e.to_string())?;
    Ok(!matches!(value, Value::Error { .. }))
}

const USAGE: &str = "usage: redis_cli [--host IPV4] [--port N] command [args...]";

/// Exit status: 0 on success, 1 if the server replied with an error, 2 on
/// usage or connection errors, so scripts can tell them apart.
fn main() -> ExitCode {
    match run() {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(e) => {
            eprintln!("{e}");
            ExitCode::from(2)
        }
    }
}
