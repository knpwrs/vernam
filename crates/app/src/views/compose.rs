//! Desktop's "New chat" (`ts/components/leftPane/LeftPaneComposeHelper.dom.tsx`):
//! the left pane's composer, opened by the pencil button or ⌘N. A search
//! for a name, username or number above New group, then everyone and
//! every group you could message; typing narrows them, and offers to look
//! up a username or number on Signal.
//!
//! Not here yet: "Find by username" and "Find by phone number" (their own
//! panes in Desktop), New group as a left-pane flow (it opens the New
//! group dialog), and the contacts' "Manage Contact" menu, whose Remove
//! the backend doesn't do.

use std::rc::Rc;

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::scroll::{Scrollbar, ScrollbarMode};
use gpui_kit::component::{Icon, Sizable as _, WindowExt as _};
use gpui_kit::prelude::*;
use gpui_kit::{App, Context, Entity, FontWeight, ListAlignment, ListState, SharedString, Subscription, Task, Window, div, list, px};
use signal_backend::recipients::{FindError, ParsedPhone};
use signal_store::{ComposeContact, Conversation, ConversationId, ConversationKind, DirectoryEntry};

use crate::state::{AppEvent, AppState};
use crate::theme::Palette;

/// Row heights (Desktop's `ConversationList` `calculateRowHeight`).
const HEADER_HEIGHT: f32 = 40.;
const SELECT_HEIGHT: f32 = 52.;
const RESULT_HEIGHT: f32 = 76.;
const WEEK_MS: f64 = 7. * 24. * 60. * 60. * 1000.;

/// One row of the list, in Desktop's order (`LeftPaneComposeHelper.getRow`).
#[derive(Debug, Clone, PartialEq)]
enum Row {
    NewGroup,
    Header(&'static str),
    /// An index into the loaded contacts.
    Contact(usize),
    Group(ConversationId),
    /// "Find by username": the username typed.
    Username(String),
    /// "Find by phone number": the number as read, and as typed.
    Phone(ParsedPhone, String),
}

/// Asks the sidebar to close the composer.
pub enum ComposeEvent {
    /// Back to the inbox (back button, Escape).
    Close,
    /// A chat was picked; `archived` shows the archive (Desktop's
    /// `switchToAssociatedView`).
    Opened { archived: bool },
}

impl gpui_kit::EventEmitter<ComposeEvent> for Compose {}

/// A contact with the text Desktop searches, read once.
struct Candidate {
    contact: ComposeContact,
    /// Each name field, as `searchable` makes it.
    names: Vec<String>,
}

pub struct Compose {
    state: Entity<AppState>,
    search: Entity<InputState>,
    /// The search text as typed (Desktop hides the top rows for any text,
    /// spaces too, but filters by the trimmed text).
    query: String,
    candidates: Rc<Vec<Candidate>>,
    rows: Rc<Vec<Row>>,
    list: ListState,
    /// The username or number being looked up (as sent), and the text to
    /// name it by.
    finding: Option<(String, Finding)>,
    load: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

/// What's being looked up, for its "not found" text.
#[derive(Debug, Clone, PartialEq)]
enum Finding {
    Username(String),
    Phone(String),
}

impl Compose {
    pub fn new(state: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Name, username, or number"));
        let subscriptions = vec![
            cx.subscribe_in(&search, window, |this, input, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change) {
                    this.query = input.read(cx).value().to_string();
                    this.rebuild(cx);
                }
            }),
            cx.subscribe_in(&state, window, |this, _, event: &AppEvent, window, cx| match event {
                AppEvent::ConversationsChanged => this.load(cx),
                AppEvent::RecipientFound { query, result } => this.found(query, result, window, cx),
                _ => {}
            }),
        ];
        search.update(cx, |search, cx| search.focus(window, cx));
        let mut this = Self {
            state,
            search,
            query: String::new(),
            candidates: Rc::default(),
            rows: Rc::default(),
            list: ListState::new(0, ListAlignment::Top, px(SELECT_HEIGHT * 8.)),
            finding: None,
            load: None,
            _subscriptions: subscriptions,
        };
        this.load(cx);
        this.rebuild(cx);
        this
    }

    /// Its search field.
    pub fn search_input(&self) -> Entity<InputState> {
        self.search.clone()
    }

    /// Reads who to list again.
    fn load(&mut self, cx: &mut Context<Self>) {
        let state = self.state.read(cx);
        let Some(me) = state.our_aci().map(str::to_owned) else { return };
        let backend = state.backend.clone();
        let db = backend.db().clone();
        let read = backend.spawn(async move { db.compose_contacts(&me).await });
        self.load = Some(cx.spawn(async move |this, cx| {
            let Ok(contacts) = read.await else { return };
            let candidates: Vec<Candidate> = contacts
                .into_iter()
                .map(|contact| {
                    let names = [
                        contact.title.as_str(),
                        &contact.profile_given,
                        &contact.profile_family,
                        &contact.nickname_given,
                        &contact.nickname_family,
                        contact.username.as_deref().unwrap_or_default(),
                    ]
                    .into_iter()
                    .filter(|n| !n.is_empty())
                    .map(searchable)
                    .collect();
                    Candidate { contact, names }
                })
                .collect();
            let _ = this.update(cx, |this, cx| {
                this.candidates = Rc::new(candidates);
                this.rebuild(cx);
            });
        }));
    }

    /// The groups to list (Desktop's `getComposableGroups`): ones we're
    /// in, not blocked or ended, with a name.
    fn groups(&self, cx: &App) -> Vec<Conversation> {
        let state = self.state.read(cx);
        state
            .conversations
            .iter()
            .chain(&state.archived)
            .filter(|c| c.kind == ConversationKind::Group && !c.left && !c.blocked && !c.ended && !c.title.trim().is_empty())
            .cloned()
            .collect()
    }

    /// Lays the rows out again for the query and data.
    fn rebuild(&mut self, cx: &mut Context<Self>) {
        let state = self.state.read(cx);
        let home = state.account.as_ref().and_then(|a| a.phone.clone());
        let term = self.query.trim();
        let now = chrono::Utc::now().timestamp_millis();
        let contacts = filter_and_sort(
            self.candidates.iter().map(|c| Entry {
                title: &c.contact.title,
                names: &c.names,
                phone: c.contact.phone.as_deref(),
                active_at: c.contact.active_at,
                archived: c.contact.archived,
            }),
            term,
            now,
        );
        let groups = self.groups(cx);
        let group_names: Vec<Vec<String>> = groups.iter().map(|g| vec![searchable(&g.title)]).collect();
        let group_order = filter_and_sort(
            groups.iter().zip(&group_names).map(|(g, names)| Entry {
                title: &g.title,
                names,
                phone: None,
                active_at: g.last_message_at,
                archived: g.archived,
            }),
            term,
            now,
        );
        let username = username_from_search(&self.query);
        let probably_username = is_probably_a_username(&self.query);
        let mut rows = Vec::new();
        if self.query.is_empty() {
            rows.push(Row::NewGroup);
        }
        if !contacts.is_empty() && !probably_username {
            rows.push(Row::Header("Contacts"));
            rows.extend(contacts.iter().map(|&ix| Row::Contact(ix)));
        }
        if !group_order.is_empty() && !probably_username {
            rows.push(Row::Header("Groups"));
            rows.extend(group_order.iter().map(|&ix| Row::Group(groups[ix].id.clone())));
        }
        // Desktop compares with the `username` of the contacts listed,
        // which it keeps only for people known by nothing else.
        let listed_username = |name: &str| {
            contacts.iter().any(|&ix| {
                let c = &self.candidates[ix].contact;
                c.username.as_deref() == Some(name) && c.title == name
            })
        };
        if let Some(name) = username.as_deref().filter(|name| !listed_username(name)) {
            rows.push(Row::Header("Find by username"));
            rows.push(Row::Username(name.to_owned()));
        }
        if username.is_none()
            && let Some(phone) = signal_backend::recipients::parse_phone(term, home.as_deref())
            && !contacts.iter().any(|&ix| self.candidates[ix].contact.phone.as_deref() == Some(phone.e164.as_str()))
        {
            rows.push(Row::Header("Find by phone number"));
            rows.push(Row::Phone(phone, term.to_owned()));
        }
        if *self.rows != rows {
            self.list.reset(rows.len());
            self.rows = Rc::new(rows);
        }
        cx.notify();
    }

    /// Opens a chat picked from the list, and closes the composer.
    fn open(&mut self, id: ConversationId, entry: Option<DirectoryEntry>, cx: &mut Context<Self>) {
        let archived = self.state.read(cx).archived.iter().any(|c| c.id == id);
        self.state.update(cx, |state, cx| match entry {
            Some(entry) if state.conversation(&id).is_none() => state.start_conversation(entry, cx),
            _ => state.select(Some(id), cx),
        });
        cx.emit(ComposeEvent::Opened { archived });
    }

    fn open_contact(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(candidate) = self.candidates.get(ix) else { return };
        let contact = &candidate.contact;
        let id = ConversationId::direct(&contact.service_id);
        let entry = DirectoryEntry { id: id.clone(), kind: ConversationKind::Direct, title: contact.title.clone(), phone: contact.phone.clone() };
        self.open(id, Some(entry), cx);
    }

    /// Looks a username or number up on Signal (Desktop's
    /// `lookupConversationWithoutServiceId`); ignored while one runs.
    fn look_up(&mut self, row: &Row, window: &mut Window, cx: &mut Context<Self>) {
        if self.finding.is_some() {
            return;
        }
        let (query, finding) = match row {
            Row::Username(name) => (name.clone(), Finding::Username(name.clone())),
            Row::Phone(phone, typed) => {
                if !phone.valid {
                    let text = format!("User not found. \u{201c}{typed}\u{201d} is not a valid phone number.");
                    window.open_alert_dialog(cx, move |alert, _, _| alert.description(text.clone()));
                    return;
                }
                (phone.e164.clone(), Finding::Phone(typed.clone()))
            }
            _ => return,
        };
        self.state.read(cx).send(signal_backend::Command::FindRecipient { query: query.clone() });
        self.finding = Some((query, finding));
        cx.notify();
    }

    /// The answer to a lookup of ours: its chat opens (the composer stays,
    /// as Desktop's `showConversation` without `switchToAssociatedView`
    /// leaves it), or Desktop's dialog or message says why not.
    fn found(&mut self, query: &str, result: &Result<DirectoryEntry, FindError>, window: &mut Window, cx: &mut Context<Self>) {
        let Some((_, finding)) = self.finding.take_if(|(q, _)| q == query) else { return };
        cx.notify();
        let error = match result {
            Ok(entry) => {
                let entry = entry.clone();
                self.state.update(cx, |state, cx| state.start_conversation(entry, cx));
                return;
            }
            Err(error) => error,
        };
        if let FindError::Failed(_) = error {
            let text = match finding {
                Finding::Username(_) => "Failed to fetch username. Check your connection and try again.",
                Finding::Phone(_) => "Failed to fetch phone number. Check your connection and try again.",
            };
            self.state.update(cx, |_, cx| cx.emit(AppEvent::Info(text.into())));
            return;
        }
        let text = match finding {
            Finding::Phone(typed) => format!("User not found. \u{201c}{typed}\u{201d} is not a Signal user."),
            Finding::Username(name) if !is_username_valid(&name) => format!(
                "{name} is not a valid username. Make sure you\u{2019}ve entered the complete username followed by its set of digits."
            ),
            Finding::Username(name) => {
                format!("{name} is not a Signal user. Make sure you\u{2019}ve entered the complete username.")
            }
        };
        window.open_alert_dialog(cx, move |alert, _, _| alert.description(text.clone()));
    }

    fn render_row(&mut self, ix: usize, cx: &mut Context<Self>) -> gpui_kit::AnyElement {
        let Some(row) = self.rows.get(ix).cloned() else { return div().into_any_element() };
        let palette = Palette::get(cx);
        match &row {
            Row::Header(text) => div()
                .h(px(HEADER_HEIGHT))
                .flex()
                .items_end()
                .pb(px(8.))
                .pl(px(16.))
                .text_size(px(14.))
                .font_weight(FontWeight::SEMIBOLD)
                .child(*text)
                .into_any_element(),
            Row::NewGroup => {
                let icon = step_icon(IconName::Users, palette);
                tile(SharedString::from("compose-new-group"), SELECT_HEIGHT, icon, div().child("New group").into_any_element(), None, Some("compose-new-group".into()), palette)
                    .on_click(cx.listener(|this, _, window, cx| {
                        crate::views::new_group::open(this.state.clone(), window, cx);
                        cx.emit(ComposeEvent::Close);
                    }))
                    .into_any_element()
            }
            Row::Contact(index) => {
                let Some(contact) = self.candidates.get(*index).map(|c| c.contact.clone()) else { return div().into_any_element() };
                let state = self.state.read(cx);
                let avatar = if contact.me {
                    crate::views::common::chat_placeholder(ConversationKind::NoteToSelf, &contact.title, 0, px(32.))
                } else {
                    crate::views::common::badged_avatar(state, &contact.service_id, &contact.title, 32., cx)
                };
                let name = div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .min_w_0()
                    .child(div().truncate().child(if contact.me { "Note to Self".to_owned() } else { contact.title.clone() }))
                    .when(contact.me, |d| d.child(crate::views::common::official_badge(palette)))
                    .when(contact.system_contact && !contact.me, |d| {
                        d.child(
                            div()
                                .debug_selector(|| "system-contact-emblem".into())
                                .child(Icon::new(IconName::CircleUser).size(px(14.)).text_color(palette.text_secondary)),
                        )
                    });
                let about = (!contact.me && !contact.about.is_empty()).then(|| match contact.about_emoji.as_str() {
                    "" => contact.about.clone(),
                    emoji => format!("{emoji} {}", contact.about),
                });
                let id = SharedString::from(format!("compose-contact-{}", contact.service_id));
                let selector = format!("compose-contact-{}", contact.title);
                let index = *index;
                tile(id, SELECT_HEIGHT, avatar, name.into_any_element(), about, Some(selector), palette)
                    .on_click(cx.listener(move |this, _, _, cx| this.open_contact(index, cx)))
                    .into_any_element()
            }
            Row::Group(id) => {
                let Some(group) = self.state.read(cx).conversation(id).cloned() else { return div().into_any_element() };
                let avatar = crate::views::common::conversation_avatar(&group, px(32.));
                let count = group.members.len();
                let members = if count == 1 { "1 member".to_owned() } else { format!("{count} members") };
                let selector = format!("compose-group-{}", group.title);
                let title = div().truncate().child(group.title.clone()).into_any_element();
                let id = group.id.clone();
                tile(SharedString::from(format!("compose-group-{}", group.id.0)), SELECT_HEIGHT, avatar, title, Some(members), Some(selector), palette)
                    .on_click(cx.listener(move |this, _, _, cx| this.open(id.clone(), None, cx)))
                    .into_any_element()
            }
            Row::Username(name) | Row::Phone(_, name) => {
                let fetching = self.finding.as_ref().is_some_and(|(query, _)| match &row {
                    Row::Username(name) => query == name,
                    Row::Phone(phone, _) => *query == phone.e164,
                    _ => false,
                });
                let avatar = div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .size(px(32.))
                    .rounded_full()
                    .bg(palette.row_hover)
                    .child(Icon::new(IconName::Search).size(px(16.)).text_color(palette.text))
                    .into_any_element();
                let selector = match &row {
                    Row::Username(_) => "compose-username-result",
                    _ => "compose-phone-result",
                };
                let title = div().truncate().child(name.clone()).into_any_element();
                tile(SharedString::from(selector), RESULT_HEIGHT, avatar, title, None, Some(selector.to_owned()), palette)
                    .when(fetching, |d| {
                        d.child(Icon::new(IconName::LoaderCircle).size(px(20.)).text_color(palette.text_secondary).flex_none().ml(px(12.)))
                    })
                    .on_click(cx.listener(move |this, _, window, cx| this.look_up(&row, window, cx)))
                    .into_any_element()
            }
        }
    }
}

/// Desktop's `ComposeStepButton` icon: a 32px circle with a 20px glyph.
fn step_icon(icon: IconName, palette: Palette) -> gpui_kit::AnyElement {
    div()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(px(32.))
        .rounded_full()
        .bg(palette.row_hover)
        .child(Icon::new(icon).size(px(20.)).text_color(palette.text))
        .into_any_element()
}

/// Desktop's `ListTile`: a row inset 10px from the pane's sides and 2px
/// from its neighbors, padded 6px by 14px, the leading avatar 12px from
/// the title and its subtitle.
fn tile(
    id: SharedString,
    height: f32,
    leading: gpui_kit::AnyElement,
    title: gpui_kit::AnyElement,
    subtitle: Option<String>,
    selector: Option<String>,
    palette: Palette,
) -> gpui_kit::Stateful<gpui_kit::Div> {
    let inner = div()
        .size_full()
        .flex()
        .items_center()
        .px(px(14.))
        .py(px(6.))
        .rounded(px(12.))
        .cursor_pointer()
        .hover(|d| d.bg(palette.row_hover))
        .child(div().flex_none().mr(px(12.)).child(leading))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(div().text_size(px(14.)).line_height(px(20.)).child(title))
                .when_some(subtitle, |d, text| {
                    d.child(div().truncate().text_size(px(12.)).line_height(px(17.)).text_color(palette.text_secondary).child(text))
                }),
        );
    div()
        .id(id)
        .when_some(selector, |d, s| d.debug_selector(move || s))
        .h(px(height))
        .px(px(10.))
        .py(px(2.))
        .flex()
        .child(inner)
}

impl Render for Compose {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = Palette::get(cx);
        let entity = cx.entity();
        let empty = self.rows.is_empty();
        div()
            .id("compose")
            .debug_selector(|| "compose".into())
            .size_full()
            .flex()
            .flex_col()
            // Escape goes back to the inbox (Desktop's `onEscapeShortcut`).
            .capture_key_down(cx.listener(|_, event: &gpui_kit::KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" {
                    cx.stop_propagation();
                    cx.emit(ComposeEvent::Close);
                }
            }))
            .child(
                div()
                    .flex()
                    .items_center()
                    .py(px(15.))
                    .pl(px(12.))
                    .child(
                        Button::new("compose-back")
                            .ghost()
                            .small()
                            .icon(IconName::ChevronLeft)
                            .tooltip("Back to inbox")
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(ComposeEvent::Close))),
                    )
                    .child(
                        div()
                            .flex_1()
                            .pr(px(40.))
                            .text_center()
                            .text_size(px(14.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("New chat"),
                    ),
            )
            .child(
                div()
                    .px(px(16.))
                    .pb(px(8.))
                    .child(Input::new(&self.search).prefix(Icon::new(IconName::Search).size_4().text_color(palette.text_secondary))),
            )
            .when(empty, |d| {
                d.child(
                    div()
                        .debug_selector(|| "compose-no-chats".into())
                        .mt(px(27.))
                        .px_4()
                        .w_full()
                        .text_center()
                        .child("No chats found"),
                )
            })
            .when(!empty, |d| {
                d.child(
                    div()
                        .relative()
                        .flex_1()
                        .min_h_0()
                        .child(
                            list(self.list.clone(), move |ix, _, cx| entity.update(cx, |this, cx| this.render_row(ix, cx)))
                                .size_full(),
                        )
                        .child(Scrollbar::vertical(&self.list).mode(ScrollbarMode::Hover)),
                )
            })
    }
}

/// Desktop's `toSearchableTokens` (`ts/util/filterAndSortConversations.std.ts`):
/// decomposed, without diacritics (here: without combining marks), with
/// punctuation as spaces; lowercased here, as Fuse matches without case.
fn searchable(text: &str) -> String {
    use unicode_normalization::UnicodeNormalization as _;
    use unicode_properties::{GeneralCategoryGroup, UnicodeGeneralCategory as _};
    text.nfd()
        .filter_map(|c| match c.general_category_group() {
            GeneralCategoryGroup::Mark => None,
            GeneralCategoryGroup::Punctuation => Some(' '),
            _ => Some(c),
        })
        .flat_map(char::to_lowercase)
        .collect()
}

/// What `filter_and_sort` sees of a contact or group.
struct Entry<'a> {
    title: &'a str,
    /// Its name fields, already `searchable`.
    names: &'a [String],
    phone: Option<&'a str>,
    active_at: i64,
    archived: bool,
}

/// Desktop's `filterAndSortConversations`: with no term, everyone by when
/// their chat was last active, then by name; with one, those it matches,
/// by Desktop's mix of match and recency. Returns indexes into `entries`.
///
/// Desktop matches with Fuse.js (fuzzy, threshold 0.2). Here each word of
/// the term must appear in one of the names, and a number matches the
/// stored number; the match's score is 0 when every word starts a word
/// of a name and 0.1 otherwise, standing in for Fuse's.
fn filter_and_sort<'a>(entries: impl Iterator<Item = Entry<'a>>, term: &str, now: i64) -> Vec<usize> {
    let entries: Vec<Entry<'a>> = entries.collect();
    let mut order: Vec<(usize, f64)> = if term.is_empty() {
        entries.iter().enumerate().map(|(ix, e)| (ix, -(e.active_at as f64))).collect()
    } else {
        let query = searchable(term);
        let words: Vec<&str> = query.split_whitespace().collect();
        let digits: String = term.chars().filter(char::is_ascii_digit).collect();
        entries
            .iter()
            .enumerate()
            .filter_map(|(ix, e)| {
                let score = match_score(e.names, &words).or_else(|| {
                    let phone: String = e.phone?.chars().filter(char::is_ascii_digit).collect();
                    (digits.len() >= 3 && phone.contains(&digits)).then_some(0.1)
                })?;
                let age = (now - e.active_at) as f64 / WEEK_MS * 0.01;
                Some((ix, age + score + if e.archived { 0.3 } else { 0. }))
            })
            .collect()
    };
    order.sort_by(|(a, sa), (b, sb)| sa.total_cmp(sb).then_with(|| alphabetical(entries[*a].title, entries[*b].title)));
    order.into_iter().map(|(ix, _)| ix).collect()
}

/// How well `words` match `names` (lower is better), or `None`.
fn match_score(names: &[String], words: &[&str]) -> Option<f64> {
    if words.is_empty() {
        return None;
    }
    let mut prefixes = true;
    for word in words {
        let starts = names.iter().any(|n| n.split_whitespace().any(|w| w.starts_with(word)));
        if !starts {
            if !names.iter().any(|n| n.contains(word)) {
                return None;
            }
            prefixes = false;
        }
    }
    Some(if prefixes { 0. } else { 0.1 })
}

/// Desktop's `sortAlphabetically`: names starting with a letter first,
/// then by name.
fn alphabetical(a: &str, b: &str) -> std::cmp::Ordering {
    let letter = |s: &str| s.chars().next().is_some_and(char::is_alphabetic);
    letter(b).cmp(&letter(a)).then_with(|| a.to_lowercase().cmp(&b.to_lowercase()))
}

/// Desktop's `USERNAME_LIKE`, `/^@?[a-zA-Z_][a-zA-Z0-9_]{2,31}(.\d*?)?$/`,
/// with its unescaped `.`: any one character may come before the digits.
fn username_like(text: &str) -> bool {
    let text = text.strip_prefix('@').unwrap_or(text);
    let chars: Vec<char> = text.chars().collect();
    let word = |c: char| c.is_ascii_alphanumeric() || c == '_';
    if !chars.first().is_some_and(|c| c.is_ascii_alphabetic() || *c == '_') {
        return false;
    }
    (2..=31).any(|k| {
        let Some(body) = chars.get(1..1 + k) else { return false };
        if !body.iter().all(|c| word(*c)) {
            return false;
        }
        let rest = &chars[1 + k..];
        rest.is_empty() || rest[1..].iter().all(char::is_ascii_digit)
    })
}

/// Desktop's `getUsernameFromSearch` (`ts/util/Username.std.ts`).
fn username_from_search(search: &str) -> Option<String> {
    let search = search.trim();
    if !search.is_empty() && search.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let name = search.strip_prefix('@').unwrap_or(search);
    username_like(name).then(|| name.to_owned())
}

/// Desktop's `isProbablyAUsername`: it starts with `@`, or ends in a dot
/// and two or three digits.
fn is_probably_a_username(text: &str) -> bool {
    let text = text.trim();
    if text.starts_with('@') {
        return true;
    }
    if !username_like(text) || text.chars().all(|c| c.is_ascii_digit()) {
        return false;
    }
    let Some((head, tail)) = text.rsplit_once('.') else { return false };
    !head.is_empty() && (2..=3).contains(&tail.len()) && tail.chars().all(|c| c.is_ascii_digit())
}

/// Desktop's `isUsernameValid`: a nickname of 3 to 32 letters, digits or
/// underscores (not starting with a digit), a dot, and digits that don't
/// start with `00` and are exactly two when they start with `0`.
fn is_username_valid(name: &str) -> bool {
    let nickname = name.split('.').next().unwrap_or_default();
    let Some((_, discriminator)) = name.rsplit_once('.') else { return false };
    let nickname_ok = (3..=32).contains(&nickname.chars().count())
        && nickname.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
        && nickname.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    let discriminator_ok = !discriminator.is_empty()
        && discriminator.chars().all(|c| c.is_ascii_digit())
        && !discriminator.starts_with("00")
        && (!discriminator.starts_with('0') || discriminator.len() == 2);
    nickname_ok && discriminator_ok
}

#[cfg(test)]
impl Compose {
    /// The rows as text, for tests.
    pub(crate) fn row_texts(&self, cx: &App) -> Vec<String> {
        self.rows
            .iter()
            .map(|row| match row {
                Row::NewGroup => "[New group]".to_owned(),
                Row::Header(text) => format!("# {text}"),
                Row::Contact(ix) => self.candidates[*ix].contact.title.clone(),
                Row::Group(id) => self.state.read(cx).conversation(id).map_or_else(String::new, |g| g.title.clone()),
                Row::Username(name) => format!("@ {name}"),
                Row::Phone(phone, _) => format!("tel {}{}", phone.e164, if phone.valid { "" } else { " (invalid)" }),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usernames_are_spotted_as_desktop_spots_them() {
        assert_eq!(username_from_search("bob").as_deref(), Some("bob"));
        assert_eq!(username_from_search("@alice.42").as_deref(), Some("alice.42"));
        assert_eq!(username_from_search("  alice_1 ").as_deref(), Some("alice_1"));
        for not in ["zz", "12345", "1bob", "two words", "@", ""] {
            assert_eq!(username_from_search(not), None, "{not}");
        }
        assert!(is_probably_a_username("@bob"));
        assert!(is_probably_a_username("alice.42") && is_probably_a_username("alice.123"));
        assert!(!is_probably_a_username("alice") && !is_probably_a_username("alice.1"));
        assert!(is_username_valid("alice.42") && is_username_valid("alice.01"));
        assert!(!is_username_valid("alice") && !is_username_valid("al.42") && !is_username_valid("alice.007"));
    }

    #[test]
    fn search_ignores_case_diacritics_and_punctuation() {
        assert_eq!(searchable("Élodie O'Brien-Ünal"), "elodie o brien unal");
    }

    fn entry<'a>(title: &'a str, names: &'a [String], active_at: i64) -> Entry<'a> {
        Entry { title, names, phone: None, active_at, archived: false }
    }

    #[test]
    fn people_sort_by_activity_then_name_and_filter_by_words() {
        let names: Vec<Vec<String>> = ["Maya Chen", "Ana Lima", "Ben Ortiz", "42 Club"].iter().map(|n| vec![searchable(n)]).collect();
        let list = |term: &str, active: [i64; 4]| {
            filter_and_sort(
                ["Maya Chen", "Ana Lima", "Ben Ortiz", "42 Club"].iter().zip(&names).zip(active).map(|((t, n), a)| entry(t, n, a)),
                term,
                1_000_000,
            )
        };
        // No term: most recent first, then letters before digits, then A-Z.
        assert_eq!(list("", [0, 0, 500, 0]), [2, 1, 0, 3]);
        // Every word must match; a word's start beats its middle.
        assert_eq!(list("ma", [0, 0, 0, 0]), [0, 1], "Maya by her name's start, Ana Lima inside hers");
        assert_eq!(list("ana li", [0; 4]), [1]);
        assert_eq!(list("zed", [0; 4]), Vec::<usize>::new());
    }

    #[test]
    fn a_number_matches_the_stored_number() {
        let names = vec![searchable("Dana Brooks")];
        let dana = Entry { title: "Dana Brooks", names: &names, phone: Some("+14155550177"), active_at: 0, archived: false };
        assert_eq!(filter_and_sort([dana].into_iter(), "415 555", 0), [0]);
    }

    mod view {
        use gpui_kit::{TestAppContext, VisualTestContext, WindowOptions};
        use signal_backend::{Backend, Config};

        use super::super::*;
        use crate::views::sidebar::Sidebar;

        struct Mounted {
            backend: Backend,
            state: Entity<AppState>,
            sidebar: Entity<Sidebar>,
            cx: VisualTestContext,
            _dir: tempfile::TempDir,
        }

        /// A sidebar over the demo, with backend events going into the
        /// model as `main.rs` sends them, once the demo's chats are in.
        fn mount(cx: &mut TestAppContext) -> Mounted {
            cx.update(gpui_kit::init);
            cx.executor().allow_parking();
            let dir = tempfile::tempdir().unwrap();
            let config = Config {
                data_dir: dir.path().to_owned(),
                db_key: [7; 32],
                device_name: "test".into(),
                demo: true,
                former_data_dirs: Vec::new(),
                call_lines: Default::default(),
            };
            let (backend, mut events) = Backend::start(config).unwrap();
            let state = cx.update(|cx| crate::state::new_state(backend.clone(), cx));
            cx.update(|cx| {
                use futures::StreamExt as _;
                let state = state.clone();
                cx.spawn(async move |cx| {
                    while let Some(event) = events.next().await {
                        state.update(cx, |state, cx| state.handle(event, cx));
                    }
                })
                .detach();
            });
            let (window, sidebar) = cx.update(|cx| {
                let state = state.clone();
                gpui_kit::open_window(WindowOptions::default(), cx, |window, cx| cx.new(|cx| Sidebar::new(state, window, cx))).unwrap()
            });
            let mut cx = VisualTestContext::from_window(window, cx);
            wait(&mut cx, |cx| state.read_with(cx, |s, _| s.our_aci().is_some() && s.conversations.len() > 3));
            Mounted { backend, state, sidebar, cx, _dir: dir }
        }

        /// Runs until `done`, for at most three seconds.
        fn wait(cx: &mut VisualTestContext, mut done: impl FnMut(&mut VisualTestContext) -> bool) {
            for _ in 0..300 {
                cx.run_until_parked();
                if done(cx) {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            panic!("timed out");
        }

        fn compose(m: &mut Mounted) -> Entity<Compose> {
            m.sidebar.read_with(&m.cx, |s, _| s.compose()).expect("New chat is open")
        }

        fn rows(m: &mut Mounted) -> Vec<String> {
            let compose = compose(m);
            compose.read_with(&m.cx, |c, cx| c.row_texts(cx))
        }

        fn type_in(m: &mut Mounted, text: &str) {
            let compose = compose(m);
            // Setting the value doesn't send `InputEvent::Change`, which
            // typing does; this is what that does.
            compose.update_in(&mut m.cx, |c, window, cx| {
                c.search.update(cx, |input, cx| input.set_value(text, window, cx));
                c.query = text.to_owned();
                c.rebuild(cx);
            });
            m.cx.run_until_parked();
        }

        fn open(m: &mut Mounted) {
            m.sidebar.update_in(&mut m.cx, |s, window, cx| s.open_compose(window, cx));
            // The contacts load in the background.
            let sidebar = m.sidebar.clone();
            wait(&mut m.cx, |cx| {
                let Some(compose) = sidebar.read_with(cx, |s, _| s.compose()) else { return false };
                compose.read_with(cx, |c, cx| c.row_texts(cx).iter().any(|r| r == "Maya Chen"))
            });
        }

        #[gpui_kit::test]
        fn new_chat_lists_contacts_then_groups_and_closes_back_to_the_search(cx: &mut TestAppContext) {
            let mut m = mount(cx);
            // Something typed in the inbox's search stays for later.
            m.sidebar.update_in(&mut m.cx, |s, window, cx| s.search_input().update(cx, |i, cx| i.set_value("hike", window, cx)));
            m.cx.run_until_parked();
            open(&mut m);
            let rows = rows(&mut m);
            assert_eq!(rows[..2], ["[New group]".to_owned(), "# Contacts".to_owned()]);
            let groups = rows.iter().position(|r| r == "# Groups").expect("a Groups section");
            for person in ["Maya Chen", "Jordan Reyes", "Note to Self"] {
                assert!(rows[2..groups].iter().any(|r| r == person), "{person} in {rows:?}");
            }
            assert!(rows[groups..].iter().any(|r| r == "Saturday hikes 🥾"));
            // Dana is in the directory but not trusted: not listed.
            assert!(!rows.iter().any(|r| r == "Dana Brooks"));
            assert!(m.cx.debug_bounds("compose").is_some());
            // Escape goes back, the search as it was.
            m.cx.simulate_keystrokes("escape");
            m.cx.run_until_parked();
            assert!(m.sidebar.read_with(&m.cx, |s, _| s.compose().is_none()));
            let query = m.sidebar.read_with(&m.cx, |s, cx| s.search_input().read(cx).value().to_string());
            assert_eq!(query, "hike");
            m.backend.shutdown();
        }

        #[gpui_kit::test]
        fn typing_narrows_the_list_and_offers_lookups(cx: &mut TestAppContext) {
            let mut m = mount(cx);
            open(&mut m);
            type_in(&mut m, "maya");
            let found = rows(&mut m);
            assert_eq!(found[..2], ["# Contacts".to_owned(), "Maya Chen".to_owned()], "{found:?}");
            // "maya" could be a username, so Desktop offers to look it up.
            assert_eq!(found.last().map(String::as_str), Some("@ maya"));
            // A username with its digits hides the contacts.
            type_in(&mut m, "maya.42");
            assert_eq!(rows(&mut m), ["# Find by username", "@ maya.42"]);
            // Nothing matches.
            type_in(&mut m, "zz");
            assert!(rows(&mut m).is_empty());
            m.cx.update(|window, _| window.refresh());
            m.cx.run_until_parked();
            assert!(m.cx.debug_bounds("compose-no-chats").is_some());
            // A number no one listed has.
            type_in(&mut m, "+1 415 555 0177");
            assert_eq!(rows(&mut m), ["# Find by phone number", "tel +14155550177"]);
            m.backend.shutdown();
        }

        #[gpui_kit::test]
        fn picking_a_contact_opens_their_chat_and_closes_new_chat(cx: &mut TestAppContext) {
            let mut m = mount(cx);
            open(&mut m);
            type_in(&mut m, "jordan");
            let compose = compose(&mut m);
            compose.update(&mut m.cx, |c, cx| {
                let ix = c.rows.iter().find_map(|r| match r {
                    Row::Contact(ix) => Some(*ix),
                    _ => None,
                });
                c.open_contact(ix.unwrap(), cx);
            });
            m.cx.run_until_parked();
            let jordan = m.state.read_with(&m.cx, |s, _| s.selected.clone().map(|id| s.conversation(&id).map(|c| c.title.clone())));
            assert_eq!(jordan.flatten().as_deref(), Some("Jordan Reyes"));
            assert!(m.sidebar.read_with(&m.cx, |s, _| s.compose().is_none()));
            m.backend.shutdown();
        }

        #[gpui_kit::test]
        fn a_username_found_opens_its_chat_and_one_not_found_says_so(cx: &mut TestAppContext) {
            let mut m = mount(cx);
            open(&mut m);
            type_in(&mut m, "maya.42");
            let compose = compose(&mut m);
            compose.update_in(&mut m.cx, |c, window, cx| c.look_up(&Row::Username("maya.42".into()), window, cx));
            let state = m.state.clone();
            wait(&mut m.cx, |cx| state.read_with(cx, |s, _| s.selected.as_ref().is_some_and(|id| id.0.ends_with("0000000000a1") || s.conversation(id).is_some_and(|c| c.title == "Maya Chen"))));
            // New chat stays open, as Desktop's does after a lookup.
            assert!(m.sidebar.read_with(&m.cx, |s, _| s.compose().is_some()));
            // No one has this one: Desktop's dialog.
            compose.update_in(&mut m.cx, |c, window, cx| c.look_up(&Row::Username("nobody.99".into()), window, cx));
            wait(&mut m.cx, |cx| cx.update(|window, cx| window.has_active_dialog(cx)));
            assert!(compose.read_with(&m.cx, |c, _| c.finding.is_none()));
            m.backend.shutdown();
        }

        #[gpui_kit::test]
        fn an_invalid_number_says_so_without_a_lookup(cx: &mut TestAppContext) {
            let mut m = mount(cx);
            open(&mut m);
            let compose = compose(&mut m);
            let row = Row::Phone(ParsedPhone { e164: "+1555".into(), valid: false }, "555".into());
            compose.update_in(&mut m.cx, |c, window, cx| c.look_up(&row, window, cx));
            m.cx.run_until_parked();
            assert!(m.cx.update(|window, cx| window.has_active_dialog(cx)));
            assert!(compose.read_with(&m.cx, |c, _| c.finding.is_none()), "nothing looked up");
            m.backend.shutdown();
        }
    }
}
