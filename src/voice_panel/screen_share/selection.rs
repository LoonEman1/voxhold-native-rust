//! Selection identity of the screen picker.
//!
//! A monitor row is matched by its Windows display identity, not by the
//! capturer number of the current list. A reorder keeps the same choice; a
//! number reused by another display, a missing display or an ambiguous match
//! clears the choice and its preview and demands a fresh selection.

use super::*;

impl ScreenShareUi {
    /// Resolve an enumeration key in its snapshot; a duplicate id resolves to
    /// `None`.
    pub(crate) fn source_for(&self, id: SourceId) -> Option<&ScreenSource> {
        let mut found = self
            .snapshot(id.kind)
            .iter()
            .filter(|item| item.id == id.id);
        let source = found.next()?;
        found.next().is_none().then_some(source)
    }

    /// Resolve a stable identity in its snapshot. Only one row may claim it.
    pub(crate) fn source_for_identity(&self, identity: &SourceIdentity) -> Option<&ScreenSource> {
        let mut found = self
            .snapshot(identity_kind(identity))
            .iter()
            .filter(|source| source.identity().as_ref() == Some(identity));
        let source = found.next()?;
        found.next().is_none().then_some(source)
    }

    /// The selected source as the current snapshot describes it.
    #[cfg(test)]
    pub(crate) fn selected_source(&self) -> Option<&ScreenSource> {
        self.selected.and_then(|id| self.source_for(id))
    }

    /// Whether the selection is in the visible category and still names the
    /// surface the user chose.
    pub(super) fn selection_is_current(&self) -> bool {
        let Some(id) = self.selected else {
            return false;
        };
        if id.kind != self.kind {
            return false;
        }
        let Some(source) = self.source_for(id) else {
            return false;
        };
        match self.selected_identity.as_ref() {
            Some(identity) => source.identity().as_ref() == Some(identity),
            None => true,
        }
    }

    /// Whether `source` is the exact surface this dialog selected.
    pub(super) fn selection_matches(&self, source: &ScreenSource) -> bool {
        match self.selected_identity.as_ref() {
            Some(identity) => source.identity().as_ref() == Some(identity),
            None => self
                .selected
                .is_some_and(|id| id == source.source_id() && source.identity().is_none()),
        }
    }

    /// Drop or re-bind a selection when a new snapshot arrives. A restart is
    /// bound to the accepted source, so a background answer never moves it.
    pub(super) fn retain_selection(&mut self, kind: SourceKind, sources: &[ScreenSource]) {
        if self.restart {
            return;
        }
        let Some(id) = self.selected else { return };
        if id.kind != kind {
            return;
        }
        // A monitor is retained through the display identity it was chosen
        // with. A window keeps its `HWND` through `SourceId::id`.
        let identity = self
            .selected_identity
            .clone()
            .or_else(|| self.source_for(id).and_then(ScreenSource::identity));
        let Some(identity) = identity else {
            if sources.iter().filter(|item| item.id == id.id).count() != 1 {
                self.selected = None;
                self.selected_identity = None;
                self.error = SOURCE_GONE_REASON.into();
            }
            return;
        };
        let mut matches = sources
            .iter()
            .filter(|source| source.identity().as_ref() == Some(&identity));
        let Some(next) = matches.next() else {
            self.selected = None;
            self.selected_identity = None;
            self.error = SOURCE_GONE_REASON.into();
            return;
        };
        if matches.next().is_some() {
            self.selected = None;
            self.selected_identity = None;
            self.error = SOURCE_CHANGED_REASON.into();
            return;
        }
        self.selected = Some(next.source_id());
        self.selected_identity = Some(identity);
        if self.error == SOURCE_CHANGED_REASON {
            self.error.clear();
        }
    }
}

pub(super) fn identity_kind(identity: &SourceIdentity) -> SourceKind {
    match identity {
        SourceIdentity::Monitor(_) => SourceKind::Monitor,
        SourceIdentity::Window(_) => SourceKind::Window,
    }
}
