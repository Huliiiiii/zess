use std::{path::Path, process::Command};

fn git(workspace: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(workspace)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let mut output = String::from_utf8(output.stdout).ok()?;
    output.truncate(output.trim_end_matches(['\n', '\r']).len());
    Some(output)
}

fn git_commit() -> Option<String> {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let git_entry = workspace.join(".git");
    if !git_entry.exists() {
        return None;
    }
    if git_entry.is_file() {
        println!("cargo::rerun-if-changed={}", git_entry.display());
    }

    let paths = git(
        &workspace,
        &[
            "rev-parse",
            "--path-format=absolute",
            "--git-common-dir",
            "--git-path",
            "HEAD",
        ],
    )?;
    let mut paths = paths.lines().map(Path::new);
    let common_dir = paths.next()?;
    let head = paths.next()?;
    println!("cargo::rerun-if-changed={}", head.display());

    let reftable = common_dir.join("reftable/tables.list");
    if reftable.is_file() {
        println!("cargo::rerun-if-changed={}", reftable.display());
        let worktree_reftable = head.with_file_name("reftable").join("tables.list");
        if worktree_reftable != reftable && worktree_reftable.is_file() {
            println!("cargo::rerun-if-changed={}", worktree_reftable.display());
        }
    } else if let Some(reference) = git(&workspace, &["symbolic-ref", "--quiet", "HEAD"])
        && let Some(reference) = git(
            &workspace,
            &[
                "rev-parse",
                "--path-format=absolute",
                "--git-path",
                &reference,
            ],
        )
    {
        let reference = Path::new(&reference);
        if reference.is_file() {
            println!("cargo::rerun-if-changed={}", reference.display());
        } else {
            let packed_refs = common_dir.join("packed-refs");
            if packed_refs.is_file() {
                println!("cargo::rerun-if-changed={}", packed_refs.display());
            }
            // Updating a packed branch can create a loose ref without changing packed-refs.
            if let Some(parent) = reference.ancestors().skip(1).find(|path| path.is_dir()) {
                println!("cargo::rerun-if-changed={}", parent.display());
            }
        }
    }

    git(&workspace, &["rev-parse", "--verify", "HEAD^{commit}"])
}

fn main() {
    println!("cargo::rerun-if-env-changed=ZESS_RELEASE_VERSION");
    println!("cargo::rerun-if-changed=build.rs");
    let version = if let Ok(version) = std::env::var("ZESS_RELEASE_VERSION") {
        let base = env!("CARGO_PKG_VERSION");
        let preview = version
            .strip_prefix(base)
            .and_then(|suffix| suffix.strip_prefix("-dev."));
        assert!(
            version == base
                || preview.is_some_and(|suffix| {
                    suffix.split('.').all(|part| {
                        !part.is_empty()
                            && (part.len() == 1 || !part.starts_with('0'))
                            && part.bytes().all(|byte| byte.is_ascii_digit())
                    })
                }),
            "ZESS_RELEASE_VERSION must be {base} or a numbered {base}-dev prerelease"
        );
        version
    } else if let Some(commit) = git_commit() {
        format!("dev-{commit}")
    } else {
        String::new()
    };
    println!("cargo::rustc-env=ZESS_BUILD_VERSION={version}");
}
