use std::{fmt, num::NonZeroUsize};

#[cfg(test)]
mod tests;

#[derive(Clone, Copy, Default)]
pub struct CommandCount(Option<NonZeroUsize>);

impl CommandCount {
    pub fn push(&mut self, digit: u8) {
        let value = self.value().unwrap_or(0);
        if let Some(value) = value
            .checked_mul(10)
            .and_then(|value| value.checked_add(usize::from(digit)))
            .filter(|&value| value <= 100_000_000)
        {
            self.0 = NonZeroUsize::new(value);
        }
    }

    pub fn value(self) -> Option<usize> {
        self.0.map(NonZeroUsize::get)
    }

    pub fn clear(&mut self) {
        *self = Self::default();
    }

    pub fn is_empty(self) -> bool {
        self.0.is_none()
    }
}

impl From<usize> for CommandCount {
    fn from(value: usize) -> Self {
        Self(NonZeroUsize::new(value))
    }
}

impl fmt::Display for CommandCount {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(value) = self.0 {
            write!(formatter, "{value}")?;
        }
        Ok(())
    }
}
