//! Small shared widgets and formatting.

use chrono::{DateTime, Datelike as _, Local, TimeZone as _};
use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;
use gpui_kit::prelude::*;
use gpui_kit::{AnyElement, Div, ObjectFit, Pixels, SharedString, div, img, px};
use signal_store::{Conversation, ConversationKind, DeliveryStatus};

use crate::theme::{Palette, avatar_colors, initials};

/// What Vernam names someone it knows nothing about, as Desktop names them
/// "Unknown contact" (or "Deleted Account", which Vernam doesn't track).
/// Vernam mostly writes "Unknown" where Desktop writes "Unknown contact",
/// so that counts too, though someone really named "Unknown" then gets the
/// icon where Desktop would show "U".
const UNKNOWN_NAMES: [&str; 3] = ["Unknown", "Unknown contact", "Deleted Account"];

/// A person's round avatar without a photo: their initials, or Desktop's
/// person icon when there are none to show (a name without letters, such
/// as a phone number) or the name only says they're unknown
/// (`shouldUseInitials` in `ts/components/Avatar.dom.tsx`).
pub fn avatar(name: &str, color: u8, size: Pixels) -> Div {
    let label = initials(name).filter(|_| !UNKNOWN_NAMES.contains(&name.trim()));
    let (bg, fg) = avatar_colors(color);
    let circle = div().flex().flex_none().items_center().justify_center().size(size).rounded_full().bg(bg).text_color(fg);
    match label {
        Some(label) => circle.text_size(size * 0.4).font_weight(gpui_kit::FontWeight::SEMIBOLD).child(SharedString::from(label)),
        None => circle.debug_selector(|| "avatar-person-icon".into()).child(Icon::new(IconName::User).size(size * 0.5).text_color(fg)),
    }
}

/// The room left at the window's top for the macOS traffic lights (Desktop's
/// `--title-bar-drag-area-height`): content in the leftmost pane starts
/// below them, and in full screen, where they're gone, at the top.
pub fn title_bar_inset(window: &gpui_kit::Window) -> Pixels {
    if window.is_fullscreen() { px(0.) } else { px(40.) }
}

/// The badge drawn on an avatar of `avatar` pixels (Desktop's
/// `_getBadgeSize` in `Avatar.dom.tsx`); none under 24.
pub fn badge_size(avatar: f32) -> Option<f32> {
    match avatar {
        a if a < 24. => None,
        a if a <= 36. => Some(16.),
        a if a <= 64. => Some(24.),
        a if a <= 112. => Some(36.),
        a => Some((a * 0.4).round()),
    }
}

/// Where the badge sits, `(bottom, right)` (Desktop's
/// `BADGE_PLACEMENT_BY_SIZE`; other sizes sit in the corner).
fn badge_placement(avatar: f32) -> (f32, f32) {
    match avatar as u32 {
        28..=32 => (-4., -2.),
        36 => (-3., 0.),
        40 | 48 => (-6., -4.),
        52 => (-6., -2.),
        56 | 64 => (-6., 0.),
        72 => (-6., -6.),
        80 => (-8., 0.),
        88 | 112 => (-4., 3.),
        _ => (0., 0.),
    }
}

/// `avatar` (of `size` pixels) with someone's badge in its corner, as
/// Desktop's `Avatar` draws `badge`.
pub fn with_badge(avatar: AnyElement, size: f32, badge: Option<std::path::PathBuf>) -> AnyElement {
    let (Some(badge), Some(badge_size)) = (badge, badge_size(size)) else { return avatar };
    let (bottom, right) = badge_placement(size);
    div()
        .relative()
        .flex_none()
        .size(px(size))
        .child(avatar)
        .child(img(badge).absolute().bottom(px(bottom)).right(px(right)).size(px(badge_size)))
        .into_any_element()
}

/// Someone's avatar with their preferred badge (Desktop shows `badges[0]`).
pub fn badged_avatar(state: &crate::state::AppState, service_id: &str, name: &str, size: f32, cx: &gpui_kit::App) -> AnyElement {
    let avatar = person_photo(state, service_id, state.photo_of(service_id), name, crate::theme::color_for(service_id), px(size));
    with_badge(avatar, size, badge_image(state, service_id, size, cx))
}

/// `photo` when there is one; Desktop's gradient when their photo wasn't
/// downloaded because we haven't accepted them (`Avatar.dom.tsx`,
/// `hasAvatar && !hasLocalImage`); otherwise [`avatar`].
pub fn person_photo(
    state: &crate::state::AppState,
    service_id: &str,
    photo: Option<&std::path::Path>,
    name: &str,
    color: u8,
    size: Pixels,
) -> AnyElement {
    match photo {
        None if state.photo_withheld(service_id) => photo_placeholder(signal_store::identifier_hash(service_id), size).into_any_element(),
        photo => person_avatar(photo, name, color, size),
    }
}

/// Signal Desktop's gradients for a photo not downloaded
/// (`AxoTokens.Avatar`'s `Gradients`, `ts/axo/AxoTokens.std.ts`): top and
/// bottom colors.
const PHOTO_PLACEHOLDERS: [(u32, u32); 20] = [
    (0x252568, 0x9c8f8f),
    (0x2a4275, 0x9d9ea1),
    (0x2e4b5f, 0x8aa9b1),
    (0x2e426c, 0x7a9377),
    (0x1a341a, 0x807f6e),
    (0x464e42, 0xd5c38f),
    (0x595643, 0x93a899),
    (0x2c2f36, 0x687466),
    (0x2b1e18, 0x968980),
    (0x7b7067, 0xa5a893),
    (0x706359, 0xbda194),
    (0x383331, 0xa48788),
    (0x924f4f, 0x897a7a),
    (0x663434, 0xc58d77),
    (0x8f4b02, 0xaa9274),
    (0x784747, 0x8c8f6f),
    (0x747474, 0xacacac),
    (0x49484c, 0xa5a6b5),
    (0x4a4e4d, 0xabafae),
    (0x3a3a3a, 0x929887),
];

/// Desktop's stand-in for a photo we haven't downloaded because the chat
/// isn't accepted (`Avatar.dom.tsx`, `hasAvatar && !hasLocalImage`): a
/// gradient picked by their identifier hash (`getAvatarPlaceholderGradient`).
pub fn photo_placeholder(hash: u8, size: Pixels) -> Div {
    let (top, bottom) = PHOTO_PLACEHOLDERS[hash as usize % PHOTO_PLACEHOLDERS.len()];
    div().flex_none().size(size).rounded_full().overflow_hidden().bg(gpui_kit::linear_gradient(
        180.,
        gpui_kit::linear_color_stop(gpui_kit::rgb(top), 0.),
        gpui_kit::linear_color_stop(gpui_kit::rgb(bottom), 1.),
    ))
}

/// Whether a chat's photo is withheld and not here: a person's (by their
/// 1:1 chat's id) or a group's.
pub fn photo_is_withheld(state: &crate::state::AppState, chat: &signal_store::ConversationId) -> bool {
    match chat.as_direct() {
        Some(sid) => state.photo_of(sid).is_none() && state.photo_withheld(sid),
        None => state.conversation(chat).is_some_and(|c| c.avatar_path.is_none() && c.avatar_withheld),
    }
}

/// The placeholder with "View" over it, which downloads the photo (Desktop's
/// `AvatarBlur.BlurPictureWithClickToView` and `startAvatarDownload`), where
/// Desktop offers it: the chat's hero, About, the contact and chat details.
/// `None` when the photo is here or there's none.
pub fn view_photo(state: &gpui_kit::Entity<crate::state::AppState>, chat: &signal_store::ConversationId, size: f32, cx: &gpui_kit::App) -> Option<AnyElement> {
    if !photo_is_withheld(state.read(cx), chat) {
        return None;
    }
    let hash = signal_backend::photo_placeholder_hash(chat);
    let (state, chat) = (state.clone(), chat.clone());
    let white = gpui_kit::rgb(0xffffff);
    Some(
        photo_placeholder(hash, px(size))
            .id(SharedString::from(format!("view-photo-{}", chat.0)))
            .debug_selector(|| "view-photo".into())
            .relative()
            .cursor_pointer()
            .child(
                // Desktop's `module-Avatar__click-to-view`: black at 20%.
                div()
                    .absolute()
                    .inset_0()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .bg(gpui_kit::hsla(0., 0., 0., 0.2))
                    .text_color(white)
                    .text_size(px((size * 0.18).clamp(10., 14.)))
                    .child(gpui_kit::svg().path("icons/signal/click-outline-24.svg").size(px((size * 0.3).min(24.))).text_color(white))
                    .child("View"),
            )
            .on_click(move |_, _, cx| state.read(cx).send(signal_backend::Command::DownloadPhoto { conversation: chat.clone() }))
            .into_any_element(),
    )
}

/// A chat's avatar, with the person's badge for a 1:1 chat (Desktop's
/// chat list and header pass `badge`).
pub fn chat_avatar(state: &crate::state::AppState, conversation: &Conversation, size: f32, cx: &gpui_kit::App) -> AnyElement {
    let avatar = conversation_avatar(conversation, px(size));
    match conversation.id.as_direct().filter(|_| conversation.kind == ConversationKind::Direct && !conversation.id.is_signal()) {
        Some(sid) => with_badge(avatar, size, badge_image(state, sid, size, cx)),
        None => avatar,
    }
}

/// The image of their preferred badge at `size` (for this theme).
pub fn badge_image(state: &crate::state::AppState, service_id: &str, size: f32, cx: &gpui_kit::App) -> Option<std::path::PathBuf> {
    let badge = state.badges.preferred(service_id)?;
    let dark = gpui_kit::component::ActiveTheme::theme(cx).is_dark();
    state.badges.image(badge, badge_size(size)?, dark)
}

/// A round photo, or `placeholder` while it loads and when the file doesn't
/// load. Desktop's `Avatar.dom.tsx` shows the placeholder until the image
/// is there, and sets `imageBroken` to keep it when the image fails: a
/// photo can be damaged as its owner uploaded it, and still decrypt.
/// (GPUI shows the loading one only after 200 ms, so a quick load doesn't
/// flash it, and only for an image with an id, whose state times the
/// wait. Avatars of the same photo side by side share that state, which
/// is the same load's.)
fn avatar_photo(path: std::path::PathBuf, size: Pixels, placeholder: impl Fn() -> AnyElement + 'static) -> AnyElement {
    let placeholder = std::rc::Rc::new(placeholder);
    let loading = placeholder.clone();
    let id = SharedString::from(format!("avatar-{}", path.display()));
    img(path)
        .id(id)
        .debug_selector(|| "avatar-photo".into())
        .flex_none()
        .size(size)
        .rounded_full()
        .object_fit(ObjectFit::Cover)
        .with_loading(move || loading())
        .with_fallback(move || div().debug_selector(|| "avatar-fallback".into()).child(placeholder()).into_any_element())
        .into_any_element()
}

/// A group's avatar from its photo, if any: the group icon without one,
/// while it loads, or when it doesn't (Desktop's `Avatar` with
/// `conversationType="group"`), for a group that isn't a chat here yet.
pub fn group_photo(photo: Option<&std::path::Path>, title: &str, color: u8, size: Pixels) -> AnyElement {
    let title = title.to_owned();
    let placeholder = move || chat_placeholder(ConversationKind::Group, &title, color, size);
    match photo {
        Some(path) => avatar_photo(path.to_path_buf(), size, placeholder),
        None => placeholder(),
    }
}

/// A person's avatar: their profile photo once downloaded, else (and
/// while it loads, or when it doesn't) [`avatar`]: initials or the person
/// icon.
pub fn person_avatar(photo: Option<&std::path::Path>, name: &str, color: u8, size: Pixels) -> AnyElement {
    match photo {
        Some(path) => {
            let name = name.to_owned();
            avatar_photo(path.to_path_buf(), size, move || avatar(&name, color, size).into_any_element())
        }
        None => avatar(name, color, size).into_any_element(),
    }
}

/// Desktop's mark after the name of an official chat, the Signal chat or
/// Note to Self (`OfficialChatInlineBadge`).
pub fn official_badge(palette: crate::theme::Palette) -> AnyElement {
    Icon::new(IconName::BadgeCheck).size_4().flex_none().text_color(palette.accent).into_any_element()
}

/// Whether a chat gets [`official_badge`].
pub fn is_official(conversation: &Conversation) -> bool {
    conversation.kind == ConversationKind::NoteToSelf || conversation.id.is_signal()
}

/// The Signal chat's avatar, as Desktop draws it (`module-Avatar--signal-
/// official`): Signal's logo in white at 62% on its blue.
pub fn signal_avatar(size: Pixels) -> AnyElement {
    div()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(size)
        .rounded_full()
        .bg(gpui_kit::rgb(0x3640fd))
        .child(
            gpui_kit::svg()
                .path("icons/signal/profile-avatar.svg")
                .size(size * 0.62)
                .text_color(gpui_kit::rgb(0xffffff)),
        )
        .into_any_element()
}

/// A conversation's avatar: its downloaded photo, else (like Signal Desktop)
/// a people icon for groups, a note icon for Note to Self, or [`avatar`],
/// which also stand in for a photo while it loads or when it doesn't.
pub fn conversation_avatar(conversation: &Conversation, size: Pixels) -> AnyElement {
    if conversation.id.is_signal() {
        return signal_avatar(size);
    }
    if let Some(path) = conversation.avatar_path.as_ref() {
        let (kind, title, color) = (conversation.kind, conversation.title.clone(), conversation.color);
        return avatar_photo(path.into(), size, move || chat_placeholder(kind, &title, color, size));
    }
    // One not downloaded because the chat isn't accepted.
    if conversation.avatar_withheld {
        let hash = signal_backend::photo_placeholder_hash(&conversation.id);
        return photo_placeholder(hash, size).into_any_element();
    }
    chat_placeholder(conversation.kind, &conversation.title, conversation.color, size)
}

/// A chat's avatar without a photo: the group or Note to Self icon, or the
/// title's [`avatar`].
pub fn chat_placeholder(kind: ConversationKind, title: &str, color: u8, size: Pixels) -> AnyElement {
    let icon = match kind {
        ConversationKind::Group => Some(IconName::Users),
        ConversationKind::NoteToSelf => Some(IconName::NotebookText),
        ConversationKind::Direct => None,
    };
    match icon {
        Some(icon) => {
            let (bg, fg) = avatar_colors(color);
            div()
                .flex()
                .flex_none()
                .items_center()
                .justify_center()
                .size(size)
                .rounded_full()
                .bg(bg)
                .child(Icon::new(icon).size(size * 0.5).text_color(fg))
                .into_any_element()
        }
        None => avatar(title, color, size).into_any_element(),
    }
}

/// Delivery-status icon for one of our messages. `read` is the color for
/// read receipts, `muted` for everything else.
/// Where a delivery status shows, which decides how failures look.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum StatusPlace {
    /// Message metadata: a failed send says "Not sent" instead of an icon.
    Bubble,
    /// Chat list: a failed send shows a red error circle.
    List,
}

/// Signal Desktop's delivery status icons: a dotted circle that turns while
/// sending, one outlined circle with a check once sent, two once delivered,
/// two filled ones once read. All in the surrounding text colour (read
/// isn't highlighted), 12pt tall; the double ones are 18pt wide.
pub fn status_icon(
    status: DeliveryStatus,
    color: gpui_kit::Hsla,
    palette: Palette,
    place: StatusPlace,
    key: impl Into<SharedString>,
) -> Option<AnyElement> {
    use gpui_kit::{Animation, AnimationExt as _, Transformation, percentage, svg};
    let icon = |name: &str, width: f32| {
        svg().path(SharedString::from(format!("icons/signal/{name}.svg"))).w(px(width)).h(px(12.)).flex_none()
    };
    Some(match status {
        // Held for a captcha: the sending circle, still.
        DeliveryStatus::Paused => icon("messagestatus-sending", 12.).text_color(color).into_any_element(),
        DeliveryStatus::Sending => icon("messagestatus-sending", 12.)
            .text_color(color)
            .with_animation(
                gpui_kit::ElementId::Name(format!("sending-{}", key.into()).into()),
                Animation::new(std::time::Duration::from_secs(4)).repeat(),
                |icon, delta| icon.with_transformation(Transformation::rotate(percentage(delta))),
            )
            .into_any_element(),
        DeliveryStatus::Sent => icon("messagestatus-sent", 12.).text_color(color).into_any_element(),
        DeliveryStatus::Delivered => icon("messagestatus-delivered", 18.).text_color(color).into_any_element(),
        // Desktop draws viewed as read (`status-icon--viewed`).
        DeliveryStatus::Read | DeliveryStatus::Viewed => icon("messagestatus-read", 18.).text_color(color).into_any_element(),
        DeliveryStatus::Failed | DeliveryStatus::PartialSent => match place {
            StatusPlace::Bubble => return None,
            StatusPlace::List => icon("error-circle", 12.).text_color(palette.danger).into_any_element(),
        },
    })
}

fn local(millis: i64) -> Option<DateTime<Local>> {
    Local.timestamp_millis_opt(millis).single()
}

/// Conversation list timestamp: "now", "5m", "3:41 PM", "Mon", "Mar 3", "3/3/24".
pub fn short_timestamp(millis: i64) -> String {
    let Some(time) = local(millis) else { return String::new() };
    let now = Local::now();
    let age = now.signed_duration_since(time);
    if age.num_seconds() < 60 {
        "now".into()
    } else if age.num_minutes() < 60 {
        format!("{}m", age.num_minutes())
    } else if time.date_naive() == now.date_naive() {
        time.format("%-I:%M %p").to_string()
    } else if (now.date_naive() - time.date_naive()).num_days() < 7 {
        time.format("%a").to_string()
    } else if time.year() == now.year() {
        time.format("%b %-d").to_string()
    } else {
        time.format("%-m/%-d/%y").to_string()
    }
}

/// Message footer time.
pub fn message_time(millis: i64) -> String {
    let Some(time) = local(millis) else { return String::new() };
    let age = Local::now().signed_duration_since(time);
    if age.num_seconds() < 60 {
        "Now".into()
    } else if age.num_minutes() < 60 {
        format!("{}m", age.num_minutes())
    } else {
        time.format("%-I:%M %p").to_string()
    }
}

/// Day separator: "Today", "Yesterday", "Monday", "March 3", "March 3, 2024".
pub fn day_label(millis: i64) -> String {
    let Some(time) = local(millis) else { return String::new() };
    let today = Local::now().date_naive();
    let day = time.date_naive();
    match (today - day).num_days() {
        0 => "Today".into(),
        1 => "Yesterday".into(),
        2..=6 => time.format("%A").to_string(),
        _ if day.year() == today.year() => time.format("%B %-d").to_string(),
        _ => time.format("%B %-d, %Y").to_string(),
    }
}

pub fn same_day(a: i64, b: i64) -> bool {
    match (local(a), local(b)) {
        (Some(a), Some(b)) => a.date_naive() == b.date_naive(),
        _ => false,
    }
}

/// Joins lines and runs of spaces with single spaces, for one-paragraph
/// previews (draft rows, quotes).
pub fn collapse_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Web links in `text`: each byte range and the URL to open. Bare domains
/// (`signal.org`, `www.example.com/x`) open over https; other schemes are
/// left as plain text so a message can't make a `file:` or `javascript:`
/// link clickable.
pub fn find_links(text: &str) -> Vec<(std::ops::Range<usize>, String)> {
    let mut finder = linkify::LinkFinder::new();
    finder.kinds(&[linkify::LinkKind::Url]).url_must_have_scheme(false);
    finder
        .links(text)
        .filter_map(|link| {
            let found = link.as_str();
            let url = match found.split_once("://") {
                Some((scheme, _)) if scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https") => {
                    found.to_owned()
                }
                Some(_) => return None,
                None if found.contains(':') && !found.contains('.') => return None,
                None => format!("https://{found}"),
            };
            Some((link.start()..link.end(), url))
        })
        .collect()
}

pub fn format_size(bytes: u64) -> String {
    match bytes {
        b if b < 1024 => format!("{b} B"),
        b if b < 1024 * 1024 => format!("{:.0} KB", b as f64 / 1024.),
        b if b < 1024 * 1024 * 1024 => format!("{:.1} MB", b as f64 / (1024. * 1024.)),
        b => format!("{:.2} GB", b as f64 / (1024. * 1024. * 1024.)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_web_links_only() {
        let text = "See https://signal.org/blog/. Or www.example.com/a?b=1, or signal.org! Not ftp://x.org or javascript:alert(1)";
        let links = find_links(text);
        let found: Vec<(&str, &str)> = links.iter().map(|(r, url)| (&text[r.clone()], url.as_str())).collect();
        assert_eq!(
            found,
            [
                ("https://signal.org/blog/", "https://signal.org/blog/"),
                ("www.example.com/a?b=1", "https://www.example.com/a?b=1"),
                ("signal.org", "https://signal.org"),
            ]
        );
        assert!(find_links("no links here").is_empty());
    }

    /// Draws a photo as a person's avatar, or as a group's.
    struct Avatar {
        photo: std::path::PathBuf,
        group: bool,
        /// The timeline's cache, which draws senders' avatars, rather than
        /// GPUI's global one.
        cache: Option<gpui_kit::Entity<crate::image_cache::LruImageCache>>,
    }

    impl Render for Avatar {
        fn render(&mut self, _: &mut gpui_kit::Window, _: &mut gpui_kit::Context<Self>) -> impl IntoElement {
            let root = match &self.cache {
                Some(cache) => div().image_cache(cache.clone()),
                None => div(),
            };
            if !self.group {
                return root.child(person_avatar(Some(&self.photo), "Sam Rivera", 3, px(28.)));
            }
            let person = crate::views::message_request::ReviewPerson {
                aci: "a".into(),
                name: "Hikers".into(),
                phone: None,
                shared_groups: Vec::new(),
                chat: None,
            };
            let mut group = person.chat_or_new();
            group.kind = ConversationKind::Group;
            group.avatar_path = self.photo.to_str().map(str::to_owned);
            root.child(conversation_avatar(&group, px(28.)))
        }
    }

    /// Writes a JPEG's first bytes, then noise: a photo uploaded damaged,
    /// which still decrypts.
    fn damaged_photo(dir: &std::path::Path) -> std::path::PathBuf {
        let photo = dir.join("d-sam.jpg");
        let mut bytes = b"\xff\xd8\xff\xe0\x00\x10JFIF\x00\x01\x01\x00\x00\x01\x00\x01\x00\x00".to_vec();
        bytes.extend((0..4096u32).map(|i| (i.wrapping_mul(2654435761) >> 13) as u8));
        std::fs::write(&photo, bytes).unwrap();
        photo
    }

    /// The placeholder's bounds once `photo` has been loaded (or not).
    fn placeholder(cx: &mut gpui_kit::TestAppContext, photo: std::path::PathBuf, group: bool) -> Option<gpui_kit::Bounds<Pixels>> {
        let source = gpui_kit::Resource::Path(photo.clone().into());
        let (_, vcx) = cx.add_window_view(|_, _| Avatar { photo, group, cache: None });
        vcx.run_until_parked();
        // The load ends after the first frame; the next draws the outcome.
        vcx.update(|window, _| window.refresh());
        vcx.run_until_parked();
        // The photo's element is there either way, holding the placeholder
        // when it doesn't load.
        assert!(vcx.debug_bounds("avatar-photo").is_some());
        // And its load has ended, so a placeholder missing isn't one yet
        // to come.
        let loaded = vcx.update(|window, cx| window.get_asset::<gpui_kit::ImgResourceLoader>(&source, cx));
        assert!(loaded.is_some(), "still loading");
        vcx.debug_bounds("avatar-fallback")
    }

    /// A photo that doesn't decode (one uploaded damaged still decrypts)
    /// shows the placeholder, as Desktop's `imageBroken` does, not nothing.
    #[gpui_kit::test]
    fn a_photo_that_wont_load_shows_the_placeholder(cx: &mut gpui_kit::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let photo = damaged_photo(dir.path());
        for group in [false, true] {
            let bounds = placeholder(cx, photo.clone(), group).unwrap_or_else(|| panic!("nothing drawn (group: {group})"));
            assert_eq!(bounds.size.height, px(28.), "group: {group}");
        }
    }

    /// The same through the timeline's own image cache.
    #[gpui_kit::test]
    fn a_sender_photo_that_wont_load_shows_the_placeholder(cx: &mut gpui_kit::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let photo = damaged_photo(dir.path());
        let (_, vcx) = cx.add_window_view(|_, cx| Avatar { photo, group: false, cache: Some(crate::image_cache::LruImageCache::new(cx)) });
        vcx.run_until_parked();
        vcx.update(|window, _| window.refresh());
        vcx.run_until_parked();
        let bounds = vcx.debug_bounds("avatar-fallback").expect("nothing drawn");
        assert_eq!(bounds.size.height, px(28.));
    }

    /// Draws a person's avatar without a photo.
    struct Nameplate(&'static str);

    impl Render for Nameplate {
        fn render(&mut self, _: &mut gpui_kit::Window, _: &mut gpui_kit::Context<Self>) -> impl IntoElement {
            div().child(person_avatar(None, self.0, 3, px(28.)))
        }
    }

    /// Desktop's person icon stands in for initials when the name has no
    /// letters or only says they're unknown.
    #[gpui_kit::test]
    fn names_without_initials_get_the_person_icon(cx: &mut gpui_kit::TestAppContext) {
        for (name, icon) in [("+1 555 0100", true), ("Unknown", true), ("Unknown contact", true), ("Ada Lovelace", false)] {
            let (_, vcx) = cx.add_window_view(|_, _| Nameplate(name));
            vcx.run_until_parked();
            assert_eq!(vcx.debug_bounds("avatar-person-icon").is_some(), icon, "{name}");
        }
    }

    /// A photo that loads shows itself.
    #[gpui_kit::test]
    fn a_photo_that_loads_shows_no_placeholder(cx: &mut gpui_kit::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let photo = dir.path().join("d-sam.png");
        image::RgbaImage::from_pixel(8, 8, image::Rgba([200, 80, 40, 255])).save(&photo).unwrap();
        assert!(placeholder(cx, photo, false).is_none());
    }
}


/// Where a mouse-down outside a popover closed it. Popovers close on mouse
/// down outside, as menus do, but the button that toggles one acts on the
/// click that follows, so clicking it while open would close the popover and
/// then reopen it. The button skips the click that began where this recorded.
#[derive(Default)]
pub(crate) struct ClosedByClick(Option<gpui_kit::Point<Pixels>>);

impl ClosedByClick {
    pub(crate) fn record(&mut self, event: &gpui_kit::MouseDownEvent) {
        self.0 = Some(event.position);
    }

    /// Whether `click` is the one that closed the popover. Forgets it either
    /// way.
    pub(crate) fn take(&mut self, click: &gpui_kit::ClickEvent) -> bool {
        let at = self.0.take();
        matches!(click, gpui_kit::ClickEvent::Mouse(m) if at == Some(m.down.position))
    }
}
