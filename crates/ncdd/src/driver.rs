//! Extract, build, and load the ncd kernel module at startup.

use std::process::Command;

/// Unload the old ncd module, then build and load the new one
/// from the embedded source archive.
pub fn load_module() -> bool {
    // 0. Unload any previous instance
    if module_loaded("ncd") {
        println!("ncdd: unloading old ncd module ...");
        let _ = Command::new("rmmod").arg("ncd").status();
    }

    // 1. Try modprobe first (driver may have been installed by DKMS)
    if Command::new("modprobe")
        .arg("ncd")
        .status()
        .is_ok_and(|s| s.success())
    {
        return true;
    }

    // 2. Extract the bundled driver source to a temp directory
    let tmp = match tempfile::TempDir::new() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("ncdd: cannot create temp dir: {}", e);
            return false;
        }
    };

    let archive = include_bytes!(env!("DRIVER_ARCHIVE"));
    let mut ar = tar::Archive::new(&archive[..]);
    if let Err(e) = ar.unpack(tmp.path()) {
        eprintln!("ncdd: cannot extract driver source: {}", e);
        return false;
    }

    println!("ncdd: building driver from bundled source ...");

    // 3. make
    let make = Command::new("make").arg("-C").arg(tmp.path()).output();
    match make {
        Ok(o) if o.status.success() => {}
        Ok(o) => {
            eprintln!(
                "ncdd: driver build failed:\n{}",
                String::from_utf8_lossy(&o.stderr)
            );
            return false;
        }
        Err(e) => {
            eprintln!(
                "ncdd: make not found — install build-essential linux-headers-$(uname -r): {}",
                e
            );
            return false;
        }
    }

    // 4. load the built .ko
    let ko = tmp.path().join("build").join("ncd.ko");
    if !ko.exists() {
        eprintln!("ncdd: driver built but {} not found", ko.display());
        return false;
    }

    if !Command::new("insmod")
        .arg(&ko)
        .status()
        .is_ok_and(|s| s.success())
    {
        eprintln!("ncdd: insmod failed — run as root");
        return false;
    }

    println!("ncdd: driver loaded");
    // TempDir is dropped here, source cleaned up
    true
}

fn module_loaded(name: &str) -> bool {
    std::fs::read_to_string("/proc/modules")
        .map(|s| s.lines().any(|l| l.starts_with(name)))
        .unwrap_or(false)
}
