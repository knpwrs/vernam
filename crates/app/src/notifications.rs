//! System notifications for new messages, with an inline reply field and
//! "Mark as Read" (Signal Desktop's notifications have neither action).
//!
//! macOS only allows notifications from an app bundle: `cargo run` wraps
//! the binary in one (scripts/cargo-runner.sh), and scripts/bundle.sh makes
//! one for release builds. A bare binary simply doesn't notify.

use futures::channel::mpsc;
use signal_store::ConversationId;

/// What the user did with a notification.
#[derive(Debug, Clone, PartialEq)]
pub enum NotificationAction {
    /// Clicked: show the chat.
    Open(ConversationId),
    /// Typed a reply in the notification.
    Reply(ConversationId, String),
    MarkRead(ConversationId),
}

/// What notifications show (Desktop's Settings → Notifications).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NotificationSetting {
    #[default]
    NameAndMessage,
    NameOnly,
    NoNameOrMessage,
    Off,
}

impl NotificationSetting {
    pub const KEY: &str = "notifications.content";

    pub fn from_setting(value: Option<&str>) -> Self {
        match value {
            Some("name") => Self::NameOnly,
            Some("none") => Self::NoNameOrMessage,
            Some("off") => Self::Off,
            _ => Self::NameAndMessage,
        }
    }

    pub fn as_setting(self) -> &'static str {
        match self {
            Self::NameAndMessage => "message",
            Self::NameOnly => "name",
            Self::NoNameOrMessage => "none",
            Self::Off => "off",
        }
    }

    /// Title and body for a message from `sender` (in `group`, if any).
    pub fn text(self, sender: &str, group: Option<&str>, preview: &str) -> (String, String) {
        let name = match group {
            Some(group) => format!("{sender} in {group}"),
            None => sender.to_owned(),
        };
        match self {
            Self::NameAndMessage => (name, preview.to_owned()),
            Self::NameOnly => (name, "New Message".into()),
            Self::NoNameOrMessage | Self::Off => ("Signal".into(), "New Message".into()),
        }
    }
}

/// One notification to show.
pub struct Notification<'a> {
    /// Unique per message, so it can be removed once read.
    pub id: &'a str,
    pub conversation: &'a ConversationId,
    pub title: &'a str,
    pub body: &'a str,
    /// With the system's sound (Desktop's "Push notification sounds").
    pub sound: bool,
}

/// Between an account's id and what it names in a notification's id and
/// thread (a chat): neither a conversation id nor an account id has it.
const ACCOUNT_SEPARATOR: char = '|';

/// One account's notifications, through the app's one [`Notifier`]: their
/// ids and threads name the account, so that what's done in one goes to
/// it, and the Dock's badge adds up every account's unread count.
pub struct AccountNotifier {
    notifier: std::rc::Rc<Notifier>,
    account: String,
    badges: std::rc::Rc<std::cell::RefCell<std::collections::HashMap<String, usize>>>,
}

impl AccountNotifier {
    pub fn new(
        notifier: std::rc::Rc<Notifier>,
        account: String,
        badges: std::rc::Rc<std::cell::RefCell<std::collections::HashMap<String, usize>>>,
    ) -> Self {
        Self { notifier, account, badges }
    }

    fn scoped(&self, id: &str) -> String {
        format!("{}{ACCOUNT_SEPARATOR}{id}", self.account)
    }

    pub fn post(&self, notification: &Notification) {
        let id = self.scoped(notification.id);
        let conversation = ConversationId(self.scoped(&notification.conversation.0));
        self.notifier.post(&Notification { id: &id, conversation: &conversation, ..*notification });
    }

    pub fn remove(&self, ids: &[String]) {
        let ids: Vec<String> = ids.iter().map(|id| self.scoped(id)).collect();
        self.notifier.remove(&ids);
    }

    /// This account's unread count; the Dock shows every account's.
    pub fn set_badge(&self, count: usize) {
        let total = {
            let mut badges = self.badges.borrow_mut();
            badges.insert(self.account.clone(), count);
            badges.values().sum()
        };
        self.notifier.set_badge(total);
    }
}

/// The account a notification's action is for (`None` for one posted
/// before there were several), and the action with its chat.
pub fn route(action: NotificationAction) -> (Option<String>, NotificationAction) {
    let split = |conversation: ConversationId| match conversation.0.split_once(ACCOUNT_SEPARATOR) {
        Some((account, chat)) => (Some(account.to_owned()), ConversationId(chat.to_owned())),
        None => (None, conversation),
    };
    match action {
        NotificationAction::Open(conversation) => {
            let (account, conversation) = split(conversation);
            (account, NotificationAction::Open(conversation))
        }
        NotificationAction::Reply(conversation, text) => {
            let (account, conversation) = split(conversation);
            (account, NotificationAction::Reply(conversation, text))
        }
        NotificationAction::MarkRead(conversation) => {
            let (account, conversation) = split(conversation);
            (account, NotificationAction::MarkRead(conversation))
        }
    }
}

#[cfg(target_os = "macos")]
pub use macos::Notifier;

#[cfg(target_os = "linux")]
pub use linux::Notifier;

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub struct Notifier;

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
impl Notifier {
    pub fn start(_actions: mpsc::UnboundedSender<NotificationAction>) -> Option<Self> {
        None
    }
    pub fn post(&self, _notification: &Notification) {}
    pub fn remove(&self, _ids: &[String]) {}
    pub fn set_badge(&self, _count: usize) {}
}

#[cfg(target_os = "macos")]
mod macos {
    use std::sync::OnceLock;

    use block2::RcBlock;
    use objc2::rc::Retained;
    use objc2::runtime::{AnyObject, Bool, NSObject, NSObjectProtocol, ProtocolObject};
    use objc2::{AllocAnyThread, define_class, msg_send};
    use objc2_foundation::{NSArray, NSBundle, NSError, NSSet, NSString};
    use objc2_user_notifications::{
        UNAuthorizationOptions, UNMutableNotificationContent, UNNotification, UNNotificationAction,
        UNNotificationActionOptions, UNNotificationCategory, UNNotificationCategoryOptions,
        UNNotificationPresentationOptions, UNNotificationRequest, UNNotificationResponse, UNNotificationSound,
        UNTextInputNotificationAction, UNTextInputNotificationResponse, UNUserNotificationCenter,
        UNUserNotificationCenterDelegate,
    };

    use super::*;

    const CATEGORY: &str = "message";
    const REPLY: &str = "reply";
    const MARK_READ: &str = "mark-read";

    /// Where the delegate (called by the system) sends what was done.
    static ACTIONS: OnceLock<mpsc::UnboundedSender<NotificationAction>> = OnceLock::new();

    define_class!(
        #[unsafe(super(NSObject))]
        #[name = "VernamNotificationDelegate"]
        struct Delegate;

        unsafe impl NSObjectProtocol for Delegate {}

        unsafe impl UNUserNotificationCenterDelegate for Delegate {
            // While the app is open (on another chat), still show it.
            #[unsafe(method(userNotificationCenter:willPresentNotification:withCompletionHandler:))]
            fn will_present(
                &self,
                _center: &UNUserNotificationCenter,
                _notification: &UNNotification,
                handler: &block2::DynBlock<dyn Fn(UNNotificationPresentationOptions)>,
            ) {
                // (A notification without a sound plays none.)
                handler.call((UNNotificationPresentationOptions::Banner
                    | UNNotificationPresentationOptions::List
                    | UNNotificationPresentationOptions::Sound,));
            }

            #[unsafe(method(userNotificationCenter:didReceiveNotificationResponse:withCompletionHandler:))]
            fn did_receive(
                &self,
                _center: &UNUserNotificationCenter,
                response: &UNNotificationResponse,
                handler: &block2::DynBlock<dyn Fn()>,
            ) {
                let conversation =
                    ConversationId(response.notification().request().content().threadIdentifier().to_string());
                let action = match response.actionIdentifier().to_string().as_str() {
                    REPLY => {
                        let object: &AnyObject = response.as_ref();
                        let text = object
                            .downcast_ref::<UNTextInputNotificationResponse>()
                            .map(|r| r.userText().to_string())
                            .unwrap_or_default();
                        (!text.trim().is_empty()).then_some(NotificationAction::Reply(conversation, text))
                    }
                    MARK_READ => Some(NotificationAction::MarkRead(conversation)),
                    // Clicked (or "Show"); dismissing does nothing.
                    id if id == unsafe { objc2_user_notifications::UNNotificationDefaultActionIdentifier }.to_string() => {
                        Some(NotificationAction::Open(conversation))
                    }
                    _ => None,
                };
                if let (Some(action), Some(actions)) = (action, ACTIONS.get()) {
                    let _ = actions.unbounded_send(action);
                }
                handler.call(());
            }
        }
    );

    impl Delegate {
        fn new() -> Retained<Self> {
            let this = Self::alloc().set_ivars(());
            unsafe { msg_send![super(this), init] }
        }
    }

    pub struct Notifier {
        center: Retained<UNUserNotificationCenter>,
    }

    impl Notifier {
        /// Asks for permission and sets up the reply action. `None` outside
        /// an app bundle, where the system refuses notifications.
        pub fn start(actions: mpsc::UnboundedSender<NotificationAction>) -> Option<Self> {
            NSBundle::mainBundle().bundleIdentifier()?;
            let _ = ACTIONS.set(actions);
            let center = UNUserNotificationCenter::currentNotificationCenter();

            let delegate = Delegate::new();
            center.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
            // The center holds its delegate weakly; this one lives as long as the app.
            std::mem::forget(delegate);

            let reply = UNTextInputNotificationAction::actionWithIdentifier_title_options_textInputButtonTitle_textInputPlaceholder(
                &NSString::from_str(REPLY),
                &NSString::from_str("Reply"),
                UNNotificationActionOptions::empty(),
                &NSString::from_str("Send"),
                &NSString::from_str("Message"),
            );
            let mark_read = UNNotificationAction::actionWithIdentifier_title_options(
                &NSString::from_str(MARK_READ),
                &NSString::from_str("Mark as Read"),
                UNNotificationActionOptions::empty(),
            );
            let actions = NSArray::from_retained_slice(&[Retained::into_super(reply), mark_read]);
            let category = UNNotificationCategory::categoryWithIdentifier_actions_intentIdentifiers_options(
                &NSString::from_str(CATEGORY),
                &actions,
                &NSArray::new(),
                UNNotificationCategoryOptions::empty(),
            );
            center.setNotificationCategories(&NSSet::from_retained_slice(&[category]));

            let granted = RcBlock::new(|granted: Bool, _error: *mut NSError| {
                tracing::info!(granted = granted.as_bool(), "notification permission");
            });
            center.requestAuthorizationWithOptions_completionHandler(
                UNAuthorizationOptions::Alert | UNAuthorizationOptions::Sound | UNAuthorizationOptions::Badge,
                &granted,
            );
            Some(Self { center })
        }

        pub fn post(&self, notification: &Notification) {
            let content = UNMutableNotificationContent::new();
            content.setTitle(&NSString::from_str(notification.title));
            content.setBody(&NSString::from_str(notification.body));
            // Groups a chat's notifications, and tells actions which chat.
            content.setThreadIdentifier(&NSString::from_str(&notification.conversation.0));
            content.setCategoryIdentifier(&NSString::from_str(CATEGORY));
            if notification.sound {
                content.setSound(Some(&UNNotificationSound::defaultSound()));
            }
            let request = UNNotificationRequest::requestWithIdentifier_content_trigger(
                &NSString::from_str(notification.id),
                &content,
                None,
            );
            self.center.addNotificationRequest_withCompletionHandler(&request, None);
        }

        /// Takes notifications for messages that have been read off screen.
        pub fn remove(&self, ids: &[String]) {
            if ids.is_empty() {
                return;
            }
            let ids: Vec<Retained<NSString>> = ids.iter().map(|id| NSString::from_str(id)).collect();
            self.center.removeDeliveredNotificationsWithIdentifiers(&NSArray::from_retained_slice(&ids));
        }

        /// The unread count on the Dock icon.
        pub fn set_badge(&self, count: usize) {
            self.center.setBadgeCount_withCompletionHandler(count as isize, None);
        }
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex, OnceLock};

    use futures::StreamExt as _;
    use zbus::zvariant::Variant as Value;

    use super::*;

    /// The daemon's names, per `org.freedesktop.Notifications`.
    const SERVICE: &str = "org.freedesktop.Notifications";
    const PATH: &str = "/org/freedesktop/Notifications";

    /// What a reply may carry: a send beyond Signal's own limit fails
    /// anyway, and a buggy daemon shouldn't push unbounded text through.
    const MAX_REPLY_CHARS: usize = 4096;

    /// Where the signal thread (called by the daemon) sends what was done.
    static ACTIONS: OnceLock<mpsc::UnboundedSender<NotificationAction>> = OnceLock::new();

    /// What the daemon assigns, and what it names: the notification id it
    /// gave back, and the chat it's about, which its actions need. (And
    /// whether it offers replies in the notification, KDE's do.)
    struct Shared {
        ids: Mutex<HashMap<String, u32>>,
        conversations: Mutex<HashMap<u32, ConversationId>>,
        inline_reply: bool,
    }

    /// A call to make at the daemon, ferried to the send thread: its
    /// callers run on the interface's main thread, which must not block
    /// on a daemon that can be slow or hung.
    enum Out {
        Post {
            id: String,
            conversation: ConversationId,
            title: String,
            body: String,
            sound: bool,
        },
        Remove(Vec<String>),
        Badge(usize),
    }

    pub struct Notifier {
        out: mpsc::UnboundedSender<Out>,
    }

    impl Notifier {
        /// Joins the session bus and its notification daemon, and listens
        /// for what the user does. `None` without a bus or a daemon, where
        /// nothing can notify.
        pub fn start(actions: mpsc::UnboundedSender<NotificationAction>) -> Option<Self> {
            let Ok(connection) = futures::executor::block_on(zbus::Connection::session()) else {
                tracing::info!("no session bus, notifications are off");
                return None;
            };
            let capabilities: Vec<String> = match futures::executor::block_on(connection.call_method(
                Some(SERVICE),
                PATH,
                Some(SERVICE),
                "GetCapabilities",
                &(),
            )) {
                Ok(reply) => reply.body().deserialize::<Vec<String>>().unwrap_or_default(),
                Err(_) => {
                    tracing::info!("no notification daemon, notifications are off");
                    return None;
                }
            };
            let _ = ACTIONS.set(actions);
            let shared = Arc::new(Shared {
                ids: Mutex::new(HashMap::new()),
                conversations: Mutex::new(HashMap::new()),
                inline_reply: capabilities.iter().any(|capability| capability == "inline-reply"),
            });
            let rule = zbus::MatchRule::builder()
                .msg_type(zbus::message::Type::Signal)
                .sender(SERVICE)
                .interface(SERVICE)
                .build()
                .expect("the rule's names are valid");
            let stream_connection = connection.clone();
            let stream_shared = shared.clone();
            let listen = std::thread::Builder::new().name("notifications".into()).spawn(move || {
                // The stream ends when the daemon stops (a restart, an
                // update): listen again, rather than miss every action from
                // then on.
                loop {
                    let listening = futures::executor::block_on(async {
                        let Ok(mut stream) = zbus::MessageStream::for_match_rule(rule.clone(), &stream_connection, None).await
                        else {
                            return false;
                        };
                        while let Some(message) = stream.next().await {
                            let Ok(message) = message else { break };
                            let header = message.header();
                            let Some(member) = header.member() else { continue };
                            if member == "ActionInvoked" {
                                act(&stream_shared, &message);
                            } else if member == "NotificationReplied" {
                                if let Ok((id, text)) = message.body().deserialize::<(u32, String)>() {
                                    replied(&stream_shared, id, text);
                                }
                            } else if member == "NotificationClosed" {
                                if let Ok((id, _reason)) = message.body().deserialize::<(u32, u32)>() {
                                    forget(&stream_shared, id);
                                }
                            }
                        }
                        true
                    });
                    if listening {
                        tracing::warn!("the notification daemon's signals stopped; listening again");
                    }
                    std::thread::sleep(std::time::Duration::from_secs(1));
                }
            });
            // If the thread couldn't start, nothing listens; notifications
            // still show, but acts go nowhere. (Like macOS's refusal path.)
            if listen.is_err() {
                return None;
            }
            let (out, mut out_rx) = mpsc::unbounded::<Out>();
            let send_connection = connection.clone();
            let send_shared = shared.clone();
            let send = std::thread::Builder::new().name("notify-send".into()).spawn(move || {
                futures::executor::block_on(async {
                    while let Some(op) = out_rx.next().await {
                        match op {
                            Out::Post { id, conversation, title, body, sound } => {
                                daemon_post(&send_connection, &send_shared, &id, conversation, &title, &body, sound)
                            }
                            Out::Remove(ids) => daemon_close(&send_connection, &send_shared, &ids),
                            Out::Badge(count) => daemon_badge(&send_connection, count),
                        }
                    }
                })
            });
            // Without it nothing reaches the daemon; notifications are off.
            if send.is_err() {
                return None;
            }
            Some(Self { out })
        }

        pub fn post(&self, notification: &Notification) {
            let _ = self.out.unbounded_send(Out::Post {
                id: notification.id.to_owned(),
                conversation: notification.conversation.clone(),
                title: notification.title.to_owned(),
                body: notification.body.to_owned(),
                sound: notification.sound,
            });
        }

        /// Takes notifications for messages that have been read off screen.
        pub fn remove(&self, ids: &[String]) {
            if ids.is_empty() {
                return;
            }
            let _ = self.out.unbounded_send(Out::Remove(ids.to_owned()));
        }

        /// The unread count, for shells with Unity's launcher API (GNOME
        /// and KDE extensions read it); best effort, as most have none.
        pub fn set_badge(&self, count: usize) {
            let _ = self.out.unbounded_send(Out::Badge(count));
        }
    }

    /// Posts one notification, and keeps what the daemon assigned so its
    /// actions can find the chat and a later `remove` can close it.
    fn daemon_post(connection: &zbus::Connection, shared: &Shared, id: &str, conversation: ConversationId, title: &str, body: &str, sound: bool) {
        let mut acts = vec!["default", "Show", "mark-read", "Mark as Read"];
        if shared.inline_reply {
            acts.extend(["inline-reply", "Reply"]);
        }
        let mut hints: HashMap<&str, Value> = HashMap::new();
        hints.insert("category", Value::from("im.received"));
        hints.insert("desktop-entry", Value::from(crate::linux::APP_ID));
        if sound {
            hints.insert("sound-name", Value::from("message-new-instant"));
        }
        let replaces = shared.ids.lock().unwrap_or_else(|e| e.into_inner()).get(id).copied().unwrap_or(0);
        let call = (
            "Vernam",
            replaces,
            crate::linux::APP_ID,
            title,
            body,
            acts.as_slice(),
            hints,
            -1i32,
        );
        match futures::executor::block_on(connection.call_method(Some(SERVICE), PATH, Some(SERVICE), "Notify", &call)) {
            Ok(reply) => {
                if let Ok(assigned) = reply.body().deserialize::<u32>() {
                    shared.ids.lock().unwrap_or_else(|e| e.into_inner()).insert(id.to_owned(), assigned);
                    shared
                        .conversations
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .insert(assigned, conversation);
                }
            }
            // Missed notifications are silent, as on macOS.
            Err(_) => {}
        }
    }

    /// Closes the daemon's notifications for messages read off screen. The
    /// ids are looked up here, on the send thread: a `remove` is enqueued
    /// after its `post`, so what `post` assigned is already in place.
    fn daemon_close(connection: &zbus::Connection, shared: &Shared, ids: &[String]) {
        for id in ids {
            let assigned = shared.ids.lock().unwrap_or_else(|e| e.into_inner()).remove(id);
            let Some(assigned) = assigned else { continue };
            let _ = futures::executor::block_on(connection.call_method(
                Some(SERVICE),
                PATH,
                Some(SERVICE),
                "CloseNotification",
                &(assigned,),
            ));
        }
    }

    /// The unread count, for shells with Unity's launcher API (GNOME and
    /// KDE extensions read it); best effort, as most have none.
    fn daemon_badge(connection: &zbus::Connection, count: usize) {
        let hints: HashMap<&str, Value> =
            HashMap::from([("count", Value::from(count as i64)), ("count-visible", Value::from(count > 0))]);
        let _ = futures::executor::block_on(connection.emit_signal(
            None::<&str>,
            "/",
            "com.canonical.Unity.LauncherEntry",
            "Update",
            &(format!("application://{}.desktop", crate::linux::APP_ID), hints),
        ));
    }

    /// What the user did with a notification.
    fn act(shared: &Shared, message: &zbus::Message) {
        let Ok((id, action)) = message.body().deserialize::<(u32, String)>() else { return };
        let Some(conversation) = conversation(shared, id) else { return };
        let action = match action.as_str() {
            "default" => Some(NotificationAction::Open(conversation)),
            "mark-read" => Some(NotificationAction::MarkRead(conversation)),
            _ => None,
        };
        send(action);
    }

    /// Plasma's inline reply arrives as its own targeted signal carrying
    /// the text, not as a third `ActionInvoked` argument (libnotificationmanager's
    /// `server_p.cpp` sendReplyText).
    fn replied(shared: &Shared, id: u32, text: String) {
        if text.trim().is_empty() {
            return;
        }
        let Some(conversation) = conversation(shared, id) else { return };
        let text = text.chars().take(MAX_REPLY_CHARS).collect::<String>();
        send(Some(NotificationAction::Reply(conversation, text)));
    }

    fn conversation(shared: &Shared, id: u32) -> Option<ConversationId> {
        // A daemon that restarted hands its old ids to other apps'
        // notifications; only what it assigned to us (and hasn't closed)
        // may answer.
        if !shared.ids.lock().unwrap_or_else(|e| e.into_inner()).values().any(|assigned| *assigned == id) {
            return None;
        }
        shared.conversations.lock().unwrap_or_else(|e| e.into_inner()).get(&id).cloned()
    }

    fn send(action: Option<NotificationAction>) {
        if let (Some(action), Some(actions)) = (action, ACTIONS.get()) {
            let _ = actions.unbounded_send(action);
        }
    }

    /// Forgets a notification that closed (dismissed, or taken by `remove`).
    fn forget(shared: &Shared, id: u32) {
        shared.conversations.lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
        shared.ids.lock().unwrap_or_else(|e| e.into_inner()).retain(|_, assigned| *assigned != id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn actions_go_to_the_account_that_posted() {
        let chat = ConversationId("g:Ab/c+d==".into());
        let posted = ConversationId(format!("0a1b{ACCOUNT_SEPARATOR}{}", chat.0));
        assert_eq!(route(NotificationAction::MarkRead(posted.clone())), (Some("0a1b".into()), NotificationAction::MarkRead(chat.clone())));
        assert_eq!(
            route(NotificationAction::Reply(posted, "hi".into())),
            (Some("0a1b".into()), NotificationAction::Reply(chat.clone(), "hi".into()))
        );
        // From before there were several accounts.
        assert_eq!(route(NotificationAction::Open(chat.clone())), (None, NotificationAction::Open(chat)));
    }

    #[test]
    fn notification_text_follows_the_setting() {
        let text = |s: NotificationSetting| s.text("Alice", Some("Hikers"), "See you at 9");
        assert_eq!(text(NotificationSetting::NameAndMessage), ("Alice in Hikers".into(), "See you at 9".into()));
        assert_eq!(text(NotificationSetting::NameOnly), ("Alice in Hikers".into(), "New Message".into()));
        assert_eq!(text(NotificationSetting::NoNameOrMessage), ("Signal".into(), "New Message".into()));
        for s in [NotificationSetting::NameOnly, NotificationSetting::NoNameOrMessage, NotificationSetting::Off] {
            assert_eq!(NotificationSetting::from_setting(Some(s.as_setting())), s);
        }
    }
}
