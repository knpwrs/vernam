//! Desktop's Chats page: address book photos, keeping muted chats
//! archived, link previews, emoticons, the text formatting popover, chat
//! folders, and importing the phone's contacts. (Its skin tone and chat
//! export aren't here.) "Keep deleted messages" is Vernam's own.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{Disableable as _, Sizable as _, WindowExt as _};
use gpui_kit::prelude::*;
use gpui_kit::{AnyElement, App, Entity, SharedString, Window, div, px};
use signal_backend::AccountSetting;

use super::{Settings, list, switch};
use crate::prefs::Toggle;
use crate::state::{AppState, ContactSync};
use crate::theme::Palette;

pub(super) fn render(settings: &Entity<Settings>, palette: Palette, cx: &mut App) -> AnyElement {
    let state = settings.read(cx).state.clone();
    let (prefs, sync) = {
        let s = state.read(cx);
        (s.prefs.clone(), s.contact_sync)
    };
    let flag = |id: &'static str, label: &'static str, description: &'static str, setting: AccountSetting| {
        let state = state.clone();
        switch(id, label, Some(description.into()), prefs.flag(setting), false, palette, move |on, _, cx| {
            state.update(cx, |s, cx| s.set_account_setting(setting, on, cx))
        })
    };
    let emoji_state = state.clone();
    let formatting_state = state.clone();
    let spell_state = state.clone();
    let keep_state = state.clone();
    let sync_state = state.clone();
    let pending = matches!(sync, ContactSync::Pending(_));
    div()
        .flex()
        .flex_col()
        .gap_5()
        .child(list(
            None,
            None,
            [
                flag(
                    "address-book-photos",
                    "Use address book photos",
                    "Display contact photos from your address book if available.",
                    AccountSetting::PreferContactAvatars,
                ),
                flag(
                    "keep-muted-chats-archived",
                    "Keep muted chats archived",
                    "Muted chats that are archived will remain archived when a new message arrives.",
                    AccountSetting::KeepMutedChatsArchived,
                ),
                switch(
                    "keep-deleted-messages",
                    "Keep deleted messages",
                    Some(
                        "When someone deletes a message for everyone, keep it here, marked as deleted. \
                         Applies to messages deleted after you turn this on."
                            .into(),
                    ),
                    prefs.on(Toggle::KeepDeletedMessages),
                    false,
                    palette,
                    move |on, window, cx| {
                        if on {
                            keep_state.update(cx, |s, cx| s.set_toggle(Toggle::KeepDeletedMessages, true, cx));
                        } else {
                            confirm_keep_deleted_off(keep_state.clone(), window, cx);
                        }
                    },
                ),
            ],
            palette,
        ))
        .child(list(
            Some("Text input"),
            None,
            [
                // Desktop applies it at the next start; here at once.
                switch(
                    "spell-check",
                    "Spell check text entered in message composition box",
                    None,
                    prefs.on(Toggle::SpellCheck),
                    false,
                    palette,
                    move |on, _, cx| spell_state.update(cx, |s, cx| s.set_toggle(Toggle::SpellCheck, on, cx)),
                ),
                switch(
                    "text-formatting",
                    "Show text formatting popover when text is selected",
                    None,
                    prefs.on(Toggle::TextFormatting),
                    false,
                    palette,
                    move |on, _, cx| formatting_state.update(cx, |s, cx| s.set_toggle(Toggle::TextFormatting, on, cx)),
                ),
                flag(
                    "link-previews",
                    "Generate link previews",
                    "Retrieve link previews directly from websites for messages you send.",
                    AccountSetting::LinkPreviews,
                ),
                switch(
                    "auto-convert-emoji",
                    "Convert typed emoticons to emoji",
                    Some("For example, :-) will be converted to 🙂".into()),
                    prefs.auto_convert_emoji,
                    false,
                    palette,
                    move |on, _, cx| emoji_state.update(cx, |s, cx| s.set_auto_convert_emoji(on, cx)),
                ),
            ],
            palette,
        ))
        .child(list(Some("Chat folders"), None, [super::chat_folders::row(settings, palette, cx)], palette))
        .child(list(
            None,
            None,
            [div()
                .flex()
                .items_center()
                .gap_3()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap_0p5()
                        .child(div().text_sm().child("Import contacts"))
                        .child(div().text_xs().text_color(palette.text_secondary).child(import_description(prefs.contacts_synced_at)))
                        .when(sync == ContactSync::Failed, |d| {
                            d.child(div().text_xs().text_color(palette.danger).child(
                                "Import failed. Make sure your computer and your phone are connected to the internet.",
                            ))
                        }),
                )
                .child(
                    Button::new("import-contacts")
                        .outline()
                        .small()
                        .label("Import now")
                        .loading(pending)
                        .disabled(pending)
                        .on_click(move |_, window, cx| {
                            sync_state.update(cx, |s, cx| s.sync_contacts(cx));
                            window.refresh();
                        }),
                )
                .into_any_element()],
            palette,
        ))
        .into_any_element()
}

/// Turning "Keep deleted messages" off asks what becomes of the messages
/// it kept: they stay, or lose what they said as any deleted message
/// does. Cancel leaves it on.
fn confirm_keep_deleted_off(state: Entity<AppState>, window: &mut Window, cx: &mut App) {
    use gpui_kit::component::dialog::DialogFooter;
    window.open_dialog(cx, move |dialog, _, _| {
        let (keep, erase) = (state.clone(), state.clone());
        dialog
            .w(px(420.))
            .title("Turn off Keep deleted messages?")
            .child(div().text_sm().child(
                "Messages already kept after someone deleted them can stay, or lose what they said, as other deleted messages do.",
            ))
            .footer(
                DialogFooter::new()
                    .child(Button::new("keep-deleted-cancel").label("Cancel").outline().on_click(|_, window, cx| window.close_dialog(cx)))
                    .child(Button::new("keep-deleted-keep").label("Keep them").outline().on_click(move |_, window, cx| {
                        keep.update(cx, |s, cx| s.set_toggle(Toggle::KeepDeletedMessages, false, cx));
                        window.close_dialog(cx);
                        window.refresh();
                    }))
                    .child(Button::new("keep-deleted-erase").label("Erase them").danger().on_click(move |_, window, cx| {
                        erase.update(cx, |s, cx| s.erase_kept_deletes(cx));
                        window.close_dialog(cx);
                        window.refresh();
                    })),
            )
    });
}

/// Desktop's `syncExplanation` and `Preferences--lastSynced` ("Last import
/// at {date} {time}"; left out before the first, where Desktop shows 1970).
fn import_description(synced_at: Option<i64>) -> SharedString {
    use chrono::TimeZone as _;
    let at = synced_at.and_then(|at| chrono::Local.timestamp_millis_opt(at).single());
    let last = at.map(|t| format!(" Last import at {} {}", t.format("%-m/%-d/%Y"), t.format("%-I:%M:%S %p"))).unwrap_or_default();
    format!("Import all Signal groups and contacts from your mobile device.{last}").into()
}
