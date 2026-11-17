use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use zess_ui::DisplayRow;

pub fn file_tree_disclosure(expanded: bool) -> &'static str {
    if expanded { "▾ " } else { "▸ " }
}

pub fn file_tree_parent(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(parent, _)| parent)
}

pub fn set_file_tree_expanded(row: &mut DisplayRow, expanded: bool) {
    let previous = file_tree_disclosure(!expanded);
    if row.text.starts_with(previous) {
        row.text
            .replace_range(..previous.len(), file_tree_disclosure(expanded));
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct FileTreeKey {
    pub path: String,
    pub show_ignored: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FileTreeRow {
    pub row: DisplayRow,
    pub directory: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct FileTreeDirectory {
    pub rows: Vec<FileTreeRow>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileTreeListing {
    Closed,
    Loading,
    Ready,
    Failed,
}

fn append_children(
    path: &str,
    expanded: &BTreeSet<String>,
    get: &mut impl FnMut(&str) -> Option<FileTreeDirectory>,
    rows: &mut Vec<DisplayRow>,
) -> Option<()> {
    for mut child in get(path)?.rows {
        let open = child.directory && expanded.contains(&child.row.id);
        if open {
            let path = child.row.id.clone();
            set_file_tree_expanded(&mut child.row, true);
            rows.push(child.row);
            append_children(&path, expanded, get, rows)?;
        } else {
            rows.push(child.row);
        }
    }
    Some(())
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct FileTree {
    pub expanded: BTreeSet<String>,
    pub directories: BTreeMap<String, FileTreeListing>,
    pub show_ignored: bool,
}

impl FileTree {
    pub fn is_loading(&self) -> bool {
        self.directories
            .values()
            .any(|state| *state == FileTreeListing::Loading)
    }

    pub fn directory_rows(&self, rows: &[DisplayRow]) -> BTreeMap<FileTreeKey, FileTreeDirectory> {
        let mut directories = self
            .directories
            .iter()
            .filter(|(_, listing)| **listing == FileTreeListing::Ready)
            .map(|(path, _)| (path.as_str(), FileTreeDirectory::default()))
            .collect::<BTreeMap<_, _>>();
        for row in rows {
            let Some(directory) = directories.get_mut(file_tree_parent(&row.id)) else {
                continue;
            };
            let mut row = FileTreeRow {
                row: row.clone(),
                directory: self.directories.contains_key(&row.id),
            };
            if row.directory {
                set_file_tree_expanded(&mut row.row, false);
            }
            directory.rows.push(row);
        }
        directories
            .into_iter()
            .map(|(path, directory)| {
                (
                    FileTreeKey {
                        path: path.into(),
                        show_ignored: self.show_ignored,
                    },
                    directory,
                )
            })
            .collect()
    }

    pub fn rows(
        &self,
        path: &str,
        mut get: impl FnMut(&str) -> Option<FileTreeDirectory>,
    ) -> Option<Vec<DisplayRow>> {
        let mut rows = Vec::new();
        append_children(
            path,
            &self.expanded,
            &mut |path| {
                if self.directories.get(path) == Some(&FileTreeListing::Failed) {
                    Some(FileTreeDirectory::default())
                } else {
                    get(path)
                }
            },
            &mut rows,
        )?;
        Some(rows)
    }
}
