use anyhow::{Context, Result};

use crate::connection::ImapSession;
use crate::display::MessageRow;
use crate::search;

/// Display the full content of messages in the terminal.
pub fn read_messages(
    session: &mut ImapSession,
    messages: &[MessageRow],
    default_folder: &str,
) -> Result<()> {
    let mut by_folder: std::collections::HashMap<String, Vec<u32>> =
        std::collections::HashMap::new();
    for msg in messages {
        let folder = msg
            .folder
            .clone()
            .unwrap_or_else(|| default_folder.to_string());
        by_folder.entry(folder).or_default().push(msg.uid);
    }

    // Build a UID→folder map so we can print in the original (sorted) order
    let mut uid_bodies: std::collections::HashMap<(String, u32), Vec<u8>> =
        std::collections::HashMap::new();

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

    // Print in the original message order (newest first, as returned by search)
    let total = messages.len();
    for (i, msg) in messages.iter().enumerate() {
        let folder = msg
            .folder
            .clone()
            .unwrap_or_else(|| default_folder.to_string());
        let key = (folder, msg.uid);

        if let Some(raw) = uid_bodies.get(&key) {
            print_message(raw);
        } else {
            eprintln!("Warning: could not fetch body for UID {}", msg.uid);
        }

        if i + 1 < total {
            println!("\n{}\n", "─".repeat(60));
        }
    }

    Ok(())
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
    let mut text_plain = None;
    let mut text_html = None;
    let mut attachments = Vec::new();

    collect_parts(parsed, &mut text_plain, &mut text_html, &mut attachments);

    let body = if let Some(text) = text_plain {
        text
    } else if let Some(html) = text_html {
        match html2text::from_read(html.as_bytes(), 80) {
            Ok(converted) => converted,
            Err(e) => {
                eprintln!("Warning: failed to convert HTML to text: {e}");
                html
            }
        }
    } else {
        String::new()
    };

    (body, attachments)
}

fn collect_parts(
    part: &mailparse::ParsedMail,
    text_plain: &mut Option<String>,
    text_html: &mut Option<String>,
    attachments: &mut Vec<String>,
) {
    let mime = part.ctype.mimetype.to_lowercase();

    // Check if this part is an attachment
    let disposition_raw = part
        .headers
        .iter()
        .find(|h| h.get_key().eq_ignore_ascii_case("Content-Disposition"))
        .map(|h| h.get_value())
        .unwrap_or_default();

    if disposition_raw.to_lowercase().starts_with("attachment") {
        if let Some(name) = part.ctype.params.get("name") {
            attachments.push(name.clone());
        } else {
            let filename = extract_disposition_filename(&disposition_raw);
            attachments.push(filename.unwrap_or_else(|| "unnamed".to_string()));
        }
        return;
    }

    if mime.starts_with("multipart/") {
        for sub in &part.subparts {
            collect_parts(sub, text_plain, text_html, attachments);
        }
    } else if mime == "text/plain" && text_plain.is_none() {
        match part.get_body() {
            Ok(body) => *text_plain = Some(body),
            Err(e) => {
                eprintln!("Warning: failed to decode text/plain: {e}");
                if let Ok(raw) = part.get_body_raw() {
                    *text_plain = Some(String::from_utf8_lossy(&raw).into_owned());
                }
            }
        }
    } else if mime == "text/html" && text_html.is_none() {
        match part.get_body() {
            Ok(body) => *text_html = Some(body),
            Err(e) => {
                eprintln!("Warning: failed to decode text/html: {e}");
                if let Ok(raw) = part.get_body_raw() {
                    *text_html = Some(String::from_utf8_lossy(&raw).into_owned());
                }
            }
        }
    }
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
