use std::{
    collections::HashSet,
    path::{Component, Path, PathBuf},
};

use anyhow::{Context, Result, bail, ensure};

use crate::{Execution, Repository, SelectedPath, validate_relative};

#[cfg(test)]
mod tests;

#[derive(Clone, Copy)]
pub enum IgnoreDestination {
    Repository,
    Local,
}

pub struct IgnoreRequest {
    pub destination: IgnoreDestination,
    pub paths: Vec<SelectedPath>,
}

#[derive(Default)]
pub struct IgnoreFiles {
    pub worktree: Option<PathBuf>,
    pub global: Option<PathBuf>,
    pub local: Option<PathBuf>,
    pub configuration: Vec<PathBuf>,
}

pub(super) fn ignore_files(
    directory: &Path,
    file_system: &dyn zess_platform::FileSystem,
) -> IgnoreFiles {
    use gix::config::source::Kind;
    use gix::path::env;

    const EXCLUDES_FILE: &str = "core.excludesFile";

    let mut config = [Kind::GitInstallation, Kind::System, Kind::Global]
        .into_iter()
        .flat_map(Kind::sources)
        .filter_map(|source| source.storage_location(&mut env::var))
        .filter(|path| !path.as_os_str().is_empty())
        .collect::<Vec<_>>();
    let mut worktree = None;
    let mut local = None;
    let configured = if let Ok((repository, Some(root))) =
        super::queries::gix_repository(directory, file_system)
    {
        worktree = file_system.canonicalize(&root).ok();
        let common = repository.common_dir();
        local = Some(common.join("info/exclude"));
        config.push(common.join("config"));
        config.push(repository.git_dir().join("config.worktree"));
        config.push(repository.git_dir().join("commondir"));
        let snapshot = repository.config_snapshot();
        config.extend(
            snapshot
                .plumbing()
                .sections()
                .filter_map(|section| section.meta().path.clone()),
        );
        snapshot.trusted_path(EXCLUDES_FILE).ok().flatten()
    } else if let Ok(mut global) = gix::config::File::from_globals() {
        if let Ok(overrides) = gix::config::File::from_environment_overrides() {
            let _ = global.append(overrides);
        }
        config.extend(
            global
                .sections()
                .filter_map(|section| section.meta().path.clone()),
        );
        global.path(EXCLUDES_FILE).and_then(|path| {
            path.interpolate(gix::config::path::interpolate::Context {
                git_install_dir: env::installation_config_prefix(),
                home_dir: env::home_dir().as_deref(),
                ..Default::default()
            })
            .ok()
        })
    } else {
        None
    };
    let config_base = std::env::current_dir().unwrap_or_else(|_| directory.to_owned());
    for path in &mut config {
        if path.is_relative() {
            *path = config_base.join(&*path);
        }
    }
    config.sort_unstable();
    config.dedup();
    let global = configured
        .or_else(|| env::xdg_config("ignore", &mut env::var))
        .map(|path| worktree.as_ref().unwrap_or(&config_base).join(path));
    IgnoreFiles {
        worktree,
        global,
        local,
        configuration: config,
    }
}

impl Repository {
    pub fn ignore_paths(&self, request: IgnoreRequest, execution: &impl Execution) -> Result<()> {
        let git = self
            .queries
            .discover(&self.root)?
            .context("discover Git repository")?;
        let root = self.file_system.canonicalize(&git.worktree)?;
        let prefix = self
            .root
            .strip_prefix(&root)
            .context("workspace is outside Git working tree")?;
        let destination = match request.destination {
            IgnoreDestination::Repository => root.join(".gitignore"),
            IgnoreDestination::Local => git.common_dir.join("info/exclude"),
        };
        self.write_ignore_paths(&destination, prefix, request.paths, execution)
            .with_context(|| format!("update ignore file {}", destination.display()))
    }

    fn write_ignore_paths(
        &self,
        destination: &Path,
        prefix: &Path,
        paths: Vec<SelectedPath>,
        execution: &impl Execution,
    ) -> Result<()> {
        ensure!(!paths.is_empty(), "No paths selected to ignore");
        let mut patterns = Vec::new();
        for selected in &paths {
            let pattern = self
                .ignore_pattern(selected, prefix, execution)
                .with_context(|| format!("cannot ignore path {:?}", selected.path))?;
            patterns.push(pattern);
        }
        execution.check()?;
        if let Ok(metadata) = self.file_system.symlink_metadata(destination) {
            ensure!(
                !metadata.file_type().is_symlink(),
                "ignore file is a symbolic link"
            );
        }
        let content = match self.file_system.read(destination) {
            Ok(content) => content,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(error) => return Err(error.into()),
        };
        let existing: HashSet<&[u8]> = content
            .split(|byte| *byte == b'\n')
            .map(|line| line.strip_suffix(b"\r").unwrap_or(line))
            .collect();
        let mut added = HashSet::new();
        let mut append = Vec::new();
        let newline = if content.windows(2).any(|pair| pair == b"\r\n") {
            b"\r\n".as_slice()
        } else {
            b"\n".as_slice()
        };
        for (selected, pattern) in paths.iter().zip(&patterns) {
            if !added.insert(pattern) {
                continue;
            }
            if existing.contains(pattern.as_bytes())
                && self.is_ignored(&selected.path, execution)?
            {
                continue;
            }
            if append.is_empty() && !content.is_empty() && !content.ends_with(b"\n") {
                append.extend_from_slice(newline);
            }
            append.extend_from_slice(pattern.as_bytes());
            append.extend_from_slice(newline);
        }
        if !append.is_empty() {
            self.file_system.create_dir_all(
                destination
                    .parent()
                    .context("ignore file has no parent directory")?,
            )?;
            self.file_system.append(destination, &append)?;
        }
        for selected in &paths {
            ensure!(
                self.is_ignored(&selected.path, execution)?,
                "Git still does not ignore path {:?}; a higher-precedence rule may override it (the ignore file may have been updated)",
                selected.path
            );
        }
        Ok(())
    }

    fn ignore_pattern(
        &self,
        selected: &SelectedPath,
        prefix: &Path,
        execution: &impl Execution,
    ) -> Result<String> {
        validate_relative(&selected.path)?;
        ensure!(!selected.path.is_empty(), "path must not be empty");
        ensure!(
            !selected.path.contains(['\n', '\r', '\0']),
            "path contains a character that cannot be represented in an ignore file"
        );
        let path = Path::new(&selected.path);
        let parent = path.parent().context("path must have a parent")?;
        self.path(parent.to_str().context("path is not UTF-8")?)?;
        let mut relative = prefix.to_path_buf();
        for component in path.components() {
            if let Component::Normal(part) = component {
                relative.push(part);
            }
        }
        ensure!(relative != prefix, "path must name a file or directory");
        let tracked = self.git_checked(
            &[
                "--literal-pathspecs",
                "ls-files",
                "--cached",
                "-z",
                "--",
                &selected.path,
            ],
            execution,
        )?;
        ensure!(
            tracked.is_empty(),
            "path contains tracked files; remove them from the index first"
        );
        let relative = relative.to_str().context("repository path is not UTF-8")?;
        let relative = zess_platform::slash_separated_path(relative);
        ensure!(
            !relative.contains(['\n', '\r', '\0']),
            "repository path contains a character that cannot be represented in an ignore file"
        );
        let mut pattern = String::from("/");
        for ch in relative.chars() {
            if matches!(ch, '\\' | '*' | '?' | '[' | ']' | ' ' | '#' | '!') {
                pattern.push('\\');
            }
            pattern.push(ch);
        }
        if selected.directory {
            pattern.push('/');
        }
        Ok(pattern)
    }

    fn is_ignored(&self, path: &str, execution: &impl Execution) -> Result<bool> {
        let output = self
            .git(
                &["check-ignore", "--quiet", "--no-index", "--", path],
                execution,
            )
            .with_context(|| format!("check ignore status for path {path:?}"))?;
        match output.status.code() {
            Some(0) => Ok(true),
            Some(1) => Ok(false),
            _ => bail!(
                "check ignore status for path {path:?}: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        }
    }
}
