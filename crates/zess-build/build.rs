use std::{env, fs, path::Path, process::Command};

use sha2::{Digest, Sha256};

fn main() {
    let manifest = env::var_os("CARGO_MANIFEST_DIR").unwrap();
    let root = Path::new(&manifest).parent().unwrap().parent().unwrap();
    let inputs = [
        "Cargo.toml",
        "Cargo.lock",
        "src",
        "crates",
        "plugins",
        "vendor",
        ".cargo",
        ".github",
        "scripts",
    ];
    for path in inputs {
        let path = root.join(path);
        println!("cargo:rerun-if-changed={}", path.display());
    }
    let git = |args: &[&str]| {
        let output = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .output()
            .expect("Git is required to calculate the build ID");
        assert!(
            output.status.success(),
            "Git could not list build inputs: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        output.stdout
    };
    let submodules = git(&["submodule", "status", "--recursive"]);
    assert!(
        !submodules
            .split(|byte| *byte == b'\n')
            .any(|line| line.starts_with(b"-")),
        "initialize Git submodules with `git submodule update --init --recursive` before building"
    );
    let index = git(&["rev-parse", "--git-path", "index"]);
    let index = std::str::from_utf8(&index).unwrap().trim_end();
    println!("cargo:rerun-if-changed={}", root.join(index).display());
    let indexes = git(&[
        "submodule",
        "foreach",
        "--quiet",
        "--recursive",
        "git rev-parse --path-format=absolute --git-path index",
    ]);
    for index in std::str::from_utf8(&indexes).unwrap().lines() {
        println!("cargo:rerun-if-changed={}", index.trim_end());
    }
    let mut args = vec!["ls-files", "--cached", "--recurse-submodules", "-z", "--"];
    args.extend(inputs);
    let output = git(&args);
    let mut files: Vec<_> = output
        .split(|byte| *byte == 0)
        .filter(|name| !name.is_empty())
        .map(|name| std::str::from_utf8(name).unwrap())
        .filter(|name| root.join(name).is_file())
        .collect();
    files.sort_unstable();
    files.dedup();

    let mut hash = Sha256::new();
    for name in files {
        let content = fs::read(root.join(name)).unwrap();
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
