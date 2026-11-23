-- Reactions move out of `messages.reactions` (JSON) into a table, as
-- Desktop's `reactions`: one row per person per message, each unread on its
-- own (Desktop's `addReaction`) and naming the timestamp the reactor
-- targeted (`targetTimestamp`), which read syncs for it carry.
CREATE TABLE reactions (
    message_id     INTEGER NOT NULL REFERENCES messages (id) ON DELETE CASCADE,
    -- Its message's, so a chat's unread reactions are one index probe
    -- (Desktop's `conversationId`); messages never move between chats.
    conversation_id TEXT NOT NULL,
    -- The reactor's service id; '' for us, as `messages.author_key`.
    author         TEXT NOT NULL,
    emoji          TEXT NOT NULL,
    sent_at        INTEGER NOT NULL,
    -- The timestamp the reaction named its message by: the original's or
    -- one of its edits'.
    target_sent_at INTEGER NOT NULL,
    -- Someone else's reaction to one of our messages, not yet read.
    unread         INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (message_id, author)
) STRICT;

CREATE INDEX reactions_unread ON reactions (conversation_id) WHERE unread = 1;

-- A deleted message takes no reactions and has none unread; one deleted
-- with "Keep deleted messages" on keeps the ones it had until it's erased.
CREATE TRIGGER messages_deleted_reactions AFTER UPDATE OF deleted, kept_deleted ON messages
WHEN new.deleted = 1
BEGIN
    DELETE FROM reactions WHERE message_id = new.id AND new.kept_deleted = 0;
    UPDATE reactions SET unread = 0 WHERE message_id = new.id AND unread = 1;
END;

-- Every reaction so far, unread where its message was, named by its
-- message's latest timestamp (an edit's, if any), as read syncs named them
-- until now: which one a reactor named wasn't kept.
INSERT OR REPLACE INTO reactions (message_id, conversation_id, author, emoji, sent_at, target_sent_at, unread)
SELECT m.id, m.conversation_id, COALESCE(json_extract(r.value, '$.author'), ''), json_extract(r.value, '$.emoji'),
       json_extract(r.value, '$.sent_at'),
       COALESCE((SELECT MAX(e.sent_at) FROM message_edits e WHERE e.message_id = m.id), m.sent_at),
       m.unread_reactions AND json_extract(r.value, '$.author') IS NOT NULL
FROM messages m, json_each(m.reactions) r
WHERE m.reactions <> '[]';

DROP INDEX messages_unread_reactions;
ALTER TABLE messages DROP COLUMN unread_reactions;
ALTER TABLE messages DROP COLUMN reactions;
