//! Messages scheduled to send later. Signal Desktop has none; Signal
//! Android keeps them on the phone and sends them when they're due, and so
//! does this app, while it runs. A scheduled message holds what the
//! composer would have sent: the text with its mentions and formatting,
//! the reply, the link preview, and the photos and files. Their bytes are
//! files in a directory the caller names (the account's `scheduled/`), so
//! that writing and reading a big video doesn't hold the database's one
//! writer: each transaction here only touches rows.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use sqlx::Row as _;

use crate::{AppDb, ConversationId, Error, Result, StyleSpan};

/// What an attachment of a scheduled message is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScheduledKind {
    /// A photo, sent with the text.
    Image,
    /// A file, sent as a message of its own.
    File,
    /// The link preview's image.
    PreviewImage,
}

impl ScheduledKind {
    fn code(self) -> i64 {
        match self {
            Self::Image => 0,
            Self::File => 1,
            Self::PreviewImage => 2,
        }
    }

    fn from_code(code: i64) -> Option<Self> {
        Some(match code {
            0 => Self::Image,
            1 => Self::File,
            2 => Self::PreviewImage,
            _ => return None,
        })
    }
}

/// A scheduled message's link preview; its image, if any, is its
/// [`ScheduledKind::PreviewImage`] attachment.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScheduledPreview {
    pub url: String,
    pub title: String,
    #[serde(default)]
    pub description: Option<String>,
    /// Unix millis.
    #[serde(default)]
    pub date: Option<i64>,
    /// The image's content type and size, when it has one.
    #[serde(default)]
    pub image: Option<ScheduledPreviewImage>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScheduledPreviewImage {
    pub content_type: String,
    pub width: u32,
    pub height: u32,
}

/// The rest of what a scheduled message sends, beside its text.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ScheduledExtras {
    /// Who each mention placeholder in the body mentions, in order.
    #[serde(default)]
    pub mentions: Vec<String>,
    /// Formatting over the body (placeholders and all).
    #[serde(default)]
    pub styles: Vec<StyleSpan>,
    /// The message replied to (our row id).
    #[serde(default)]
    pub quote: Option<i64>,
    #[serde(default)]
    pub preview: Option<ScheduledPreview>,
}

/// An attachment as listed, without its bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScheduledAttachment {
    pub kind: ScheduledKind,
    pub file_name: Option<String>,
    /// Where its bytes are: a file of this name in the scheduled directory.
    pub file: String,
    /// Bytes.
    pub size: u64,
}

/// A scheduled message, as listed.
#[derive(Debug, Clone, PartialEq)]
pub struct ScheduledMessage {
    pub id: i64,
    pub conversation: ConversationId,
    /// Unix millis.
    pub send_at: i64,
    /// Unix millis.
    pub created_at: i64,
    /// With a mention placeholder per mention.
    pub body: String,
    pub extras: ScheduledExtras,
    /// In the order they were added.
    pub attachments: Vec<ScheduledAttachment>,
}

/// An attachment with its bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScheduledFile {
    pub kind: ScheduledKind,
    pub file_name: Option<String>,
    pub bytes: Vec<u8>,
}

/// A message to schedule.
#[derive(Debug, Clone, PartialEq)]
pub struct NewScheduled {
    pub conversation: ConversationId,
    /// Unix millis.
    pub send_at: i64,
    pub body: String,
    pub extras: ScheduledExtras,
    pub files: Vec<ScheduledFile>,
}

impl AppDb {
    /// Schedules a message, its photos and files written to `dir` first;
    /// returns its id.
    pub async fn schedule_message(&self, message: &NewScheduled, dir: &Path, now: i64) -> Result<i64> {
        tokio::fs::create_dir_all(dir).await?;
        let mut written: Vec<String> = Vec::with_capacity(message.files.len());
        let saved = async {
            for file in &message.files {
                let name = format!("{:032x}", rand::random::<u128>());
                tokio::fs::write(dir.join(&name), &file.bytes).await?;
                written.push(name);
            }
            let mut tx = self.write().await?;
            let id: i64 = sqlx::query_scalar(
                "INSERT INTO scheduled_messages (conversation_id, send_at, created_at, body, extras) \
                 VALUES (?, ?, ?, ?, ?) RETURNING id",
            )
            .bind(&message.conversation.0)
            .bind(message.send_at)
            .bind(now)
            .bind(&message.body)
            .bind(serde_json::to_string(&message.extras)?)
            .fetch_one(&mut *tx)
            .await?;
            for (position, (file, name)) in message.files.iter().zip(&written).enumerate() {
                sqlx::query(
                    "INSERT INTO scheduled_attachments (scheduled_id, position, kind, file_name, file, size) \
                     VALUES (?, ?, ?, ?, ?, ?)",
                )
                .bind(id)
                .bind(position as i64)
                .bind(file.kind.code())
                .bind(&file.file_name)
                .bind(name)
                .bind(file.bytes.len() as i64)
                .execute(&mut *tx)
                .await?;
            }
            tx.commit().await?;
            Ok::<_, Error>(id)
        }
        .await;
        if saved.is_err() {
            remove_files(dir, written.iter().map(String::as_str)).await;
        }
        saved
    }

    /// Every scheduled message, soonest first.
    pub async fn scheduled_messages(&self) -> Result<Vec<ScheduledMessage>> {
        let rows = sqlx::query(
            "SELECT id, conversation_id, send_at, created_at, body, extras FROM scheduled_messages ORDER BY send_at, id",
        )
        .fetch_all(&self.pool)
        .await?;
        let attachments = sqlx::query(
            "SELECT scheduled_id, kind, file_name, file, size FROM scheduled_attachments ORDER BY scheduled_id, position",
        )
        .fetch_all(&self.pool)
        .await?;
        let mut messages: Vec<ScheduledMessage> = rows.iter().map(scheduled_from_row).collect::<Result<_>>()?;
        for row in attachments {
            let id: i64 = row.try_get("scheduled_id")?;
            let Some(attachment) = attachment_from_row(id, &row, "left out")? else { continue };
            if let Some(message) = messages.iter_mut().find(|m| m.id == id) {
                message.attachments.push(attachment);
            }
        }
        Ok(messages)
    }

    /// Removes scheduled message `id` and returns it with its attachments'
    /// bytes, read from `dir`, to send (`None`: it's gone already, sent or
    /// deleted). It's read first and removed only once every file has been
    /// read; the removal says whether this call took it, so it's sent at
    /// most once.
    pub async fn take_scheduled_message(
        &self,
        id: i64,
        dir: &Path,
    ) -> Result<Option<(ScheduledMessage, Vec<ScheduledFile>)>> {
        let Some(row) = sqlx::query(
            "SELECT id, conversation_id, send_at, created_at, body, extras FROM scheduled_messages WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?
        else {
            return Ok(None);
        };
        let mut message = scheduled_from_row(&row)?;
        // Sent without its mentions, the text would show placeholders in
        // their place: unreadable extras keep it here, and say why.
        let extras: String = row.try_get("extras")?;
        message.extras = serde_json::from_str(&extras)?;
        let rows = sqlx::query(
            "SELECT kind, file_name, file, size FROM scheduled_attachments WHERE scheduled_id = ? ORDER BY position",
        )
        .bind(id)
        .fetch_all(&self.pool)
        .await?;
        let mut files = Vec::with_capacity(rows.len());
        for row in rows {
            let Some(attachment) = attachment_from_row(id, &row, "not sent")? else { continue };
            let bytes = tokio::fs::read(file_path(dir, &attachment.file)?).await?;
            files.push(ScheduledFile { kind: attachment.kind, file_name: attachment.file_name.clone(), bytes });
            message.attachments.push(attachment);
        }
        let taken = sqlx::query("DELETE FROM scheduled_messages WHERE id = ?").bind(id).execute(&self.writer).await?;
        if taken.rows_affected() == 0 {
            // Sent or deleted meanwhile; whoever did removes the files.
            return Ok(None);
        }
        remove_files(dir, message.attachments.iter().map(|a| a.file.as_str())).await;
        Ok(Some((message, files)))
    }

    /// Moves scheduled message `id` to `send_at`; false when it's gone.
    pub async fn reschedule_message(&self, id: i64, send_at: i64) -> Result<bool> {
        let done = sqlx::query("UPDATE scheduled_messages SET send_at = ? WHERE id = ?")
            .bind(send_at)
            .bind(id)
            .execute(&self.writer)
            .await?;
        Ok(done.rows_affected() > 0)
    }

    /// Deletes scheduled message `id` unsent, and its files in `dir`; false
    /// when it's gone.
    pub async fn delete_scheduled_message(&self, id: i64, dir: &Path) -> Result<bool> {
        let mut tx = self.write().await?;
        let files: Vec<String> = sqlx::query_scalar("SELECT file FROM scheduled_attachments WHERE scheduled_id = ?")
            .bind(id)
            .fetch_all(&mut *tx)
            .await?;
        let done = sqlx::query("DELETE FROM scheduled_messages WHERE id = ?").bind(id).execute(&mut *tx).await?;
        tx.commit().await?;
        remove_files(dir, files.iter().map(String::as_str)).await;
        Ok(done.rows_affected() > 0)
    }

    /// Removes the files in `dir` that no scheduled message has, among those
    /// last changed before `before`: left by a chat or account deleted, or
    /// by a quit while a message was being scheduled. Files changed since
    /// may belong to a message still being saved. Returns how many went.
    pub async fn prune_scheduled_files(&self, dir: &Path, before: SystemTime) -> Result<usize> {
        let kept: std::collections::HashSet<String> =
            sqlx::query_scalar("SELECT file FROM scheduled_attachments").fetch_all(&self.pool).await?.into_iter().collect();
        let mut entries = match tokio::fs::read_dir(dir).await {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(error) => return Err(error.into()),
        };
        let mut removed = 0;
        while let Some(entry) = entries.next_entry().await? {
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else { continue };
            if kept.contains(&name) {
                continue;
            }
            let changed = entry.metadata().await.and_then(|m| m.modified());
            if changed.is_ok_and(|at| at < before) && tokio::fs::remove_file(entry.path()).await.is_ok() {
                removed += 1;
            }
        }
        Ok(removed)
    }
}

/// Removes scheduled messages' files from `dir`, such as a deleted chat's.
pub async fn remove_scheduled_files(dir: &Path, attachments: &[ScheduledAttachment]) {
    remove_files(dir, attachments.iter().map(|a| a.file.as_str())).await;
}

async fn remove_files(dir: &Path, names: impl Iterator<Item = &str>) {
    for name in names {
        let Ok(path) = file_path(dir, name) else { continue };
        if let Err(error) = tokio::fs::remove_file(&path).await
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(error = %error, "couldn't remove a scheduled message's file");
        }
    }
}

/// A file's path in `dir`; only names this module makes (hex) are joined.
fn file_path(dir: &Path, name: &str) -> Result<PathBuf> {
    if name.is_empty() || !name.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(Error::Schema(format!("bad scheduled file name {name:?}")));
    }
    Ok(dir.join(name))
}

/// An attachment row; `None` (and logged, saying it's `skipped`) when it's
/// of a kind this build doesn't know.
fn attachment_from_row(id: i64, row: &sqlx::sqlite::SqliteRow, skipped: &str) -> Result<Option<ScheduledAttachment>> {
    let code: i64 = row.try_get("kind")?;
    let Some(kind) = ScheduledKind::from_code(code) else {
        tracing::warn!(id, kind = code, "scheduled attachment of an unknown kind {skipped}");
        return Ok(None);
    };
    Ok(Some(ScheduledAttachment {
        kind,
        file_name: row.try_get("file_name")?,
        file: row.try_get("file")?,
        size: row.try_get::<i64, _>("size")?.max(0) as u64,
    }))
}

fn scheduled_from_row(row: &sqlx::sqlite::SqliteRow) -> Result<ScheduledMessage> {
    let extras: String = row.try_get("extras")?;
    Ok(ScheduledMessage {
        id: row.try_get("id")?,
        conversation: ConversationId(row.try_get("conversation_id")?),
        send_at: row.try_get("send_at")?,
        created_at: row.try_get("created_at")?,
        body: row.try_get("body")?,
        // Listed even when its extras can't be read (a newer build's);
        // taking it to send fails instead.
        extras: serde_json::from_str(&extras).unwrap_or_else(|error| {
            tracing::warn!(error = %error, "unreadable scheduled message extras");
            ScheduledExtras::default()
        }),
        attachments: Vec::new(),
    })
}
