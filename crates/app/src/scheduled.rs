//! Scheduled messages. Signal Desktop has none; Signal Android schedules a
//! message from a long press on Send and sends it from the phone when it's
//! due. Here a right click on Send does it: the composer keeps what it
//! would have sent in the account's database (`signal_store::scheduled`),
//! and the account's `AppState` sends it when it's due, as the commands
//! Send would have sent then. They go only while the app runs: one that
//! came due while it was closed, or offline, goes once the account is next
//! online.

use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Datelike as _, Local, NaiveTime, TimeZone as _, Timelike as _};
use signal_backend::{Command, LinkPreviewDraft, OutgoingFile, OutgoingImage, PreviewImage};
use signal_store::{
    ConversationId, ScheduledFile, ScheduledKind, ScheduledMessage, ScheduledPreview, ScheduledPreviewImage, StyleSpan,
};

/// The longest the timer sleeps before looking at the clock again. A
/// sleeping Mac stops the timer's clock, so a long sleep would wake late.
pub const MAX_WAIT: Duration = Duration::from_secs(30);

/// What the composer sends: the text with the photos (or alone), else with
/// the first file, and each file as a message of its own, as in Desktop.
#[allow(clippy::too_many_arguments)]
pub fn commands(
    conversation: &ConversationId,
    body: String,
    quote: Option<i64>,
    preview: Option<LinkPreviewDraft>,
    mentions: Vec<String>,
    styles: Vec<StyleSpan>,
    images: Vec<OutgoingImage>,
    files: Vec<OutgoingFile>,
) -> Vec<Command> {
    let mut commands = Vec::new();
    let mut caption = Some((body, quote, mentions, styles));
    if !images.is_empty() || files.is_empty() {
        let (body, quote, mentions, styles) = caption.take().unwrap_or_default();
        commands.push(if images.is_empty() {
            Command::SendText { conversation: conversation.clone(), body, quote, preview, mentions, styles }
        } else {
            Command::SendImages { conversation: conversation.clone(), images, caption: body, quote, mentions, styles }
        });
    }
    for file in files {
        let (caption, quote, mentions, styles) = caption.take().unwrap_or_default();
        commands.push(Command::SendFile { conversation: conversation.clone(), file, caption, quote, mentions, styles });
    }
    commands
}

/// The commands a scheduled message sends, taken from the database.
pub fn commands_for(message: ScheduledMessage, files: Vec<ScheduledFile>) -> Vec<Command> {
    let mut images = Vec::new();
    let mut outgoing = Vec::new();
    let mut preview_image = None;
    for file in files {
        let bytes = Arc::new(file.bytes);
        match file.kind {
            ScheduledKind::Image => images.push(OutgoingImage { bytes, file_name: file.file_name }),
            ScheduledKind::File => outgoing.push(OutgoingFile {
                bytes,
                file_name: file.file_name.unwrap_or_else(|| "file".into()),
                voice: false,
            }),
            ScheduledKind::PreviewImage => preview_image = Some(bytes),
        }
    }
    let extras = message.extras;
    let preview = extras.preview.map(|p| preview_from_store(p, preview_image));
    commands(&message.conversation, message.body, extras.quote, preview, extras.mentions, extras.styles, images, outgoing)
}

/// A link preview as kept, and its image as an attachment.
pub fn preview_to_store(preview: &LinkPreviewDraft) -> (ScheduledPreview, Option<ScheduledFile>) {
    let image = preview.image.as_ref();
    (
        ScheduledPreview {
            url: preview.url.clone(),
            title: preview.title.clone(),
            description: preview.description.clone(),
            date: preview.date,
            image: image.map(|i| ScheduledPreviewImage {
                content_type: i.content_type.to_owned(),
                width: i.width,
                height: i.height,
            }),
        },
        image.map(|i| ScheduledFile { kind: ScheduledKind::PreviewImage, file_name: None, bytes: i.bytes.to_vec() }),
    )
}

/// The content types a preview image is made as (`media::ImageKind`).
const PREVIEW_IMAGE_TYPES: [&str; 4] = ["image/png", "image/jpeg", "image/gif", "image/webp"];

/// A kept link preview as sent; an image whose kept type isn't one a
/// preview is made as is left off.
fn preview_from_store(preview: ScheduledPreview, bytes: Option<Arc<Vec<u8>>>) -> LinkPreviewDraft {
    let image = preview.image.zip(bytes).and_then(|(image, bytes)| {
        let content_type = PREVIEW_IMAGE_TYPES.into_iter().find(|t| *t == image.content_type)?;
        Some(PreviewImage { bytes, content_type, width: image.width, height: image.height })
    });
    LinkPreviewDraft {
        url: preview.url,
        title: preview.title,
        description: preview.description,
        date: preview.date,
        image,
    }
}

/// A local date and time: the earlier of a repeated hour, and for one the
/// clocks skip (spring forward), the same time an hour on.
pub fn local(naive: chrono::NaiveDateTime) -> Option<DateTime<Local>> {
    Local
        .from_local_datetime(&naive)
        .earliest()
        .or_else(|| Local.from_local_datetime(&(naive + chrono::Duration::hours(1))).earliest())
}

/// `hour` o'clock on `day`'s date in local time.
fn at(day: DateTime<Local>, hour: u32) -> Option<DateTime<Local>> {
    local(day.date_naive().and_time(NaiveTime::from_hms_opt(hour, 0, 0)?))
}

/// The quick choices in the schedule dialog: in an hour, tonight at 8 (until
/// 7 PM), and tomorrow at 8 AM.
pub fn presets(now: DateTime<Local>) -> Vec<(String, DateTime<Local>)> {
    let mut presets = Vec::new();
    let in_an_hour = now + chrono::Duration::hours(1);
    let in_an_hour = in_an_hour.with_second(0).and_then(|t| t.with_nanosecond(0)).unwrap_or(in_an_hour);
    presets.push(("In 1 hour".to_owned(), in_an_hour));
    if let Some(tonight) = at(now, 20).filter(|t| *t - now >= chrono::Duration::hours(1)) {
        presets.push((format!("Tonight at {}", time_of_day(tonight)), tonight));
    }
    if let Some(tomorrow) = at(now + chrono::Duration::days(1), 8) {
        presets.push((format!("Tomorrow at {}", time_of_day(tomorrow)), tomorrow));
    }
    presets
}

fn time_of_day(time: DateTime<Local>) -> String {
    time.format("%-I:%M %p").to_string()
}

/// When a scheduled message goes: "Today at 8:00 PM", "Tomorrow at
/// 8:00 AM", "Mon, Nov 23 at 9:00 AM", and the year when it isn't this one.
pub fn when_label(send_at: i64, now: DateTime<Local>) -> String {
    let Some(time) = Local.timestamp_millis_opt(send_at).single() else { return String::new() };
    let day = match (time.date_naive() - now.date_naive()).num_days() {
        0 => "Today".to_owned(),
        1 => "Tomorrow".to_owned(),
        _ if time.year() == now.year() => time.format("%a, %b %-d").to_string(),
        _ => time.format("%a, %b %-d, %Y").to_string(),
    };
    format!("{day} at {}", time_of_day(time))
}

/// Why a scheduled message that's due isn't sent: what would stop Send in
/// the chat now. It waits, and goes once nothing does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Held {
    /// Blocked, a group we left or that ended, one we're invited to or
    /// asked to join, an announcement-only group we don't administer, or an
    /// unanswered message request: the composer isn't there
    /// (`group_admin::composer_replacement` and the conversation view).
    CantSend,
    /// The chat isn't loaded yet, so what would stop Send isn't known.
    NotLoaded,
    /// These people's safety numbers changed since we verified them
    /// (Desktop's `withPreSendChecks`): Send would ask first.
    SafetyNumber(Vec<String>),
}

impl Held {
    pub fn reason(&self) -> &'static str {
        match self {
            Self::CantSend => "Waiting: you can't send messages in this chat now.",
            Self::NotLoaded => "Waiting for the chat to load.",
            Self::SafetyNumber(_) => "Waiting: a safety number changed. Send now to confirm.",
        }
    }
}

/// Whether a scheduled message for `chat` must wait. `me`: our ACI.
pub fn held(chat: Option<&signal_store::Conversation>, unverified: &std::collections::HashSet<String>, me: &str) -> Option<Held> {
    use signal_store::Membership;
    let Some(chat) = chat else { return Some(Held::NotLoaded) };
    let group = &chat.group_state;
    let member = matches!(group.membership, Membership::Member);
    let announcements = group.announcements_only && !chat.admins.iter().any(|a| a == me);
    let group_bars = chat.kind == signal_store::ConversationKind::Group && (!member || announcements);
    if chat.blocked || chat.left || chat.ended || !chat.accepted || group_bars {
        return Some(Held::CantSend);
    }
    let people = signal_backend::stopping_send(chat, unverified);
    (!people.is_empty()).then_some(Held::SafetyNumber(people))
}

/// How long the timer waits before looking again for messages due, with
/// the soonest due at `next` (none: it stops).
pub fn wait(next: Option<i64>, now: i64) -> Option<Duration> {
    let next = next?;
    let left = Duration::from_millis(next.saturating_sub(now).max(0) as u64);
    Some(left.min(MAX_WAIT))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn local(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> DateTime<Local> {
        Local.with_ymd_and_hms(y, mo, d, h, mi, 0).earliest().unwrap()
    }

    fn image(name: &str) -> OutgoingImage {
        OutgoingImage { bytes: Arc::new(vec![1]), file_name: Some(name.into()) }
    }

    fn file(name: &str) -> OutgoingFile {
        OutgoingFile { bytes: Arc::new(vec![2]), file_name: name.into(), voice: false }
    }

    #[test]
    fn text_goes_with_photos_else_with_the_first_file() {
        let chat = ConversationId("d:alice".into());
        let text = |c: Vec<Command>| -> Vec<String> {
            c.into_iter()
                .map(|c| match c {
                    Command::SendText { body, quote, .. } => format!("text {body} {quote:?}"),
                    Command::SendImages { caption, images, quote, .. } => format!("images{} {caption} {quote:?}", images.len()),
                    Command::SendFile { caption, file, quote, .. } => format!("file {} {caption} {quote:?}", file.file_name),
                    other => panic!("{other:?}"),
                })
                .collect()
        };
        let send = |images, files| commands(&chat, "hi".into(), Some(3), None, Vec::new(), Vec::new(), images, files);
        assert_eq!(text(send(Vec::new(), Vec::new())), ["text hi Some(3)"]);
        assert_eq!(text(send(vec![image("a"), image("b")], vec![file("c")])), ["images2 hi Some(3)", "file c  None"]);
        assert_eq!(text(send(Vec::new(), vec![file("c"), file("d")])), ["file c hi Some(3)", "file d  None"]);
    }

    #[test]
    fn a_scheduled_message_sends_what_was_kept() {
        let draft = LinkPreviewDraft {
            url: "https://example.com".into(),
            title: "Example".into(),
            description: Some("A page".into()),
            date: Some(5),
            image: Some(PreviewImage { bytes: Arc::new(vec![9, 9]), content_type: "image/webp", width: 4, height: 3 }),
        };
        let (preview, preview_file) = preview_to_store(&draft);
        let message = ScheduledMessage {
            id: 1,
            conversation: ConversationId("d:alice".into()),
            send_at: 0,
            created_at: 0,
            body: "look".into(),
            extras: signal_store::ScheduledExtras {
                mentions: vec!["aci".into()],
                styles: Vec::new(),
                quote: Some(4),
                preview: Some(preview),
            },
            attachments: Vec::new(),
        };
        let commands = commands_for(message, preview_file.into_iter().collect());
        let [Command::SendText { body, quote, preview, mentions, .. }] = commands.as_slice() else { panic!("{commands:?}") };
        assert_eq!((body.as_str(), *quote, mentions.as_slice()), ("look", Some(4), ["aci".to_owned()].as_slice()));
        assert_eq!(preview.as_ref(), Some(&draft));
    }

    #[test]
    fn a_preview_image_of_an_unknown_type_is_left_off() {
        let preview = ScheduledPreview {
            url: "u".into(),
            title: "t".into(),
            description: None,
            date: None,
            image: Some(ScheduledPreviewImage { content_type: "image/x-icon".into(), width: 1, height: 1 }),
        };
        assert_eq!(preview_from_store(preview, Some(Arc::new(vec![1]))).image, None);
    }

    #[test]
    fn photos_and_files_keep_their_order_and_names() {
        let message = ScheduledMessage {
            id: 1,
            conversation: ConversationId("d:alice".into()),
            send_at: 0,
            created_at: 0,
            body: "caption".into(),
            extras: Default::default(),
            attachments: Vec::new(),
        };
        let files = vec![
            ScheduledFile { kind: ScheduledKind::Image, file_name: Some("a.png".into()), bytes: vec![1] },
            ScheduledFile { kind: ScheduledKind::File, file_name: Some("b.pdf".into()), bytes: vec![2] },
            ScheduledFile { kind: ScheduledKind::Image, file_name: None, bytes: vec![3] },
        ];
        let commands = commands_for(message, files);
        let [Command::SendImages { images, caption, .. }, Command::SendFile { file, caption: none, .. }] = commands.as_slice()
        else {
            panic!("{commands:?}")
        };
        assert_eq!(images.iter().map(|i| i.bytes[0]).collect::<Vec<_>>(), [1, 3]);
        assert_eq!((caption.as_str(), file.file_name.as_str(), none.as_str()), ("caption", "b.pdf", ""));
    }

    #[test]
    fn presets_skip_tonight_once_its_close() {
        let labels = |now| presets(now).into_iter().map(|(l, _)| l).collect::<Vec<_>>();
        assert_eq!(labels(local(2026, 11, 17, 9, 30)), ["In 1 hour", "Tonight at 8:00 PM", "Tomorrow at 8:00 AM"]);
        assert_eq!(labels(local(2026, 11, 17, 19, 30)), ["In 1 hour", "Tomorrow at 8:00 AM"]);
        let (_, hour) = &presets(local(2026, 11, 17, 9, 30))[0];
        assert_eq!(*hour, local(2026, 11, 17, 10, 30));
        let (_, tomorrow) = &presets(local(2026, 12, 31, 22, 0))[1];
        assert_eq!(*tomorrow, local(2027, 1, 1, 8, 0));
    }

    #[test]
    fn labels_name_the_day() {
        let now = local(2026, 11, 17, 9, 0);
        let label = |t: DateTime<Local>| when_label(t.timestamp_millis(), now);
        assert_eq!(label(local(2026, 11, 17, 20, 0)), "Today at 8:00 PM");
        assert_eq!(label(local(2026, 11, 18, 8, 5)), "Tomorrow at 8:05 AM");
        assert_eq!(label(local(2026, 11, 23, 9, 0)), "Mon, Nov 23 at 9:00 AM");
        assert_eq!(label(local(2027, 1, 4, 9, 0)), "Mon, Jan 4, 2027 at 9:00 AM");
    }

    #[test]
    fn a_message_waits_where_send_would_be_stopped() {
        use signal_store::{ConversationKind, Membership};
        let mut chat = crate::views::message_request::ReviewPerson {
            aci: "bob".into(),
            name: "Bob".into(),
            phone: None,
            shared_groups: Vec::new(),
            chat: None,
        }
        .chat_or_new();
        chat.accepted = true;
        let none = std::collections::HashSet::new();
        assert_eq!(held(Some(&chat), &none, "me"), None);
        assert_eq!(held(None, &none, "me"), Some(Held::NotLoaded));
        let unverified: std::collections::HashSet<String> = ["bob".to_owned()].into();
        assert_eq!(held(Some(&chat), &unverified, "me"), Some(Held::SafetyNumber(vec!["bob".into()])));
        let mut blocked = chat.clone();
        blocked.blocked = true;
        assert_eq!(held(Some(&blocked), &none, "me"), Some(Held::CantSend));

        let mut group = chat.clone();
        group.kind = ConversationKind::Group;
        group.group_state.announcements_only = true;
        assert_eq!(held(Some(&group), &none, "me"), Some(Held::CantSend), "announcements only");
        group.admins = vec!["me".into()];
        assert_eq!(held(Some(&group), &none, "me"), None, "but we're an admin");
        group.group_state.membership = Membership::Requesting { password: String::new() };
        assert_eq!(held(Some(&group), &none, "me"), Some(Held::CantSend), "asked to join");
    }

    #[test]
    fn the_timer_wakes_at_the_next_message_or_every_half_minute() {
        assert_eq!(wait(None, 0), None);
        assert_eq!(wait(Some(5_000), 1_000), Some(Duration::from_secs(4)));
        assert_eq!(wait(Some(10_000_000), 0), Some(MAX_WAIT));
        assert_eq!(wait(Some(0), 1_000), Some(Duration::ZERO));
    }
}
