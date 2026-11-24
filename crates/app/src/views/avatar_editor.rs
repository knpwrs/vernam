//! Signal Desktop's photo editor (`ts/components/AvatarEditor.dom.tsx`), as
//! the profile editor and a new group use it: the photo as it will be,
//! "Photo" to upload one (cropped square as Desktop uploads it), "Text" to
//! make one of up to three characters (`AvatarTextEditor`), "Remove", and
//! the list Desktop keeps (`userAvatarData`, [`crate::avatar_data`]): its
//! presets, then the photos made here. Clicking the selected preset again
//! recolors it (`AvatarIconEditor`), the selected text photo edits it, and
//! an uploaded or text photo has a button to delete it (`BetterAvatar`).
//!
//! As in Desktop, the list changes as soon as a photo is uploaded, made,
//! recolored or deleted; Save only chooses the photo (and moves it to the
//! front of the list).

use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{Icon, Sizable as _, WindowExt as _};
use gpui_kit::prelude::*;
use gpui_kit::{AnyElement, App, Context, Entity, Image, ImageFormat, Subscription, Window, div, img, px};

use crate::avatar_data::AvatarData;
#[cfg(test)]
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
    #[cfg(test)]
    pub fn from_preset(preset: &Preset) -> Result<Self, String> {
        let png = crate::avatar_presets::render(preset).map_err(|e| e.to_string())?;
        Self::from_image(&png)
    }

    /// The photo `data` draws, made as Desktop uploads it.
    pub fn from_data(data: &AvatarData, dir: &std::path::Path) -> Result<Self, String> {
        let bytes = crate::avatar_data::render(data, dir).map_err(|e| format!("{e}."))?;
        Self::from_image(&bytes)
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

/// A photo as saved from the editor: its JPEG and its preview image.
pub type Photo = (Arc<Vec<u8>>, Arc<Image>);

/// Draws the photo now (`Current`) at a size.
pub type CurrentView = Rc<dyn Fn(f32) -> AnyElement>;

/// What the editor shows.
enum Mode {
    Main,
    /// Desktop's `AvatarTextEditor`: `editing` the selected text photo, or a
    /// new one; `before` is the selection to go back to on Cancel.
    Text { input: Entity<InputState>, color: u8, editing: Option<AvatarData>, initial: (String, u8), font_size: f32, before: Option<AvatarData>, _subscription: Subscription },
    /// Desktop's `AvatarIconEditor`: the selected preset, recolored.
    Icon { original: AvatarData, color: u8, preview: Option<Photo> },
}

/// Desktop's `AvatarTextEditor` bubble size.
const BUBBLE: f32 = 120.;

/// What a host's footer offers (Desktop's `AvatarModalButtons`, whose
/// Save each mode enables on its own changes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Footer {
    /// Choosing the photo: Save once it differs from the current one.
    Main { changed: bool },
    /// Making a text photo or recoloring a preset: Save makes it.
    Sub { can_save: bool },
}

/// The editor, as the "Group Avatar" dialog and the profile editor's avatar
/// page show it. `id` prefixes its elements' ids and selectors.
pub struct AvatarEditor {
    id: &'static str,
    list: Vec<AvatarData>,
    /// Text and uploaded photos as drawn, by what they draw.
    thumbs: HashMap<String, Arc<Image>>,
    /// The photo from the list the staged one is, ringed in the grid.
    selected: Option<AvatarData>,
    staged: Staged,
    /// The photo now: its JPEG, when known, to tell an unchanged pick.
    current: Option<Arc<Vec<u8>>>,
    has_current: bool,
    current_view: CurrentView,
    /// Shown with no photo (the group icon, initials).
    placeholder: CurrentView,
    /// The pick or preset being waited on.
    latest: Latest,
    mode: Mode,
    /// Where uploaded photos are kept.
    dir: PathBuf,
    /// Keeps the list (ours, a group's, a new group's draft).
    on_list: Rc<dyn Fn(&[AvatarData], &mut App)>,
}

impl AvatarEditor {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: &'static str,
        list: Vec<AvatarData>,
        current: Option<Arc<Vec<u8>>>,
        has_current: bool,
        current_view: CurrentView,
        placeholder: CurrentView,
        dir: PathBuf,
        on_list: impl Fn(&[AvatarData], &mut App) + 'static,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut this = Self {
            id,
            list,
            thumbs: HashMap::new(),
            selected: None,
            staged: Staged::Current,
            current,
            has_current,
            current_view,
            placeholder,
            latest: Latest::default(),
            mode: Mode::Main,
            dir,
            on_list: Rc::new(on_list),
        };
        this.draw_thumbs(cx);
        this
    }

    /// The staged photo.
    pub fn staged(&self) -> &Staged {
        &self.staged
    }

    /// The photo Save would give: `None` for none (or, for a current photo
    /// not known as bytes, the current one kept).
    pub fn result(&self) -> Option<Photo> {
        match &self.staged {
            Staged::Current => self.current.clone().map(|jpeg| {
                let image = Arc::new(Image::from_bytes(ImageFormat::Jpeg, jpeg.to_vec()));
                (jpeg, image)
            }),
            Staged::Remove => None,
            Staged::New(jpeg, image) => Some((jpeg.clone(), image.clone())),
        }
    }

    /// Whether the photo chosen differs from the one now.
    pub fn changed(&self) -> bool {
        match &self.staged {
            Staged::Current => false,
            Staged::Remove => self.has_current,
            Staged::New(jpeg, _) => self.current.as_ref() != Some(jpeg),
        }
    }

    pub fn footer(&self, cx: &App) -> Footer {
        match &self.mode {
            Mode::Main => Footer::Main { changed: self.changed() },
            Mode::Text { input, color, initial, .. } => {
                let text = input.read(cx).value().to_string();
                Footer::Sub { can_save: !text.trim().is_empty() && (text != initial.0 || *color != initial.1) }
            }
            Mode::Icon { original, color, preview } => Footer::Sub { can_save: preview.is_some() && *color != original.color_index() },
        }
    }

    /// Desktop's Save in the main view: the photo chosen from the list moves
    /// to its front (`replaceAvatar(selected, selected)`).
    pub fn commit(&mut self, cx: &mut Context<Self>) {
        if let Some(selected) = self.selected.clone()
            && self.changed()
        {
            self.list = crate::avatar_data::replace(&self.list, selected.clone(), Some(&selected));
            self.selected = self.list.first().cloned();
            (self.on_list)(&self.list, cx);
        }
    }

    fn set_list(&mut self, list: Vec<AvatarData>, cx: &mut Context<Self>) {
        self.list = list;
        (self.on_list)(&self.list, cx);
        self.draw_thumbs(cx);
        cx.notify();
    }

    /// What `data` draws, as a key for its drawing.
    fn thumb_key(data: &AvatarData) -> String {
        format!("{:?}|{:?}|{:?}", data.color, data.text, data.image)
    }

    /// Draws the text and uploaded photos not drawn yet, off the UI thread.
    fn draw_thumbs(&mut self, cx: &mut Context<Self>) {
        let missing: Vec<AvatarData> = self.list.iter().filter(|d| d.icon.is_none() && !self.thumbs.contains_key(&Self::thumb_key(d))).cloned().collect();
        if missing.is_empty() {
            return;
        }
        let dir = self.dir.clone();
        cx.spawn(async move |this, cx| {
            let drawn = cx
                .background_executor()
                .spawn(async move {
                    missing
                        .into_iter()
                        .filter_map(|data| {
                            // Text draws as a PNG; an uploaded photo is the JPEG
                            // `Staged::from_image` made.
                            let format = if data.image.is_some() { ImageFormat::Jpeg } else { ImageFormat::Png };
                            match crate::avatar_data::render(&data, &dir) {
                                Ok(bytes) => Some((Self::thumb_key(&data), Arc::new(Image::from_bytes(format, bytes)))),
                                Err(error) => {
                                    tracing::warn!(%error, "couldn't draw a saved avatar");
                                    None
                                }
                            }
                        })
                        .collect::<Vec<_>>()
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.thumbs.extend(drawn);
                cx.notify();
            });
        })
        .detach();
    }

    /// A photo picked or made (`Ok`), or why it can't be used; a refused one
    /// leaves the staged photo as it was.
    fn stage(&mut self, photo: Result<Staged, String>, selected: Option<AvatarData>, window: &mut Window, cx: &mut Context<Self>) {
        match photo {
            Ok(photo) => {
                self.staged = photo;
                self.selected = selected;
            }
            Err(reason) => {
                let text = format!("That photo can\u{2019}t be used: {reason}");
                window.open_alert_dialog(cx, move |alert, _, _| alert.description(text.clone()));
            }
        }
        cx.notify();
    }

    /// "Photo": a picked file, kept in the list (Desktop's
    /// `saveAvatarToDisk`) and chosen.
    fn upload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let request = self.latest.next();
        pick_photo("Choose photo", window, cx, move |this: &mut Self, photo, window, cx| this.uploaded(photo, request, window, cx));
    }

    /// The photo picked for upload `request`, saved and added to the list.
    fn uploaded(&mut self, photo: Result<Staged, String>, request: u64, window: &mut Window, cx: &mut Context<Self>) {
        {
            if !self.latest.is(request) {
                return;
            }
            let this = self;
            let (jpeg, image) = match photo {
                Ok(Staged::New(jpeg, image)) => (jpeg, image),
                other => return this.stage(other, None, window, cx),
            };
            // The file first, then the list that names it.
            let dir = this.dir.clone();
            let bytes = jpeg.clone();
            cx.spawn_in(window, async move |this, cx| {
                let written = cx.background_executor().spawn(async move { crate::avatar_data::write_image(&dir, &bytes) }).await;
                let _ = this.update_in(cx, |this, window, cx| {
                    if !this.latest.is(request) {
                        // Picked over: the file isn't wanted.
                        if let Ok(name) = &written {
                            crate::avatar_data::remove_image(&this.dir, name);
                        }
                        return;
                    }
                    match written {
                        Ok(name) => {
                            let data = AvatarData { id: 0, color: None, icon: None, text: None, image: Some(name) };
                            let list = crate::avatar_data::replace(&this.list, data, None);
                            let added = list.first().cloned();
                            this.thumbs.insert(Self::thumb_key(added.as_ref().unwrap()), image.clone());
                            this.set_list(list, cx);
                            this.stage(Ok(Staged::New(jpeg, image)), added, window, cx);
                        }
                        Err(error) => this.stage(Err(format!("it couldn\u{2019}t be saved ({error}).")), None, window, cx),
                    }
                });
            })
            .detach();
        }
    }

    /// "Remove": no photo.
    fn remove_photo(&mut self, cx: &mut Context<Self>) {
        self.latest.next();
        self.staged = Staged::Remove;
        self.selected = None;
        cx.notify();
    }

    /// A photo in the list clicked: chosen, or, when it's the one chosen
    /// already, edited (Desktop's `BetterAvatar` `onClick`).
    fn pick(&mut self, data: AvatarData, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected.as_ref().is_some_and(|s| s.same(&data)) {
            if data.text.is_some() {
                self.edit_text(Some(data), window, cx);
            } else if data.icon.is_some() {
                self.edit_icon(data, cx);
            }
            return;
        }
        let request = self.latest.next();
        let dir = self.dir.clone();
        cx.spawn_in(window, async move |this, cx| {
            let drawing = data.clone();
            let drawn = cx.background_executor().spawn(async move { Staged::from_data(&drawing, &dir) }).await;
            let _ = this.update_in(cx, |this, window, cx| {
                if this.latest.is(request) {
                    this.stage(drawn, Some(data), window, cx);
                }
            });
        })
        .detach();
    }

    /// The delete button on a text or uploaded photo (Desktop's
    /// `deleteAvatarFromDisk`): out of the list, then its file deleted.
    fn delete(&mut self, data: AvatarData, cx: &mut Context<Self>) {
        let list = crate::avatar_data::without(&self.list, &data);
        if self.selected.as_ref().is_some_and(|s| s.same(&data)) {
            self.latest.next();
            self.selected = None;
            self.staged = Staged::Current;
        }
        self.set_list(list, cx);
        if let Some(name) = data.image {
            let dir = self.dir.clone();
            cx.background_executor().spawn(async move { crate::avatar_data::remove_image(&dir, &name) }).detach();
        }
    }

    /// "Text", or the selected text photo clicked again.
    fn edit_text(&mut self, editing: Option<AvatarData>, window: &mut Window, cx: &mut Context<Self>) {
        let text = editing.as_ref().and_then(|d| d.text.clone()).unwrap_or_default();
        let color = editing.as_ref().map_or(0, AvatarData::color_index);
        let input = cx.new(|cx| InputState::new(window, cx));
        let last = std::cell::RefCell::new(text.clone());
        let subscription = cx.subscribe_in(&input, window, move |this, input, event: &InputEvent, window, cx| {
            if !matches!(event, InputEvent::Change) {
                return;
            }
            use unicode_segmentation::UnicodeSegmentation as _;
            let value = input.read(cx).value().to_string();
            // Desktop refuses a change past three characters.
            if value.graphemes(true).count() > crate::avatar_text::MAX_LENGTH {
                let previous = last.borrow().clone();
                input.update(cx, |input, cx| input.set_value(previous, window, cx));
                return;
            }
            *last.borrow_mut() = value.clone();
            this.fit_text(value, cx);
        });
        // Filled with the cursor after the text, ready to change it.
        input.update(cx, |input, cx| {
            input.set_value(text.clone(), window, cx);
            input.focus(window, cx);
        });
        let before = self.selected.take();
        self.mode = Mode::Text { input, color, editing, initial: (text.clone(), color), font_size: BUBBLE * 0.45, before, _subscription: subscription };
        self.fit_text(text, cx);
        cx.notify();
    }

    /// The bubble's font size for `text`, worked out off the UI thread.
    fn fit_text(&mut self, text: String, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let measuring = text.clone();
            let size = cx.background_executor().spawn(async move { crate::avatar_text::fitted_size(&measuring, BUBBLE) }).await;
            let _ = this.update(cx, |this, cx| {
                if let Mode::Text { input, font_size, .. } = &mut this.mode
                    && input.read(cx).value() == text.as_str()
                {
                    *font_size = size;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// The selected preset clicked again: recolor it.
    fn edit_icon(&mut self, original: AvatarData, cx: &mut Context<Self>) {
        let color = original.color_index();
        self.mode = Mode::Icon { original, color, preview: None };
        self.draw_icon(cx);
        cx.notify();
    }

    /// The recolored preset's preview, drawn off the UI thread.
    fn draw_icon(&mut self, cx: &mut Context<Self>) {
        let Mode::Icon { original, color, .. } = &self.mode else { return };
        let mut data = original.clone();
        data.color = Some(crate::avatar_data::COLOR_NAMES[*color as usize].into());
        let wanted = *color;
        let dir = self.dir.clone();
        cx.spawn(async move |this, cx| {
            let drawn = cx.background_executor().spawn(async move { Staged::from_data(&data, &dir) }).await;
            let _ = this.update(cx, |this, cx| {
                if let (Mode::Icon { color, preview, .. }, Ok(Staged::New(jpeg, image))) = (&mut this.mode, drawn)
                    && *color == wanted
                {
                    *preview = Some((jpeg, image));
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn pick_color(&mut self, picked: u8, cx: &mut Context<Self>) {
        match &mut self.mode {
            Mode::Text { color, .. } => *color = picked,
            Mode::Icon { color, preview, .. } => {
                *color = picked;
                *preview = None;
            }
            Mode::Main => return,
        }
        self.draw_icon(cx);
        cx.notify();
    }

    /// Cancel in the text or color editor: back to the photo, as it was.
    pub fn cancel_sub(&mut self, cx: &mut Context<Self>) {
        if let Mode::Text { before, .. } = std::mem::replace(&mut self.mode, Mode::Main)
            && self.selected.is_none()
        {
            self.selected = before;
        }
        cx.notify();
    }

    /// Save in the text or color editor: the photo made replaces the one
    /// edited in the list (Desktop's `replaceAvatar`), and is chosen.
    pub fn save_sub(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !matches!(self.footer(cx), Footer::Sub { can_save: true }) {
            return;
        }
        match std::mem::replace(&mut self.mode, Mode::Main) {
            Mode::Text { input, color, editing, .. } => {
                let text = input.read(cx).value().to_string();
                let made = AvatarData { id: 0, color: Some(crate::avatar_data::COLOR_NAMES[color as usize].into()), icon: None, text: Some(text), image: None };
                let request = self.latest.next();
                let dir = self.dir.clone();
                cx.spawn_in(window, async move |this, cx| {
                    let drawing = made.clone();
                    let drawn = cx.background_executor().spawn(async move { Staged::from_data(&drawing, &dir) }).await;
                    let _ = this.update_in(cx, |this, window, cx| {
                        if !this.latest.is(request) {
                            return;
                        }
                        if drawn.is_err() {
                            return this.stage(drawn, None, window, cx);
                        }
                        let list = crate::avatar_data::replace(&this.list, made, editing.as_ref());
                        let added = list.first().cloned();
                        this.set_list(list, cx);
                        this.stage(drawn, added, window, cx);
                    });
                })
                .detach();
            }
            Mode::Icon { original, color, preview: Some((jpeg, image)) } => {
                let mut recolored = original.clone();
                recolored.color = Some(crate::avatar_data::COLOR_NAMES[color as usize].into());
                let list = crate::avatar_data::replace(&self.list, recolored, Some(&original));
                let added = list.first().cloned();
                self.latest.next();
                self.set_list(list, cx);
                self.stage(Ok(Staged::New(jpeg, image)), added, window, cx);
            }
            other => self.mode = other,
        }
        cx.notify();
    }

    fn preview(&self, size: f32) -> AnyElement {
        match &self.staged {
            Staged::New(_, image) => img(image.clone()).size(px(size)).aspect_square().rounded_full().object_fit(gpui_kit::ObjectFit::Cover).into_any_element(),
            Staged::Remove => (self.placeholder)(size),
            Staged::Current => (self.current_view)(size),
        }
    }

    /// One photo of the list, ringed when chosen, with a delete button
    /// for a text or uploaded one.
    fn render_item(&self, ix: usize, data: &AvatarData, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let id = self.id;
        let ringed = self.selected.as_ref().is_some_and(|s| s.same(data));
        let (bg, _) = crate::theme::avatar_colors(data.color_index());
        let drawing = match data.icon.as_deref().and_then(crate::avatar_presets::by_name) {
            Some(preset) => {
                let image = Arc::new(Image::from_bytes(ImageFormat::Svg, preset.svg.to_vec()));
                div().size(px(52.)).rounded_full().overflow_hidden().bg(bg).child(img(image).size(px(52.))).into_any_element()
            }
            None => match self.thumbs.get(&Self::thumb_key(data)) {
                Some(image) => img(image.clone()).size(px(52.)).rounded_full().object_fit(gpui_kit::ObjectFit::Cover).into_any_element(),
                None => div().size(px(52.)).rounded_full().bg(bg).into_any_element(),
            },
        };
        let picked = data.clone();
        let deleting = data.clone();
        // The ring sits around the photo, as Desktop's selected avatar's
        // border does, so the photo keeps its size.
        div()
            .id((gpui_kit::SharedString::from(format!("{id}-preset")), ix))
            .debug_selector(move || format!("{id}-preset-{ix}"))
            .relative()
            .p(px(2.))
            .rounded_full()
            .border_2()
            .border_color(if ringed { palette.accent } else { gpui_kit::transparent_black() })
            .cursor_pointer()
            // An element has one selector: the ring's is on the drawing.
            .child(div().when(ringed, |d| d.debug_selector(move || format!("{id}-preset-selected-{ix}"))).child(drawing))
            .when(data.deletable(), |d| {
                d.child(
                    div()
                        .id((gpui_kit::SharedString::from(format!("{id}-delete")), ix))
                        .debug_selector(move || format!("{id}-delete-{ix}"))
                        .absolute()
                        .top_0()
                        .right_0()
                        .size(px(18.))
                        .rounded_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .bg(palette.text_secondary)
                        .cursor_pointer()
                        .child(Icon::new(IconName::X).size(px(12.)).text_color(palette.background))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.delete(deleting.clone(), cx);
                        })),
                )
            })
            .on_click(cx.listener(move |this, _, window, cx| this.pick(picked.clone(), window, cx)))
            .into_any_element()
    }

    /// Desktop's `AvatarColorPicker`: "Choose a color" over the twelve.
    fn render_colors(&self, selected: u8, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let id = self.id;
        let bubbles = (0..crate::avatar_data::COLOR_NAMES.len() as u8).map(|ix| {
            let (bg, fg) = crate::theme::avatar_colors(ix);
            let ringed = ix == selected;
            div()
                .id((gpui_kit::SharedString::from(format!("{id}-color")), ix as usize))
                .debug_selector(move || format!("{id}-color-{ix}"))
                .p(px(2.))
                .rounded_full()
                .border_2()
                .border_color(if ringed { palette.accent } else { gpui_kit::transparent_black() })
                .cursor_pointer()
                .child(
                    div()
                        .when(ringed, |d| d.debug_selector(move || format!("{id}-color-selected-{ix}")))
                        .size(px(40.))
                        .rounded_full()
                        .bg(bg)
                        .border_2()
                        .border_color(fg.opacity(0.2)),
                )
                .on_click(cx.listener(move |this, _, _, cx| this.pick_color(ix, cx)))
        });
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(div().text_xs().text_color(palette.text_secondary).child("Choose a color"))
            .child(div().flex().flex_wrap().gap_2().children(bubbles))
            .into_any_element()
    }

    fn render_main(&self, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let id = self.id;
        let has_photo = match self.staged {
            Staged::Current => self.has_current,
            Staged::Remove => false,
            Staged::New(..) => true,
        };
        let items: Vec<AnyElement> = self.list.iter().enumerate().map(|(ix, data)| self.render_item(ix, data, palette, cx)).collect();
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(div().flex().justify_center().child(self.preview(96.)))
            .child(
                div()
                    .flex()
                    .justify_center()
                    .gap_2()
                    .child(
                        Button::new(gpui_kit::SharedString::from(format!("{id}-upload-photo")))
                            .label("Photo")
                            .outline()
                            .small()
                            .on_click(cx.listener(|this, _, window, cx| this.upload(window, cx))),
                    )
                    .child(
                        Button::new(gpui_kit::SharedString::from(format!("{id}-text")))
                            .label("Text")
                            .outline()
                            .small()
                            .on_click(cx.listener(|this, _, window, cx| this.edit_text(None, window, cx))),
                    )
                    .when(has_photo, |d| {
                        d.child(
                            Button::new(gpui_kit::SharedString::from(format!("{id}-remove-photo")))
                                .label("Remove")
                                .ghost()
                                .small()
                                .on_click(cx.listener(|this, _, _, cx| this.remove_photo(cx))),
                        )
                    }),
            )
            .child(div().text_xs().text_color(palette.text_secondary).child("Select an avatar"))
            .child(div().flex().flex_wrap().gap_2().children(items))
            .into_any_element()
    }
}

impl Render for AvatarEditor {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = Palette::get(cx);
        let id = self.id;
        match &self.mode {
            Mode::Main => self.render_main(palette, cx),
            Mode::Text { input, color, font_size, .. } => {
                let (bg, fg) = crate::theme::avatar_colors(*color);
                let bubble = div()
                    .debug_selector(move || format!("{id}-text-bubble"))
                    .size(px(BUBBLE))
                    .rounded_full()
                    .bg(bg)
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        div().w(px(BUBBLE * 0.8)).child(
                            Input::new(input)
                                .appearance(false)
                                .text_align(gpui_kit::TextAlign::Center)
                                .font_family(crate::fonts::INTER)
                                .text_size(px(*font_size))
                                // Tall enough for the glyphs at this size
                                // (an input's line is short by default).
                                .line_height(px(*font_size * 1.25))
                                .h(px(*font_size * 1.4))
                                .text_color(fg),
                        ),
                    );
                let colors = self.render_colors(*color, palette, cx);
                div().flex().flex_col().gap_3().child(div().flex().justify_center().child(bubble)).child(colors).into_any_element()
            }
            Mode::Icon { original, color, preview } => {
                let (bg, _) = crate::theme::avatar_colors(*color);
                let shown = match preview {
                    Some((_, image)) => img(image.clone()).size(px(96.)).rounded_full().into_any_element(),
                    None => {
                        let svg = original.icon.as_deref().and_then(crate::avatar_presets::by_name).map(|p| Arc::new(Image::from_bytes(ImageFormat::Svg, p.svg.to_vec())));
                        div().size(px(96.)).rounded_full().overflow_hidden().bg(bg).children(svg.map(|s| img(s).size(px(96.)))).into_any_element()
                    }
                };
                let colors = self.render_colors(*color, palette, cx);
                div()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(div().debug_selector(move || format!("{id}-icon-preview")).flex().justify_center().child(shown))
                    .child(colors)
                    .into_any_element()
            }
        }
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

/// Opens the editor in a dialog titled `title` (Desktop's `AvatarEditor`
/// with `isInsideDialog`, as "Group Avatar" opens it): `list` the photos
/// it keeps, `on_list` keeping its changes; Save hands the photo chosen (or
/// none) to `on_save`. Closing it with changes asks first; Cancel in the
/// text or color editor goes back to the photo.
#[allow(clippy::too_many_arguments)]
pub fn open_dialog(
    title: &'static str,
    current: Option<Photo>,
    list: Vec<AvatarData>,
    dir: PathBuf,
    placeholder: impl Fn(f32) -> AnyElement + 'static,
    on_list: impl Fn(&[AvatarData], &mut App) + 'static,
    on_save: impl Fn(Option<Photo>, &mut Window, &mut gpui_kit::App) + 'static,
    window: &mut Window,
    cx: &mut gpui_kit::App,
) -> Entity<AvatarEditor> {
    use gpui_kit::component::Disableable as _;
    use gpui_kit::component::dialog::DialogFooter;
    let placeholder: CurrentView = Rc::new(placeholder);
    let current_view: CurrentView = match &current {
        Some((_, image)) => {
            let image = image.clone();
            Rc::new(move |size| img(image.clone()).size(px(size)).aspect_square().rounded_full().object_fit(gpui_kit::ObjectFit::Cover).into_any_element())
        }
        None => placeholder.clone(),
    };
    let has_current = current.is_some();
    let editor = cx.new(|cx| AvatarEditor::new("avatar-editor", list, current.map(|(jpeg, _)| jpeg), has_current, current_view, placeholder, dir, on_list, cx));
    let on_save = Rc::new(on_save);
    let shown = editor.clone();
    window.open_dialog(cx, move |dialog, _, cx| {
        let footer = shown.read(cx).footer(cx);
        let (save, on_save, closing, cancelling) = (shown.clone(), on_save.clone(), shown.clone(), shown.clone());
        // Escape, a click outside and the close button come here: in the
        // text or color editor, back to the photo; with changes, ask first.
        let on_cancel = move |_: &gpui_kit::ClickEvent, window: &mut Window, cx: &mut gpui_kit::App| match closing.read(cx).footer(cx) {
            Footer::Sub { .. } => {
                closing.update(cx, |e, cx| e.cancel_sub(cx));
                false
            }
            Footer::Main { changed: true } => {
                confirm_discard(window, cx);
                false
            }
            Footer::Main { changed: false } => true,
        };
        let (can_save, sub) = match footer {
            Footer::Main { changed } => (changed, false),
            Footer::Sub { can_save } => (can_save, true),
        };
        dialog.title(title).w(px(440.)).child(shown.clone()).on_cancel(on_cancel).footer(
            DialogFooter::new()
                .child(Button::new("avatar-editor-cancel").label("Cancel").outline().on_click(move |_, window, cx| {
                    match cancelling.read(cx).footer(cx) {
                        Footer::Sub { .. } => cancelling.update(cx, |e, cx| e.cancel_sub(cx)),
                        Footer::Main { changed: true } => confirm_discard(window, cx),
                        Footer::Main { changed: false } => window.close_dialog(cx),
                    }
                }))
                .child(Button::new("avatar-editor-save").label("Save").primary().disabled(!can_save).on_click(move |_, window, cx| {
                    if sub {
                        save.update(cx, |e, cx| e.save_sub(window, cx));
                        return;
                    }
                    save.update(cx, |e, cx| e.commit(cx));
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

    /// The "Group Avatar" dialog over an empty window, with the group
    /// presets; what it keeps and what Save gave.
    struct Opened {
        vcx: gpui_kit::VisualTestContext,
        editor: Entity<AvatarEditor>,
        saved: Rc<std::cell::RefCell<Option<Option<Photo>>>>,
        kept: Rc<std::cell::RefCell<Option<Vec<AvatarData>>>>,
        dir: tempfile::TempDir,
    }

    fn open_editor(cx: &mut gpui_kit::TestAppContext) -> Opened {
        cx.update(gpui_kit::init);
        let window = cx.update(|cx| gpui_kit::open_window(gpui_kit::WindowOptions::default(), cx, |_, cx| cx.new(|_| Blank)).unwrap().0);
        let mut vcx = gpui_kit::VisualTestContext::from_window(window, cx);
        let saved = Rc::new(std::cell::RefCell::new(None));
        let kept = Rc::new(std::cell::RefCell::new(None));
        let dir = tempfile::tempdir().unwrap();
        let (saving, keeping, photos) = (saved.clone(), kept.clone(), dir.path().to_owned());
        let editor = vcx.update(|window, cx| {
            open_dialog(
                "Group Avatar",
                None,
                crate::avatar_data::defaults(true),
                photos,
                |_| div().into_any_element(),
                move |list, _| *keeping.borrow_mut() = Some(list.to_vec()),
                move |photo, _, _| *saving.borrow_mut() = Some(photo),
                window,
                cx,
            )
        });
        vcx.run_until_parked();
        Opened { vcx, editor, saved, kept, dir }
    }

    fn settle(vcx: &mut gpui_kit::VisualTestContext) {
        vcx.run_until_parked();
        vcx.update(|window, _| window.refresh());
        vcx.run_until_parked();
    }

    fn pick_preset(vcx: &mut gpui_kit::VisualTestContext, editor: &Entity<AvatarEditor>, ix: usize) {
        editor.update_in(vcx, |e, window, cx| {
            let data = e.list[ix].clone();
            e.pick(data, window, cx);
        });
        settle(vcx);
    }

    /// Clicks what `selector` marks, at its middle (the grid's items and
    /// colors are plain clickable elements).
    fn tap(vcx: &mut gpui_kit::VisualTestContext, selector: &'static str) {
        settle(vcx);
        let at = vcx.debug_bounds(selector).unwrap_or_else(|| panic!("no {selector}")).center();
        vcx.simulate_click(at, gpui_kit::Modifiers::none());
        settle(vcx);
    }

    fn click(vcx: &mut gpui_kit::VisualTestContext, id: impl Into<gpui_kit::ElementId>) {
        use gpui_kit::test::TestWindowExt as _;
        let id = id.into();
        vcx.update(|window, cx| {
            window.render_frame(cx);
            window.click(id, cx);
        });
        settle(vcx);
    }

    #[gpui_kit::test]
    fn the_picked_preset_is_ringed_and_a_refused_file_keeps_it(cx: &mut gpui_kit::TestAppContext) {
        let Opened { mut vcx, editor, .. } = open_editor(cx);
        pick_preset(&mut vcx, &editor, 2);
        assert!(vcx.debug_bounds("avatar-editor-preset-selected-2").is_some(), "ringed");
        assert!(vcx.debug_bounds("avatar-editor-preset-selected-0").is_none());
        let staged = editor.read_with(&vcx, |e, _| e.result().map(|(jpeg, _)| jpeg));
        assert!(staged.is_some());
        // A file that can't be used: an alert over the editor, and the
        // preset stays staged and ringed.
        editor.update_in(&mut vcx, |e, window, cx| e.stage(Err("not an image.".into()), None, window, cx));
        vcx.run_until_parked();
        assert_eq!(editor.read_with(&vcx, |e, _| (e.result().map(|(jpeg, _)| jpeg), e.selected.as_ref().map(|s| s.id))), (staged, Some(2)));
        // Closing the alert leaves the editor open under it.
        vcx.update(|window, cx| window.close_dialog(cx));
        vcx.run_until_parked();
        assert!(vcx.update(|window, cx| window.has_active_dialog(cx)), "the editor is still open");
    }

    #[gpui_kit::test]
    fn closing_with_changes_asks_first(cx: &mut gpui_kit::TestAppContext) {
        let Opened { mut vcx, editor, .. } = open_editor(cx);
        // No changes: Cancel just closes.
        click(&mut vcx, "avatar-editor-cancel");
        assert!(!vcx.update(|window, cx| window.has_active_dialog(cx)));
        drop(editor);
        let Opened { mut vcx, editor: editor2, saved, .. } = open_editor(cx);
        pick_preset(&mut vcx, &editor2, 1);
        // With a change: Cancel asks, and the prompt's Cancel keeps editing.
        click(&mut vcx, "avatar-editor-cancel");
        assert!(vcx.debug_bounds("avatar-editor-discard").is_some(), "asks first");
        click(&mut vcx, "avatar-editor-discard-cancel");
        assert!(vcx.debug_bounds("avatar-editor-discard").is_none());
        assert!(vcx.debug_bounds("avatar-editor-preset-selected-1").is_some(), "still editing");
        // Escape asks too; Discard closes both, saving nothing.
        vcx.simulate_keystrokes("escape");
        settle(&mut vcx);
        assert!(vcx.debug_bounds("avatar-editor-discard").is_some(), "Escape asks first");
        click(&mut vcx, "avatar-editor-discard-confirm");
        assert!(!vcx.update(|window, cx| window.has_active_dialog(cx)));
        assert!(saved.borrow().is_none(), "nothing saved");
    }

    /// A small photo, as a picked file would be once read.
    fn photo() -> Staged {
        let mut png = Vec::new();
        image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(8, 8, image::Rgba([200, 80, 40, 255])))
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        Staged::from_image(&png).unwrap()
    }

    #[gpui_kit::test]
    fn an_upload_is_kept_first_in_the_list_and_deleting_it_removes_its_file(cx: &mut gpui_kit::TestAppContext) {
        let Opened { mut vcx, editor, kept, dir, .. } = open_editor(cx);
        editor.update_in(&mut vcx, |e, window, cx| {
            let request = e.latest.next();
            e.uploaded(Ok(photo()), request, window, cx);
        });
        settle(&mut vcx);
        // Kept at once (Desktop's `saveAvatarToDisk`), first, and chosen.
        let list = kept.borrow().clone().expect("the list was kept");
        assert_eq!(list.len(), 13);
        let name = list[0].image.clone().expect("an uploaded photo");
        assert!(dir.path().join(&name).exists(), "its file");
        assert!(vcx.debug_bounds("avatar-editor-preset-selected-0").is_some());
        assert!(editor.read_with(&vcx, |e, _| e.changed()));
        // Deleting it: out of the list, its file gone, the photo unchosen.
        tap(&mut vcx, "avatar-editor-delete-0");
        assert_eq!(kept.borrow().as_ref().map(Vec::len), Some(12));
        assert!(!dir.path().join(&name).exists(), "its file deleted");
        assert!(!editor.read_with(&vcx, |e, _| e.changed()));
        // Presets have no delete button.
        assert!(vcx.debug_bounds("avatar-editor-delete-0").is_none());
    }

    #[gpui_kit::test]
    fn a_stale_upload_is_dropped_with_its_file(cx: &mut gpui_kit::TestAppContext) {
        let Opened { mut vcx, editor, kept, dir, .. } = open_editor(cx);
        editor.update_in(&mut vcx, |e, window, cx| {
            let request = e.latest.next();
            // Something else chosen before it lands.
            e.latest.next();
            e.uploaded(Ok(photo()), request, window, cx);
        });
        settle(&mut vcx);
        assert!(kept.borrow().is_none(), "the list unchanged");
        assert_eq!(std::fs::read_dir(dir.path()).map(|d| d.count()).unwrap_or(0), 0);
    }

    #[gpui_kit::test]
    fn text_makes_a_photo_kept_in_the_list_and_edited_again(cx: &mut gpui_kit::TestAppContext) {
        let Opened { mut vcx, editor, kept, saved, .. } = open_editor(cx);
        click(&mut vcx, "avatar-editor-text");
        assert!(vcx.debug_bounds("avatar-editor-text-bubble").is_some());
        // Nothing typed: Save waits.
        assert_eq!(editor.read_with(&vcx, |e, cx| e.footer(cx)), Footer::Sub { can_save: false });
        // Three characters at most, as Desktop's input refuses a fourth.
        vcx.simulate_input("ABCD");
        settle(&mut vcx);
        let typed = editor.read_with(&vcx, |e, cx| match &e.mode {
            Mode::Text { input, .. } => input.read(cx).value().to_string(),
            _ => String::new(),
        });
        assert_eq!(typed, "ABC");
        tap(&mut vcx, "avatar-editor-color-5");
        assert!(vcx.debug_bounds("avatar-editor-color-selected-5").is_some());
        click(&mut vcx, "avatar-editor-save");
        // Back to the photo: the text photo first in the list, chosen.
        let list = kept.borrow().clone().unwrap();
        assert_eq!((list[0].text.as_deref(), list[0].color.as_deref(), list.len()), (Some("ABC"), Some("A150"), 13));
        assert!(vcx.debug_bounds("avatar-editor-preset-selected-0").is_some());
        // Clicked again it edits that one, which keeps its place and id.
        tap(&mut vcx, "avatar-editor-preset-0");
        vcx.simulate_keystrokes("backspace");
        settle(&mut vcx);
        click(&mut vcx, "avatar-editor-save");
        let list = kept.borrow().clone().unwrap();
        assert_eq!((list[0].text.as_deref(), list[0].id, list.len()), (Some("AB"), 12, 13));
        // Save: the photo, and the list unchanged but for the choice.
        click(&mut vcx, "avatar-editor-save");
        assert!(saved.borrow().as_ref().is_some_and(|photo| photo.is_some()));
    }

    #[gpui_kit::test]
    fn the_chosen_preset_clicked_again_is_recolored(cx: &mut gpui_kit::TestAppContext) {
        let Opened { mut vcx, editor, kept, .. } = open_editor(cx);
        pick_preset(&mut vcx, &editor, 3);
        tap(&mut vcx, "avatar-editor-preset-3");
        assert!(vcx.debug_bounds("avatar-editor-icon-preview").is_some(), "the color editor");
        // Its own color: nothing to save.
        assert!(vcx.debug_bounds("avatar-editor-color-selected-7").is_some(), "celebration is A170");
        assert_eq!(editor.read_with(&vcx, |e, cx| e.footer(cx)), Footer::Sub { can_save: false });
        // Cancel: back, the list as it was.
        click(&mut vcx, "avatar-editor-cancel");
        assert!(vcx.debug_bounds("avatar-editor-preset-selected-3").is_some());
        assert!(kept.borrow().is_none());
        // Recolored and saved: it moves to the front with its id.
        tap(&mut vcx, "avatar-editor-preset-3");
        tap(&mut vcx, "avatar-editor-color-0");
        click(&mut vcx, "avatar-editor-save");
        let list = kept.borrow().clone().unwrap();
        assert_eq!((list[0].icon.as_deref(), list[0].color.as_deref(), list[0].id, list.len()), (Some("celebration"), Some("A100"), 3, 12));
        let staged = editor.read_with(&vcx, |e, _| e.result().map(|(jpeg, _)| jpeg)).unwrap();
        let corner = image::load_from_memory(&staged).unwrap().to_rgba8().get_pixel(0, 0).0;
        assert!(corner[0].abs_diff(0xe3) < 6 && corner[2].abs_diff(0xfe) < 6, "on A100: {corner:?}");
    }

    #[test]
    fn presets_make_photos() {
        for preset in crate::avatar_presets::PRESETS.iter().chain(&crate::avatar_presets::GROUP_PRESETS) {
            assert!(matches!(Staged::from_preset(preset), Ok(Staged::New(..))));
        }
    }
}
