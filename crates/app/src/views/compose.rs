//! Desktop's "New chat" (`ts/components/leftPane/LeftPaneComposeHelper.dom.tsx`):
//! the left pane's composer, opened by the pencil button or ⌘N. A search
//! for a name, username or number above New group, then everyone and
//! every group you could message; typing narrows them, and offers to look
//! up a username or number on Signal.
//!
//! "Find by username" and "Find by phone number" replace it in the pane
//! (`LeftPaneFindByUsernameHelper`, `LeftPaneFindByPhoneNumberHelper`),
//! each a field and a Next button that looks the entry up.
//!
//! "New group" goes on in the pane too: "Choose members"
//! (`LeftPaneChooseGroupMembersHelper`), then "Name this group"
//! (`LeftPaneSetGroupMetadataHelper`), which creates it.
//!
//! Not here yet: the contacts' "Manage Contact" menu, whose Remove the
//! backend doesn't do.

use std::collections::{BTreeSet, HashMap};
use std::rc::Rc;

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::scroll::{Scrollbar, ScrollbarMode};
use gpui_kit::component::{Disableable as _, Icon, Sizable as _, WindowExt as _};
use gpui_kit::prelude::*;
use gpui_kit::{App, Context, Entity, FontWeight, ListAlignment, ListOffset, ListState, SharedString, Subscription, Task, Window, div, list, px};
use signal_backend::recipients::{FindError, ParsedPhone};
use signal_store::{ComposeContact, Conversation, ConversationId, ConversationKind, DirectoryEntry};

use crate::state::{AppEvent, AppState};
use crate::theme::Palette;
use crate::views::new_group::{Alerts, Limits, SizeAlert};

/// Row heights (Desktop's `ConversationList` `calculateRowHeight`).
const HEADER_HEIGHT: f32 = 40.;
const SELECT_HEIGHT: f32 = 52.;
const RESULT_HEIGHT: f32 = 76.;
const WEEK_MS: f64 = 7. * 24. * 60. * 60. * 1000.;

/// One row of the list, in Desktop's order (`LeftPaneComposeHelper.getRow`).
#[derive(Debug, Clone, PartialEq)]
enum Row {
    NewGroup,
    /// The "Find by username" and "Find by phone number" buttons.
    FindByUsername,
    FindByPhoneNumber,
    Header(&'static str),
    /// An index into the loaded contacts.
    Contact(usize),
    Group(ConversationId),
    /// "Find by username": the username typed.
    Username(String),
    /// "Find by phone number": the number as read, and as typed.
    Phone(ParsedPhone, String),
    /// Choose members' rows: a contact to pick (`ContactCheckbox`), and a
    /// username or number to look up and pick (`UsernameCheckbox`,
    /// `PhoneNumberCheckbox`).
    Checkbox(usize),
    UsernameCheckbox(String),
    PhoneCheckbox(ParsedPhone, String),
}

/// Asks the sidebar to close the composer.
pub enum ComposeEvent {
    /// Back to the inbox (back button, Escape).
    Close,
    /// Closed for a dialog that takes the keyboard (New group), so the
    /// inbox search doesn't take it back.
    Handoff,
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

/// Which pane shows (Desktop's `ComposerStep`, the steps built here).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    NewChat,
    FindByUsername,
    FindByPhoneNumber,
    ChooseMembers,
    SetGroupMetadata,
}

/// The group being made (Desktop's group-creation state), kept from
/// Choose members to Name this group and back.
struct GroupDraft {
    /// Who's picked, and in the order picked (the pills' order).
    chosen: BTreeSet<String>,
    order: Vec<String>,
    /// Names of people picked by a lookup, who aren't listed.
    found_names: HashMap<String, String>,
    alerts: Alerts,
    /// The picked photo: its file, and its bytes.
    avatar: Option<(std::path::PathBuf, Vec<u8>)>,
    /// The disappearing message timer, in seconds.
    timer: u32,
    name: Entity<InputState>,
    /// "Create" was pressed and the backend hasn't answered.
    creating: bool,
    _subscription: Subscription,
}

/// Desktop's `maxLengthCount` for a group's name, in characters.
const GROUP_NAME_MAX: usize = 32;

/// A Find pane's field, and for a number the country picked.
struct Find {
    input: Entity<InputState>,
    /// Desktop's `selectedRegion`: none until one is picked or a typed
    /// country code picks one.
    region: Option<String>,
    _subscription: Subscription,
}

pub struct Compose {
    state: Entity<AppState>,
    step: Step,
    /// The Find pane's field, while one shows.
    find: Option<Find>,
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
    /// New group's choices, from Choose members on.
    group: Option<GroupDraft>,
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
                    this.rebuild(false, cx);
                }
            }),
            cx.subscribe_in(&state, window, |this, _, event: &AppEvent, window, cx| match event {
                // A profile brings a new name to search and a new About.
                AppEvent::ConversationsChanged | AppEvent::ProfileChanged(_) => this.load(cx),
                AppEvent::RecipientFound { query, result } => this.found(query, result, window, cx),
                AppEvent::GroupCreated => this.group_created(cx),
                // The dialog is the state's (Desktop's `CREATE_GROUP_REJECTED`).
                AppEvent::GroupCreateFailed => {
                    if let Some(group) = &mut this.group {
                        group.creating = false;
                        cx.notify();
                    }
                }
                _ => {}
            }),
        ];
        search.update(cx, |search, cx| search.focus(window, cx));
        let mut this = Self {
            state,
            step: Step::NewChat,
            find: None,
            search,
            query: String::new(),
            candidates: Rc::default(),
            rows: Rc::default(),
            list: ListState::new(0, ListAlignment::Top, px(SELECT_HEIGHT * 8.)),
            finding: None,
            group: None,
            load: None,
            _subscriptions: subscriptions,
        };
        this.load(cx);
        this.rebuild(false, cx);
        this
    }

    /// Its field: New chat's (and Choose members') search, the Find
    /// pane's, or the group's name.
    pub fn search_input(&self) -> Entity<InputState> {
        match (self.step, &self.find, &self.group) {
            (Step::SetGroupMetadata, _, Some(group)) => group.name.clone(),
            (_, Some(find), _) => find.input.clone(),
            _ => self.search.clone(),
        }
    }

    /// A group is being created: nothing leaves the pane until it's answered.
    fn creating(&self) -> bool {
        self.group.as_ref().is_some_and(|g| g.creating)
    }

    /// The pencil or ⌘N while it's open (Desktop's `START_COMPOSING`): a
    /// Find pane goes back to a fresh New chat; New chat keeps its search.
    /// A Find pane's lookup isn't dropped (back and Escape wait for it too;
    /// Desktop's still opens its chat when it comes).
    pub fn start(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.step != Step::NewChat && self.finding.is_none() && !self.creating() {
            self.show_new_chat(window, cx);
        }
        self.search_input().update(cx, |input, cx| input.focus(window, cx));
    }

    /// Shows a Find pane (Desktop's `showFindByUsername`, `showFindByPhoneNumber`).
    /// Not while New chat's own lookup runs: its answer would land on the pane.
    fn show_find(&mut self, step: Step, window: &mut Window, cx: &mut Context<Self>) {
        if self.finding.is_some() {
            return;
        }
        let placeholder = if step == Step::FindByUsername { "Username" } else { "Phone number" };
        let input = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder));
        let subscription = cx.subscribe_in(&input, window, |this, input, event: &InputEvent, window, cx| match event {
            InputEvent::Change if this.step == Step::FindByPhoneNumber => {
                let text = input.read(cx).value().to_string();
                let selected = this.find.as_ref().and_then(|f| f.region.clone());
                if let Some((region, national)) = signal_backend::recipients::switch_country(&text, selected.as_deref()) {
                    if let Some(find) = &mut this.find {
                        find.region = Some(region);
                    }
                    input.update(cx, |input, cx| input.set_value(national, window, cx));
                }
                cx.notify();
            }
            InputEvent::Change => cx.notify(),
            InputEvent::PressEnter { .. } => this.next(window, cx),
            _ => {}
        });
        input.update(cx, |input, cx| input.focus(window, cx));
        self.find = Some(Find { input, region: None, _subscription: subscription });
        self.step = step;
        cx.notify();
    }

    /// Back from a Find pane to a fresh New chat (Desktop's `startComposing`),
    /// which isn't while a lookup runs.
    /// From Choose members it's New chat again, the picks dropped (Desktop's
    /// `startComposing`); from Name this group, Choose members with them
    /// kept (`showChooseGroupMembers`), not while creating.
    fn back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.finding.is_some() || self.creating() {
            return;
        }
        match self.step {
            Step::SetGroupMetadata => self.show_choose_members(window, cx),
            _ => self.show_new_chat(window, cx),
        }
    }

    fn show_new_chat(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.step = Step::NewChat;
        self.find = None;
        self.group = None;
        self.finding = None;
        self.query.clear();
        self.search.update(cx, |input, cx| {
            input.set_value("", window, cx);
            input.focus(window, cx);
        });
        self.rebuild(false, cx);
    }

    /// Choose members (Desktop's `showChooseGroupMembers`): a fresh pick
    /// from New chat, or the same one back from Name this group; the
    /// search starts empty either way. Not while a lookup runs.
    fn show_choose_members(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.finding.is_some() {
            return;
        }
        if self.group.is_none() {
            // Desktop starts a new group with the default timer for new
            // chats (`universalExpireTimer`).
            let timer = self.state.read(cx).prefs.universal_timer;
            let name = cx.new(|cx| InputState::new(window, cx).placeholder("Group name (required)"));
            let subscription = cx.subscribe_in(&name, window, |this, input, event: &InputEvent, window, cx| match event {
                // Desktop's `Input` refuses more than 32 characters.
                InputEvent::Change => {
                    use unicode_segmentation::UnicodeSegmentation as _;
                    let text = input.read(cx).value().to_string();
                    if text.graphemes(true).count() > GROUP_NAME_MAX {
                        let kept: String = text.graphemes(true).take(GROUP_NAME_MAX).collect();
                        input.update(cx, |input, cx| input.set_value(kept, window, cx));
                    }
                    cx.notify();
                }
                InputEvent::PressEnter { .. } => this.create(cx),
                _ => {}
            });
            self.group = Some(GroupDraft {
                chosen: BTreeSet::new(),
                order: Vec::new(),
                found_names: HashMap::new(),
                alerts: Alerts::default(),
                avatar: None,
                timer,
                name,
                creating: false,
                _subscription: subscription,
            });
        }
        self.step = Step::ChooseMembers;
        self.find = None;
        self.query.clear();
        self.search.update(cx, |input, cx| {
            input.set_value("", window, cx);
            input.focus(window, cx);
        });
        self.rebuild(false, cx);
    }

    /// Name this group (Desktop's `startSettingGroupMetadata`).
    fn show_set_metadata(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(group) = &self.group else { return };
        // Desktop disables Next past the hard limit, which picking can't reach.
        if group.chosen.len() + 1 > Limits::now(self.state.read(cx)).hard {
            return;
        }
        let name = group.name.clone();
        self.step = Step::SetGroupMetadata;
        name.update(cx, |input, cx| input.focus(window, cx));
        cx.notify();
    }

    /// Picks or unpicks someone for the new group (Desktop's
    /// `toggleSelectedContactForGroupAddition`), with its size alerts,
    /// which count the members besides us.
    fn toggle_member(&mut self, sid: &str, window: &mut Window, cx: &mut Context<Self>) {
        let limits = Limits::now(self.state.read(cx));
        let Some(group) = &mut self.group else { return };
        let alert = crate::views::new_group::toggle(&mut group.chosen, sid, 1, limits, &mut group.alerts);
        if group.chosen.contains(sid) {
            if !group.order.iter().any(|s| s == sid) {
                group.order.push(sid.to_owned());
            }
        } else {
            group.order.retain(|s| s != sid);
        }
        if let Some(alert) = alert {
            let max = match alert {
                SizeAlert::Maximum => limits.hard - 1,
                SizeAlert::Recommended => limits.recommended - 1,
            };
            crate::views::new_group::show_alert(alert, max, window, cx);
        }
        cx.notify();
    }

    /// A picked person's name, and the first name a pill shows (Desktop's
    /// `systemGivenName ?? firstName`, else the title; here the profile's
    /// given name, else the title).
    fn member_names(&self, sid: &str) -> (String, String) {
        if let Some(c) = self.candidates.iter().map(|c| &c.contact).find(|c| c.service_id == sid) {
            let first = if c.profile_given.trim().is_empty() { c.title.clone() } else { c.profile_given.clone() };
            return (c.title.clone(), first);
        }
        let title = self.group.as_ref().and_then(|g| g.found_names.get(sid).cloned()).unwrap_or_else(|| "Unknown".into());
        (title.clone(), title)
    }

    /// "Create" (Desktop's `createGroup`): for a name that isn't blank, and
    /// not twice.
    fn create(&mut self, cx: &mut Context<Self>) {
        if self.step != Step::SetGroupMetadata {
            return;
        }
        let Some(group) = &mut self.group else { return };
        let title = group.name.read(cx).value().trim().to_string();
        if group.creating || title.is_empty() {
            return;
        }
        group.creating = true;
        let command = signal_backend::Command::CreateGroup {
            title,
            members: group.order.clone(),
            avatar: group.avatar.as_ref().map(|(_, bytes)| bytes.clone()),
            expire_timer: group.timer,
        };
        self.state.read(cx).send(command);
        cx.notify();
    }

    /// The group we created opened (the state selected it): the composer
    /// closes, to the inbox (Desktop's `showConversation` with
    /// `switchToAssociatedView`).
    fn group_created(&mut self, cx: &mut Context<Self>) {
        if self.creating() {
            if let Some(group) = &mut self.group {
                group.creating = false;
            }
            cx.emit(ComposeEvent::Opened { archived: false });
        }
    }

    /// Picks a photo for the new group (Desktop's `AvatarPreview` upload).
    fn pick_photo(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(gpui_kit::PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Choose a group photo".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = picked.await else { return };
            let Some(path) = paths.into_iter().next() else { return };
            let bytes = match std::fs::read(&path) {
                Ok(bytes) => bytes,
                Err(error) => return tracing::warn!(%error, "couldn't read the group photo"),
            };
            let _ = this.update(cx, |this, cx| {
                if let Some(group) = &mut this.group {
                    group.avatar = Some((path, bytes));
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// The account's region (Desktop's `regionCode`), from its number.
    fn home_region(&self, cx: &App) -> Option<String> {
        let phone = self.state.read(cx).account.as_ref()?.phone.clone()?;
        signal_backend::recipients::region_of(&phone)
    }

    /// What Next would look up, if it may (Desktop's `#doLookup` checks):
    /// a username-like entry, or a valid number read with the country
    /// picked or the account's.
    fn next_lookup(&self, cx: &App) -> Option<Row> {
        let find = self.find.as_ref()?;
        let text = find.input.read(cx).value().to_string();
        match self.step {
            Step::FindByUsername => username_from_search(&text).map(Row::Username),
            Step::FindByPhoneNumber => {
                let region = find.region.clone().or_else(|| self.home_region(cx));
                let phone = signal_backend::recipients::parse_phone_in(&text, region.as_deref())?;
                phone.valid.then(|| Row::Phone(phone, text.trim().to_owned()))
            }
            Step::NewChat | Step::ChooseMembers | Step::SetGroupMetadata => None,
        }
    }

    /// Next, or Enter in a Find pane's field.
    fn next(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.finding.is_some() {
            return;
        }
        if let Some(row) = self.next_lookup(cx) {
            self.look_up(&row, window, cx);
        }
    }

    /// Picks a country (Desktop's `setComposeSelectedRegion`).
    fn pick_country(&mut self, region: &str, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(find) = &mut self.find {
            find.region = Some(region.to_owned());
            let input = find.input.clone();
            input.update(cx, |input, cx| input.focus(window, cx));
        }
        cx.notify();
    }

    /// Desktop's `ChooseCountryCodeModal`: "Country code", a search, and
    /// every country with its calling code; picking one closes it. Desktop
    /// searches names and codes with Fuse (threshold 0.1); here a name has
    /// to contain the text (without case or accents), or a code the digits.
    fn open_country_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search"));
        let focus = search.clone();
        let countries = Rc::new(signal_backend::countries::countries());
        let entity = cx.entity();
        window.open_dialog(cx, move |dialog, _, cx| {
            let palette = Palette::get(cx);
            let term = search.read(cx).value().to_string();
            let rows = country_matches(&countries, &term).into_iter().map(|ix| {
                let country = countries[ix].clone();
                let entity = entity.clone();
                let selector = format!("country-{}", country.region);
                div()
                    .id(SharedString::from(selector.clone()))
                    .debug_selector(move || selector)
                    .flex()
                    .items_center()
                    .gap_3()
                    .px_2()
                    .py_1p5()
                    .rounded_lg()
                    .cursor_pointer()
                    .hover(|d| d.bg(palette.row_hover))
                    .child(div().flex_1().truncate().child(country.name))
                    .child(div().text_color(palette.text_secondary).child(country.code.clone()))
                    .on_click(move |_, window, cx| {
                        window.close_dialog(cx);
                        entity.update(cx, |this, cx| this.pick_country(country.region, window, cx));
                    })
            });
            dialog
                .title("Country code")
                .w(px(440.))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(Input::new(&search).cleanable(true))
                        .child(div().id("country-list").h(px(360.)).overflow_y_scroll().children(rows)),
                )
        });
        // After opening: the dialog takes the focus as it opens.
        focus.update(cx, |input, cx| input.focus(window, cx));
    }

    /// Reads who to list again.
    fn load(&mut self, cx: &mut Context<Self>) {
        let state = self.state.read(cx);
        let Some(me) = state.our_aci().map(str::to_owned) else { return };
        let backend = state.backend.clone();
        let db = backend.db().clone();
        let read = backend.spawn(async move { db.compose_contacts(&me).await });
        self.load = Some(cx.spawn(async move |this, cx| {
            let contacts = match read.await {
                Ok(contacts) => contacts,
                Err(error) => {
                    tracing::warn!(%error, "couldn't read who New chat lists");
                    return;
                }
            };
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
                this.rebuild(true, cx);
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

    /// Lays the rows out again for the query and data. `keep_place` keeps
    /// the scroll position (the data changed: a message reorders the list);
    /// a new query starts at the top.
    fn rebuild(&mut self, keep_place: bool, cx: &mut Context<Self>) {
        let state = self.state.read(cx);
        let home = state.account.as_ref().and_then(|a| a.phone.clone());
        let term = self.query.trim();
        let now = chrono::Utc::now().timestamp_millis();
        // The term as a number, read with the account's region as Desktop
        // reads it, so a local number finds a contact's stored one.
        let parsed = signal_backend::recipients::parse_phone(term, home.as_deref());
        let e164 = parsed.as_ref().map(|p| p.e164.as_str());
        let contacts = filter_and_sort(
            self.candidates.iter().map(|c| Entry {
                title: &c.contact.title,
                names: &c.names,
                phone: c.contact.phone.as_deref(),
                active_at: c.contact.active_at,
                archived: c.contact.archived,
            }),
            term,
            e164,
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
            None,
            now,
        );
        let username = username_from_search(&self.query);
        let probably_username = is_probably_a_username(&self.query);
        let mut rows = Vec::new();
        if self.step == Step::ChooseMembers {
            self.member_rows(&mut rows, &contacts, username.as_deref(), parsed.clone(), term);
        } else if self.step == Step::NewChat || self.step == Step::FindByUsername || self.step == Step::FindByPhoneNumber {
            self.new_chat_rows(&mut rows, &contacts, &groups, &group_order, username.as_deref(), probably_username, parsed.clone(), term);
        }
        if *self.rows != rows {
            if keep_place {
                // As the chat list does: `reset` drops the place at the next
                // layout, so splice to the new count and remeasure instead.
                let top = self.list.logical_scroll_top();
                let old = self.list.item_count();
                if rows.len() > old {
                    self.list.splice(old..old, rows.len() - old);
                } else if rows.len() < old {
                    self.list.splice(rows.len()..old, 0);
                }
                self.list.scroll_to(ListOffset { item_ix: top.item_ix.min(rows.len().saturating_sub(1)), offset_in_item: top.offset_in_item });
                self.list.remeasure();
            } else {
                self.list.reset(rows.len());
            }
            self.rows = Rc::new(rows);
        }
        cx.notify();
    }

    /// New chat's rows (Desktop's `LeftPaneComposeHelper.getRow`).
    #[allow(clippy::too_many_arguments)]
    fn new_chat_rows(
        &self,
        rows: &mut Vec<Row>,
        contacts: &[usize],
        groups: &[Conversation],
        group_order: &[usize],
        username: Option<&str>,
        probably_username: bool,
        parsed: Option<ParsedPhone>,
        term: &str,
    ) {
        if self.query.is_empty() {
            rows.extend([Row::NewGroup, Row::FindByUsername, Row::FindByPhoneNumber]);
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
        if let Some(name) = username.filter(|name| !listed_username(name)) {
            rows.push(Row::Header("Find by username"));
            rows.push(Row::Username(name.to_owned()));
        }
        if username.is_none()
            && let Some(phone) = parsed
            && !contacts.iter().any(|&ix| self.candidates[ix].contact.phone.as_deref() == Some(phone.e164.as_str()))
        {
            rows.push(Row::Header("Find by phone number"));
            rows.push(Row::Phone(phone, term.to_owned()));
        }
    }

    /// Choose members' rows (Desktop's `LeftPaneChooseGroupMembersHelper.getRow`):
    /// everyone New chat lists but us, then a number to look up, or a
    /// username, unless it's ours or someone listed's.
    fn member_rows(&self, rows: &mut Vec<Row>, contacts: &[usize], username: Option<&str>, parsed: Option<ParsedPhone>, term: &str) {
        let listed: Vec<usize> = contacts.iter().copied().filter(|&ix| !self.candidates[ix].contact.me).collect();
        if !listed.is_empty() {
            rows.push(Row::Header("Contacts"));
            rows.extend(listed.iter().map(|&ix| Row::Checkbox(ix)));
        }
        let me = self.candidates.iter().map(|c| &c.contact).find(|c| c.me);
        let ours_username = me.and_then(|c| c.username.as_deref());
        let ours_phone = me.and_then(|c| c.phone.as_deref());
        let username = username.filter(|name| {
            Some(*name) != ours_username && !listed.iter().any(|&ix| self.candidates[ix].contact.username.as_deref() == Some(*name))
        });
        if username.is_none()
            && let Some(phone) = parsed
            && Some(phone.e164.as_str()) != ours_phone
            && !listed.iter().any(|&ix| self.candidates[ix].contact.phone.as_deref() == Some(phone.e164.as_str()))
        {
            rows.push(Row::Header("Find by phone number"));
            rows.push(Row::PhoneCheckbox(phone, term.to_owned()));
        }
        if let Some(name) = username {
            rows.push(Row::Header("Find by username"));
            rows.push(Row::UsernameCheckbox(name.to_owned()));
        }
    }

    /// Opens a chat picked from the list, and closes the composer.
    fn open(&mut self, id: ConversationId, entry: Option<DirectoryEntry>, cx: &mut Context<Self>) {
        let archived = self.state.read(cx).archived.iter().any(|c| c.id == id);
        self.open_into(id, entry, archived, cx);
    }

    /// [`Self::open`], showing the archive after or not.
    fn open_into(&mut self, id: ConversationId, entry: Option<DirectoryEntry>, archived: bool, cx: &mut Context<Self>) {
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
    /// `lookupConversationWithoutServiceId`); ignored while the same one
    /// runs. Another takes its place, and the earlier answer is dropped.
    fn look_up(&mut self, row: &Row, window: &mut Window, cx: &mut Context<Self>) {
        let (query, finding) = match row {
            Row::Username(name) | Row::UsernameCheckbox(name) => {
                // Desktop's `usernames.hash` throws for one that can't be a
                // username (`alice.007`), which it reports as not valid.
                if !is_username_valid(name) {
                    let text = format!(
                        "{name} is not a valid username. Make sure you\u{2019}ve entered the complete username followed by its set of digits."
                    );
                    window.open_alert_dialog(cx, move |alert, _, _| alert.description(text.clone()));
                    return;
                }
                (name.clone(), Finding::Username(name.clone()))
            }
            Row::Phone(phone, typed) | Row::PhoneCheckbox(phone, typed) => {
                if !phone.valid {
                    let text = format!("User not found. \u{201c}{typed}\u{201d} is not a valid phone number.");
                    window.open_alert_dialog(cx, move |alert, _, _| alert.description(text.clone()));
                    return;
                }
                (phone.e164.clone(), Finding::Phone(typed.clone()))
            }
            _ => return,
        };
        if self.finding.as_ref().is_some_and(|(q, _)| *q == query) {
            return;
        }
        self.state.read(cx).send(signal_backend::Command::FindRecipient { query: query.clone() });
        self.finding = Some((query, finding));
        cx.notify();
    }

    /// The answer to a lookup of ours: its chat opens and the composer
    /// closes, as picking a contact does, or Desktop's dialog or message
    /// says why not. (Desktop's code calls `showConversation` without
    /// `switchToAssociatedView` here, which reads as leaving the composer
    /// open, but the running app goes back to the chat list.)
    fn found(&mut self, query: &str, result: &Result<DirectoryEntry, FindError>, window: &mut Window, cx: &mut Context<Self>) {
        let Some((_, finding)) = self.finding.take_if(|(q, _)| q == query) else { return };
        cx.notify();
        let error = match result {
            // Choose members: the person found is picked (Desktop toggles
            // them into the selection).
            Ok(entry) if self.step == Step::ChooseMembers => {
                let Some(sid) = entry.id.as_direct().map(str::to_owned) else { return };
                if let Some(group) = &mut self.group {
                    group.found_names.insert(sid.clone(), entry.title.clone());
                }
                self.toggle_member(&sid, window, cx);
                return;
            }
            Ok(entry) => {
                // The Find panes go back to the inbox (Desktop's `showInbox`
                // after `showConversation`), even for an archived chat.
                // One event: the sidebar drops the composer at the first.
                if self.step == Step::NewChat {
                    self.open(entry.id.clone(), Some(entry.clone()), cx);
                } else {
                    self.open_into(entry.id.clone(), Some(entry.clone()), false, cx);
                }
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
                    .on_click(cx.listener(|this, _, window, cx| this.show_choose_members(window, cx)))
                    .into_any_element()
            }
            Row::FindByUsername | Row::FindByPhoneNumber => {
                let (icon, label, selector, step) = match &row {
                    Row::FindByUsername => (IconName::AtSign, "Find by username", "compose-find-by-username", Step::FindByUsername),
                    _ => (IconName::Hash, "Find by phone number", "compose-find-by-phone-number", Step::FindByPhoneNumber),
                };
                tile(SharedString::from(selector), SELECT_HEIGHT, step_icon(icon, palette), div().child(label).into_any_element(), None, Some(selector.into()), palette)
                    .on_click(cx.listener(move |this, _, window, cx| this.show_find(step, window, cx)))
                    .into_any_element()
            }
            Row::Contact(index) => {
                let Some(contact) = self.candidates.get(*index).map(|c| c.contact.clone()) else { return div().into_any_element() };
                let avatar = contact_avatar(&self.state, &contact, 32., cx);
                let id = SharedString::from(format!("compose-contact-{}", contact.service_id));
                let selector = format!("compose-contact-{}", contact.title);
                let index = *index;
                tile(id, SELECT_HEIGHT, avatar, contact_name(&contact, palette), contact_about(&contact), Some(selector), palette)
                    .on_click(cx.listener(move |this, _, _, cx| this.open_contact(index, cx)))
                    .into_any_element()
            }
            // Desktop's `ContactCheckbox`: the contact row with a round
            // checkbox, which can't be ticked once the group is full.
            Row::Checkbox(index) => {
                let Some(contact) = self.candidates.get(*index).map(|c| c.contact.clone()) else { return div().into_any_element() };
                let hard = Limits::now(self.state.read(cx)).hard;
                let (on, full) = self.group.as_ref().map_or((false, false), |g| (g.chosen.contains(&contact.service_id), g.chosen.len() + 1 >= hard));
                let disabled = !on && full;
                let avatar = contact_avatar(&self.state, &contact, 32., cx);
                let id = SharedString::from(format!("compose-member-{}", contact.service_id));
                let selector = format!("compose-member-{}", contact.title);
                let sid = contact.service_id.clone();
                tile_with(id, SELECT_HEIGHT, avatar, contact_name(&contact, palette), contact_about(&contact), Some(selector), Some(checkbox(on, palette)), !disabled, palette)
                    .when(disabled, |d| d.opacity(0.5))
                    .when(!disabled, |d| d.on_click(cx.listener(move |this, _, window, cx| this.toggle_member(&sid, window, cx))))
                    .into_any_element()
            }
            // Desktop's `UsernameCheckbox` and `PhoneNumberCheckbox`: looked
            // up when clicked, a spinner instead of the checkbox meanwhile.
            Row::UsernameCheckbox(name) | Row::PhoneCheckbox(_, name) => {
                let fetching = self.finding.as_ref().is_some_and(|(query, _)| match &row {
                    Row::UsernameCheckbox(name) => query == name,
                    Row::PhoneCheckbox(phone, _) => *query == phone.e164,
                    _ => false,
                });
                let selector = match &row {
                    Row::UsernameCheckbox(_) => "compose-username-checkbox",
                    _ => "compose-phone-checkbox",
                };
                let trailing = if fetching {
                    Icon::new(IconName::LoaderCircle).size(px(20.)).text_color(palette.text_secondary).into_any_element()
                } else {
                    checkbox(false, palette)
                };
                let title = div().truncate().child(name.clone()).into_any_element();
                tile_with(SharedString::from(selector), SELECT_HEIGHT, search_glyph(palette), title, None, Some(selector.to_owned()), Some(trailing), true, palette)
                    .on_click(cx.listener(move |this, _, window, cx| this.look_up(&row, window, cx)))
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
                let avatar = search_glyph(palette);
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

/// Desktop's `Avatar` with `searchResult`: the search glyph in a circle.
fn search_glyph(palette: Palette) -> gpui_kit::AnyElement {
    div()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(px(32.))
        .rounded_full()
        .bg(palette.row_hover)
        .child(Icon::new(IconName::Search).size(px(16.)).text_color(palette.text))
        .into_any_element()
}

/// A contact's avatar: Note to Self's icon, or their photo with its badge.
fn contact_avatar(state: &Entity<AppState>, contact: &ComposeContact, size: f32, cx: &App) -> gpui_kit::AnyElement {
    if contact.me {
        return crate::views::common::chat_placeholder(ConversationKind::NoteToSelf, &contact.title, 0, px(size));
    }
    crate::views::common::badged_avatar(state.read(cx), &contact.service_id, &contact.title, size, cx)
}

/// Desktop's `ContactName`: the name, Note to Self's check, or the
/// address-book emblem.
fn contact_name(contact: &ComposeContact, palette: Palette) -> gpui_kit::AnyElement {
    div()
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
        })
        .into_any_element()
}

/// The About line under a contact (not Note to Self's), with its emoji.
fn contact_about(contact: &ComposeContact) -> Option<String> {
    (!contact.me && !contact.about.is_empty()).then(|| match contact.about_emoji.as_str() {
        "" => contact.about.clone(),
        emoji => format!("{emoji} {}", contact.about),
    })
}

/// Desktop's `ListTile.checkbox`: a round box, filled with a check when on.
fn checkbox(on: bool, palette: Palette) -> gpui_kit::AnyElement {
    div()
        .flex()
        .items_center()
        .justify_center()
        .size(px(20.))
        .rounded_full()
        .border_2()
        .border_color(if on { palette.accent } else { palette.text_secondary })
        .when(on, |d| d.bg(palette.accent).child(Icon::new(IconName::Check).size(px(14.)).text_color(gpui_kit::white())))
        .into_any_element()
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
    tile_with(id, height, leading, title, subtitle, selector, None, true, palette)
}

/// [`tile`] with a trailing element (a checkbox, a spinner), and one that
/// isn't clicked (`isClickable: false`: no pointer or hover).
#[allow(clippy::too_many_arguments)]
fn tile_with(
    id: SharedString,
    height: f32,
    leading: gpui_kit::AnyElement,
    title: gpui_kit::AnyElement,
    subtitle: Option<String>,
    selector: Option<String>,
    trailing: Option<gpui_kit::AnyElement>,
    clickable: bool,
    palette: Palette,
) -> gpui_kit::Stateful<gpui_kit::Div> {
    let inner = div()
        .flex_1()
        .min_w_0()
        .h_full()
        .flex()
        .items_center()
        .px(px(14.))
        .py(px(6.))
        .rounded(px(12.))
        .when(clickable, |d| d.cursor_pointer().hover(|d| d.bg(palette.row_hover)))
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
        )
        .when_some(trailing, |d, trailing| d.child(div().flex_none().ml(px(12.)).child(trailing)));
    div()
        .id(id)
        .when_some(selector, |d, s| d.debug_selector(move || s))
        .w_full()
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
        let step = self.step;
        let busy = self.finding.is_some() || self.creating();
        // Desktop's Find panes reuse "Back to member selection" for theirs.
        let (title, back_label) = match step {
            Step::NewChat => ("New chat", "Back to inbox"),
            Step::FindByUsername => ("Find by username", "Back to member selection"),
            Step::FindByPhoneNumber => ("Find by phone number", "Back to member selection"),
            Step::ChooseMembers => ("Choose members", "Back"),
            Step::SetGroupMetadata => ("Name this group", "Back to member selection"),
        };
        let header = div()
            .flex()
            .items_center()
            .py(px(15.))
            .pl(px(12.))
            .child(
                Button::new("compose-back")
                    .ghost()
                    .small()
                    .icon(IconName::ChevronLeft)
                    .tooltip(back_label)
                    .disabled(step != Step::NewChat && busy)
                    .on_click(cx.listener(|this, _, window, cx| match this.step {
                        Step::NewChat => cx.emit(ComposeEvent::Close),
                        _ => this.back(window, cx),
                    })),
            )
            .child(div().flex_1().pr(px(40.)).text_center().text_size(px(14.)).font_weight(FontWeight::SEMIBOLD).child(title));
        let list_or_empty = |empty_text: &'static str, selector: &'static str| {
            if empty {
                div().debug_selector(move || selector.into()).mt(px(27.)).px_4().w_full().text_center().child(empty_text).into_any_element()
            } else {
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .child(list(self.list.clone(), move |ix, _, cx| entity.update(cx, |this, cx| this.render_row(ix, cx))).size_full())
                    .child(Scrollbar::vertical(&self.list).mode(ScrollbarMode::Hover))
                    .into_any_element()
            }
        };
        let search = div()
            .px(px(16.))
            .pb(px(8.))
            .child(Input::new(&self.search).prefix(Icon::new(IconName::Search).size_4().text_color(palette.text_secondary)));
        let body = match step {
            Step::NewChat => div().flex_1().min_h_0().flex().flex_col().child(search).child(list_or_empty("No chats found", "compose-no-chats")).into_any_element(),
            Step::FindByUsername | Step::FindByPhoneNumber => self.render_find(palette, cx),
            Step::ChooseMembers => {
                let list = list_or_empty("No contacts found", "compose-no-contacts");
                self.render_choose_members(search.into_any_element(), list, palette, cx)
            }
            Step::SetGroupMetadata => self.render_metadata(palette, cx),
        };
        div()
            .id("compose")
            .debug_selector(|| "compose".into())
            .relative()
            .size_full()
            .flex()
            .flex_col()
            // Escape goes back (Desktop's `onEscapeShortcut`): to the inbox
            // from New chat, to New chat from a Find pane or Choose members,
            // to Choose members from Name this group (not while it looks
            // something up or creates the group).
            .capture_key_down(cx.listener(|this, event: &gpui_kit::KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" {
                    cx.stop_propagation();
                    match this.step {
                        Step::NewChat => cx.emit(ComposeEvent::Close),
                        _ => this.back(window, cx),
                    }
                }
            }))
            .child(header)
            .child(body)
    }
}

impl Compose {
    /// Desktop's `LeftPaneChooseGroupMembersHelper`: the search, the picks
    /// as pills, the list, and Next (or Skip with no one picked).
    fn render_choose_members(&mut self, search: gpui_kit::AnyElement, list: gpui_kit::AnyElement, palette: Palette, cx: &mut Context<Self>) -> gpui_kit::AnyElement {
        let order = self.group.as_ref().map(|g| g.order.clone()).unwrap_or_default();
        let pills = order.iter().map(|sid| {
            let (title, first) = self.member_names(sid);
            let avatar = crate::views::common::person_photo(self.state.read(cx), sid, self.state.read(cx).photo_of(sid), &title, crate::theme::color_for(sid), px(20.));
            let remove = sid.clone();
            let selector = format!("compose-pill-{title}");
            div()
                .debug_selector(move || selector)
                .flex()
                .items_center()
                .px(px(8.))
                .py(px(4.))
                .rounded(px(6.))
                .bg(palette.row_hover)
                .max_w_full()
                .child(avatar)
                .child(div().px(px(6.)).text_size(px(13.)).line_height(px(18.)).truncate().child(first))
                .child(
                    Button::new(SharedString::from(format!("compose-pill-remove-{sid}")))
                        .ghost()
                        .xsmall()
                        .icon(IconName::X)
                        .tooltip("Remove contact")
                        .on_click(cx.listener(move |this, _, window, cx| this.toggle_member(&remove, window, cx))),
                )
        });
        let picked = !order.is_empty();
        let state = self.state.clone();
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(search)
            // Vernam's: Desktop joins a group only from its link, which
            // Vernam can take pasted here (the old New group dialog had it).
            .child(
                div().px(px(16.)).pb(px(8.)).child(
                    Button::new("compose-join-link")
                        .label("Join with a group link")
                        .ghost()
                        .xsmall()
                        .text_color(palette.accent)
                        .on_click(cx.listener(move |_, _, window, cx| {
                            crate::views::group_admin::open_paste_link(state.clone(), window, cx);
                            cx.emit(ComposeEvent::Handoff);
                        })),
                ),
            )
            .when(picked, |d| {
                d.child(
                    div()
                        .id("compose-pills")
                        .flex()
                        .flex_wrap()
                        .gap_x(px(12.))
                        .gap_y(px(8.))
                        .px(px(16.))
                        .py(px(4.))
                        .mb(px(10.))
                        .max_h(px(88.))
                        .overflow_y_scroll()
                        .children(pills),
                )
            })
            .child(list)
            .child(self.footer(if picked { "Next" } else { "Skip" }, false, true, cx.listener(|this, _, window, cx| this.show_set_metadata(window, cx))))
            .into_any_element()
    }

    /// Desktop's `LeftPaneSetGroupMetadataHelper`: the group's photo, name
    /// and disappearing message timer, who's in it, and Create.
    fn render_metadata(&mut self, palette: Palette, cx: &mut Context<Self>) -> gpui_kit::AnyElement {
        use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
        let Some(group) = &self.group else { return div().into_any_element() };
        let creating = group.creating;
        let ready = !creating && !group.name.read(cx).value().trim().is_empty();
        let timer = group.timer;
        let photo = group.avatar.as_ref().map(|(path, _)| path.clone());
        let name = group.name.clone();
        let title = name.read(cx).value().to_string();
        let order = group.order.clone();
        // Desktop's `AvatarPreview`: 96px, the upload button on it.
        let preview = div()
            .id("compose-group-photo")
            .debug_selector(|| "compose-group-photo".into())
            .relative()
            .size(px(96.))
            .when(!creating, |d| d.cursor_pointer().on_click(cx.listener(|this, _, window, cx| this.pick_photo(window, cx))))
            .child(crate::views::common::group_photo(photo.as_deref(), &title, 0, px(96.)))
            .child(
                div()
                    .absolute()
                    .bottom_0()
                    .right_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .size(px(32.))
                    .rounded_full()
                    .bg(palette.background)
                    .border_1()
                    .border_color(palette.panel_border)
                    .child(Icon::new(IconName::Camera).size(px(16.)).text_color(palette.text)),
            );
        let timer_button = Button::new("compose-group-timer")
            .label(crate::views::chat_menu::format_timer_long(timer))
            .outline()
            .small()
            .disabled(creating)
            .dropdown_menu({
                let this = cx.entity();
                move |mut menu, _, _| {
                    for seconds in crate::views::chat_menu::TIMER_CHOICES {
                        let this = this.clone();
                        menu = menu.item(PopupMenuItem::new(crate::views::chat_menu::format_timer_long(seconds)).checked(timer == seconds).on_click(
                            move |_, _, cx| {
                                this.update(cx, |this, cx| {
                                    if let Some(group) = &mut this.group {
                                        group.timer = seconds;
                                        cx.notify();
                                    }
                                });
                            },
                        ));
                    }
                    menu
                }
            });
        let members = order.iter().map(|sid| {
            let (title, _) = self.member_names(sid);
            let avatar = crate::views::common::badged_avatar(self.state.read(cx), sid, &title, 32., cx);
            let contact = self.candidates.iter().map(|c| &c.contact).find(|c| &c.service_id == sid);
            let name = match contact {
                Some(contact) => contact_name(contact, palette),
                None => div().truncate().child(title.clone()).into_any_element(),
            };
            let about = contact.and_then(contact_about);
            tile_with(SharedString::from(format!("compose-group-member-{sid}")), SELECT_HEIGHT, avatar, name, about, Some(format!("compose-group-member-{title}")), None, false, palette)
        });
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(
                div()
                    .id("compose-metadata")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .child(div().flex().justify_center().child(preview))
                    .child(div().m(px(16.)).child(Input::new(&name).disabled(creating)))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .mx(px(16.))
                            .mb(px(16.))
                            .child(div().mr(px(12.)).child("Disappearing messages"))
                            .child(timer_button),
                    )
                    .when(!order.is_empty(), |d| {
                        d.child(
                            div()
                                .h(px(HEADER_HEIGHT))
                                .flex()
                                .items_end()
                                .pb(px(8.))
                                .pl(px(16.))
                                .text_size(px(14.))
                                .font_weight(FontWeight::SEMIBOLD)
                                .child("Members"),
                        )
                        .children(members)
                    }),
            )
            .child(self.footer("Create", creating, ready, cx.listener(|this, _, _, cx| this.create(cx))))
            .into_any_element()
    }

    /// A pane's button at the bottom right (Desktop's `getFooterContents`).
    fn footer(
        &self,
        label: &'static str,
        loading: bool,
        enabled: bool,
        on_click: impl Fn(&gpui_kit::ClickEvent, &mut Window, &mut App) + 'static,
    ) -> gpui_kit::AnyElement {
        div()
            .flex()
            .justify_end()
            .p(px(12.))
            .child(Button::new("compose-next").primary().label(label).loading(loading).disabled(!enabled || loading).on_click(on_click))
            .into_any_element()
    }
}

impl Compose {
    /// A Find pane's field (and for a number, the country above it) and
    /// its Next button at the bottom right (Desktop's `getFooterContents`).
    fn render_find(&mut self, palette: Palette, cx: &mut Context<Self>) -> gpui_kit::AnyElement {
        let Some(find) = &self.find else { return div().into_any_element() };
        let busy = self.finding.is_some();
        let ready = !busy && self.next_lookup(cx).is_some();
        let field = Input::new(&find.input).disabled(busy);
        let body = match self.step {
            Step::FindByUsername => div()
                .flex()
                .flex_col()
                .mt(px(8.))
                .px(px(16.))
                .child(field)
                .child(
                    div()
                        .mt(px(12.))
                        .text_size(px(12.))
                        .line_height(px(16.))
                        .text_color(palette.text_secondary)
                        .child("Enter a username followed by a dot and its set of numbers."),
                ),
            _ => {
                // Desktop's `CountryCodeSelect`: the country picked or
                // "Country code", its calling code or the account's.
                let picked = find.region.as_deref().and_then(signal_backend::countries::country);
                let code = picked
                    .as_ref()
                    .map(|c| c.code.clone())
                    .or_else(|| self.home_region(cx).and_then(|r| signal_backend::countries::calling_code(&r)).map(|c| format!("+{c}")))
                    .unwrap_or_default();
                let name = picked.as_ref().map_or("Country code", |c| c.name);
                let select = div()
                    .id("compose-country")
                    .debug_selector(|| "compose-country".into())
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .h(px(28.))
                    .px(px(12.))
                    .rounded(px(8.))
                    .bg(palette.row_hover)
                    .text_size(px(13.))
                    .line_height(px(18.))
                    .when(!busy, |d| d.cursor_pointer().on_click(cx.listener(|this, _, window, cx| this.open_country_picker(window, cx))))
                    .child(div().flex_1().truncate().child(name))
                    .child(div().child(code))
                    .child(Icon::new(IconName::ChevronDown).size(px(16.)).text_color(palette.text_secondary));
                div().flex().flex_col().gap(px(12.)).mt(px(8.)).px(px(16.)).child(select).child(field)
            }
        };
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(body)
            .child(div().flex_1())
            .child(
                div().flex().justify_end().p(px(12.)).child(
                    Button::new("compose-next")
                        .primary()
                        .label("Next")
                        .loading(busy)
                        .disabled(!ready)
                        .on_click(cx.listener(|this, _, window, cx| this.next(window, cx))),
                ),
            )
            .into_any_element()
    }
}

/// The countries Desktop's search would keep for `term`, as indexes.
fn country_matches(countries: &[signal_backend::countries::Country], term: &str) -> Vec<usize> {
    // Words with single spaces, so "antigua barbuda" finds "Antigua &
    // Barbuda" (its "&" a space).
    let words = |text: &str| searchable(text).split_whitespace().collect::<Vec<_>>().join(" ");
    let term = words(term);
    let digits: String = term.chars().filter(char::is_ascii_digit).collect();
    // Nothing to match by (empty, or only "+"): every country.
    let all = term.trim_start_matches('+').trim().is_empty();
    countries
        .iter()
        .enumerate()
        .filter(|(_, c)| all || words(c.name).contains(&term) || (!digits.is_empty() && c.code[1..].starts_with(&digits)))
        .map(|(ix, _)| ix)
        .collect()
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
fn filter_and_sort<'a>(entries: impl Iterator<Item = Entry<'a>>, term: &str, e164: Option<&str>, now: i64) -> Vec<usize> {
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
                    let stored = e.phone?;
                    // The term read as a number (a local "07911 123456" is
                    // "+447911123456"), or its digits inside the number.
                    if e164 == Some(stored) {
                        return Some(0.);
                    }
                    let phone: String = stored.chars().filter(char::is_ascii_digit).collect();
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
                Row::FindByUsername => "[Find by username]".to_owned(),
                Row::FindByPhoneNumber => "[Find by phone number]".to_owned(),
                Row::Header(text) => format!("# {text}"),
                Row::Contact(ix) => self.candidates[*ix].contact.title.clone(),
                Row::Group(id) => self.state.read(cx).conversation(id).map_or_else(String::new, |g| g.title.clone()),
                Row::Username(name) => format!("@ {name}"),
                Row::Phone(phone, _) => format!("tel {}{}", phone.e164, if phone.valid { "" } else { " (invalid)" }),
                Row::Checkbox(ix) => {
                    let contact = &self.candidates[*ix].contact;
                    let on = self.group.as_ref().is_some_and(|g| g.chosen.contains(&contact.service_id));
                    format!("[{}] {}", if on { "x" } else { " " }, contact.title)
                }
                Row::UsernameCheckbox(name) => format!("[ ] @ {name}"),
                Row::PhoneCheckbox(phone, _) => format!("[ ] tel {}", phone.e164),
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
                None,
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
        assert_eq!(filter_and_sort([dana].into_iter(), "415 555", None, 0), [0]);
        // A local number, read with the account's region.
        let uk = Entry { title: "Dana Brooks", names: &names, phone: Some("+447911123456"), active_at: 0, archived: false };
        assert_eq!(filter_and_sort([uk].into_iter(), "07911 123456", Some("+447911123456"), 0), [0]);
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
                c.rebuild(false, cx);
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
            m.sidebar.update_in(&mut m.cx, |s, window, cx| s.search_input(cx).update(cx, |i, cx| i.set_value("hike", window, cx)));
            m.cx.run_until_parked();
            open(&mut m);
            let rows = rows(&mut m);
            assert_eq!(rows[..4], ["[New group]", "[Find by username]", "[Find by phone number]", "# Contacts"].map(String::from));
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
            let query = m.sidebar.read_with(&m.cx, |s, cx| s.search_input(cx).read(cx).value().to_string());
            assert_eq!(query, "hike");
            m.backend.shutdown();
        }

        fn step(m: &mut Mounted) -> Step {
            compose(m).read_with(&m.cx, |c, _| c.step)
        }

        fn find_text(m: &mut Mounted) -> String {
            compose(m).read_with(&m.cx, |c, cx| c.find.as_ref().map(|f| f.input.read(cx).value().to_string()).unwrap_or_default())
        }

        fn next_ready(m: &mut Mounted) -> bool {
            compose(m).read_with(&m.cx, |c, cx| c.next_lookup(cx).is_some())
        }

        fn show(m: &mut Mounted, step: Step) {
            let compose = compose(m);
            compose.update_in(&mut m.cx, |c, window, cx| c.show_find(step, window, cx));
            m.cx.run_until_parked();
        }

        #[gpui_kit::test]
        fn the_find_panes_open_from_new_chat_and_go_back_to_a_fresh_one(cx: &mut TestAppContext) {
            let mut m = mount(cx);
            open(&mut m);
            // Their buttons go while anything is typed.
            type_in(&mut m, " ");
            assert!(!rows(&mut m).iter().any(|r| r.starts_with("[Find")));
            show(&mut m, Step::FindByUsername);
            assert_eq!(step(&mut m), Step::FindByUsername);
            // The pane's field is the one the sidebar focuses.
            let field = compose(&mut m).read_with(&m.cx, |c, _| c.search_input());
            assert_eq!(m.sidebar.read_with(&m.cx, |s, cx| s.search_input(cx)), field);
            // Escape: New chat again, its search empty, the buttons back.
            m.cx.simulate_keystrokes("escape");
            m.cx.run_until_parked();
            assert_eq!(step(&mut m), Step::NewChat);
            assert!(m.sidebar.read_with(&m.cx, |s, _| s.compose().is_some()));
            assert_eq!(compose(&mut m).read_with(&m.cx, |c, _| c.query.clone()), "");
            assert!(rows(&mut m).contains(&"[Find by phone number]".to_owned()));
            // ⌘N from a Find pane does the same.
            show(&mut m, Step::FindByPhoneNumber);
            m.sidebar.update_in(&mut m.cx, |s, window, cx| s.open_compose(window, cx));
            assert_eq!(step(&mut m), Step::NewChat);
            m.backend.shutdown();
        }

        fn new_group(m: &mut Mounted) {
            let compose = compose(m);
            compose.update_in(&mut m.cx, |c, window, cx| c.show_choose_members(window, cx));
            m.cx.run_until_parked();
        }

        fn pick(m: &mut Mounted, title: &str) {
            let compose = compose(m);
            compose.update_in(&mut m.cx, |c, window, cx| {
                let sid = c.candidates.iter().find(|c| c.contact.title == title).unwrap().contact.service_id.clone();
                c.toggle_member(&sid, window, cx);
            });
            m.cx.run_until_parked();
        }

        fn picked(m: &mut Mounted) -> Vec<String> {
            compose(m).read_with(&m.cx, |c, _| c.group.as_ref().map(|g| g.order.iter().map(|sid| c.member_names(sid).0).collect()).unwrap_or_default())
        }

        fn footer_label(m: &mut Mounted) -> &'static str {
            compose(m).read_with(&m.cx, |c, _| if c.group.as_ref().is_some_and(|g| !g.order.is_empty()) { "Next" } else { "Skip" })
        }

        #[gpui_kit::test]
        fn choosing_members_picks_in_order_and_back_drops_them(cx: &mut TestAppContext) {
            let mut m = mount(cx);
            open(&mut m);
            new_group(&mut m);
            assert_eq!(step(&mut m), Step::ChooseMembers);
            // Everyone New chat lists but Note to Self, with checkboxes.
            let listed = rows(&mut m);
            assert_eq!(listed[0], "# Contacts");
            assert!(listed.contains(&"[ ] Maya Chen".to_owned()));
            assert!(!listed.iter().any(|r| r.contains("Note to Self") || r.contains("[New group]")));
            assert_eq!(footer_label(&mut m), "Skip");
            // Picked in order, shown as pills in that order.
            pick(&mut m, "Jordan Reyes");
            pick(&mut m, "Maya Chen");
            assert_eq!(picked(&mut m), ["Jordan Reyes", "Maya Chen"]);
            assert!(rows(&mut m).contains(&"[x] Maya Chen".to_owned()));
            assert!(m.cx.debug_bounds("compose-pill-Jordan Reyes").is_some());
            assert_eq!(footer_label(&mut m), "Next");
            // Unpicking, as the pill's x does.
            pick(&mut m, "Jordan Reyes");
            assert_eq!(picked(&mut m), ["Maya Chen"]);
            // Search narrows the list, and offers a username like it to
            // look up, as Desktop does.
            type_in(&mut m, "maya");
            assert_eq!(rows(&mut m), ["# Contacts", "[x] Maya Chen", "# Find by username", "[ ] @ maya"]);
            // Too short for a username, and no number: nothing.
            type_in(&mut m, "zz");
            assert!(m.cx.debug_bounds("compose-no-contacts").is_some());
            // Escape: New chat again, the picks dropped.
            m.cx.simulate_keystrokes("escape");
            m.cx.run_until_parked();
            assert_eq!(step(&mut m), Step::NewChat);
            new_group(&mut m);
            assert!(picked(&mut m).is_empty());
            m.backend.shutdown();
        }

        #[gpui_kit::test]
        fn a_number_looked_up_while_choosing_is_picked(cx: &mut TestAppContext) {
            let mut m = mount(cx);
            open(&mut m);
            new_group(&mut m);
            // Dana is in the demo's directory but not listed.
            type_in(&mut m, "+1 415 555 0177");
            assert_eq!(rows(&mut m), ["# Find by phone number", "[ ] tel +14155550177"]);
            let compose = compose(&mut m);
            compose.update_in(&mut m.cx, |c, window, cx| {
                let row = c.rows.iter().find(|r| matches!(r, Row::PhoneCheckbox(..))).cloned().unwrap();
                c.look_up(&row, window, cx);
            });
            wait(&mut m.cx, |cx| compose.read_with(cx, |c, _| c.group.as_ref().is_some_and(|g| !g.order.is_empty())));
            assert_eq!(picked(&mut m), ["Dana Brooks"]);
            // Still choosing: nothing opened.
            assert_eq!(step(&mut m), Step::ChooseMembers);
            m.backend.shutdown();
        }

        #[gpui_kit::test]
        fn naming_the_group_keeps_the_picks_and_enter_creates_it(cx: &mut TestAppContext) {
            let mut m = mount(cx);
            open(&mut m);
            new_group(&mut m);
            pick(&mut m, "Maya Chen");
            pick(&mut m, "Ana Souza");
            let compose = compose(&mut m);
            compose.update_in(&mut m.cx, |c, window, cx| c.show_set_metadata(window, cx));
            m.cx.run_until_parked();
            assert_eq!(step(&mut m), Step::SetGroupMetadata);
            assert!(m.cx.debug_bounds("compose-group-member-Maya Chen").is_some());
            // Back to Choose members keeps the picks.
            m.cx.simulate_keystrokes("escape");
            m.cx.run_until_parked();
            assert_eq!(step(&mut m), Step::ChooseMembers);
            assert_eq!(picked(&mut m), ["Maya Chen", "Ana Souza"]);
            compose.update_in(&mut m.cx, |c, window, cx| c.show_set_metadata(window, cx));
            m.cx.run_until_parked();
            // A blank name doesn't create.
            m.cx.simulate_keystrokes("enter");
            assert!(!compose.read_with(&m.cx, |c, _| c.creating()));
            // At most 32 characters.
            m.cx.simulate_input(&"x".repeat(40));
            let name = compose.read_with(&m.cx, |c, cx| c.group.as_ref().unwrap().name.read(cx).value().to_string());
            assert_eq!(name.chars().count(), GROUP_NAME_MAX);
            let name = compose.read_with(&m.cx, |c, _| c.group.as_ref().unwrap().name.clone());
            m.cx.update(|window, cx| name.update(cx, |n, cx| n.set_value("", window, cx)));
            m.cx.simulate_input("Ridge runners");
            // Enter creates it; it opens and the composer closes.
            m.cx.simulate_keystrokes("enter");
            let state = m.state.clone();
            wait(&mut m.cx, |cx| state.read_with(cx, |s, _| s.selected.as_ref().and_then(|id| s.conversation(id)).is_some_and(|c| c.title == "Ridge runners")));
            wait(&mut m.cx, |cx| m.sidebar.read_with(cx, |s, _| s.compose().is_none()));
            let members = m.state.read_with(&m.cx, |s, _| s.selected.as_ref().and_then(|id| s.conversation(id)).map(|c| c.members.len()));
            assert_eq!(members, Some(2));
            m.backend.shutdown();
        }

        #[gpui_kit::test]
        fn a_failed_create_lets_it_be_tried_again(cx: &mut TestAppContext) {
            let mut m = mount(cx);
            open(&mut m);
            new_group(&mut m);
            let compose = compose(&mut m);
            compose.update_in(&mut m.cx, |c, window, cx| {
                c.show_set_metadata(window, cx);
                if let Some(g) = &mut c.group {
                    g.creating = true;
                }
            });
            // Creating: back and ⌘N wait for the answer.
            m.cx.simulate_keystrokes("escape");
            m.sidebar.update_in(&mut m.cx, |s, window, cx| s.open_compose(window, cx));
            assert_eq!(step(&mut m), Step::SetGroupMetadata);
            m.state.update(&mut m.cx, |_, cx| cx.emit(AppEvent::GroupCreateFailed));
            m.cx.run_until_parked();
            assert!(!compose.read_with(&m.cx, |c, _| c.creating()));
            assert!(m.sidebar.read_with(&m.cx, |s, _| s.compose().is_some()), "still open to try again");
            m.backend.shutdown();
        }

        #[gpui_kit::test]
        fn a_find_pane_goes_to_the_inbox_even_for_an_archived_chat(cx: &mut TestAppContext) {
            let mut m = mount(cx);
            open(&mut m);
            // A chat that's archived.
            let chat = m.state.update(&mut m.cx, |s, _| {
                let chat = s.conversations.remove(0);
                s.archived.push(chat.clone());
                chat
            });
            show(&mut m, Step::FindByUsername);
            let compose = compose(&mut m);
            let opened = Rc::new(std::cell::RefCell::new(Vec::new()));
            let seen = opened.clone();
            let _watch = m.cx.update(|_, cx| cx.subscribe(&compose, move |_, event: &ComposeEvent, _| {
                if let ComposeEvent::Opened { archived } = event {
                    seen.borrow_mut().push(*archived);
                }
            }));
            let entry = DirectoryEntry { id: chat.id.clone(), kind: chat.kind, title: chat.title.clone(), phone: None };
            compose.update_in(&mut m.cx, |c, window, cx| {
                c.finding = Some(("someone.42".into(), Finding::Username("someone.42".into())));
                c.found("someone.42", &Ok(entry), window, cx);
            });
            m.cx.run_until_parked();
            // One event, for the inbox (Desktop's `showInbox`).
            assert_eq!(*opened.borrow(), [false]);
            m.backend.shutdown();
        }

        #[gpui_kit::test]
        fn a_lookup_keeps_the_find_panes_and_new_chat_where_they_are(cx: &mut TestAppContext) {
            let mut m = mount(cx);
            open(&mut m);
            // New chat's own lookup running: a Find pane doesn't open over it.
            let compose = compose(&mut m);
            compose.update(&mut m.cx, |c, _| c.finding = Some(("nobody.99".into(), Finding::Username("nobody.99".into()))));
            show(&mut m, Step::FindByPhoneNumber);
            assert_eq!(step(&mut m), Step::NewChat);
            // A Find pane's lookup running: ⌘N doesn't drop it.
            compose.update(&mut m.cx, |c, _| c.finding = None);
            show(&mut m, Step::FindByUsername);
            compose.update(&mut m.cx, |c, _| c.finding = Some(("nobody.99".into(), Finding::Username("nobody.99".into()))));
            m.sidebar.update_in(&mut m.cx, |s, window, cx| s.open_compose(window, cx));
            assert_eq!(step(&mut m), Step::FindByUsername);
            assert!(compose.read_with(&m.cx, |c, _| c.finding.is_some()));
            m.backend.shutdown();
        }

        #[gpui_kit::test]
        fn find_by_username_looks_up_what_was_typed(cx: &mut TestAppContext) {
            let mut m = mount(cx);
            open(&mut m);
            show(&mut m, Step::FindByUsername);
            assert!(!next_ready(&mut m), "nothing typed");
            m.cx.simulate_input("x");
            assert!(!next_ready(&mut m), "too short for a username");
            m.cx.simulate_input("1");
            m.cx.simulate_keystrokes("backspace backspace");
            // Not anyone's: Desktop's dialog, and the pane stays.
            m.cx.simulate_input("nobody.99");
            assert!(next_ready(&mut m));
            m.cx.simulate_keystrokes("enter");
            wait(&mut m.cx, |cx| cx.update(|window, cx| window.has_active_dialog(cx)));
            assert_eq!(step(&mut m), Step::FindByUsername);
            m.cx.update(|window, cx| window.close_dialog(cx));
            // Someone's: their chat opens and the composer closes.
            let compose = compose(&mut m);
            compose.update_in(&mut m.cx, |c, window, cx| {
                let input = c.find.as_ref().unwrap().input.clone();
                input.update(cx, |i, cx| i.set_value("maya.42", window, cx));
                c.next(window, cx);
            });
            let state = m.state.clone();
            wait(&mut m.cx, |cx| state.read_with(cx, |s, _| s.selected.as_ref().and_then(|id| s.conversation(id)).is_some_and(|c| c.title == "Maya Chen")));
            assert!(m.sidebar.read_with(&m.cx, |s, _| s.compose().is_none()));
            m.backend.shutdown();
        }

        #[gpui_kit::test]
        fn find_by_phone_number_switches_country_and_looks_the_number_up(cx: &mut TestAppContext) {
            let mut m = mount(cx);
            open(&mut m);
            show(&mut m, Step::FindByPhoneNumber);
            // The demo's own number has no country to read a number by.
            m.cx.simulate_input("415 555 0177");
            assert!(!next_ready(&mut m), "no country to read it with");
            m.cx.simulate_keystrokes("cmd-a backspace");
            // A typed country code picks the country and leaves the rest.
            m.cx.simulate_input("+1 202 555 0187");
            let region = compose(&mut m).read_with(&m.cx, |c, _| c.find.as_ref().and_then(|f| f.region.clone()));
            assert_eq!(region.as_deref(), Some("US"));
            assert_eq!(find_text(&mut m), "2025550187");
            assert!(next_ready(&mut m));
            // No one has it: Desktop's dialog with the number as typed.
            m.cx.simulate_keystrokes("enter");
            wait(&mut m.cx, |cx| cx.update(|window, cx| window.has_active_dialog(cx)));
            m.cx.update(|window, cx| window.close_dialog(cx));
            // Dana's, in the directory: her chat opens, the composer closes.
            let compose = compose(&mut m);
            compose.update_in(&mut m.cx, |c, window, cx| {
                let input = c.find.as_ref().unwrap().input.clone();
                input.update(cx, |i, cx| i.set_value("415 555 0177", window, cx));
                c.next(window, cx);
            });
            let state = m.state.clone();
            wait(&mut m.cx, |cx| state.read_with(cx, |s, _| s.selected.as_ref().and_then(|id| s.conversation(id)).is_some_and(|c| c.title == "Dana Brooks")));
            assert!(m.sidebar.read_with(&m.cx, |s, _| s.compose().is_none()));
            m.backend.shutdown();
        }

        #[gpui_kit::test]
        fn the_country_dialog_lists_and_filters_countries_and_picks_one(cx: &mut TestAppContext) {
            let mut m = mount(cx);
            open(&mut m);
            show(&mut m, Step::FindByPhoneNumber);
            let compose = compose(&mut m);
            compose.update_in(&mut m.cx, |c, window, cx| c.open_country_picker(window, cx));
            m.cx.run_until_parked();
            assert!(m.cx.debug_bounds("country-AF").is_some(), "Afghanistan first");
            m.cx.simulate_input("united k");
            m.cx.run_until_parked();
            assert!(m.cx.debug_bounds("country-GB").is_some());
            assert!(m.cx.debug_bounds("country-AF").is_none());
            let gb = m.cx.debug_bounds("country-GB").unwrap();
            m.cx.simulate_click(gb.center(), gpui_kit::Modifiers::none());
            m.cx.run_until_parked();
            assert!(!m.cx.update(|window, cx| window.has_active_dialog(cx)));
            let region = compose.read_with(&m.cx, |c, _| c.find.as_ref().and_then(|f| f.region.clone()));
            assert_eq!(region.as_deref(), Some("GB"));
            // A London number now reads.
            m.cx.simulate_input("020 7946 0958");
            assert!(next_ready(&mut m));
            m.backend.shutdown();
        }

        #[test]
        fn countries_filter_by_name_or_code() {
            let all = signal_backend::countries::countries();
            let names = |term: &str| country_matches(&all, term).into_iter().map(|ix| all[ix].region).collect::<Vec<_>>();
            assert_eq!(names("").len(), all.len());
            assert!(names("Åland").contains(&"AX"));
            assert!(names("aland").contains(&"AX"), "without accents");
            assert!(names("44").contains(&"GB"));
            assert!(names("+44").contains(&"GB"));
            assert!(names("zzz").is_empty());
            // A "+" alone matches every code; runs of punctuation are one space.
            assert_eq!(names("+").len(), all.len());
            assert!(names("antigua barbuda").contains(&"AG"));
            assert!(names("congo kinshasa").contains(&"CD"));
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
            // New chat closes, as Desktop's does after a lookup.
            assert!(m.sidebar.read_with(&m.cx, |s, _| s.compose().is_none()));
            // No one has this one: Desktop's dialog.
            open(&mut m);
            let compose = self::compose(&mut m);
            compose.update_in(&mut m.cx, |c, window, cx| c.look_up(&Row::Username("nobody.99".into()), window, cx));
            wait(&mut m.cx, |cx| cx.update(|window, cx| window.has_active_dialog(cx)));
            assert!(compose.read_with(&m.cx, |c, _| c.finding.is_none()));
            m.backend.shutdown();
        }

        #[gpui_kit::test]
        fn a_username_that_cant_be_one_says_so_without_a_lookup(cx: &mut TestAppContext) {
            let mut m = mount(cx);
            open(&mut m);
            let compose = compose(&mut m);
            // Desktop's `usernames.hash` refuses a discriminator of "007".
            compose.update_in(&mut m.cx, |c, window, cx| c.look_up(&Row::Username("alice.007".into()), window, cx));
            m.cx.run_until_parked();
            assert!(m.cx.update(|window, cx| window.has_active_dialog(cx)));
            assert!(compose.read_with(&m.cx, |c, _| c.finding.is_none()), "nothing looked up");
            m.backend.shutdown();
        }

        #[gpui_kit::test]
        fn new_data_keeps_the_place_and_a_new_query_starts_at_the_top(cx: &mut TestAppContext) {
            let mut m = mount(cx);
            open(&mut m);
            let compose = compose(&mut m);
            let top = |m: &mut Mounted| compose.read_with(&m.cx, |c, _| c.list.logical_scroll_top().item_ix);
            // Short enough that the list scrolls (every row fits otherwise,
            // and the list keeps to the top).
            m.cx.simulate_resize(gpui_kit::size(px(400.), px(300.)));
            m.cx.run_until_parked();
            compose.update(&mut m.cx, |c, _| c.list.scroll_to(ListOffset { item_ix: 3, offset_in_item: px(0.) }));
            // A chat changing reorders the list (as a message does).
            compose.update(&mut m.cx, |c, cx| {
                let mut candidates: Vec<Candidate> = Rc::try_unwrap(std::mem::take(&mut c.candidates)).unwrap_or_else(|rc| {
                    rc.iter().map(|c| Candidate { contact: c.contact.clone(), names: c.names.clone() }).collect()
                });
                candidates.reverse();
                c.candidates = Rc::new(candidates);
                c.rebuild(true, cx);
            });
            m.cx.run_until_parked();
            assert_eq!(top(&mut m), 3);
            type_in(&mut m, "a");
            assert_eq!(top(&mut m), 0);
            m.backend.shutdown();
        }

        #[gpui_kit::test]
        fn searching_a_chat_closes_new_chat(cx: &mut TestAppContext) {
            let mut m = mount(cx);
            open(&mut m);
            let chat = m.state.read_with(&m.cx, |s, _| s.conversations[0].id.clone());
            m.sidebar.update_in(&mut m.cx, |s, window, cx| s.search_in_chat(chat, window, cx));
            m.cx.run_until_parked();
            assert!(m.sidebar.read_with(&m.cx, |s, _| s.compose().is_none()));
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
