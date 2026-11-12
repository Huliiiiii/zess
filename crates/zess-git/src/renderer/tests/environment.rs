use std::ffi::{OsStr, OsString};

use super::*;

struct Execute {
    environment: Vec<(&'static str, &'static str)>,
}

impl Execution for Execute {
    fn environment(&self, name: &OsStr) -> Option<OsString> {
        self.environment
            .iter()
            .find_map(|(key, value)| (OsStr::new(key) == name).then(|| OsString::from(value)))
    }

    fn check(&self) -> anyhow::Result<()> {
        Ok(())
    }

    fn output(&self, spec: &zess_platform::ProcessSpec) -> anyhow::Result<std::process::Output> {
        let mut command = std::process::Command::new(&spec.program);
        command.args(&spec.args);
        if let Some(cwd) = &spec.cwd {
            command.current_dir(cwd);
        }
        command
            .env_remove("GIT_EXTERNAL_DIFF")
            .env_remove("DFT_COLOR");
        command.envs(self.environment.iter().copied());
        command.envs(spec.env.iter().map(|(key, value)| (key, value)));
        Ok(command.output()?)
    }

    fn output_with_input(
        &self,
        _: &zess_platform::ProcessSpec,
        _: &[u8],
    ) -> anyhow::Result<std::process::Output> {
        anyhow::bail!("this renderer has no pager")
    }
}

#[test]
fn external_diff_is_selected_from_the_execution_environment() {
    let fixture = crate::tests::Repository::new();
    fixture.commit("old\n");
    std::fs::write(fixture.0.join("file"), "new\n").unwrap();
    std::fs::write(
        fixture.0.join("renderer.sh"),
        "printf 'context:%s\\n' \"$1\"\n",
    )
    .unwrap();
    let repository = Repository::open(&fixture.0).unwrap();
    let execution = Execute {
        environment: vec![("GIT_EXTERNAL_DIFF", "sh ./renderer.sh")],
    };
    let renderer = repository
        .diff_renderer(&DiffRenderOptions::default(), &execution)
        .unwrap();
    let rendered = renderer
        .render(
            &repository,
            &DiffSource::Changes {
                side: DiffSide::Unstaged,
            },
            &DiffTarget {
                path: "file".into(),
                original_path: None,
            },
            "raw",
            &execution,
        )
        .unwrap()
        .unwrap();
    assert_eq!(rendered.text, "context:file\n");
}

#[test]
fn configured_diff_preserves_the_execution_color_preference() {
    let fixture = crate::tests::Repository::new();
    fixture.commit("old\n");
    std::fs::write(fixture.0.join("file"), "new\n").unwrap();
    std::fs::write(
        fixture.0.join("renderer.sh"),
        "printf '%s\\n' \"$DFT_COLOR\"\n",
    )
    .unwrap();
    fixture.git(&["config", "diff.external", "sh ./renderer.sh"]);
    let repository = Repository::open(&fixture.0).unwrap();
    let execution = Execute {
        environment: vec![("DFT_COLOR", "never")],
    };
    let renderer = repository
        .diff_renderer(&DiffRenderOptions::default(), &execution)
        .unwrap();
    let rendered = renderer
        .render(
            &repository,
            &DiffSource::Changes {
                side: DiffSide::Unstaged,
            },
            &DiffTarget {
                path: "file".into(),
                original_path: None,
            },
            "raw",
            &execution,
        )
        .unwrap()
        .unwrap();
    assert_eq!(rendered.text, "never\n");
}
