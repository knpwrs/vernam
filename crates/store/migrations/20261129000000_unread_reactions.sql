-- Someone else reacted to one of our messages since we last read its chat.
-- Desktop marks each row of its `reactions` table unread, but reads them
-- and syncs them per message reacted to (`getUnreadReactionsAndMarkRead`,
-- `markReactionAsRead`), as this flag does.
ALTER TABLE messages ADD COLUMN unread_reactions INTEGER NOT NULL DEFAULT 0;
CREATE INDEX messages_unread_reactions ON messages (conversation_id) WHERE unread_reactions = 1;
