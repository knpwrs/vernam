//! Vernam: a native client for Signal, unofficial, after Signal Desktop.

mod accounts;
mod assets;
mod avatar_data;
mod avatar_presets;
mod avatar_text;
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
mod scheduled;
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
    let host = std::process::Command::new("scutil")
        .args(["--get", "ComputerName"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty());
    host.unwrap_or_else(|| "Vernam".into())
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

    // Links macOS opens us with (`sgnl://`, registered in the bundle's
    // Info.plist by scripts/bundle.sh), handed to the app once it runs.
    let (link_tx, mut links) = futures::channel::mpsc::unbounded::<String>();
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
