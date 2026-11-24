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
        // Its own reason ("not an image", "the image is too small").
        let jpeg = signal_backend::profiles::avatar_image(bytes).map_err(|e| format!("{e}."))?;
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

/// Which pick or preset an editor is waiting on: each one (and Remove)
/// takes a new number, and a result for an older one is dropped, so a
/// slow photo or preset can't land over one picked after it.
#[derive(Debug, Default)]
pub struct Latest(u64);

impl Latest {
    /// A new request's number; earlier ones are now stale.
    pub fn next(&mut self) -> u64 {
        self.0 += 1;
        self.0
    }

    /// Whether `request` is still the latest.
    pub fn is(&self, request: u64) -> bool {
        self.0 == request
    }
}

/// A picked photo file, read and processed (to run off the UI thread: a
/// large file would stall it); `Err` says why it can't be used.
pub fn read_photo(path: &std::path::Path) -> Result<Staged, String> {
    use std::io::Read as _;
    let unreadable = |e: std::io::Error| format!("it couldn\u{2019}t be read ({e}).");
    // Not a pipe or a device, which would block or never end: checked
    // before opening (opening a named pipe waits for a writer), and on the
    // file opened, should it have been swapped in between.
    if !std::fs::metadata(path).map_err(unreadable)?.is_file() {
        return Err("it isn\u{2019}t a file.".into());
    }
    let file = std::fs::File::open(path).map_err(unreadable)?;
    let metadata = file.metadata().map_err(unreadable)?;
    if !metadata.is_file() {
        return Err("it isn\u{2019}t a file.".into());
    }
    let too_large = || "it\u{2019}s larger than 25 MB.".to_owned();
    if metadata.len() > PHOTO_MAX_BYTES {
        return Err(too_large());
    }
    // The cap holds while reading too, should the file grow.
    let mut bytes = Vec::new();
    file.take(PHOTO_MAX_BYTES + 1).read_to_end(&mut bytes).map_err(unreadable)?;
    if bytes.len() as u64 > PHOTO_MAX_BYTES {
        return Err(too_large());
    }
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

/// Draws `preset` off the UI thread (a 1024-pixel render, scaled and saved
/// as a JPEG) and hands the result to `done`.
pub fn stage_preset<V: 'static>(
    preset: &'static Preset,
    window: &mut Window,
    cx: &mut Context<V>,
    done: impl FnOnce(&mut V, Result<Staged, String>, &mut Window, &mut Context<V>) + 'static,
) {
    cx.spawn_in(window, async move |this, cx| {
        let drawn = cx.background_executor().spawn(async move { Staged::from_preset(preset) }).await;
        let _ = this.update_in(cx, |this, window, cx| done(this, drawn, window, cx));
    })
    .detach();
}

/// The editor's body: `preview` (the photo as staged), "Upload photo",
/// "Remove" when `has_photo`, then "Select an avatar" over the presets,
/// `selected` ringed (Desktop's `BetterAvatar` `isSelected`). `id`
/// prefixes the elements' ids and selectors.
#[allow(clippy::too_many_arguments)]
pub fn body<V: 'static>(
    id: &'static str,
    preview: AnyElement,
    has_photo: bool,
    presets: &'static [Preset],
    selected: Option<usize>,
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
        let ringed = selected == Some(ix);
        // The ring sits around the photo, as Desktop's selected avatar's
        // border does, so the photo keeps its size.
        div()
            .id((gpui_kit::SharedString::from(format!("{id}-preset")), ix))
            .debug_selector(move || format!("{id}-preset-{ix}"))
            .when(ringed, |d| d.debug_selector(move || format!("{id}-preset-selected-{ix}")))
            .p(px(2.))
            .rounded_full()
            .border_2()
            .border_color(if ringed { palette.accent } else { gpui_kit::transparent_black() })
            .cursor_pointer()
            .child(div().size(px(52.)).rounded_full().overflow_hidden().bg(bg).child(img(image).size(px(52.))))
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
    /// The preset the staged photo is, ringed in the grid.
    preset: Option<usize>,
    /// The pick or preset being waited on.
    latest: Latest,
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

    pub fn changed(&self) -> bool {
        !matches!(self.staged, Staged::Current) && self.result().map(|(jpeg, _)| jpeg) != self.current.as_ref().map(|(jpeg, _)| jpeg.clone())
    }

    /// A picked photo or a preset (`preset`), or why it can't be used; a
    /// refused one leaves the staged photo as it was.
    fn stage(&mut self, photo: Result<Staged, String>, preset: Option<usize>, window: &mut Window, cx: &mut Context<Self>) {
        match photo {
            Ok(photo) => {
                self.staged = photo;
                self.preset = preset;
            }
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
            self.preset,
            palette,
            cx,
            |this, window, cx| {
                let request = this.latest.next();
                pick_photo("Choose photo", window, cx, move |this: &mut Self, photo, window, cx| {
                    if this.latest.is(request) {
                        this.stage(photo, None, window, cx);
                    }
                });
            },
            |this, _, cx| {
                this.latest.next();
                this.staged = Staged::Remove;
                this.preset = None;
                cx.notify();
            },
            |this, ix, window, cx| {
                let preset = &this.presets[ix];
                let request = this.latest.next();
                stage_preset(preset, window, cx, move |this: &mut Self, photo, window, cx| {
                    if this.latest.is(request) {
                        this.stage(photo, Some(ix), window, cx);
                    }
                });
            },
        )
    }
}

/// Asks before closing the editor with changes unsaved (Desktop's
/// `useConfirmDiscard` in `AvatarEditor`, with `ConfirmDiscardDialog`'s
/// untitled default): Cancel keeps it open, Discard closes both.
fn confirm_discard(window: &mut Window, cx: &mut gpui_kit::App) {
    use gpui_kit::component::dialog::DialogFooter;
    window.open_dialog(cx, |dialog, _, _| {
        dialog
            .w(px(360.))
            .child(div().debug_selector(|| "avatar-editor-discard".into()).text_sm().child("Would you like to discard these changes?"))
            .footer(
                DialogFooter::new()
                    .child(Button::new("avatar-editor-discard-cancel").label("Cancel").outline().on_click(|_, window, cx| window.close_dialog(cx)))
                    .child(Button::new("avatar-editor-discard-confirm").label("Discard").danger().on_click(|_, window, cx| {
                        // This one, then the editor under it.
                        window.close_dialog(cx);
                        window.close_dialog(cx);
                    })),
            )
    });
}

/// Opens the editor in a dialog titled `title`; Save hands the photo (or
/// none) to `on_save`. Closing it with changes asks first.
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
    let editor = cx.new(|_| DialogEditor { current, staged: Staged::Current, preset: None, latest: Latest::default(), presets, placeholder: std::rc::Rc::new(placeholder) });
    let on_save = std::rc::Rc::new(on_save);
    let shown = editor.clone();
    window.open_dialog(cx, move |dialog, _, cx| {
        let changed = shown.read(cx).changed();
        let (save, on_save, closing, cancelling) = (shown.clone(), on_save.clone(), shown.clone(), shown.clone());
        // Escape, a click outside and the close button come here: with
        // changes, ask first and stay open.
        let on_cancel = move |_: &gpui_kit::ClickEvent, window: &mut Window, cx: &mut gpui_kit::App| {
            if closing.read(cx).changed() {
                confirm_discard(window, cx);
                return false;
            }
            true
        };
        dialog.title(title).w(px(440.)).child(shown.clone()).on_cancel(on_cancel).footer(
            DialogFooter::new()
                .child(Button::new("avatar-editor-cancel").label("Cancel").outline().on_click(move |_, window, cx| {
                    if cancelling.read(cx).changed() {
                        confirm_discard(window, cx);
                    } else {
                        window.close_dialog(cx);
                    }
                }))
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
    fn a_named_pipe_is_refused_without_waiting_for_a_writer() {
        let dir = tempfile::tempdir().unwrap();
        let pipe = dir.path().join("pipe");
        assert!(std::process::Command::new("mkfifo").arg(&pipe).status().unwrap().success());
        // Opening it would wait for a writer forever; on a thread so a
        // regression fails the test rather than hanging it.
        let (send, receive) = std::sync::mpsc::channel();
        std::thread::spawn(move || send.send(read_photo(&pipe).err()));
        let refused = receive.recv_timeout(std::time::Duration::from_secs(5)).expect("read_photo blocked on a pipe");
        assert_eq!(refused.as_deref(), Some("it isn\u{2019}t a file."));
    }

    #[test]
    fn only_the_latest_request_counts() {
        let mut latest = Latest::default();
        let slow = latest.next();
        let quick = latest.next();
        assert!(latest.is(quick));
        assert!(!latest.is(slow), "the earlier pick lands late and is dropped");
        // Remove makes both stale.
        latest.next();
        assert!(!latest.is(quick));
    }

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
        // Not a regular file (a folder here; a pipe or device the same).
        assert!(read_photo(dir.path()).is_err_and(|reason| reason.contains("isn\u{2019}t a file")));
        // An image too small says so, as the profile editor did.
        let dot = dir.path().join("dot.png");
        image::RgbaImage::from_pixel(1, 1, image::Rgba([0, 0, 0, 255])).save(&dot).unwrap();
        assert!(read_photo(&dot).is_err_and(|reason| reason.contains("too small")), "{:?}", read_photo(&dot).err());
    }

    struct Blank;

    impl Render for Blank {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
        }
    }

    /// The "Group Avatar" dialog over an empty window, and what Save gave.
    fn open_editor(cx: &mut gpui_kit::TestAppContext) -> (gpui_kit::VisualTestContext, gpui_kit::Entity<DialogEditor>, std::rc::Rc<std::cell::RefCell<Option<Option<Photo>>>>) {
        cx.update(gpui_kit::init);
        let window = cx.update(|cx| gpui_kit::open_window(gpui_kit::WindowOptions::default(), cx, |_, cx| cx.new(|_| Blank)).unwrap().0);
        let mut vcx = gpui_kit::VisualTestContext::from_window(window, cx);
        let saved = std::rc::Rc::new(std::cell::RefCell::new(None));
        let saving = saved.clone();
        let editor = vcx.update(|window, cx| {
            open_dialog(
                "Group Avatar",
                None,
                &crate::avatar_presets::GROUP_PRESETS,
                || div().into_any_element(),
                move |photo, _, _| *saving.borrow_mut() = Some(photo),
                window,
                cx,
            )
        });
        vcx.run_until_parked();
        (vcx, editor, saved)
    }

    fn pick_preset(vcx: &mut gpui_kit::VisualTestContext, editor: &gpui_kit::Entity<DialogEditor>, ix: usize) {
        editor.update_in(vcx, |_, window, cx| {
            let preset = &crate::avatar_presets::GROUP_PRESETS[ix];
            stage_preset(preset, window, cx, move |this: &mut DialogEditor, photo, window, cx| this.stage(photo, Some(ix), window, cx));
        });
        vcx.run_until_parked();
        vcx.update(|window, _| window.refresh());
        vcx.run_until_parked();
    }

    #[gpui_kit::test]
    fn the_picked_preset_is_ringed_and_a_refused_file_keeps_it(cx: &mut gpui_kit::TestAppContext) {
        let (mut vcx, editor, _) = open_editor(cx);
        pick_preset(&mut vcx, &editor, 2);
        assert!(vcx.debug_bounds("avatar-editor-preset-selected-2").is_some(), "ringed");
        assert!(vcx.debug_bounds("avatar-editor-preset-selected-0").is_none());
        let staged = editor.read_with(&vcx, |e, _| e.result().map(|(jpeg, _)| jpeg));
        assert!(staged.is_some());
        // A file that can't be used: an alert over the editor, and the
        // preset stays staged and ringed.
        editor.update_in(&mut vcx, |e, window, cx| e.stage(Err("not an image.".into()), None, window, cx));
        vcx.run_until_parked();
        assert_eq!(editor.read_with(&vcx, |e, _| (e.result().map(|(jpeg, _)| jpeg), e.preset)), (staged, Some(2)));
        // Closing the alert leaves the editor open under it.
        vcx.update(|window, cx| window.close_dialog(cx));
        vcx.run_until_parked();
        assert!(vcx.update(|window, cx| window.has_active_dialog(cx)), "the editor is still open");
    }

    #[gpui_kit::test]
    fn closing_with_changes_asks_first(cx: &mut gpui_kit::TestAppContext) {
        use gpui_kit::test::TestWindowExt as _;
        let (mut vcx, editor, saved) = open_editor(cx);
        let click = |vcx: &mut gpui_kit::VisualTestContext, id: &'static str| {
            vcx.update(|window, cx| {
                window.render_frame(cx);
                window.click(gpui_kit::ElementId::from(id), cx);
            });
            vcx.run_until_parked();
        };
        // No changes: Cancel just closes.
        click(&mut vcx, "avatar-editor-cancel");
        assert!(!vcx.update(|window, cx| window.has_active_dialog(cx)));
        drop(editor);
        let (mut vcx, editor2, _) = open_editor(cx);
        pick_preset(&mut vcx, &editor2, 1);
        // With a change: Cancel asks, and the prompt's Cancel keeps editing.
        click(&mut vcx, "avatar-editor-cancel");
        vcx.update(|window, _| window.refresh());
        vcx.run_until_parked();
        assert!(vcx.debug_bounds("avatar-editor-discard").is_some(), "asks first");
        click(&mut vcx, "avatar-editor-discard-cancel");
        vcx.update(|window, _| window.refresh());
        vcx.run_until_parked();
        assert!(vcx.debug_bounds("avatar-editor-discard").is_none());
        assert!(vcx.debug_bounds("avatar-editor-preset-selected-1").is_some(), "still editing");
        // Escape asks too; Discard closes both, saving nothing.
        vcx.simulate_keystrokes("escape");
        vcx.update(|window, _| window.refresh());
        vcx.run_until_parked();
        assert!(vcx.debug_bounds("avatar-editor-discard").is_some(), "Escape asks first");
        click(&mut vcx, "avatar-editor-discard-confirm");
        assert!(!vcx.update(|window, cx| window.has_active_dialog(cx)));
        assert!(saved.borrow().is_none(), "nothing saved");
    }

    #[test]
    fn presets_make_photos() {
        for preset in crate::avatar_presets::PRESETS.iter().chain(&crate::avatar_presets::GROUP_PRESETS) {
            assert!(matches!(Staged::from_preset(preset), Ok(Staged::New(..))));
        }
    }
}
