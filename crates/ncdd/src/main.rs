mod config;
mod device;
mod driver;
mod netlink;
mod start;

#[cfg(target_os = "linux")]
#[tokio::main]
async fn main() {
    if !driver::load_module() {
        std::process::exit(1);
    }
    start::run().await;
}

#[cfg(not(target_os = "linux"))]
fn main() {
    compile_error!("ncdd only supports Linux");
}
