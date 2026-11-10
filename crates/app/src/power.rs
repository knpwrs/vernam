//! The machine waking from sleep (Desktop's `powerMonitor` `resume`,
//! `ts/main/powerChannel.main.ts`). Connections that were open when it
//! went to sleep may have died meanwhile without saying so.

/// Calls `f` each time the Mac wakes from sleep, for as long as the app
/// runs, on the thread that posts it (the main thread). Power Nap's brief dark wakes don't count, as
/// they don't for Electron's `resume`.
#[cfg(target_os = "macos")]
pub fn on_wake(f: impl Fn() + Send + 'static) {
    use block2::RcBlock;
    use objc2_app_kit::{NSWorkspace, NSWorkspaceDidWakeNotification};
    use objc2_foundation::NSNotification;

    let block = RcBlock::new(move |_: std::ptr::NonNull<NSNotification>| {
        tracing::info!("woke from sleep");
        f();
    });
    let center = NSWorkspace::sharedWorkspace().notificationCenter();
    // SAFETY: no object to match, no queue (the posting thread), and a
    // block that only calls `f`, which is `Send`.
    let observer = unsafe {
        center.addObserverForName_object_queue_usingBlock(Some(NSWorkspaceDidWakeNotification), None, None, &block)
    };
    // Observed until the app quits.
    std::mem::forget(observer);
}

/// Calls `f` each time the machine wakes from sleep, for as long as the
/// app runs (logind's `PrepareForSleep`, as it resumes).
#[cfg(target_os = "linux")]
pub fn on_wake(f: impl Fn() + Send + 'static) {
    use futures::StreamExt as _;
    let listen = std::thread::Builder::new().name("power".into()).spawn(move || {
        let Ok(system) = futures::executor::block_on(zbus::Connection::system()) else {
            tracing::info!("no system bus, wakes aren't seen");
            return;
        };
        let rule = zbus::MatchRule::builder()
            .msg_type(zbus::message::Type::Signal)
            .sender("org.freedesktop.login1")
            .interface("org.freedesktop.login1.Manager")
            .member("PrepareForSleep")
            .build()
            .expect("the rule's names are valid");
        // The stream ends when logind restarts: listen again, rather than
        // miss every wake from then on.
        let mut told = false;
        loop {
            let listening = futures::executor::block_on(async {
                let Ok(mut stream) = zbus::MessageStream::for_match_rule(rule.clone(), &system, None).await else {
                    return false;
                };
                while let Some(message) = stream.next().await {
                    let Ok(message) = message else { break };
                    // The argument says going to sleep (`true`) or waking (`false`).
                    if matches!(message.body().deserialize::<bool>(), Ok(false)) {
                        tracing::info!("woke from sleep");
                        f();
                    }
                }
                true
            });
            if listening {
                tracing::warn!("the logind stream stopped; listening again");
            } else if !told {
                tracing::info!("logind isn't answering, wakes aren't seen");
                told = true;
            }
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
    });
    if let Err(error) = listen {
        tracing::warn!("couldn't listen for wakes: {error}");
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub fn on_wake(_f: impl Fn() + Send + 'static) {}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use objc2_app_kit::{NSWorkspace, NSWorkspaceDidWakeNotification};

    #[test]
    fn a_wake_calls_back() {
        let woke = Arc::new(AtomicUsize::new(0));
        let counter = woke.clone();
        super::on_wake(move || {
            counter.fetch_add(1, Ordering::SeqCst);
        });
        let center = NSWorkspace::sharedWorkspace().notificationCenter();
        // What macOS posts on waking, posted here (to this process only).
        unsafe { center.postNotificationName_object(NSWorkspaceDidWakeNotification, None) };
        assert_eq!(woke.load(Ordering::SeqCst), 1);
    }
}
