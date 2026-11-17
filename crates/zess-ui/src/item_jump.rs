use std::collections::HashSet;

pub struct ItemJump {
    targets: Vec<String>,
    first: Option<u8>,
}

pub enum JumpInput {
    Waiting(ItemJump),
    Selected(String),
    Cancelled,
}

impl ItemJump {
    pub fn new<'a>(visible_ids: impl Iterator<Item = &'a str>) -> Option<Self> {
        let mut seen = HashSet::new();
        let targets = visible_ids
            .filter(|id| !id.is_empty() && seen.insert(*id))
            .take(26 * 26)
            .map(str::to_owned)
            .collect::<Vec<_>>();
        if targets.is_empty() {
            None
        } else {
            Some(Self {
                targets,
                first: None,
            })
        }
    }

    pub fn label(&self, id: &str) -> Option<String> {
        let index = self.targets.iter().position(|target| target == id)?;
        let first = (b'a' + (index / 26) as u8) as char;
        let second = (b'a' + (index % 26) as u8) as char;
        Some(format!("{first}{second}"))
    }

    pub fn prompt(&self) -> String {
        match self.first {
            Some(first) => format!(
                "Goto item: {}_ · type the second letter · Esc to cancel",
                char::from(b'a' + first)
            ),
            None => "Goto item: type a highlighted label · Esc to cancel".into(),
        }
    }

    pub fn input(mut self, key: &str) -> JumpInput {
        let [key] = *key.as_bytes() else {
            return JumpInput::Cancelled;
        };
        if !key.is_ascii_lowercase() {
            return JumpInput::Cancelled;
        }
        let letter = key - b'a';
        match self.first {
            Some(first) => {
                let index = usize::from(first) * 26 + usize::from(letter);
                self.targets
                    .get(index)
                    .cloned()
                    .map_or(JumpInput::Cancelled, JumpInput::Selected)
            }
            None if usize::from(letter) * 26 < self.targets.len() => {
                self.first = Some(letter);
                JumpInput::Waiting(self)
            }
            None => JumpInput::Cancelled,
        }
    }
}
