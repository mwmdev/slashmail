use anyhow::{Context, Result};
use std::collections::HashMap;

use crate::connection::ImapSession;
use crate::display::MessageRow;
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
    let mut by_folder: HashMap<String, Vec<u32>> = HashMap::new();
    for msg in messages {
        let folder = msg
            .folder
            .clone()
            .unwrap_or_else(|| default_folder.to_string());
        by_folder.entry(folder).or_default().push(msg.uid);
    }

    let mut uid_bodies: HashMap<(String, u32), Vec<u8>> = HashMap::new();

    for (folder, uids) in &by_folder {
        session
            .select(folder)
            .with_context(|| format!("Failed to select '{folder}'"))?;

        for chunk in &search::build_uid_set(uids) {
            let fetches = session
                .uid_fetch(chunk, "BODY.PEEK[]")
                .with_context(|| format!("Failed to fetch messages from '{folder}'"))?;

            for fetch in fetches.iter() {
                let uid = match fetch.uid {
                    Some(u) => u,
                    None => continue,
                };
                if let Some(body) = fetch.body() {
                    uid_bodies.insert((folder.clone(), uid), body.to_vec());
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
                .map(|name| format!(" in account '{name}'"))
                .unwrap_or_default();
            eprintln!("Warning: could not fetch body for UID {}{account}", msg.uid);
        }

        if i + 1 < total {
            println!("\n{}\n", "─".repeat(60));
        }
    }
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
            eprintln!("Warning: failed to parse message: {e}");
            let text = String::from_utf8_lossy(raw);
            println!("{text}");
            return;
        }
    };

    // Extract headers
    let get_header = |name: &str| -> String {
        for h in &parsed.headers {
            if h.get_key().eq_ignore_ascii_case(name) {
                return h.get_value();
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
        println!("{}", text.trim_end());
    }

    if !attachments.is_empty() {
        println!(
            "\n[{} attachment{}: {}]",
            attachments.len(),
            if attachments.len() == 1 { "" } else { "s" },
            attachments.join(", ")
        );
    }
}

fn extract_body(parsed: &mailparse::ParsedMail) -> (String, Vec<String>) {
    let mut attachments = Vec::new();
    collect_attachments(parsed, &mut attachments);

    let body = match decoded_text_body(parsed) {
        Ok(Some(body)) => body,
        Ok(None) => String::new(),
        Err(error) => {
            eprintln!("Warning: failed to decode message body: {error:#}");
            String::new()
        }
    };

    (body, attachments)
}

/// Return the first usable, decoded, non-attachment text body.
///
/// Plain text is preferred across the full MIME tree. When only HTML is
/// available it is converted to text. Decode and conversion failures are
/// returned to the caller so stateful draft orchestration can fail before
/// mutating a mailbox.
pub(crate) fn decoded_text_body(parsed: &mailparse::ParsedMail) -> Result<Option<String>> {
    let mut text_plain = None;
    let mut text_html = None;
    collect_text_parts(parsed, &mut text_plain, &mut text_html)?;

    if let Some(text) = text_plain {
        return Ok(Some(text));
    }

    match text_html {
        Some(html) => html2text::from_read(html.as_bytes(), 80)
            .context("Failed to convert HTML body to text")
            .map(Some),
        None => Ok(None),
    }
}

fn collect_text_parts(
    part: &mailparse::ParsedMail,
    text_plain: &mut Option<String>,
    text_html: &mut Option<String>,
) -> Result<()> {
    let mime = part.ctype.mimetype.to_lowercase();

    if is_attachment(part) {
        return Ok(());
    }

    if mime.starts_with("multipart/") {
        for sub in &part.subparts {
            collect_text_parts(sub, text_plain, text_html)?;
        }
    } else if mime == "text/plain" && text_plain.is_none() {
        *text_plain = Some(
            part.get_body()
                .context("Failed to decode text/plain body")?,
        );
    } else if mime == "text/html" && text_html.is_none() {
        *text_html = Some(part.get_body().context("Failed to decode text/html body")?);
    }

    Ok(())
}

fn collect_attachments(part: &mailparse::ParsedMail, attachments: &mut Vec<String>) {
    let disposition_raw = content_disposition(part);

    if disposition_raw.to_lowercase().starts_with("attachment") {
        if let Some(name) = part.ctype.params.get("name") {
            attachments.push(name.clone());
        } else {
            let filename = extract_disposition_filename(&disposition_raw);
            attachments.push(filename.unwrap_or_else(|| "unnamed".to_string()));
        }
        return;
    }

    for sub in &part.subparts {
        collect_attachments(sub, attachments);
    }
}

fn is_attachment(part: &mailparse::ParsedMail) -> bool {
    content_disposition(part)
        .to_ascii_lowercase()
        .starts_with("attachment")
}

fn content_disposition(part: &mailparse::ParsedMail) -> String {
    part.headers
        .iter()
        .find(|header| header.get_key().eq_ignore_ascii_case("Content-Disposition"))
        .map(|header| header.get_value())
        .unwrap_or_default()
}

fn extract_disposition_filename(disposition: &str) -> Option<String> {
    disposition.split(';').find_map(|param| {
        let param = param.trim();
        if param.to_lowercase().starts_with("filename=") {
            let value = param["filename=".len()..].trim();
            Some(value.trim_matches('"').to_string())
        } else {
            None
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_body_plain_text() {
        let raw = b"Content-Type: text/plain\r\n\r\nHello world";
        let parsed = mailparse::parse_mail(raw).unwrap();
        let (text, attachments) = extract_body(&parsed);
        assert_eq!(text, "Hello world");
        assert!(attachments.is_empty());
    }

    #[test]
    fn message_key_uses_account_fallback_folder() {
        let mut defaults = DefaultFolderMap::new();
        defaults.insert(Some("work".to_string()), "Sent".to_string());
        let msg = MessageRow {
            account: Some("work".to_string()),
            uid: 42,
            folder: None,
            from: String::new(),
            subject: String::new(),
            date: String::new(),
            timestamp: 0,
            size: 0,
        };

        assert_eq!(
            message_key(&msg, &defaults),
            (Some("work".to_string()), "Sent".to_string(), 42)
        );
    }

    #[test]
    fn message_key_prefers_explicit_folder() {
        let mut defaults = DefaultFolderMap::new();
        defaults.insert(Some("work".to_string()), "Sent".to_string());
        let msg = MessageRow {
            account: Some("work".to_string()),
            uid: 42,
            folder: Some("Archive".to_string()),
            from: String::new(),
            subject: String::new(),
            date: String::new(),
            timestamp: 0,
            size: 0,
        };

        assert_eq!(
            message_key(&msg, &defaults),
            (Some("work".to_string()), "Archive".to_string(), 42)
        );
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
    fn extract_body_no_text() {
        let raw = b"Content-Type: application/pdf\r\nContent-Disposition: attachment; filename=\"doc.pdf\"\r\n\r\nbinary";
        let parsed = mailparse::parse_mail(raw).unwrap();
        let (text, attachments) = extract_body(&parsed);
        assert!(text.is_empty());
        assert_eq!(attachments, vec!["doc.pdf"]);
    }

    #[test]
    fn extract_disposition_filename_quoted() {
        assert_eq!(
            extract_disposition_filename("attachment; filename=\"report.pdf\""),
            Some("report.pdf".to_string())
        );
    }

    #[test]
    fn extract_disposition_filename_unquoted() {
        assert_eq!(
            extract_disposition_filename("attachment; filename=report.pdf"),
            Some("report.pdf".to_string())
        );
    }

    #[test]
    fn extract_disposition_filename_missing() {
        assert_eq!(extract_disposition_filename("attachment"), None);
    }
}
