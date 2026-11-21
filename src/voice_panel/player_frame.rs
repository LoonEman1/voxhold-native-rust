//! Picture slot of the incoming stream player.
//!
//! The slot is swapped in the poll/update path before the next repaint, and
//! `Window::drop_image` removes the sprite-atlas entry regardless of other
//! `Arc` owners. The order below therefore means "hand over the slot, then
//! release the old picture", not "keep the old texture alive through its
//! `Arc`": safety comes from the update running before the repaint.

use super::*;

impl VoicePanel {
    /// Release the incoming player picture, if one is held. The slot is empty
    /// before `drop_image` runs, which is what the next repaint must observe.
    pub(super) fn clear_player_frame(&mut self, window: &mut Window) {
        if let Some(previous) = self.frame.take() {
            self.release_player_image(window, previous);
        }
    }

    /// Install the newest decoded frame and release the previous one only
    /// after the slot already points at the new picture.
    pub(super) fn set_player_frame(&mut self, window: &mut Window, image: Arc<RenderImage>) {
        let previous = self.frame.replace(image);
        if let Some(previous) = previous {
            self.release_player_image(window, previous);
        }
    }

    fn release_player_image(&mut self, window: &mut Window, image: Arc<RenderImage>) {
        let _ = window.drop_image(image);
        #[cfg(test)]
        {
            // Read at the exact moment of the drop, so a test observes the
            // slot the renderer would see, not just the order of two calls.
            self.last_release_slot_image = self.frame.as_ref().map(|image| image.id.0 as u64);
            self.player_images_released = self.player_images_released.saturating_add(1);
        }
    }
}
