use std::{collections::HashSet, process::Command};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::{DiffSide, DiffSource, DiffTarget, Execution, RenderedDiff, Repository};

#[cfg(test)]
mod tests;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct DiffRenderOptions {
    pub width: Option<usize>,
}

pub struct DiffRenderer {
    width: Option<usize>,
    pager: Option<String>,
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
        let mut command = Command::new("git");
        command
            .args([
                "config",
                "--null",
                "--get-regexp",
                r"^(core\.pager|pager\.diff|diff\.external|diff\..*\.(command|textconv))$",
            ])
            .current_dir(&self.root);
        let output = cancellation.output(&mut command)?;
        if !output.status.success() && output.status.code() != Some(1) {
            bail!(
                "read Git diff renderer configuration: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        let mut core_pager = None;
        let mut diff_pager = None;
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
                None if record == "pager.diff" => (record, "true"),
                None => bail!("Git diff renderer configuration {record} requires a value"),
            };
            match key {
                "core.pager" => core_pager = Some(value.to_owned()),
                "pager.diff" => diff_pager = Some(value.to_owned()),
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
        external |= std::env::var_os("GIT_EXTERNAL_DIFF").is_some();
        let git_pager = std::env::var_os("GIT_PAGER")
            .map(|pager| {
                pager
                    .into_string()
                    .map_err(|_| anyhow::anyhow!("GIT_PAGER is not UTF-8"))
            })
            .transpose()?;
        let pager = select_pager(
            git_pager,
            diff_pager,
            core_pager,
            std::env::var("PAGER").ok(),
        );
        Ok(DiffRenderer {
            width: options.width,
            pager: pager.filter(|pager| !pager.trim().is_empty() && pager.trim() != "cat"),
            external,
            textconv_drivers,
            external_drivers,
        })
    }
}

fn select_pager(
    git_pager: Option<String>,
    diff_pager: Option<String>,
    core_pager: Option<String>,
    pager: Option<String>,
) -> Option<String> {
    if diff_pager.as_deref().is_some_and(|value| {
        ["false", "no", "off", ""]
            .into_iter()
            .any(|disabled| value.eq_ignore_ascii_case(disabled))
            || value.parse::<i64>() == Ok(0)
    }) {
        return None;
    }
    if let Some(pager) = git_pager {
        return Some(pager);
    }
    match diff_pager.as_deref() {
        Some(value)
            if ["true", "yes", "on"]
                .into_iter()
                .any(|enabled| value.eq_ignore_ascii_case(enabled))
                || value.parse::<i64>().is_ok() =>
        {
            core_pager.or(pager)
        }
        None => core_pager.or(pager),
        Some(pager) => Some(pager.to_owned()),
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
        repository.path(&target.path)?;
        if let Some(original) = &target.original_path {
            repository.path(original)?;
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
        let repository_root = gix::discover(&repository.root)?
            .workdir()
            .context("Git repository has no working directory")?
            .canonicalize()?;
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
                text,
                repository_root,
            }));
        };
        let mut command = Command::new("sh");
        command
            .arg("-c")
            .arg(pager)
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
            .output_with_input(&mut command, text.as_bytes())
            .context("run configured Git diff pager")?;
        if !output.status.success() {
            bail!(
                "Git diff pager failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(Some(RenderedDiff {
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
        let mut command = self.git_command(&args, cancellation)?;
        command.env("OSC1717", "V1");
        if let Some(width) = width {
            command
                .env("COLUMNS", width.to_string())
                .env("DFT_WIDTH", width.to_string());
        }
        if std::env::var_os("DFT_COLOR").is_none() {
            command.env("DFT_COLOR", "always");
        }
        let output = cancellation.output(&mut command)?;
        if !output.status.success() && !(untracked && output.status.code() == Some(1)) {
            bail!(
                "render configured Git diff: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        String::from_utf8(output.stdout).context("configured Git diff output is not UTF-8")
    }
}
