use std::{cell::RefCell, rc::Rc};

use anyhow::Result;
use serde_json::Value;
use zess_plugin::HostRequest;
use zess_services::NativeServices;

use zess_plugin::{BufferPluginHandle, PluginRegistrar};

mod browser;
mod file_actions;
mod files;
mod git;
mod schedule;

type Services = Rc<RefCell<NativeServices>>;

struct ViewOptions<T>(Option<(Value, Rc<T>)>);

impl<T> Default for ViewOptions<T> {
    fn default() -> Self {
        Self(None)
    }
}

impl<T> ViewOptions<T> {
    fn resolve(
        &mut self,
        options: &Value,
        parse: impl FnOnce(&Value) -> Result<T>,
    ) -> Result<Rc<T>> {
        if let Some((source, parsed)) = &self.0
            && source == options
        {
            return Ok(Rc::clone(parsed));
        }

        let parsed = Rc::new(parse(options)?);
        self.0 = Some((options.clone(), Rc::clone(&parsed)));
        Ok(parsed)
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Event {
    Init,
    Navigate,
    ViewCloned,
    ViewEntered,
    ViewLeaving,
    FilterChanged,
    ModeChanged,
    Find,
    MiniBufferChanged,
    FsChanged,
    Refresh,
    Select,
    Open,
    Edit,
    EditLayer,
    Preview,
    Left,
    Right,
    Toggle,
    ToggleView,
    YankPath,
    YankAbsolutePath,
    YankName,
    YankContent,
    YankDiff,
    ToggleIgnored,
    Create,
    Rename,
    Delete,
    UndoPending,
    LayerResult,
    Save,
    Stage,
    Unstage,
    Discard,
    Ignore,
    Exclude,
    Page,
    PageChanges,
    PageHistory,
    Back,
    Unknown,
}

impl Event {
    fn parse(name: &str) -> Self {
        match name {
            "init" => Self::Init,
            "navigate" => Self::Navigate,
            "view_cloned" => Self::ViewCloned,
            "view_entered" => Self::ViewEntered,
            "view_leaving" => Self::ViewLeaving,
            "filter_changed" => Self::FilterChanged,
            "mode_changed" => Self::ModeChanged,
            "find" => Self::Find,
            "minibuffer_changed" => Self::MiniBufferChanged,
            "fs_changed" => Self::FsChanged,
            "refresh" => Self::Refresh,
            "select" => Self::Select,
            "open" => Self::Open,
            "edit" => Self::Edit,
            "edit-layer" => Self::EditLayer,
            "preview" => Self::Preview,
            "left" => Self::Left,
            "right" => Self::Right,
            "toggle" => Self::Toggle,
            "toggle-view" => Self::ToggleView,
            "yank-path" => Self::YankPath,
            "yank-absolute-path" => Self::YankAbsolutePath,
            "yank-name" => Self::YankName,
            "yank-content" => Self::YankContent,
            "yank-diff" => Self::YankDiff,
            "toggle_ignored" => Self::ToggleIgnored,
            "create" => Self::Create,
            "rename" => Self::Rename,
            "delete" => Self::Delete,
            "undo_pending" => Self::UndoPending,
            "layer_result" => Self::LayerResult,
            "save" => Self::Save,
            "stage" => Self::Stage,
            "unstage" => Self::Unstage,
            "discard" => Self::Discard,
            "ignore" => Self::Ignore,
            "exclude" => Self::Exclude,
            "page" => Self::Page,
            "page-changes" => Self::PageChanges,
            "page-history" => Self::PageHistory,
            "back" => Self::Back,
            _ => Self::Unknown,
        }
    }
}

pub struct BuiltinPlugins {
    files: BufferPluginHandle,
    changes: BufferPluginHandle,
    history: BufferPluginHandle,
    commit_files: BufferPluginHandle,
}

impl BuiltinPlugins {
    pub fn new(services: Services) -> Self {
        Self {
            files: Rc::new(RefCell::new(files::Files::new(services.clone()))),
            changes: Rc::new(RefCell::new(git::Git::new(
                services.clone(),
                git::Page::Changes,
            ))),
            history: Rc::new(RefCell::new(git::Git::new(
                services.clone(),
                git::Page::History,
            ))),
            commit_files: Rc::new(RefCell::new(git::Git::new(
                services,
                git::Page::CommitFiles,
            ))),
        }
    }

    pub fn register(&self, registry: &mut dyn PluginRegistrar) {
        registry.view_defaults(
            "files",
            [
                (
                    "double-click-command".into(),
                    serde_json::json!("files.toggle"),
                ),
                ("git-status".into(), serde_json::json!(true)),
                ("preview-wrap".into(), serde_json::json!(true)),
                ("default-yank-register".into(), serde_json::json!("+")),
            ]
            .into_iter()
            .collect(),
        );
        for kind in [
            "git",
            "git-history",
            "git-commit-files",
            "diff",
            "preview",
            "directory",
        ] {
            let mut defaults = [("header-style".into(), serde_json::json!(""))]
                .into_iter()
                .collect::<serde_json::Map<_, _>>();
            if matches!(kind, "git" | "git-commit-files") {
                defaults.insert("git-view".into(), serde_json::json!("tree"));
                defaults.insert("stats-alignment".into(), serde_json::json!("all"));
                defaults.insert("default-yank-register".into(), serde_json::json!("+"));
            }
            registry.view_defaults(kind, defaults);
        }
        for (scope, label) in [
            ("files", "Files"),
            ("files.find", "Find files"),
            ("git", "Git changes"),
            ("git-history", "Git history"),
            ("git-commit-files", "Git commit files"),
            ("git-diff", "Git diff"),
            ("git-commit-preview", "Historical file preview"),
            ("git-history-preview", "Commit preview"),
        ] {
            registry.scope(scope, label);
        }
        for (kind, backend) in [
            ("files", &self.files),
            ("git", &self.changes),
            ("git-history", &self.history),
            ("git-commit-files", &self.commit_files),
        ] {
            registry.buffer(kind, backend.clone());
            registry.mode(kind, "filter");
            browser::register(registry, kind);
        }
        for kind in ["files", "git"] {
            browser::register_file_actions(
                registry,
                kind,
                "Open source file",
                "Edit file in a temporary fullscreen layer",
            );
        }
        browser::register_file_actions(
            registry,
            "git-commit-files",
            "Open historical file",
            "Read historical file in a temporary fullscreen layer",
        );
        registry.command(
            "files",
            "Open file tree",
            HostRequest::OpenBuffer {
                kind: "files".into(),
                argument: serde_json::json!({}),
            },
        );
        registry.command(
            "git",
            "Open Git changes",
            HostRequest::OpenBuffer {
                kind: "git".into(),
                argument: serde_json::json!({}),
            },
        );
        for (event, description, keys) in [
            ("open", "Open selected file", vec!["Enter"]),
            ("left", "Collapse / parent directory", vec!["h"]),
            ("right", "Expand directory", vec!["l"]),
            ("create", "Create file / directory", vec!["a"]),
            ("create", "Create file / directory", vec!["i"]),
            ("rename", "Rename selection", vec!["r"]),
            ("delete", "Mark selection for deletion", vec!["d"]),
            ("undo_pending", "Undo pending operations", vec!["u"]),
        ] {
            browser::bind_event(registry, "files", event, description, &keys);
        }
        registry.mode("files", "find");
        registry.buffer_command(
            "files.toggle",
            "Expand / collapse directory",
            "files",
            "toggle",
        );
        browser::bind_event(registry, "files", "find", "Find files recursively", &["f"]);
        for (key, command) in [
            ("Tab", "data.move-down"),
            ("S-Tab", "data.move-up"),
            ("Down", "data.move-down"),
            ("Up", "data.move-up"),
            ("C-n", "data.move-down"),
            ("C-p", "data.move-up"),
        ] {
            registry.binding("files.find", "prompt", &[key], command);
        }
        registry.buffer_command(
            "files.toggle_ignored",
            "Show / hide ignored paths",
            "files",
            "toggle_ignored",
        );
        for (kind, description) in [
            ("git", "Read diff"),
            ("git-history", "Browse commit files"),
            ("git-commit-files", "Read historical diff"),
        ] {
            for (direction, key) in [("previous", "["), ("next", "]")] {
                let command = format!("{kind}.page-{direction}");
                registry.buffer_command(&command, "Switch Git page", kind, "page");
                for mode in ["normal", "select"] {
                    registry.binding(kind, mode, &["C-w", key], &command);
                }
            }
            for (page, key, description) in [
                ("changes", "1", "Browse changes"),
                ("history", "2", "Browse history"),
            ] {
                let command = format!("{kind}.page-{page}");
                registry.buffer_command(&command, description, kind, &format!("page-{page}"));
                for mode in ["normal", "select"] {
                    registry.binding(kind, mode, &["C-w", key], &command);
                }
            }
            browser::bind_event(registry, kind, "open", description, &["Enter"]);
        }
        for (event, description, keys) in [
            ("stage", "Stage whole files / directories", &["s"][..]),
            ("unstage", "Unstage whole files / directories", &["u"][..]),
            (
                "discard",
                "Discard working changes in files / directories",
                &["d"][..],
            ),
            ("ignore", "Ignore in .gitignore (shared)", &["i", "g"][..]),
            ("exclude", "Ignore in info/exclude (local)", &["i", "l"][..]),
        ] {
            browser::bind_event(registry, "git", event, description, keys);
        }
        registry.buffer_command(
            "git-commit-files.back",
            "Return to commits",
            "git-commit-files",
            "back",
        );
        registry.binding(
            "git-commit-files",
            "normal",
            &["Esc"],
            "git-commit-files.back",
        );
        for kind in ["git", "git-commit-files"] {
            registry.buffer_command(
                &format!("{kind}.toggle-view"),
                "Toggle tree / flat view",
                kind,
                "toggle-view",
            );
            browser::bind_event(
                registry,
                kind,
                "left",
                "Collapse / parent directory",
                &["h"],
            );
            browser::bind_event(registry, kind, "right", "Expand / child directory", &["l"]);
        }
        for mode in ["normal", "select"] {
            for (keys, command) in [
                (vec!["space", "s"], "diff.apply-selection"),
                (vec!["a"], "diff.toggle-selection"),
                (vec!["}"], "diff.hunk-next"),
                (vec!["{"], "diff.hunk-previous"),
            ] {
                registry.binding("git-diff", mode, &keys, command);
            }
        }
        for (keys, command) in [
            (vec!["/"], "diff.search-forward"),
            (vec!["?"], "diff.search-backward"),
            (vec!["n"], "diff.search-next"),
            (vec!["N"], "diff.search-previous"),
            (vec!["j"], "diff.move-next"),
            (vec!["l"], "diff.move-next"),
            (vec!["k"], "diff.move-previous"),
            (vec!["h"], "diff.move-previous"),
            (vec!["a"], "diff.toggle-selection"),
            (vec!["v"], "diff.range-selection"),
            (vec!["g", "g"], "diff.goto-first"),
            (vec!["g", "e"], "diff.goto-last"),
            (vec!["C-d"], "diff.page-half-down"),
            (vec!["C-u"], "diff.page-half-up"),
            (vec!["e"], "diff.edit-source"),
            (vec!["g", "f"], "preview.open-source"),
            (vec!["}"], "diff.hunk-next"),
            (vec!["{"], "diff.hunk-previous"),
            (vec!["Esc"], "preview.return"),
        ] {
            registry.binding("diff", "normal", &keys, command);
        }
        for scope in ["git-diff", "git-commit-preview", "git-history-preview"] {
            registry.binding(scope, "normal", &["Esc"], "preview.return");
        }
    }
}
