const MAX_UNDO: usize = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryStep {
    Undo,
    Redo,
}

#[derive(Debug, Clone)]
pub struct SnapshotHistory<S> {
    current: Option<S>,
    undo: Vec<S>,
    redo: Vec<S>,
}

impl<S> Default for SnapshotHistory<S> {
    fn default() -> Self {
        Self {
            current: None,
            undo: Vec::new(),
            redo: Vec::new(),
        }
    }
}

impl<S: Clone + PartialEq> SnapshotHistory<S> {
    pub fn observe(&mut self, now: &S) {
        if self.current.as_ref() == Some(now) {
            return;
        }
        if let Some(previous) = self.current.replace(now.clone()) {
            self.undo.push(previous);
            if self.undo.len() > MAX_UNDO {
                self.undo.remove(0);
            }
            self.redo.clear();
        }
    }

    pub fn step(&mut self, step: HistoryStep) -> Option<S> {
        let (from, to) = match step {
            HistoryStep::Undo => (&mut self.undo, &mut self.redo),
            HistoryStep::Redo => (&mut self.redo, &mut self.undo),
        };
        let target = from.pop()?;
        let left = self
            .current
            .replace(target.clone())
            .expect("a stepped-over snapshot implies a current one");
        to.push(left);
        Some(target)
    }

    pub fn clear_redo(&mut self) {
        self.redo.clear();
    }

    pub fn newest_first(&self) -> impl Iterator<Item = &S> {
        self.current.iter().chain(self.undo.iter().rev())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use HistoryStep::{Redo, Undo};

    fn observed(states: &[i32]) -> SnapshotHistory<i32> {
        let mut history = SnapshotHistory::default();
        for state in states {
            history.observe(state);
        }
        history
    }

    #[test]
    fn observe_records_changes_only() {
        let mut history = observed(&[1, 1, 2, 2, 2, 3]);
        assert_eq!(history.step(Undo), Some(2));
        assert_eq!(history.step(Undo), Some(1));
        assert_eq!(history.step(Undo), None);
    }

    #[test]
    fn undo_restores_previous_and_redo_returns() {
        let mut history = observed(&[1, 2, 3]);
        assert_eq!(history.step(Undo), Some(2));
        assert_eq!(history.step(Undo), Some(1));
        assert_eq!(history.step(Redo), Some(2));
        assert_eq!(history.step(Redo), Some(3));
        assert_eq!(history.step(Redo), None);
        assert_eq!(history.step(Undo), Some(2));
    }

    #[test]
    fn new_change_after_undo_clears_redo() {
        let mut history = observed(&[1, 2, 3]);
        assert_eq!(history.step(Undo), Some(2));
        history.observe(&4);
        assert_eq!(history.step(Redo), None);
        assert_eq!(history.step(Undo), Some(2));
        assert_eq!(history.step(Undo), Some(1));
    }

    #[test]
    fn applying_a_step_then_observing_it_records_nothing() {
        let mut history = observed(&[1, 2, 3]);
        let undone = history.step(Undo).unwrap();
        history.observe(&undone);
        assert_eq!(history.step(Redo), Some(3));
        history.observe(&3);
        assert_eq!(history.step(Undo), Some(2));
        assert_eq!(history.step(Undo), Some(1));
    }

    #[test]
    fn undo_and_redo_on_empty_return_none() {
        let mut history = SnapshotHistory::<i32>::default();
        assert_eq!(history.step(Undo), None);
        assert_eq!(history.step(Redo), None);
        history.observe(&1);
        assert_eq!(history.step(Undo), None);
        assert_eq!(history.step(Redo), None);
    }

    #[test]
    fn cap_drops_the_oldest_changes() {
        let states: Vec<i32> = (0..=MAX_UNDO as i32 + 5).collect();
        let mut history = observed(&states);
        let mut undone = Vec::new();
        while let Some(state) = history.step(Undo) {
            undone.push(state);
        }
        assert_eq!(undone.len(), MAX_UNDO);
        assert_eq!(undone.last(), Some(&5));
    }

    #[test]
    fn clear_redo_keeps_undo() {
        let mut history = observed(&[1, 2, 3]);
        assert_eq!(history.step(Undo), Some(2));
        history.clear_redo();
        assert_eq!(history.step(Redo), None);
        assert_eq!(history.step(Undo), Some(1));
    }

    #[test]
    fn newest_first_lists_the_current_then_the_undo_stack() {
        let mut history = observed(&[1, 2, 3]);
        assert_eq!(
            history.newest_first().copied().collect::<Vec<_>>(),
            [3, 2, 1]
        );
        history.step(Undo);
        assert_eq!(history.newest_first().copied().collect::<Vec<_>>(), [2, 1]);
        assert_eq!(SnapshotHistory::<i32>::default().newest_first().count(), 0);
    }
}
