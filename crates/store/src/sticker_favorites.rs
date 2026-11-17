//! Favorite stickers, as Signal Desktop keeps them (`favorite_stickers`,
//! `ts/sql/server/stickers.std.ts`, merged by `mergeFavoriteStickerRecord`
//! in `ts/services/storageRecordOps.preload.ts`): a sticker by its pack and
//! id, with the pack's key (the pack needn't be installed), when it was
//! favorited, or when it was unfavorited (a tombstone kept for Storage
//! Service). They live in one `settings` row as JSON rather than a table of
//! their own.

use serde::{Deserialize, Serialize};

use crate::{AppDb, Result};

/// Storage Service's limit (`FavoriteStickerRecord`: "If limit is exceeded
/// (500), reject newest one").
pub const FAVORITE_STICKER_LIMIT: usize = 500;

const SETTING: &str = "sticker_favorites";

/// One favorite, or the tombstone of one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FavoriteSticker {
    /// Hex.
    pub pack_id: String,
    pub sticker_id: u32,
    /// Hex; unset on a tombstone.
    #[serde(default)]
    pub pack_key: Option<String>,
    /// Unix millis; unset on a tombstone.
    #[serde(default)]
    pub favorited_at: Option<i64>,
    /// Unix millis; set on a tombstone.
    #[serde(default)]
    pub deleted_at: Option<i64>,
    /// Changed here and not yet written to Storage Service (Desktop's
    /// `storageNeedsSync`).
    #[serde(default)]
    pub needs_sync: bool,
    /// The sticker's emoji, from its pack's manifest, once fetched.
    #[serde(default)]
    pub emoji: Option<String>,
    /// Its image, downloaded on its own for a pack that isn't installed.
    #[serde(default)]
    pub path: Option<String>,
}

impl FavoriteSticker {
    pub fn is_active(&self) -> bool {
        self.deleted_at.is_none()
    }

    fn is(&self, pack_id: &str, sticker_id: u32) -> bool {
        self.pack_id == pack_id && self.sticker_id == sticker_id
    }
}

/// A favorite as a Storage Service record has it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteFavoriteSticker {
    /// Hex.
    pub pack_id: String,
    pub sticker_id: u32,
    /// Hex, when the record has one.
    pub pack_key: Option<String>,
    pub favorited_at: i64,
    /// Unix millis, 0 when it's a favorite.
    pub deleted_at: i64,
}

/// What [`AppDb::favorite_sticker`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Favorited {
    /// Added, or a tombstone favorited again; to be written.
    Added(FavoriteSticker),
    /// It was a favorite already.
    Already,
    /// There are [`FAVORITE_STICKER_LIMIT`] favorites already.
    Full,
}

/// What a merge changed.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct FavoriteMerge {
    pub changed: bool,
    /// Favorites that were removed or turned into tombstones, with the
    /// image they had, for it to be deleted.
    pub removed: Vec<FavoriteSticker>,
    /// Remote favorites without a pack key, skipped.
    pub keyless: usize,
    /// Favorites over the limit, not kept.
    pub rejected: usize,
}

/// Desktop's merge (`mergeFavoriteStickerRecord`, and `processManifest` for
/// local records the manifest doesn't have), applied to `local`:
///
/// - A remote tombstone older than `drop_before` (now less the message
///   queue time) is dropped, the local record with it.
/// - A remote tombstone replaces the local record.
/// - A remote favorite without a pack key is skipped (Desktop throws).
/// - A remote favorite replaces the local record, keeping its image.
/// - A local favorite the manifest doesn't have is written again, as
///   Desktop clears its storage id and uploads it; a local tombstone it
///   doesn't have goes.
///
/// A local record with a change not yet written is left for that write:
/// we read every record each time, so unlike Desktop we can't tell a remote
/// record we've merged before from a new one.
///
/// Then, over the limit, the newest favorites are rejected.
pub fn merge_favorites(
    local: &mut Vec<FavoriteSticker>,
    remote: &[RemoteFavoriteSticker],
    drop_before: i64,
) -> FavoriteMerge {
    let mut merge = FavoriteMerge::default();
    let before = local.clone();
    let mut seen = std::collections::HashSet::new();
    for record in remote {
        seen.insert((record.pack_id.clone(), record.sticker_id));
        let at = local.iter().position(|f| f.is(&record.pack_id, record.sticker_id));
        if at.is_some_and(|i| local[i].needs_sync) {
            continue;
        }
        let old = at.map(|i| local[i].clone());
        let next = if record.deleted_at > 0 {
            (record.deleted_at >= drop_before).then(|| FavoriteSticker {
                pack_id: record.pack_id.clone(),
                sticker_id: record.sticker_id,
                pack_key: None,
                favorited_at: None,
                deleted_at: Some(record.deleted_at),
                needs_sync: false,
                emoji: None,
                path: None,
            })
        } else {
            let Some(key) = record.pack_key.clone() else {
                merge.keyless += 1;
                continue;
            };
            Some(FavoriteSticker {
                pack_id: record.pack_id.clone(),
                sticker_id: record.sticker_id,
                pack_key: Some(key),
                favorited_at: Some(record.favorited_at),
                deleted_at: None,
                needs_sync: false,
                emoji: old.as_ref().and_then(|o| o.emoji.clone()),
                path: old.as_ref().and_then(|o| o.path.clone()),
            })
        };
        if let Some(old) = old.as_ref().filter(|o| o.is_active() && !next.as_ref().is_some_and(FavoriteSticker::is_active)) {
            merge.removed.push(old.clone());
        }
        match (at, next) {
            (Some(i), Some(next)) => local[i] = next,
            (Some(i), None) => {
                local.remove(i);
            }
            (None, Some(next)) => local.push(next),
            (None, None) => {}
        }
    }
    local.retain_mut(|f| {
        if f.needs_sync || seen.contains(&(f.pack_id.clone(), f.sticker_id)) {
            return true;
        }
        if f.is_active() {
            f.needs_sync = true;
            return true;
        }
        false
    });
    // Over the limit: keep the oldest favorites, after those favorited
    // here and not yet written (they were within the limit when made, and
    // Storage Service doesn't have them to bring back).
    let mut active: Vec<(bool, i64, String, u32)> = local
        .iter()
        .filter(|f| f.is_active())
        .map(|f| (!f.needs_sync, f.favorited_at.unwrap_or(0), f.pack_id.clone(), f.sticker_id))
        .collect();
    if active.len() > FAVORITE_STICKER_LIMIT {
        active.sort();
        let rejected: std::collections::HashSet<_> =
            active.split_off(FAVORITE_STICKER_LIMIT).into_iter().map(|(_, _, pack, id)| (pack, id)).collect();
        merge.rejected = rejected.len();
        local.retain(|f| {
            let reject = f.is_active() && rejected.contains(&(f.pack_id.clone(), f.sticker_id));
            if reject && f.path.is_some() {
                merge.removed.push(f.clone());
            }
            !reject
        });
    }
    sort(local);
    merge.changed = *local != before;
    merge
}

/// Favorites from a transferred backup, added to `local` as Desktop's
/// `#fromFavoriteStickerList` (`ts/services/backups/import.preload.ts`)
/// adds them: each as a favorite Storage Service already has (not to be
/// written), skipped when there's a record of that sticker here already
/// (Desktop's insert fails) or no pack key, and none past
/// [`FAVORITE_STICKER_LIMIT`].
pub fn import_favorites(local: &mut Vec<FavoriteSticker>, imported: &[RemoteFavoriteSticker]) -> FavoriteImport {
    let mut active = local.iter().filter(|f| f.is_active()).count();
    let mut import = FavoriteImport::default();
    for record in imported {
        let Some(key) = record.pack_key.clone() else {
            import.keyless += 1;
            continue;
        };
        if local.iter().any(|f| f.is(&record.pack_id, record.sticker_id)) {
            import.already += 1;
            continue;
        }
        if active >= FAVORITE_STICKER_LIMIT {
            import.over_limit += 1;
            continue;
        }
        local.push(FavoriteSticker {
            pack_id: record.pack_id.clone(),
            sticker_id: record.sticker_id,
            pack_key: Some(key),
            favorited_at: Some(record.favorited_at),
            deleted_at: None,
            needs_sync: false,
            emoji: None,
            path: None,
        });
        active += 1;
        import.added += 1;
    }
    sort(local);
    import
}

/// What [`import_favorites`] did with each favorite.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct FavoriteImport {
    pub added: usize,
    /// A record of that sticker was here already.
    pub already: usize,
    /// Past [`FAVORITE_STICKER_LIMIT`].
    pub over_limit: usize,
    /// Without a pack key.
    pub keyless: usize,
}

/// Newest favorite first, then tombstones.
fn sort(favorites: &mut [FavoriteSticker]) {
    favorites.sort_by(|a, b| {
        b.favorited_at.cmp(&a.favorited_at).then_with(|| a.pack_id.cmp(&b.pack_id)).then_with(|| a.sticker_id.cmp(&b.sticker_id))
    });
}

fn parse(value: Option<String>) -> Vec<FavoriteSticker> {
    value.and_then(|v| serde_json::from_str(&v).ok()).unwrap_or_default()
}

impl AppDb {
    /// Every favorite, newest first, then tombstones.
    pub async fn favorite_stickers(&self) -> Result<Vec<FavoriteSticker>> {
        Ok(parse(self.setting(SETTING).await?))
    }

    /// Changes the favorites in one write transaction.
    async fn edit_favorites<T>(&self, edit: impl FnOnce(&mut Vec<FavoriteSticker>) -> T) -> Result<T> {
        let mut tx = self.write().await?;
        let value: Option<String> =
            sqlx::query_scalar("SELECT value FROM settings WHERE key = ?").bind(SETTING).fetch_optional(&mut *tx).await?;
        let mut favorites = parse(value);
        let before = favorites.clone();
        let result = edit(&mut favorites);
        if favorites != before {
            sqlx::query("INSERT OR REPLACE INTO settings (key, value) VALUES (?, ?)")
                .bind(SETTING)
                .bind(serde_json::to_string(&favorites)?)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(result)
    }

    /// Favorites a sticker (Desktop's `addFavoriteStickerFromStickerRef`,
    /// which revives an unfavorited one too) at `now`, unless there are
    /// [`FAVORITE_STICKER_LIMIT`] already.
    pub async fn favorite_sticker(&self, pack_id: &str, pack_key: &str, sticker_id: u32, now: i64) -> Result<Favorited> {
        self.edit_favorites(|favorites| {
            let at = favorites.iter().position(|f| f.is(pack_id, sticker_id));
            if at.is_some_and(|i| favorites[i].is_active()) {
                return Favorited::Already;
            }
            if favorites.iter().filter(|f| f.is_active()).count() >= FAVORITE_STICKER_LIMIT {
                return Favorited::Full;
            }
            let favorite = FavoriteSticker {
                pack_id: pack_id.to_owned(),
                sticker_id,
                pack_key: Some(pack_key.to_owned()),
                favorited_at: Some(now),
                deleted_at: None,
                needs_sync: true,
                emoji: None,
                path: None,
            };
            match at {
                Some(i) => favorites[i] = favorite.clone(),
                None => favorites.push(favorite.clone()),
            }
            sort(favorites);
            Favorited::Added(favorite)
        })
        .await
    }

    /// Unfavorites a sticker at `now` (Desktop's `markFavoriteStickerDeleted`):
    /// a tombstone, to be written. Returns the favorite it was, or `None`
    /// when it wasn't one.
    pub async fn unfavorite_sticker(&self, pack_id: &str, sticker_id: u32, now: i64) -> Result<Option<FavoriteSticker>> {
        self.edit_favorites(|favorites| {
            let i = favorites.iter().position(|f| f.is(pack_id, sticker_id) && f.is_active())?;
            let old = favorites[i].clone();
            favorites[i] = FavoriteSticker {
                pack_key: None,
                favorited_at: None,
                deleted_at: Some(now),
                needs_sync: true,
                emoji: None,
                path: None,
                ..old.clone()
            };
            sort(favorites);
            Some(old)
        })
        .await
    }

    /// Storage Service's favorites, merged (see [`merge_favorites`]).
    pub async fn merge_storage_favorites(&self, remote: &[RemoteFavoriteSticker], drop_before: i64) -> Result<FavoriteMerge> {
        self.edit_favorites(|favorites| merge_favorites(favorites, remote, drop_before)).await
    }

    /// A transferred backup's favorites (see [`import_favorites`]).
    pub async fn import_favorite_stickers(&self, imported: &[RemoteFavoriteSticker]) -> Result<FavoriteImport> {
        self.edit_favorites(|favorites| import_favorites(favorites, imported)).await
    }

    /// `written` (as it was then) is in Storage Service: no longer pending,
    /// unless it changed again meanwhile.
    pub async fn favorite_sticker_written(&self, written: &FavoriteSticker) -> Result<()> {
        self.edit_favorites(|favorites| {
            if let Some(f) = favorites.iter_mut().find(|f| {
                f.is(&written.pack_id, written.sticker_id)
                    && (&f.pack_key, f.favorited_at, f.deleted_at) == (&written.pack_key, written.favorited_at, written.deleted_at)
            }) {
                f.needs_sync = false;
            }
        })
        .await
    }

    /// A favorite's image and emoji, once downloaded. Nothing changes for
    /// one that isn't a favorite any more.
    pub async fn set_favorite_sticker_image(&self, pack_id: &str, sticker_id: u32, path: &str, emoji: Option<&str>) -> Result<bool> {
        self.edit_favorites(|favorites| {
            let Some(f) = favorites.iter_mut().find(|f| f.is(pack_id, sticker_id) && f.is_active()) else { return false };
            f.path = Some(path.to_owned());
            f.emoji = emoji.map(str::to_owned);
            true
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn favorite(pack: &str, id: u32, at: i64) -> FavoriteSticker {
        FavoriteSticker {
            pack_id: pack.into(),
            sticker_id: id,
            pack_key: Some("ab".into()),
            favorited_at: Some(at),
            deleted_at: None,
            needs_sync: false,
            emoji: None,
            path: None,
        }
    }

    fn remote(pack: &str, id: u32, at: i64, deleted_at: i64) -> RemoteFavoriteSticker {
        RemoteFavoriteSticker {
            pack_id: pack.into(),
            sticker_id: id,
            pack_key: (deleted_at == 0).then(|| "ab".into()),
            favorited_at: if deleted_at == 0 { at } else { 0 },
            deleted_at,
        }
    }

    #[test]
    fn remote_records_replace_local_ones() {
        let mut local = vec![FavoriteSticker { path: Some("/img".into()), ..favorite("aa", 1, 10) }, favorite("aa", 2, 20)];
        let merge = merge_favorites(&mut local, &[remote("aa", 1, 30, 0), remote("aa", 2, 0, 500), remote("bb", 3, 25, 0)], 100);
        assert!(merge.changed);
        // Reordered by when, the image kept; the unfavorited one a tombstone.
        assert_eq!(local[0], FavoriteSticker { path: Some("/img".into()), ..favorite("aa", 1, 30) });
        assert_eq!(local[1], favorite("bb", 3, 25));
        assert_eq!((local[2].deleted_at, &local[2].pack_key, local[2].favorited_at), (Some(500), &None, None));
        assert_eq!(merge.removed, vec![favorite("aa", 2, 20)]);
        // Again: nothing changes.
        assert_eq!(merge_favorites(&mut local, &[remote("aa", 1, 30, 0), remote("aa", 2, 0, 500), remote("bb", 3, 25, 0)], 100).changed, false);
    }

    #[test]
    fn old_tombstones_and_keyless_favorites_are_dropped() {
        let mut local = vec![favorite("aa", 1, 10)];
        let keyless = RemoteFavoriteSticker { pack_key: None, ..remote("cc", 1, 5, 0) };
        let merge = merge_favorites(&mut local, &[remote("aa", 1, 0, 50), remote("bb", 2, 0, 40), keyless], 100);
        assert!(local.is_empty(), "{local:?}");
        assert_eq!((merge.removed.len(), merge.keyless), (1, 1));
    }

    #[test]
    fn local_changes_wait_for_their_write_and_missing_favorites_are_written_again() {
        let pending = FavoriteSticker { needs_sync: true, ..favorite("aa", 1, 10) };
        let synced_tombstone = FavoriteSticker { pack_key: None, favorited_at: None, deleted_at: Some(5), ..favorite("bb", 1, 0) };
        let mut local = vec![pending.clone(), favorite("cc", 1, 8), synced_tombstone];
        merge_favorites(&mut local, &[remote("aa", 1, 0, 900)], 100);
        // The pending favorite stays as it is; the favorite Storage
        // Service lacks is to be written; the tombstone it lacks goes.
        assert_eq!(local, vec![pending, FavoriteSticker { needs_sync: true, ..favorite("cc", 1, 8) }]);
    }

    #[test]
    fn over_the_limit_the_newest_are_rejected() {
        let mut local = Vec::new();
        let records: Vec<_> = (0..FAVORITE_STICKER_LIMIT as u32 + 3).map(|i| remote("aa", i, 1000 + i64::from(i), 0)).collect();
        let merge = merge_favorites(&mut local, &records, 0);
        assert_eq!(merge.rejected, 3);
        assert_eq!(local.len(), FAVORITE_STICKER_LIMIT);
        assert!(local.iter().all(|f| f.sticker_id < FAVORITE_STICKER_LIMIT as u32));
        assert_eq!(local[0].sticker_id, FAVORITE_STICKER_LIMIT as u32 - 1, "newest first");
    }

    #[test]
    fn over_the_limit_an_unwritten_local_favorite_stays() {
        let mut local = vec![FavoriteSticker { needs_sync: true, ..favorite("bb", 7, 9_999) }];
        let records: Vec<_> = (0..FAVORITE_STICKER_LIMIT as u32).map(|i| remote("aa", i, 1000 + i64::from(i), 0)).collect();
        let merge = merge_favorites(&mut local, &records, 0);
        assert_eq!((merge.rejected, local.len()), (1, FAVORITE_STICKER_LIMIT));
        assert!(local.iter().any(|f| f.pack_id == "bb" && f.needs_sync));
        assert!(!local.iter().any(|f| f.pack_id == "aa" && f.sticker_id == FAVORITE_STICKER_LIMIT as u32 - 1));
    }

    #[test]
    fn a_backups_favorites_are_added_as_already_written() {
        // A favorite and a tombstone here; the backup names both again, one
        // without a key, and two new ones.
        let tombstone = FavoriteSticker { pack_key: None, favorited_at: None, deleted_at: Some(5), ..favorite("aa", 2, 0) };
        let mut local = vec![favorite("aa", 1, 10), tombstone.clone()];
        let keyless = RemoteFavoriteSticker { pack_key: None, ..remote("cc", 1, 40, 0) };
        let records = [remote("aa", 1, 99, 0), remote("aa", 2, 99, 0), keyless, remote("bb", 1, 30, 0), remote("bb", 2, 20, 0)];
        let import = import_favorites(&mut local, &records);
        assert_eq!(import, FavoriteImport { added: 2, already: 2, over_limit: 0, keyless: 1 });
        let ids: Vec<_> = local.iter().map(|f| (f.pack_id.as_str(), f.sticker_id, f.favorited_at, f.needs_sync)).collect();
        assert_eq!(
            ids,
            [("bb", 1, Some(30), false), ("bb", 2, Some(20), false), ("aa", 1, Some(10), false), ("aa", 2, None, false)]
        );
        assert_eq!(local[3], tombstone);
    }

    #[test]
    fn a_backups_favorites_stop_at_the_limit() {
        let mut local: Vec<_> = (0..FAVORITE_STICKER_LIMIT as u32 - 2).map(|i| favorite("aa", i, 1)).collect();
        let records: Vec<_> = (0..5).map(|i| remote("bb", i, 2, 0)).collect();
        assert_eq!(import_favorites(&mut local, &records), FavoriteImport { added: 2, over_limit: 3, ..Default::default() });
        assert_eq!(local.iter().filter(|f| f.is_active()).count(), FAVORITE_STICKER_LIMIT);
        assert!(local.iter().any(|f| f.pack_id == "bb" && f.sticker_id == 1));
        assert!(!local.iter().any(|f| f.pack_id == "bb" && f.sticker_id == 2));
    }
}
