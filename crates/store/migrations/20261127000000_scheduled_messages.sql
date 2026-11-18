-- Messages to send later (Signal Android's scheduled messages; Desktop has
-- none): what the composer would have sent, held until `send_at` (unix
-- millis). `extras` is JSON (`ScheduledExtras`): mentions, formatting, the
-- reply and the link preview. A chat can be written to before it has a
-- row of its own, so `conversation_id` has no foreign key; deleting a chat
-- deletes its scheduled messages itself.
CREATE TABLE scheduled_messages (
    id              INTEGER PRIMARY KEY,
    conversation_id TEXT NOT NULL,
    send_at         INTEGER NOT NULL,
    created_at      INTEGER NOT NULL,
    body            TEXT NOT NULL,
    extras          TEXT NOT NULL
);
CREATE INDEX scheduled_messages_send_at ON scheduled_messages (send_at);
CREATE INDEX scheduled_messages_conversation ON scheduled_messages (conversation_id);

-- Their photos and files. The bytes are files in the account's
-- `scheduled/` directory, named `file` (random hex), like received
-- attachments: kept in the database, a big video would hold its one writer
-- while it's written and read. `kind`: 0 a photo, 1 a file, 2 the link
-- preview's image. `size` is in bytes.
CREATE TABLE scheduled_attachments (
    scheduled_id INTEGER NOT NULL REFERENCES scheduled_messages (id) ON DELETE CASCADE,
    position     INTEGER NOT NULL,
    kind         INTEGER NOT NULL,
    file_name    TEXT,
    file         TEXT NOT NULL,
    size         INTEGER NOT NULL,
    PRIMARY KEY (scheduled_id, position)
);
