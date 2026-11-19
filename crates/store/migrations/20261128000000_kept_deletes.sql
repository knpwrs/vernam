-- 1 when someone else deleted a message for everyone while "Keep deleted
-- messages" was on: it's deleted (`deleted` = 1, so we send no reply or
-- reaction to it) but its text, attachments and edit history stay to be
-- shown. Not a Signal Desktop feature.
ALTER TABLE messages ADD COLUMN kept_deleted INTEGER NOT NULL DEFAULT 0;
-- Finds them for "Erase them" without reading every message.
CREATE INDEX messages_kept_deleted ON messages(id) WHERE kept_deleted = 1;
