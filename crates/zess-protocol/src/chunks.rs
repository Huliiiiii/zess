use serde::de::DeserializeOwned;

/// Splits serialized JSON on character boundaries; each escaped piece and its
/// frame envelope fit the transport limit even for six-byte JSON escapes.
pub fn json_parts(mut json: &str, maximum: usize) -> impl Iterator<Item = (&str, bool)> {
    assert!(
        maximum >= 280,
        "frame must fit its envelope and a UTF-8 character"
    );
    let limit = (maximum - 256) / 6;
    std::iter::from_fn(move || {
        if json.is_empty() {
            return None;
        }
        let end = json.floor_char_boundary(limit.min(json.len()));
        let (part, remaining) = json.split_at(end);
        json = remaining;
        Some((part, remaining.is_empty()))
    })
}

#[derive(Default)]
pub(crate) struct JsonParts(String);

impl JsonParts {
    pub(crate) fn assemble<T: DeserializeOwned>(
        &mut self,
        json: String,
        last: bool,
    ) -> serde_json::Result<Option<T>> {
        self.0.push_str(&json);
        if !last {
            return Ok(None);
        }
        serde_json::from_str(&std::mem::take(&mut self.0)).map(Some)
    }

    pub(crate) fn clear(&mut self) {
        self.0.clear();
    }
}

impl crate::ClientMessage {
    pub fn can_chunk(&self) -> bool {
        matches!(
            self,
            Self::Resource { .. }
                | Self::SaveWorkspaceView { .. }
                | Self::Command { .. }
                | Self::FormatStatusline { .. }
        )
    }
}

impl crate::ServerMessage {
    pub fn can_chunk(&self) -> bool {
        matches!(
            self,
            Self::Resource { .. }
                | Self::ResourceCacheInvalidated { .. }
                | Self::Attached { .. }
                | Self::Snapshot { .. }
                | Self::Delta { .. }
                | Self::CommandResult { .. }
                | Self::FormattedStatusline { .. }
        )
    }
}

/// Consecutive parts of one logical message on one connection.
#[derive(Default)]
pub struct MessageStream(JsonParts);

impl MessageStream {
    pub fn client(
        &mut self,
        json: String,
        last: bool,
    ) -> serde_json::Result<Option<crate::ClientMessage>> {
        let message = self.0.assemble::<crate::ClientMessage>(json, last)?;
        if message.as_ref().is_some_and(|message| !message.can_chunk()) {
            return Err(serde::de::Error::custom(
                "unexpected fragmented client message",
            ));
        }
        Ok(message)
    }

    pub fn server(
        &mut self,
        json: String,
        last: bool,
    ) -> serde_json::Result<Option<crate::ServerMessage>> {
        let message = self.0.assemble::<crate::ServerMessage>(json, last)?;
        if message.as_ref().is_some_and(|message| !message.can_chunk()) {
            return Err(serde::de::Error::custom(
                "unexpected fragmented server message",
            ));
        }
        Ok(message)
    }
}
