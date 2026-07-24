use std::collections::HashSet;

use anyhow::{anyhow, bail, Context, Result};
use lettre::message::{header, Mailbox, Message, MessageBuilder, SinglePart};
use mailparse::{addrparse_header, MailAddr, MailHeader, MailHeaderMap, ParsedMail};

use crate::read::decoded_text_body;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BodyFormat {
    Plain,
    Html,
}

#[derive(Clone, Debug)]
pub struct NewDraftInput {
    pub sender: Mailbox,
    pub to: Vec<Mailbox>,
    pub cc: Vec<Mailbox>,
    pub bcc: Vec<Mailbox>,
    pub subject: String,
    pub body: String,
    pub format: BodyFormat,
}

#[derive(Clone, Debug)]
pub struct ReplyDraftInput<'a> {
    pub sender: Mailbox,
    pub source: &'a [u8],
    pub body: String,
    pub format: BodyFormat,
    pub quote_original: bool,
}

#[derive(Clone, Debug)]
pub struct ComposedDraft {
    pub bytes: Vec<u8>,
    pub message_id: String,
    pub to: Vec<Mailbox>,
    pub cc: Vec<Mailbox>,
    pub bcc: Vec<Mailbox>,
    pub subject: String,
}

#[derive(Debug)]
struct ReplyContext {
    to: Vec<Mailbox>,
    cc: Vec<Mailbox>,
    subject: String,
    in_reply_to: Option<String>,
    references: Option<String>,
    attribution: String,
    quoted_text: Option<String>,
}

struct MessageInput {
    sender: Mailbox,
    to: Vec<Mailbox>,
    cc: Vec<Mailbox>,
    bcc: Vec<Mailbox>,
    subject: String,
    body: String,
    format: BodyFormat,
    in_reply_to: Option<String>,
    references: Option<String>,
}

pub fn parse_mailbox(value: &str, field: &str) -> Result<Mailbox> {
    validate_header_text(value, field)?;
    let mailbox = value
        .parse::<Mailbox>()
        .with_context(|| format!("Invalid {field} mailbox"))?;
    validate_mailbox(&mailbox, field)?;
    Ok(mailbox)
}

pub fn compose_new_draft(input: NewDraftInput) -> Result<ComposedDraft> {
    validate_mailbox(&input.sender, "sender")?;
    validate_mailboxes(&input.to, "To")?;
    validate_mailboxes(&input.cc, "Cc")?;
    validate_mailboxes(&input.bcc, "Bcc")?;
    validate_header_text(&input.subject, "subject")?;

    if input.to.is_empty() {
        bail!("At least one To recipient is required");
    }

    build_message(MessageInput {
        sender: input.sender,
        to: input.to,
        cc: input.cc,
        bcc: input.bcc,
        subject: input.subject,
        body: input.body,
        format: input.format,
        in_reply_to: None,
        references: None,
    })
}

pub fn compose_reply_draft(input: ReplyDraftInput<'_>) -> Result<ComposedDraft> {
    validate_mailbox(&input.sender, "sender")?;
    let source = mailparse::parse_mail(input.source).context("Failed to parse reply source")?;
    let context = derive_reply_context(&source, &input.sender, input.quote_original)?;
    let body = compose_reply_body(
        &input.body,
        input.format,
        input
            .quote_original
            .then_some((context.attribution.as_str(), context.quoted_text.as_deref())),
    );

    build_message(MessageInput {
        sender: input.sender,
        to: context.to,
        cc: context.cc,
        bcc: Vec::new(),
        subject: context.subject,
        body,
        format: input.format,
        in_reply_to: context.in_reply_to,
        references: context.references,
    })
}

fn build_message(input: MessageInput) -> Result<ComposedDraft> {
    let MessageInput {
        sender,
        to,
        cc,
        bcc,
        subject,
        body,
        format,
        in_reply_to,
        references,
    } = input;
    let mut builder = Message::builder()
        .from(sender)
        .subject(subject.clone())
        .date_now()
        .message_id(None)
        .keep_bcc();

    for mailbox in &to {
        builder = builder.to(mailbox.clone());
    }
    for mailbox in &cc {
        builder = builder.cc(mailbox.clone());
    }
    for mailbox in &bcc {
        builder = builder.bcc(mailbox.clone());
    }
    builder = add_thread_headers(builder, in_reply_to, references);

    let part = match format {
        BodyFormat::Plain => SinglePart::plain(body),
        BodyFormat::Html => SinglePart::html(body),
    };
    let message = builder
        .singlepart(part)
        .context("Failed to build draft message")?;
    let message_id = message
        .headers()
        .get::<header::MessageId>()
        .ok_or_else(|| anyhow!("Draft message is missing its generated Message-ID"))?
        .as_ref()
        .to_string();

    Ok(ComposedDraft {
        bytes: message.formatted(),
        message_id,
        to,
        cc,
        bcc,
        subject,
    })
}

fn add_thread_headers(
    mut builder: MessageBuilder,
    in_reply_to: Option<String>,
    references: Option<String>,
) -> MessageBuilder {
    if let Some(in_reply_to) = in_reply_to {
        builder = builder.in_reply_to(in_reply_to);
    }
    if let Some(references) = references {
        builder = builder.references(references);
    }
    builder
}

fn derive_reply_context(
    source: &ParsedMail<'_>,
    sender: &Mailbox,
    quote_original: bool,
) -> Result<ReplyContext> {
    let reply_to_headers = source.headers.get_all_headers("Reply-To");
    let primary = if reply_to_headers.is_empty() {
        parse_required_mailboxes(&source.headers.get_all_headers("From"), "From")?
    } else {
        parse_required_mailboxes(&reply_to_headers, "Reply-To")?
    };

    let source_to = parse_optional_mailboxes(&source.headers.get_all_headers("To"), "To")?;
    let source_cc = parse_optional_mailboxes(&source.headers.get_all_headers("Cc"), "Cc")?;
    let sender_key = address_key(sender);

    let mut to = Vec::new();
    let mut seen_to = HashSet::new();
    for mailbox in primary.iter().chain(source_to.iter()) {
        let key = address_key(mailbox);
        if key != sender_key && seen_to.insert(key) {
            to.push(mailbox.clone());
        }
    }

    let mut cc = Vec::new();
    let mut seen_cc = HashSet::new();
    for mailbox in source_cc {
        let key = address_key(&mailbox);
        if key != sender_key && !seen_to.contains(&key) && seen_cc.insert(key) {
            cc.push(mailbox);
        }
    }

    if to.is_empty() && cc.is_empty() {
        bail!("Reply has no recipient after excluding the sender");
    }

    let source_subject = first_validated_value(source, "Subject")?.unwrap_or_default();
    let subject = normalize_reply_subject(&source_subject);
    let parent_id = parse_single_message_id(source, "Message-ID")?;
    let source_references = parse_message_id_list(source, "References")?;
    let source_in_reply_to = parse_message_id_list(source, "In-Reply-To")?;
    let in_reply_to = parent_id.as_ref().map(|id| format!("<{id}>"));

    let mut references = source_references.or(source_in_reply_to).unwrap_or_default();
    if let Some(parent_id) = &parent_id {
        if !references.iter().any(|id| id == parent_id) {
            references.push(parent_id.clone());
        }
    }
    let references = (!references.is_empty()).then(|| format_message_ids(&references));

    let attribution_sender = primary
        .first()
        .expect("required origin parsing returned a nonempty list");
    let date = first_validated_value(source, "Date")?;
    let attribution = match date {
        Some(date) if !date.trim().is_empty() => {
            format!("On {}, {} wrote:", date.trim(), attribution_sender)
        }
        _ => format!("{attribution_sender} wrote:"),
    };
    validate_header_text(&attribution, "reply attribution")?;

    let quoted_text = if quote_original {
        decoded_text_body(source).context("Failed to prepare quoted source body")?
    } else {
        None
    };

    Ok(ReplyContext {
        to,
        cc,
        subject,
        in_reply_to,
        references,
        attribution,
        quoted_text,
    })
}

fn parse_required_mailboxes(headers: &[&MailHeader<'_>], field: &str) -> Result<Vec<Mailbox>> {
    if headers.is_empty() {
        bail!("Reply source is missing {field}");
    }
    let mailboxes = parse_mailbox_headers(headers, field)?;
    if mailboxes.is_empty() {
        bail!("Reply source {field} has no mailbox");
    }
    Ok(mailboxes)
}

fn parse_optional_mailboxes(headers: &[&MailHeader<'_>], field: &str) -> Result<Vec<Mailbox>> {
    parse_mailbox_headers(headers, field)
}

fn parse_mailbox_headers(headers: &[&MailHeader<'_>], field: &str) -> Result<Vec<Mailbox>> {
    let mut mailboxes = Vec::new();
    for header in headers {
        let decoded = header.get_value();
        validate_header_text(&decoded, &format!("source {field}"))?;
        let addresses = addrparse_header(header)
            .with_context(|| format!("Invalid source {field} address header"))?;
        for address in addresses.iter() {
            match address {
                MailAddr::Single(single) => {
                    mailboxes.push(single_to_mailbox(single, field)?);
                }
                MailAddr::Group(group) => {
                    validate_header_text(&group.group_name, &format!("source {field} group"))?;
                    for single in &group.addrs {
                        mailboxes.push(single_to_mailbox(single, field)?);
                    }
                }
            }
        }
    }
    Ok(mailboxes)
}

fn single_to_mailbox(single: &mailparse::SingleInfo, field: &str) -> Result<Mailbox> {
    if let Some(name) = &single.display_name {
        validate_header_text(name, &format!("source {field} display name"))?;
    }
    validate_header_text(&single.addr, &format!("source {field} address"))?;
    let address = single
        .addr
        .parse()
        .with_context(|| format!("Invalid source {field} mailbox"))?;
    let mailbox = Mailbox::new(single.display_name.clone(), address);
    validate_mailbox(&mailbox, &format!("source {field}"))?;
    Ok(mailbox)
}

fn first_validated_value(source: &ParsedMail<'_>, field: &str) -> Result<Option<String>> {
    match source.headers.get_first_value(field) {
        Some(value) => {
            validate_header_text(&value, &format!("source {field}"))?;
            Ok(Some(value))
        }
        None => Ok(None),
    }
}

fn parse_single_message_id(source: &ParsedMail<'_>, field: &str) -> Result<Option<String>> {
    Ok(parse_message_id_list(source, field)?.and_then(|ids| {
        if ids.len() == 1 {
            ids.into_iter().next()
        } else {
            None
        }
    }))
}

fn parse_message_id_list(source: &ParsedMail<'_>, field: &str) -> Result<Option<Vec<String>>> {
    let values = source.headers.get_all_values(field);
    if values.is_empty() {
        return Ok(None);
    }

    let mut parsed = Vec::new();
    for value in values {
        validate_header_text(&value, &format!("source {field}"))?;
        let ids = match mailparse::msgidparse(&value) {
            Ok(ids) => ids,
            Err(_) => return Ok(None),
        };
        if ids.is_empty() || ids.iter().any(|id| !is_valid_message_id(id)) {
            return Ok(None);
        }
        parsed.extend(ids.iter().cloned());
    }
    Ok(Some(parsed))
}

fn is_valid_message_id(id: &str) -> bool {
    if !id.is_ascii() || id.bytes().any(|byte| byte.is_ascii_control()) {
        return false;
    }
    let mut parts = id.split('@');
    let Some(left) = parts.next() else {
        return false;
    };
    let Some(right) = parts.next() else {
        return false;
    };
    if parts.next().is_some() || left.is_empty() || right.is_empty() {
        return false;
    }

    is_dot_atom(left)
        && (is_dot_atom(right)
            || (right.starts_with('[')
                && right.ends_with(']')
                && right.len() > 2
                && right[1..right.len() - 1]
                    .bytes()
                    .all(|byte| byte.is_ascii_graphic() && !matches!(byte, b'[' | b']' | b'\\'))))
}

fn is_dot_atom(value: &str) -> bool {
    !value.starts_with('.')
        && !value.ends_with('.')
        && !value.contains("..")
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(
                    byte,
                    b'!' | b'#'
                        | b'$'
                        | b'%'
                        | b'&'
                        | b'\''
                        | b'*'
                        | b'+'
                        | b'-'
                        | b'.'
                        | b'/'
                        | b'='
                        | b'?'
                        | b'^'
                        | b'_'
                        | b'`'
                        | b'{'
                        | b'|'
                        | b'}'
                        | b'~'
                )
        })
}

fn format_message_ids(ids: &[String]) -> String {
    ids.iter()
        .map(|id| format!("<{id}>"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn normalize_reply_subject(subject: &str) -> String {
    let mut remainder = subject.trim();
    loop {
        let Some(prefix) = remainder.get(..3) else {
            break;
        };
        if prefix.eq_ignore_ascii_case("re:") {
            remainder = remainder[3..].trim_start();
        } else {
            break;
        }
    }

    if remainder.is_empty() {
        "Re:".to_string()
    } else {
        format!("Re: {remainder}")
    }
}

fn compose_reply_body(
    body: &str,
    format: BodyFormat,
    quote: Option<(&str, Option<&str>)>,
) -> String {
    let Some((attribution, quoted_text)) = quote else {
        return body.to_string();
    };

    let quote = match format {
        BodyFormat::Plain => {
            let quoted = quoted_text
                .filter(|text| !text.is_empty())
                .map(quote_plain_text);
            match quoted {
                Some(quoted) => format!("{attribution}\n{quoted}"),
                None => attribution.to_string(),
            }
        }
        BodyFormat::Html => {
            let attribution = escape_html(attribution);
            let quoted = quoted_text
                .filter(|text| !text.is_empty())
                .map(escape_html_with_breaks)
                .unwrap_or_default();
            if quoted.is_empty() {
                format!(r#"<blockquote type="cite">{attribution}</blockquote>"#)
            } else {
                format!(
                    r#"<blockquote type="cite"><div>{attribution}</div><div>{quoted}</div></blockquote>"#
                )
            }
        }
    };

    if body.is_empty() {
        quote
    } else {
        match format {
            BodyFormat::Plain => format!("{body}\n\n{quote}"),
            BodyFormat::Html => format!("{body}<br><br>{quote}"),
        }
    }
}

fn quote_plain_text(text: &str) -> String {
    text.replace("\r\n", "\n")
        .replace('\r', "\n")
        .split('\n')
        .map(|line| {
            if line.is_empty() {
                ">".to_string()
            } else {
                format!("> {line}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn escape_html_with_breaks(text: &str) -> String {
    escape_html(&text.replace("\r\n", "\n").replace('\r', "\n")).replace('\n', "<br>\n")
}

fn escape_html(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            _ => escaped.push(character),
        }
    }
    escaped
}

fn validate_mailboxes(mailboxes: &[Mailbox], field: &str) -> Result<()> {
    for mailbox in mailboxes {
        validate_mailbox(mailbox, field)?;
    }
    Ok(())
}

fn address_key(mailbox: &Mailbox) -> String {
    mailbox.email.to_string().to_ascii_lowercase()
}

fn validate_mailbox(mailbox: &Mailbox, field: &str) -> Result<()> {
    if let Some(name) = &mailbox.name {
        validate_header_text(name, &format!("{field} display name"))?;
    }
    validate_header_text(mailbox.email.as_ref(), &format!("{field} address"))
}

fn validate_header_text(value: &str, field: &str) -> Result<()> {
    if value
        .chars()
        .any(|character| character == '\0' || character.is_control())
    {
        bail!("{field} contains a disallowed header control character");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mailbox(value: &str) -> Mailbox {
        parse_mailbox(value, "test").unwrap()
    }

    fn new_input() -> NewDraftInput {
        NewDraftInput {
            sender: mailbox("Alice <alice@example.com>"),
            to: vec![mailbox("Bob <bob@example.com>")],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: "Hello".to_string(),
            body: "Body".to_string(),
            format: BodyFormat::Plain,
        }
    }

    fn compose_reply(
        source: &[u8],
        format: BodyFormat,
        quote_original: bool,
    ) -> Result<ComposedDraft> {
        compose_reply_draft(ReplyDraftInput {
            sender: mailbox("Me <me@example.com>"),
            source,
            body: match format {
                BodyFormat::Plain => "New reply".to_string(),
                BodyFormat::Html => "<p>New reply</p>".to_string(),
            },
            format,
            quote_original,
        })
    }

    fn header_value(parsed: &ParsedMail<'_>, name: &str) -> Option<String> {
        parsed.headers.get_first_value(name)
    }

    #[test]
    fn new_draft_keeps_bcc_and_generates_identity_headers() {
        let composed = compose_new_draft(NewDraftInput {
            sender: parse_mailbox("Alice <alice@example.com>", "sender").unwrap(),
            to: vec![
                parse_mailbox("Bób <bob@example.com>", "To").unwrap(),
                parse_mailbox("Carol <carol@example.com>", "To").unwrap(),
            ],
            cc: vec![parse_mailbox("Dora <dora@example.com>", "Cc").unwrap()],
            bcc: vec![parse_mailbox("Secret <secret@example.com>", "Bcc").unwrap()],
            subject: "Héllo 世界".to_string(),
            body: "Plain body".to_string(),
            format: BodyFormat::Plain,
        })
        .unwrap();

        let parsed = mailparse::parse_mail(&composed.bytes).unwrap();
        assert!(parsed
            .headers
            .iter()
            .any(|header| header.get_key().eq_ignore_ascii_case("Bcc")));
        assert!(parsed
            .headers
            .iter()
            .any(|header| header.get_key().eq_ignore_ascii_case("Date")));
        assert_eq!(
            parsed
                .headers
                .iter()
                .find(|header| header.get_key().eq_ignore_ascii_case("Message-ID"))
                .unwrap()
                .get_value(),
            composed.message_id
        );
        assert_eq!(parsed.ctype.mimetype, "text/plain");
        assert_eq!(
            header_value(&parsed, "Subject").as_deref(),
            Some("Héllo 世界")
        );
        assert_eq!(composed.to.len(), 2);
        assert_eq!(composed.cc.len(), 1);
        assert_eq!(composed.bcc.len(), 1);
    }

    #[test]
    fn new_draft_supports_empty_subject_and_body() {
        let mut input = new_input();
        input.subject = String::new();
        input.body = String::new();

        let composed = compose_new_draft(input).unwrap();
        let parsed = mailparse::parse_mail(&composed.bytes).unwrap();

        assert_eq!(header_value(&parsed, "Subject"), Some(String::new()));
        assert!(parsed.get_body().unwrap().trim().is_empty());
        assert_eq!(parsed.ctype.mimetype, "text/plain");
        assert_eq!(parsed.ctype.params.get("charset").unwrap(), "utf-8");
    }

    #[test]
    fn new_draft_html_is_a_utf8_single_part() {
        let mut input = new_input();
        input.format = BodyFormat::Html;
        input.body = "<p>Héllo</p>".to_string();

        let composed = compose_new_draft(input).unwrap();
        let parsed = mailparse::parse_mail(&composed.bytes).unwrap();

        assert_eq!(parsed.ctype.mimetype, "text/html");
        assert_eq!(parsed.ctype.params.get("charset").unwrap(), "utf-8");
        assert_eq!(parsed.get_body().unwrap().trim_end(), "<p>Héllo</p>");
        assert!(header_value(&parsed, "Content-Transfer-Encoding").is_some());
    }

    #[test]
    fn new_draft_rejects_missing_to_and_caller_header_injection() {
        let mut missing_to = new_input();
        missing_to.to.clear();
        assert!(compose_new_draft(missing_to).is_err());

        let mut subject_injection = new_input();
        subject_injection.subject = "Hello\r\nBcc: attacker@example.com".to_string();
        assert!(compose_new_draft(subject_injection).is_err());

        let injected_sender = Mailbox::new(
            Some("Alice\nBcc: attacker@example.com".to_string()),
            "alice@example.com".parse().unwrap(),
        );
        let mut sender_injection = new_input();
        sender_injection.sender = injected_sender;
        assert!(compose_new_draft(sender_injection).is_err());

        assert!(parse_mailbox("Bob\r\nBcc: attacker@example.com", "To").is_err());
        assert!(parse_mailbox("not-an-address", "To").is_err());
    }

    #[test]
    fn reply_derives_reply_all_and_safe_quote() {
        let source = b"From: Sender <sender@example.com>\r\n\
Reply-To: Reply <reply@example.com>\r\n\
To: Me <me@example.com>, Other <other@example.com>\r\n\
Cc: Other Again <OTHER@example.com>, Team <team@example.com>\r\n\
Date: Thu, 24 Jul 2026 12:00:00 +0200\r\n\
Subject: RE: Re: Topic\r\n\
Message-ID: <parent@example.com>\r\n\
References: <root@example.com>\r\n\
Content-Type: text/plain; charset=utf-8\r\n\r\n\
<script>alert(1)</script>";

        let composed = compose_reply_draft(ReplyDraftInput {
            sender: parse_mailbox("Me <me@example.com>", "sender").unwrap(),
            source,
            body: "<p>New reply</p>".to_string(),
            format: BodyFormat::Html,
            quote_original: true,
        })
        .unwrap();

        assert_eq!(composed.subject, "Re: Topic");
        assert_eq!(
            composed
                .to
                .iter()
                .map(|mailbox| mailbox.email.to_string())
                .collect::<Vec<_>>(),
            ["reply@example.com", "other@example.com"]
        );
        assert_eq!(
            composed
                .cc
                .iter()
                .map(|mailbox| mailbox.email.to_string())
                .collect::<Vec<_>>(),
            ["team@example.com"]
        );
        assert!(composed.bcc.is_empty());

        let parsed = mailparse::parse_mail(&composed.bytes).unwrap();
        let body = parsed.get_body().unwrap();
        assert!(body.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
        assert!(!body.contains("<script>alert(1)</script>"));
    }

    #[test]
    fn reply_flattens_groups_and_preserves_recipient_classes() {
        let source = b"From: Sender <sender@example.com>\r\n\
To: Friends: Me <me@example.com>, Other <other@example.com>;\r\n\
Cc: Team: Cc One <cc@example.com>;\r\n\
Subject: Topic\r\n\r\nBody";

        let composed = compose_reply(source, BodyFormat::Plain, false).unwrap();

        assert_eq!(
            composed
                .to
                .iter()
                .map(|mailbox| mailbox.email.to_string())
                .collect::<Vec<_>>(),
            ["sender@example.com", "other@example.com"]
        );
        assert_eq!(
            composed
                .cc
                .iter()
                .map(|mailbox| mailbox.email.to_string())
                .collect::<Vec<_>>(),
            ["cc@example.com"]
        );
    }

    #[test]
    fn reply_rejects_malformed_origin_visible_recipient_and_empty_result() {
        let missing_origin = b"To: other@example.com\r\nSubject: Topic\r\n\r\nBody";
        assert!(compose_reply(missing_origin, BodyFormat::Plain, false).is_err());

        let malformed_reply_to = b"From: sender@example.com\r\nReply-To: invalid\r\n\r\nBody";
        assert!(compose_reply(malformed_reply_to, BodyFormat::Plain, false).is_err());

        let malformed_visible = b"From: sender@example.com\r\nTo: invalid\r\n\r\nBody";
        assert!(compose_reply(malformed_visible, BodyFormat::Plain, false).is_err());

        let self_only = b"From: me@example.com\r\nTo: ME@example.com\r\n\r\nBody";
        assert!(compose_reply(self_only, BodyFormat::Plain, false).is_err());
    }

    #[test]
    fn reply_normalizes_subject_and_builds_thread_headers() {
        let source = b"From: sender@example.com\r\n\
Subject:  rE: RE:   Topic\r\n\
Message-ID: <parent@example.com>\r\n\
References: <root@example.com> <middle@example.com>\r\n\r\nBody";

        let composed = compose_reply(source, BodyFormat::Plain, false).unwrap();
        let parsed = mailparse::parse_mail(&composed.bytes).unwrap();

        assert_eq!(composed.subject, "Re: Topic");
        assert_eq!(
            header_value(&parsed, "In-Reply-To").as_deref(),
            Some("<parent@example.com>")
        );
        assert_eq!(
            header_value(&parsed, "References").as_deref(),
            Some("<root@example.com> <middle@example.com> <parent@example.com>")
        );
    }

    #[test]
    fn reply_uses_in_reply_to_as_references_fallback() {
        let source = b"From: sender@example.com\r\n\
Message-ID: <parent@example.com>\r\n\
In-Reply-To: <grandparent@example.com>\r\n\r\nBody";

        let composed = compose_reply(source, BodyFormat::Plain, false).unwrap();
        let parsed = mailparse::parse_mail(&composed.bytes).unwrap();

        assert_eq!(
            header_value(&parsed, "References").as_deref(),
            Some("<grandparent@example.com> <parent@example.com>")
        );
    }

    #[test]
    fn reply_uses_valid_folded_ids_and_omits_malformed_ids() {
        let folded = b"From: sender@example.com\r\n\
Subject: Topic\r\n\
\tcontinued\r\n\
Message-ID:\r\n\
\t<parent@example.com>\r\n\
References: <root@example.com>\r\n\
\t<middle@example.com>\r\n\r\nBody";
        let composed = compose_reply(folded, BodyFormat::Plain, false).unwrap();
        let parsed = mailparse::parse_mail(&composed.bytes).unwrap();
        assert_eq!(composed.subject, "Re: Topic continued");
        assert_eq!(
            header_value(&parsed, "References").as_deref(),
            Some("<root@example.com> <middle@example.com> <parent@example.com>")
        );

        let malformed = b"From: sender@example.com\r\n\
Message-ID: not-a-message-id\r\n\
References: also-bad\r\n\r\nBody";
        let composed = compose_reply(malformed, BodyFormat::Plain, false).unwrap();
        let parsed = mailparse::parse_mail(&composed.bytes).unwrap();
        assert_eq!(header_value(&parsed, "In-Reply-To"), None);
        assert_eq!(header_value(&parsed, "References"), None);

        let invalid_syntax = b"From: sender@example.com\r\n\
Message-ID: <parent:injected@example.com>\r\n\
References: <root;bad@example.com>\r\n\r\nBody";
        let composed = compose_reply(invalid_syntax, BodyFormat::Plain, false).unwrap();
        let parsed = mailparse::parse_mail(&composed.bytes).unwrap();
        assert_eq!(header_value(&parsed, "In-Reply-To"), None);
        assert_eq!(header_value(&parsed, "References"), None);
    }

    #[test]
    fn reply_preserves_available_references_without_parent_id() {
        let source = b"From: sender@example.com\r\n\
References: <root@example.com>\r\n\r\nBody";
        let composed = compose_reply(source, BodyFormat::Plain, false).unwrap();
        let parsed = mailparse::parse_mail(&composed.bytes).unwrap();

        assert_eq!(header_value(&parsed, "In-Reply-To"), None);
        assert_eq!(
            header_value(&parsed, "References").as_deref(),
            Some("<root@example.com>")
        );
    }

    #[test]
    fn source_derived_header_controls_are_rejected() {
        let sources: Vec<Vec<u8>> = vec![
            b"From: sender@example.com\r\nSubject: bad\0subject\r\n\r\nBody".to_vec(),
            b"From: \"bad\0name\" <sender@example.com>\r\n\r\nBody".to_vec(),
            b"From: sender@example.com\r\nMessage-ID: <bad\0id@example.com>\r\n\r\nBody".to_vec(),
            b"From: sender@example.com\r\nReferences: <bad\0id@example.com>\r\n\r\nBody".to_vec(),
            b"From: sender@example.com\r\nDate: bad\0date\r\n\r\nBody".to_vec(),
        ];

        for source in sources {
            assert!(compose_reply(&source, BodyFormat::Plain, false).is_err());
        }
    }

    #[test]
    fn html_reply_escapes_all_source_markup_but_keeps_caller_html() {
        let source = b"From: \"Eve </blockquote>\" <eve@example.com>\r\n\
Date: Now <img src=x onerror=alert(1)>\r\n\
Content-Type: text/plain; charset=utf-8\r\n\r\n\
</blockquote><script>alert(1)</script><img src=x onerror=alert(2)>&lt;script&gt;";

        let composed = compose_reply(source, BodyFormat::Html, true).unwrap();
        let parsed = mailparse::parse_mail(&composed.bytes).unwrap();
        let body = parsed.get_body().unwrap();

        assert!(body.starts_with("<p>New reply</p>"));
        assert!(body.contains("&lt;/blockquote&gt;&lt;script&gt;"));
        assert!(body.contains("&lt;img src=x onerror=alert(2)&gt;"));
        assert!(body.contains("&amp;lt;script&amp;gt;"));
        assert!(!body.contains("<script>"));
        assert!(!body.contains("<img src=x"));
    }

    #[test]
    fn html_only_source_markup_never_enters_the_reply_as_markup() {
        let source = b"From: eve@example.com\r\n\
Content-Type: text/html; charset=utf-8\r\n\r\n\
<p>Hello</p><script>alert(1)</script><img src=x onerror=alert(2)></blockquote>";

        let composed = compose_reply(source, BodyFormat::Html, true).unwrap();
        let parsed = mailparse::parse_mail(&composed.bytes).unwrap();
        let body = parsed.get_body().unwrap();

        assert!(body.contains("Hello"));
        assert!(!body.contains("<script"));
        assert!(!body.contains("<img src=x"));
        assert!(!body.contains("onerror="));
    }

    #[test]
    fn plain_reply_prefixes_each_quoted_line() {
        let source = b"From: sender@example.com\r\n\
Content-Type: text/plain; charset=utf-8\r\n\r\n\
First line\r\nSecond line";

        let composed = compose_reply(source, BodyFormat::Plain, true).unwrap();
        let parsed = mailparse::parse_mail(&composed.bytes).unwrap();
        let body = parsed.get_body().unwrap();

        assert!(body.contains("\r\n> First line\r\n> Second line"));
    }

    #[test]
    fn no_quote_omits_attribution_and_does_not_decode_source_body() {
        let source = b"From: sender@example.com\r\n\
Content-Type: text/plain\r\n\
Content-Transfer-Encoding: base64\r\n\r\n\
%%%invalid%%%";

        let composed = compose_reply(source, BodyFormat::Plain, false).unwrap();
        let parsed = mailparse::parse_mail(&composed.bytes).unwrap();

        assert_eq!(parsed.get_body().unwrap().trim_end(), "New reply");
        assert!(!parsed.get_body().unwrap().contains("wrote:"));
    }

    #[test]
    fn quote_decode_failure_is_a_pre_composition_error() {
        let source = b"From: sender@example.com\r\n\
Content-Type: text/plain\r\n\
Content-Transfer-Encoding: base64\r\n\r\n\
%%%invalid%%%";

        assert!(compose_reply(source, BodyFormat::Plain, true).is_err());
    }

    #[test]
    fn empty_source_body_keeps_attribution_without_payload() {
        let source = b"From: sender@example.com\r\nSubject: Topic\r\n\r\n";
        let composed = compose_reply(source, BodyFormat::Plain, true).unwrap();
        let parsed = mailparse::parse_mail(&composed.bytes).unwrap();
        let body = parsed.get_body().unwrap();

        assert!(body.contains("sender@example.com wrote:"));
        assert!(!body.contains("\n>"));
    }
}
