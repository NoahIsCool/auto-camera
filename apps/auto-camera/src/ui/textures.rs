//! GPU textures for the monitors and multiview tiles.
//!
//! Snapshots carry pictures as `Arc`-backed RGBA frames. The engine renders
//! monitors and tiles at their own rates and shares a frame between
//! consecutive snapshots until it is re-rendered, so the UI uploads a
//! texture only when the frame's pixel buffer is a *different allocation*
//! from the one last uploaded under that name ([`needs_upload`]). Comparing
//! the `Arc` pointers is exact and free, where comparing sequence numbers
//! would miss a re-crop of the same source frame. Textures of shots that
//! left the catalog are freed by [`Textures::retain`].

use std::collections::HashMap;
use std::sync::Arc;

use ac_media::VideoFrame;
use eframe::egui::{ColorImage, Context, TextureHandle, TextureId, TextureOptions, Vec2, vec2};

/// One uploaded picture.
struct Slot {
    /// The pixel buffer that was uploaded.
    data: Arc<[u8]>,
    /// The texture.
    handle: TextureHandle,
}

/// Named textures, re-uploaded only when their picture changes.
#[derive(Default)]
pub struct Textures {
    /// By name.
    slots: HashMap<String, Slot>,
}

impl std::fmt::Debug for Textures {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Textures")
            .field("count", &self.slots.len())
            .finish()
    }
}

/// True when `frame` is not the buffer `uploaded`.
pub fn needs_upload(uploaded: Option<&Arc<[u8]>>, frame: &VideoFrame) -> bool {
    uploaded.is_none_or(|data| !Arc::ptr_eq(data, &frame.data))
}

/// `frame` as an egui image.
pub fn color_image(frame: &VideoFrame) -> ColorImage {
    ColorImage::from_rgba_unmultiplied([frame.width as usize, frame.height as usize], &frame.data)
}

impl Textures {
    /// The texture named `name` showing `frame`, uploading it if it changed;
    /// `None` without a frame. Returns the texture and its pixel size.
    pub fn show(
        &mut self,
        ctx: &Context,
        name: &str,
        frame: Option<&VideoFrame>,
    ) -> Option<(TextureId, Vec2)> {
        let frame = frame?;
        let size = vec2(frame.width as f32, frame.height as f32);
        match self.slots.get_mut(name) {
            Some(slot) if !needs_upload(Some(&slot.data), frame) => {}
            Some(slot) => {
                slot.handle.set(color_image(frame), TextureOptions::LINEAR);
                slot.data = frame.data.clone();
            }
            None => {
                let handle = ctx.load_texture(name, color_image(frame), TextureOptions::LINEAR);
                self.slots.insert(
                    name.to_owned(),
                    Slot {
                        data: frame.data.clone(),
                        handle,
                    },
                );
            }
        }
        self.slots.get(name).map(|s| (s.handle.id(), size))
    }

    /// Frees every texture whose name fails `keep`.
    pub fn retain(&mut self, keep: impl Fn(&str) -> bool) {
        self.slots.retain(|name, _| keep(name));
    }

    /// Number of textures held.
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.slots.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ac_core::SourceId;
    use std::time::Duration;

    /// A 2×1 opaque grey frame.
    fn frame(grey: u8) -> VideoFrame {
        VideoFrame::new(
            SourceId("s".into()),
            0,
            Duration::ZERO,
            2,
            1,
            vec![grey, grey, grey, 255, grey, grey, grey, 255].into(),
        )
        .unwrap()
    }

    #[test]
    fn uploads_happen_only_for_new_buffers() {
        let a = frame(10);
        assert!(needs_upload(None, &a));
        assert!(!needs_upload(Some(&a.data), &a));
        assert!(!needs_upload(Some(&a.data), &a.clone()), "clones share");
        let b = frame(10);
        assert!(needs_upload(Some(&a.data), &b), "same pixels, new buffer");
    }

    #[test]
    fn frames_convert_to_images_of_their_size() {
        let img = color_image(&frame(200));
        assert_eq!(img.size, [2, 1]);
        assert_eq!(img.pixels[0].r(), 200);
    }

    #[test]
    fn textures_are_reused_and_freed() {
        let ctx = Context::default();
        let mut t = Textures::default();
        assert!(t.show(&ctx, "pgm", None).is_none());
        let a = frame(1);
        let (id, size) = t.show(&ctx, "pgm", Some(&a)).unwrap();
        assert_eq!(size, vec2(2.0, 1.0));
        assert_eq!(t.show(&ctx, "pgm", Some(&a)).unwrap().0, id);
        assert_eq!(
            t.show(&ctx, "pgm", Some(&frame(2))).unwrap().0,
            id,
            "updated in place"
        );
        t.show(&ctx, "tile/a", Some(&a));
        assert_eq!(t.len(), 2);
        t.retain(|n| n == "pgm");
        assert_eq!(t.len(), 1);
    }
}
