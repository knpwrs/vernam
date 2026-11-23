//! Hidden sticker packs, as Signal Desktop has them (commit 5edf85d:
//! `markStickerPackAsHidden` in `ts/state/ducks/stickers.preload.ts`,
//! `maybeUnhideStickerPack` in `ts/util/stickers.preload.ts`,
//! `mergeHiddenStickerPackRecord` in `ts/services/storageRecordOps.preload.ts`):
//! hiding a received pack (uninstalling it first if it's installed) or
//! unhiding it writes its `HiddenStickerPackRecord`; installing a pack
//! unhides it; Storage Service's records are merged at each sync.
//!
//! Network work is bounded: at most [`WRITES_PER_SYNC`] records left
//! unwritten are written at each sync.

use std::cell::Cell;

use signal_store::{Hid, HiddenStickerPack, RemoteHiddenStickerPack};

use super::*;

/// Records written again at each sync, for changes whose write failed and
/// hidden packs Storage Service doesn't have.
const WRITES_PER_SYNC: usize = 20;

/// Counts hidden packs changed here, so a sync can tell whether one came
/// after its Storage Service snapshot (as favorite stickers'
/// `local_changes`).
#[derive(Default)]
pub(super) struct HiddenPackChanges(Cell<u64>);

impl HiddenPackChanges {
    pub(super) fn get(&self) -> u64 {
        self.0.get()
    }

    fn bump(&self) {
        self.0.set(self.0.get() + 1);
    }
}

impl Actor {
    /// Desktop's `markStickerPackAsHidden(packId, true)`: an installed pack
    /// is uninstalled first (here and on our other devices), then hidden
    /// with the key we know, and its record written. Desktop only offers
    /// it for "Shared With You" packs, behind `stickerPackHiding`.
    pub(super) async fn hide_sticker_pack(self: &Rc<Self>, id: Vec<u8>, key: Vec<u8>) {
        // An Undo from the toast waits for this hide.
        let _hiding = self.hide_command_lock.lock().await;
        if self.load_stickers().await.iter().any(|p| p.id == id) {
            self.uninstall_sticker_pack_ui(id.clone(), key.clone()).await;
            if self.load_stickers().await.iter().any(|p| p.id == id) {
                return;
            }
        }
        self.hidden_pack_changes.bump();
        let key = (!key.is_empty()).then(|| hex_of(&key));
        let hidden = match self.db.hide_sticker_pack(&hex_of(&id), key.as_deref()).await {
            Ok(Hid::Hidden(hidden)) => hidden,
            Ok(Hid::Already) => return warn!("not hiding a sticker pack that's hidden already"),
            Ok(Hid::NoKey) => return self.error("couldn't hide the sticker pack", "its key isn't known"),
            Err(error) => return self.error("failed to hide the sticker pack", error),
        };
        self.emit(Event::StickerManager(self.sticker_manager_view().await));
        self.write_hidden_pack_now(&hidden.pack_id).await;
    }

    /// Undoing a hide (Desktop's `markStickerPackAsHidden(packId, false)`,
    /// from its toast): a tombstone, written. The pack isn't installed
    /// again.
    pub(super) async fn unhide_sticker_pack(self: &Rc<Self>, id: Vec<u8>) {
        let _hiding = self.hide_command_lock.lock().await;
        if let Some(tombstone) = self.unhide_here(&id).await {
            self.emit(Event::StickerManager(self.sticker_manager_view().await));
            self.write_hidden_pack_now(&tombstone.pack_id).await;
        }
    }

    /// Desktop's `maybeUnhideStickerPack`, for a pack just installed (from
    /// anywhere): a hidden one is unhidden here at once and its tombstone
    /// written in a task of its own, so the receive loop (a synced install)
    /// doesn't wait on other records' writes. `true` when it was hidden.
    pub(super) async fn sticker_pack_unhidden(self: &Rc<Self>, id: &[u8]) -> bool {
        let Some(tombstone) = self.unhide_here(id).await else { return false };
        let this = self.clone();
        tokio::task::spawn_local(async move { this.write_hidden_pack_now(&tombstone.pack_id).await });
        true
    }

    /// Unhides a pack in our database: its tombstone, when it was hidden.
    pub(super) async fn unhide_here(&self, id: &[u8]) -> Option<HiddenStickerPack> {
        match self.db.unhide_sticker_pack(&hex_of(id), now_millis()).await {
            Ok(Some(tombstone)) => {
                info!("unhiding a sticker pack");
                self.hidden_pack_changes.bump();
                Some(tombstone)
            }
            Ok(None) => None,
            Err(error) => {
                warn!(%error, "failed to unhide a sticker pack");
                None
            }
        }
    }

    /// Writes a pack's record now, or leaves it for the next sync when that
    /// fails, as Desktop leaves it to its storage sync.
    pub(super) async fn write_hidden_pack_now(&self, pack_id: &str) {
        let Some(manager) = self.manager() else { return };
        let _writing = self.hidden_pack_write_lock.lock().await;
        match self.write_hidden_sticker_pack(&manager, pack_id).await {
            Ok(true) => self.storage_written(),
            Ok(false) => {}
            Err(error) => warn!(error = %format!("{error:#}"), "couldn't write a hidden sticker pack's record yet"),
        }
    }

    /// Writes a pack's record as our database has it now, if it's still to
    /// be written, then marks that state written (a change made meanwhile
    /// stays to be written). Only with `hidden_pack_write_lock` held, so two
    /// writes never overlap and a retry can't put back an older state.
    /// `Ok(true)` when it wrote.
    async fn write_hidden_sticker_pack(&self, manager: &SignalManager, pack_id: &str) -> anyhow::Result<bool> {
        let current = self.db.hidden_sticker_packs().await?.into_iter().find(|p| p.pack_id == pack_id);
        let Some(pack) = current.filter(|p| p.needs_sync) else { return Ok(false) };
        let id = hex::decode(&pack.pack_id)?;
        // A tombstone's record has no key (Desktop's `toHiddenStickerPackRecord`).
        let key = match pack.unhidden_at {
            Some(_) => Vec::new(),
            None => hex::decode(pack.pack_key.as_deref().context("a hidden sticker pack without its key")?)?,
        };
        let unhidden_at = pack.unhidden_at.unwrap_or(0).max(0) as u64;
        storage::write_hidden_sticker_pack(manager, &id, &key, unhidden_at).await?;
        self.db.hidden_sticker_pack_written(&pack).await?;
        // No longer pending, so a sync whose snapshot came before this
        // write would undo it: it counts as a change too.
        self.hidden_pack_changes.bump();
        Ok(true)
    }

    /// Storage Service's hidden packs, merged (Desktop's
    /// `mergeHiddenStickerPackRecord`; tombstones older than the message
    /// queue time go), then what's left to write written. `changes` is
    /// [`HiddenPackChanges`] from before the snapshot was read: a pack
    /// hidden or unhidden since would be undone by its older record, so
    /// the merge waits for the next sync.
    pub(super) async fn apply_storage_hidden_sticker_packs(self: &Rc<Self>, remote: Vec<RemoteHiddenStickerPack>, changes: u64) {
        // Held from the check to the last write: a write finishing between
        // them would otherwise be undone by the older snapshot.
        let _writing = self.hidden_pack_write_lock.lock().await;
        if self.hidden_pack_changes.get() != changes {
            return info!("hidden sticker packs changed during the sync; merging them at the next");
        }
        let drop_before = now_millis() - crate::remote_config().message_queue_time_ms();
        let merge = match self.db.merge_storage_hidden_sticker_packs(&remote, drop_before).await {
            Ok(merge) => merge,
            Err(error) => return warn!(%error, "failed to save hidden sticker packs"),
        };
        if merge.keyless > 0 {
            warn!(skipped = merge.keyless, "hidden sticker pack records without a pack key skipped");
        }
        if merge.changed {
            self.emit(Event::StickerManager(self.sticker_manager_view().await));
        }
        let Some(manager) = self.manager() else { return };
        let pending: Vec<_> =
            self.db.hidden_sticker_packs().await.unwrap_or_default().into_iter().filter(|p| p.needs_sync).take(WRITES_PER_SYNC).collect();
        let mut written = false;
        for pack in &pending {
            match self.write_hidden_sticker_pack(&manager, &pack.pack_id).await {
                Ok(wrote) => written |= wrote,
                // One that can't be written doesn't hold up the rest.
                Err(error) => warn!(error = %format!("{error:#}"), "couldn't write a hidden sticker pack's record"),
            }
        }
        if written {
            self.storage_written();
        }
    }
}
