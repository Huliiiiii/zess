use serde::{Deserialize, Serialize};

#[cfg(test)]
mod tests;

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StyleColor {
    #[default]
    Default,
    Indexed(u8),
    Rgb {
        red: u8,
        green: u8,
        blue: u8,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StyleAttribute {
    Bold,
    Dim,
    Italic,
    Underline,
    Blink,
    Reverse,
    Strike,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TextStyle {
    #[serde(default)]
    pub foreground: Option<StyleColor>,
    #[serde(default)]
    pub background: Option<StyleColor>,
    #[serde(default)]
    pub attributes: Vec<StyleAttribute>,
    #[serde(default)]
    pub removed_attributes: Vec<StyleAttribute>,
}

impl TextStyle {
    pub fn reset() -> Self {
        Self {
            foreground: Some(StyleColor::Default),
            background: Some(StyleColor::Default),
            attributes: Vec::new(),
            removed_attributes: vec![
                StyleAttribute::Bold,
                StyleAttribute::Dim,
                StyleAttribute::Italic,
                StyleAttribute::Underline,
                StyleAttribute::Blink,
                StyleAttribute::Reverse,
                StyleAttribute::Strike,
            ],
        }
    }

    pub fn patch(mut self, other: &Self) -> Self {
        self.foreground = other.foreground.or(self.foreground);
        self.background = other.background.or(self.background);
        self.attributes
            .retain(|attribute| !other.removed_attributes.contains(attribute));
        for attribute in &other.attributes {
            if !self.attributes.contains(attribute) {
                self.attributes.push(attribute.clone());
            }
        }
        self.removed_attributes
            .retain(|attribute| !other.attributes.contains(attribute));
        for attribute in &other.removed_attributes {
            if !self.removed_attributes.contains(attribute) {
                self.removed_attributes.push(attribute.clone());
            }
        }
        self
    }
}
