use std::collections::HashSet;

use zess_platform::ProcessSpec;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::{DiffSide, DiffSource, DiffTarget, Execution, RenderedDiff, Repository};

#[cfg(test)]
mod tests;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PagerCommand {
    pub program: String,
    #[serde(default)]
    pub args: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct DiffRenderOptions {
    pub width: Option<usize>,
    pub pager: Option<PagerCommand>,
    #[serde(default)]
    pub file_header: bool,
}

pub struct DiffRenderer {
    file_header: bool,
    width: Option<usize>,
    pager: Option<PagerCommand>,
    external: bool,
    textconv_drivers: HashSet<String>,
    external_drivers: HashSet<String>,
}

impl Repository {
    pub fn diff_renderer(
        &self,
        options: &DiffRenderOptions,
        cancellation: &impl Execution,
    ) -> Result<DiffRenderer> {
        anyhow::ensure!(
            options.width.is_none_or(|width| width > 0),
            "diff width must be positive"
        );
        cancellation.check()?;
        if let Some(pager) = &options.pager {
            anyhow::ensure!(
                !pager.program.trim().is_empty(),
                "diff pager program must not be empty"
            );
            anyhow::ensure!(
                !pager.program.contains('\0') && pager.args.iter().all(|arg| !arg.contains('\0')),
                "diff pager program and arguments must not contain NUL"
            );
            return Ok(DiffRenderer {
                file_header: options.file_header,
                width: options.width,
                pager: Some(pager.clone()),
                external: false,
                textconv_drivers: HashSet::new(),
                external_drivers: HashSet::new(),
            });
        }
        let mut command = ProcessSpec::new("git");
        command
            .args([
                "config",
                "--null",
                "--get-regexp",
                r"^(diff\.external|diff\..*\.(command|textconv))$",
            ])
            .current_dir(&self.root);
        let output = cancellation.output(&command)?;
        if !output.status.success() && output.status.code() != Some(1) {
            bail!(
                "read Git diff renderer configuration: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        let mut external = false;
        let mut textconv_drivers = HashSet::new();
        let mut external_drivers = HashSet::new();
        for record in output
            .stdout
            .split(|byte| *byte == 0)
            .filter(|record| !record.is_empty())
        {
            let record = std::str::from_utf8(record)
                .context("Git diff renderer configuration is not UTF-8")?;
            let (key, value) = match record.split_once('\n') {
                Some(pair) => pair,
                None => bail!("Git diff renderer configuration {record} requires a value"),
            };
            match key {
                "diff.external" => external = !value.is_empty(),
                _ => {
                    if let Some((driver, option)) = key
                        .strip_prefix("diff.")
                        .and_then(|key| key.rsplit_once('.'))
                    {
                        let drivers = if option == "command" {
                            &mut external_drivers
                        } else {
                            &mut textconv_drivers
                        };
                        if value.is_empty() {
                            drivers.remove(driver);
                        } else {
                            drivers.insert(driver.to_owned());
                        }
                    }
                }
            }
        }
        external |= cancellation
            .environment(std::ffi::OsStr::new("GIT_EXTERNAL_DIFF"))
            .is_some();
        Ok(DiffRenderer {
            file_header: options.file_header,
            width: options.width,
            pager: None,
            external,
            textconv_drivers,
            external_drivers,
        })
    }
}

impl DiffRenderer {
    pub fn render(
        &self,
        repository: &Repository,
        source: &DiffSource,
        target: &DiffTarget,
        raw: &str,
        cancellation: &impl Execution,
    ) -> Result<Option<RenderedDiff>> {
        cancellation.check()?;
        for path in std::iter::once(&target.path).chain(&target.original_path) {
            match source {
                DiffSource::Changes { .. } => {
                    repository.path(path)?;
                }
                DiffSource::Commit { .. } => crate::validate_relative(path)?,
            }
        }
        let mut external = self.external;
        let mut allow_external = self.external;
        if !external && !(self.textconv_drivers.is_empty() && self.external_drivers.is_empty()) {
            let output = repository.git(
                &["check-attr", "-z", "diff", "--", &target.path],
                cancellation,
            )?;
            if !output.status.success() {
                bail!(
                    "read Git diff attributes: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                );
            }
            let driver = output
                .stdout
                .split(|byte| *byte == 0)
                .nth(2)
                .and_then(|value| std::str::from_utf8(value).ok());
            external = driver.is_some_and(|driver| {
                self.textconv_drivers.contains(driver) || self.external_drivers.contains(driver)
            });
            allow_external = driver.is_some_and(|driver| self.external_drivers.contains(driver));
        }
        if !external && self.pager.is_none() {
            return Ok(None);
        }
        let git = repository
            .queries
            .discover(&repository.root)?
            .context("Git repository not found")?;
        let repository_root = repository.file_system.canonicalize(&git.worktree)?;
        let text = if external {
            repository.rendered_git_diff(
                source,
                target,
                allow_external,
                self.width,
                cancellation,
            )?
        } else {
            raw.to_owned()
        };
        let Some(pager) = &self.pager else {
            return Ok(Some(RenderedDiff {
                file_header: self.file_header,
                text,
                repository_root,
            }));
        };
        let mut command = ProcessSpec::new(&pager.program);
        command
            .args(&pager.args)
            .current_dir(&repository.root)
            .env("GIT_PAGER", "cat")
            .env("DELTA_PAGER", "cat")
            .env("BAT_PAGER", "cat")
            .env("OSC1717", "V1")
            .env("CLICOLOR_FORCE", "1")
            .env("FORCE_COLOR", "1");
        if let Some(width) = self.width {
            command
                .env("COLUMNS", width.to_string())
                .env("DFT_WIDTH", width.to_string());
        }
        let output = cancellation
            .output_with_input(&command, text.as_bytes())
            .context("run configured Git diff pager")?;
        if !output.status.success() {
            bail!(
                "Git diff pager failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(Some(RenderedDiff {
            file_header: self.file_header,
            text: String::from_utf8(output.stdout).context("Git diff pager output is not UTF-8")?,
            repository_root,
        }))
    }
}

impl Repository {
    fn rendered_git_diff(
        &self,
        source: &DiffSource,
        target: &DiffTarget,
        allow_external: bool,
        width: Option<usize>,
        cancellation: &impl Execution,
    ) -> Result<String> {
        let mut args = vec!["--literal-pathspecs"];
        let untracked;
        match source {
            DiffSource::Changes { side } => {
                let tracked = self.git(&["ls-files", "-z", "--", &target.path], cancellation)?;
                if !tracked.status.success() {
                    bail!(
                        "read Git tracked file: {}",
                        String::from_utf8_lossy(&tracked.stderr).trim()
                    );
                }
                let committed =
                    self.git(&["ls-tree", "-z", "HEAD", "--", &target.path], cancellation)?;
                untracked = tracked.stdout.is_empty()
                    && committed.stdout.is_empty()
                    && *side == DiffSide::Unstaged;
                args.push("diff");
                if untracked {
                    args.push("--no-index");
                } else if *side == DiffSide::Staged {
                    args.push("--cached");
                }
            }
            DiffSource::Commit { commit } => {
                untracked = false;
                args.extend([
                    "show",
                    "--format=",
                    "--diff-merges=first-parent",
                    commit.as_str(),
                ]);
            }
        }
        args.extend([
            "--patch",
            if allow_external {
                "--ext-diff"
            } else {
                "--no-ext-diff"
            },
            "--textconv",
            "--color=always",
            "--ignore-submodules=all",
            "--",
        ]);
        if untracked {
            args.push("/dev/null");
        }
        args.push(&target.path);
        if !untracked && let Some(original) = &target.original_path {
            args.push(original);
        }
        let mut command = match source {
            DiffSource::Changes { .. } => self.git_worktree_command(cancellation)?,
            DiffSource::Commit { .. } => self.git_command(),
        };
        command.args(&args);
        command.env("OSC1717", "V1");
        if let Some(width) = width {
            command
                .env("COLUMNS", width.to_string())
                .env("DFT_WIDTH", width.to_string());
        }
        if cancellation
            .environment(std::ffi::OsStr::new("DFT_COLOR"))
            .is_none()
        {
            command.env("DFT_COLOR", "always");
        }
        let output = cancellation.output(&command)?;
        if !output.status.success() && !(untracked && output.status.code() == Some(1)) {
            bail!(
                "render configured Git diff: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        String::from_utf8(output.stdout).context("configured Git diff output is not UTF-8")
    }
}
