use std::{
    collections::HashSet,
    ffi::{OsStr, OsString},
    path::{Component, Path, PathBuf},
    process::Output,
    sync::Arc,
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use zess_platform::{FileSystem, NativeFileSystem, ProcessSpec};

pub struct SelectedPath {
    pub path: String,
    pub directory: bool,
}

pub mod ignore;
pub mod mutation;
pub mod patch;
mod renderer;
pub use renderer::{DiffRenderOptions, DiffRenderer, PagerCommand};
mod queries;
mod stats;
pub use queries::{GitQueries, NativeGitQueries, RepositoryLocation};
#[cfg(test)]
mod tests;

pub trait Execution {
    /// Returns inherited values from this execution context. Implementations that
    /// inherit an external environment must expose it here; the default exposes none.
    fn environment(&self, _name: &OsStr) -> Option<OsString> {
        None
    }
    fn check(&self) -> Result<()>;
    fn output(&self, command: &ProcessSpec) -> Result<Output>;
    fn output_with_input(&self, command: &ProcessSpec, input: &[u8]) -> Result<Output>;
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiffSide {
    Staged,
    Unstaged,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub struct DiffTarget {
    pub path: String,
    pub original_path: Option<String>,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DiffSource {
    Changes { side: DiffSide },
    Commit { commit: String },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FileDiff {
    pub target: DiffTarget,
    pub raw: String,
    #[serde(default)]
    pub rendered: Option<RenderedDiff>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RenderedDiff {
    #[serde(default)]
    pub file_header: bool,
    pub text: String,
    pub repository_root: PathBuf,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GitDiff {
    pub source: DiffSource,
    pub files: Vec<FileDiff>,
}

pub struct Blob {
    pub repository: String,
    pub commit: String,
    pub path: String,
    pub oid: String,
    pub bytes: Arc<Vec<u8>>,
}

pub enum BlobRead {
    File(Blob),
    Missing,
    UnsupportedObject,
    TooLarge,
}

pub struct DiffColors {
    pub added: Option<Vec<u8>>,
    pub deleted: Option<Vec<u8>>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GitEntry {
    pub path: String,
    pub original_path: Option<String>,
    pub index: String,
    pub worktree: String,
    pub untracked: bool,
    pub line_stats: Option<LineStats>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct LineStats {
    pub additions: u64,
    pub deletions: u64,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileStatus {
    Added,
    Modified,
    Deleted,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StagingStatus {
    Staged,
    Unstaged,
    Both,
    Untracked,
    Conflict,
    Unmodified,
    Ignored,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GitDecoration {
    pub path: String,
    pub original_path: Option<String>,
    pub status: FileStatus,
    pub staging_status: StagingStatus,
}

impl GitEntry {
    pub fn staging_status(&self) -> StagingStatus {
        match (self.index.as_str(), self.worktree.as_str()) {
            ("D", "D")
            | ("A", "U")
            | ("U", "D")
            | ("U", "A")
            | ("D", "U")
            | ("A", "A")
            | ("U", "U") => StagingStatus::Conflict,
            ("?", "?") => StagingStatus::Untracked,
            ("!", "!") => StagingStatus::Ignored,
            (" ", " ") => StagingStatus::Unmodified,
            (" ", _) => StagingStatus::Unstaged,
            (_, " ") => StagingStatus::Staged,
            _ => StagingStatus::Both,
        }
    }

    pub fn status(&self) -> FileStatus {
        if self.index == "D" || self.worktree == "D" {
            FileStatus::Deleted
        } else if self.untracked || self.index == "A" || self.worktree == "A" {
            FileStatus::Added
        } else {
            FileStatus::Modified
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Commit {
    pub oid: String,
    pub author: String,
    pub date: String,
    pub message: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CommitFile {
    pub path: String,
    pub status: String,
    pub original_path: Option<String>,
    pub additions: Option<u64>,
    pub deletions: Option<u64>,
}

impl CommitFile {
    pub fn line_stats(&self) -> Option<LineStats> {
        Some(LineStats {
            additions: self.additions?,
            deletions: self.deletions?,
        })
    }
}

pub struct PatchRequest {
    pub path: String,
    pub original_path: Option<String>,
    pub side: DiffSide,
    pub raw: String,
    pub rows: Vec<std::ops::RangeInclusive<patch::PatchRow>>,
}

pub struct Repository {
    root: PathBuf,
    file_system: Arc<dyn FileSystem>,
    queries: Arc<dyn GitQueries>,
}

impl Repository {
    pub fn diff_files(
        &self,
        source: &DiffSource,
        files: &[DiffTarget],
        options: &DiffRenderOptions,
        cancellation: &impl Execution,
    ) -> Result<GitDiff> {
        let renderer = self.diff_renderer(options, cancellation)?;
        let mut diffs = Vec::with_capacity(files.len());
        for target in files {
            cancellation.check()?;
            let raw = match source {
                DiffSource::Changes { side } => self.diff(
                    &target.path,
                    target.original_path.as_deref(),
                    Some(*side),
                    cancellation,
                )?,
                DiffSource::Commit { commit } => self.commit_diff(
                    commit,
                    &target.path,
                    target.original_path.as_deref(),
                    cancellation,
                )?,
            };
            let rendered = renderer.render(self, source, target, &raw, cancellation)?;
            diffs.push(FileDiff {
                target: target.clone(),
                raw,
                rendered,
            });
        }
        cancellation.check()?;
        Ok(GitDiff {
            source: source.clone(),
            files: diffs,
        })
    }

    pub fn open(root: impl AsRef<Path>) -> Result<Self> {
        Self::with_file_system(root, Arc::new(NativeFileSystem))
    }

    pub fn with_file_system(
        root: impl AsRef<Path>,
        file_system: Arc<dyn FileSystem>,
    ) -> Result<Self> {
        let queries = Arc::new(NativeGitQueries::new(file_system.clone()));
        Self::with_backends(root, file_system, queries)
    }

    pub fn with_backends(
        root: impl AsRef<Path>,
        file_system: Arc<dyn FileSystem>,
        queries: Arc<dyn GitQueries>,
    ) -> Result<Self> {
        let root = file_system
            .canonicalize(root.as_ref())
            .context("cannot open Git workspace")?;
        anyhow::ensure!(
            file_system.metadata(&root)?.is_dir(),
            "Git workspace must be a directory"
        );
        Ok(Self {
            root,
            file_system,
            queries,
        })
    }

    pub fn log(&self, cancellation: &impl Execution) -> Result<Vec<Commit>> {
        let head = self.git(&["rev-parse", "--verify", "HEAD"], cancellation)?;
        if !head.status.success() {
            // A repository with an unborn HEAD has no commits.
            self.git_checked(&["rev-parse", "--git-dir"], cancellation)?;
            return Ok(Vec::new());
        }
        let output = self.git_checked(
            &["log", "--format=%H%x00%an%x00%aI%x00%B", "-z"],
            cancellation,
        )?;
        let mut fields = output.split(|byte| *byte == 0);
        let mut commits = Vec::new();
        while let Some(oid) = fields.next().filter(|field| !field.is_empty()) {
            let mut next = || -> Result<String> {
                Ok(
                    std::str::from_utf8(fields.next().context("incomplete commit record")?)?
                        .to_owned(),
                )
            };
            commits.push(Commit {
                oid: std::str::from_utf8(oid)?.to_owned(),
                author: next()?,
                date: next()?,
                message: next()?,
            });
        }
        Ok(commits)
    }

    pub fn commit_files(
        &self,
        commit: &str,
        cancellation: &impl Execution,
    ) -> Result<Vec<CommitFile>> {
        let oid = commit_oid(commit)?;
        let parent = self.commit_parent(oid, cancellation)?;
        let output = self.commit_diff_output(
            oid,
            parent.as_deref(),
            "--name-status",
            &["."],
            cancellation,
        )?;
        let mut counts = self.queries.commit_counts(&self.root, oid, cancellation)?;
        let mut fields = output
            .split(|byte| *byte == 0)
            .filter(|field| !field.is_empty());
        let prefix = self.git_checked(&["rev-parse", "--show-prefix"], cancellation)?;
        let prefix = std::str::from_utf8(&prefix)?.trim_end_matches('\n');
        let mut files = Vec::new();
        while let Some(status) = fields.next() {
            cancellation.check()?;
            let status = std::str::from_utf8(status)?.to_owned();
            let first = std::str::from_utf8(fields.next().context("missing commit path")?)?;
            let (path, original) = if status.starts_with(['R', 'C']) {
                (
                    std::str::from_utf8(fields.next().context("missing rename destination")?)?,
                    Some(first),
                )
            } else {
                (first, None)
            };
            let line_stats = counts.remove(path).context("missing commit line counts")?;
            let Some(path) = path.strip_prefix(prefix) else {
                continue;
            };
            files.push(CommitFile {
                path: path.into(),
                status,
                original_path: original
                    .and_then(|path| path.strip_prefix(prefix))
                    .map(str::to_owned),
                additions: line_stats.map(|stats| stats.additions),
                deletions: line_stats.map(|stats| stats.deletions),
            });
        }
        Ok(files)
    }

    pub fn commit_diff(
        &self,
        commit: &str,
        path: &str,
        original_path: Option<&str>,
        cancellation: &impl Execution,
    ) -> Result<String> {
        let oid = commit_oid(commit)?;
        validate_relative(path)?;
        let mut paths = vec![path];
        if let Some(original) = original_path {
            validate_relative(original)?;
            paths.push(original);
        }
        let parent = self.commit_parent(oid, cancellation)?;
        let output =
            self.commit_diff_output(oid, parent.as_deref(), "--patch", &paths, cancellation)?;
        Ok(String::from_utf8_lossy(&output).into_owned())
    }

    pub fn blob(
        &self,
        commit: &str,
        path: &str,
        cancellation: &impl Execution,
    ) -> Result<BlobRead> {
        let oid = commit_oid(commit)?;
        validate_relative(path)?;
        cancellation.check()?;
        self.queries.blob(&self.root, oid, path, cancellation)
    }

    fn git_checked(&self, args: &[&str], cancellation: &impl Execution) -> Result<Vec<u8>> {
        let output = self.git(args, cancellation)?;
        if !output.status.success() {
            bail!(
                "git {}: {}",
                args.join(" "),
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(output.stdout)
    }

    fn commit_parent(&self, oid: &str, cancellation: &impl Execution) -> Result<Option<String>> {
        self.queries.commit_parent(&self.root, oid, cancellation)
    }

    fn commit_diff_output(
        &self,
        oid: &str,
        parent: Option<&str>,
        format: &str,
        paths: &[&str],
        cancellation: &impl Execution,
    ) -> Result<Vec<u8>> {
        let mut args = vec!["--literal-pathspecs"];
        if let Some(parent) = parent {
            args.extend(["diff", parent, oid]);
        } else {
            args.extend(["diff-tree", "--root", "--no-commit-id", "-r", oid]);
        }
        args.extend([
            "--no-ext-diff",
            "--no-textconv",
            "--no-color",
            "--no-relative",
            "-M",
            "-z",
            format,
            "--",
        ]);
        args.extend_from_slice(paths);
        self.git_checked(&args, cancellation)
    }

    fn path(&self, relative: &str) -> Result<PathBuf> {
        let relative = Path::new(relative);

        if relative
            .components()
            .any(|part| !matches!(part, Component::Normal(_) | Component::CurDir))
        {
            bail!("path must remain relative to the workspace");
        }

        let path = self.root.join(relative);
        let mut ancestor = path.as_path();

        while !self.file_system.try_exists(ancestor)? {
            if self.file_system.symlink_metadata(ancestor).is_ok() {
                bail!("broken symbolic link in workspace path");
            }

            ancestor = ancestor.parent().context("invalid workspace path")?;
        }

        if !self
            .file_system
            .canonicalize(ancestor)?
            .starts_with(&self.root)
        {
            bail!("symbolic link leaves the workspace");
        }

        Ok(path)
    }

    fn git(&self, args: &[&str], cancellation: &impl Execution) -> Result<std::process::Output> {
        let mut command = self.git_command();
        command.args(args);
        cancellation
            .output(&command)
            .with_context(|| format!("run git {}", args.join(" ")))
    }

    fn git_command(&self) -> ProcessSpec {
        let mut command = ProcessSpec::new("git");
        command
            .arg("--no-pager")
            .args([
                "-c",
                "core.fsmonitor=false",
                "-c",
                "core.untrackedCache=false",
                "-c",
                "diff.suppressBlankEmpty=false",
            ])
            .current_dir(&self.root)
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_OPTIONAL_LOCKS", "0");
        command
    }

    fn git_worktree(
        &self,
        args: &[&str],
        cancellation: &impl Execution,
    ) -> Result<std::process::Output> {
        let mut command = self.git_worktree_command(cancellation)?;
        command.args(args);
        cancellation
            .output(&command)
            .with_context(|| format!("run git {}", args.join(" ")))
    }

    fn git_worktree_command(&self, cancellation: &impl Execution) -> Result<ProcessSpec> {
        let mut filters = ProcessSpec::new("git");
        filters
            .args([
                "config",
                "--null",
                "--name-only",
                "--get-regexp",
                r"^filter\..*\.(clean|smudge|process|required)$",
            ])
            .current_dir(&self.root);
        let filters = cancellation
            .output(&filters)
            .context("cannot read git filter configuration")?;

        if !filters.status.success() && filters.status.code() != Some(1) {
            bail!(
                "cannot read git filter configuration: {}",
                String::from_utf8_lossy(&filters.stderr).trim()
            );
        }

        let mut drivers = HashSet::new();

        for key in filters
            .stdout
            .split(|byte| *byte == 0)
            .filter(|key| !key.is_empty())
        {
            let key = std::str::from_utf8(key)?;
            let (driver, _) = key.rsplit_once('.').context("invalid git filter key")?;
            drivers.insert(driver);
        }

        let mut command = self.git_command();

        // Diff's --no-textconv does not disable working-tree clean filters.
        for driver in drivers {
            for operation in ["clean", "smudge", "process"] {
                command.arg("-c").arg(format!("{driver}.{operation}="));
            }

            command.arg("-c").arg(format!("{driver}.required=false"));
        }

        Ok(command)
    }

    pub fn status(&self, cancellation: &impl Execution) -> Result<Vec<GitEntry>> {
        self.status_with_progress(cancellation, |_| {})
    }

    pub fn status_with_progress(
        &self,
        cancellation: &impl Execution,
        progress: impl FnOnce(&[GitEntry]),
    ) -> Result<Vec<GitEntry>> {
        let top = self
            .git(&["rev-parse", "--show-toplevel"], cancellation)
            .context("locate Git working tree")?;

        if !top.status.success() {
            bail!(
                "locate Git working tree: {}",
                String::from_utf8_lossy(&top.stderr).trim()
            );
        }

        let top = PathBuf::from(
            String::from_utf8(top.stdout)?
                .strip_suffix('\n')
                .context("invalid git root output")?,
        );
        let mut entries = self.status_entries(&top, cancellation)?;
        cancellation.check()?;
        progress(&entries);
        let mut counts = self
            .queries
            .worktree_counts(&self.root, cancellation)
            .context("count changed lines in working tree")?;
        for entry in &mut entries {
            cancellation.check()?;
            entry.line_stats = counts.remove(&entry.path).unwrap_or(if entry.untracked {
                None
            } else {
                Some(LineStats {
                    additions: 0,
                    deletions: 0,
                })
            });
        }
        Ok(entries)
    }

    pub fn decorations(&self, cancellation: &impl Execution) -> Result<Vec<GitDecoration>> {
        cancellation.check()?;
        let Some(location) = self.queries.discover(&self.root)? else {
            return Ok(Vec::new());
        };
        Ok(self
            .status_entries(&location.worktree, cancellation)?
            .into_iter()
            .map(|entry| GitDecoration {
                status: entry.status(),
                staging_status: entry.staging_status(),
                path: entry.path,
                original_path: entry.original_path,
            })
            .collect())
    }

    fn status_entries(&self, top: &Path, cancellation: &impl Execution) -> Result<Vec<GitEntry>> {
        let top = self.file_system.canonicalize(top)?;
        // Compare native directory identities, then keep porcelain paths in Git's
        // slash-separated form, including on Windows with verbatim root paths.
        let prefix = self
            .root
            .strip_prefix(&top)?
            .to_str()
            .context("Git workspace path is not UTF-8")?;
        let mut prefix = zess_platform::slash_separated_path(prefix);
        if !prefix.is_empty() {
            prefix.push('/');
        }
        let output = self.git_worktree(
            &[
                "status",
                "--porcelain=v1",
                "-z",
                "--untracked-files=all",
                "--ignore-submodules=all",
                "--",
                ".",
            ],
            cancellation,
        )?;

        if !output.status.success() {
            return Err(anyhow::anyhow!(
                String::from_utf8_lossy(&output.stderr).trim().to_owned()
            ))
            .with_context(|| format!("read Git status in {}", self.root.display()));
        }

        let mut records = output
            .stdout
            .split(|byte| *byte == 0)
            .filter(|record| !record.is_empty());
        let mut entries = Vec::new();

        while let Some(record) = records.next() {
            cancellation.check()?;

            if record.len() < 4 {
                bail!("invalid git status record");
            }

            let path = std::str::from_utf8(&record[3..])?;
            let original =
                if record[0] == b'R' || record[0] == b'C' || record[1] == b'R' || record[1] == b'C'
                {
                    Some(std::str::from_utf8(
                        records.next().context("missing git rename source")?,
                    )?)
                } else {
                    None
                };
            let Some(path) = path.strip_prefix(&prefix) else {
                continue;
            };
            let original = original.and_then(|path| path.strip_prefix(&prefix));

            let untracked = record[0] == b'?' && record[1] == b'?';
            entries.push(GitEntry {
                path: path.into(),
                original_path: original.map(str::to_owned),
                index: char::from(record[0]).to_string(),
                worktree: char::from(record[1]).to_string(),
                untracked,
                line_stats: None,
            });
        }

        Ok(entries)
    }

    pub fn diff(
        &self,
        relative: &str,
        original: Option<&str>,
        side: Option<DiffSide>,
        cancellation: &impl Execution,
    ) -> Result<String> {
        self.path(relative)?;

        let tracked = self.git(
            &["--literal-pathspecs", "ls-files", "-z", "--", relative],
            cancellation,
        )?;

        if !tracked.status.success() {
            bail!("{}", String::from_utf8_lossy(&tracked.stderr).trim());
        }

        let committed = self.git(
            &[
                "--literal-pathspecs",
                "ls-tree",
                "-z",
                "HEAD",
                "--",
                relative,
            ],
            cancellation,
        )?;
        let mut diff_args = vec![
            "--literal-pathspecs",
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--ignore-submodules=all",
            "--relative",
            "--no-color",
            "--no-renames",
            "--src-prefix=a/",
            "--dst-prefix=b/",
            "--unified=3",
            "--output-indicator-new=+",
            "--output-indicator-old=-",
            "--output-indicator-context= ",
            "--",
            relative,
        ];
        match side {
            Some(DiffSide::Staged) => diff_args.insert(5, "--cached"),
            Some(DiffSide::Unstaged) => {}
            None => diff_args.insert(5, "HEAD"),
        }

        if let Some(original) = original {
            self.path(original)?;
            diff_args.push(original);
        }

        let text = if tracked.stdout.is_empty()
            && committed.stdout.is_empty()
            && side != Some(DiffSide::Staged)
        {
            let output = self.git_worktree(
                &[
                    "diff",
                    "--no-index",
                    "--no-color",
                    "--src-prefix=a/",
                    "--dst-prefix=b/",
                    "--unified=3",
                    "--output-indicator-new=+",
                    "--output-indicator-old=-",
                    "--output-indicator-context= ",
                    "--no-ext-diff",
                    "--no-textconv",
                    "--",
                    "/dev/null",
                    relative,
                ],
                cancellation,
            )?;

            if !output.status.success() && output.status.code() != Some(1) {
                bail!(
                    "cannot read untracked diff: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                );
            }

            String::from_utf8(output.stdout).context("partial staging requires a UTF-8 patch")?
        } else {
            let output = self.git_worktree(&diff_args, cancellation)?;

            if output.status.success() {
                String::from_utf8(output.stdout)
                    .context("partial staging requires a UTF-8 patch")?
            } else {
                let staged = self.git_worktree(
                    &[
                        "--literal-pathspecs",
                        "diff",
                        "--cached",
                        "--ignore-submodules=all",
                        "--no-ext-diff",
                        "--no-textconv",
                        "--",
                        relative,
                    ],
                    cancellation,
                )?;
                let unstaged = self.git_worktree(
                    &[
                        "--literal-pathspecs",
                        "diff",
                        "--ignore-submodules=all",
                        "--no-ext-diff",
                        "--no-textconv",
                        "--",
                        relative,
                    ],
                    cancellation,
                )?;

                if !staged.status.success() || !unstaged.status.success() {
                    bail!(
                        "cannot read git diff: {}",
                        String::from_utf8_lossy(&output.stderr).trim()
                    );
                }

                format!(
                    "{}{}",
                    String::from_utf8_lossy(&staged.stdout),
                    String::from_utf8_lossy(&unstaged.stdout)
                )
            }
        };

        Ok(text)
    }

    pub fn diff_colors(&self, cancellation: &impl Execution) -> Result<DiffColors> {
        let mut command = ProcessSpec::new("git");
        command
            .args([
                "--no-pager",
                "config",
                "--null",
                "--type=color",
                "--get-regexp",
                r"^color\.diff\.(new|old)$",
            ])
            .current_dir(&self.root);
        let output = cancellation
            .output(&command)
            .context("cannot read Git diff colors")?;
        if !output.status.success() && output.status.code() != Some(1) {
            bail!(
                "cannot read Git diff colors: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        let mut colors = DiffColors {
            added: None,
            deleted: None,
        };
        for record in output
            .stdout
            .split(|byte| *byte == 0)
            .filter(|record| !record.is_empty())
        {
            let separator = record
                .iter()
                .position(|byte| *byte == b'\n')
                .context("invalid Git color record")?;
            let style = Some(record[separator + 1..].to_vec());
            match &record[..separator] {
                b"color.diff.new" => colors.added = style,
                b"color.diff.old" => colors.deleted = style,
                _ => bail!("unexpected Git color key"),
            }
        }
        cancellation.check()?;
        Ok(colors)
    }

    pub fn apply_patch(&self, request: PatchRequest, cancellation: &impl Execution) -> Result<()> {
        let text = self.diff(
            &request.path,
            request.original_path.as_deref(),
            Some(request.side),
            cancellation,
        )?;
        if text != request.raw {
            bail!("Git diff changed; refreshed preview, select the changes again");
        }
        let patch = patch::RawPatch::parse(&request.raw)?.selection(request.side, &request.rows)?;
        let prefix = self.git_checked(&["rev-parse", "--show-prefix"], cancellation)?;
        let prefix = std::str::from_utf8(&prefix)?
            .strip_suffix('\n')
            .context("invalid Git workspace prefix")?;
        let repository = self.git_checked(&["rev-parse", "--show-toplevel"], cancellation)?;
        let repository = std::str::from_utf8(&repository)?
            .strip_suffix('\n')
            .context("invalid Git repository root")?;
        let mut command = ProcessSpec::new("git");
        command.args([
            "--no-pager",
            "-c",
            "core.fsmonitor=false",
            "apply",
            "--cached",
            "--whitespace=nowarn",
        ]);
        if request.side == DiffSide::Staged {
            command.arg("--reverse");
        }
        if !prefix.is_empty() {
            command.arg(format!("--directory={prefix}"));
        }
        command
            .arg("-")
            .current_dir(repository)
            .env("GIT_TERMINAL_PROMPT", "0");
        let output = cancellation
            .output_with_input(&command, patch.as_bytes())
            .context("cannot apply selected Git patch")?;
        if !output.status.success() {
            bail!(
                "git apply: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(())
    }
}

fn commit_oid(value: &str) -> Result<&str> {
    if !matches!(value.len(), 40 | 64) || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("commit must be a full object ID");
    }
    Ok(value)
}

fn validate_relative(relative: &str) -> Result<()> {
    if Path::new(relative)
        .components()
        .any(|part| !matches!(part, Component::Normal(_) | Component::CurDir))
    {
        bail!("path must remain relative to the workspace");
    }
    Ok(())
}

pub fn metadata_directories(root: &Path) -> Result<Vec<PathBuf>> {
    metadata_directories_with_file_system(root, &NativeFileSystem)
}

pub fn metadata_directories_with_file_system(
    root: &Path,
    file_system: &dyn FileSystem,
) -> Result<Vec<PathBuf>> {
    for ancestor in root.ancestors() {
        let marker = ancestor.join(".git");
        let metadata = match file_system.metadata(&marker) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(error).with_context(|| format!("cannot inspect {}", marker.display()));
            }
        };
        let directory = if metadata.is_dir() {
            marker
        } else {
            let marker_text = file_system
                .read_to_string(&marker)
                .with_context(|| format!("cannot read Git directory from {}", marker.display()))?;
            let directory = marker_text
                .trim_end_matches(['\r', '\n'])
                .strip_prefix("gitdir: ")
                .with_context(|| format!("invalid Git directory file: {}", marker.display()))?;
            ancestor.join(directory)
        };
        let directory = file_system
            .canonicalize(&directory)
            .with_context(|| format!("cannot open Git directory for {}", root.display()))?;
        let common_file = directory.join("commondir");
        let common = match file_system.read_to_string(&common_file) {
            Ok(common) => common,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(vec![directory]);
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("cannot read {}", common_file.display()));
            }
        };
        let common = file_system
            .canonicalize(&directory.join(common.trim_end_matches(['\r', '\n'])))
            .with_context(|| format!("cannot open Git common directory for {}", root.display()))?;
        return Ok(vec![directory, common]);
    }
    Ok(Vec::new())
}
