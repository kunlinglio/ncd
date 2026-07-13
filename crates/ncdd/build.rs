use std::path::Path;

fn main() {
    let driver_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("driver");

    let out = std::env::var("OUT_DIR").unwrap();
    let dst = Path::new(&out).join("driver.tar");

    let mut ar = tar::Builder::new(std::fs::File::create(&dst).unwrap());
    for entry in walkdir::WalkDir::new(&driver_root)
        .min_depth(1)
        .into_iter()
        .filter_entry(|e| {
            // skip build artifacts
            e.file_name() != "build"
        })
    {
        let entry = entry.unwrap();
        let path = entry.path();
        let name = path.strip_prefix(&driver_root).unwrap();
        if path.is_dir() {
            ar.append_dir(name, path).unwrap();
        } else {
            ar.append_path_with_name(path, name).unwrap();
        }
    }
    ar.finish().unwrap();

    println!("cargo:rerun-if-changed={}", driver_root.display());
    println!("cargo:rustc-env=DRIVER_ARCHIVE={}", dst.display());
}
