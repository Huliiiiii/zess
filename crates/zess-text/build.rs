use std::{env, fs, path::Path};

fn main() {
    let manifest = env::var_os("CARGO_MANIFEST_DIR").unwrap();
    let manifest = Path::new(&manifest);
    let mut queries = Vec::new();
    collect(&manifest.join("queries"), &mut queries);
    queries.sort();

    let mut resources = String::from("&[\n");
    for path in queries {
        let relative = path.strip_prefix(manifest).unwrap().to_str().unwrap();
        resources.push_str(&format!(
            "({relative:?}, include_bytes!(concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/\", {relative:?}))),\n"
        ));
    }
    resources.push_str("]\n");
    fs::write(
        Path::new(&env::var_os("OUT_DIR").unwrap()).join("queries.rs"),
        resources,
    )
    .unwrap();
    println!("cargo:rerun-if-changed=queries");
}

fn collect(directory: &Path, files: &mut Vec<std::path::PathBuf>) {
    for entry in fs::read_dir(directory).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        if entry.file_type().unwrap().is_dir() {
            collect(&path, files);
        } else {
            files.push(path);
        }
    }
}
