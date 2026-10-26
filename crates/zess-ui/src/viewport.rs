#[cfg(test)]
mod tests;

pub enum ScrollDirection {
    Up,
    Down,
}

pub struct HalfPageScroll {
    direction: ScrollDirection,
    distance: usize,
    height: usize,
}

impl HalfPageScroll {
    pub fn new(height: usize, count: usize, direction: ScrollDirection) -> Self {
        Self {
            direction,
            distance: (height / 2).max(1).saturating_mul(count),
            height,
        }
    }

    pub fn distance(&self) -> usize {
        self.distance
    }

    pub fn viewport_start(&self, scroll: usize, row: usize, scrolloff: usize) -> usize {
        let scroll = match self.direction {
            ScrollDirection::Up => scroll.saturating_sub(self.distance),
            ScrollDirection::Down => scroll.saturating_add(self.distance),
        };
        scroll_with_margin(scroll, row, self.height, scrolloff)
    }
}

/// Keeps the cursor within the viewport margins after a requested scroll.
pub fn scroll_with_margin(scroll: usize, row: usize, height: usize, scrolloff: usize) -> usize {
    let height = height.max(1);
    let margin = scroll_margin(height, scrolloff);
    let first = row.saturating_sub(height - 1 - margin);
    let last = row.saturating_sub(margin);
    scroll.clamp(first, last)
}

/// The furthest viewport start that keeps the final row within the cursor margins.
pub fn scroll_limit_with_margin(rows: usize, height: usize, scrolloff: usize) -> usize {
    rows.saturating_sub(scroll_margin(height, scrolloff) + 1)
}

fn scroll_margin(height: usize, scrolloff: usize) -> usize {
    scrolloff.min(height.saturating_sub(1) / 2)
}
