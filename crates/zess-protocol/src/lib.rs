use serde::{Deserialize, Serialize};
use zess_ui::{Frame, MouseContext, MouseEvent};

pub const MAX_MESSAGE_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_INPUT_BYTES: usize = 1024 * 1024;

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    Status,
    Stop {
        force: bool,
    },
    Attach {
        client_id: String,
        width: u16,
        height: u16,
        #[serde(default)]
        files: Vec<String>,
        #[serde(default)]
        context_epoch: Option<u64>,
    },
    Input {
        sequence: u64,
        key: String,
        context_epoch: u64,
    },
    Mouse {
        sequence: u64,
        context_epoch: u64,
        mouse_context: MouseContext,
        event: MouseEvent,
    },
    Skip {
        through: u64,
        context_epoch: u64,
    },
    Resize {
        width: u16,
        height: u16,
    },
    ClipboardResponse {
        id: u64,
        text: Option<String>,
        error: Option<String>,
    },
    Detach,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    Status {
        pid: u32,
        workspace: String,
        attached: bool,
        modified: bool,
    },
    Attached {
        acknowledged: u64,
        context_epoch: u64,
        replay_allowed: bool,
        frame: Frame,
    },
    Update {
        acknowledged: u64,
        frame: Frame,
    },
    ClipboardWrite {
        text: String,
    },
    ClipboardWritePrimary {
        text: String,
    },
    ClipboardRead {
        id: u64,
        primary: bool,
    },
    Rejected {
        reason: String,
    },
    Exited,
}
