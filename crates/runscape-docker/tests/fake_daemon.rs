//! End-to-end verification of the Bollard collector without a Docker daemon.
//!
//! Docker is not installed everywhere Runscape is developed, and a test that
//! only runs on the maintainer's laptop verifies nothing. This test stands up a
//! unix socket that answers the three Docker endpoints the collector uses,
//! points Bollard at it through `DOCKER_HOST`, and exercises the real path:
//! ping, container list, JSON decoding, stats arithmetic, duration measurement.
//!
//! It contains exactly one test function on purpose: `DOCKER_HOST` is process
//! state, and a second test in this binary could race with it.

use std::path::Path;

use runscape_docker::{ContainerCollector, ContainerState, DockerAvailability};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};

const CONTAINERS: &str = r#"[
  {
    "Id": "1111111111",
    "Names": ["/shop-db-1"],
    "Image": "postgres:16",
    "State": "running",
    "Status": "Up 2 minutes",
    "Labels": {
      "com.docker.compose.project": "shop",
      "com.docker.compose.service": "db"
    },
    "Ports": [
      { "IP": "127.0.0.1", "PrivatePort": 5432, "PublicPort": 55432, "Type": "tcp" },
      { "PrivatePort": 9187, "Type": "tcp" }
    ],
    "NetworkSettings": { "Networks": { "shop_default": {} } }
  },
  {
    "Id": "2222222222",
    "Names": ["/shop-cache-1"],
    "Image": "redis:7",
    "State": "exited",
    "Status": "Exited (0) 5 minutes ago",
    "Labels": {
      "com.docker.compose.project": "shop",
      "com.docker.compose.service": "cache"
    }
  }
]"#;

/// Docker streams stats as newline-delimited JSON, one document per line, and
/// Bollard decodes it that way — so this must stay on a single line.
///
/// 50ms of CPU over 1000ms of machine time on 4 cores = 20%.
/// 100 MiB resident of which 40 MiB is page cache = 60 MiB.
const STATS: &str = concat!(
    r#"{"id":"1111111111","name":"/shop-db-1","#,
    r#""cpu_stats":{"cpu_usage":{"total_usage":50000000},"system_cpu_usage":1000000000,"online_cpus":4},"#,
    r#""precpu_stats":{"cpu_usage":{"total_usage":0},"system_cpu_usage":0,"online_cpus":4},"#,
    r#""memory_stats":{"usage":104857600,"stats":{"inactive_file":41943040}}}"#,
    "\n",
);

#[test]
fn inspects_a_fake_docker_daemon_end_to_end() {
    let dir = tempfile::tempdir().expect("temp dir");
    let socket = dir.path().join("docker.sock");

    // Set before the runtime exists, so no other thread can be reading the
    // environment concurrently.
    // SAFETY: single-threaded at this point; this binary runs one test.
    unsafe {
        std::env::set_var("DOCKER_HOST", format!("unix://{}", socket.display()));
    }

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");

    runtime.block_on(async move {
        let listener = UnixListener::bind(&socket).expect("bind fake docker socket");
        let server = tokio::spawn(serve(listener));

        let collector = DockerAvailability::detect()
            .await
            .into_collector()
            .expect("the fake daemon answers a ping");

        assert_stats_off(&collector, &socket).await;
        assert_stats_on(&collector).await;

        server.abort();
    });
}

async fn assert_stats_off(collector: &runscape_docker::BollardCollector, socket: &Path) {
    assert!(!collector.stats_enabled());

    let snapshot = collector.snapshot().await.expect("list containers");

    assert!(
        snapshot.duration.as_nanos() > 0,
        "collector duration must be measured"
    );
    assert_eq!(
        snapshot.containers.len(),
        2,
        "stopped containers are listed too"
    );
    assert_eq!(
        snapshot
            .containers
            .iter()
            .map(|c| c.identity.name.as_str())
            .collect::<Vec<_>>(),
        vec!["shop-cache-1", "shop-db-1"],
        "containers are sorted by identity name"
    );

    let running: Vec<_> = snapshot.running().collect();
    assert_eq!(running.len(), 1);

    let db = running[0];
    assert_eq!(db.id, "1111111111");
    assert_eq!(db.image, "postgres:16");
    assert_eq!(db.state, ContainerState::Running);
    assert_eq!(db.status, "Up 2 minutes");
    assert_eq!(db.identity.compose_project.as_deref(), Some("shop"));
    assert_eq!(db.identity.compose_service.as_deref(), Some("db"));
    assert_eq!(db.networks, vec!["shop_default".to_owned()]);
    assert_eq!(db.ports.len(), 2, "exposed-only ports are still observed");
    assert_eq!(db.cpu_percent, None, "stats are off");
    assert_eq!(db.memory_bytes, None);

    let service = db.to_service(None, snapshot.captured_at);
    assert_eq!(service.name, "db");
    assert_eq!(service.fingerprint, "container|shop|db");
    assert_eq!(service.health, runscape_core::Health::Healthy);
    assert_eq!(
        service.endpoints.len(),
        1,
        "only the published port is reachable from the host"
    );
    assert_eq!(service.primary_port(), Some(55432));

    let cache = snapshot.containers[0].to_service(None, snapshot.captured_at);
    assert_eq!(cache.name, "cache");
    assert_eq!(cache.health, runscape_core::Health::Stopped);
    assert!(cache.endpoints.is_empty());
    assert_ne!(service.id, cache.id);

    // The daemon must not have needed anything beyond the endpoints the fake
    // implements, which the 404 branch would have turned into an error.
    assert!(socket.exists());
}

async fn assert_stats_on(collector: &runscape_docker::BollardCollector) {
    let collector = collector.clone().with_stats(true);
    assert!(collector.stats_enabled());

    let snapshot = collector
        .snapshot()
        .await
        .expect("list containers with stats");

    let db = snapshot
        .running()
        .next()
        .expect("the running container is still there");
    let cpu = db.cpu_percent.expect("cpu was sampled");
    assert!((cpu - 20.0).abs() < 0.01, "got {cpu}");
    assert_eq!(db.memory_bytes, Some(60 * 1024 * 1024));

    let cache = &snapshot.containers[0];
    assert_eq!(cache.identity.name, "shop-cache-1");
    assert_eq!(
        cache.cpu_percent, None,
        "a stopped container is not worth a stats round-trip"
    );
    assert_eq!(cache.memory_bytes, None);
}

/// Answer Docker API requests until the test aborts the task.
async fn serve(listener: UnixListener) {
    while let Ok((stream, _)) = listener.accept().await {
        tokio::spawn(handle(stream));
    }
}

async fn handle(mut stream: UnixStream) {
    let mut request = Vec::with_capacity(1024);
    let mut chunk = [0u8; 512];

    // Read up to the end of the headers; none of these requests has a body.
    loop {
        match stream.read(&mut chunk).await {
            Ok(0) => return,
            Ok(read) => {
                request.extend_from_slice(&chunk[..read]);
                if request.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
                if request.len() > 64 * 1024 {
                    return;
                }
            }
            Err(_) => return,
        }
    }

    let head = String::from_utf8_lossy(&request);
    let path = head.split_whitespace().nth(1).unwrap_or_default();
    let response = match route(path) {
        Some((content_type, body)) => format!(
            "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        ),
        // A route the collector should never need; loud rather than silent.
        None => {
            "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_owned()
        }
    };

    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.shutdown().await;
}

/// Route by suffix so that Bollard's API-version path prefix and query strings
/// do not matter.
fn route(path: &str) -> Option<(&'static str, &'static str)> {
    let path = path.split('?').next().unwrap_or_default();

    if path.ends_with("/_ping") {
        Some(("text/plain", "OK"))
    } else if path.ends_with("/containers/json") {
        Some(("application/json", CONTAINERS))
    } else if path.ends_with("/stats") {
        Some(("application/json", STATS))
    } else {
        None
    }
}
