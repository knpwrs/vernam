//! Desktop's "Forward To" dialog (`ForwardMessagesModal`): pick up to five
//! chats, then Continue. One message that can be edited (not a sticker,
//! voice message or shared contact) goes through Desktop's forward editor
//! first: its text can change or go, and its attachments and link preview
//! can be removed, before Send.

use std::collections::BTreeSet;

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::DialogFooter;
use gpui_kit::component::input::{Input, InputState, Textarea, TextareaState};
use gpui_kit::component::{Disableable as _, Icon, Sizable as _, WindowExt as _};
use gpui_kit::prelude::*;
use gpui_kit::{AnyElement, App, Entity, Focusable as _, FontWeight, ObjectFit, SharedString, Window, div, img, px};
use signal_backend::Command;
use signal_backend::forwards::ForwardEdit;
use signal_store::{ConversationId, ConversationKind, Message};

use crate::state::AppState;
use crate::theme::Palette;
use crate::views::common::conversation_avatar;

/// Desktop's `MAX_FORWARD`.
pub const MAX_CHATS: usize = 5;
/// Desktop forwards at most this many messages at once.
pub const MAX_MESSAGES: usize = 30;

/// The dialog's state: the chats picked, and the editor's.
pub(crate) struct Forward {
    picked: BTreeSet<ConversationId>,
    /// The one message forwarded, when it can be edited.
    draft: Option<Draft>,
    /// The editor shows in place of the chats.
    editing: bool,
}

pub(crate) struct Draft {
    message: Message,
    body: Entity<TextareaState>,
    /// Redraws the dialog as the text changes: Send and the link preview
    /// follow it.
    _typing: gpui_kit::Subscription,
    /// Its attachments still going, by index.
    attachments: Vec<usize>,
    link_preview: bool,
}

impl Draft {
    fn edit(&self, cx: &App) -> ForwardEdit {
        ForwardEdit {
            body: self.body.read(cx).value().to_string(),
            attachments: self.attachments.clone(),
            link_preview: self.link_preview,
        }
    }
}

/// Opens the dialog for `message_ids`; for one, after loading it to see
/// whether it can be edited.
pub fn open(state: Entity<AppState>, message_ids: Vec<i64>, window: &mut Window, cx: &mut App) {
    let [id] = message_ids[..] else {
        return show(state, message_ids, None, window, cx);
    };
    let backend = state.read(cx).backend.clone();
    let db = backend.db().clone();
    let query = backend.spawn(async move { db.message(id).await });
    let window_handle = window.window_handle();
    cx.spawn(async move |cx| {
        let message = match query.await {
            Ok(message) => message,
            Err(error) => {
                tracing::warn!(%error, "couldn't load a message to forward");
                None
            }
        };
        let editable = message.filter(signal_backend::forwards::editable);
        let _ = window_handle.update(cx, |_, window, cx| show(state, message_ids, editable, window, cx));
    })
    .detach();
}

fn show(state: Entity<AppState>, message_ids: Vec<i64>, editable: Option<Message>, window: &mut Window, cx: &mut App) {
    let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search"));
    let draft = editable.map(|message| {
        let text = message.body.clone();
        let body = cx.new(|cx| {
            let mut body = TextareaState::new(window, cx).auto_grow(2, 8).placeholder("Add a message");
            if !text.is_empty() {
                body.set_value(text, window, cx);
            }
            body
        });
        let _typing = cx.subscribe(&body, |_, _: &gpui_kit::component::input::InputEvent, cx| cx.refresh_windows());
        Draft { attachments: (0..message.attachments.len()).collect(), link_preview: message.link_preview.is_some(), message, body, _typing }
    });
    let forward = cx.new(|_| Forward { picked: BTreeSet::new(), draft, editing: false });
    window.open_dialog(cx, move |dialog, window, cx| {
        let palette = Palette::get(cx);
        let send = {
            let (state, forward, ids) = (state.clone(), forward.clone(), message_ids.clone());
            move |window: &mut Window, cx: &mut App| {
                let f = forward.read(cx);
                let conversations: Vec<ConversationId> = f.picked.iter().cloned().collect();
                let edit = f.draft.as_ref().filter(|_| f.editing).map(|d| d.edit(cx));
                state.read(cx).send(Command::Forward { message_ids: ids.clone(), conversations, edit });
                window.close_all_dialogs(cx);
            }
        };
        let dialog = dialog.title("Forward To").w(px(420.));
        if forward.read(cx).editing {
            let ready = forward
                .read(cx)
                .draft
                .as_ref()
                .is_some_and(|d| signal_backend::forwards::content(&d.message, Some(&d.edit(cx))).is_some());
            let back = forward.clone();
            return dialog.child(editor(&forward, palette, window, cx)).footer(
                DialogFooter::new()
                    .child(Button::new("forward-back").label("Back").outline().on_click(move |_, window, cx| {
                        back.update(cx, |f, _| f.editing = false);
                        window.refresh();
                    }))
                    .child(
                        Button::new("forward-send")
                            .label("Send")
                            .primary()
                            .disabled(!ready)
                            .on_click(move |_, window, cx| send(window, cx)),
                    ),
            );
        }
        let query = search.read(cx).value().trim().to_lowercase();
        let chosen = forward.read(cx).picked.clone();
        let full = chosen.len() >= MAX_CHATS;
        let chats: Vec<_> = state
            .read(cx)
            .conversations
            .iter()
            // Nothing can be sent to the Signal chat, or to an ended group
            // (Desktop's `maybeForwardMessages`).
            .filter(|c| !c.blocked && !c.id.is_signal() && !c.ended)
            .filter(|c| query.is_empty() || c.title.to_lowercase().contains(&query))
            .cloned()
            .collect();
        let rows = chats.into_iter().map(|chat| {
            let on = chosen.contains(&chat.id);
            let id = chat.id.clone();
            let forward = forward.clone();
            let title = if chat.kind == ConversationKind::NoteToSelf { "Note to Self".to_owned() } else { chat.title.clone() };
            let row_id = SharedString::from(format!("forward-{}", chat.id.0));
            div()
                .id(row_id.clone())
                .debug_selector(move || row_id.to_string())
                .flex()
                .items_center()
                .gap_3()
                .px_2()
                .py_1p5()
                .rounded_lg()
                .when(on || !full, |d| d.cursor_pointer().hover(|d| d.bg(palette.row_hover)))
                .when(!on && full, |d| d.opacity(0.5))
                .on_click(move |_, window, cx| {
                    forward.update(cx, |f, _| {
                        if !f.picked.remove(&id) && f.picked.len() < MAX_CHATS {
                            f.picked.insert(id.clone());
                        }
                    });
                    window.refresh();
                })
                .child(conversation_avatar(&chat, px(32.)))
                .child(div().flex_1().truncate().child(title))
                .child(
                    div()
                        .size(px(18.))
                        .rounded_full()
                        .border_2()
                        .border_color(if on { palette.accent } else { palette.text_secondary })
                        .when(on, |d| d.bg(palette.accent)),
                )
        });
        let to_editor = forward.clone();
        dialog
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(Input::new(&search))
                    .child(div().id("forward-chats").h(px(360.)).overflow_y_scroll().children(rows)),
            )
            .footer(
                DialogFooter::new()
                    .child(Button::new("forward-cancel").label("Cancel").outline().on_click(|_, window, cx| window.close_all_dialogs(cx)))
                    .child(
                        Button::new("forward-continue")
                            .label("Continue")
                            .primary()
                            .disabled(chosen.is_empty())
                            .on_click(move |_, window, cx| {
                                // Desktop's arrow to the editor, else its send.
                                if let Some(body) = to_editor.read(cx).draft.as_ref().map(|d| d.body.clone()) {
                                    to_editor.update(cx, |f, _| f.editing = true);
                                    // Ready to type, as Desktop's `isActive`.
                                    let handle = body.read(cx).focus_handle(cx);
                                    window.focus(&handle, cx);
                                    window.refresh();
                                } else {
                                    send(window, cx);
                                }
                            }),
                    ),
            )
    });
}

/// Desktop's `ForwardMessageEditor`: the link preview and attachments, each
/// with a button to leave it out, above the text. Every attachment has
/// downloaded (`forwards::editable`), as Desktop opens no forward dialog
/// until they have.
fn editor(forward: &Entity<Forward>, palette: Palette, _window: &mut Window, cx: &mut App) -> AnyElement {
    let Some(draft) = forward.read(cx).draft.as_ref() else { return div().into_any_element() };
    let remove = |id: SharedString, forward: Entity<Forward>, change: fn(&mut Draft, usize), ix: usize| {
        div()
            .id(id.clone())
            .debug_selector(move || id.to_string())
            .p_1()
            .flex_none()
            .cursor_pointer()
            .rounded_full()
            .bg(palette.input_bg)
            .on_click(move |_, window, cx| {
                forward.update(cx, |f, _| {
                    if let Some(draft) = f.draft.as_mut() {
                        change(draft, ix);
                    }
                });
                window.refresh();
            })
            .child(Icon::new(IconName::X).size_3().text_color(palette.text_secondary))
    };
    // Shown while it would go: kept, and its link still in the text.
    let text = draft.body.read(cx).value().to_string();
    let preview = draft
        .message
        .link_preview
        .as_ref()
        .filter(|preview| {
            draft.link_preview
                && (text.trim_end() == draft.message.body.trim_end() || signal_backend::forwards::links_to(&text, &preview.url))
        })
        .map(|preview| {
        let image = preview.image.as_ref().and_then(|i| i.local_path.clone());
        div()
            .flex()
            .items_center()
            .gap_3()
            .p_2()
            .rounded_lg()
            .bg(palette.input_bg)
            .when_some(image, |d, image| d.child(img(std::path::PathBuf::from(image)).size(px(48.)).aspect_square().flex_none().rounded_md().object_fit(ObjectFit::Cover)))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .when_some(preview.title.clone(), |d, title| {
                        d.child(div().text_sm().font_weight(FontWeight::SEMIBOLD).truncate().child(title))
                    })
                    .child(div().text_xs().text_color(palette.text_secondary).truncate().child(preview.url.clone())),
            )
            .child(remove("forward-remove-preview".into(), forward.clone(), |d, _| d.link_preview = false, 0))
    });
    let attachments = draft.attachments.iter().filter_map(|&ix| {
        let attachment = draft.message.attachments.get(ix)?;
        let picture = attachment.content_type.starts_with("image/");
        let thumbnail = if picture { attachment.local_path.clone() } else { attachment.thumbnail_path.clone() };
        let name = attachment.file_name.clone().unwrap_or_else(|| signal_store::attachment_preview(attachment).to_owned());
        Some(
            div()
                .relative()
                .size(px(72.))
                .flex_none()
                .rounded_lg()
                .overflow_hidden()
                .bg(palette.input_bg)
                .child(match thumbnail {
                    Some(path) => img(std::path::PathBuf::from(path)).size(px(72.)).aspect_square().rounded_lg().object_fit(ObjectFit::Cover).into_any_element(),
                    None => div().size_full().p_2().flex().items_end().text_xs().child(name).into_any_element(),
                })
                .child(
                    div().absolute().top_1().right_1().child(remove(
                        format!("forward-remove-attachment-{ix}").into(),
                        forward.clone(),
                        |d, ix| d.attachments.retain(|&i| i != ix),
                        ix,
                    )),
                ),
        )
    });
    div()
        .flex()
        .flex_col()
        .gap_3()
        .children(preview)
        .child(div().flex().flex_wrap().gap_2().children(attachments))
        .child(Textarea::new(&draft.body).small())
        .into_any_element()
}
