//! Several Signal accounts in one app, which Signal Desktop doesn't do.
//! Each account is linked as its own device and has its own data directory
//! (`accounts/<id>/` in the app's), database key and backend, all running
//! at once; `accounts.json` lists them in the switcher's order. The window
//! shows one at a time. What's the app's rather than an account's stays at
//! the top of the data directory: the logs and the window's size and place.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use anyhow::{Context as _, Result};
use futures::StreamExt as _;
use gpui_kit::{App, AppContext as _, Context, Entity, Global};
use serde::{Deserialize, Serialize};
use signal_backend::{Backend, CallLines, Config};

use crate::former_name::{self, Former};
use crate::keys::{self, KeyItem, KeyStore};
use crate::state::{AppEvent, AppState};

/// How long removing an account waits for its backend to stop.
const STOP_WAIT: std::time::Duration = std::time::Duration::from_secs(60);
/// How long removing an account waits for its call to end.
const HANG_UP_WAIT: std::time::Duration = std::time::Duration::from_secs(2);

/// Where the accounts' directories are, in the app's.
const ACCOUNTS_DIR: &str = "accounts";
/// The list of accounts, in the app's data directory.
const REGISTRY: &str = "accounts.json";
/// The app's own files at the top of its data directory; anything else
/// there, alongside a database, is the one account's, from before there
/// were several.
const APP_FILES: &[&str] = &["logs", "window.json", "window.json.partial", ACCOUNTS_DIR, REGISTRY, ".DS_Store"];
/// The one account's databases, which move last (each after its `-wal`
/// and `-shm`): while either is at the top, the move isn't done.
const DATABASES: &[&str] = &["app.db", "signal.db"];

/// The accounts, as `accounts.json` keeps them.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Registry {
    /// In the switcher's order.
    pub accounts: Vec<Entry>,
    /// The one shown when the app last ran.
    #[serde(default)]
    pub active: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    /// Random, made here: an account is listed before it's linked, so
    /// before there's an ACI.
    pub id: String,
    /// The account the app had before there were several: its key is in
    /// the Keychain item that was the app's (or a former name's), and its
    /// files' stored paths name the top of the data directory.
    #[serde(default)]
    pub legacy: bool,
    /// It's been linked: only one that never was can be cancelled from its
    /// link screen (one linked before shows that screen when its
    /// registration doesn't load, which is no reason to delete it).
    #[serde(default)]
    pub linked: bool,
}

impl Entry {
    fn new() -> Self {
        use rand::RngCore as _;
        let mut bytes = [0u8; 8];
        rand::rng().fill_bytes(&mut bytes);
        Self { id: hex::encode(bytes), legacy: false, linked: false }
    }

    /// The account's data directory.
    pub fn dir(&self, root: &Path) -> PathBuf {
        root.join(ACCOUNTS_DIR).join(&self.id)
    }

    pub fn key_item(&self) -> KeyItem {
        if self.legacy { KeyItem::legacy() } else { KeyItem::account(&self.id) }
    }
}

/// The accounts in `root`, the app's data directory (none yet without an
/// `accounts.json`). One that can't be read stops the launch rather than
/// being written over.
pub fn load(root: &Path) -> Result<Registry> {
    let path = root.join(REGISTRY);
    match std::fs::read_to_string(&path) {
        Ok(text) => {
            let registry: Registry = serde_json::from_str(&text).with_context(|| format!("reading {}", path.display()))?;
            // An id names a directory that removing the account deletes:
            // only the kind made here.
            if let Some(entry) = registry.accounts.iter().find(|e| e.id.is_empty() || e.id.len() > 32 || !e.id.bytes().all(|b| b.is_ascii_hexdigit())) {
                anyhow::bail!("{} lists an account id that isn't one: {:?}", path.display(), entry.id);
            }
            Ok(registry)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Registry::default()),
        Err(error) => Err(error).with_context(|| format!("reading {}", path.display())),
    }
}

/// Writes the list all at once (a new file, renamed over the old).
pub fn save(root: &Path, registry: &Registry) -> Result<()> {
    let path = root.join(REGISTRY);
    let partial = root.join(format!("{REGISTRY}.partial"));
    std::fs::write(&partial, serde_json::to_vec_pretty(registry)?).with_context(|| format!("writing {}", partial.display()))?;
    std::fs::rename(&partial, &path).with_context(|| format!("writing {}", path.display()))
}

/// The accounts in `root`, after moving the one account's files from the
/// top of it (where the app kept them before there were several) into its
/// own directory. The account is listed before anything moves, each file
/// or directory moves in one rename, and the databases go last, so a
/// launch cut short finishes the move next time. `running`: whether the
/// app is open elsewhere with these databases, when the move is refused.
pub fn adopt_layout(root: &Path, running: impl FnOnce() -> bool) -> Result<Registry> {
    let mut registry = load(root)?;
    // A database at the top that the account has already (one a build from
    // before made after the move) is left, and isn't a move to finish.
    let moved = registry.accounts.iter().find(|entry| entry.legacy).map(|entry| entry.dir(root));
    let pending = DATABASES
        .iter()
        .any(|name| root.join(name).exists() && moved.as_ref().is_none_or(|dir| !dir.join(name).exists()));
    if !pending {
        return Ok(registry);
    }
    if running() {
        anyhow::bail!("Vernam is open already with the data in {}", root.display());
    }
    let mut leftovers: Vec<String> = std::fs::read_dir(root)
        .with_context(|| format!("reading {}", root.display()))?
        .map(|entry| entry.map(|entry| entry.file_name().to_string_lossy().into_owned()))
        .collect::<std::io::Result<_>>()?;
    leftovers.retain(|name| {
        // The hand-off socket, which this launch bound at startup, stays
        // at the top with the app's own files.
        !(name.starts_with("vernam-") && name.ends_with(".links"))
            && !APP_FILES.contains(&name.as_str())
            && !name.starts_with(REGISTRY)
    });
    // The rest, then the journals, then the databases.
    leftovers.sort_by_key(|name| (DATABASES.contains(&name.as_str()), DATABASES.iter().any(|db| name.starts_with(db))));
    let entry = match registry.accounts.iter().find(|entry| entry.legacy) {
        Some(entry) => entry.clone(),
        None => {
            let entry = Entry { legacy: true, linked: true, ..Entry::new() };
            registry.accounts.insert(0, entry.clone());
            registry.active.get_or_insert_with(|| entry.id.clone());
            save(root, &registry)?;
            entry
        }
    };
    let dir = entry.dir(root);
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    for name in leftovers {
        let (from, to) = (root.join(&name), dir.join(&name));
        // A database's journals move before it, so with the database moved
        // a journal at the top is another's: never put onto the account's.
        let database = DATABASES.iter().find(|db| name.starts_with(*db));
        if to.exists() || database.is_some_and(|db| *db != name && dir.join(db).exists()) {
            tracing::warn!("left {} where it is: the account has one already", from.display());
            continue;
        }
        std::fs::rename(&from, &to).with_context(|| format!("moving {} to {}", from.display(), to.display()))?;
    }
    tracing::info!("moved the account's data into {}", dir.display());
    Ok(registry)
}

/// Lists a new account (not linked yet) and makes its directory.
pub fn add(root: &Path, registry: &mut Registry) -> Result<Entry> {
    let entry = Entry::new();
    std::fs::create_dir_all(entry.dir(root)).with_context(|| format!("creating {}", entry.dir(root).display()))?;
    registry.accounts.push(entry.clone());
    save(root, registry)?;
    Ok(entry)
}

/// Takes an account off the list; its data is deleted with [`delete`]
/// once nothing uses it. (A launch cut short in between leaves a
/// directory nothing lists.)
pub fn remove(root: &Path, registry: &mut Registry, id: &str) -> Result<Option<Entry>> {
    let Some(index) = registry.accounts.iter().position(|entry| entry.id == id) else { return Ok(None) };
    let entry = registry.accounts.remove(index);
    if registry.active.as_deref() == Some(id) {
        registry.active = registry.accounts.first().map(|entry| entry.id.clone());
    }
    save(root, registry)?;
    Ok(Some(entry))
}

/// Deletes a removed account's data.
pub fn delete(root: &Path, entry: &Entry) -> Result<()> {
    let dir = entry.dir(root);
    match std::fs::remove_dir_all(&dir) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("deleting {}", dir.display())),
    }
}

/// Deletes account directories nothing lists: a removal cut short (the
/// app quit, or the backend didn't stop) leaves one. Returns their ids, for
/// their keys to go too. Only names an account id could have are touched.
pub fn sweep(root: &Path, registry: &Registry) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(root.join(ACCOUNTS_DIR)) else { return Vec::new() };
    let mut swept = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let id_like = !name.is_empty() && name.len() <= 32 && name.bytes().all(|b| b.is_ascii_hexdigit());
        if !id_like || registry.accounts.iter().any(|account| account.id == name) {
            continue;
        }
        match std::fs::remove_dir_all(entry.path()) {
            Ok(()) => {
                tracing::info!("deleted the data of removed account {name}");
                swept.push(name);
            }
            Err(error) => tracing::warn!(%error, "couldn't delete the data of removed account {name}"),
        }
    }
    swept
}

/// What starting an account's backend needs from the app.
#[derive(Clone)]
pub struct Launch {
    /// The app's data directory.
    pub root: PathBuf,
    pub key_store: KeyStore,
    pub device_name: String,
    pub demo: bool,
    pub call_lines: CallLines,
}

/// Opens an account's databases and starts its backend. For the legacy
/// account, `moved_from` is the former name whose data just moved in (its
/// key opens it), and the paths stored under the top of the data directory
/// and the former names' directories are rewritten to name its own.
pub fn start(
    launch: &Launch,
    entry: &Entry,
    moved_from: Option<&'static Former>,
) -> Result<(Backend, futures::channel::mpsc::UnboundedReceiver<signal_backend::Event>)> {
    let data_dir = entry.dir(&launch.root);
    std::fs::create_dir_all(&data_dir).with_context(|| format!("creating {}", data_dir.display()))?;
    let item = entry.key_item();
    // Data that moved in under a former name and hasn't had its key since.
    let moved_from = moved_from.or_else(|| (entry.legacy && launch.key_store == KeyStore::System).then(|| former_name::moved_from(&data_dir)).flatten());
    let loaded = keys::load_or_create(&data_dir, &item, launch.key_store, moved_from)?;
    tracing::info!("data directory: {}", data_dir.display());
    let mut former_data_dirs = Vec::new();
    if entry.legacy {
        // The paths stored under a former name name its directory, even
        // once the data has moved.
        if launch.key_store == KeyStore::System {
            former_data_dirs = former_name::former_data_dirs();
        }
        former_data_dirs.push(launch.root.clone());
    }
    let started = Backend::start(Config {
        data_dir: data_dir.clone(),
        db_key: loaded.key,
        device_name: launch.device_name.clone(),
        demo: launch.demo,
        former_data_dirs,
        call_lines: launch.call_lines.clone(),
    })?;
    // The key opened the data: kept as Vernam's, after which where moved-in
    // data came from is done with. If keeping it fails, the next launch
    // tries again in the same order.
    match keys::keep(&data_dir, &item, &loaded) {
        Ok(()) => former_name::key_settled(&data_dir),
        Err(error) => tracing::warn!(%error, "couldn't keep the database key"),
    }
    Ok(started)
}

/// One of the app's accounts, running.
pub struct Account {
    pub entry: Entry,
    pub state: Entity<AppState>,
    /// Its events and changes, while it's in the app.
    _subscriptions: Vec<gpui_kit::Subscription>,
}

/// The app's accounts and which one the window shows.
pub struct Accounts {
    launch: Launch,
    registry: Registry,
    pub list: Vec<Account>,
    /// Which of `list` the window shows.
    pub active: usize,
    notifier: Option<Rc<crate::notifications::Notifier>>,
    /// Each account's unread count, which the Dock's badge adds up.
    badges: Rc<RefCell<HashMap<String, usize>>>,
    /// The accounts' backends, for what's not on the UI thread.
    running: Running,
    /// What the switcher shows of each account, to redraw only when it changes.
    summaries: HashMap<String, Summary>,
    /// Why accounts couldn't be opened, for the window to say once it's open.
    pub startup_errors: Vec<String>,
}

/// What the window shows of an account it doesn't: its name, photo and
/// unread count in the switcher, and whether a call of its is on top.
#[derive(Clone, PartialEq)]
struct Summary {
    name: String,
    photo: Option<PathBuf>,
    unread: u32,
    call: bool,
    linked: bool,
}

impl Summary {
    fn of(state: &AppState) -> Self {
        Self {
            name: crate::views::nav::account_name(state),
            photo: state.own_profile.as_ref().and_then(|profile| profile.photo.clone()),
            unread: state.unread_chats_badge(),
            call: state.call.is_some() || state.lobby.is_some(),
            linked: state.account.is_some(),
        }
    }
}

/// The accounts' backends (a stopped one ignores what it's sent).
pub type Running = std::sync::Arc<std::sync::Mutex<Vec<Backend>>>;

/// The accounts, once the app is running.
struct AccountsGlobal(Entity<Accounts>);

impl Global for AccountsGlobal {}

/// The app's accounts, once they're running.
pub fn get(cx: &App) -> Option<Entity<Accounts>> {
    cx.try_global::<AccountsGlobal>().map(|global| global.0.clone())
}

/// The account the window shows.
pub fn active_state(cx: &App) -> Option<Entity<AppState>> {
    let accounts = get(cx)?;
    let accounts = accounts.read(cx);
    accounts.list.get(accounts.active).map(|account| account.state.clone())
}

/// The app's data directory (where the logs are), once running.
pub fn app_dir(cx: &App) -> Option<PathBuf> {
    get(cx).map(|accounts| accounts.read(cx).launch.root.clone())
}

impl Accounts {
    /// Starts the backends of the accounts in `registry` (and, when there
    /// are none, of a new one to link) and makes the result the app's. An
    /// account that can't be opened is skipped, unless none can.
    pub fn start(launch: Launch, mut registry: Registry, moved_from: Option<&'static Former>, cx: &mut App) -> Result<Entity<Accounts>> {
        // Removals cut short last time, finished before anything runs.
        for id in sweep(&launch.root, &registry) {
            keys::forget(&KeyItem::account(&id), launch.key_store);
        }
        if registry.accounts.is_empty() {
            let entry = add(&launch.root, &mut registry)?;
            registry.active = Some(entry.id);
            save(&launch.root, &registry)?;
        }
        let entity = cx.new(|cx| {
            let mut this = Accounts {
                launch,
                registry: registry.clone(),
                list: Vec::new(),
                active: 0,
                notifier: None,
                badges: Rc::default(),
                running: Running::default(),
                summaries: HashMap::new(),
                startup_errors: Vec::new(),
            };
            this.start_notifications(cx);
            this
        });
        let mut failures = Vec::new();
        for entry in &registry.accounts {
            let moved = if entry.legacy { moved_from } else { None };
            let result = entity.update(cx, |this, cx| this.open(entry.clone(), moved, cx));
            if let Err(error) = result {
                tracing::error!("couldn't open account {}: {error:#}", entry.id);
                failures.push(error);
            }
        }
        if entity.read(cx).list.is_empty() {
            return Err(failures.pop().unwrap_or_else(|| anyhow::anyhow!("no account to open")));
        }
        entity.update(cx, |this, cx| {
            this.startup_errors = failures.iter().map(|error| format!("Couldn't open an account: {error:#}")).collect();
            let active = this.registry.active.clone();
            match this.list.iter().position(|account| Some(&account.entry.id) == active.as_ref()) {
                Some(index) => this.activate(index, cx),
                // The one shown last didn't open: it stays the one to show
                // once it does.
                None => this.show(0, false, cx),
            }
        });
        cx.set_global(AccountsGlobal(entity.clone()));
        Ok(entity)
    }

    /// The app's accounts as `backends` (made up, from 0), the first shown.
    #[cfg(test)]
    pub(crate) fn for_test(
        root: PathBuf,
        backends: Vec<(Backend, futures::channel::mpsc::UnboundedReceiver<signal_backend::Event>)>,
        cx: &mut App,
    ) -> Entity<Accounts> {
        let launch = Launch { root, key_store: KeyStore::File, device_name: "test".into(), demo: true, call_lines: CallLines::default() };
        let entity = cx.new(|_| Accounts {
            launch,
            registry: Registry::default(),
            list: Vec::new(),
            active: 0,
            notifier: None,
            badges: Rc::default(),
            running: Running::default(),
            summaries: HashMap::new(),
            startup_errors: Vec::new(),
        });
        entity.update(cx, |this, cx| {
            for (i, (backend, events)) in backends.into_iter().enumerate() {
                let state = crate::state::new_state(backend, cx);
                this.attach(Entry { id: i.to_string(), legacy: false, linked: true }, state, events, cx);
            }
            this.activate(0, cx);
        });
        cx.set_global(AccountsGlobal(entity.clone()));
        entity
    }

    /// Starts an account's backend and its state, with backend events
    /// pumped into it.
    fn open(&mut self, entry: Entry, moved_from: Option<&'static Former>, cx: &mut Context<Self>) -> Result<()> {
        let (backend, events) = start(&self.launch, &entry, moved_from)?;
        let state = crate::state::new_state(backend, cx);
        self.attach(entry, state, events, cx);
        Ok(())
    }

    /// Takes `state` in as the account `entry`.
    fn attach(
        &mut self,
        entry: Entry,
        state: Entity<AppState>,
        mut events: futures::channel::mpsc::UnboundedReceiver<signal_backend::Event>,
        cx: &mut Context<Self>,
    ) {
        if let Some(notifier) = &self.notifier {
            let notifier = crate::notifications::AccountNotifier::new(notifier.clone(), entry.id.clone(), self.badges.clone());
            state.update(cx, |state, _| state.notifier = Some(Rc::new(notifier)));
        }
        self.running.lock().unwrap_or_else(|e| e.into_inner()).push(state.read(cx).backend.clone());
        let pump_state = state.clone();
        cx.spawn(async move |_, cx| {
            while let Some(event) = events.next().await {
                pump_state.update(cx, |state, cx| state.handle(event, cx));
            }
        })
        .detach();
        let id = entry.id.clone();
        let unlinked = cx.subscribe(&state, move |this, _, event: &AppEvent, cx| {
            if matches!(event, AppEvent::Unlinked) {
                this.unlinked(&id, cx);
            }
        });
        // The switcher shows each account's name, photo and unread count,
        // and the window a hidden account's call.
        let id = entry.id.clone();
        let summary = cx.observe(&state, move |this, state, cx| {
            let summary = Summary::of(state.read(cx));
            if this.summaries.get(&id) == Some(&summary) {
                return;
            }
            // Linked once: from now on, never cancelled.
            if summary.linked {
                this.mark_linked(&id);
            }
            this.summaries.insert(id.clone(), summary);
            cx.notify();
        });
        self.list.push(Account { entry, state, _subscriptions: vec![unlinked, summary] });
    }

    /// One notification center for the app: what's done in a notification
    /// goes to its account.
    fn start_notifications(&mut self, cx: &mut Context<Self>) {
        let (tx, mut rx) = futures::channel::mpsc::unbounded();
        self.notifier = crate::notifications::Notifier::start(tx).map(Rc::new);
        if self.notifier.is_none() {
            return;
        }
        cx.spawn(async move |this, cx| {
            while let Some(action) = rx.next().await {
                let routed = this.update(cx, |this, cx| {
                    let (account, action) = crate::notifications::route(action);
                    // Before there were several, notifications had no account.
                    let index = match account {
                        Some(id) => this.list.iter().position(|a| a.entry.id == id),
                        None => this.list.iter().position(|a| a.entry.legacy).or(Some(0)),
                    };
                    let Some(index) = index.filter(|&i| i < this.list.len()) else { return };
                    if matches!(action, crate::notifications::NotificationAction::Open(_)) {
                        this.activate(index, cx);
                    }
                    let state = this.list[index].state.clone();
                    state.update(cx, |state, cx| state.on_notification(action, cx));
                });
                if routed.is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    /// Notes (once) that the account `id` has been linked.
    fn mark_linked(&mut self, id: &str) {
        let Some(entry) = self.registry.accounts.iter_mut().find(|entry| entry.id == id && !entry.linked) else { return };
        entry.linked = true;
        if let Some(account) = self.list.iter_mut().find(|account| account.entry.id == id) {
            account.entry.linked = true;
        }
        if let Err(error) = save(&self.launch.root, &self.registry) {
            tracing::warn!("couldn't save the accounts: {error:#}");
        }
    }

    /// Shows the account at `index` in the window.
    pub fn activate(&mut self, index: usize, cx: &mut Context<Self>) {
        self.show(index, true, cx);
    }

    /// Shows the account at `index`, and with `keep`, keeps it as the one
    /// to show next time.
    fn show(&mut self, index: usize, keep: bool, cx: &mut Context<Self>) {
        if index >= self.list.len() {
            return;
        }
        self.active = index;
        for (i, account) in self.list.iter().enumerate() {
            account.state.update(cx, |state, cx| state.set_shown(i == index, cx));
        }
        let id = self.list[index].entry.id.clone();
        if keep && self.registry.active.as_ref() != Some(&id) {
            self.registry.active = Some(id);
            if let Err(error) = save(&self.launch.root, &self.registry) {
                tracing::warn!("couldn't save the accounts: {error:#}");
            }
        }
        cx.notify();
    }

    /// Adds an account and shows it, to be linked.
    pub fn add_account(&mut self, cx: &mut Context<Self>) {
        let result = add(&self.launch.root, &mut self.registry).and_then(|entry| {
            let opened = self.open(entry.clone(), None, cx);
            if opened.is_err() {
                let _ = remove(&self.launch.root, &mut self.registry, &entry.id);
                let _ = delete(&self.launch.root, &entry);
                // (Its key may have been made before the backend failed.)
                keys::forget(&entry.key_item(), self.launch.key_store);
            }
            opened
        });
        match result {
            Ok(()) => self.activate(self.list.len() - 1, cx),
            Err(error) => {
                tracing::error!("couldn't add an account: {error:#}");
                if let Some(account) = self.list.get(self.active) {
                    account.state.update(cx, |_, cx| cx.emit(AppEvent::Error(format!("Couldn't add an account: {error:#}").into())));
                }
            }
        }
    }

    /// Whether the account at `index` can be removed without unlinking it:
    /// it's never been linked (see [`Entry::linked`]), and isn't the only one.
    pub fn can_cancel(&self, index: usize, cx: &App) -> bool {
        self.list.len() > 1
            && self.list.get(index).is_some_and(|account| {
                !account.entry.legacy && !account.entry.linked && account.state.read(cx).account.is_none()
            })
    }

    /// Takes an account off the app: its backend stops, and its data and
    /// key are deleted. Never the last one.
    pub fn remove_account(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(index) = self.list.iter().position(|account| account.entry.id == id) else { return };
        if self.list.len() < 2 {
            return;
        }
        let account = self.list.remove(index);
        let in_call = account.state.update(cx, |state, _| {
            state.forget_notifications();
            // Nothing more of it on screen or in the Dock's badge.
            state.notifier = None;
            let in_call = state.call.is_some();
            if in_call {
                state.hang_up();
            }
            in_call
        });
        self.badges.borrow_mut().remove(id);
        self.summaries.remove(id);
        let data_dir = account.entry.dir(&self.launch.root);
        self.running.lock().unwrap_or_else(|e| e.into_inner()).retain(|backend| backend.data_dir() != data_dir);
        if let Err(error) = remove(&self.launch.root, &mut self.registry, id) {
            tracing::warn!("couldn't take the account off the list: {error:#}");
        }
        let active = if self.active > index || self.active >= self.list.len() { self.active.saturating_sub(1) } else { self.active };
        self.activate(active, cx);
        // A call is given a moment to end (Stop would cancel the hang-up
        // with the rest of the backend's work), then the backend stops,
        // and only then do its files go: nothing of it writes there after.
        let (root, key_store, state) = (self.launch.root.clone(), self.launch.key_store, account.state);
        let entry = account.entry;
        cx.spawn(async move |_, cx| {
            let mut waited = std::time::Duration::ZERO;
            while in_call && waited < HANG_UP_WAIT && cx.update(|cx| state.read(cx).call.is_some()) {
                cx.background_executor().timer(std::time::Duration::from_millis(50)).await;
                waited += std::time::Duration::from_millis(50);
            }
            let backend = cx.update(|cx| state.read(cx).backend.clone());
            let id = entry.id.clone();
            cx.background_spawn(async move {
                // (Off the UI thread: it may be connecting still.)
                if !backend.stop(STOP_WAIT) {
                    // Still using them: its files stay, unlisted.
                    return tracing::warn!("account {id}'s backend didn't stop; left its data in {}", entry.dir(&root).display());
                }
                if let Err(error) = delete(&root, &entry) {
                    tracing::warn!("couldn't delete the account's data: {error:#}");
                }
                keys::forget(&entry.key_item(), key_store);
                tracing::info!("removed account {id}");
            })
            .await;
        })
        .detach();
    }

    /// An account was unlinked (from its settings or the phone) and its
    /// data wiped: with others in the app, it goes; alone, it's linked again.
    fn unlinked(&mut self, id: &str, cx: &mut Context<Self>) {
        if self.list.len() > 1 {
            self.remove_account(id, cx);
        }
    }

    /// Ends every account's call and closes every database, as the app quits.
    pub fn shut_down(&self, cx: &App) {
        for account in &self.list {
            let state = account.state.read(cx);
            // A call still up (the Dock's Quit doesn't ask) is hung up.
            if state.call.is_some() {
                state.send(signal_backend::Command::HangUp);
            }
            state.backend.shutdown();
        }
    }

    pub fn running(&self) -> Running {
        self.running.clone()
    }

    /// The account in a call (or ringing), if any: one at a time.
    pub fn in_call(&self, cx: &App) -> Option<usize> {
        self.list.iter().position(|account| account.state.read(cx).call.is_some())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn the_one_accounts_files_move_into_its_directory_once() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        for name in ["logs", "attachments"] {
            std::fs::create_dir_all(root.join(name)).unwrap();
        }
        for name in ["app.db", "signal.db", "db.key", "window.json", "moved-from"] {
            std::fs::write(root.join(name), name).unwrap();
        }
        // A launch binds its hand-off socket before the move runs.
        std::fs::write(root.join("vernam-1000-0123456789abcdef.links"), "").unwrap();
        let registry = adopt_layout(root, || false).unwrap();
        let [entry] = &registry.accounts[..] else { panic!("{registry:?}") };
        assert!(entry.legacy && entry.linked, "never cancelled from the link screen");
        assert_eq!(registry.active.as_deref(), Some(entry.id.as_str()));
        assert_eq!(entry.key_item(), KeyItem::legacy());
        let dir = entry.dir(root);
        for name in ["app.db", "signal.db", "db.key", "moved-from", "attachments"] {
            assert!(dir.join(name).exists(), "{name} moved");
            assert!(!root.join(name).exists(), "{name} gone from the top");
        }
        // The app's own stay.
        assert!(root.join("logs").is_dir() && root.join("window.json").exists());
        assert!(!dir.join("logs").exists());
        // The hand-off socket stays at the top: this launch is listening on it.
        assert!(root.join("vernam-1000-0123456789abcdef.links").exists());
        assert!(!dir.join("vernam-1000-0123456789abcdef.links").exists());
        // Again: nothing to do, the same account.
        assert_eq!(adopt_layout(root, || false).unwrap(), registry);
        assert_eq!(load(root).unwrap(), registry);
    }

    #[test]
    fn a_move_cut_short_finishes_into_the_same_account() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        let entry = Entry { id: "0a1b".into(), legacy: true, linked: true };
        let registry = Registry { accounts: vec![entry.clone()], active: Some("0a1b".into()) };
        save(root, &registry).unwrap();
        std::fs::create_dir_all(entry.dir(root)).unwrap();
        std::fs::write(entry.dir(root).join("app.db"), "moved").unwrap();
        std::fs::write(root.join("signal.db"), "not yet").unwrap();
        assert_eq!(adopt_layout(root, || false).unwrap(), registry);
        assert_eq!(std::fs::read_to_string(entry.dir(root).join("signal.db")).unwrap(), "not yet");
        assert!(!root.join("signal.db").exists());
    }

    #[test]
    fn nothing_moves_without_a_database_or_while_the_app_is_open() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        // A save of the window's place cut short is the app's, not an account.
        std::fs::write(root.join("window.json.partial"), "{}").unwrap();
        std::fs::write(root.join("notes.txt"), "someone's").unwrap();
        assert!(adopt_layout(root, || panic!("not asked")).unwrap().accounts.is_empty());
        assert!(root.join("notes.txt").exists());
        // With the account's databases there and the app open elsewhere,
        // nothing moves.
        std::fs::write(root.join("app.db"), "app").unwrap();
        assert!(adopt_layout(root, || true).is_err());
        assert!(root.join("app.db").exists());
        assert!(load(root).unwrap().accounts.is_empty());
        let registry = adopt_layout(root, || false).unwrap();
        assert!(registry.accounts[0].dir(root).join("app.db").exists());
        assert!(root.join("window.json.partial").exists());
        // A build from before made new databases at the top after the move:
        // they stay, and there's nothing to move (or to ask about) again.
        std::fs::write(root.join("app.db"), "new").unwrap();
        assert_eq!(adopt_layout(root, || panic!("not asked")).unwrap(), registry);
        // Its journal stays with it even while another move is to finish.
        std::fs::write(root.join("app.db-wal"), "new's").unwrap();
        std::fs::write(root.join("signal.db"), "signal").unwrap();
        adopt_layout(root, || false).unwrap();
        assert!(root.join("app.db-wal").exists() && !registry.accounts[0].dir(root).join("app.db-wal").exists());
        assert!(registry.accounts[0].dir(root).join("signal.db").exists());
        assert_eq!(std::fs::read_to_string(registry.accounts[0].dir(root).join("app.db")).unwrap(), "app");
    }

    #[test]
    fn a_fresh_directory_has_no_accounts_until_one_is_added_and_removed() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        std::fs::create_dir_all(root.join("logs")).unwrap();
        let mut registry = adopt_layout(root, || false).unwrap();
        assert!(registry.accounts.is_empty());
        let first = add(root, &mut registry).unwrap();
        let second = add(root, &mut registry).unwrap();
        assert!(!first.legacy && first.id != second.id);
        assert_eq!(second.key_item(), KeyItem::account(&second.id));
        assert!(second.dir(root).is_dir());
        registry.active = Some(second.id.clone());
        // Taken off the list, then the app quit before deleting it: the
        // next launch does.
        assert_eq!(remove(root, &mut registry, &second.id).unwrap(), Some(second.clone()));
        std::fs::write(second.dir(root).join("app.db"), "theirs").unwrap();
        std::fs::create_dir_all(root.join(ACCOUNTS_DIR).join("not-an-account")).unwrap();
        assert_eq!(sweep(root, &registry), [second.id.clone()]);
        assert!(!second.dir(root).exists() && first.dir(root).is_dir());
        assert!(root.join(ACCOUNTS_DIR).join("not-an-account").is_dir());
        delete(root, &second).unwrap();
        assert_eq!(registry.accounts, [first.clone()]);
        assert_eq!(registry.active, Some(first.id.clone()), "the next is shown");
        assert_eq!(load(root).unwrap(), registry);
        // A list that can't be read isn't written over.
        // Nor one naming a directory outside `accounts/`.
        std::fs::write(root.join(REGISTRY), r#"{"accounts":[{"id":"../.."}]}"#).unwrap();
        assert!(load(root).is_err());
        std::fs::write(root.join(REGISTRY), "{").unwrap();
        assert!(adopt_layout(root, || false).is_err());
    }
}
