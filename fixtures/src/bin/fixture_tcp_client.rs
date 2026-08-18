//! Deterministic TCP client fixture (task T0.4).
//!
//! Connects to a target, announces both endpoints and its own PID, then holds
//! the connection open for a configurable duration so that socket discovery has
//! a stable ESTABLISHED pair to observe.
//!
//! ```bash
//! fixture-tcp-client --target 127.0.0.1:41001 --hold-secs 10
//! ```

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

use clap::Parser;

#[derive(Debug, Parser)]
#[command(
    name = "fixture-tcp-client",
    about = "Deterministic TCP client fixture"
)]
struct Args {
    /// `host:port` to connect to.
    #[arg(long)]
    target: SocketAddr,

    /// How long to hold the connection open after connecting.
    #[arg(long, default_value_t = 10)]
    hold_secs: u64,

    /// How long to wait for the connection to be accepted.
    #[arg(long, default_value_t = 5)]
    connect_timeout_secs: u64,
}

fn main() -> std::io::Result<()> {
    let args = Args::parse();

    let stream =
        TcpStream::connect_timeout(&args.target, Duration::from_secs(args.connect_timeout_secs))?;
    let local = stream.local_addr()?;
    let remote = stream.peer_addr()?;

    println!(
        "fixture-tcp-client: CONNECTED pid={} local={local} remote={remote}",
        std::process::id()
    );
    std::io::stdout().flush()?;

    // Drain whatever the server sends without ever closing our side early.
    stream.set_read_timeout(Some(Duration::from_millis(200)))?;
    let mut stream = stream;
    let mut sink = [0u8; 256];
    let deadline = std::time::Instant::now() + Duration::from_secs(args.hold_secs);

    while std::time::Instant::now() < deadline {
        match stream.read(&mut sink) {
            Ok(0) => {
                // Server closed; keep the process alive for the full hold so
                // process discovery still has something to look at.
                std::thread::sleep(Duration::from_millis(100));
            }
            Ok(_) => continue,
            Err(err)
                if matches!(
                    err.kind(),
                    std::io::ErrorKind::WouldBlock
                        | std::io::ErrorKind::TimedOut
                        | std::io::ErrorKind::Interrupted
                ) => {}
            Err(err) => {
                eprintln!("fixture-tcp-client: read failed: {err}");
                break;
            }
        }
    }

    println!("fixture-tcp-client: EXIT pid={}", std::process::id());
    Ok(())
}
