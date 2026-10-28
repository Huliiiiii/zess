use std::{cell::RefCell, rc::Rc};

use zess_plugin::HostRequest;
use zess_services::NativeServices;

use zess_plugin::{BufferPluginHandle, PluginRegistrar};

mod browser;
mod schedule;

type Services = Rc<RefCell<NativeServices>>;

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
            "page" => Self::Page,
            "page-changes" => Self::PageChanges,
            "page-history" => Self::PageHistory,
            "back" => Self::Back,
            _ => Self::Unknown,
        }
    }
}
