//! Deterministic TCP server fixture (task T0.4).
//!
//! Binds a configurable localhost port, announces the bound address and its own
//! PID on stdout, serves a fixed response to every connection, and exits after
//! a configurable lifetime.
//!
//! ```bash
//! fixture-tcp-server --port 41001 --lifetime-secs 30
//! fixture-tcp-server --port 0            # kernel-assigned port, printed on stdout
//! ```

use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use clap::Parser;

#[derive(Debug, Parser)]
#[command(
    name = "fixture-tcp-server",
    about = "Deterministic TCP server fixture"
)]
struct Args {
    /// Port to bind. `0` asks the kernel for a free port.
    #[arg(long, default_value_t = 41001)]
    port: u16,

    /// Address to bind. Loopback only by default.
    #[arg(long, default_value_t = IpAddr::V4(Ipv4Addr::LOCALHOST))]
    host: IpAddr,

    /// How long to keep serving before exiting.
    #[arg(long, default_value_t = 30)]
    lifetime_secs: u64,

    /// Bytes written to every accepted connection.
    #[arg(long, default_value = "runscape-fixture\n")]
    response: String,
}

fn main() -> std::io::Result<()> {
    let args = Args::parse();

    let listener = TcpListener::bind(SocketAddr::new(args.host, args.port))?;
    let bound = listener.local_addr()?;

    println!(
        "fixture-tcp-server: READY pid={} addr={bound}",
        std::process::id()
    );
    std::io::stdout().flush()?;

    let deadline = Instant::now() + Duration::from_secs(args.lifetime_secs);
    let stop = Arc::new(AtomicBool::new(false));

    // A short accept timeout keeps the lifetime deadline accurate without a
    // second thread or an async runtime.
    listener.set_nonblocking(true)?;

    let response = Arc::new(args.response.into_bytes());
    let mut workers = Vec::new();

    while Instant::now() < deadline && !stop.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, peer)) => {
                let response = Arc::clone(&response);
                workers.push(std::thread::spawn(move || {
                    serve(stream, peer, &response);
                }));
            }
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(err) => {
                eprintln!("fixture-tcp-server: accept failed: {err}");
                break;
            }
        }
    }

    stop.store(true, Ordering::Relaxed);
    for worker in workers {
        let _ = worker.join();
    }

    println!("fixture-tcp-server: EXIT pid={}", std::process::id());
    Ok(())
}

/// Hold the connection open for the client's benefit: write the response, then
/// block on a read until the peer closes. This keeps the socket in ESTABLISHED
/// for as long as the client wants it, which is what the discovery test needs.
fn serve(mut stream: TcpStream, peer: SocketAddr, response: &[u8]) {
    // On macOS/BSD an accepted socket inherits O_NONBLOCK from its listener.
    // Without this the first read returns WouldBlock, the worker exits, and the
    // connection is torn down before discovery can observe it.
    if let Err(err) = stream.set_nonblocking(false) {
        eprintln!("fixture-tcp-server: blocking mode failed for {peer}: {err}");
    }
    if let Err(err) = stream.set_nodelay(true) {
        eprintln!("fixture-tcp-server: nodelay failed for {peer}: {err}");
    }
    if let Err(err) = stream.write_all(response) {
        eprintln!("fixture-tcp-server: write failed for {peer}: {err}");
        return;
    }
    let _ = stream.flush();

    let mut sink = [0u8; 256];
    loop {
        match stream.read(&mut sink) {
            Ok(0) => break,
            Ok(_) => continue,
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }
}
