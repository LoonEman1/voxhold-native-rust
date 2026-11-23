//! Screen request seams of the panel tests.
//!
//! The request is built from the same selection, quality and codec helpers
//! production Start uses, so a test never describes a payload the app cannot
//! publish.

use super::*;

impl VoicePanel {
    /// The quality/audio fields the picker will send.
    pub fn test_screen_request(&self) -> voxhold_media::publisher::PublishRequest {
        // Mirror production: only a restart dialog is locked to the confirmed
        // live source. An ordinary picker sends the row it selected in the
        // current snapshot, so a `confirmed` left over from the previous
        // publication can never answer for it.
        let source = if self.screen.restart {
            self.screen.restart_source().cloned()
        } else {
            self.screen.selected_source().cloned()
        }
        .unwrap_or(voxhold_media::screen_capture::ScreenSource {
            id: 0,
            kind: voxhold_media::screen_capture::SourceKind::Monitor,
            title: String::new(),
            display_id: -1,
            width: 0,
            height: 0,
            monitor: None,
        });
        let quality = self.screen.quality_for_source(&source);
        let mut request = voxhold_media::publisher::PublishRequest {
            source,
            quality,
            // A failed resolution is a fixture bug: production refuses to
            // publish instead of substituting another codec, and so must a
            // test that describes the same request.
            codec: self
                .codec_resolution()
                .expect("the fixture codec must resolve"),
            audio: self.screen.audio_requested(),
            audio_scope: self.screen.audio_scope,
        };
        // Mirror the production `commit_screen_share`: the selected identity
        // decides the kind, so a stale window-only choice for a monitor (or the
        // reverse) cannot slip through the test seam. The fallback request uses
        // its own source kind when no row is selected.
        let kind = self
            .screen
            .selected
            .map(|id| id.kind)
            .unwrap_or(request.source.kind);
        request.audio_scope = self.screen.effective_audio_scope(kind);
        request
    }

    /// The codec intent the dialog shows: Auto on a fresh picker, the concrete
    /// confirmed codec on a restart.
    pub fn test_screen_codec_intent(&self) -> CodecIntent {
        self.screen.codec
    }

    /// Replace both the visible intent and the resolved codec of the dialog, so
    /// a classification test can name any codec regardless of the host build.
    pub fn test_set_screen_codec(&mut self, codec: voxhold_media::publisher::Codec) {
        self.screen.codec = CodecIntent::Explicit(codec);
        self.screen.test_codec = Some(codec);
    }

    /// Model an unavailable codec without depending on the host build's probe,
    /// so the dialog's refusal line is paintable in a UI test.
    pub fn test_set_codec_error(&mut self, reason: &str) {
        self.test_codec_error = Some(reason.to_owned());
    }

    /// Replace the sender availability matrix of the dialog. Every codec in
    /// `entries` gets the injected state (`None` is available); a UI test can
    /// paint a greyed item, its reason and the Auto answer deterministically.
    pub fn test_set_screen_codec_availability(
        &mut self,
        entries: Vec<(voxhold_media::publisher::Codec, Option<String>)>,
    ) {
        self.screen.test_codec_availability = Some(entries);
    }

    /// The codec the live attempt really confirmed on air, never the dialog's
    /// current desire.
    pub fn test_screen_confirmed_codec(&self) -> Option<voxhold_media::publisher::Codec> {
        self.screen
            .sharing
            .confirmed
            .as_ref()
            .map(|confirmed| confirmed.codec)
    }
}
