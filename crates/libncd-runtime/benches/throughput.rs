use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use std::net::Ipv4Addr;
use std::time::Duration;

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

/// Unidirectional throughput: host → device, 100 × 64 KB messages.
fn throughput_1kb(c: &mut Criterion) {
    let rt = tokio_rt();
    const MSG_SIZE: usize = 1 * 1024; // 1 KB
    const MSG_COUNT: usize = 1000;

    let total = (MSG_SIZE * MSG_COUNT) as u64;
    let mut g = c.benchmark_group("throughput-1KB");
    g.throughput(Throughput::Bytes(total));
    g.sample_size(10);

    g.bench_function("unidirectional", |b| {
        b.iter_with_setup(
            || rt.block_on(async { (pair().await, vec![0u8; MSG_SIZE]) }),
            |((mut host, mut device), msg)| {
                rt.block_on(async {
                    let sender = tokio::spawn(async move {
                        for _ in 0..MSG_COUNT {
                            write(&mut host, msg.clone()).await.unwrap();
                        }
                        close(host).await.ok();
                    });
                    let receiver = tokio::spawn(async move {
                        let mut total = 0usize;
                        while let Ok(data) = read(&mut device).await {
                            total += data.len();
                            if total >= MSG_SIZE * MSG_COUNT {
                                break;
                            }
                        }
                    });
                    let _ = tokio::join!(sender, receiver);
                });
            },
        );
    });

    g.finish();
}

/// Unidirectional throughput: host → device, 100 × 64 KB messages.
fn throughput_64kb(c: &mut Criterion) {
    let rt = tokio_rt();
    const MSG_SIZE: usize = 64 * 1024; // 64 KB
    const MSG_COUNT: usize = 200;

    let total = (MSG_SIZE * MSG_COUNT) as u64;
    let mut g = c.benchmark_group("throughput-64KB");
    g.throughput(Throughput::Bytes(total));
    g.sample_size(10);

    g.bench_function("unidirectional", |b| {
        b.iter_with_setup(
            || rt.block_on(async { (pair().await, vec![0u8; MSG_SIZE]) }),
            |((mut host, mut device), msg)| {
                rt.block_on(async {
                    let sender = tokio::spawn(async move {
                        for _ in 0..MSG_COUNT {
                            write(&mut host, msg.clone()).await.unwrap();
                        }
                        close(host).await.ok();
                    });
                    let receiver = tokio::spawn(async move {
                        let mut total = 0usize;
                        while let Ok(data) = read(&mut device).await {
                            total += data.len();
                            if total >= MSG_SIZE * MSG_COUNT {
                                break;
                            }
                        }
                    });
                    let _ = tokio::join!(sender, receiver);
                });
            },
        );
    });

    g.finish();
}

/// Unidirectional throughput: host → device, 100 × 1 MB messages.
fn throughput_1mb(c: &mut Criterion) {
    let rt = tokio_rt();
    const MSG_SIZE: usize = 1024 * 1024; // 1 MB
    const MSG_COUNT: usize = 100;

    let total = (MSG_SIZE * MSG_COUNT) as u64;
    let mut g = c.benchmark_group("throughput-1MB");
    g.throughput(Throughput::Bytes(total));
    g.sample_size(10);

    g.bench_function("unidirectional", |b| {
        b.iter_with_setup(
            || rt.block_on(async { (pair().await, vec![0u8; MSG_SIZE]) }),
            |((mut host, mut device), msg)| {
                rt.block_on(async {
                    let sender = tokio::spawn(async move {
                        for _ in 0..MSG_COUNT {
                            write(&mut host, msg.clone()).await.unwrap();
                        }
                        close(host).await.ok();
                    });
                    let receiver = tokio::spawn(async move {
                        let mut total = 0usize;
                        while let Ok(data) = read(&mut device).await {
                            total += data.len();
                            if total >= MSG_SIZE * MSG_COUNT {
                                break;
                            }
                        }
                    });
                    let _ = tokio::join!(sender, receiver);
                });
            },
        );
    });

    g.finish();
}

/// Unidirectional throughput: host → device, 20 × 10 MB messages.
fn throughput_10mb(c: &mut Criterion) {
    let rt = tokio_rt();
    const MSG_SIZE: usize = 10 * 1024 * 1024; // 10 MB
    const MSG_COUNT: usize = 20;

    let total = (MSG_SIZE * MSG_COUNT) as u64;
    let mut g = c.benchmark_group("throughput-10MB");
    g.throughput(Throughput::Bytes(total));
    g.sample_size(10);

    g.bench_function("unidirectional", |b| {
        b.iter_with_setup(
            || rt.block_on(async { (pair().await, vec![0u8; MSG_SIZE]) }),
            |((mut host, mut device), msg)| {
                rt.block_on(async {
                    let sender = tokio::spawn(async move {
                        for _ in 0..MSG_COUNT {
                            write(&mut host, msg.clone()).await.unwrap();
                        }
                        close(host).await.ok();
                    });
                    let receiver = tokio::spawn(async move {
                        let mut total = 0usize;
                        while let Ok(data) = read(&mut device).await {
                            total += data.len();
                            if total >= MSG_SIZE * MSG_COUNT {
                                break;
                            }
                        }
                    });
                    let _ = tokio::join!(sender, receiver);
                });
            },
        );
    });

    g.finish();
}

criterion_group!(
    name = throughput;
    config = Criterion::default().measurement_time(Duration::from_secs(15));
    targets = throughput_1kb, throughput_64kb, throughput_1mb, throughput_10mb,
);

criterion_main!(throughput);
