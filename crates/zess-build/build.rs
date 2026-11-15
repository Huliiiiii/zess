use std::{env, fs, path::Path};

use sha2::{Digest, Sha256};

fn main() {
    let manifest = env::var_os("CARGO_MANIFEST_DIR").unwrap();
    let root = Path::new(&manifest).parent().unwrap().parent().unwrap();
    let mut files = Vec::new();
    for path in [
        "Cargo.toml",
        "Cargo.lock",
        "src",
        "crates",
        "plugins",
        "vendor",
        ".cargo",
    ] {
        let path = root.join(path);
        println!("cargo:rerun-if-changed={}", path.display());
        if path.exists() {
            collect(&path, &mut files);
        }
    }
    let mut files: Vec<_> = files
        .into_iter()
        .map(|path| {
            let relative = path.strip_prefix(root).unwrap();
            let name = relative
                .components()
                .map(|component| component.as_os_str().to_str().unwrap())
                .collect::<Vec<_>>()
                .join("/");
            (name, path)
        })
        .collect();
    files.sort_by(|a, b| a.0.cmp(&b.0));

    let mut hash = Sha256::new();
    for (name, path) in files {
        let content = fs::read(&path).unwrap();
        let content = if let Ok(text) = std::str::from_utf8(&content) {
            text.replace("\r\n", "\n").into_bytes()
        } else {
            content
        };
        hash.update((name.len() as u64).to_be_bytes());
        hash.update(name.as_bytes());
        hash.update((content.len() as u64).to_be_bytes());
        hash.update(content);
    }
    let id: String = hash
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    println!("cargo:rustc-env=ZESS_BUILD_ID={id}");
}

fn collect(path: &Path, files: &mut Vec<std::path::PathBuf>) {
    if path.is_dir() {
        for entry in fs::read_dir(path).unwrap() {
            let path = entry.unwrap().path();
            if path.file_name().unwrap() != ".git" {
                collect(&path, files);
            }
        }
    } else {
        files.push(path.to_owned());
    }
}
