//! Server-scoped list of screen publications. The list is replaced by
//! `stream.snapshot` and patched by `stream.started/updated/stopped`; it is
//! bounded so a broken server cannot grow it without limit.

use crate::controller::Publication;

/// Go keeps one list per visible voice room; this client watches one channel
/// at a time, so the catalog is bounded to that room.
pub const MAX_PUBLICATIONS: usize = 64;

#[derive(Clone, Debug)]
pub struct StreamCatalog {
    server: i64,
    channel: i64,
    streams: Vec<Publication>,
}

impl StreamCatalog {
    pub fn new(server: i64, channel: i64) -> Self {
        Self {
            server,
            channel,
            streams: Vec::new(),
        }
    }

    pub fn server(&self) -> i64 {
        self.server
    }

    pub fn channel(&self) -> i64 {
        self.channel
    }

    pub fn list(&self) -> &[Publication] {
        &self.streams
    }

    /// A publication is only visible in the selected server and voice room and
    /// must be addressable by its publisher connection.
    pub fn visible(&self, publication: &Publication) -> bool {
        publication.server_id == self.server
            && publication.channel_id == self.channel
            && !publication.publisher_connection_id.is_empty()
            && (publication.mode == "server" || publication.mode == "p2p")
    }

    /// Exact identity lookup. A server may carry several publications for the
    /// same user from different connections, so the connection id is part of
    /// the key and is never replaced by a "same user" fallback.
    pub fn get(
        &self,
        server_id: i64,
        channel_id: i64,
        publisher_connection_id: &str,
    ) -> Option<&Publication> {
        self.streams.iter().find(|publication| {
            publication.server_id == server_id
                && publication.channel_id == channel_id
                && publication.publisher_connection_id == publisher_connection_id
        })
    }

    /// Replace the list with a snapshot. Returns whether the visible list
    /// changed in a way that needs a repaint or a viewer re-check.
    pub fn replace(&mut self, streams: impl IntoIterator<Item = Publication>) -> bool {
        let mut next: Vec<Publication> = Vec::new();
        for publication in streams {
            if !self.visible(&publication) || next.len() >= MAX_PUBLICATIONS {
                continue;
            }
            if next
                .iter()
                .any(|old| old.publisher_connection_id == publication.publisher_connection_id)
            {
                continue;
            }
            next.push(publication);
        }
        let changed = !same_list(&self.streams, &next);
        self.streams = next;
        changed
    }

    /// `stream.started`/`stream.updated`: replace the entry with the same
    /// publisher connection in place, otherwise append.
    pub fn upsert(&mut self, publication: Publication) -> bool {
        if !self.visible(&publication) {
            return false;
        }
        if let Some(existing) = self
            .streams
            .iter_mut()
            .find(|old| old.publisher_connection_id == publication.publisher_connection_id)
        {
            if *existing == publication {
                return false;
            }
            *existing = publication;
            return true;
        }
        if self.streams.len() >= MAX_PUBLICATIONS {
            return false;
        }
        self.streams.push(publication);
        true
    }

    /// `stream.stopped`: an empty connection id means every publication in the
    /// channel. A stale stop for an unknown publisher changes nothing.
    pub fn stop(&mut self, server_id: i64, channel_id: i64, publisher: &str) -> bool {
        if server_id != self.server || channel_id != self.channel {
            return false;
        }
        let before = self.streams.len();
        self.streams.retain(|publication| {
            if publisher.is_empty() {
                false
            } else {
                publication.publisher_connection_id != publisher
            }
        });
        before != self.streams.len()
    }
}

fn same_list(left: &[Publication], right: &[Publication]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .all(|(a, b)| a.publisher_connection_id == b.publisher_connection_id && a == b)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn publication(connection: &str, user: i64) -> Publication {
        Publication {
            server_id: 1,
            channel_id: 10,
            publisher_user_id: user,
            publisher_connection_id: connection.into(),
            mode: "server".into(),
            codec: "vp8".into(),
            has_audio: false,
            viewer_count: 1,
            started_at_ms: 1,
        }
    }

    #[test]
    fn snapshot_replaces_and_bounds_the_list() {
        let mut catalog = StreamCatalog::new(1, 10);
        assert!(catalog.replace([publication("a", 1), publication("b", 2)]));
        assert_eq!(catalog.list().len(), 2);
        // Same snapshot again is not a change.
        assert!(!catalog.replace([publication("a", 1), publication("b", 2)]));
        // Foreign rooms and duplicates are dropped.
        let mut foreign = publication("c", 3);
        foreign.channel_id = 11;
        assert!(catalog.replace([publication("a", 1), foreign, publication("a", 1)]));
        assert_eq!(catalog.list().len(), 1);
    }

    #[test]
    fn a_stale_stop_keeps_the_other_publisher() {
        let mut catalog = StreamCatalog::new(1, 10);
        catalog.replace([publication("current", 3), publication("other", 4)]);
        assert!(!catalog.stop(1, 10, "old"));
        assert_eq!(catalog.list().len(), 2);
        assert!(catalog.stop(1, 10, "current"));
        assert_eq!(catalog.list().len(), 1);
        assert_eq!(catalog.list()[0].publisher_connection_id, "other");
        // A stop from another room never touches this catalog.
        assert!(!catalog.stop(1, 11, "other"));
        assert!(catalog.stop(1, 10, ""));
        assert!(catalog.list().is_empty());
    }

    #[test]
    fn an_update_replaces_in_place_and_keeps_order() {
        let mut catalog = StreamCatalog::new(1, 10);
        catalog.replace([publication("a", 1), publication("b", 2)]);
        let mut updated = publication("a", 1);
        updated.viewer_count = 7;
        assert!(catalog.upsert(updated));
        assert_eq!(catalog.list()[0].viewer_count, 7);
        assert_eq!(catalog.list()[1].publisher_connection_id, "b");
        let current = catalog.list()[0].clone();
        assert!(!catalog.upsert(current));
    }

    #[test]
    fn a_republication_gets_its_own_entry_and_an_unknown_connection_is_not_retargeted() {
        let mut catalog = StreamCatalog::new(1, 10);
        catalog.replace([publication("old", 3)]);
        assert!(catalog.upsert(publication("new", 3)));
        assert_eq!(catalog.list().len(), 2);
        assert_eq!(
            catalog.get(1, 10, "old").unwrap().publisher_connection_id,
            "old"
        );
        assert!(
            catalog.get(1, 10, "gone").is_none(),
            "another connection of the same user must never be substituted"
        );
    }
}
