use criterion::{Criterion, criterion_group, criterion_main};
use std::net::Ipv4Addr;
use std::time::{Duration, Instant};

use libncd_runtime::{ConnHandler, OpenParams, close, open, read, write};

/// A multi-threaded tokio runtime for benchmarks.
fn tokio_rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap()
}

/// Pick a random free port on localhost.
async fn pick_free_port() -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("Failed to bind to port 0");
    listener.local_addr().unwrap().port()
}

/// Open a connected host-device pair.
async fn pair() -> (ConnHandler, ConnHandler) {
    let port = pick_free_port().await;
    let (host, device) = tokio::join!(
        open(OpenParams::Host {
            listen_addr: std::net::IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)),
            listen_port: port,
        }),
        open(OpenParams::Device {
            host_addr: std::net::IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)),
            host_port: port,
        }),
    );
    (host.unwrap(), device.unwrap())
}

/// Print a concise percentile summary line (goes to stderr so it does not
/// interfere with criterion's structured stdout output).
fn emit_percentiles(label: &str, mut latencies: Vec<Duration>) {
    latencies.sort_unstable();
    let n = latencies.len();
    let avg = latencies.iter().sum::<Duration>() / n as u32;
    let p50 = latencies[n / 2];
    let p95 = latencies[n * 95 / 100];
    let p99 = latencies[n * 99 / 100];
    eprintln!(
        "  [{label}]  n={n}  avg={avg:.2?}  p50={p50:.2?}  p95={p95:.2?}  p99={p99:.2?}  \
         min={min:.2?}  max={max:.2?}",
        min = latencies.first().unwrap(),
        max = latencies.last().unwrap(),
    );
}

const WARMUP_ITERS: usize = 300;

/// Run a ping-pong RTT measurement.
///
/// Spawns an echo server on the device side, sends `msg_size` bytes from the
/// host, waits for the echoed reply, and collects individual round‑trip
/// timings.  Returns the per‑sample latencies and the total wall‑clock time
/// across all iterations.
async fn run_ping_pong(msg_size: usize, warmup: usize, iters: u64) -> (Vec<Duration>, Duration) {
    let (mut host, device) = pair().await;

    // Spawn echo server on the device side
    let mut echo_device = device;
    let echo = tokio::spawn(async move {
        loop {
            match read(&mut echo_device).await {
                Ok(data) => {
                    if write(&mut echo_device, data).await.is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    // Warmup phase
    let warmup_msg = vec![0u8; msg_size];
    for _ in 0..warmup {
        write(&mut host, warmup_msg.clone()).await.unwrap();
        let _ = read(&mut host).await.unwrap();
    }

    // Measurement phase — every iteration is a single ping-pong RTT
    let mut latencies = Vec::with_capacity(iters as usize);
    let msg = vec![0u8; msg_size];
    let total_start = Instant::now();
    for _ in 0..iters {
        let t0 = Instant::now();
        write(&mut host, msg.clone()).await.unwrap();
        let _ = read(&mut host).await.unwrap();
        latencies.push(t0.elapsed());
    }
    let total_elapsed = total_start.elapsed();

    // Cleanup
    close(host).await.ok();
    echo.abort();

    (latencies, total_elapsed)
}

/// Ping-pong RTT latency with 64‑byte payloads.
fn latency_ping_pong_64b(c: &mut Criterion) {
    let rt = tokio_rt();
    let mut g = c.benchmark_group("latency");
    g.sample_size(10);
    g.measurement_time(Duration::from_secs(30));

    g.bench_function("ping-pong-rtt-64B", |b| {
        b.iter_custom(|iters| {
            rt.block_on(async {
                let (latencies, total) = run_ping_pong(64, WARMUP_ITERS, iters).await;
                // Only print percentiles during the final measurement phase
                // (criterion uses small `iters` during calibration).
                if iters >= 5_000 {
                    emit_percentiles("ping-pong-rtt-64B", latencies);
                }
                total
            })
        });
    });

    g.finish();
}

/// Ping-pong RTT latency with 1 KB payloads.
fn latency_ping_pong_1kb(c: &mut Criterion) {
    let rt = tokio_rt();
    let mut g = c.benchmark_group("latency");
    g.sample_size(10);
    g.measurement_time(Duration::from_secs(30));

    g.bench_function("ping-pong-rtt-1KB", |b| {
        b.iter_custom(|iters| {
            rt.block_on(async {
                let (latencies, total) = run_ping_pong(1024, WARMUP_ITERS, iters).await;
                if iters >= 5_000 {
                    emit_percentiles("ping-pong-rtt-1KB", latencies);
                }
                total
            })
        });
    });

    g.finish();
}

/// Ping-pong RTT latency with 64 KB payloads.
fn latency_ping_pong_64kb(c: &mut Criterion) {
    let rt = tokio_rt();
    let mut g = c.benchmark_group("latency");
    g.sample_size(10);
    g.measurement_time(Duration::from_secs(30));

    g.bench_function("ping-pong-rtt-64KB", |b| {
        b.iter_custom(|iters| {
            rt.block_on(async {
                let (latencies, total) = run_ping_pong(64 * 1024, WARMUP_ITERS, iters).await;
                if iters >= 5_000 {
                    emit_percentiles("ping-pong-rtt-64KB", latencies);
                }
                total
            })
        });
    });

    g.finish();
}

criterion_group!(
    name = latency;
    config = Criterion::default().measurement_time(Duration::from_secs(30));
    targets = latency_ping_pong_64b, latency_ping_pong_1kb, latency_ping_pong_64kb,
);

criterion_main!(latency);
