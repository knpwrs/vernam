//! Hidden sticker packs, as Signal Desktop keeps them (`hidden_sticker_packs`
//! in `ts/sql/server/stickers.std.ts`, merged by
//! `mergeHiddenStickerPackRecord` in `ts/services/storageRecordOps.preload.ts`,
//! Desktop commit 5edf85d): a pack received in a sticker, hidden from the
//! sticker manager's "Shared With You", with its key, or when it was
//! unhidden (a tombstone kept for Storage Service). Installing a pack
//! unhides it. They live in one `settings` row as JSON rather than a table
//! of their own.

use serde::{Deserialize, Serialize};

use crate::{AppDb, Result};

const SETTING: &str = "hidden_sticker_packs";

/// One hidden pack, or the tombstone of one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HiddenStickerPack {
    /// Hex.
    pub pack_id: String,
    /// Hex. A tombstone made here keeps it, so hiding the pack again
    /// needn't look it up (Desktop's `markHiddenStickerPackDeleted` leaves
    /// `packKey`); one merged from Storage Service has none.
    #[serde(default)]
    pub pack_key: Option<String>,
    /// Unix millis; set on a tombstone.
    #[serde(default)]
    pub unhidden_at: Option<i64>,
    /// Changed here and not yet written to Storage Service (Desktop's
    /// `storageNeedsSync`).
    #[serde(default)]
    pub needs_sync: bool,
}

impl HiddenStickerPack {
    pub fn is_hidden(&self) -> bool {
        self.unhidden_at.is_none()
    }
}

/// A hidden pack as a Storage Service record or a backup has it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteHiddenStickerPack {
    /// Hex.
    pub pack_id: String,
    /// Hex, when the record has one.
    pub pack_key: Option<String>,
    /// Unix millis, 0 when it's hidden.
    pub unhidden_at: i64,
}

/// What [`AppDb::hide_sticker_pack`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Hid {
    /// Hidden, or a tombstone hidden again; to be written.
    Hidden(HiddenStickerPack),
    /// It was hidden already.
    Already,
    /// Neither a tombstone here nor `sticker_packs` has its key.
    NoKey,
}

/// What a merge changed.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct HiddenPackMerge {
    pub changed: bool,
    /// Remote hidden packs without a pack key, skipped.
    pub keyless: usize,
}

/// Desktop's merge (`mergeHiddenStickerPackRecord`, and `processManifest`
/// and `generateManifest` in `ts/services/storage.preload.ts` for local
/// records), applied to `local`:
///
/// - A remote tombstone older than `drop_before` (now less the message
///   queue time) is dropped, the local record with it.
/// - A remote tombstone replaces the local record, keeping the key it had.
/// - A remote hidden pack without a key is skipped (Desktop throws).
/// - A remote hidden pack replaces the local record.
/// - A local hidden pack the manifest doesn't have is written again, as
///   Desktop clears its storage id and uploads it; a local tombstone it
///   doesn't have goes.
/// - A local tombstone older than `drop_before` goes, as `generateManifest`
///   drops it.
///
/// A local record with a change not yet written is left for that write, as
/// for favorite stickers: every record is read each sync, so a remote
/// record merged before can't be told from a new one.
pub fn merge_hidden_packs(local: &mut Vec<HiddenStickerPack>, remote: &[RemoteHiddenStickerPack], drop_before: i64) -> HiddenPackMerge {
    let mut merge = HiddenPackMerge::default();
    let before = local.clone();
    let mut seen = std::collections::HashSet::new();
    for record in remote {
        seen.insert(record.pack_id.clone());
        let at = local.iter().position(|p| p.pack_id == record.pack_id);
        if at.is_some_and(|i| local[i].needs_sync) {
            continue;
        }
        let next = if record.unhidden_at > 0 {
            (record.unhidden_at >= drop_before).then(|| HiddenStickerPack {
                pack_id: record.pack_id.clone(),
                // A tombstone's record has no key; ours keeps the one it had,
                // for hiding it again.
                pack_key: at.and_then(|i| local[i].pack_key.clone()),
                unhidden_at: Some(record.unhidden_at),
                needs_sync: false,
            })
        } else {
            let Some(key) = record.pack_key.clone() else {
                merge.keyless += 1;
                continue;
            };
            Some(HiddenStickerPack { pack_id: record.pack_id.clone(), pack_key: Some(key), unhidden_at: None, needs_sync: false })
        };
        match (at, next) {
            (Some(i), Some(next)) => local[i] = next,
            (Some(i), None) => {
                local.remove(i);
            }
            (None, Some(next)) => local.push(next),
            (None, None) => {}
        }
    }
    local.retain_mut(|p| {
        if p.unhidden_at.is_some_and(|at| at < drop_before) {
            return false;
        }
        if p.needs_sync || seen.contains(&p.pack_id) {
            return true;
        }
        if p.is_hidden() {
            p.needs_sync = true;
            return true;
        }
        false
    });
    merge.changed = *local != before;
    merge
}

/// A transferred backup's hidden packs, added to `local` as Desktop's
/// `#fromHiddenStickerPack` (`ts/services/backups/import.preload.ts`) adds
/// them: hidden, as Storage Service already has them (not to be written),
/// skipped when there's a record of that pack here already (Desktop's
/// insert fails). Returns how many were added.
pub fn import_hidden_packs(local: &mut Vec<HiddenStickerPack>, imported: &[RemoteHiddenStickerPack]) -> usize {
    let mut added = 0;
    for record in imported {
        let Some(key) = record.pack_key.clone() else { continue };
        if local.iter().any(|p| p.pack_id == record.pack_id) {
            continue;
        }
        local.push(HiddenStickerPack { pack_id: record.pack_id.clone(), pack_key: Some(key), unhidden_at: None, needs_sync: false });
        added += 1;
    }
    added
}

fn parse(value: Option<String>) -> Vec<HiddenStickerPack> {
    value.and_then(|v| serde_json::from_str(&v).ok()).unwrap_or_default()
}

impl AppDb {
    /// Every hidden pack and tombstone.
    pub async fn hidden_sticker_packs(&self) -> Result<Vec<HiddenStickerPack>> {
        Ok(parse(self.setting(SETTING).await?))
    }

    /// The hex ids of packs hidden now (Desktop's
    /// `getAllHiddenStickerPacksActive`).
    pub async fn hidden_sticker_pack_ids(&self) -> Result<std::collections::HashSet<String>> {
        Ok(self.hidden_sticker_packs().await?.into_iter().filter(HiddenStickerPack::is_hidden).map(|p| p.pack_id).collect())
    }

    /// Changes the hidden packs in one write transaction.
    async fn edit_hidden_packs<T>(&self, edit: impl FnOnce(&mut Vec<HiddenStickerPack>) -> T) -> Result<T> {
        let mut tx = self.write().await?;
        let value: Option<String> =
            sqlx::query_scalar("SELECT value FROM settings WHERE key = ?").bind(SETTING).fetch_optional(&mut *tx).await?;
        let mut packs = parse(value);
        let before = packs.clone();
        let result = edit(&mut packs);
        if packs != before {
            sqlx::query("INSERT OR REPLACE INTO settings (key, value) VALUES (?, ?)")
                .bind(SETTING)
                .bind(serde_json::to_string(&packs)?)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(result)
    }

    /// Hides a pack (Desktop's `markStickerPackAsHidden`), with the key a
    /// tombstone of it kept or, failing that, the key `sticker_packs` has
    /// (Desktop commit 8049395, so hiding after a synced unhide works), or
    /// failing that `fallback_key` (hex: the key the sticker manager has).
    pub async fn hide_sticker_pack(&self, pack_id: &str, fallback_key: Option<&str>) -> Result<Hid> {
        let known_key: Option<String> =
            sqlx::query_scalar("SELECT key FROM sticker_packs WHERE id = ?").bind(pack_id).fetch_optional(&self.pool).await?;
        self.edit_hidden_packs(|packs| {
            let at = packs.iter().position(|p| p.pack_id == pack_id);
            if at.is_some_and(|i| packs[i].is_hidden()) {
                return Hid::Already;
            }
            let key = at
                .and_then(|i| packs[i].pack_key.clone())
                .filter(|k| !k.is_empty())
                .or(known_key.filter(|k| !k.is_empty()))
                .or(fallback_key.filter(|k| !k.is_empty()).map(str::to_owned));
            let Some(key) = key else {
                return Hid::NoKey;
            };
            let hidden = HiddenStickerPack { pack_id: pack_id.to_owned(), pack_key: Some(key), unhidden_at: None, needs_sync: true };
            match at {
                Some(i) => packs[i] = hidden.clone(),
                None => packs.push(hidden.clone()),
            }
            Hid::Hidden(hidden)
        })
        .await
    }

    /// Unhides a pack at `now` (Desktop's `maybeUnhideStickerPack`, when
    /// it's installed, deleted, or hiding it is undone): a tombstone, to be
    /// written. `None` when it wasn't hidden.
    pub async fn unhide_sticker_pack(&self, pack_id: &str, now: i64) -> Result<Option<HiddenStickerPack>> {
        self.edit_hidden_packs(|packs| {
            let pack = packs.iter_mut().find(|p| p.pack_id == pack_id && p.is_hidden())?;
            pack.unhidden_at = Some(now);
            pack.needs_sync = true;
            Some(pack.clone())
        })
        .await
    }

    /// Storage Service's hidden packs, merged (see [`merge_hidden_packs`]).
    pub async fn merge_storage_hidden_sticker_packs(&self, remote: &[RemoteHiddenStickerPack], drop_before: i64) -> Result<HiddenPackMerge> {
        self.edit_hidden_packs(|packs| merge_hidden_packs(packs, remote, drop_before)).await
    }

    /// A transferred backup's hidden packs (see [`import_hidden_packs`]).
    pub async fn import_hidden_sticker_packs(&self, imported: &[RemoteHiddenStickerPack]) -> Result<usize> {
        self.edit_hidden_packs(|packs| import_hidden_packs(packs, imported)).await
    }

    /// `written` (as it was then) is in Storage Service: no longer pending,
    /// unless it changed again meanwhile.
    pub async fn hidden_sticker_pack_written(&self, written: &HiddenStickerPack) -> Result<()> {
        self.edit_hidden_packs(|packs| {
            if let Some(p) = packs.iter_mut().find(|p| p.pack_id == written.pack_id && p.unhidden_at == written.unhidden_at) {
                p.needs_sync = false;
            }
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hidden(pack: &str) -> HiddenStickerPack {
        HiddenStickerPack { pack_id: pack.into(), pack_key: Some("ab".into()), unhidden_at: None, needs_sync: false }
    }

    fn tombstone(pack: &str, at: i64) -> HiddenStickerPack {
        HiddenStickerPack { pack_key: None, unhidden_at: Some(at), ..hidden(pack) }
    }

    fn remote(pack: &str, unhidden_at: i64) -> RemoteHiddenStickerPack {
        RemoteHiddenStickerPack { pack_id: pack.into(), pack_key: (unhidden_at == 0).then(|| "ab".into()), unhidden_at }
    }

    #[test]
    fn remote_records_replace_local_ones() {
        let mut local = vec![hidden("aa"), tombstone("bb", 300)];
        let records = [remote("aa", 500), remote("bb", 0), remote("cc", 0)];
        let merge = merge_hidden_packs(&mut local, &records, 100);
        assert!(merge.changed);
        // The tombstone keeps the key its pack had here.
        let aa = HiddenStickerPack { pack_key: hidden("aa").pack_key, ..tombstone("aa", 500) };
        assert_eq!(local, vec![aa, hidden("bb"), hidden("cc")]);
        // Again: nothing changes.
        assert!(!merge_hidden_packs(&mut local, &records, 100).changed);
    }

    #[test]
    fn old_tombstones_and_keyless_packs_are_dropped() {
        let mut local = vec![hidden("aa"), tombstone("dd", 50), HiddenStickerPack { needs_sync: true, ..tombstone("ee", 60) }];
        let keyless = RemoteHiddenStickerPack { pack_key: None, ..remote("cc", 0) };
        let merge = merge_hidden_packs(&mut local, &[remote("aa", 50), remote("bb", 40), keyless, remote("dd", 50)], 100);
        // Remote tombstones past the queue time go, local ones too (even one
        // not yet written, as Desktop's `generateManifest` drops it).
        assert!(local.is_empty(), "{local:?}");
        assert_eq!(merge.keyless, 1);
    }

    #[test]
    fn local_changes_wait_for_their_write_and_missing_records_are_written_again() {
        let pending = HiddenStickerPack { needs_sync: true, ..tombstone("aa", 900) };
        let mut local = vec![pending.clone(), hidden("cc"), tombstone("bb", 800)];
        merge_hidden_packs(&mut local, &[remote("aa", 0)], 100);
        // The pending tombstone stays as it is; the hidden pack Storage
        // Service lacks is to be written; the tombstone it lacks goes.
        assert_eq!(local, vec![pending, HiddenStickerPack { needs_sync: true, ..hidden("cc") }]);
    }

    #[test]
    fn a_backups_hidden_packs_are_added_as_already_written() {
        let mut local = vec![tombstone("aa", 5)];
        let keyless = RemoteHiddenStickerPack { pack_key: None, ..remote("cc", 0) };
        assert_eq!(import_hidden_packs(&mut local, &[remote("aa", 0), remote("bb", 0), keyless]), 1);
        assert_eq!(local, vec![tombstone("aa", 5), hidden("bb")]);
    }
}
