//! Vernam: a native client for Signal, unofficial, after Signal Desktop.

mod accounts;
mod assets;
mod avatar_presets;
mod chat_color;
mod audio;
mod emoji_usage;
mod fonts;
mod former_name;
mod formatting;
mod image_cache;
mod keys;
mod login_item;
mod main_window;
mod menus;
mod loop_cache;
mod notifications;
mod platform;
mod power;
mod prefs;
mod recorder;
mod reminders;
mod rich_input;
mod shortcuts;
mod sidebar_width;
mod sounds;
mod spelling;
mod ringtone;
mod state;
mod sticker_image;
mod theme;
mod video;
mod video_player;
mod views;
mod window_state;

use std::path::PathBuf;

use anyhow::{Context as _, Result};
use futures::StreamExt as _;
use gpui_kit::component::Theme;
use gpui_kit::{App, KeyBinding};


gpui_kit::actions!(signal, [Quit, About, OpenSettings, ShowDebugLog, FocusSearch, SearchInChat, NextConversation, PreviousConversation, CreateStickerPack, AddAccount]);

/// Shows the app's account at this index (Ctrl-Cmd-1 to 9).
#[derive(Clone, PartialEq, gpui_kit::Action)]
#[action(namespace = signal, no_json)]
pub struct SwitchAccount(pub usize);

/// `$SIGNAL_GPUI_DATA_DIR` (a development profile, keyed by a file), or the
/// platform's application data directory (keyed by the Keychain), where
/// the data of a former name (Ziggy, Signal GPUI) moves on first launch.
/// Each account's data is in a directory of its own in it (`accounts`).
fn data_dir() -> Result<(PathBuf, keys::KeyStore, former_name::Moved)> {
    if let Some(dir) = std::env::var_os("SIGNAL_GPUI_DATA_DIR") {
        return Ok((PathBuf::from(dir), keys::KeyStore::File, former_name::Moved::Nothing));
    }
    let dir = directories::ProjectDirs::from("io.github", "knpwrs", "Vernam")
        .map(|dirs| dirs.data_dir().to_path_buf())
        .context("no home directory")?;
    let moved = former_name::move_data(&dir)?;
    Ok((dir, keys::KeyStore::System, moved))
}

/// Another Vernam process is running (not this one).
fn other_vernam_running() -> bool {
    let Ok(output) = std::process::Command::new("pgrep").args(["-x", "vernam"]).output() else { return false };
    let me = std::process::id().to_string();
    String::from_utf8_lossy(&output.stdout).lines().any(|pid| pid.trim() != me)
}

fn device_name() -> String {
    #[cfg(target_os = "macos")]
    {
        let host = std::process::Command::new("scutil")
            .args(["--get", "ComputerName"])
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty());
        host.unwrap_or_else(|| "Vernam".into())
    }
    #[cfg(target_os = "linux")]
    {
        linux::device_name().unwrap_or_else(|| "Vernam".into())
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        "Vernam".into()
    }
}

/// Linux's pieces of what macOS gets from its bundle: the machine's name,
/// one Vernam at a time with its links, and a place on the desktop.
#[cfg(target_os = "linux")]
mod linux {
    use std::io::{BufRead as _, Read as _, Write as _};
    use std::os::unix::ffi::OsStrExt as _;
    use std::os::unix::fs::PermissionsExt as _;
    use std::os::unix::io::AsRawFd as _;
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::PathBuf;

    use futures::channel::mpsc;

    /// The window's app id (`main_window`), what wayland's shells and X11's
    /// WM_CLASS name, and the icon and desktop file are named for. The
    /// notifier names it too, as the desktop-entry hint and Unity's badge
    /// target.
    pub(crate) const APP_ID: &str = "io.github.knpwrs.vernam";

    /// The machine's name, as the user sees it: logind's pretty name if it
    /// has one, else `/etc/hostname` (`scutil --get ComputerName`).
    pub fn device_name() -> Option<String> {
        let pretty = std::fs::read_to_string("/etc/machine-info").ok().and_then(|info| {
            info.lines()
                .find_map(|line| line.strip_prefix("PRETTY_HOSTNAME="))
                .map(|name| name.trim_matches('"').trim().to_owned())
                .filter(|name| !name.is_empty())
        });
        pretty.or_else(|| {
            std::fs::read_to_string("/etc/hostname")
                .ok()
                .map(|host| host.trim().to_owned())
                .filter(|host| !host.is_empty())
        })
    }

    /// The socket the running Vernam reads `sgnl://` links on, one per
    /// user and data directory, in the directory: two Vernams with
    /// different data directories (a development profile beside the real
    /// account) run side by side, as they do on macOS. In the directory,
    /// not `/run/user`, so the hand-off crosses mount namespaces: a
    /// firejail'd instance shares only the whitelisted directory with
    /// the unjailed launcher a desktop link runs. The uid in the name
    /// keeps the path distinct when a directory is ever shared between
    /// users.
    pub(crate) fn socket_path(data_dir: &std::path::Path) -> Option<PathBuf> {
        // FNV-1a, not DefaultHasher: the name must match across builds (an
        // update relaunching beside the old instance), and std doesn't
        // promise to keep DefaultHasher's algorithm.
        let mut hash: u64 = 0xcbf29ce484222325;
        for byte in data_dir.as_os_str().as_bytes() {
            hash = (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3);
        }
        Some(data_dir.join(format!("vernam-{}-{hash:016x}.links", unsafe { libc::getuid() })))
    }

    /// Hands `sgnl://` links to the Vernam of the same data directory
    /// that's already running, and returns true if there was one (this
    /// launch then quits, as Desktop's `requestSingleInstanceLock` ends a
    /// second instance). Otherwise this one listens for later launches'
    /// links, feeding `tx`, and returns false.
    pub fn hand_off(data_dir: &std::path::Path, links: &[String], tx: mpsc::UnboundedSender<String>) -> bool {
        match socket_path(data_dir).map(|socket| hand_off_at(&socket, links, tx)) {
            Some(handed) => handed,
            None => false,
        }
    }

    pub(crate) fn hand_off_at(
        socket: &std::path::Path,
        links: &[String],
        tx: mpsc::UnboundedSender<String>,
    ) -> bool {
        let uid = unsafe { libc::getuid() };
        // Only a listener of this user takes the links: the data
        // directory is wherever the user pointed the app at, and the
        // links carry linking secrets.
        let take = |stream: UnixStream| peer_uid(&stream) == Some(uid) && send_links(stream, links);
        if let Ok(stream) = UnixStream::connect(socket) {
            if take(stream) {
                return true;
            }
            // It went away between connecting and writing (or isn't ours to
            // write to); carry on here.
        }
        match UnixListener::bind(socket) {
            Ok(listener) => listen_for_links(socket, listener, tx),
            Err(first_error) => {
                // The path is taken: either a Vernam bound it just now
                // (simultaneous launches) or a crash left its socket
                // behind. Wait for whoever bound it to listen, then hand
                // off to it.
                for _ in 0..5 {
                    std::thread::sleep(std::time::Duration::from_millis(100));
                    match UnixStream::connect(socket) {
                        Ok(stream) => {
                            if take(stream) {
                                return true;
                            }
                            // It answered but died mid-hand-off (or isn't
                            // ours); its path is free now.
                            break;
                        }
                        // Nothing listens behind the path: whoever bound it
                        // never got to listening (a bind is followed by a
                        // listen within microseconds, and this retry began
                        // a hundred milliseconds after it). Stale, and the
                        // way in.
                        Err(error) if error.kind() == std::io::ErrorKind::ConnectionRefused => break,
                        // The path isn't there yet (a launch still starting):
                        // wait for it.
                        Err(_) => {}
                    }
                }
                // Nothing answered: the socket is stale, and the way in.
                let _ = std::fs::remove_file(socket);
                match UnixListener::bind(socket) {
                    Ok(listener) => listen_for_links(socket, listener, tx),
                    Err(error) => {
                        // One data directory can't have two instances (the
                        // store queues its writes for one connection), so
                        // this launch refuses rather than become a second
                        // one.
                        tracing::error!(
                            "can't listen for sgnl:// links on {}: {error} (first try: {first_error}); is another Vernam starting?",
                            socket.display()
                        );
                        std::process::exit(1);
                    }
                }
            }
        }
    }

    fn listen_for_links(socket: &std::path::Path, listener: UnixListener, tx: mpsc::UnboundedSender<String>) -> bool {
        // The socket can live in a world-readable directory (the temp_dir
        // fallback), so keep it private and check who connects: only this
        // user's launches hand over links.
        let _ = std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o600));
        tracing::info!("listening for sgnl:// links on {}", socket.display());
        let uid = unsafe { libc::getuid() };
        let _ = std::thread::Builder::new().name("links".into()).spawn(move || {
            for stream in listener.incoming().flatten() {
                if peer_uid(&stream) != Some(uid) {
                    tracing::warn!("ignored a link hand-off from another user's process");
                    continue;
                }
                // One thread per launch, so a stalled client can't hold up
                // the ones after it.
                let tx = tx.clone();
                let _ = std::thread::Builder::new().name("link".into()).spawn(move || {
                    // Links are short; the cap only bounds what one client
                    // can make the reader hold.
                    for link in std::io::BufReader::new(stream.take(64 * 1024)).lines().map_while(Result::ok) {
                        if link.starts_with("sgnl:") {
                            tracing::info!("a link came in from another launch");
                            let _ = tx.unbounded_send(link);
                        }
                    }
                });
            }
        });
        false
    }

    fn send_links(mut stream: UnixStream, links: &[String]) -> bool {
        for link in links {
            if writeln!(stream, "{link}").is_err() {
                return false;
            }
        }
        true
    }

    /// The connecting process's user, via SO_PEERCRED.
    pub(crate) fn peer_uid(stream: &UnixStream) -> Option<u32> {
        let mut credentials = libc::ucred { pid: 0, uid: u32::MAX, gid: u32::MAX };
        let mut length = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
        let ok = unsafe {
            libc::getsockopt(
                stream.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                (&mut credentials as *mut libc::ucred).cast(),
                &mut length,
            )
        } == 0;
        ok.then_some(credentials.uid)
    }

    /// Registers Vernam with the desktop, as `sgnl://` handler and with an
    /// icon: what the macOS bundle's Info.plist and its icon do, what a
    /// packaged install would ship. Best effort, written every launch so
    /// an update takes.
    pub fn register_desktop() {
        // Never from a development build: its Exec would point at
        // `target/debug`, taking the installed app's sgnl:// links (and
        // autostart, through the desktop entry) away from it.
        if cfg!(debug_assertions) {
            tracing::info!("a development build registers no desktop entry");
            return;
        }
        let Ok(exe) = std::env::current_exe() else { return };
        let Some(data_dir) = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")))
        else {
            return;
        };
        let entry = data_dir.join("applications").join(format!("{APP_ID}.desktop"));
        let desktop = format!(
            "[Desktop Entry]\nType=Application\nName=Vernam\nComment=Unofficial Signal client\nExec=\"{}\" %U\n\
             StartupWMClass={APP_ID}\nIcon={APP_ID}\nMimeType=x-scheme-handler/sgnl;\nTerminal=false\n\
             Categories=Network;InstantMessaging;\n",
            exe.display()
        );
        if std::fs::create_dir_all(entry.parent().expect("a file in a directory"))
            .and_then(|_| std::fs::write(&entry, desktop))
            .is_err()
        {
            tracing::warn!("couldn't write {}", entry.display());
            return;
        }
        for (size, bytes) in [
            ("16x16", include_bytes!("../assets/app-icon/16x16.png") as &[u8]),
            ("24x24", include_bytes!("../assets/app-icon/24x24.png")),
            ("32x32", include_bytes!("../assets/app-icon/32x32.png")),
            ("48x48", include_bytes!("../assets/app-icon/48x48.png")),
            ("64x64", include_bytes!("../assets/app-icon/64x64.png")),
            ("256x256", include_bytes!("../assets/app-icon/256x256.png")),
            ("512x512", include_bytes!("../assets/app-icon/512x512.png")),
        ] {
            let icon = data_dir.join("icons/hicolor").join(size).join("apps").join(format!("{APP_ID}.png"));
            if std::fs::create_dir_all(icon.parent().expect("a file in a directory"))
                .and_then(|_| std::fs::write(&icon, bytes))
                .is_err()
            {
                tracing::warn!("couldn't write {}", icon.display());
            }
        }
        // `xdg-mime` merges the handler into the user's mimeapps.list;
        // without it (or xdg-utils), the desktop file still registers.
        let _ = std::process::Command::new("xdg-mime")
            .args(["default", &format!("{APP_ID}.desktop"), "x-scheme-handler/sgnl"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
}

#[cfg(all(test, target_os = "linux"))]
mod linux_tests {
    use super::linux;
    use futures::StreamExt as _;

    /// The second launch hands its links to the one listening.
    #[test]
    fn a_second_launch_hands_its_links_over() {
        let socket = std::env::temp_dir().join(format!("vernam-links-test-{}", std::process::id()));
        let (tx, mut rx) = futures::channel::mpsc::unbounded();
        assert!(!linux::hand_off_at(&socket, &[], tx), "the first launch listens");
        let (tx2, _) = futures::channel::mpsc::unbounded();
        let link = "sgnl://link.signal.me/#/settings".to_owned();
        assert!(linux::hand_off_at(&socket, &[link.clone()], tx2), "the second hands over");
        assert_eq!(futures::executor::block_on(rx.next()), Some(link));
        let _ = std::fs::remove_file(&socket);
    }

    /// A connection carries the connecting process's user, which is what
    /// the hand-off listener checks before reading its links.
    #[test]
    fn a_connection_carries_the_connecting_processs_user() {
        use std::os::unix::net::UnixStream;
        let (_ours, theirs) = UnixStream::pair().unwrap();
        assert_eq!(linux::peer_uid(&theirs), Some(unsafe { libc::getuid() }));
    }

    /// A socket left by a crash doesn't block: it's cleared and this
    /// launch listens in its place, and the next launch hands over.
    #[test]
    fn a_stale_socket_does_not_block_the_launch() {
        use std::os::unix::net::UnixListener;
        let socket = std::env::temp_dir().join(format!("vernam-links-test-stale-{}", std::process::id()));
        drop(UnixListener::bind(&socket).unwrap());
        let (tx, mut rx) = futures::channel::mpsc::unbounded();
        assert!(!linux::hand_off_at(&socket, &[], tx), "the launch listens on the cleared path");
        let (tx2, _) = futures::channel::mpsc::unbounded();
        let link = "sgnl://link.signal.me/#/settings".to_owned();
        assert!(linux::hand_off_at(&socket, &[link.clone()], tx2), "the next launch hands over");
        assert_eq!(futures::executor::block_on(rx.next()), Some(link));
        let _ = std::fs::remove_file(&socket);
    }

    /// The hand-off socket is per data directory, not per user: a
    /// development profile beside the real account runs side by side.
    #[test]
    fn the_socket_is_per_data_directory() {
        let one = linux::socket_path(std::path::Path::new("/tmp/one")).unwrap();
        let one_again = linux::socket_path(std::path::Path::new("/tmp/one")).unwrap();
        let two = linux::socket_path(std::path::Path::new("/tmp/two")).unwrap();
        assert_eq!(one, one_again, "a data directory keeps its socket");
        assert_ne!(one, two, "different data directories don't hand off to each other");
        assert!(one.starts_with("/tmp/one"), "the socket lives in its data directory");
    }
}

/// How many days of logs are kept.
const LOG_DAYS: usize = 3;

/// Logs to stderr and to daily files in `logs/` (three days kept), which
/// Help → Debug Log collects. Returns the guard that flushes the file writer.
fn init_logging(data_dir: &std::path::Path) -> Option<tracing_appender::non_blocking::WorkerGuard> {
    use tracing_subscriber::Layer as _;
    use tracing_subscriber::layer::SubscriberExt as _;
    use tracing_subscriber::util::SubscriberInitExt as _;

    let filter = || {
        tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| "info,presage=info,libsignal_service=warn,sqlx=warn".into())
    };
    let files = tracing_appender::rolling::Builder::new()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .filename_prefix("vernam")
        .filename_suffix("log")
        .max_log_files(LOG_DAYS)
        .build(signal_backend::debug_log::log_dir(data_dir));
    former_name::prune_former_logs(&signal_backend::debug_log::log_dir(data_dir), LOG_DAYS);
    let (file_layer, guard) = match files {
        Ok(files) => {
            let (writer, guard) = tracing_appender::non_blocking(files);
            let layer = tracing_subscriber::fmt::layer().with_ansi(false).with_writer(writer);
            (Some(layer), Some(guard))
        }
        Err(error) => {
            eprintln!("can't write log files: {error}");
            (None, None)
        }
    };
    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer().with_filter(filter()))
        .with(file_layer.map(|layer| layer.with_filter(filter())))
        .init();

    // Crashes belong in the debug log too.
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        tracing::error!("panic: {info}\n{}", std::backtrace::Backtrace::force_capture());
        default_hook(info);
    }));
    guard
}

fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("cmd-q", Quit, None),
        KeyBinding::new("cmd-,", OpenSettings, Some("Signal")),
        KeyBinding::new("cmd-f", FocusSearch, Some("Signal")),
        KeyBinding::new("cmd-shift-f", SearchInChat, Some("Signal")),
        KeyBinding::new("alt-down", NextConversation, Some("Signal")),
        KeyBinding::new("alt-up", PreviousConversation, Some("Signal")),
        KeyBinding::new("ctrl-tab", NextConversation, Some("Signal")),
        KeyBinding::new("ctrl-shift-tab", PreviousConversation, Some("Signal")),
        // Desktop's formatting shortcuts.
        KeyBinding::new("cmd-b", views::composer::FormatBold, Some("Composer")),
        KeyBinding::new("cmd-i", views::composer::FormatItalic, Some("Composer")),
        KeyBinding::new("cmd-shift-x", views::composer::FormatStrikethrough, Some("Composer")),
        KeyBinding::new("cmd-e", views::composer::FormatMonospace, Some("Composer")),
        KeyBinding::new("cmd-shift-b", views::composer::FormatSpoiler, Some("Composer")),
        KeyBinding::new("cmd-shift-y", views::composer::RecordVoice, Some("Composer")),
    ]);
    // The app's accounts (Cmd-1 to 9 are Desktop's, for chats).
    // KeyBinding::new takes a 'static str.
    const SWITCH: [&str; 9] =
        ["ctrl-cmd-1", "ctrl-cmd-2", "ctrl-cmd-3", "ctrl-cmd-4", "ctrl-cmd-5", "ctrl-cmd-6", "ctrl-cmd-7", "ctrl-cmd-8", "ctrl-cmd-9"];
    cx.bind_keys(SWITCH.into_iter().enumerate().map(|(n, keys)| KeyBinding::new(keys, SwitchAccount(n), None)));

    menus::bind_keys(cx);
    shortcuts::bind_keys(cx);
}

fn main() -> Result<()> {
    let (data_dir, key_store, moved) = data_dir()?;
    std::fs::create_dir_all(&data_dir).with_context(|| format!("creating {}", data_dir.display()))?;
    let _log_guard = init_logging(&data_dir);
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "starting");

    // Links the system opens us with (`sgnl://`: registered in the macOS
    // bundle's Info.plist by scripts/bundle.sh, and on Linux in the desktop
    // file `register_desktop` writes), handed to the app once it runs.
    let (link_tx, mut links) = futures::channel::mpsc::unbounded::<String>();
    // Linux: one Vernam at a time, and a place on the desktop.
    #[cfg(target_os = "linux")]
    {
        let from_argv = std::env::args().skip(1).filter(|arg| arg.starts_with("sgnl:")).collect::<Vec<_>>();
        if linux::hand_off(&data_dir, &from_argv, link_tx.clone()) {
            return Ok(());
        }
        for link in from_argv {
            let _ = link_tx.unbounded_send(link);
        }
        linux::register_desktop();
    }

    // From an earlier launch that moved the data but didn't get its key.
    let mut moved_from = match key_store {
        keys::KeyStore::System => former_name::moved_from(&data_dir),
        keys::KeyStore::File => None,
    };
    match moved {
        former_name::Moved::From(former, dir) => {
            tracing::info!("moved {}'s data here from {}", former.name, dir.display());
            moved_from = Some(former);
        }
        former_name::Moved::LeftIn(former, dir) => {
            tracing::warn!("{}'s data is still in {}: this directory existed already", former.name, dir.display())
        }
        // (`StillRunning` doesn't get here: `move_data` stops the launch.)
        former_name::Moved::Nothing | former_name::Moved::StillRunning(_) => {}
    }

    // Parse the emoji index off the main thread while the window opens.
    std::thread::spawn(signal_emoji::warm_up);

    let window_dir = data_dir.clone();
    // The real data directory's account moves into `accounts/` once, which
    // can't be done under a Vernam still running with it (a build from
    // before there were several). Development profiles aren't checked.
    let refused = std::cell::Cell::new(false);
    let running = || {
        refused.set(key_store == keys::KeyStore::System && other_vernam_running());
        refused.get()
    };
    let registry = accounts::adopt_layout(&data_dir, running).inspect_err(|_| {
        if !refused.get() {
            return;
        }
        former_name::alert(
            "Quit Vernam to open this version",
            "This version keeps each account's data in a folder of its own and moves your chats there the first time it \
             opens, which it can't do while another Vernam is running. Quit it, then open Vernam again.",
        )
    })?;
    let launch = accounts::Launch {
        root: data_dir,
        key_store,
        device_name: device_name(),
        demo: std::env::var_os("SIGNAL_GPUI_DEMO").is_some(),
        call_lines: Default::default(),
    };

    let app = gpui_kit::application().with_assets(assets::Assets);
    app.on_open_urls(move |urls| {
        for url in urls {
            let _ = link_tx.unbounded_send(url);
        }
    });
    // Desktop's `activate`: the Dock icon brings the window back.
    app.on_reopen(main_window::show);
    app.run(move |cx| {
        gpui_kit::init(cx);
        fonts::register(cx);
        Theme::sync_system_appearance(None, cx);
        bind_keys(cx);
        menus::install(cx);

        let accounts = match accounts::Accounts::start(launch, registry, moved_from, cx) {
            Ok(accounts) => accounts,
            Err(error) => {
                tracing::error!("failed to open the accounts: {error:#}");
                cx.quit();
                return;
            }
        };
        let running = accounts.read(cx).running();
        power::on_wake(move || {
            for backend in running.lock().unwrap_or_else(|e| e.into_inner()).iter() {
                backend.send(signal_backend::Command::Resumed);
            }
        });
        cx.on_action(|_: &Quit, cx| main_window::quit(cx));
        // However the app quits (the menu, the Dock, logging out), the
        // databases close first.
        let quit_accounts = accounts.clone();
        cx.on_app_quit(move |cx| {
            quit_accounts.read(cx).shut_down(cx);
            async {}
        })
        .detach();
        let add_accounts = accounts.clone();
        cx.on_action(move |_: &AddAccount, cx| {
            main_window::show(cx);
            add_accounts.update(cx, |accounts, cx| accounts.add_account(cx));
        });
        let switch_accounts = accounts.clone();
        cx.on_action(move |switch: &SwitchAccount, cx| switch_accounts.update(cx, |accounts, cx| accounts.activate(switch.0, cx)));

        // Desktop's File → Create/upload sticker pack: its own window. It
        // uploads as the account shown, so only once that one is linked.
        cx.on_action(move |_: &CreateStickerPack, cx| {
            let Some(creator_state) = accounts::active_state(cx) else { return };
            if creator_state.read(cx).account.is_some() {
                views::sticker_creator::open(creator_state.clone(), cx);
            } else if let Some(window) = cx.windows().first() {
                // Desktop's warning (`openArtCreator`, app/main.main.ts).
                use gpui_kit::component::WindowExt as _;
                use gpui_kit::component::notification::Notification;
                let _ = window.update(cx, |_, window, cx| {
                    window.push_notification(
                        Notification::warning("Please set up Signal on your phone and desktop to use the Sticker Pack Creator"),
                        cx,
                    )
                });
            }
        });

        // Desktop's `open-url` (app/main.main.ts): the window comes forward
        // and the link opens, in the account it shows.
        cx.spawn(async move |cx| {
            while let Some(url) = links.next().await {
                cx.update(|cx| {
                    main_window::show(cx);
                    if let Some(state) = accounts::active_state(cx) {
                        state.update(cx, |state, cx| state.open_link_from_system(url, cx));
                    }
                });
            }
        })
        .detach();

        // Opens without taking focus from whatever you're typing in (for
        // screenshots and scripted runs).
        let background = std::env::var_os("SIGNAL_GPUI_BACKGROUND").is_some();
        if let Err(e) = main_window::open(accounts, window_dir.clone(), background, cx) {
            tracing::error!("failed to open window: {e:#}");
            cx.quit();
        }
        if !background {
            cx.activate(true);
        }
    });
    Ok(())
}
