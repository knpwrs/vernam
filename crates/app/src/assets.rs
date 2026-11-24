//! Icons: GPUI Kit's default set plus the Lucide icons this app uses.

use std::borrow::Cow;

use gpui_kit::{AssetSource, Result, SharedString};

gpui_kit::assets::icon_assets!(
    ExtraIcons,
    [
        Album,
        AppWindow,
        Archive,
        ArchiveRestore,
        BadgeCheck,
        ArrowDown,
        Ban,
        Bell,
        BellOff,
        Camera,
        ChevronLeft,
        ChevronRight,
        CircleUser,
        Hash,
        Hand,
        LayoutGrid,
        SquareUser,
        ChartBar,
        CheckCheck,
        CircleAlert,
        CircleDashed,
        Clock,
        Download,
        Ellipsis,
        EllipsisVertical,
        FaceSlightlySmiling,
        FaceSlightlySmilingPlus,
        File,
        FolderDown,
        Forward,
        Info,
        LoaderCircle,
        RotateCcw,
        Image,
        Link,
        ListFilter,
        Lock,
        LogOut,
        Maximize2,
        MessageCircle,
        Mic,
        MicOff,
        Minimize2,
        AtSign,
        Bold,
        Code,
        EyeOff,
        Italic,
        Strikethrough,
        MessageSquareCheck,
        MessageSquareDot,
        MessageSquareShare,
        NotebookText,
        Paperclip,
        Pause,
        Phone,
        PhoneIncoming,
        PhoneMissed,
        PhoneOff,
        PhoneOutgoing,
        Pencil,
        Pin,
        PinOff,
        Play,
        Reply,
        ScreenShare,
        ScreenShareOff,
        Send,
        Share2,
        SquareCheck,
        SquarePen,
        Timer,
        Trash,
        TriangleAlert,
        User,
        Video,
        VideoOff,
        Users,
        WifiOff,
        X
    ]
);

/// Signal Desktop's own icons (AGPL-3.0-only, like this app), where Lucide
/// has no match: the message status circles, the Signal chat's logo, the
/// navigation tabs' icons, and the hand on a photo's "View".
const SIGNAL_ICONS: &[(&str, &[u8])] = &[
    ("icons/signal/messagestatus-sending.svg", include_bytes!("../assets/icons/signal/messagestatus-sending.svg")),
    ("icons/signal/messagestatus-sent.svg", include_bytes!("../assets/icons/signal/messagestatus-sent.svg")),
    ("icons/signal/messagestatus-delivered.svg", include_bytes!("../assets/icons/signal/messagestatus-delivered.svg")),
    ("icons/signal/messagestatus-read.svg", include_bytes!("../assets/icons/signal/messagestatus-read.svg")),
    ("icons/signal/error-circle.svg", include_bytes!("../assets/icons/signal/error-circle.svg")),
    ("icons/signal/profile-avatar.svg", include_bytes!("../assets/icons/signal/profile-avatar.svg")),
    ("icons/signal/click-outline-24.svg", include_bytes!("../assets/icons/signal/click-outline-24.svg")),
    ("icons/signal/nav-chat.svg", include_bytes!("../assets/icons/signal/nav-chat.svg")),
    ("icons/signal/nav-chat-fill.svg", include_bytes!("../assets/icons/signal/nav-chat-fill.svg")),
    ("icons/signal/nav-phone.svg", include_bytes!("../assets/icons/signal/nav-phone.svg")),
    ("icons/signal/nav-phone-fill.svg", include_bytes!("../assets/icons/signal/nav-phone-fill.svg")),
    ("icons/signal/nav-stories.svg", include_bytes!("../assets/icons/signal/nav-stories.svg")),
    ("icons/signal/nav-stories-fill.svg", include_bytes!("../assets/icons/signal/nav-stories-fill.svg")),
    ("icons/signal/nav-settings.svg", include_bytes!("../assets/icons/signal/nav-settings.svg")),
    ("icons/signal/nav-settings-fill.svg", include_bytes!("../assets/icons/signal/nav-settings-fill.svg")),
    ("icons/signal/nav-plus.svg", include_bytes!("../assets/icons/signal/nav-plus.svg")),
    ("icons/signal/nav-menu.svg", include_bytes!("../assets/icons/signal/nav-menu.svg")),
];

pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if let Some((_, bytes)) = SIGNAL_ICONS.iter().find(|(name, _)| *name == path) {
            return Ok(Some(Cow::Borrowed(bytes)));
        }
        if let Some(bytes) = ExtraIcons.load(path)? {
            return Ok(Some(bytes));
        }
        gpui_kit::assets::Assets.load(path)
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut paths = gpui_kit::assets::Assets.list(path)?;
        paths.extend(ExtraIcons.list(path)?);
        paths.extend(SIGNAL_ICONS.iter().map(|(name, _)| *name).filter(|name| name.starts_with(path)).map(Into::into));
        paths.sort();
        paths.dedup();
        Ok(paths)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::assets::IconName;

    /// Every `IconName::X` in the app's code (comments aside) loads. GPUI
    /// Kit's default set holds only its own components' icons, so one
    /// missing from `ExtraIcons` draws nothing and logs "could not find
    /// asset". Signal's own icons, whose paths are built at run time, aren't
    /// checked here.
    #[test]
    fn every_icon_name_the_app_uses_is_embedded() {
        let mut source = String::new();
        let mut dirs = vec![std::path::PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/src"))];
        while let Some(dir) = dirs.pop() {
            for entry in std::fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    dirs.push(path);
                } else if path.extension().is_some_and(|ext| ext == "rs") {
                    let text = std::fs::read_to_string(&path).unwrap();
                    for line in text.lines().filter(|line| !line.trim_start().starts_with("//")) {
                        source.push_str(line);
                        source.push('\n');
                    }
                }
            }
        }
        let named = |name: &str| {
            source.match_indices(&format!("IconName::{name}")).any(|(at, found)| {
                !source[at + found.len()..].starts_with(|c: char| c.is_ascii_alphanumeric() || c == '_')
            })
        };
        let missing: Vec<String> = IconName::ALL
            .iter()
            .filter(|icon| named(&format!("{icon:?}")) && !matches!(Assets.load(&icon.path()), Ok(Some(_))))
            .map(|icon| format!("{icon:?}"))
            .collect();
        assert!(missing.is_empty(), "add these to ExtraIcons: {missing:?}");
    }
}
