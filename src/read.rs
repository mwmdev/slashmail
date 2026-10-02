use crate::attachment;
use anyhow::{anyhow, Context, Result};
use mailparse::MailHeaderMap;
use serde::Serialize;
use std::collections::HashMap;

use crate::connection::ImapSession;
use crate::display::{
    sanitize_folder_name, sanitize_terminal_body, sanitize_terminal_field, MessageRow,
};
use crate::search;

pub type MessageBodyMap = HashMap<(Option<String>, String, u32), Vec<u8>>;
pub type DefaultFolderMap = HashMap<Option<String>, String>;

/// Display the full content of messages in the terminal.
pub fn read_messages(
    session: &mut ImapSession,
    messages: &[MessageRow],
    default_folder: &str,
) -> Result<()> {
    let fetched = fetch_message_bodies(session, messages, default_folder)?;

    let mut defaults = DefaultFolderMap::new();
    defaults.insert(None, default_folder.to_string());
    for msg in messages {
        if let Some(account) = &msg.account {
            defaults.insert(Some(account.clone()), default_folder.to_string());
        }
    }

    let mut bodies = MessageBodyMap::new();
    for msg in messages {
        let folder = msg
            .folder
            .clone()
            .unwrap_or_else(|| default_folder.to_string());
        if let Some(body) = fetched.get(&(folder.clone(), msg.uid)) {
            bodies.insert((msg.account.clone(), folder, msg.uid), body.clone());
        }
    }

    print_messages_with_bodies(messages, &defaults, &bodies);
    Ok(())
}

pub fn fetch_message_bodies(
    session: &mut ImapSession,
    messages: &[MessageRow],
    default_folder: &str,
) -> Result<HashMap<(String, u32), Vec<u8>>> {
    // Validate every row's mailbox identity before fetching any body.
    let groups = search::group_message_uids(messages, default_folder)?;

    let mut uid_bodies: HashMap<(String, u32), Vec<u8>> = HashMap::new();

    for (folder, (uid_validity, uids)) in &groups {
        search::examine_verified(session, folder, *uid_validity)?;

        for chunk in &search::build_uid_set(uids) {
            let fetches = session.uid_fetch(chunk, "BODY.PEEK[]").with_context(|| {
                format!(
                    "Failed to fetch messages from '{}'",
                    sanitize_folder_name(folder)
                )
            })?;

            for fetch in fetches.iter() {
                let uid = match fetch.uid {
                    Some(u) => u,
                    None => continue,
                };
                if let Some(body) = fetch.body() {
                    uid_bodies.insert((folder.to_string(), uid), body.to_vec());
                }
            }
        }
    }

    Ok(uid_bodies)
}

pub fn print_messages_with_bodies(
    messages: &[MessageRow],
    default_folders: &DefaultFolderMap,
    bodies: &MessageBodyMap,
) {
    // Print in the original message order (newest first, as returned by search)
    let total = messages.len();
    for (i, msg) in messages.iter().enumerate() {
        let key = message_key(msg, default_folders);

        if let Some(raw) = bodies.get(&key) {
            print_message(raw);
        } else {
            let account = msg
                .account
                .as_deref()
                .map(|name| format!(" in account '{}'", sanitize_terminal_field(name)))
                .unwrap_or_default();
            eprintln!("Warning: could not fetch body for UID {}{account}", msg.uid);
        }

        if i + 1 < total {
            println!("\n{}\n", "─".repeat(60));
        }
    }
}

#[derive(Serialize)]
struct ReadMessageJson<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    account: Option<&'a str>,
    folder: String,
    uid: u32,
    message_id: Option<&'a str>,
    in_reply_to: &'a [String],
    references: &'a [String],
    from: String,
    to: String,
    cc: String,
    date: String,
    timestamp: i64,
    subject: String,
    seen: bool,
    answered: bool,
    flagged: bool,
    body: String,
    attachments: Vec<attachment::AttachmentSummary>,
}

/// Render fetched messages as a JSON array, in the order given.
///
/// Unlike terminal output, a missing, unparseable, or undecodable body is an
/// error so scripted callers never mistake raw or partial content for a
/// decoded result.
pub fn render_messages_json(
    messages: &[MessageRow],
    default_folders: &DefaultFolderMap,
    bodies: &MessageBodyMap,
) -> Result<String> {
    let mut rendered = Vec::with_capacity(messages.len());
    for msg in messages {
        let key = message_key(msg, default_folders);
        let raw = bodies
            .get(&key)
            .ok_or_else(|| anyhow!("Could not fetch body for UID {}", msg.uid))?;
        let parsed = mailparse::parse_mail(raw)
            .with_context(|| format!("Failed to parse message UID {}", msg.uid))?;
        let header = |name: &str| parsed.headers.get_first_value(name).unwrap_or_default();
        rendered.push(ReadMessageJson {
            account: msg.account.as_deref(),
            folder: key.1,
            uid: msg.uid,
            message_id: msg.message_id.as_deref(),
            in_reply_to: &msg.in_reply_to,
            references: &msg.references,
            from: header("From"),
            to: header("To"),
            cc: header("Cc"),
            date: header("Date"),
            timestamp: msg.timestamp,
            subject: header("Subject"),
            seen: msg.seen,
            answered: msg.answered,
            flagged: msg.flagged,
            body: decoded_text_body(&parsed)
                .with_context(|| format!("Failed to decode body of message UID {}", msg.uid))?
                .unwrap_or_default()
                .trim_end()
                .to_string(),
            attachments: attachment::attachment_summaries(&parsed),
        });
    }
    serde_json::to_string(&rendered).context("Failed to serialize messages as JSON")
}

pub fn message_key(
    msg: &MessageRow,
    default_folders: &DefaultFolderMap,
) -> (Option<String>, String, u32) {
    let folder = msg.folder.clone().unwrap_or_else(|| {
        default_folders
            .get(&msg.account)
            .cloned()
            .unwrap_or_else(|| "INBOX".to_string())
    });
    (msg.account.clone(), folder, msg.uid)
}

fn print_message(raw: &[u8]) {
    let parsed = match mailparse::parse_mail(raw) {
        Ok(m) => m,
        Err(e) => {
            eprintln!(
                "Warning: failed to parse message: {}",
                sanitize_terminal_field(&e.to_string())
            );
            let text = String::from_utf8_lossy(raw);
            println!("{}", sanitize_terminal_body(&text));
            return;
        }
    };

    // Extract headers
    let get_header = |name: &str| -> String {
        for h in &parsed.headers {
            if h.get_key().eq_ignore_ascii_case(name) {
                return sanitize_terminal_field(&h.get_value());
            }
        }
        String::new()
    };

    let from = get_header("From");
    let to = get_header("To");
    let cc = get_header("Cc");
    let date = get_header("Date");
    let subject = get_header("Subject");

    println!("From:    {from}");
    println!("To:      {to}");
    if !cc.is_empty() {
        println!("Cc:      {cc}");
    }
    println!("Date:    {date}");
    println!("Subject: {subject}");
    println!();

    // Extract body text and attachment names
    let (text, attachments) = extract_body(&parsed);

    if text.is_empty() {
        println!("[No text content]");
    } else {
        println!("{}", sanitize_terminal_body(text.trim_end()));
    }

    if let Some(summary) = render_attachment_summary(&attachments) {
        println!("\n{summary}");
    }
}

fn render_attachment_summary(attachments: &[String]) -> Option<String> {
    if attachments.is_empty() {
        return None;
    }

    let names = attachments
        .iter()
        .map(|name| crate::display::sanitize_terminal_field(name))
        .collect::<Vec<_>>()
        .join(", ");
    Some(format!(
        "[{} attachment{}: {names}]",
        attachments.len(),
        if attachments.len() == 1 { "" } else { "s" }
    ))
}

fn extract_body(parsed: &mailparse::ParsedMail) -> (String, Vec<String>) {
    let attachments = attachment::attachment_names(parsed);

    (display_text_body(parsed), attachments)
}

fn display_text_body(parsed: &mailparse::ParsedMail) -> String {
    collect_body_text(parsed, BodyMode::Display)
        .expect("display mode handles decode errors")
        .unwrap_or_default()
}

#[derive(Clone, Copy)]
enum BodyMode {
    Display,
    Strict,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Representation {
    Plain,
    Html,
}

fn body_representation(part: &mailparse::ParsedMail<'_>) -> Option<Representation> {
    if attachment::is_attachment(part) {
        return None;
    }
    match part.ctype.mimetype.to_ascii_lowercase().as_str() {
        "text/plain" => Some(Representation::Plain),
        "text/html" => Some(Representation::Html),
        "multipart/related" => related_root(part).and_then(body_representation),
        "multipart/alternative" => {
            let mut preferred = None;
            for child in &part.subparts {
                match body_representation(child) {
                    Some(Representation::Plain) => preferred = Some(Representation::Plain),
                    Some(Representation::Html) if preferred.is_none() => {
                        preferred = Some(Representation::Html);
                    }
                    _ => {}
                }
            }
            preferred
        }
        mime if mime.starts_with("multipart/") => {
            let mut representation = None;
            for child in &part.subparts {
                match body_representation(child) {
                    Some(Representation::Plain) => return Some(Representation::Plain),
                    Some(Representation::Html) => representation = Some(Representation::Html),
                    None => {}
                }
            }
            representation
        }
        _ => None,
    }
}

fn related_root<'a>(part: &'a mailparse::ParsedMail<'_>) -> Option<&'a mailparse::ParsedMail<'a>> {
    let start = part.ctype.params.get("start");
    start
        .and_then(|start| {
            let wanted = start.trim().trim_start_matches('<').trim_end_matches('>');
            part.subparts.iter().find(|child| {
                child
                    .headers
                    .get_first_value("Content-ID")
                    .is_some_and(|id| {
                        id.trim().trim_start_matches('<').trim_end_matches('>') == wanted
                    })
            })
        })
        .or_else(|| part.subparts.first())
}

fn collect_body_text(part: &mailparse::ParsedMail<'_>, mode: BodyMode) -> Result<Option<String>> {
    if attachment::is_attachment(part) {
        return Ok(None);
    }
    let mime = part.ctype.mimetype.to_ascii_lowercase();
    match mime.as_str() {
        "text/plain" | "text/html" => {
            let text = match mode {
                BodyMode::Display => display_part_text(part, &mime),
                BodyMode::Strict => part
                    .get_body()
                    .with_context(|| format!("Failed to decode {mime} body"))?,
            };
            if mime == "text/plain" {
                return Ok(Some(text));
            }
            let converted = match html2text::from_read(text.as_bytes(), 80) {
                Ok(converted) => converted,
                Err(error) => match mode {
                    BodyMode::Strict => {
                        return Err(error).context("Failed to convert HTML body to text");
                    }
                    BodyMode::Display => {
                        eprintln!(
                            "Warning: failed to convert HTML body to text: {}",
                            sanitize_terminal_field(&error.to_string())
                        );
                        text
                    }
                },
            };
            Ok(Some(converted))
        }
        "multipart/related" => related_root(part)
            .map(|root| collect_body_text(root, mode))
            .unwrap_or(Ok(None)),
        "multipart/alternative" => {
            let mut plain = None;
            let mut html = None;
            for child in &part.subparts {
                match body_representation(child) {
                    Some(Representation::Plain) => plain = Some(child),
                    Some(Representation::Html) => html = Some(child),
                    None => {}
                }
            }
            match plain.or(html) {
                Some(child) => collect_body_text(child, mode),
                None => Ok(None),
            }
        }
        mime if mime.starts_with("multipart/") => {
            let mut combined: Option<String> = None;
            for child in &part.subparts {
                if let Some(text) = collect_body_text(child, mode)? {
                    if text.trim_end_matches(['\r', '\n']).is_empty() {
                        continue;
                    }
                    match &mut combined {
                        Some(previous) => {
                            previous.truncate(previous.trim_end_matches(['\r', '\n']).len());
                            previous.push_str("\n\n");
                            previous.push_str(&text);
                        }
                        None => combined = Some(text),
                    }
                }
            }
            Ok(combined)
        }
        _ => Ok(None),
    }
}

fn display_part_text(part: &mailparse::ParsedMail, mime: &str) -> String {
    part.get_body().unwrap_or_else(|error| {
        eprintln!(
            "Warning: failed to decode {mime} body: {}",
            sanitize_terminal_field(&error.to_string())
        );
        raw_part_text(part)
    })
}

fn raw_part_text(part: &mailparse::ParsedMail) -> String {
    use mailparse::body::Body;

    let body = part.get_body_encoded();
    let raw = match &body {
        Body::Base64(body) | Body::QuotedPrintable(body) => body.get_raw(),
        Body::SevenBit(body) | Body::EightBit(body) => body.get_raw(),
        Body::Binary(body) => body.get_raw(),
    };
    String::from_utf8_lossy(raw).into_owned()
}

/// Decode the selected MIME body without silently substituting undecodable alternatives.
pub(crate) fn decoded_text_body(parsed: &mailparse::ParsedMail) -> Result<Option<String>> {
    collect_body_text(parsed, BodyMode::Strict)
}

#[cfg(test)]
pub(crate) fn nested_multipart_fixture(depth: usize) -> Vec<u8> {
    let mut message = b"Content-Type: text/plain\r\n\r\nDeep body".to_vec();
    for index in 0..depth {
        let boundary = format!("depth{index}");
        let mut outer =
            format!("Content-Type: multipart/mixed; boundary={boundary}\r\n\r\n--{boundary}\r\n")
                .into_bytes();
        outer.extend_from_slice(&message);
        outer.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
        message = outer;
    }
    message
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn structural_body_selection_composes_mixed_and_limits_alternatives_and_related() {
        let raw = b"Content-Type: multipart/mixed; boundary=outer\r\n\r\n\
--outer\r\nContent-Type: text/plain\r\n\r\nIntroduction\r\n\
--outer\r\nContent-Type: multipart/alternative; boundary=alt\r\n\r\n\
--alt\r\nContent-Type: text/html\r\n\r\n<p>Wrong alternative</p>\r\n\
--alt\r\nContent-Type: text/plain\r\n\r\nFinal instructions\r\n--alt--\r\n\
--outer\r\nContent-Type: multipart/related; boundary=rel; start=\"<root@site>\"\r\n\r\n\
--rel\r\nContent-Type: text/plain\r\nContent-ID: <resource@site>\r\n\r\nResource must not leak\r\n\
--rel\r\nContent-Type: text/html\r\nContent-ID: <root@site>\r\n\r\n<p>Root content</p>\r\n\
--rel--\r\n--outer--";
        let parsed = mailparse::parse_mail(raw).unwrap();
        for text in [
            decoded_text_body(&parsed).unwrap().unwrap(),
            display_text_body(&parsed),
        ] {
            assert!(
                text.starts_with("Introduction\n\nFinal instructions\n\nRoot content"),
                "{text}"
            );
            assert!(!text.contains("Wrong alternative"));
            assert!(!text.contains("Resource must not leak"));
        }
    }

    #[test]
    fn unselected_alternative_decode_error_is_not_visited() {
        let raw = b"Content-Type: multipart/alternative; boundary=x\r\n\r\n\
--x\r\nContent-Type: text/html\r\nContent-Transfer-Encoding: base64\r\n\r\n%%%\r\n\
--x\r\nContent-Type: text/plain\r\n\r\nSelected\r\n--x--";
        let parsed = mailparse::parse_mail(raw).unwrap();
        assert_eq!(
            decoded_text_body(&parsed).unwrap().as_deref(),
            Some("Selected")
        );
    }

    #[test]
    fn alternative_picks_last_plain_and_selected_decode_failure_is_strict() {
        let raw = b"Content-Type: multipart/alternative; boundary=x\r\n\r\n\
--x\r\nContent-Type: text/plain\r\n\r\nOld\r\n\
--x\r\nContent-Type: text/plain\r\n\r\nNew\r\n--x--";
        let parsed = mailparse::parse_mail(raw).unwrap();
        assert_eq!(decoded_text_body(&parsed).unwrap().as_deref(), Some("New"));

        let broken = b"Content-Type: multipart/alternative; boundary=x\r\n\r\n\
--x\r\nContent-Type: text/plain\r\n\r\nOld\r\n\
--x\r\nContent-Type: text/plain\r\nContent-Transfer-Encoding: base64\r\n\r\n%%%\r\n--x--";
        let parsed = mailparse::parse_mail(broken).unwrap();
        assert!(decoded_text_body(&parsed)
            .unwrap_err()
            .to_string()
            .contains("text/plain"));
        assert!(display_text_body(&parsed).contains("%%%"));
    }

    #[test]
    fn upstream_depth_limit_controls_decoded_body() {
        let accepted = nested_multipart_fixture(100);
        let parsed = mailparse::parse_mail(&accepted).unwrap();
        assert_eq!(
            decoded_text_body(&parsed).unwrap().as_deref(),
            Some("Deep body")
        );
        let rejected = nested_multipart_fixture(101);
        assert!(mailparse::parse_mail(&rejected).is_err());
    }

    #[test]
    fn extract_body_plain_text() {
        let raw = b"Content-Type: text/plain\r\n\r\nHello world";
        let parsed = mailparse::parse_mail(raw).unwrap();
        let (text, attachments) = extract_body(&parsed);
        assert_eq!(text, "Hello world");
        assert!(attachments.is_empty());
    }

    fn row(folder: Option<&str>) -> MessageRow {
        MessageRow {
            account: Some("work".to_string()),
            uid: 42,
            folder: folder.map(str::to_string),
            from: String::new(),
            subject: String::new(),
            date: String::new(),
            timestamp: 0,
            size: 0,
            message_id: None,
            in_reply_to: Vec::new(),
            references: Vec::new(),
            seen: false,
            answered: false,
            flagged: false,
            uid_validity: None,
            gmail_msgid: None,
            arrival: None,
        }
    }

    #[test]
    fn message_key_uses_account_fallback_folder() {
        let mut defaults = DefaultFolderMap::new();
        defaults.insert(Some("work".to_string()), "Sent".to_string());
        let msg = row(None);

        assert_eq!(
            message_key(&msg, &defaults),
            (Some("work".to_string()), "Sent".to_string(), 42)
        );
    }

    #[test]
    fn message_key_prefers_explicit_folder() {
        let mut defaults = DefaultFolderMap::new();
        defaults.insert(Some("work".to_string()), "Sent".to_string());
        let msg = row(Some("Archive"));

        assert_eq!(
            message_key(&msg, &defaults),
            (Some("work".to_string()), "Archive".to_string(), 42)
        );
    }

    #[test]
    fn json_reports_full_headers_resolved_folder_body_and_attachments() {
        let long_subject = "S".repeat(120);
        let raw = format!(
            "From: A Very Long Sender Name <a.very.long.sender.address@example.com>\r\n\
To: me@example.com\r\n\
Subject: {long_subject}\r\n\
Date: Mon, 1 Jun 2026 10:00:00 +0200\r\n\
Content-Type: multipart/mixed; boundary=b\r\n\r\n\
--b\r\nContent-Type: text/plain\r\n\r\nSee screenshot\r\n\
--b\r\nContent-Type: image/png\r\nContent-Disposition: attachment; filename=shot.png\r\n\
Content-Transfer-Encoding: base64\r\n\r\niVBORw0K\r\n--b--\r\n"
        );
        let mut defaults = DefaultFolderMap::new();
        defaults.insert(Some("work".to_string()), "INBOX".to_string());
        let mut bodies = MessageBodyMap::new();
        bodies.insert(
            (Some("work".to_string()), "INBOX".to_string(), 42),
            raw.into_bytes(),
        );
        let msg = MessageRow {
            message_id: Some("<m@example.com>".to_string()),
            references: vec!["<root@example.com>".to_string()],
            answered: true,
            ..row(None)
        };

        let json: serde_json::Value =
            serde_json::from_str(&render_messages_json(&[msg], &defaults, &bodies).unwrap())
                .unwrap();
        let message = &json[0];
        assert_eq!(message["folder"], "INBOX");
        assert_eq!(message["account"], "work");
        assert_eq!(message["subject"], long_subject);
        assert_eq!(
            message["from"],
            "A Very Long Sender Name <a.very.long.sender.address@example.com>"
        );
        assert_eq!(message["message_id"], "<m@example.com>");
        assert_eq!(message["references"][0], "<root@example.com>");
        assert_eq!(message["answered"], true);
        assert_eq!(message["body"].as_str().unwrap().trim(), "See screenshot");
        assert_eq!(
            message["attachments"],
            serde_json::json!([{
                "part": "2",
                "filename": "shot.png",
                "content_type": "image/png",
                "size": 6
            }])
        );
    }

    #[test]
    fn json_fails_instead_of_omitting_unfetched_message() {
        let defaults = DefaultFolderMap::new();
        let error = render_messages_json(&[row(Some("INBOX"))], &defaults, &MessageBodyMap::new())
            .unwrap_err();
        assert!(error.to_string().contains("UID 42"));
    }

    #[test]
    fn json_fails_instead_of_returning_undecodable_body_as_text() {
        let mut bodies = MessageBodyMap::new();
        bodies.insert(
            (Some("work".to_string()), "INBOX".to_string(), 42),
            b"Content-Type: text/plain\r\nContent-Transfer-Encoding: base64\r\n\r\n%%%invalid%%%"
                .to_vec(),
        );
        let error = render_messages_json(&[row(Some("INBOX"))], &DefaultFolderMap::new(), &bodies)
            .unwrap_err();
        assert!(format!("{error:#}").contains("Failed to decode body of message UID 42"));
    }

    #[test]
    fn extract_body_html_only() {
        let raw = b"Content-Type: text/html\r\n\r\n<p>Hello world</p>";
        let parsed = mailparse::parse_mail(raw).unwrap();
        let (text, attachments) = extract_body(&parsed);
        assert!(text.contains("Hello world"));
        assert!(attachments.is_empty());
    }

    #[test]
    fn extract_body_multipart_prefers_plain() {
        let raw = b"Content-Type: multipart/alternative; boundary=bound\r\n\r\n\
--bound\r\nContent-Type: text/plain\r\n\r\nPlain text\r\n\
--bound\r\nContent-Type: text/html\r\n\r\n<p>HTML text</p>\r\n\
--bound--";
        let parsed = mailparse::parse_mail(raw).unwrap();
        let (text, _) = extract_body(&parsed);
        assert!(text.trim() == "Plain text");
    }

    #[test]
    fn decoded_text_body_prefers_nested_encoded_plain_and_skips_attachments() {
        let raw = b"Content-Type: multipart/mixed; boundary=outer\r\n\r\n\
--outer\r\n\
Content-Type: text/plain; name=attached.txt\r\n\
Content-Disposition: attachment; filename=attached.txt\r\n\r\n\
Do not quote this\r\n\
--outer\r\n\
Content-Type: multipart/alternative; boundary=inner\r\n\r\n\
--inner\r\n\
Content-Type: text/html; charset=utf-8\r\n\
Content-Transfer-Encoding: quoted-printable\r\n\r\n\
<p>HTML=20fallback</p>\r\n\
--inner\r\n\
Content-Type: text/plain; charset=utf-8\r\n\
Content-Transfer-Encoding: base64\r\n\r\n\
UGxhaW4gw6lsw6l2w6k=\r\n\
--inner--\r\n\
--outer--";
        let parsed = mailparse::parse_mail(raw).unwrap();

        assert_eq!(
            decoded_text_body(&parsed).unwrap().unwrap().trim(),
            "Plain élévé"
        );
    }

    #[test]
    fn decoded_text_body_converts_html_without_returning_markup() {
        let raw = b"Content-Type: text/html; charset=utf-8\r\n\r\n\
<p>Hello <strong>world</strong></p>";
        let parsed = mailparse::parse_mail(raw).unwrap();
        let text = decoded_text_body(&parsed).unwrap().unwrap();

        assert!(text.contains("Hello"));
        assert!(text.contains("world"));
        assert!(!text.contains("<strong>"));
    }

    #[test]
    fn decoded_text_body_returns_none_for_attachment_only_message() {
        let raw = b"Content-Type: text/plain\r\n\
Content-Disposition: attachment; filename=note.txt\r\n\r\n\
Attached text";
        let parsed = mailparse::parse_mail(raw).unwrap();

        assert_eq!(decoded_text_body(&parsed).unwrap(), None);
    }

    #[test]
    fn decoded_text_body_returns_transfer_decode_errors() {
        let raw = b"Content-Type: text/plain\r\n\
Content-Transfer-Encoding: base64\r\n\r\n\
%%%invalid%%%";
        let parsed = mailparse::parse_mail(raw).unwrap();

        assert!(decoded_text_body(&parsed).is_err());
    }

    #[test]
    fn extract_body_falls_back_to_raw_malformed_base64() {
        let raw = b"Content-Type: text/plain\r\n\
Content-Transfer-Encoding: base64\r\n\r\n\
%%%invalid%%%";
        let parsed = mailparse::parse_mail(raw).unwrap();

        let (text, attachments) = extract_body(&parsed);

        assert_eq!(text, "%%%invalid%%%");
        assert!(attachments.is_empty());
    }

    #[test]
    fn extract_body_no_text() {
        let raw = b"Content-Type: application/pdf\r\nContent-Disposition: attachment; filename=\"doc.pdf\"\r\n\r\nbinary";
        let parsed = mailparse::parse_mail(raw).unwrap();
        let (text, attachments) = extract_body(&parsed);
        assert!(text.is_empty());
        assert_eq!(attachments, vec!["doc.pdf"]);
    }

    #[test]
    fn extract_body_decodes_rfc2231_filename_and_prefers_it_to_type_name() {
        let raw = b"Content-Type: multipart/mixed; boundary=bound\r\n\r\n\
--bound\r\n\
Content-Type: text/plain; charset=utf-8\r\n\r\n\
Visible body\r\n\
--bound\r\n\
Content-Type: application/pdf; name=\"compatibility-fallback.pdf\"\r\n\
Content-Disposition: attachment;\r\n\
\tfilename*0*=utf-8''r%C3%A9sum%C3%A9%20;\r\n\
\tfilename*1*=final.pdf\r\n\r\n\
hidden attachment text\r\n\
--bound--";
        let parsed = mailparse::parse_mail(raw).unwrap();

        let (text, attachments) = extract_body(&parsed);

        assert_eq!(text.trim(), "Visible body");
        assert_eq!(attachments, ["résumé final.pdf"]);
        assert_eq!(
            decoded_text_body(&parsed).unwrap().unwrap().trim(),
            "Visible body"
        );
    }

    #[test]
    fn attachment_summary_sanitizes_decoded_rfc2231_terminal_controls() {
        let raw = b"Content-Type: application/octet-stream\r\n\
Content-Disposition: attachment;\r\n\
\tfilename*=utf-8''safe%1B%5D52%3Bc%3Bsecret%07%0A%E2%80%AEtail.txt\r\n\r\n\
bytes";
        let parsed = mailparse::parse_mail(raw).unwrap();
        let (_, attachments) = extract_body(&parsed);

        assert!(attachments[0].contains('\u{1b}'));
        let summary = render_attachment_summary(&attachments).unwrap();
        assert_eq!(summary, "[1 attachment: safe  tail.txt]");
        assert!(!summary.chars().any(char::is_control));
        assert!(!summary.contains('\u{202e}'));
        assert!(!summary.contains("secret"));
    }

    #[test]
    fn attachment_name_uses_content_type_name_as_compatibility_fallback() {
        let raw = b"Content-Type: application/octet-stream; name=\"legacy.bin\"\r\n\
Content-Disposition: attachment\r\n\r\n\
bytes";
        let parsed = mailparse::parse_mail(raw).unwrap();

        let (text, attachments) = extract_body(&parsed);

        assert!(text.is_empty());
        assert_eq!(attachments, ["legacy.bin"]);
    }

    #[test]
    fn unnamed_attachment_is_still_classified_and_excluded_from_text() {
        let raw = b"Content-Type: text/plain\r\n\
Content-Disposition: attachment\r\n\r\n\
hidden text";
        let parsed = mailparse::parse_mail(raw).unwrap();

        let (text, attachments) = extract_body(&parsed);

        assert!(text.is_empty());
        assert_eq!(attachments, ["unnamed"]);
        assert_eq!(decoded_text_body(&parsed).unwrap(), None);
    }
}
