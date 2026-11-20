//! Channel capability rules shared by the workspace and the room chat.

use crate::Channel;

impl Channel {
    pub fn readable(&self) -> bool {
        self.permissions & 1 != 0
    }
    /// Text history that the channel column always shows. A voice room keeps
    /// its messages behind the room chat panel, so it is excluded here and
    /// enabled through [`Channel::chat_readable`].
    pub fn has_history(&self) -> bool {
        self.chat_readable() && self.kind != "voice"
    }
    /// A timeline this client may read when the surface for it is open. Voice
    /// channels carry messages in the backend exactly like text channels
    /// (`internal/message/sqlite/repository.go`); E2EE rooms keep their history
    /// hidden until a supported decryption path exists.
    pub fn chat_readable(&self) -> bool {
        self.readable()
            && !self.e2ee_required
            && (self.kind == "text" || self.kind == "news" || self.kind == "voice")
    }
    pub fn chat_writable(&self) -> bool {
        self.chat_readable()
            && self.permissions & 2 != 0
            && (self.kind != "news" || self.can_publish)
    }
    pub fn writable(&self) -> bool {
        self.has_history() && self.chat_writable()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permissions() {
        let mut c = Channel {
            kind: "news".into(),
            permissions: 3,
            ..Default::default()
        };
        assert!(c.has_history());
        assert!(!c.writable());
        c.can_publish = true;
        assert!(c.writable());
        c.e2ee_required = true;
        assert!(!c.has_history());
    }

    #[test]
    fn voice_channels_have_a_chat_timeline_separate_from_the_column() {
        let mut c = Channel {
            kind: "voice".into(),
            permissions: 3,
            ..Default::default()
        };
        // The room chat reads the same backend timeline, but the normal
        // channel column must not treat the room as a text channel.
        assert!(!c.has_history());
        assert!(!c.writable());
        assert!(c.chat_readable());
        assert!(c.chat_writable());
        c.permissions = 1;
        assert!(!c.chat_writable(), "Read alone still allows viewing");
        c.permissions = 3;
        c.e2ee_required = true;
        assert!(!c.chat_readable(), "E2EE handling is not weakened");
        assert!(!c.chat_writable());
    }
}
