//! Signal Desktop's photo editor (`ts/components/AvatarEditor.dom.tsx`), as
//! the profile editor and a new group use it: the photo as it will be, a
//! button to upload one (cropped square as Desktop uploads it), one to
//! remove it, and Desktop's presets for a person or a group.
//!
//! Desktop's editor also makes text photos (`AvatarTextEditor`) and keeps
//! the photos made with it (`userAvatarData`). Neither is here: drawing
//! text needs Desktop's Inter and an emoji font in the SVG renderer, which
//! Vernam builds without text, and keeping them needs storage per account.

use std::sync::Arc;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{Sizable as _, WindowExt as _};
use gpui_kit::prelude::*;
use gpui_kit::{AnyElement, Context, Image, ImageFormat, Window, div, img, px};

use crate::avatar_presets::Preset;
use crate::theme::Palette;

/// The photo as edited.
#[derive(Clone)]
pub enum Staged {
    /// What there is now.
    Current,
    Remove,
    /// A processed JPEG, with its image for the preview.
    New(Arc<Vec<u8>>, Arc<Image>),
}

impl Staged {
    /// A picked or preset image, cropped and saved as Desktop uploads it
    /// (`processImageFile`); `Err` says why it can't be used.
    pub fn from_image(bytes: &[u8]) -> Result<Self, String> {
        let jpeg = signal_backend::profiles::avatar_image(bytes).map_err(|_| "it isn\u{2019}t an image Vernam can read.".to_owned())?;
        let image = Arc::new(Image::from_bytes(ImageFormat::Jpeg, jpeg.clone()));
        Ok(Self::New(Arc::new(jpeg), image))
    }

    /// The preset `preset` as drawn by Desktop's `avatarDataToBytes`.
    pub fn from_preset(preset: &Preset) -> Result<Self, String> {
        let png = crate::avatar_presets::render(preset).map_err(|e| e.to_string())?;
        Self::from_image(&png)
    }
}

/// The largest photo file read. Vernam's other images read whole
/// (release-note media, GIFs) are 25 MiB; a photo shrunk to 512 pixels
/// needs no more.
pub const PHOTO_MAX_BYTES: u64 = 25 * 1024 * 1024;

/// A picked photo file, read and processed (to run off the UI thread: a
/// large file would stall it); `Err` says why it can't be used.
pub fn read_photo(path: &std::path::Path) -> Result<Staged, String> {
    let size = std::fs::metadata(path).map_err(|e| format!("it couldn\u{2019}t be read ({e})."))?.len();
    if size > PHOTO_MAX_BYTES {
        return Err("it\u{2019}s larger than 25 MB.".into());
    }
    let bytes = std::fs::read(path).map_err(|e| format!("it couldn\u{2019}t be read ({e})."))?;
    Staged::from_image(&bytes)
}

/// Asks for a photo file, reads it off the UI thread, and hands the result
/// to `done` (Desktop's `AvatarUploadButton`).
pub fn pick_photo<V: 'static>(
    prompt: &'static str,
    window: &mut Window,
    cx: &mut Context<V>,
    done: impl FnOnce(&mut V, Result<Staged, String>, &mut Window, &mut Context<V>) + 'static,
) {
    let picked = cx.prompt_for_paths(gpui_kit::PathPromptOptions { files: true, directories: false, multiple: false, prompt: Some(prompt.into()) });
    cx.spawn_in(window, async move |this, cx| {
        let Ok(Ok(Some(paths))) = picked.await else { return };
        let Some(path) = paths.into_iter().next() else { return };
        let read = cx.background_executor().spawn(async move { read_photo(&path) }).await;
        let _ = this.update_in(cx, |this, window, cx| done(this, read, window, cx));
    })
    .detach();
}

/// The editor's body: `preview` (the photo as staged), "Upload photo",
/// "Remove" when `has_photo`, then "Select an avatar" over the presets.
/// `id` prefixes the elements' ids and selectors.
#[allow(clippy::too_many_arguments)]
pub fn body<V: 'static>(
    id: &'static str,
    preview: AnyElement,
    has_photo: bool,
    presets: &'static [Preset],
    palette: Palette,
    cx: &mut Context<V>,
    on_upload: impl Fn(&mut V, &mut Window, &mut Context<V>) + 'static,
    on_remove: impl Fn(&mut V, &mut Window, &mut Context<V>) + 'static,
    on_preset: impl Fn(&mut V, usize, &mut Window, &mut Context<V>) + 'static,
) -> AnyElement {
    let on_preset = std::rc::Rc::new(on_preset);
    let presets = presets.iter().enumerate().map(|(ix, preset)| {
        let (bg, _) = crate::theme::avatar_colors(preset.color);
        let image = Arc::new(Image::from_bytes(ImageFormat::Svg, preset.svg.to_vec()));
        let on_preset = on_preset.clone();
        div()
            .id((gpui_kit::SharedString::from(format!("{id}-preset")), ix))
            .debug_selector(move || format!("{id}-preset-{ix}"))
            .size(px(56.))
            .rounded_full()
            .overflow_hidden()
            .bg(bg)
            .cursor_pointer()
            .child(img(image).size(px(56.)))
            .on_click(cx.listener(move |this, _, window, cx| on_preset(this, ix, window, cx)))
    });
    div()
        .flex()
        .flex_col()
        .gap_3()
        .child(div().flex().justify_center().child(preview))
        .child(
            div()
                .flex()
                .justify_center()
                .gap_2()
                .child(
                    Button::new(gpui_kit::SharedString::from(format!("{id}-upload-photo")))
                        .label("Upload photo")
                        .outline()
                        .small()
                        .on_click(cx.listener(move |this, _, window, cx| on_upload(this, window, cx))),
                )
                .when(has_photo, |d| {
                    d.child(
                        Button::new(gpui_kit::SharedString::from(format!("{id}-remove-photo")))
                            .label("Remove")
                            .ghost()
                            .small()
                            .on_click(cx.listener(move |this, _, window, cx| on_remove(this, window, cx))),
                    )
                }),
        )
        .child(div().text_xs().text_color(palette.text_secondary).child("Select an avatar"))
        .child(div().flex().flex_wrap().gap_2().children(presets))
        .into_any_element()
}

/// A photo as saved from the editor: its JPEG and its preview image.
pub type Photo = (Arc<Vec<u8>>, Arc<Image>);

/// The editor in a dialog of its own (Desktop's `AvatarEditor` with
/// `isInsideDialog`, as "Group Avatar" opens it): Cancel, and Save once the
/// photo differs from `current` (`AvatarModalButtons`' `hasChanges`).
pub struct DialogEditor {
    current: Option<Photo>,
    staged: Staged,
    presets: &'static [Preset],
    /// Shown with no photo (the group icon, initials).
    placeholder: std::rc::Rc<dyn Fn() -> AnyElement>,
}

impl DialogEditor {
    /// The photo Save would give: `None` for no photo.
    fn result(&self) -> Option<Photo> {
        match &self.staged {
            Staged::Current => self.current.clone(),
            Staged::Remove => None,
            Staged::New(jpeg, image) => Some((jpeg.clone(), image.clone())),
        }
    }

    fn changed(&self) -> bool {
        !matches!(self.staged, Staged::Current) && self.result().map(|(jpeg, _)| jpeg) != self.current.as_ref().map(|(jpeg, _)| jpeg.clone())
    }

    fn stage(&mut self, photo: Result<Staged, String>, window: &mut Window, cx: &mut Context<Self>) {
        match photo {
            Ok(photo) => self.staged = photo,
            Err(reason) => {
                let text = format!("That file can\u{2019}t be used as a photo: {reason}");
                window.open_alert_dialog(cx, move |alert, _, _| alert.description(text.clone()));
            }
        }
        cx.notify();
    }

    fn preview(&self, size: f32) -> AnyElement {
        match self.result() {
            Some((_, image)) => img(image).size(px(size)).aspect_square().rounded_full().object_fit(gpui_kit::ObjectFit::Cover).into_any_element(),
            None => (self.placeholder)(),
        }
    }
}

impl Render for DialogEditor {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = Palette::get(cx);
        let has_photo = self.result().is_some();
        body(
            "avatar-editor",
            self.preview(96.),
            has_photo,
            self.presets,
            palette,
            cx,
            |_, window, cx| pick_photo("Choose photo", window, cx, |this: &mut Self, photo, window, cx| this.stage(photo, window, cx)),
            |this, _, cx| {
                this.staged = Staged::Remove;
                cx.notify();
            },
            |this, ix, window, cx| {
                let photo = Staged::from_preset(&this.presets[ix]);
                this.stage(photo, window, cx);
            },
        )
    }
}

/// Opens the editor in a dialog titled `title`; Save hands the photo (or
/// none) to `on_save`.
pub fn open_dialog(
    title: &'static str,
    current: Option<Photo>,
    presets: &'static [Preset],
    placeholder: impl Fn() -> AnyElement + 'static,
    on_save: impl Fn(Option<Photo>, &mut Window, &mut gpui_kit::App) + 'static,
    window: &mut Window,
    cx: &mut gpui_kit::App,
) -> gpui_kit::Entity<DialogEditor> {
    use gpui_kit::component::dialog::DialogFooter;
    use gpui_kit::component::Disableable as _;
    let editor = cx.new(|_| DialogEditor { current, staged: Staged::Current, presets, placeholder: std::rc::Rc::new(placeholder) });
    let on_save = std::rc::Rc::new(on_save);
    let shown = editor.clone();
    window.open_dialog(cx, move |dialog, _, cx| {
        let changed = shown.read(cx).changed();
        let (save, on_save) = (shown.clone(), on_save.clone());
        dialog.title(title).w(px(440.)).child(shown.clone()).footer(
            DialogFooter::new()
                .child(Button::new("avatar-editor-cancel").label("Cancel").outline().on_click(|_, window, cx| window.close_dialog(cx)))
                .child(Button::new("avatar-editor-save").label("Save").primary().disabled(!changed).on_click(move |_, window, cx| {
                    let photo = save.read(cx).result();
                    on_save(photo, window, cx);
                    window.close_dialog(cx);
                })),
        )
    });
    editor
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_photo_must_be_an_image_of_at_most_25_mb() {
        let dir = tempfile::tempdir().unwrap();
        let not_image = dir.path().join("notes.txt");
        std::fs::write(&not_image, b"not a photo").unwrap();
        assert!(read_photo(&not_image).is_err());
        let photo = dir.path().join("photo.png");
        image::RgbaImage::from_pixel(8, 8, image::Rgba([200, 80, 40, 255])).save(&photo).unwrap();
        assert!(matches!(read_photo(&photo), Ok(Staged::New(jpeg, _)) if jpeg.starts_with(&[0xff, 0xd8])));
        let large = dir.path().join("large.png");
        let file = std::fs::File::create(&large).unwrap();
        file.set_len(PHOTO_MAX_BYTES + 1).unwrap();
        assert!(read_photo(&large).is_err_and(|reason| reason.contains("25 MB")));
        assert!(read_photo(&dir.path().join("missing.png")).is_err());
    }

    #[test]
    fn presets_make_photos() {
        for preset in crate::avatar_presets::PRESETS.iter().chain(&crate::avatar_presets::GROUP_PRESETS) {
            assert!(matches!(Staged::from_preset(preset), Ok(Staged::New(..))));
        }
    }
}
