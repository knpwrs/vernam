//! The photos Signal Desktop's editor keeps for reuse (`userAvatarData`,
//! `AvatarDataType` in `ts/types/Avatar.std.ts`): its presets, recolored or
//! not, text photos, and uploaded photos. Desktop keeps a list on each
//! conversation (`conversation.avatars`), starting from the presets
//! (`getDefaultAvatars`): ours for the profile editor, a group's for that
//! group, and a new group's in the composer until it's made.
//!
//! Here a list is a settings row of JSON per conversation, `avatar_data.me`
//! for ours and `avatar_data.<conversation id>` for a group's (no
//! migration), and an uploaded photo is a file in the account's
//! `avatar-data` directory, named at random (Desktop's `writeNewAvatarData`).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::avatar_presets::{GROUP_PRESETS, PRESETS};

/// Desktop's avatar color names, in the order of the app's avatar colors
/// (A100 is 0).
pub const COLOR_NAMES: [&str; 12] = ["A100", "A110", "A120", "A130", "A140", "A150", "A160", "A170", "A180", "A190", "A200", "A210"];

/// The settings row with our own list.
pub const OURS: &str = "avatar_data.me";

/// The settings row with a group's list.
pub fn group_key(conversation: &signal_store::ConversationId) -> String {
    format!("avatar_data.{}", conversation.0)
}

/// One photo in the list (Desktop's `AvatarDataType`): a preset (`icon`)
/// on `color`, `text` on `color`, or an uploaded photo (`image`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AvatarData {
    pub id: u64,
    /// Desktop's color name (`A100`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// The uploaded photo's file in the `avatar-data` directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
}

impl AvatarData {
    /// Its color as an index into the avatar colors (A100 when unnamed).
    pub fn color_index(&self) -> u8 {
        self.color.as_deref().and_then(|name| COLOR_NAMES.iter().position(|n| *n == name)).unwrap_or(0) as u8
    }

    /// Desktop's `BetterAvatar` offers to delete what isn't a preset.
    pub fn deletable(&self) -> bool {
        self.icon.is_none()
    }

    /// Desktop's `isSameAvatarData`: the same file, else the same id.
    pub fn same(&self, other: &AvatarData) -> bool {
        match (&self.image, &other.image) {
            (Some(a), Some(b)) => a == b,
            _ => self.id == other.id,
        }
    }

    /// Something that draws (a preset Vernam has, text, or a photo file
    /// named as Vernam names them), so a list read back can't name a path.
    fn valid(&self) -> bool {
        let file = self.image.as_deref().map(valid_file_name);
        let icon = self.icon.as_deref().map(|name| crate::avatar_presets::by_name(name).is_some());
        let text = self.text.as_deref().map(|text| !text.trim().is_empty());
        matches!((file, icon, text), (Some(true), None, None) | (None, Some(true), None) | (None, None, Some(true)))
    }
}

/// Desktop's `getDefaultAvatars`: its presets with their colors, ids from 0.
pub fn defaults(group: bool) -> Vec<AvatarData> {
    let presets = if group { &GROUP_PRESETS } else { &PRESETS };
    presets
        .iter()
        .enumerate()
        .map(|(ix, preset)| AvatarData {
            id: ix as u64,
            color: Some(COLOR_NAMES[preset.color as usize].into()),
            icon: Some(preset.name.into()),
            text: None,
            image: None,
        })
        .collect()
}

/// Desktop's `getNextAvatarId`: one past the largest.
pub fn next_id(list: &[AvatarData]) -> u64 {
    list.iter().map(|a| a.id + 1).max().unwrap_or(0)
}

/// Desktop's `replaceAvatar`: `new` first, taking `prev`'s place and id
/// when it replaces one, else a new id.
pub fn replace(list: &[AvatarData], mut new: AvatarData, prev: Option<&AvatarData>) -> Vec<AvatarData> {
    new.id = prev.map_or_else(|| next_id(list), |p| p.id);
    let mut out = vec![new];
    out.extend(list.iter().filter(|a| !prev.is_some_and(|p| p.same(a))).cloned());
    out
}

/// Desktop's `filterAvatarData`: the list without `gone`.
pub fn without(list: &[AvatarData], gone: &AvatarData) -> Vec<AvatarData> {
    list.iter().filter(|a| !gone.same(a)).cloned().collect()
}

/// The list read from `json`, or `None` when it can't be read; entries
/// that don't draw are left out.
pub fn parse(json: &str) -> Option<Vec<AvatarData>> {
    let list: Vec<AvatarData> = serde_json::from_str(json).ok()?;
    Some(list.into_iter().filter(AvatarData::valid).collect())
}

/// Where uploaded photos are kept, in the account's directory.
pub fn dir(data_dir: &Path) -> PathBuf {
    data_dir.join("avatar-data")
}

/// A name `write_image` gives: 32 hex digits and `.jpg`.
fn valid_file_name(name: &str) -> bool {
    name.strip_suffix(".jpg").is_some_and(|stem| stem.len() == 32 && stem.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// Saves an uploaded photo (already a processed JPEG) under a new random
/// name, and returns the name.
pub fn write_image(dir: &Path, jpeg: &[u8]) -> std::io::Result<String> {
    std::fs::create_dir_all(dir)?;
    let bytes: [u8; 16] = rand::random();
    let name = format!("{}.jpg", hex::encode(bytes));
    std::fs::write(dir.join(&name), jpeg)?;
    Ok(name)
}

/// Deletes an uploaded photo's file (Desktop's `deleteAvatar`); a name not
/// of `write_image`'s is left alone.
pub fn remove_image(dir: &Path, name: &str) {
    if !valid_file_name(name) {
        return;
    }
    if let Err(error) = std::fs::remove_file(dir.join(name)) {
        tracing::warn!(%error, "couldn't delete a saved avatar photo");
    }
}

/// The photo `data` draws (Desktop's `avatarDataToBytes`): a preset on its
/// color, text on its color, or the uploaded photo, as image bytes.
pub fn render(data: &AvatarData, dir: &Path) -> anyhow::Result<Vec<u8>> {
    let color = data.color_index();
    if let Some(name) = data.image.as_deref() {
        anyhow::ensure!(valid_file_name(name), "not a saved photo");
        return Ok(std::fs::read(dir.join(name))?);
    }
    if let Some(text) = data.text.as_deref() {
        let (bg, fg) = crate::theme::avatar_rgb(color);
        return crate::avatar_text::render(text, bg, fg);
    }
    let preset = data.icon.as_deref().and_then(crate::avatar_presets::by_name).ok_or_else(|| anyhow::anyhow!("no such preset"))?;
    crate::avatar_presets::render_on(preset, color)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(id: u64, text: &str) -> AvatarData {
        AvatarData { id, color: Some("A110".into()), icon: None, text: Some(text.into()), image: None }
    }

    #[test]
    fn the_defaults_are_desktops_presets_in_its_colors() {
        let people = defaults(false);
        assert_eq!(people.len(), 12);
        assert_eq!((people[0].icon.as_deref(), people[0].color.as_deref(), people[0].id), (Some("abstract_01"), Some("A130"), 0));
        let groups = defaults(true);
        assert_eq!((groups[0].icon.as_deref(), groups[0].color.as_deref()), (Some("balloon"), Some("A180")));
        assert_eq!(groups[11].id, 11);
        assert!(people.iter().chain(&groups).all(|a| !a.deletable() && a.valid()));
    }

    #[test]
    fn replacing_puts_the_new_one_first_as_desktop_does() {
        let list = defaults(true);
        // A new text photo: first, with the next id.
        let added = replace(&list, text(0, "AB"), None);
        assert_eq!((added[0].text.as_deref(), added[0].id, added.len()), (Some("AB"), 12, 13));
        // Editing it keeps its id and its place is taken.
        let edited = replace(&added, text(0, "CD"), Some(&added[0]));
        assert_eq!((edited[0].text.as_deref(), edited[0].id, edited.len()), (Some("CD"), 12, 13));
        // A recolored preset moves to the front with its id.
        let mut recolored = edited[5].clone();
        recolored.color = Some("A200".into());
        let moved = replace(&edited, recolored, Some(&edited[5]));
        assert_eq!((moved[0].id, moved[0].color.as_deref(), moved.len()), (edited[5].id, Some("A200"), 13));
        assert_eq!(without(&moved, &moved[0]).len(), 12);
    }

    #[test]
    fn a_list_read_back_keeps_only_what_draws() {
        let good = text(3, "AB");
        let path = AvatarData { id: 4, color: None, icon: None, text: None, image: Some("../../etc/passwd".into()) };
        let unknown = AvatarData { id: 5, color: None, icon: Some("nope".into()), text: None, image: None };
        let json = serde_json::to_string(&vec![good.clone(), path, unknown]).unwrap();
        assert_eq!(parse(&json), Some(vec![good]));
        assert_eq!(parse("not json"), None);
    }

    #[test]
    fn photos_are_saved_named_at_random_and_deleted() {
        let dir = tempfile::tempdir().unwrap();
        let dir = super::dir(dir.path());
        let name = write_image(&dir, b"\xff\xd8jpeg").unwrap();
        assert!(valid_file_name(&name));
        let photo = AvatarData { id: 1, color: None, icon: None, text: None, image: Some(name.clone()) };
        assert_eq!(render(&photo, &dir).unwrap(), b"\xff\xd8jpeg");
        assert_ne!(write_image(&dir, b"x").unwrap(), name, "each its own name");
        remove_image(&dir, &name);
        assert!(!dir.join(&name).exists());
        // Only names it gives.
        std::fs::write(dir.join("keep.txt"), b"x").unwrap();
        remove_image(&dir, "keep.txt");
        assert!(dir.join("keep.txt").exists());
    }

    #[test]
    fn each_kind_renders() {
        let dir = tempfile::tempdir().unwrap();
        let mut preset = defaults(false)[3].clone();
        preset.color = Some("A100".into());
        let png = render(&preset, dir.path()).unwrap();
        let corner = image::load_from_memory(&png).unwrap().to_rgba8().get_pixel(0, 0).0;
        assert_eq!(corner, [0xe3, 0xe3, 0xfe, 0xff], "recolored to A100");
        assert!(render(&text(1, "AB"), dir.path()).is_ok());
    }
}
