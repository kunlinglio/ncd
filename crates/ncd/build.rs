use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use flate2::read::GzDecoder;
use tar::Archive;
use zip::CompressionMethod;
use zip::write::SimpleFileOptions;

use sha2::{Digest, Sha256};

/// Parsed from `[package.metadata.ncd]` in Cargo.toml.
struct BundleMeta {
    python_version: String,
    pbs_release: String,
}

fn log(msg: impl Into<String>) {
    let msg = msg.into();
    // cargo:warning= messages are always shown during the build, so the
    // user can see what the build script is doing even on success.
    println!("cargo:warning=[NCD Building] {msg}");
}

fn main() {
    // Re-run build script when these files change
    println!("cargo:rerun-if-changed=adapters/");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=Cargo.toml");

    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let target_dir = out_dir.join("../../../..").canonicalize().unwrap();
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());

    // Download Python and resolve Python home
    let meta = read_bundle_meta();
    let python_home = download_from_pbs(&meta, &target_dir.join("pbs-cache"));
    let python_bin = python_bin_path(&python_home);
    assert!(
        python_bin.exists(),
        "Python binary not found at {}",
        python_bin.display()
    );

    // Assemble bundle directory
    let bundle_dir = out_dir.join("bundle");
    if bundle_dir.exists() {
        let _ = std::fs::remove_dir_all(&bundle_dir);
    }
    std::fs::create_dir_all(&bundle_dir).unwrap();

    // 1. Copy entire Python installation
    log(format!("Copying Python from {} ...", python_home.display()));
    copy_dir_all(&python_home, &bundle_dir.join("python")).unwrap();

    // 2. Copy adapters to a staging directory first so `uv pip install` doesn't
    //    pollute the source tree (which would invalidate the build cache).
    let adapters_src = manifest_dir.join("adapters");
    let adapters_staging = out_dir.join("adapters-staging");
    if adapters_staging.exists() {
        let _ = std::fs::remove_dir_all(&adapters_staging);
    }
    copy_dir_all(&adapters_src, &adapters_staging).unwrap();

    let site_packages_dir = bundle_dir.join("site-packages");
    log("Installing python dependencies...");
    let status = Command::new("uv")
        .args(["pip", "install", "--python"])
        .arg(&python_bin)
        .args(["--target"])
        .arg(&site_packages_dir)
        .arg(&adapters_staging)
        .status()
        .expect("Failed to execute uv");
    assert!(
        status.success(),
        "uv pip install failed — check network connectivity"
    );

    // 3. Copy adapter scripts (from clean source, not staging)
    copy_adapters(&adapters_src, &bundle_dir.join("adapters")).unwrap();

    // Create deflated zip archive
    let bundle_zip = out_dir.join("bundle.zip");
    log("Packaging bundle archive...");
    {
        let file = std::fs::File::create(&bundle_zip).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        add_dir_to_zip(&mut zip, &bundle_dir, "").unwrap();
        zip.finish().unwrap();
    }

    // Pre-compute fingerprint for runtime validation
    let bundle_bytes = std::fs::read(&bundle_zip).unwrap();
    let hash = bundle_fingerprint(&bundle_bytes);
    log(format!("BUNDLE_HASH={hash}"));
    println!("cargo:rustc-env=BUNDLE_HASH={hash}");
}

/// Read `[package.metadata.python-bundle]` from the crate's Cargo.toml.
fn read_bundle_meta() -> BundleMeta {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("Cargo.toml");
    let content = std::fs::read_to_string(&manifest).expect("Failed to read Cargo.toml");
    let parsed: toml::Value = toml::from_str(&content).expect("Failed to parse Cargo.toml");

    let meta = &parsed["package"]["metadata"]["python-bundle"];
    BundleMeta {
        python_version: meta["python-version"]
            .as_str()
            .expect("package.metadata.python-bundle.python-version must be a string")
            .to_string(),
        pbs_release: meta["pbs-release"]
            .as_str()
            .expect("package.metadata.python-bundle.pbs-release must be a string")
            .to_string(),
    }
}

/// Download `python-build-standalone` from GitHub into cache_dir.
fn download_from_pbs(meta: &BundleMeta, cache_dir: &Path) -> PathBuf {
    let target = pbs_target();
    let asset = format!(
        "cpython-{}+{}-{target}-install_only.tar.gz",
        meta.python_version, meta.pbs_release,
    );
    let url = format!(
        "https://github.com/astral-sh/python-build-standalone/releases/download/{}/{asset}",
        meta.pbs_release,
    );

    let _ = std::fs::create_dir_all(cache_dir);
    let tarball = cache_dir.join(&asset);

    if !tarball.exists() {
        log(format!("Downloading {url} ..."));
        download(&url, &tarball);
    }

    let python_dir = cache_dir.join("python");
    if !python_bin_path(&python_dir).exists() {
        log("Extracting python-build-standalone ...".to_string());
        extract_pbs(&tarball, &cache_dir);
    }

    python_dir
}

/// Map the Rust `TARGET` triple to a python-build-standalone target name.
fn pbs_target() -> &'static str {
    match std::env::var("TARGET").unwrap().as_str() {
        "aarch64-apple-darwin" => "aarch64-apple-darwin",
        "x86_64-apple-darwin" => "x86_64-apple-darwin",
        "x86_64-unknown-linux-gnu" => "x86_64-unknown-linux-gnu",
        "aarch64-unknown-linux-gnu" => "aarch64-unknown-linux-gnu",
        "x86_64-pc-windows-msvc" => "x86_64-pc-windows-msvc",
        other => panic!(
            "Unsupported target: {other}.\n\
             Set NCD_PYTHON_HOME=/path/to/python/install to use a pre-downloaded Python."
        ),
    }
}

fn download(url: &str, dest: &Path) {
    let agent = ureq::AgentBuilder::new().try_proxy_from_env(true).build();

    let resp = agent
        .get(url)
        .set("User-Agent", "ncd-build/0.1")
        .call()
        .unwrap_or_else(|e| panic!("Failed to download {url}: {e}"));

    let len: u64 = resp
        .header("Content-Length")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);

    let mut reader = resp.into_reader();
    // Write to a .part file first; rename on success so interrupted
    // downloads don't leave a truncated file that looks complete.
    let part = dest.with_extension("part");
    let mut file = std::fs::File::create(&part).unwrap();
    let mut buf = [0u8; 8192];
    let mut downloaded: u64 = 0;
    let mut last_milestone: u64 = 0;
    loop {
        let n = reader
            .read(&mut buf)
            .unwrap_or_else(|e| panic!("Download error: {e}"));
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n]).unwrap();
        downloaded += n as u64;
        if len > 0 {
            let pct = (downloaded * 100) / len;
            let milestone = pct / 25;
            if milestone > last_milestone {
                log(format!(
                    "Downloading {}% ({}/{} bytes)",
                    pct,
                    downloaded,
                    len,
                ));
                last_milestone = milestone;
            }
        }
    }
    drop(file);
    // On Windows, rename fails if the destination already exists (unlike Unix
    // where it atomically replaces). Remove it first so the rename succeeds.
    #[cfg(windows)]
    {
        if dest.exists() {
            std::fs::remove_file(dest)
                .unwrap_or_else(|e| panic!("Failed to remove existing file {}: {}", dest.display(), e));
        }
    }
    std::fs::rename(&part, dest).unwrap();
    if len > 0 && downloaded != len {
        panic!(
            "Download incomplete: expected {len} bytes, got {downloaded} — \
             delete the PBS cache and rebuild"
        );
    }
}

fn extract_pbs(tarball: &Path, dest: &Path) {
    let file = std::fs::File::open(tarball)
        .unwrap_or_else(|e| panic!("Failed to open {}: {}", tarball.display(), e));
    let decoder = GzDecoder::new(file);
    let mut archive = Archive::new(decoder);

    // Extract entries individually so we can skip symlinks on Windows
    // (creating symlinks requires elevated privileges or Developer Mode).
    for entry_result in archive
        .entries()
        .unwrap_or_else(|e| panic!("Failed to read archive entries in {}: {}", tarball.display(), e))
    {
        let mut entry = entry_result
            .unwrap_or_else(|e| panic!("Failed to read archive entry in {}: {}", tarball.display(), e));

        let kind = entry.header().entry_type();
        // On Windows, skip symlinks and hardlinks — they require elevated privileges.
        if cfg!(windows) && (kind == tar::EntryType::Symlink || kind == tar::EntryType::Link) {
            continue;
        }

        entry
            .unpack_in(dest)
            .unwrap_or_else(|e| panic!("Failed to extract entry from {}: {}", tarball.display(), e));
    }
}

fn python_exe_name() -> &'static str {
    if cfg!(target_os = "windows") {
        "python.exe"
    } else {
        "python3"
    }
}

/// Return the path to the Python interpreter inside a PBS installation directory.
fn python_bin_path(python_home: &Path) -> PathBuf {
    if cfg!(target_os = "windows") {
        // Windows PBS layout: python/python.exe (no "bin" subdirectory)
        python_home.join(python_exe_name())
    } else {
        // Unix PBS layout: python/bin/python3
        python_home.join("bin").join(python_exe_name())
    }
}

/// Recursively copy a directory tree.
fn copy_dir_all(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let name = entry.file_name();
        let src_path = entry.path();
        let dst_path = dst.join(&name);

        if ty.is_dir() {
            copy_dir_all(&src_path, &dst_path)?;
        } else if ty.is_symlink() {
            #[cfg(unix)]
            {
                let target = std::fs::read_link(&src_path)?;
                std::os::unix::fs::symlink(&target, &dst_path)?;
            }
            #[cfg(windows)]
            {
                // Windows symlinks require elevated privileges; copy the resolved target instead.
                // If the target cannot be resolved (e.g., broken symlink), skip it gracefully.
                match std::fs::canonicalize(&src_path) {
                    Ok(resolved) => {
                        if resolved.is_dir() {
                            copy_dir_all(&resolved, &dst_path)?;
                        } else {
                            std::fs::copy(&resolved, &dst_path)?;
                        }
                    }
                    Err(e) => {
                        // Broken or unresolvable symlink — skip it.
                        log(format!(
                            "Skipping symlink {} (cannot resolve target): {e}",
                            src_path.display()
                        ));
                    }
                }
            }
        } else {
            std::fs::copy(&src_path, &dst_path)?;
        }
    }
    Ok(())
}

/// Copy only the adapter files we need: *.py, *.toml, skipping venv/cache artifacts.
fn copy_adapters(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        let src_path = entry.path();
        let dst_path = dst.join(&name);

        // Skip venv, cache dirs, and uv metadata
        let skip = matches!(name_str.as_ref(), ".venv" | "__pycache__" | ".DS_Store")
            || name_str.ends_with(".pyc")
            || name_str == "uv.lock"
            || name_str == "pyproject.toml"
            || name_str.starts_with('.');

        if skip {
            continue;
        }

        if entry.file_type()?.is_dir() {
            copy_adapters(&src_path, &dst_path)?;
        } else {
            std::fs::copy(&src_path, &dst_path)?;
        }
    }
    Ok(())
}

/// Walk `dir` recursively, adding every entry to the zip under `prefix`.
fn add_dir_to_zip<W: Write + std::io::Seek>(
    zip: &mut zip::ZipWriter<W>,
    dir: &Path,
    prefix: &str,
) -> std::io::Result<()> {
    let dir_opts = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .unix_permissions(0o755);
    let file_opts = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .unix_permissions(0o644);
    let exe_opts = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .unix_permissions(0o755);

    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name();
        let zip_path = if prefix.is_empty() {
            name.to_string_lossy().into_owned()
        } else {
            format!("{prefix}/{}", name.to_string_lossy())
        };

        let ft = entry.file_type()?;

        if ft.is_dir() {
            zip.add_directory(&zip_path, dir_opts)?;
            add_dir_to_zip(zip, &path, &zip_path)?;
        } else if ft.is_symlink() {
            let target = std::fs::read_link(&path)?;
            zip.add_symlink(&zip_path, target.display().to_string(), dir_opts)?;
        } else {
            let opts = if is_executable(&path) {
                exe_opts
            } else {
                file_opts
            };
            zip.start_file(&zip_path, opts)?;
            let mut f = std::fs::File::open(&path)?;
            std::io::copy(&mut f, zip)?;
        }
    }
    Ok(())
}

/// Return `true` if the file has any executable bit set.
fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path)
            .map(|m| m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(windows)]
    {
        // On Windows, executability is determined by file extension
        path.extension()
            .and_then(|e| e.to_str())
            .map(|e| {
                e.eq_ignore_ascii_case("exe")
                    || e.eq_ignore_ascii_case("bat")
                    || e.eq_ignore_ascii_case("cmd")
            })
            .unwrap_or(false)
    }
}

/// SHA-256 hex digest of the bundle (first 16 chars — enough for uniqueness).
fn bundle_fingerprint(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    format!("{:016x}", hasher.finalize())
}
