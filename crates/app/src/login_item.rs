//! "Open at computer login" (Desktop's `autoLaunch`, which asks Electron's
//! `app.setLoginItemSettings`): on macOS the app registers itself as a
//! login item through `SMAppService.mainAppService` (macOS 13 and later),
//! which only works from an app bundle. Like Desktop, the setting isn't
//! stored: it's whatever the system says.

/// Turns the login item on and off and reads it back; tests swap in a
/// stand-in so nothing is registered with the system.
#[derive(Clone, Copy)]
pub struct LoginItem {
    pub is_enabled: fn() -> bool,
    pub set: fn(bool) -> Result<(), String>,
}

impl Default for LoginItem {
    fn default() -> Self {
        #[cfg(test)]
        return Self { is_enabled: || false, set: |_| Ok(()) };
        #[cfg(not(test))]
        Self { is_enabled: system::is_enabled, set: system::set }
    }
}

#[cfg(all(target_os = "macos", not(test)))]
mod system {
    use objc2::rc::Retained;
    use objc2::runtime::{AnyClass, AnyObject};
    use objc2::msg_send;
    use objc2_foundation::{NSBundle, NSError};

    #[link(name = "ServiceManagement", kind = "framework")]
    unsafe extern "C" {}

    /// `SMAppServiceStatusEnabled`.
    const ENABLED: isize = 1;

    fn service() -> Option<Retained<AnyObject>> {
        let class = AnyClass::get(c"SMAppService")?;
        unsafe { msg_send![class, mainAppService] }
    }

    pub fn is_enabled() -> bool {
        service().is_some_and(|service| {
            let status: isize = unsafe { msg_send![&*service, status] };
            status == ENABLED
        })
    }

    pub fn set(on: bool) -> Result<(), String> {
        if NSBundle::mainBundle().bundleIdentifier().is_none() {
            return Err("Vernam can only open at login when run as an app.".into());
        }
        let service = service().ok_or("Opening at login needs macOS 13 or later.")?;
        let result: Result<(), Retained<NSError>> = unsafe {
            if on { msg_send![&*service, registerAndReturnError: _] } else { msg_send![&*service, unregisterAndReturnError: _] }
        };
        result.map_err(|error| error.localizedDescription().to_string())
    }
}

/// Open at login is a file in `~/.config/autostart`, per the freedesktop
/// autostart standard (what GNOME, KDE and friends read).
#[cfg(all(target_os = "linux", not(test)))]
mod system {
    use std::path::PathBuf;

    /// The autostart entry, named for the app like the desktop file that
    /// registers it (`main.rs`).
    fn file() -> Option<PathBuf> {
        let config = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?;
        Some(config.join("autostart").join("vernam.desktop"))
    }

    /// The entry is the setting: whatever the system says, as on macOS.
    pub fn is_enabled() -> bool {
        file().map(|file| file.exists()).unwrap_or(false)
    }

    pub fn set(on: bool) -> Result<(), String> {
        // Never from a development build: its Exec would point at
        // `target/debug` (as `register_desktop`'s entry would in main.rs).
        if on && cfg!(debug_assertions) {
            return Err("Vernam doesn't open at login from a development build.".into());
        }
        let Some(file) = file() else {
            return Err("Vernam doesn't know where your configuration lives.".into());
        };
        if !on {
            return match std::fs::remove_file(&file) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(format!("couldn't remove {file:?}: {error}")),
            };
        }
        if let Some(directory) = file.parent() {
            std::fs::create_dir_all(directory).map_err(|error| format!("couldn't create {directory:?}: {error}"))?;
        }
        let exe = std::env::current_exe().map_err(|_| "Vernam doesn't know where it runs from.".to_string())?;
        let entry = format!("[Desktop Entry]\nType=Application\nName=Vernam\nExec=\"{}\"\n", exe.display());
        std::fs::write(&file, entry).map_err(|error| format!("couldn't write {file:?}: {error}"))
    }
}

#[cfg(all(not(any(target_os = "macos", target_os = "linux")), not(test)))]
mod system {
    pub fn is_enabled() -> bool {
        false
    }

    pub fn set(_on: bool) -> Result<(), String> {
        Err("Opening at login isn't available here.".into())
    }
}
