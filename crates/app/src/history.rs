//! Undo / Redo。各世代は Project の snapshot で、100 件または 256 MiB を超えた古い世代から捨てる。

use mvm_project::bytes::approximate_project_bytes;
use mvm_project::{Project, edit_history_entries_to_drop};

const MAX_ENTRIES: usize = 100;
const MAX_BYTES: usize = 256 * 1024 * 1024;

/// 世代と、その大きさの見積もり。見積もりは積んだときに 1 回だけ数える。
#[derive(Clone, Debug)]
struct Entry {
    project: Project,
    bytes: usize,
}

impl Entry {
    fn new(project: Project) -> Self {
        let bytes = approximate_project_bytes(&project);
        Self { project, bytes }
    }

    fn into_project(self) -> Project {
        self.project
    }
}

#[derive(Clone, Debug, Default)]
pub struct History {
    undo: Vec<Entry>,
    redo: Vec<Entry>,
}

impl History {
    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    #[cfg(test)]
    fn undo_len(&self) -> usize {
        self.undo.len()
    }

    pub fn clear(&mut self) {
        self.undo.clear();
        self.redo.clear();
    }

    pub fn push_undo(&mut self, project: Project) {
        self.undo.push(Entry::new(project));
        self.redo.clear();
        self.trim();
    }

    pub fn undo(&mut self, current: Project) -> Option<Project> {
        let previous = self.undo.pop()?;
        self.redo.push(Entry::new(current));
        self.trim();
        Some(previous.into_project())
    }

    pub fn redo(&mut self, current: Project) -> Option<Project> {
        let next = self.redo.pop()?;
        self.undo.push(Entry::new(current));
        self.trim();
        Some(next.into_project())
    }

    fn trim(&mut self) {
        let undo_sizes: Vec<usize> = self.undo.iter().map(|entry| entry.bytes).collect();
        let redo_sizes: Vec<usize> = self.redo.iter().map(|entry| entry.bytes).collect();
        let drop = edit_history_entries_to_drop(&undo_sizes, &redo_sizes, MAX_ENTRIES, MAX_BYTES);
        let undo_drop = drop.undo.min(self.undo.len());
        let redo_drop = drop.redo.min(self.redo.len());
        self.undo.drain(0..undo_drop);
        self.redo.drain(0..redo_drop);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn undo_and_redo_round_trip_and_a_new_edit_drops_the_redo() {
        let mut history = History::default();
        let first = Project::default();
        let mut second = first.clone();
        second.output_width = 1280;
        let mut third = second.clone();
        third.output_width = 640;
        history.push_undo(first.clone());
        history.push_undo(second.clone());
        let back = history.undo(third.clone()).expect("undo");
        assert_eq!(back.output_width, 1280);
        let again = history.redo(back).expect("redo");
        assert_eq!(again.output_width, 640);
        history.push_undo(again);
        assert!(!history.can_redo());
    }

    #[test]
    fn the_entry_budget_drops_the_oldest_undo_and_keeps_the_latest() {
        let mut history = History::default();
        for width in 0..120 {
            let project = Project {
                output_width: width,
                ..Project::default()
            };
            history.push_undo(project);
        }
        assert!(history.undo_len() <= 100);
        assert!(history.undo_len() >= 1);
        let latest = history.undo(Project::default()).expect("最新");
        assert_eq!(latest.output_width, 119);
    }
}
