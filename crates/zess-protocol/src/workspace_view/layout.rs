use serde::{Deserialize, Serialize};
use zess_plugin::PreviewArea as Rect;

#[derive(Clone, Copy)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

impl Direction {
    pub fn adjacent<T: Copy + PartialEq>(self, root: T, rectangles: &[(T, Rect)]) -> Option<T> {
        let (_, current) = rectangles.iter().find(|(id, _)| *id == root)?;
        let horizontal = matches!(self, Self::Left | Self::Right);
        rectangles
            .iter()
            .filter(|(id, rect)| {
                *id != root
                    && match self {
                        Self::Left => rect.x + rect.width <= current.x,
                        Self::Right => rect.x >= current.x + current.width,
                        Self::Up => rect.y + rect.height <= current.y,
                        Self::Down => rect.y >= current.y + current.height,
                    }
            })
            .min_by_key(|(_, rect)| {
                let gap = match self {
                    Self::Left => current.x - rect.x - rect.width,
                    Self::Right => rect.x - current.x - current.width,
                    Self::Up => current.y - rect.y - rect.height,
                    Self::Down => rect.y - current.y - current.height,
                };
                if horizontal {
                    (
                        gap,
                        (rect.y * 2 + rect.height).abs_diff(current.y * 2 + current.height),
                    )
                } else {
                    (
                        gap,
                        (rect.x * 2 + rect.width).abs_diff(current.x * 2 + current.width),
                    )
                }
            })
            .map(|(id, _)| *id)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Axis {
    Horizontal,
    Vertical,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Layout<T> {
    Leaf(T),
    Split {
        axis: Axis,
        first: Box<Self>,
        second: Box<Self>,
    },
}

impl<T: Copy + PartialEq> Layout<T> {
    fn first(&self) -> T {
        match self {
            Self::Leaf(value) => *value,
            Self::Split { first, .. } => first.first(),
        }
    }

    fn visit_mut(&mut self, visit: &mut impl FnMut(&mut T)) {
        match self {
            Self::Leaf(value) => visit(value),
            Self::Split { first, second, .. } => {
                first.visit_mut(visit);
                second.visit_mut(visit);
            }
        }
    }

    pub fn leaves(&self, leaves: &mut Vec<T>) {
        match self {
            Self::Leaf(id) => leaves.push(*id),
            Self::Split { first, second, .. } => {
                first.leaves(leaves);
                second.leaves(leaves);
            }
        }
    }

    pub fn split(&mut self, target: T, new: T, axis: Axis) -> bool {
        match self {
            Self::Leaf(id) if *id == target => {
                *self = Self::Split {
                    axis,
                    first: Box::new(Self::Leaf(target)),
                    second: Box::new(Self::Leaf(new)),
                };
                true
            }
            Self::Split { first, second, .. } => {
                first.split(target, new, axis) || second.split(target, new, axis)
            }
            _ => false,
        }
    }

    pub fn remove(self, target: T) -> Option<Self> {
        match self {
            Self::Leaf(id) => (id != target).then_some(Self::Leaf(id)),
            Self::Split {
                axis,
                first,
                second,
            } => match (first.remove(target), second.remove(target)) {
                (Some(first), Some(second)) => Some(Self::Split {
                    axis,
                    first: Box::new(first),
                    second: Box::new(second),
                }),
                (Some(other), None) | (None, Some(other)) => Some(other),
                _ => None,
            },
        }
    }

    fn children_on_axis<'a>(&'a self, axis: Axis, out: &mut Vec<&'a Self>) {
        match self {
            Self::Split {
                axis: child_axis,
                first,
                second,
            } if *child_axis == axis => {
                first.children_on_axis(axis, out);
                second.children_on_axis(axis, out);
            }
            _ => out.push(self),
        }
    }

    pub fn page_rectangles(&self, rect: Rect, out: &mut Vec<(T, Rect)>) {
        match self {
            Self::Leaf(id) => out.push((*id, rect)),
            Self::Split { axis, .. } => {
                let mut children = Vec::new();
                self.children_on_axis(*axis, &mut children);
                let count = children.len();
                let gap = usize::from(*axis == Axis::Vertical);
                let extent = match axis {
                    Axis::Vertical => rect.width,
                    Axis::Horizontal => rect.height,
                };
                let mut available = extent.saturating_sub(gap * (count - 1));
                let mut offset = 0;
                for (index, child) in children.into_iter().enumerate() {
                    let size = available / (count - index);
                    let area = match axis {
                        Axis::Vertical => Rect {
                            x: rect.x + offset.min(extent),
                            width: size,
                            ..rect
                        },
                        Axis::Horizontal => Rect {
                            y: rect.y + offset,
                            height: size,
                            ..rect
                        },
                    };
                    child.page_rectangles(area, out);
                    available -= size;
                    offset += size + gap;
                }
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentPane {
    pub view_id: u64,
    pub buffer_id: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DocumentPanes {
    layout: Layout<DocumentPane>,
    focused_view: u64,
    next_view: u64,
}

impl DocumentPanes {
    pub fn new(view_id: u64, buffer_id: u64) -> Self {
        Self {
            layout: Layout::Leaf(DocumentPane { view_id, buffer_id }),
            focused_view: view_id,
            next_view: view_id + 1,
        }
    }

    pub fn panes(&self) -> Vec<DocumentPane> {
        let mut panes = Vec::new();
        self.layout.leaves(&mut panes);
        panes
    }

    pub fn active(&self) -> DocumentPane {
        self.panes()
            .into_iter()
            .find(|pane| pane.view_id == self.focused_view)
            .unwrap_or_else(|| self.layout.first())
    }

    pub fn activate_buffer(&mut self, buffer_id: u64) {
        self.set_buffer(self.focused_view, buffer_id);
    }

    pub fn set_buffer(&mut self, view_id: u64, buffer_id: u64) {
        self.layout.visit_mut(&mut |pane| {
            if pane.view_id == view_id {
                pane.buffer_id = buffer_id;
            }
        });
    }

    pub fn replace_buffer(&mut self, closed: u64, replacement: u64) {
        self.layout.visit_mut(&mut |pane| {
            if pane.buffer_id == closed {
                pane.buffer_id = replacement;
            }
        });
    }

    pub fn split(&mut self, axis: Axis) -> (DocumentPane, DocumentPane) {
        let source = self.active();
        let target = DocumentPane {
            view_id: self.next_view,
            buffer_id: source.buffer_id,
        };
        self.next_view += 1;
        self.layout.split(source, target, axis);
        self.focused_view = target.view_id;
        (source, target)
    }

    pub fn focus(&mut self, view_id: u64) -> bool {
        if !self.panes().iter().any(|pane| pane.view_id == view_id) {
            return false;
        }
        self.focused_view = view_id;
        true
    }

    pub fn focus_next(&mut self) {
        let panes = self.panes();
        let current = panes
            .iter()
            .position(|pane| pane.view_id == self.focused_view)
            .unwrap_or(0);
        self.focused_view = panes[(current + 1) % panes.len()].view_id;
    }

    pub fn close(&mut self, view_id: u64) -> bool {
        let panes = self.panes();
        if panes.len() == 1 {
            return false;
        }
        let Some(current) = panes.iter().position(|pane| pane.view_id == view_id) else {
            return false;
        };
        let active = panes[current];
        let next = panes
            .get(current + 1)
            .unwrap_or(&panes[current.saturating_sub(1)]);
        let layout = std::mem::replace(&mut self.layout, Layout::Leaf(*next));
        self.layout = layout.remove(active).expect("another pane remains");
        if self.focused_view == view_id {
            self.focused_view = next.view_id;
        }
        true
    }

    pub fn rectangles(&self, area: Rect) -> Vec<(DocumentPane, Rect)> {
        let mut rectangles = Vec::new();
        self.layout.page_rectangles(area, &mut rectangles);
        rectangles
    }

    pub fn focus_direction(&mut self, direction: Direction, area: Rect) {
        if let Some(pane) = direction.adjacent(self.active(), &self.rectangles(area)) {
            self.focused_view = pane.view_id;
        }
    }
}
