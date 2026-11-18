//! Scheduled messages' dialogs (`crate::scheduled`): when to send one, and
//! a chat's list of them, each to send now, reschedule or delete.

use chrono::{Local, NaiveTime};
use gpui_kit::assets::IconName;
use gpui_kit::component::{Disableable as _, Icon, Sizable as _};
use gpui_kit::component::WindowExt as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::date_picker::{DatePicker, DatePickerState};
use gpui_kit::component::dialog::DialogFooter;
use gpui_kit::component::time_field::{HourCycle, TimePrecision};
use gpui_kit::prelude::*;
use gpui_kit::{App, Context, Entity, SharedString, Window, div, px};
use signal_store::{ConversationId, ScheduledKind, ScheduledMessage};

use crate::state::AppState;
use crate::theme::Palette;

/// The time a custom choice starts at: 9 AM tomorrow.
const CUSTOM_HOUR: u32 = 9;

struct PickTime {
    /// The presets' times (unix millis), then the custom one.
    presets: Vec<(String, i64)>,
    choice: usize,
    custom: Entity<DatePickerState>,
    /// The custom time isn't in the future.
    past: bool,
}

impl PickTime {
    fn custom(&self) -> bool {
        self.choice == self.presets.len()
    }

    /// The time picked (unix millis), if there is one.
    fn picked(&self, cx: &App) -> Option<i64> {
        if !self.custom() {
            return self.presets.get(self.choice).map(|(_, at)| *at);
        }
        let naive = self.custom.read(cx).date_time().start()?;
        Some(crate::scheduled::local(naive)?.timestamp_millis())
    }
}

/// Asks when to send: a preset (in an hour, tonight, tomorrow morning) or a
/// date and time, in the future. `on_pick` gets it (unix millis).
pub fn pick_time(
    title: &'static str,
    confirm: &'static str,
    window: &mut Window,
    cx: &mut App,
    on_pick: impl Fn(i64, &mut Window, &mut App) + 'static,
) {
    let now = Local::now();
    let presets: Vec<(String, i64)> =
        crate::scheduled::presets(now).into_iter().map(|(label, at)| (label, at.timestamp_millis())).collect();
    let today = now.date_naive();
    let custom = cx.new(|cx| {
        let mut picker = DatePickerState::new(window, cx)
            .time_precision(TimePrecision::Minute)
            .hour_cycle(HourCycle::H12)
            .date_format("%a, %b %-d, %Y %-I:%M %p")
            .disabled_matcher(move |day: &chrono::NaiveDate| *day < today);
        let start = (today + chrono::Duration::days(1)).and_time(NaiveTime::from_hms_opt(CUSTOM_HOUR, 0, 0).unwrap_or_default());
        picker.set_date_time(start, window, cx);
        picker
    });
    let pick = cx.new(|_| PickTime { presets, choice: 0, custom, past: false });
    let on_pick = std::rc::Rc::new(on_pick);
    window.open_dialog(cx, move |dialog, _, cx| {
        let palette = Palette::get(cx);
        let this = pick.read(cx);
        let rows = this.presets.iter().map(|(label, _)| label.clone()).chain(["Custom date and time".to_owned()]);
        let rows: Vec<_> = rows
            .enumerate()
            .map(|(ix, label)| {
                let pick = pick.clone();
                choice_row(ix, label, ix == this.choice, palette, move |window, cx| {
                    pick.update(cx, |p, _| {
                        p.choice = ix;
                        p.past = false;
                    });
                    window.refresh();
                })
            })
            .collect();
        let custom = this.custom().then(|| this.custom.clone());
        let past = this.past;
        let (pick, on_pick) = (pick.clone(), on_pick.clone());
        dialog
            .title(title)
            .w(px(400.))
            .child(
                div()
                    .debug_selector(|| "schedule-dialog".into())
                    .flex()
                    .flex_col()
                    .gap_1()
                    .children(rows)
                    .when_some(custom, |d, custom| d.child(div().pt_2().child(DatePicker::new(&custom))))
                    .when(past, |d| {
                        d.child(div().pt_2().text_sm().text_color(palette.danger).child("Choose a time in the future."))
                    })
                    .child(div().pt_3().text_xs().text_color(palette.text_secondary).child(
                        "Scheduled messages are sent by this app, so it must be open at that time. If it isn't, the message is sent the next time it's open and online.",
                    )),
            )
            .footer(
                DialogFooter::new()
                    .child(Button::new("schedule-cancel").label("Cancel").outline().on_click(|_, window, cx| window.close_dialog(cx)))
                    .child(div().debug_selector(|| "schedule-confirm".into()).child(
                        Button::new("schedule-confirm").label(confirm).primary().on_click(move |_, window, cx| {
                            let now = Local::now().timestamp_millis();
                            match pick.read(cx).picked(cx).filter(|at| *at > now) {
                                Some(at) => {
                                    window.close_dialog(cx);
                                    on_pick(at, window, cx);
                                }
                                None => {
                                    pick.update(cx, |p, _| p.past = true);
                                    window.refresh();
                                }
                            }
                        }),
                    )),
            )
    });
}

/// A radio row, as the pin dialog's.
fn choice_row(
    ix: usize,
    label: String,
    picked: bool,
    palette: Palette,
    on_click: impl Fn(&mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .id(("schedule-choice", ix))
        .debug_selector(move || format!("schedule-choice-{ix}"))
        .flex()
        .items_center()
        .gap_3()
        .px_2()
        .py_1p5()
        .rounded_lg()
        .cursor_pointer()
        .hover(|d| d.bg(palette.row_hover))
        .on_click(move |_, window, cx| on_click(window, cx))
        .child(
            div()
                .size(px(16.))
                .rounded_full()
                .border_2()
                .border_color(if picked { palette.accent } else { palette.text_secondary })
                .when(picked, |d| d.bg(palette.accent)),
        )
        .child(label)
}

/// A chat's scheduled messages, soonest first, kept up to date.
struct ScheduledList {
    state: Entity<AppState>,
    conversation: ConversationId,
    _observe: gpui_kit::Subscription,
}

/// Lists a chat's scheduled messages.
pub fn open_list(state: Entity<AppState>, conversation: ConversationId, window: &mut Window, cx: &mut App) {
    let list = cx.new(|cx| ScheduledList {
        _observe: cx.observe(&state, |_, _, cx| cx.notify()),
        state,
        conversation,
    });
    window.open_dialog(cx, move |dialog, _, _| dialog.title("Scheduled messages").w(px(460.)).child(list.clone()));
}

impl Render for ScheduledList {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = Palette::get(cx);
        let state = self.state.read(cx);
        let now = Local::now();
        let rows: Vec<_> = state
            .scheduled_in(&self.conversation)
            .map(|message| {
                let text = preview_text(message, state);
                let id = message.id;
                let held = state.scheduled_held(message);
                let cant_send = matches!(held, Some(crate::scheduled::Held::CantSend | crate::scheduled::Held::NotLoaded));
                let reason = held.as_ref().map(|h| h.reason());
                let (send, reschedule, delete) = (self.state.clone(), self.state.clone(), self.state.clone());
                div()
                    .debug_selector(move || format!("scheduled-{id}"))
                    .flex()
                    .flex_col()
                    .gap_1()
                    .py_2()
                    .border_b_1()
                    .border_color(palette.panel_border)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .text_xs()
                            .text_color(palette.text_secondary)
                            .child(Icon::new(IconName::Clock).size_3())
                            .child(crate::scheduled::when_label(message.send_at, now)),
                    )
                    .child(div().text_sm().text_color(palette.text).line_clamp(3).child(text))
                    .when_some(reason, |d, reason| d.child(div().text_xs().text_color(palette.danger).child(reason)))
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .child(
                                Button::new(SharedString::from(format!("scheduled-send-{id}")))
                                    .label("Send now")
                                    .ghost()
                                    .small()
                                    .disabled(cant_send)
                                    .on_click(move |_, window, cx| send_now(send.clone(), id, window, cx)),
                            )
                            .child(Button::new(SharedString::from(format!("scheduled-reschedule-{id}"))).label("Reschedule").ghost().small().on_click(
                                move |_, window, cx| {
                                    let state = reschedule.clone();
                                    pick_time("Reschedule message", "Reschedule", window, cx, move |at, _, cx| {
                                        state.update(cx, |state, cx| state.reschedule(id, at, cx));
                                    });
                                },
                            ))
                            .child(Button::new(SharedString::from(format!("scheduled-delete-{id}"))).label("Delete").ghost().small().on_click(
                                move |_, _, cx| delete.update(cx, |state, cx| state.delete_scheduled(id, cx)),
                            )),
                    )
            })
            .collect();
        div()
            .debug_selector(|| "scheduled-list".into())
            .flex()
            .flex_col()
            .when(rows.is_empty(), |d| d.child(div().py_2().text_sm().text_color(palette.text_secondary).child("No scheduled messages.")))
            .children(rows)
    }
}

/// Sends a scheduled message now; with a safety number changed since it
/// was verified, after asking, as Send does (Desktop's `withPreSendChecks`).
fn send_now(state: Entity<AppState>, id: i64, window: &mut Window, cx: &mut App) {
    let held = {
        let s = state.read(cx);
        s.scheduled.iter().find(|m| m.id == id).and_then(|m| s.scheduled_held(m))
    };
    let people = match held {
        None => return state.update(cx, |state, cx| state.send_scheduled(vec![id], cx)),
        Some(crate::scheduled::Held::CantSend | crate::scheduled::Held::NotLoaded) => return,
        Some(crate::scheduled::Held::SafetyNumber(people)) => people,
    };
    let names: Vec<String> = people
        .iter()
        .map(|sid| state.read(cx).conversation(&ConversationId::direct(sid)).map_or_else(|| "Someone".to_owned(), |c| c.title.clone()))
        .collect();
    window.open_alert_dialog(cx, move |alert, _, _| {
        let (state, people) = (state.clone(), people.clone());
        alert
            .title("Safety Number Changes")
            .description(format!(
                "{} may have reinstalled Signal or changed devices. Send the scheduled message anyway?",
                names.join(", ")
            ))
            .confirm()
            .ok_text("Send anyway")
            // Returning true closes the alert and leaves the list open.
            .on_ok(move |_, _, cx| {
                state.update(cx, |state, cx| {
                    for sid in &people {
                        state.unverified.remove(sid);
                        state.send(signal_backend::Command::SetVerified { conversation: ConversationId::direct(sid), verified: false });
                    }
                    state.send_scheduled(vec![id], cx);
                });
                true
            })
    });
}

/// A chat's scheduled messages, above its composer: how many and when the
/// next goes (or that one is waiting). A click lists them.
pub fn bar(state: &Entity<AppState>, conversation: &ConversationId, palette: Palette, cx: &App) -> Option<gpui_kit::AnyElement> {
    let app = state.read(cx);
    let messages: Vec<_> = app.scheduled_in(conversation).collect();
    let next = messages.first()?.send_at;
    let now = Local::now();
    let waiting = messages.iter().any(|m| m.send_at <= now.timestamp_millis() && app.scheduled_held(m).is_some());
    let count = messages.len();
    let label = if count == 1 { "1 scheduled message".to_owned() } else { format!("{count} scheduled messages") };
    let detail = if waiting { "Waiting to send".to_owned() } else { format!("Next: {}", crate::scheduled::when_label(next, now)) };
    let (state, conversation) = (state.clone(), conversation.clone());
    Some(
        div()
            .id("scheduled-bar")
            .debug_selector(|| "scheduled-bar".into())
            .mx_3()
            .mt_2()
            .px_3()
            .py_1p5()
            .flex()
            .items_center()
            .gap_2()
            .rounded_lg()
            .bg(palette.input_bg)
            .text_sm()
            .cursor_pointer()
            .hover(|d| d.bg(palette.row_hover))
            .on_click(move |_, window, cx| open_list(state.clone(), conversation.clone(), window, cx))
            .child(Icon::new(IconName::Clock).size_4().text_color(palette.text_secondary))
            .child(div().text_color(palette.text).child(label))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(if waiting { palette.danger } else { palette.text_secondary })
                    .child(detail),
            )
            .into_any_element(),
    )
}

/// What a scheduled message says: its text with mentions by name, else
/// what it sends ("2 photos", "report.pdf").
pub(crate) fn preview_text(message: &ScheduledMessage, state: &AppState) -> String {
    let mut mentions = message.extras.mentions.iter();
    let mut text = String::with_capacity(message.body.len());
    for c in message.body.chars() {
        if c == signal_backend::mapping::MENTION_PLACEHOLDER {
            let name = mentions
                .next()
                .and_then(|aci| state.conversation(&ConversationId::direct(aci)))
                .map_or_else(|| "Someone".to_owned(), |c| c.title.clone());
            text.push('@');
            text.push_str(&name);
        } else {
            text.push(c);
        }
    }
    let text = crate::views::common::collapse_whitespace(&text);
    let photos = message.attachments.iter().filter(|a| a.kind == ScheduledKind::Image).count();
    let files: Vec<&str> = message
        .attachments
        .iter()
        .filter(|a| a.kind == ScheduledKind::File)
        .map(|a| a.file_name.as_deref().unwrap_or("File"))
        .collect();
    let mut parts = Vec::new();
    match photos {
        0 => {}
        1 => parts.push("1 photo".to_owned()),
        n => parts.push(format!("{n} photos")),
    }
    parts.extend(files.into_iter().map(str::to_owned));
    match (text.is_empty(), parts.is_empty()) {
        (_, true) => text,
        (true, false) => parts.join(", "),
        (false, false) => format!("{text} ({})", parts.join(", ")),
    }
}
