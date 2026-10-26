use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
pub struct Cursor {
    pub row: usize,
    pub column: usize,
    pub anchor_row: usize,
    pub anchor_column: usize,
}
