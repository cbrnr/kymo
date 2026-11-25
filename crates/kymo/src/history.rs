//! Undo and redo of annotation edits and bad channel marks.
//!
//! An edit is recorded as the states of the annotations it changed, before and after, so
//! one step can hold any number of changes (a drag that ends in a merge, say) and undoing
//! or redoing it is setting each annotation to one side's state. Stim annotations are never
//! edited, so they never appear here. Marking channels bad or good is a step of its own,
//! the marks it changed; it doesn't count as an edit of the annotations (see
//! [`History::edited`]).

use kymo_core::{
    AnnotationData, AnnotationError, AnnotationId, AnnotationOp, AnnotationStore, ChannelId,
};

/// One annotation's content before and after an edit; `None` where it doesn't exist.
#[derive(Clone, Debug, PartialEq)]
pub struct Change {
    pub id: AnnotationId,
    pub before: Option<AnnotationData>,
    pub after: Option<AnnotationData>,
}

/// One undoable edit's annotation changes. [`History::push`] keeps each annotation once.
pub type Step = Vec<Change>;

/// A channel's bad mark changed: the mark it got (`true` for bad); it had the other.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Mark {
    pub channel: ChannelId,
    pub bad: bool,
}

/// What an undoable edit changed: annotations, or channels' bad marks.
#[derive(Clone, Debug, PartialEq)]
struct Entry {
    changes: Step,
    marks: Vec<Mark>,
}

impl Entry {
    fn edits_annotations(&self) -> bool {
        !self.changes.is_empty()
    }
}

/// The edits that can be undone and redone, most recent last.
#[derive(Clone, Debug)]
pub struct History {
    undo: Vec<Entry>,
    redo: Vec<Entry>,
    /// How many steps were done at the saved state of the annotations (at first, the
    /// opened one); `None` once no undo or redo can return to it.
    saved: Option<usize>,
}

impl Default for History {
    fn default() -> Self {
        Self {
            undo: Vec::new(),
            redo: Vec::new(),
            saved: Some(0),
        }
    }
}

impl History {
    /// Whether the annotations differ from the saved state, the opened one until
    /// [`mark_saved`](Self::mark_saved): bad marks done or undone since don't count.
    pub fn edited(&self) -> bool {
        let done = self.undo.len();
        match self.saved {
            None => true,
            Some(saved) if saved <= done => self.undo[saved..].iter().any(Entry::edits_annotations),
            // Undone past it: the steps between are the last undone, at the redo stack's top.
            Some(saved) => self.redo[self.redo.len() - (saved - done)..]
                .iter()
                .any(Entry::edits_annotations),
        }
    }

    /// Makes the current state the saved one.
    pub fn mark_saved(&mut self) {
        self.saved = Some(self.undo.len());
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// Records an edit already made to the store. Changes of the same annotation become
    /// one, from its first state to its last; those that leave it as it was are dropped,
    /// and so is a step without any other. A recorded step clears what could be redone.
    /// Returns whether it was recorded.
    pub fn push(&mut self, changes: Step) -> bool {
        let mut step = Step::new();
        for change in changes {
            match step.iter_mut().find(|c| c.id == change.id) {
                Some(c) => c.after = change.after,
                None => step.push(change),
            }
        }
        step.retain(|c| c.before != c.after);
        self.record(Entry {
            changes: step,
            marks: Vec::new(),
        })
    }

    /// Records channels' bad marks already changed in the view as one step (a step without
    /// any is dropped), which clears what could be redone. Returns whether it was recorded.
    pub fn push_marks(&mut self, marks: Vec<Mark>) -> bool {
        self.record(Entry {
            changes: Step::new(),
            marks,
        })
    }

    fn record(&mut self, entry: Entry) -> bool {
        if entry.changes.is_empty() && entry.marks.is_empty() {
            return false;
        }
        // The saved state was undone: with redo gone, it can't come back, unless the steps
        // between left the annotations alone.
        let done = self.undo.len();
        if let Some(saved) = self.saved
            && saved > done
        {
            let lost = &self.redo[self.redo.len() - (saved - done)..];
            self.saved = (!lost.iter().any(Entry::edits_annotations)).then_some(done);
        }
        self.undo.push(entry);
        self.redo.clear();
        true
    }

    /// Sets each annotation of `changes` to its content (`None` removes it) and records
    /// them as one step. On error, the store is left as it was and nothing is recorded.
    pub fn edit(
        &mut self,
        store: &mut AnnotationStore,
        changes: Vec<(AnnotationId, Option<AnnotationData>)>,
    ) -> Result<(), AnnotationError> {
        let mut step = Step::new();
        for (id, after) in changes {
            let before = store.get(id).map(|a| a.data.clone());
            if let Err(error) = set(store, id, after.clone()) {
                restore(store, &step, |c| &c.before);
                return Err(error);
            }
            step.push(Change { id, before, after });
        }
        self.push(step);
        Ok(())
    }

    /// Undoes the last edit, giving bad marks back to `set_bad`; returns the annotations it
    /// changed, or `None` if there was nothing to undo.
    pub fn undo(
        &mut self,
        store: &mut AnnotationStore,
        mut set_bad: impl FnMut(ChannelId, bool),
    ) -> Option<Vec<AnnotationId>> {
        let entry = self.undo.pop()?;
        restore(store, &entry.changes, |c| &c.before);
        for mark in entry.marks.iter().rev() {
            set_bad(mark.channel, !mark.bad);
        }
        let ids = entry.changes.iter().map(|c| c.id).collect();
        self.redo.push(entry);
        Some(ids)
    }

    /// Redoes the last undone edit, giving bad marks to `set_bad`; returns the annotations
    /// it changed, or `None` if there was nothing to redo.
    pub fn redo(
        &mut self,
        store: &mut AnnotationStore,
        mut set_bad: impl FnMut(ChannelId, bool),
    ) -> Option<Vec<AnnotationId>> {
        let entry = self.redo.pop()?;
        restore(store, &entry.changes, |c| &c.after);
        for mark in &entry.marks {
            set_bad(mark.channel, mark.bad);
        }
        let ids = entry.changes.iter().map(|c| c.id).collect();
        self.undo.push(entry);
        Some(ids)
    }
}

/// Sets annotation `id` in `store` to `data`: inserts, replaces or removes it.
pub fn set(
    store: &mut AnnotationStore,
    id: AnnotationId,
    data: Option<AnnotationData>,
) -> Result<(), AnnotationError> {
    let op = match (store.get(id).is_some(), data) {
        (true, Some(data)) => AnnotationOp::Replace(kymo_core::Annotation { id, data }),
        (false, Some(data)) => AnnotationOp::Insert(kymo_core::Annotation { id, data }),
        (true, None) => AnnotationOp::Remove(id),
        (false, None) => return Ok(()),
    };
    store.apply(op).map(|_| ())
}

/// Sets each annotation of `step`, last first, to the side `side` picks: an edit being
/// rolled back may hold an annotation more than once. Those states were in the store
/// before, so they are valid.
fn restore(
    store: &mut AnnotationStore,
    step: &Step,
    side: impl Fn(&Change) -> &Option<AnnotationData>,
) {
    for change in step.iter().rev() {
        set(store, change.id, side(change).clone())
            .expect("the history holds states the store had");
    }
}
