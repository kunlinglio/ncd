//! Self-contained Python runtime bundle.
//!
//! Embeds a deflated zip archive (built by `build.rs`) containing:
//!   `python/`        — relocatable CPython installation
//!   `site-packages/` — pip-installed packages (pyserial)
//!   `adapters/`      — NCD adapter scripts + adapter_list.toml
//!
//! On first access the archive is extracted to a temp directory.
//! The bundle fingerprint is embedded in the directory name, so version
//! changes automatically trigger re-extraction.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use sha2::{Digest, Sha256};
use tokio::process::Command as AsyncCommand;

const BUNDLE_BYTES: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/bundle.zip"));
const BUNDLE_HASH: &str = env!("BUNDLE_HASH");

static BUNDLE: OnceLock<Bundle> = OnceLock::new();

pub fn init() -> Result<(), BundleError> {
    if BUNDLE.get().is_some() {
        return Ok(());
    }
    let bundle = Bundle::init_inner()?;
    let _ = BUNDLE.set(bundle);
    Ok(())
}

pub fn run_python(script_path: &Path) -> AsyncCommand {
    let mut cmd = AsyncCommand::new(python_path());
    cmd.arg(script_path)
        .env("PYTHONHOME", python_home())
        .env("PYTHONPATH", pythonpath());
    cmd
}

pub fn run_python_sync(script_path: &Path) -> std::process::Command {
    let mut cmd = std::process::Command::new(python_path());
    cmd.arg(script_path)
        .env("PYTHONHOME", python_home())
        .env("PYTHONPATH", pythonpath());
    cmd
}

pub fn drivers_dir() -> &'static Path {
    static CACHED: OnceLock<PathBuf> = OnceLock::new();
    CACHED.get_or_init(|| bundle().cache_dir.join("adapters"))
}

fn python_path() -> &'static Path {
    static CACHED: OnceLock<PathBuf> = OnceLock::new();
    CACHED.get_or_init(|| {
        bundle()
            .cache_dir
            .join("python")
            .join("bin")
            .join(python_binary_name())
    })
}

fn python_home() -> &'static Path {
    static CACHED: OnceLock<PathBuf> = OnceLock::new();
    CACHED.get_or_init(|| bundle().cache_dir.join("python"))
}

fn pythonpath() -> String {
    format!(
        "{}:{}",
        drivers_dir().display(),
        site_packages_dir().display(),
    )
}

struct Bundle {
    cache_dir: PathBuf,
}

impl Bundle {
    fn init_inner() -> Result<Self, BundleError> {
        let cache_dir = bundle_dir_path();
        let hash_marker = cache_dir.join(".bundle_hash");

        // Check whether the cached extraction is valid.
        let needs_extract = !hash_marker.is_file()
            || std::fs::read_to_string(&hash_marker)
                .map(|s| s.trim() != BUNDLE_HASH)
                .unwrap_or(true);

        if needs_extract {
            if cache_dir.exists() {
                std::fs::remove_dir_all(&cache_dir)
                    .map_err(|e| BundleError::io(e, "removing stale bundle"))?;
            }
            std::fs::create_dir_all(&cache_dir)
                .map_err(|e| BundleError::io(e, "creating bundle dir"))?;

            eprintln!("ncd: Extracting Python runtime bundle...");
            let reader = std::io::Cursor::new(BUNDLE_BYTES);
            let mut archive = zip::ZipArchive::new(reader).map_err(|e| {
                BundleError::io(
                    std::io::Error::new(std::io::ErrorKind::Other, e),
                    "opening bundle zip",
                )
            })?;

            for i in 0..archive.len() {
                let mut entry = archive.by_index(i).map_err(|e| {
                    BundleError::io(
                        std::io::Error::new(std::io::ErrorKind::Other, e),
                        "reading zip entry",
                    )
                })?;
                let out_path = cache_dir.join(entry.name());

                if entry.is_dir() {
                    std::fs::create_dir_all(&out_path)
                        .map_err(|e| BundleError::io(e, "creating dir"))?;
                } else if entry.is_symlink() {
                    let mut target = String::new();
                    std::io::Read::read_to_string(&mut entry, &mut target)
                        .map_err(|e| BundleError::io(e, "reading symlink"))?;
                    if let Some(parent) = out_path.parent() {
                        std::fs::create_dir_all(parent)
                            .map_err(|e| BundleError::io(e, "creating parent dirs"))?;
                    }
                    let _ = std::fs::remove_file(&out_path);
                    #[cfg(unix)]
                    {
                        std::os::unix::fs::symlink(Path::new(target.trim()), &out_path)
                            .map_err(|e| BundleError::io(e, "creating symlink"))?;
                    }
                } else {
                    if let Some(parent) = out_path.parent() {
                        std::fs::create_dir_all(parent)
                            .map_err(|e| BundleError::io(e, "creating parent dirs"))?;
                    }
                    let mut out = std::fs::File::create(&out_path)
                        .map_err(|e| BundleError::io(e, "creating file"))?;
                    std::io::copy(&mut entry, &mut out)
                        .map_err(|e| BundleError::io(e, "extracting file"))?;
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        if let Some(mode) = entry.unix_mode() {
                            out.set_permissions(std::fs::Permissions::from_mode(mode))
                                .ok();
                        }
                    }
                }
            }

            // Verify the embedded data hasn't been corrupted.
            let actual_hash = bundle_fingerprint(BUNDLE_BYTES);
            if actual_hash != BUNDLE_HASH {
                let _ = std::fs::remove_dir_all(&cache_dir);
                return Err(BundleError::HashMismatch {
                    expected: BUNDLE_HASH.to_string(),
                    actual: actual_hash,
                });
            }

            // Write hash marker so we can skip extraction on the next run.
            std::fs::write(&hash_marker, BUNDLE_HASH)
                .map_err(|e| BundleError::io(e, "writing hash marker"))?;

            eprintln!("ncd: Bundle extracted to {}", cache_dir.display());
        }

        Ok(Self { cache_dir })
    }
}

fn bundle() -> &'static Bundle {
    BUNDLE
        .get()
        .expect("bundle::init() must be called before accessing bundle paths")
}

fn site_packages_dir() -> &'static Path {
    static CACHED: OnceLock<PathBuf> = OnceLock::new();
    CACHED.get_or_init(|| bundle().cache_dir.join("site-packages"))
}

#[cfg(not(target_os = "windows"))]
fn python_binary_name() -> &'static str {
    "python3"
}

#[cfg(target_os = "windows")]
fn python_binary_name() -> &'static str {
    "python.exe"
}

fn bundle_dir_path() -> PathBuf {
    std::env::temp_dir().join(format!("ncd-{BUNDLE_HASH}"))
}

/// SHA-256 hex digest of the bundle (first 16 chars — same as build.rs).
fn bundle_fingerprint(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    format!("{:016x}", hasher.finalize())
}

#[derive(Debug, thiserror::Error)]
pub enum BundleError {
    #[error("Bundle I/O error ({0}): {1}")]
    Io(std::io::Error, String),
    #[error(
        "Bundle hash mismatch: expected {expected}, got {actual}. \
         The embedded bundle may be corrupted — rebuild the project."
    )]
    HashMismatch { expected: String, actual: String },
}

impl BundleError {
    fn io(e: std::io::Error, ctx: impl Into<String>) -> Self {
        Self::Io(e, ctx.into())
    }
}
