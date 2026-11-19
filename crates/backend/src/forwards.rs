//! What a forward sends: a copy of a message's text, attachments and link
//! preview, or, when one message is forwarded, what the forward editor left
//! of them (Desktop's `ForwardMessagesModal` and `ts/types/ForwardDraft`).

use signal_store::{AttachmentMeta, LinkPreview, Message};

/// One forwarded message as changed in the forward editor: its new text,
/// the attachments kept (by index into the message's), and whether its link
/// preview stays.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForwardEdit {
    pub body: String,
    pub attachments: Vec<usize>,
    pub link_preview: bool,
}

/// What a forward of `message` sends: its text, attachments and link
/// preview, after `edit`. `None` when it can't be forwarded: deleted, a poll
/// or view-once (Desktop's `canForward`), a kept attachment that's a sticker
/// or hasn't downloaded (`isDraftForwardable`), nothing left to send
/// (`isDraftEmpty`), or text longer than a message may be. Attachments go as
/// they are, never re-encoded (Desktop reuses the file), whether or not the
/// original's re-encode has happened.
pub fn content(message: &Message, edit: Option<&ForwardEdit>) -> Option<(String, Vec<AttachmentMeta>, Option<LinkPreview>)> {
    if message.deleted || message.poll.is_some() || message.view_once.is_some() {
        return None;
    }
    let kept: Vec<&AttachmentMeta> = match edit {
        Some(edit) => message.attachments.iter().enumerate().filter(|(ix, _)| edit.attachments.contains(ix)).map(|(_, a)| a).collect(),
        None => message.attachments.iter().collect(),
    };
    if kept.iter().any(|a| a.local_path.is_none() || a.sticker) {
        return None;
    }
    let attachments: Vec<AttachmentMeta> = kept.into_iter().map(|a| AttachmentMeta { transcode: false, ..a.clone() }).collect();
    let body = match edit {
        // Only spaces left is no text; trailing ones go, as Send's do.
        Some(edit) => edit.body.trim_end().to_owned(),
        None => message.body.clone(),
    };
    // Longer than any message may be (the composer's limit too).
    if body.len() > crate::long_text::MAX_TEXT_BYTES {
        return None;
    }
    let link_preview = message
        .link_preview
        .clone()
        // Unchanged text keeps it as it came (an imported one may not match
        // its link exactly).
        .filter(|preview| edit.is_none_or(|edit| edit.link_preview && (body == message.body.trim_end() || links_to(&body, &preview.url))));
    if body.is_empty() && attachments.is_empty() {
        return None;
    }
    Some((body, attachments, link_preview))
}

/// Whether edited text still has a preview's link, whole (a preview must
/// name a link the message has, as Desktop's `isValidLinkPreview` checks):
/// "https://example.com/abc" isn't "https://example.com/a".
pub fn links_to(body: &str, url: &str) -> bool {
    crate::link_preview::links_in(body).iter().any(|link| link == url)
}

/// Whether the forward editor offers to change `message` before it goes
/// (Desktop's `isDraftEditable`): not a sticker, voice message or shared
/// contact, nor anything that can't be forwarded at all.
pub fn editable(message: &Message) -> bool {
    content(message, None).is_some()
        && message.content.is_none()
        && !message.attachments.iter().any(|a| a.sticker || a.voice)
}

#[cfg(test)]
mod tests {
    use super::*;
    use signal_store::{ConversationId, DeliveryStatus, Direction};

    fn file(name: &str, downloaded: bool) -> AttachmentMeta {
        AttachmentMeta {
            content_type: "image/png".into(),
            file_name: Some(name.into()),
            size: 1,
            width: None,
            height: None,
            local_path: downloaded.then(|| format!("/tmp/{name}")),
            sticker: false,
            gif: false,
            voice: false,
            remote: None,
            thumbnail_path: None,
            sticker_pack: None,
            transcode: true,
        }
    }

    fn message(body: &str, attachments: Vec<AttachmentMeta>) -> Message {
        Message {
            id: 1,
            conversation_id: ConversationId("d:a".into()),
            sent_at: 1,
            received_at: 1,
            direction: Direction::Incoming,
            author: Some("a".into()),
            author_name: None,
            body: body.into(),
            status: DeliveryStatus::Read,
            reactions: Vec::new(),
            quote: None,
            attachments,
            link_preview: None,
            deleted: false,
            kept_deleted: false,
            edited: false,
            expires_in: 0,
            expire_start: None,
            edit_count: 0,
            notice: None,
            mentions: Vec::new(),
            styles: Vec::new(),
            cta: None,
            deleted_by_admin: None,
            long_text: None,
            poll: None,
            view_once: None,
            content: None,
            sealed_sender: Vec::new(),
            is_read: true,
        }
    }

    fn edit(body: &str, attachments: &[usize], link_preview: bool) -> ForwardEdit {
        ForwardEdit { body: body.into(), attachments: attachments.to_vec(), link_preview }
    }

    #[test]
    fn a_forward_copies_the_message_unless_edited() {
        let photo = message("look", vec![file("a.png", true)]);
        let (body, attachments, _) = content(&photo, None).unwrap();
        assert_eq!(body, "look");
        assert_eq!(attachments.len(), 1);
        assert!(!attachments[0].transcode, "sent as the file is");

        // The text erased: the photo goes alone.
        let (body, attachments, _) = content(&photo, Some(&edit("  ", &[0], true))).unwrap();
        assert!(body.is_empty());
        assert_eq!(attachments.len(), 1);
        // New text.
        assert_eq!(content(&photo, Some(&edit("for you", &[0], true))).unwrap().0, "for you");
        // The photo removed, the text stays.
        let (body, attachments, _) = content(&photo, Some(&edit("look", &[], true))).unwrap();
        assert_eq!((body.as_str(), attachments.len()), ("look", 0));
        assert_eq!(content(&photo, Some(&edit("for you\n\n", &[0], true))).unwrap().0, "for you");
        let long = "x".repeat(crate::long_text::MAX_TEXT_BYTES + 1);
        assert_eq!(content(&photo, Some(&edit(&long, &[0], true))), None, "too long to send");
        // Nothing left.
        assert_eq!(content(&photo, Some(&edit("", &[], true))), None);
    }

    #[test]
    fn only_downloaded_files_that_arent_stickers_go() {
        let two = message("", vec![file("a.png", true), file("b.png", false)]);
        assert_eq!(content(&two, None), None, "one hasn't downloaded");
        // Removing the one that hasn't lets the other go.
        let (_, attachments, _) = content(&two, Some(&edit("", &[0], true))).unwrap();
        assert_eq!(attachments[0].file_name.as_deref(), Some("a.png"));
        let sticker = message("", vec![AttachmentMeta { sticker: true, ..file("s.webp", true) }]);
        assert_eq!(content(&sticker, None), None);
        assert!(!editable(&sticker));
        let voice = message("", vec![AttachmentMeta { voice: true, ..file("v.m4a", true) }]);
        assert!(content(&voice, None).is_some() && !editable(&voice), "forwarded as it is");
        assert!(editable(&message("hi", Vec::new())));
        assert!(!editable(&Message { deleted: true, ..message("hi", Vec::new()) }));
    }

    #[test]
    fn a_link_preview_stays_while_its_link_does() {
        let preview = LinkPreview {
            url: "https://example.com/a".into(),
            title: Some("A".into()),
            description: None,
            date: None,
            image: None,
        };
        let link = Message { link_preview: Some(preview.clone()), ..message("see https://example.com/a", Vec::new()) };
        assert_eq!(content(&link, None).unwrap().2, Some(preview.clone()));
        assert_eq!(content(&link, Some(&edit("https://example.com/a !", &[], true))).unwrap().2, Some(preview.clone()));
        assert_eq!(content(&link, Some(&edit("see https://example.com/a", &[], false))).unwrap().2, None, "removed");
        assert_eq!(content(&link, Some(&edit("see it", &[], true))).unwrap().2, None, "its link is gone");
        assert_eq!(content(&link, Some(&edit("see https://example.com/abc", &[], true))).unwrap().2, None, "another link");
        let imported = Message { link_preview: Some(LinkPreview { url: "https://example.com/a/".into(), ..preview.clone() }), ..link.clone() };
        assert!(content(&imported, Some(&edit("see https://example.com/a", &[], true))).unwrap().2.is_some(), "text unchanged");
    }
}
