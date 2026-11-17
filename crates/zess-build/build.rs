use std::{collections::HashSet, env, fs, path::Path, process::Command};

use sha2::{Digest, Sha256};

// Runtime-embedded resources and the scripts that build and package the server.
const RESOURCES: &[&str] = &[
    "crates/zess-text/languages.toml",
    "vendor/helix-loader/languages.toml",
    "plugins/defaults.w",
    "plugins/outline.w",
    "plugins/zess/buffer.w",
    "plugins/zess/config.w",
    "plugins/zess/keys.w",
    "plugins/zess/state.w",
    "plugins/zess/status-layout.w",
    "plugins/zess/status.w",
    "plugins/zess/ui.w",
    "vendor/tree-sitter-wisp/LICENSE",
    "vendor/tree-sitter-wisp/src/parser.c",
    "vendor/tree-sitter-wisp/src/scanner.c",
    "vendor/tree-sitter-wisp/src/tree_sitter/alloc.h",
    "vendor/tree-sitter-wisp/src/tree_sitter/array.h",
    "vendor/tree-sitter-wisp/src/tree_sitter/parser.h",
    "vendor/steel-core/src/scheme/stdlib.scm",
    "vendor/steel-core/src/scheme/kernel.scm",
    "vendor/steel-core/src/scheme/print.scm",
    "vendor/steel-core/src/scheme/modules/option.scm",
    "vendor/steel-core/src/scheme/modules/result.scm",
    "vendor/steel-core/src/scheme/modules/iterators.scm",
    "vendor/steel-core/src/scheme/modules/async.scm",
    "vendor/steel-core/src/scheme/modules/sync.scm",
    "vendor/steel-core/src/scheme/modules/contracts.scm",
    "vendor/steel-core/src/scheme/modules/ports.scm",
    "vendor/steel-core/src/scheme/modules/parameters.scm",
    "vendor/steel-core/src/scheme/modules/reader.scm",
    "vendor/steel-core/src/scheme/modules/match.scm",
    "vendor/steel-core/src/scheme/modules/sort.scm",
    "vendor/steel-core/src/scheme/modules/serde.scm",
    "scripts/server-build.sh",
    "scripts/server-release.py",
];

struct PackageInputs {
    directory: String,
    build_script: Option<String>,
}

fn production_input(
    path: &str,
    packages: &[PackageInputs],
    steel_features: &HashSet<String>,
) -> bool {
    if matches!(path, "Cargo.toml" | "Cargo.lock")
        || RESOURCES.contains(&path)
        || path.starts_with("crates/zess-text/queries/")
    {
        return true;
    }
    if path
        .split('/')
        .any(|part| matches!(part, "tests" | "benches" | "examples"))
        || path.starts_with("src/bin/")
        || path.contains("/src/bin/")
        || path.ends_with("/tests.rs")
        || path.ends_with("_tests.rs")
        || matches!(
            path,
            "crates/zess-plugins/src/builtin/files/file_yank.rs"
                | "vendor/steel-core/src/parser/prop/mod.rs"
                | "vendor/steel-core/src/steel_vm/test_util.rs"
                | "vendor/helix-loader/src/main.rs"
        )
    {
        return false;
    }
    let required_feature = if path.starts_with("vendor/steel-core/src/jit2/")
        || path == "vendor/steel-core/src/steel_vm/vm/jit.rs"
    {
        Some("jit2")
    } else if matches!(
        path,
        "vendor/steel-core/src/steel_vm/dylib.rs" | "vendor/steel-core/src/steel_vm/ffi.rs"
    ) {
        Some("dylibs")
    } else if path == "vendor/steel-core/src/steel_vm/profiling.rs" {
        Some("op-code-profiling")
    } else {
        None
    };
    if required_feature.is_some_and(|feature| !steel_features.contains(feature)) {
        return false;
    }
    packages.iter().any(|package| {
        let prefix = if package.directory.is_empty() {
            String::new()
        } else {
            format!("{}/", package.directory)
        };
        path == format!("{prefix}Cargo.toml")
            || package.build_script.as_deref() == Some(path)
            || (path.ends_with(".rs") && path.starts_with(&format!("{prefix}src/")))
    })
}

fn production_packages(root: &Path) -> (Vec<PackageInputs>, HashSet<String>) {
    let read_manifest = |path: &str| -> toml::Value {
        toml::from_str(&fs::read_to_string(root.join(path)).unwrap()).unwrap()
    };
    let workspace = read_manifest("Cargo.toml");
    let steel = read_manifest("vendor/steel-core/Cargo.toml");
    let workspace_dependencies = workspace["workspace"]["dependencies"].as_table().unwrap();
    let mut directories = vec![root.to_path_buf()];
    let mut visited = HashSet::new();
    let mut packages = Vec::new();
    let mut pending = Vec::new();
    while let Some(directory) = directories.pop() {
        let directory = fs::canonicalize(directory).unwrap();
        if !visited.insert(directory.clone()) {
            continue;
        }
        let relative = directory
            .strip_prefix(root)
            .unwrap()
            .components()
            .map(|part| part.as_os_str().to_str().unwrap())
            .collect::<Vec<_>>()
            .join("/");
        let manifest: toml::Value =
            toml::from_str(&fs::read_to_string(directory.join("Cargo.toml")).unwrap()).unwrap();
        let build = manifest["package"].get("build");
        let build_script = if build.and_then(toml::Value::as_bool) == Some(false) {
            None
        } else {
            let script = build.and_then(toml::Value::as_str).unwrap_or("build.rs");
            let path = directory.join(script);
            path.is_file().then(|| {
                path.strip_prefix(root)
                    .unwrap()
                    .components()
                    .map(|part| part.as_os_str().to_str().unwrap())
                    .collect::<Vec<_>>()
                    .join("/")
            })
        };
        packages.push(PackageInputs {
            directory: relative,
            build_script,
        });
        let mut scopes = vec![&manifest];
        if let Some(targets) = manifest.get("target").and_then(toml::Value::as_table) {
            scopes.extend(targets.values());
        }
        for scope in scopes {
            for kind in ["dependencies", "build-dependencies"] {
                let Some(dependencies) = scope.get(kind).and_then(toml::Value::as_table) else {
                    continue;
                };
                for (name, dependency) in dependencies {
                    let inherited =
                        dependency.get("workspace").and_then(toml::Value::as_bool) == Some(true);
                    let declared = if inherited {
                        &workspace_dependencies[name]
                    } else {
                        dependency
                    };
                    let Some(path) = declared.get("path").and_then(toml::Value::as_str) else {
                        continue;
                    };
                    let base = if inherited { root } else { &directory };
                    let path = fs::canonicalize(base.join(path)).unwrap();
                    if path == root.join("vendor/steel-core") {
                        for item in [declared, dependency] {
                            if let Some(features) = item.get("features") {
                                pending.extend(
                                    features
                                        .as_array()
                                        .unwrap()
                                        .iter()
                                        .map(|value| value.as_str().unwrap().to_owned()),
                                );
                            }
                        }
                        if declared
                            .get("default-features")
                            .and_then(toml::Value::as_bool)
                            .unwrap_or(true)
                            || (inherited
                                && dependency
                                    .get("default-features")
                                    .and_then(toml::Value::as_bool)
                                    .unwrap_or(false))
                        {
                            pending.push("default".to_owned());
                        }
                    }
                    directories.push(path);
                }
            }
        }
    }
    let available = steel["features"].as_table().unwrap();
    let mut active = HashSet::new();
    while let Some(feature) = pending.pop() {
        if active.insert(feature.clone())
            && let Some(aliases) = available.get(&feature).and_then(toml::Value::as_array)
        {
            pending.extend(aliases.iter().filter_map(|value| {
                let value = value.as_str().unwrap();
                available.contains_key(value).then(|| value.to_owned())
            }));
        }
    }
    (packages, active)
}

fn main() {
    let manifest = env::var_os("CARGO_MANIFEST_DIR").unwrap();
    let git_root = Path::new(&manifest).parent().unwrap().parent().unwrap();
    let root = fs::canonicalize(git_root).unwrap();
    let root = root.as_path();
    let git = |args: &[&str]| {
        let output = Command::new("git")
            .arg("-C")
            .arg(git_root)
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
    let (packages, features) = production_packages(root);
    let output = git(&["ls-files", "--cached", "--recurse-submodules", "-z"]);
    let mut files: Vec<_> = output
        .split(|byte| *byte == 0)
        .filter(|name| !name.is_empty())
        .map(|name| std::str::from_utf8(name).unwrap())
        .filter(|name| root.join(name).is_file())
        .filter(|name| production_input(name, &packages, &features))
        .collect();
    files.sort_unstable();
    files.dedup();

    let mut hash = Sha256::new();
    for name in files {
        println!("cargo:rerun-if-changed={}", root.join(name).display());
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
