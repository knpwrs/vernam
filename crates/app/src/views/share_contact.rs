//! Desktop's "Share contact" dialog (`ts/components/ShareContactDialog.dom.tsx`),
//! from the composer's add menu: "Select contact" (search, by first
//! initial), then "Share contact" (the photo and name always go, the
//! number, nickname and note when checked), with "Edit name" and "Select a
//! photo" on the way. Sending is `Command::ShareContact`.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::{Dialog, DialogFooter};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::assets::IconName;
use gpui_kit::component::{Disableable as _, Icon, Sizable as _, WindowExt as _};
use gpui_kit::prelude::*;
use gpui_kit::{AnyElement, App, Entity, SharedString, Window, div, px};
use signal_backend::Command;
use signal_backend::share_contact::{
    self, ContactName, ShareCandidate, ShareContactDraft, ShareContactSelection, apply_selection, can_share, format_phone,
};
use signal_store::ConversationId;
use unicode_segmentation::UnicodeSegmentation as _;

use crate::state::AppState;
use crate::theme::Palette;
use crate::views::common::person_avatar;

/// Desktop's `NAME_FIELD_MAX_GRAPHEMES` and `NAME_FIELD_MAX_BYTES`.
const NAME_MAX_GRAPHEMES: usize = 128;
const NAME_MAX_BYTES: usize = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    SelectContact,
    Share,
    EditName,
    SelectPhoto,
}

/// The person picked, and what goes of them.
pub struct Picked {
    pub candidate: ShareCandidate,
    pub draft: ShareContactDraft,
    pub selection: ShareContactSelection,
    color: u8,
}

/// The name editor's fields, in Desktop's order.
struct NameFields {
    prefix: Entity<InputState>,
    given: Entity<InputState>,
    middle: Entity<InputState>,
    family: Entity<InputState>,
    suffix: Entity<InputState>,
    organization: Entity<InputState>,
}

pub struct ShareContactDialog {
    state: Entity<AppState>,
    recipient: ConversationId,
    /// `None` while they load.
    pub candidates: Option<Vec<ShareCandidate>>,
    search: Entity<InputState>,
    pub step: Step,
    pub picked: Option<Picked>,
    names: NameFields,
    /// The photo picked on "Select a photo": an index into the draft's
    /// options, `None` for the default avatar.
    photo_choice: Option<usize>,
    /// Counts picks, so only the latest one's draft is shown.
    picks: u64,
}

impl ShareContactDialog {
    /// Picks a candidate and loads what Desktop's draft starts from.
    pub fn pick(&mut self, candidate: ShareCandidate, cx: &mut gpui_kit::Context<Self>) {
        let state = self.state.read(cx);
        let backend = state.backend.clone();
        let our_phone = state.account.as_ref().and_then(|a| a.phone.clone());
        let color = state.conversation(&candidate.id).map_or_else(
            || candidate.id.as_direct().map_or(0, signal_store::avatar_color),
            |c| c.color,
        );
        self.picks += 1;
        let pick = self.picks;
        let db = backend.db().clone();
        let read = {
            let candidate = candidate.clone();
            backend.spawn(async move { share_contact::read_draft(&db, &candidate).await })
        };
        cx.spawn(async move |this, cx| {
            let draft = share_contact::with_e164_as_missing_name(read.await, our_phone.as_deref());
            let _ = this.update(cx, |this, cx| {
                // Another was picked meanwhile, or the dialog went back.
                if this.picks != pick || this.step != Step::SelectContact {
                    return;
                }
                let selection = share_contact::default_selection(&draft);
                this.picked = Some(Picked { candidate, draft, selection, color });
                this.step = Step::Share;
                cx.notify();
            });
        })
        .detach();
    }

    /// What would be sent now.
    pub fn share(&self) -> Option<share_contact::ShareContact> {
        self.picked.as_ref().map(|p| apply_selection(&p.draft, p.selection))
    }

    fn back(&mut self) {
        self.step = match self.step {
            Step::EditName | Step::SelectPhoto => Step::Share,
            Step::Share | Step::SelectContact => {
                self.picked = None;
                Step::SelectContact
            }
        };
    }

    fn edit_name(&mut self, window: &mut Window, cx: &mut gpui_kit::Context<Self>) {
        let Some(picked) = &self.picked else { return };
        let name = picked.draft.name.clone().unwrap_or_default();
        let organization = picked.draft.organization.clone().unwrap_or_default();
        let fields = [
            (&self.names.prefix, name.prefix),
            (&self.names.given, name.given),
            (&self.names.middle, name.middle),
            (&self.names.family, name.family),
            (&self.names.suffix, name.suffix),
            (&self.names.organization, organization),
        ];
        for (input, value) in fields {
            input.update(cx, |input, cx| input.set_value(value, window, cx));
        }
        self.names.given.update(cx, |input, cx| input.focus(window, cx));
        self.step = Step::EditName;
    }

    /// Desktop's `EditNameStep` "Done": needs a first or last name.
    fn can_save_name(&self, cx: &App) -> bool {
        !self.names.given.read(cx).value().trim().is_empty() || !self.names.family.read(cx).value().trim().is_empty()
    }

    fn save_name(&mut self, cx: &App) {
        let field = |input: &Entity<InputState>| limit_name(input.read(cx).value().trim());
        let name = ContactName {
            prefix: field(&self.names.prefix),
            given: field(&self.names.given),
            middle: field(&self.names.middle),
            family: field(&self.names.family),
            suffix: field(&self.names.suffix),
        };
        let organization = Some(field(&self.names.organization)).filter(|o| !o.is_empty());
        if let Some(picked) = &mut self.picked {
            picked.draft.name = Some(name);
            picked.draft.organization = organization;
        }
        self.step = Step::Share;
    }

    fn select_photo(&mut self) {
        let Some(picked) = &self.picked else { return };
        self.photo_choice = picked.draft.avatar.as_ref().and_then(|a| picked.draft.avatar_options.iter().position(|o| o == a));
        self.step = Step::SelectPhoto;
    }

    fn save_photo(&mut self) {
        if let Some(picked) = &mut self.picked {
            picked.draft.avatar = self.photo_choice.and_then(|ix| picked.draft.avatar_options.get(ix).cloned());
        }
        self.step = Step::Share;
    }

    /// The candidates the search leaves.
    pub fn visible(&self, cx: &App) -> Vec<ShareCandidate> {
        let query = self.search.read(cx).value().to_string();
        self.candidates.iter().flatten().filter(|c| share_contact::matches(c, &query)).cloned().collect()
    }
}

/// A name field cut to Desktop's limits.
fn limit_name(value: &str) -> String {
    let mut out = String::new();
    for grapheme in value.graphemes(true).take(NAME_MAX_GRAPHEMES) {
        if out.len() + grapheme.len() > NAME_MAX_BYTES {
            break;
        }
        out.push_str(grapheme);
    }
    out
}

/// Opens the dialog for sharing a contact in `recipient`.
pub fn open(state: Entity<AppState>, recipient: ConversationId, window: &mut Window, cx: &mut App) -> Option<Entity<ShareContactDialog>> {
    if window.has_active_dialog(cx) {
        return None;
    }
    let input = |placeholder: &'static str, window: &mut Window, cx: &mut App| cx.new(|cx| InputState::new(window, cx).placeholder(placeholder));
    let names = NameFields {
        prefix: input("Prefix", window, cx),
        given: input("First name", window, cx),
        middle: input("Middle name", window, cx),
        family: input("Last name", window, cx),
        suffix: input("Suffix", window, cx),
        organization: input("Organization", window, cx),
    };
    let search = input("Search", window, cx);
    search.update(cx, |input, cx| input.focus(window, cx));
    let dialog = cx.new(|_| ShareContactDialog {
        state: state.clone(),
        recipient: recipient.clone(),
        candidates: None,
        search,
        step: Step::SelectContact,
        picked: None,
        names,
        photo_choice: None,
        picks: 0,
    });
    let backend = state.read(cx).backend.clone();
    let db = backend.db().clone();
    let load = backend.spawn(async move { share_contact::candidates(&db, &recipient).await });
    let loaded = dialog.clone();
    cx.spawn(async move |cx| {
        let candidates = load.await;
        loaded.update(cx, |d, cx| {
            d.candidates = Some(candidates);
            cx.notify();
        });
    })
    .detach();
    let shown = dialog.clone();
    window.open_dialog(cx, move |d, window, cx| render(&dialog, d, window, cx));
    Some(shown)
}

/// A round check, filled when on (Desktop's round `AxoCheckbox`).
fn check(on: bool, palette: Palette) -> AnyElement {
    div()
        .size(px(20.))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .border_2()
        .border_color(if on { palette.accent } else { palette.text_secondary })
        .when(on, |d| d.bg(palette.accent).child(Icon::new(IconName::Check).size_3().text_color(palette.on_accent)))
        .into_any_element()
}

fn clickable(id: impl Into<SharedString>, palette: Palette) -> gpui_kit::Stateful<gpui_kit::Div> {
    let id: SharedString = id.into();
    let selector = id.to_string();
    div()
        .id(id)
        .debug_selector(move || selector.clone())
        .flex()
        .items_center()
        .gap_3()
        .px_2()
        .py_1p5()
        .rounded_lg()
        .cursor_pointer()
        .hover(|d| d.bg(palette.row_hover))
}

/// A title with Desktop's back arrow before it.
fn back_title(dialog: &Entity<ShareContactDialog>, title: &'static str) -> AnyElement {
    let back = dialog.clone();
    div()
        .flex()
        .items_center()
        .gap_2()
        .child(
            Button::new("share-contact-back").ghost().small().icon(IconName::ChevronLeft).tooltip("Back").on_click(move |_, window, cx| {
                back.update(cx, |d, _| d.back());
                window.refresh();
            }),
        )
        .child(title)
        .into_any_element()
}

fn render(dialog: &Entity<ShareContactDialog>, d: Dialog, window: &mut Window, cx: &mut App) -> Dialog {
    let palette = Palette::get(cx);
    let step = dialog.read(cx).step;
    let _ = window;
    match step {
        Step::SelectContact => render_select(dialog, d, palette, cx),
        Step::Share => render_share(dialog, d, palette, cx),
        Step::EditName => render_edit_name(dialog, d, cx),
        Step::SelectPhoto => render_select_photo(dialog, d, palette, cx),
    }
}

fn render_select(dialog: &Entity<ShareContactDialog>, d: Dialog, palette: Palette, cx: &mut App) -> Dialog {
    let this = dialog.read(cx);
    let visible = this.visible(cx);
    let query = this.search.read(cx).value().trim().to_owned();
    let loading = this.candidates.is_none();
    let state = this.state.read(cx);
    let rows: Vec<AnyElement> = visible
        .into_iter()
        .enumerate()
        .map(|(ix, candidate)| {
            let chat = state.conversation(&candidate.id);
            let color = chat.map_or_else(|| candidate.id.as_direct().map_or(0, signal_store::avatar_color), |c| c.color);
            let photo = candidate
                .id
                .as_direct()
                .and_then(|sid| state.photo_of(sid))
                .map(|p| p.to_string_lossy().into_owned())
                .or_else(|| chat.and_then(|c| c.avatar_path.clone()));
            let pick = dialog.clone();
            let picked = candidate.clone();
            clickable(format!("share-contact-candidate-{ix}"), palette)
                .on_click(move |_, window, cx| {
                    pick.update(cx, |d, cx| d.pick(picked.clone(), cx));
                    window.refresh();
                })
                .child(crate::views::common::person_photo(
                    state,
                    candidate.id.as_direct().unwrap_or_default(),
                    photo.as_deref().map(std::path::Path::new),
                    &candidate.title,
                    color,
                    px(36.),
                ))
                .child(div().flex_1().min_w_0().truncate().child(candidate.title.clone()))
                .into_any_element()
        })
        .collect();
    let empty = (!loading && rows.is_empty()).then(|| {
        let text = if query.is_empty() { "No contacts".to_owned() } else { format!("No results for “{query}”") };
        div().debug_selector(|| "share-contact-empty".into()).p_4().text_color(palette.text_secondary).child(text)
    });
    d.title("Select contact").w(px(440.)).child(
        div()
            .debug_selector(|| "share-contact-select".into())
            .flex()
            .flex_col()
            .gap_2()
            .child(Input::new(&this.search))
            .child(div().id("share-contact-candidates").h(px(440.)).overflow_y_scroll().children(rows).children(empty)),
    )
}

/// A detail that's checked to go (Desktop's `FieldItem`).
fn field_item(
    dialog: &Entity<ShareContactDialog>,
    id: &'static str,
    label: &'static str,
    value: String,
    on: bool,
    toggle: fn(&mut ShareContactSelection),
    palette: Palette,
) -> AnyElement {
    let dialog = dialog.clone();
    clickable(id, palette)
        .on_click(move |_, window, cx| {
            dialog.update(cx, |d, _| {
                if let Some(picked) = &mut d.picked {
                    toggle(&mut picked.selection);
                }
            });
            window.refresh();
        })
        .child(check(on, palette))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(div().text_xs().text_color(palette.text_secondary).child(label))
                .child(div().text_sm().whitespace_normal().child(value)),
        )
        .into_any_element()
}

/// A detail that always goes and can be edited (Desktop's `EditableFieldItem`).
fn editable_item(id: &'static str, edit_label: &'static str, content: AnyElement, palette: Palette) -> gpui_kit::Stateful<gpui_kit::Div> {
    clickable(id, palette)
        .child(check(true, palette))
        .child(div().flex_1().min_w_0().child(content))
        .child(
            div()
                .flex_none()
                .child(Icon::new(IconName::Pencil).size_4().text_color(palette.text_secondary))
                .id(SharedString::from(format!("{id}-edit")))
                .tooltip(move |window, cx| gpui_kit::component::tooltip::Tooltip::new(edit_label).build(window, cx)),
        )
}

/// The photo Desktop shows for `avatar` (none: the initials).
/// Its initials come from the draft's name as the sent contact's will
/// (Desktop's `getContactInitials`), else the contact's title.
fn draft_avatar(picked: &Picked, avatar: Option<&share_contact::AvatarOption>, size: f32) -> AnyElement {
    let draft = &picked.draft;
    let name = draft
        .name
        .as_ref()
        .and_then(|n| signal_store::combine_names(&n.given, &n.family).or_else(|| n.full()))
        .or_else(|| draft.organization.clone().filter(|o| !o.is_empty()))
        .unwrap_or_else(|| picked.candidate.title.clone());
    person_avatar(avatar.map(|a| std::path::Path::new(a.path.as_str())), &name, picked.color, px(size))
}

fn photo_label(avatar: Option<&share_contact::AvatarOption>) -> &'static str {
    match avatar {
        None => "Default avatar",
        Some(a) if a.is_profile => "Profile photo",
        Some(_) => "Contact photo",
    }
}

fn render_share(dialog: &Entity<ShareContactDialog>, d: Dialog, palette: Palette, cx: &mut App) -> Dialog {
    let this = dialog.read(cx);
    let Some(picked) = &this.picked else { return d };
    let our_phone = this.state.read(cx).account.as_ref().and_then(|a| a.phone.clone());
    let recipient = this.state.read(cx).conversation(&this.recipient).map(|c| c.title.clone()).unwrap_or_default();
    let draft = &picked.draft;
    let selection = picked.selection;
    let mut items: Vec<AnyElement> = Vec::new();
    if !draft.avatar_options.is_empty() {
        let edit = dialog.clone();
        let photo = div()
            .flex()
            .items_center()
            .gap_2()
            .child(draft_avatar(picked, draft.avatar.as_ref(), 48.))
            .child(div().text_xs().text_color(palette.text_secondary).child(photo_label(draft.avatar.as_ref())));
        items.push(
            editable_item("share-contact-photo", "Edit photo", photo.into_any_element(), palette)
                .on_click(move |_, window, cx| {
                    edit.update(cx, |d, _| d.select_photo());
                    window.refresh();
                })
                .into_any_element(),
        );
    }
    if let Some(name) = draft.name.as_ref().and_then(ContactName::full) {
        let edit = dialog.clone();
        items.push(
            editable_item("share-contact-name", "Edit name", div().text_sm().child(name).into_any_element(), palette)
                .on_click(move |_, window, cx| {
                    edit.update(cx, |d, cx| d.edit_name(window, cx));
                    window.refresh();
                })
                .into_any_element(),
        );
    }
    if let Some(e164) = &draft.e164 {
        let value = format_phone(e164, our_phone.as_deref());
        items.push(field_item(dialog, "share-contact-phone", "Phone", value, selection.e164, |s| s.e164 = !s.e164, palette));
    }
    if let Some((given, family)) = &draft.nickname
        && let Some(nickname) = signal_store::combine_names(given, family)
    {
        items.push(field_item(dialog, "share-contact-nickname", "Nickname", nickname, selection.nickname, |s| s.nickname = !s.nickname, palette));
    }
    if let Some(note) = &draft.note {
        items.push(field_item(dialog, "share-contact-note", "Notes", note.clone(), selection.note, |s| s.note = !s.note, palette));
    }
    let share = apply_selection(draft, selection);
    let ready = can_share(&share);
    let description = draft.aci.is_some().then(|| {
        div()
            .px_2()
            .text_sm()
            .text_color(palette.text_secondary)
            .child(format!("Anyone you share this profile with will be able to message {}.", picked.candidate.title))
    });
    let (state, recipient_id, send) = (this.state.clone(), this.recipient.clone(), dialog.clone());
    d.title(back_title(dialog, "Share contact"))
        .w(px(440.))
        .child(
            div()
                .debug_selector(|| "share-contact-share".into())
                .flex()
                .flex_col()
                .gap_3()
                .children(description)
                .child(div().flex().flex_col().children(items)),
        )
        .footer(
            DialogFooter::new().child(
                div()
                    .w_full()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(div().flex_1().min_w_0().truncate().text_sm().text_color(palette.text_secondary).child(recipient))
                    .child(
                        div().debug_selector(|| "share-contact-send".into()).child(
                            Button::new("share-contact-send")
                                .primary()
                                .rounded_full()
                                .icon(IconName::Send)
                                .tooltip("Send contact")
                                .disabled(!ready)
                                .on_click(move |_, window, cx| {
                                    let Some(share) = send.read(cx).share().filter(can_share) else { return };
                                    state.read(cx).send(Command::ShareContact { conversation: recipient_id.clone(), share: Box::new(share) });
                                    window.close_all_dialogs(cx);
                                }),
                        ),
                    ),
            ),
        )
}

fn render_edit_name(dialog: &Entity<ShareContactDialog>, d: Dialog, cx: &mut App) -> Dialog {
    let this = dialog.read(cx);
    let names = &this.names;
    let ready = this.can_save_name(cx);
    let fields = [&names.prefix, &names.given, &names.middle, &names.family, &names.suffix, &names.organization];
    let (cancel, save) = (dialog.clone(), dialog.clone());
    d.title(back_title(dialog, "Edit name"))
        .w(px(440.))
        .close_button(false)
        .child(div().debug_selector(|| "share-contact-edit-name".into()).flex().flex_col().gap_2().children(fields.map(Input::new)))
        .footer(
            DialogFooter::new()
                .child(Button::new("share-contact-name-cancel").label("Cancel").outline().on_click(move |_, window, cx| {
                    cancel.update(cx, |d, _| d.back());
                    window.refresh();
                }))
                .child(
                    div().debug_selector(|| "share-contact-name-done".into()).child(
                        Button::new("share-contact-name-done").label("Done").primary().disabled(!ready).on_click(move |_, window, cx| {
                            save.update(cx, |d, cx| {
                                if d.can_save_name(cx) {
                                    d.save_name(cx);
                                }
                            });
                            window.refresh();
                        }),
                    ),
                ),
        )
}

fn render_select_photo(dialog: &Entity<ShareContactDialog>, d: Dialog, palette: Palette, cx: &mut App) -> Dialog {
    let this = dialog.read(cx);
    let Some(picked) = &this.picked else { return d };
    let choice = this.photo_choice;
    let mut options: Vec<(Option<usize>, Option<&share_contact::AvatarOption>)> =
        picked.draft.avatar_options.iter().enumerate().map(|(ix, a)| (Some(ix), Some(a))).collect();
    options.push((None, None));
    let tiles = options.into_iter().map(|(ix, avatar)| {
        let on = choice == ix;
        let pick = dialog.clone();
        let id = format!("share-contact-photo-{}", ix.map_or("default".to_owned(), |i| i.to_string()));
        div()
            .id(SharedString::from(id.clone()))
            .debug_selector(move || id.clone())
            .flex()
            .flex_col()
            .items_center()
            .gap_1()
            .cursor_pointer()
            .on_click(move |_, window, cx| {
                pick.update(cx, |d, _| d.photo_choice = ix);
                window.refresh();
            })
            .child(
                div()
                    .relative()
                    .child(draft_avatar(picked, avatar, 80.))
                    .child(div().absolute().right_0().bottom_0().rounded_full().bg(palette.background).child(check(on, palette))),
            )
            .child(div().text_xs().text_color(palette.text_secondary).child(photo_label(avatar)))
    });
    let (cancel, save) = (dialog.clone(), dialog.clone());
    d.title(back_title(dialog, "Select a photo"))
        .w(px(360.))
        .close_button(false)
        .child(div().debug_selector(|| "share-contact-select-photo".into()).flex().justify_center().gap_3().pt_2().pb_5().children(tiles))
        .footer(
            DialogFooter::new()
                .child(Button::new("share-contact-photo-cancel").label("Cancel").outline().on_click(move |_, window, cx| {
                    cancel.update(cx, |d, _| d.back());
                    window.refresh();
                }))
                .child(Button::new("share-contact-photo-done").label("Done").primary().on_click(move |_, window, cx| {
                    save.update(cx, |d, _| d.save_photo());
                    window.refresh();
                })),
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::views::test_support::{click_at, demo, press, type_into, wait};
    use gpui_kit::TestAppContext;

    const MAYA: &str = "00000000-0000-4000-8000-00000000a11c";
    const JORDAN: &str = "00000000-0000-4000-8000-0000000000b0";

    #[test]
    fn name_fields_stop_at_desktops_limits() {
        assert_eq!(limit_name("Paige"), "Paige");
        assert_eq!(limit_name(&"a".repeat(200)).len(), NAME_MAX_GRAPHEMES);
        // Four bytes each: the byte limit comes first.
        assert_eq!(limit_name(&"🎉".repeat(200)).len(), NAME_MAX_BYTES);
    }

    // Linux's gpui test platform types and measures differently; this asserts macOS's
    // text system (the pre-port Linux tree failed it the same way).
    #[cfg_attr(target_os = "linux", ignore)]
    #[gpui_kit::test]
    fn sharing_a_contact_goes_through_desktops_steps(cx: &mut TestAppContext) {
        let (backend, state, mut cx, _dir) = demo(&mut *cx);
        wait(&mut cx, "the chats", &mut |cx| state.read_with(cx, |s, _| s.conversation(&ConversationId::direct(JORDAN)).is_some()));
        let jordan = ConversationId::direct(JORDAN);
        let dialog = cx.update(|window, cx| open(state.clone(), jordan.clone(), window, cx)).expect("dialog");
        wait(&mut cx, "the candidates", &mut |cx| dialog.read_with(cx, |d, _| d.candidates.is_some()));
        // Not the chat's own person, by name, with Maya first.
        let titles: Vec<String> = dialog.read_with(&cx, |d, _| d.candidates.iter().flatten().map(|c| c.title.clone()).collect());
        assert!(!titles.contains(&"Jordan Reyes".to_owned()), "{titles:?}");
        assert!(titles.contains(&"Maya Chen".to_owned()), "{titles:?}");
        let mut sorted = titles.clone();
        sorted.sort_by_key(|t| t.to_lowercase());
        assert_eq!(titles, sorted);
        // The search narrows them.
        let search = dialog.read_with(&cx, |d, _| d.search.clone());
        type_into(&mut cx, &search, "maya");
        assert_eq!(dialog.read_with(&cx, |d, cx| d.visible(cx).len()), 1);
        type_into(&mut cx, &search, "zzz");
        wait(&mut cx, "no results", &mut |cx| crate::views::test_support::shows(cx, "share-contact-empty"));
        type_into(&mut cx, &search, "maya");
        click_at(&mut cx, "share-contact-candidate-0");
        wait(&mut cx, "the share step", &mut |cx| dialog.read_with(cx, |d, _| d.step == Step::Share));
        // The number is checked, the name goes; Maya is on Signal.
        let share = dialog.read_with(&cx, |d, _| d.share()).unwrap();
        assert_eq!(share.contact.aci.as_deref(), Some(MAYA));
        assert_eq!(share.contact.display_name().as_deref(), Some("Maya Chen"));
        assert_eq!(share.contact.numbers.len(), 1);
        // Unchecking the number still leaves her name and account.
        click_at(&mut cx, "share-contact-phone");
        let share = dialog.read_with(&cx, |d, _| d.share()).unwrap();
        assert!(share.contact.numbers.is_empty() && can_share(&share));
        // Editing the name: Done needs a first or last name.
        click_at(&mut cx, "share-contact-name");
        assert_eq!(dialog.read_with(&cx, |d, _| d.step), Step::EditName);
        let (given, family) = dialog.read_with(&cx, |d, _| (d.names.given.clone(), d.names.family.clone()));
        type_into(&mut cx, &given, "");
        type_into(&mut cx, &family, "");
        assert!(!dialog.read_with(&cx, |d, cx| d.can_save_name(cx)));
        type_into(&mut cx, &given, "Maya");
        type_into(&mut cx, &family, "C.");
        press(&mut cx, "share-contact-name-done");
        assert_eq!(dialog.read_with(&cx, |d, _| d.step), Step::Share);
        let share = dialog.read_with(&cx, |d, _| d.share()).unwrap();
        assert_eq!(share.contact.display_name().as_deref(), Some("Maya C."));
        // Sent: the chat shows it as ours.
        press(&mut cx, "share-contact-send");
        wait(&mut cx, "the sent contact", &mut |cx| {
            state.read_with(cx, |s, _| {
                s.conversation(&jordan).and_then(|c| c.last_message_preview.clone()).is_some_and(|p| p == "👤 Maya C.")
            })
        });
        backend.shutdown();
    }
}
