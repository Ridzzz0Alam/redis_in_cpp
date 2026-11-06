//! Command-line client. Ported from `apps/redis_cli.cpp` / the book's client.

use std::io::Write;
use std::os::unix::ffi::OsStringExt;
use std::process::ExitCode;

use redis_rust::net;
use redis_rust::protocol::format_value;

fn run() -> Result<(), String> {
    let args: Vec<Vec<u8>> = std::env::args_os().skip(1).map(|s| s.into_vec()).collect();

    let mut host = String::from("127.0.0.1");
    let mut port: u16 = 1234;
    let mut i = 0usize;
    while i + 1 < args.len() {
        match args[i].as_slice() {
            b"--host" => {
                host = String::from_utf8_lossy(&args[i + 1]).into_owned();
                i += 2;
            }
            b"--port" => {
                let text = String::from_utf8_lossy(&args[i + 1]);
                port = text.parse().map_err(|_| "invalid port".to_string())?;
                i += 2;
            }
            _ => break,
        }
    }
    let cmd = args[i..].to_vec();

    let mut stream = net::connect_tcp(&host, port)?;
    let value = net::call(&mut stream, &cmd)?;
    let mut text = Vec::new();
    format_value(&value, &mut text);
    let mut stdout = std::io::stdout().lock();
    stdout
        .write_all(&text)
        .and_then(|()| stdout.flush())
        .map_err(|e| e.to_string())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}
