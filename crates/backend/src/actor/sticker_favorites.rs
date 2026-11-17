//! Favorite stickers, as Signal Desktop has them (`markStickerAsFavorite`
//! in `ts/state/ducks/stickers.preload.ts`, `ts/util/stickers.preload.ts`,
//! `mergeFavoriteStickerRecord` in `ts/services/storageRecordOps.preload.ts`):
//! favoriting or unfavoriting writes the sticker's `FavoriteStickerRecord`;
//! Storage Service's records are merged at each sync; a favorite from a
//! pack that isn't installed has its one sticker downloaded on its own.
//!
//! Network work is bounded: at most [`DOWNLOADS_PER_RUN`] stickers are
//! downloaded each time, one at a time, and at most [`WRITES_PER_SYNC`]
//! records left unwritten are written at each sync.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use signal_store::{FavoriteSticker, Favorited, RemoteFavoriteSticker};

use super::*;

/// Favorites' stickers downloaded each time.
const DOWNLOADS_PER_RUN: usize = 24;
/// Records written again at each sync, for changes whose write failed and
/// favorites Storage Service doesn't have.
const WRITES_PER_SYNC: usize = 20;

/// Between runs, when one leaves favorites still to download.
const NEXT_RUN_AFTER: std::time::Duration = std::time::Duration::from_secs(5 * 60);

/// How long a sticker whose download failed (offline, a timeout) waits
/// before it's tried again.
const RETRY_AFTER: std::time::Duration = std::time::Duration::from_secs(60 * 60);

/// Where the favorites' downloads are, for one account's backend.
#[derive(Default)]
pub(super) struct FavoriteDownloads {
    /// Counts favorites changed here, so a sync can tell whether one came
    /// after its Storage Service snapshot.
    pub(super) local_changes: Cell<u64>,
    running: Cell<bool>,
    /// Asked for while a run was going: another run follows it.
    again: Cell<bool>,
    /// Stickers whose download failed, and when: `None` for good (a pack
    /// without the sticker, a bad id), otherwise tried again after
    /// [`RETRY_AFTER`].
    failed: RefCell<HashMap<(String, u32), Option<std::time::Instant>>>,
}

impl FavoriteDownloads {
    /// Whether a failed download of this sticker should wait.
    fn failed_recently(&self, pack_id: &str, sticker_id: u32) -> bool {
        match self.failed.borrow().get(&(pack_id.to_owned(), sticker_id)) {
            Some(None) => true,
            Some(Some(at)) => at.elapsed() < RETRY_AFTER,
            None => false,
        }
    }
}

impl Actor {
    fn favorites_dir(&self, pack_id: &str) -> PathBuf {
        self.config.data_dir.join("stickers").join("favorites").join(pack_id)
    }

    /// Deletes a favorite's own image (never an installed pack's).
    fn remove_favorite_image(&self, favorite: &FavoriteSticker) {
        let Some(path) = favorite.path.as_deref().map(PathBuf::from) else { return };
        if path.starts_with(self.config.data_dir.join("stickers").join("favorites")) {
            let _ = std::fs::remove_file(path);
        }
    }

    /// What the picker shows (Desktop's `getAllInstalledFavoriteStickers`):
    /// favorites newest first, each with its installed pack's image or its
    /// own; those still downloading are left out.
    pub(super) async fn favorite_sticker_views(&self) -> Vec<crate::FavoriteStickerView> {
        let favorites = self.db.favorite_stickers().await.unwrap_or_default();
        let packs = self.load_stickers().await;
        favorites
            .into_iter()
            .filter(FavoriteSticker::is_active)
            .filter_map(|f| {
                let (pack_id, pack_key) = (hex::decode(&f.pack_id).ok()?, hex::decode(f.pack_key.as_deref()?).ok()?);
                let installed = packs.iter().find(|p| p.id == pack_id).and_then(|p| p.stickers.iter().find(|s| s.id == f.sticker_id));
                let sticker = match installed {
                    Some(sticker) => sticker.clone(),
                    None => {
                        let path = PathBuf::from(f.path.as_deref()?);
                        path.exists().then(|| crate::StickerView { id: f.sticker_id, emoji: f.emoji.clone(), path })?
                    }
                };
                Some(crate::FavoriteStickerView { pack_id, pack_key, sticker })
            })
            .collect()
    }

    /// Tells the interface which stickers are favorites.
    pub(super) async fn emit_favorite_sticker_ids(&self) {
        match crate::stickers::favorite_sticker_ids(&self.db).await {
            Ok(ids) => self.emit(Event::FavoriteStickerIds(ids)),
            Err(error) => warn!(%error, "couldn't read favorite stickers"),
        }
    }

    /// Tells the interface, and downloads what favorites lack.
    pub(super) async fn favorite_stickers_changed(self: &Rc<Self>) {
        self.emit(Event::FavoriteStickers(self.favorite_sticker_views().await));
        self.emit_favorite_sticker_ids().await;
        self.download_favorite_stickers();
    }

    /// Desktop's `markStickerAsFavorite`: saved here, then its record
    /// written for our other devices.
    pub(super) async fn set_sticker_favorite(self: &Rc<Self>, pack_id: Vec<u8>, pack_key: Vec<u8>, sticker_id: u32, favorite: bool) {
        let (hex_id, now) = (hex_of(&pack_id), now_millis());
        let changes = &self.favorite_downloads.local_changes;
        changes.set(changes.get() + 1);
        let changed = if favorite {
            // The pack's key as we know it, over the one this request names
            // (a message's could be wrong): Desktop's
            // `addFavoriteStickerFromStickerRef` reads it from `sticker_packs`.
            let rows = self.db.sticker_pack_rows().await.unwrap_or_default();
            let key = rows.into_iter().find(|p| p.id == hex_id).map_or_else(|| hex_of(&pack_key), |p| p.key);
            match self.db.favorite_sticker(&hex_id, &key, sticker_id, now).await {
                Ok(Favorited::Added(added)) => Some(added),
                Ok(Favorited::Already) => None,
                Ok(Favorited::Full) => {
                    // Desktop's toast (`StickerFavoritesTooMany`, commit d465a37).
                    let limit = signal_store::FAVORITE_STICKER_LIMIT;
                    warn!(limit, "not favoriting a sticker: at the limit");
                    return self.emit(Event::Error(format!("You can only add {limit} stickers to Favorites")));
                }
                Err(error) => return self.error("failed to save the favorite sticker", error),
            }
        } else {
            match self.db.unfavorite_sticker(&hex_id, sticker_id, now).await {
                Ok(Some(old)) => {
                    // Desktop's `uninstallFavoriteSticker`: the sticker's
                    // own image goes; an installed pack keeps its.
                    self.remove_favorite_image(&old);
                    self.db.favorite_stickers().await.ok().and_then(|all| all.into_iter().find(|f| f.pack_id == hex_id && f.sticker_id == sticker_id))
                }
                Ok(None) => None,
                Err(error) => return self.error("failed to remove the favorite sticker", error),
            }
        };
        self.favorite_stickers_changed().await;
        let (Some(changed), Some(manager)) = (changed, self.manager()) else { return };
        match self.write_favorite_sticker(&manager, &changed).await {
            Ok(()) => self.storage_written(),
            // Saved here and written again at the next sync, as Desktop
            // leaves it to its storage sync.
            Err(error) => warn!(error = %format!("{error:#}"), "couldn't write a favorite sticker's record yet"),
        }
    }

    /// Writes `favorite`'s record as it is now, then marks it written.
    async fn write_favorite_sticker(&self, manager: &SignalManager, favorite: &FavoriteSticker) -> anyhow::Result<()> {
        let pack_id = hex::decode(&favorite.pack_id)?;
        let key = favorite.pack_key.as_deref().map(hex::decode).transpose()?.unwrap_or_default();
        let favorited_at = favorite.favorited_at.unwrap_or(0).max(0) as u64;
        let deleted_at = favorite.deleted_at.unwrap_or(0).max(0) as u64;
        storage::write_favorite_sticker(manager, &pack_id, favorite.sticker_id, &key, favorited_at, deleted_at).await?;
        self.db.favorite_sticker_written(favorite).await?;
        // No longer pending, so a sync whose snapshot came before this
        // write would undo it: it counts as a change too.
        let changes = &self.favorite_downloads.local_changes;
        changes.set(changes.get() + 1);
        Ok(())
    }

    /// Storage Service's favorites, merged (Desktop's
    /// `mergeFavoriteStickerRecord`; tombstones older than the message
    /// queue time go), then what's left to write written.
    /// `changes` is [`FavoriteDownloads::local_changes`] from before the
    /// snapshot was read: a favorite changed since would be undone by its
    /// older record, so the merge waits for the next sync.
    pub(super) async fn apply_storage_favorite_stickers(self: &Rc<Self>, remote: Vec<RemoteFavoriteSticker>, changes: u64) {
        if self.favorite_downloads.local_changes.get() != changes {
            return info!("favorite stickers changed during the sync; merging them at the next");
        }
        let drop_before = now_millis() - crate::remote_config().message_queue_time_ms();
        let merge = match self.db.merge_storage_favorites(&remote, drop_before).await {
            Ok(merge) => merge,
            Err(error) => return warn!(%error, "failed to save favorite stickers"),
        };
        if merge.keyless > 0 {
            warn!(skipped = merge.keyless, "favorite sticker records without a pack key skipped");
        }
        if merge.rejected > 0 {
            warn!(rejected = merge.rejected, limit = signal_store::FAVORITE_STICKER_LIMIT, "favorite stickers over the limit, newest left out");
        }
        for removed in &merge.removed {
            self.remove_favorite_image(removed);
        }
        if merge.changed {
            self.favorite_stickers_changed().await;
        }
        let Some(manager) = self.manager() else { return };
        let pending: Vec<_> =
            self.db.favorite_stickers().await.unwrap_or_default().into_iter().filter(|f| f.needs_sync).take(WRITES_PER_SYNC).collect();
        let mut written = false;
        for favorite in &pending {
            match self.write_favorite_sticker(&manager, favorite).await {
                Ok(()) => written = true,
                Err(error) => {
                    warn!(error = %format!("{error:#}"), "couldn't write a favorite sticker's record");
                    break;
                }
            }
        }
        if written {
            self.storage_written();
        }
    }

    /// Downloads the stickers of favorites whose pack isn't installed
    /// (Desktop's `installFavoriteSticker`, which downloads only those of a
    /// "known" pack), in the background, one at a time.
    /// Only with the picker's flag on: nothing shows them otherwise.
    fn download_favorite_stickers(self: &Rc<Self>) {
        if !crate::remote_config().sticker_favorites() {
            return;
        }
        let downloads = &self.favorite_downloads;
        if downloads.running.replace(true) {
            downloads.again.set(true);
            return;
        }
        let this = self.clone();
        tokio::task::spawn_local(async move {
            loop {
                this.favorite_downloads.again.set(false);
                let (any, more) = this.try_download_favorite_stickers().await;
                if any {
                    this.emit(Event::FavoriteStickers(this.favorite_sticker_views().await));
                    this.emit_favorite_sticker_ids().await;
                }
                if this.favorite_downloads.again.get() {
                    continue;
                }
                if !more {
                    break;
                }
                // The rest a little later, as profile photos' batches go;
                // still running meanwhile, so asking again starts nothing.
                tokio::time::sleep(NEXT_RUN_AFTER).await;
            }
            this.favorite_downloads.running.set(false);
        });
    }

    /// Whether any arrived, and whether more are left for another run.
    async fn try_download_favorite_stickers(&self) -> (bool, bool) {
        let installed: HashSet<Vec<u8>> = match self.store.sticker_packs().await {
            Ok(packs) => packs.flatten().map(|p| p.id).collect(),
            Err(_) => return (false, false),
        };
        let mut missing: Vec<FavoriteSticker> = self
            .db
            .favorite_stickers()
            .await
            .unwrap_or_default()
            .into_iter()
            .filter(|f| f.is_active() && f.pack_key.is_some())
            .filter(|f| !hex::decode(&f.pack_id).is_ok_and(|id| installed.contains(&id)))
            .filter(|f| !f.path.as_deref().is_some_and(|p| std::path::Path::new(p).exists()))
            .filter(|f| !self.favorite_downloads.failed_recently(&f.pack_id, f.sticker_id))
            .collect();
        let more = missing.len() > DOWNLOADS_PER_RUN;
        missing.truncate(DOWNLOADS_PER_RUN);
        if missing.is_empty() {
            return (false, false);
        }
        let Some(mut manager) = self.manager() else { return (false, false) };
        let failed = &self.favorite_downloads.failed;
        let failed_for_good = |f: &FavoriteSticker| failed.borrow_mut().insert((f.pack_id.clone(), f.sticker_id), None);
        let failed_for_now =
            |f: &FavoriteSticker| failed.borrow_mut().insert((f.pack_id.clone(), f.sticker_id), Some(std::time::Instant::now()));
        let mut manifests: HashMap<String, Option<HashMap<u32, Option<String>>>> = HashMap::new();
        let mut any = false;
        for favorite in missing {
            let (Ok(pack_id), Some(Ok(key))) = (hex::decode(&favorite.pack_id), favorite.pack_key.as_deref().map(hex::decode)) else {
                warn!("a favorite sticker's pack id or key isn't hex");
                failed_for_good(&favorite);
                continue;
            };
            // The pack's manifest, once a run: the sticker's emoji, and
            // whether the pack has it.
            if !manifests.contains_key(&favorite.pack_id) {
                let manifest = match manager.sticker_pack_manifest(&pack_id, &key).await {
                    Ok(manifest) => Some(manifest.stickers.iter().map(|s| (s.id, s.emoji.clone())).collect()),
                    Err(error) => {
                        debug!(%error, "couldn't fetch a favorite sticker's pack manifest");
                        None
                    }
                };
                manifests.insert(favorite.pack_id.clone(), manifest);
            }
            let Some(stickers) = manifests[&favorite.pack_id].as_ref() else {
                failed_for_now(&favorite);
                continue;
            };
            let Some(emoji) = stickers.get(&favorite.sticker_id) else {
                warn!(sticker = favorite.sticker_id, "a favorite sticker isn't in its pack");
                failed_for_good(&favorite);
                continue;
            };
            let bytes = match manager.sticker_image(&pack_id, &key, favorite.sticker_id).await {
                Ok(bytes) => bytes,
                Err(error) => {
                    debug!(%error, "couldn't download a favorite sticker");
                    failed_for_now(&favorite);
                    continue;
                }
            };
            let Some(path) = Self::save_sticker_image(&self.favorites_dir(&favorite.pack_id), favorite.sticker_id, &bytes) else {
                warn!("couldn't save a favorite sticker");
                failed_for_now(&favorite);
                continue;
            };
            let path = path.to_string_lossy().into_owned();
            match self.db.set_favorite_sticker_image(&favorite.pack_id, favorite.sticker_id, &path, emoji.as_deref()).await {
                Ok(true) => any = true,
                // Unfavorited meanwhile.
                Ok(false) => {
                    let _ = std::fs::remove_file(&path);
                }
                Err(error) => warn!(%error, "failed to save a favorite sticker's image"),
            }
        }
        (any, more)
    }
}
