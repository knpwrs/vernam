//! Who "New chat" lists, as Signal Desktop's `getComposableContacts`
//! (`ts/state/selectors/conversations.dom.ts`) picks them: people we can
//! message who have something to show for a name and whom we trust.

use sqlx::Row as _;

use crate::{AppDb, ConversationId, Result};

/// Someone "New chat" lists.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ComposeContact {
    pub service_id: String,
    /// The name shown (their chat's title, else the address book's).
    pub title: String,
    /// Their number, as stored for them.
    pub phone: Option<String>,
    /// Their profile's About, and its emoji.
    pub about: String,
    pub about_emoji: String,
    /// The other names Desktop searches: profile, nickname, username.
    pub profile_given: String,
    pub profile_family: String,
    pub nickname_given: String,
    pub nickname_family: String,
    pub username: Option<String>,
    /// In our address book (Desktop's `isInSystemContacts`).
    pub system_contact: bool,
    /// Ourselves: Note to Self.
    pub me: bool,
    /// When their chat was last active (unix millis; 0 without a chat).
    pub active_at: i64,
    pub archived: bool,
}

impl AppDb {
    /// Everyone "New chat" lists (Desktop's `getComposableContacts` with
    /// `canComposeConversation`): Note to Self, and each person with a 1:1
    /// chat or an address-book entry who isn't blocked, has a name, and is
    /// trusted: in our address book, in a group we haven't left, or someone
    /// we share our profile with (`isTrusted`). `me` is our ACI.
    ///
    /// Not here, as Vernam doesn't keep them: contacts Desktop marks removed
    /// (`removalStage`) beyond the address book's hidden ones, which the
    /// directory already leaves out, and those found unregistered.
    pub async fn compose_contacts(&self, me: &str) -> Result<Vec<ComposeContact>> {
        let rows = sqlx::query(
            "WITH in_groups AS ( \
                 SELECT DISTINCT j.value AS sid FROM conversations g, json_each(g.members) j \
                 WHERE g.kind = 'group' AND g.left_group = 0), \
             people AS ( \
                 SELECT substr(id, 3) AS sid, id, title, phone, last_message_at AS active_at, archived \
                 FROM conversations WHERE kind IN ('direct', 'note_to_self') AND blocked = 0 AND id != ?2 \
                 UNION ALL \
                 SELECT substr(id, 3), id, title, phone, 0, 0 FROM directory \
                 WHERE kind = 'direct' AND id NOT IN (SELECT id FROM conversations)) \
             SELECT p.sid, p.title, p.phone, p.active_at, p.archived, p.sid = ?1 AS me, \
                 EXISTS (SELECT 1 FROM system_contacts s WHERE s.service_id = p.sid) AS system_contact, \
                 COALESCE(pr.about, '') AS about, COALESCE(pr.about_emoji, '') AS about_emoji, \
                 COALESCE(pr.given_name, '') AS profile_given, COALESCE(pr.family_name, '') AS profile_family, \
                 pr.username, COALESCE(n.given, '') AS nickname_given, COALESCE(n.family, '') AS nickname_family \
             FROM people p \
             LEFT JOIN profiles pr ON pr.service_id = p.sid \
             LEFT JOIN contact_nicknames n ON n.service_id = p.sid \
             WHERE p.title != '' AND (p.sid = ?1 \
                 OR EXISTS (SELECT 1 FROM system_contacts s WHERE s.service_id = p.sid) \
                 OR p.sid IN (SELECT sid FROM in_groups) \
                 OR EXISTS (SELECT 1 FROM profile_sharing ps WHERE ps.id = p.id))",
        )
        .bind(me)
        // The Signal chat isn't someone to message.
        .bind(&ConversationId::signal().0)
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|row| {
                Ok(ComposeContact {
                    service_id: row.try_get("sid")?,
                    title: row.try_get("title")?,
                    phone: row.try_get("phone")?,
                    about: row.try_get("about")?,
                    about_emoji: row.try_get("about_emoji")?,
                    profile_given: row.try_get("profile_given")?,
                    profile_family: row.try_get("profile_family")?,
                    nickname_given: row.try_get("nickname_given")?,
                    nickname_family: row.try_get("nickname_family")?,
                    username: row.try_get("username")?,
                    system_contact: row.try_get("system_contact")?,
                    me: row.try_get("me")?,
                    active_at: row.try_get("active_at")?,
                    archived: row.try_get("archived")?,
                })
            })
            .collect()
    }
}
