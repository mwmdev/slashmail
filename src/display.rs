use comfy_table::{presets::UTF8_FULL_CONDENSED, Cell, ContentArrangement, Table};

#[derive(Clone, Debug, serde::Serialize)]
pub struct MessageRow {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
    pub uid: u32,
    pub folder: Option<String>,
    pub from: String,
    pub subject: String,
    pub date: String,
    pub timestamp: i64,
    pub size: u32,
    pub message_id: Option<String>,
    pub in_reply_to: Vec<String>,
    pub references: Vec<String>,
    pub seen: bool,
    pub answered: bool,
    pub flagged: bool,
    /// UIDVALIDITY of the mailbox the UID was searched in. Internal only:
    /// UID actions refuse to run if the mailbox identity has changed.
    #[serde(skip)]
    pub uid_validity: Option<u32>,
}

pub fn format_size(bytes: u64) -> String {
    if bytes >= 1_048_576 {
        format!("{:.1}M", bytes as f64 / 1_048_576.0)
    } else if bytes >= 1024 {
        format!("{:.0}K", bytes as f64 / 1024.0)
    } else {
        format!("{bytes}B")
    }
}

/// Render untrusted text as one inert terminal line: escape sequences are
/// removed with their payloads, every other control, C1, or bidi formatting
/// character becomes a space, and so does any invisible formatting character
/// that could make two different names look identical.
pub fn sanitize_terminal_field(value: &str) -> String {
    sanitize_terminal(value, false)
}

/// Like [`sanitize_terminal_field`], but keeps multi-line structure: LF and
/// HTAB are preserved, CRLF or lone CR become LF, and invisible formatting
/// characters (soft hyphens, zero-width spaces) are dropped rather than
/// shown as spaces.
pub fn sanitize_terminal_body(value: &str) -> String {
    sanitize_terminal(value, true)
}

fn is_unsafe_format_character(character: char) -> bool {
    matches!(
        character,
        '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{2028}'..='\u{202e}' | '\u{2066}'..='\u{2069}'
    )
}

/// Characters that render as nothing. Joiners (U+200C/U+200D) and variation
/// selectors are kept because scripts and emoji need them; tag characters are
/// handled separately because emoji flag sequences use them.
fn is_invisible_character(character: char) -> bool {
    matches!(
        character,
        '\u{00ad}'
            | '\u{180e}'
            | '\u{200b}'
            | '\u{2060}'..='\u{2064}'
            | '\u{206a}'..='\u{206f}'
            | '\u{feff}'
            | '\u{fff9}'..='\u{fffb}'
    )
}

const BLACK_FLAG: char = '\u{1f3f4}';
const CANCEL_TAG: char = '\u{e007f}';

fn is_tag_character(character: char) -> bool {
    matches!(character, '\u{e0000}'..='\u{e007f}')
}

fn sanitize_terminal(value: &str, multiline: bool) -> String {
    #[derive(Clone, Copy)]
    enum State {
        Text,
        /// After ESC, possibly followed by intermediate bytes.
        Escape,
        /// Control sequence; discarded through its final byte.
        Csi,
        /// OSC/DCS/SOS/PM/APC payload; discarded through ST (or BEL for OSC).
        ControlString {
            bell_terminates: bool,
        },
        /// ESC seen inside a control string; `\` completes ST.
        ControlStringEscape {
            bell_terminates: bool,
        },
    }

    let mut output = String::with_capacity(value.len());
    let mut state = State::Text;
    let mut after_cr = false;
    // Set after a black flag or a tag inside an emoji flag sequence.
    let mut in_flag_tags = false;
    for character in value.chars() {
        let was_after_cr = std::mem::take(&mut after_cr);
        let was_in_flag_tags = std::mem::take(&mut in_flag_tags);
        state = match state {
            State::Text if is_tag_character(character) && was_in_flag_tags => {
                output.push(character);
                in_flag_tags = character != CANCEL_TAG;
                State::Text
            }
            State::Text if is_tag_character(character) || is_invisible_character(character) => {
                if !multiline {
                    output.push(' ');
                }
                State::Text
            }
            State::Text => match character {
                '\u{1b}' => State::Escape,
                '\u{009b}' => State::Csi,
                '\u{009d}' => State::ControlString {
                    bell_terminates: true,
                },
                '\u{0090}' | '\u{0098}' | '\u{009e}' | '\u{009f}' => State::ControlString {
                    bell_terminates: false,
                },
                '\n' if multiline => {
                    if !was_after_cr {
                        output.push('\n');
                    }
                    State::Text
                }
                '\r' if multiline => {
                    output.push('\n');
                    after_cr = true;
                    State::Text
                }
                '\t' if multiline => {
                    output.push('\t');
                    State::Text
                }
                character
                    if character.is_control()
                        || matches!(character as u32, 0x80..=0x9f)
                        || is_unsafe_format_character(character) =>
                {
                    output.push(' ');
                    State::Text
                }
                character => {
                    output.push(character);
                    in_flag_tags = character == BLACK_FLAG;
                    State::Text
                }
            },
            State::Escape => match character {
                '[' => State::Csi,
                ']' => State::ControlString {
                    bell_terminates: true,
                },
                'P' | 'X' | '^' | '_' => State::ControlString {
                    bell_terminates: false,
                },
                '\u{20}'..='\u{2f}' => State::Escape,
                _ => State::Text,
            },
            State::Csi => {
                if matches!(character as u32, 0x40..=0x7e) {
                    State::Text
                } else {
                    State::Csi
                }
            }
            State::ControlString { bell_terminates } => match character {
                '\u{7}' if bell_terminates => State::Text,
                '\u{9c}' => State::Text,
                '\u{1b}' => State::ControlStringEscape { bell_terminates },
                _ => State::ControlString { bell_terminates },
            },
            State::ControlStringEscape { bell_terminates } => match character {
                '\\' => State::Text,
                '\u{1b}' => State::ControlStringEscape { bell_terminates },
                _ => State::ControlString { bell_terminates },
            },
        };
    }
    output
}

/// Truncate a string to at most `max` characters, appending "..." if truncated.
/// Safe for multi-byte UTF-8.
fn truncate_str(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max.saturating_sub(3)).collect();
        format!("{truncated}...")
    }
}

/// Table dates omit the numeric timezone offset; JSON keeps the full value.
fn table_date(date: &str) -> &str {
    match date.find(" +").or_else(|| date.find(" -")) {
        Some(position) => &date[..position],
        None => date,
    }
}

pub fn display_messages_json(messages: &[MessageRow]) {
    println!("{}", serde_json::to_string(messages).unwrap());
}

pub fn display_messages(messages: &[MessageRow]) {
    if messages.is_empty() {
        println!("No messages found.");
        return;
    }

    let has_account = messages.iter().any(|m| m.account.is_some());
    let has_folder = messages.iter().any(|m| m.folder.is_some());
    let mut table = Table::new();
    table.load_preset(UTF8_FULL_CONDENSED);
    table.set_content_arrangement(ContentArrangement::Dynamic);

    let mut header = vec!["UID", "From", "Subject", "Date", "Size"];
    if has_account {
        header.insert(1, "Account");
    }
    if has_folder {
        header.insert(if has_account { 2 } else { 1 }, "Folder");
    }
    table.set_header(header);

    for msg in messages {
        let mut row: Vec<Cell> = vec![Cell::new(msg.uid)];
        if has_account {
            row.push(Cell::new(sanitize_terminal_field(
                msg.account.as_deref().unwrap_or(""),
            )));
        }
        if has_folder {
            row.push(Cell::new(sanitize_terminal_field(
                msg.folder.as_deref().unwrap_or(""),
            )));
        }
        row.push(Cell::new(truncate_str(
            &sanitize_terminal_field(&msg.from),
            40,
        )));
        row.push(Cell::new(truncate_str(
            &sanitize_terminal_field(&msg.subject),
            60,
        )));
        row.push(Cell::new(sanitize_terminal_field(table_date(&msg.date))));
        row.push(Cell::new(format_size(msg.size as u64)));
        table.add_row(row);
    }

    println!("{table}");
    println!("{} message(s)", messages.len());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_str_short_unchanged() {
        assert_eq!(truncate_str("hello", 10), "hello");
    }

    #[test]
    fn truncate_str_exact_length_unchanged() {
        assert_eq!(truncate_str("hello", 5), "hello");
    }

    #[test]
    fn truncate_str_long_adds_ellipsis() {
        assert_eq!(truncate_str("hello world", 8), "hello...");
    }

    #[test]
    fn truncate_str_empty() {
        assert_eq!(truncate_str("", 10), "");
    }

    #[test]
    fn format_size_zero() {
        assert_eq!(format_size(0), "0B");
    }

    #[test]
    fn format_size_bytes() {
        assert_eq!(format_size(999), "999B");
    }

    #[test]
    fn format_size_kilobytes() {
        assert_eq!(format_size(1024), "1K");
    }

    #[test]
    fn format_size_kilobytes_rounds() {
        assert_eq!(format_size(1536), "2K");
    }

    #[test]
    fn format_size_megabytes() {
        assert_eq!(format_size(1_048_576), "1.0M");
    }

    #[test]
    fn format_size_megabytes_large() {
        assert_eq!(format_size(5_242_880), "5.0M");
    }

    #[test]
    fn terminal_fields_remove_escape_controls_and_unsafe_unicode() {
        let rendered = sanitize_terminal_field("safe\u{1b}]52;c;secret\u{7}\n\u{202e}\u{2066}tail");

        assert_eq!(rendered, "safe   tail");
        assert!(!rendered.chars().any(char::is_control));
    }

    #[test]
    fn invisible_characters_cannot_disguise_names() {
        let trash_lookalike = "Tra\u{200b}sh";
        assert_ne!(sanitize_terminal_field(trash_lookalike), "Trash");
        assert_eq!(sanitize_terminal_field(trash_lookalike), "Tra sh");
        assert_eq!(
            sanitize_terminal_field("a\u{ad}b\u{feff}c\u{2060}d\u{e0041}e"),
            "a b c d e"
        );
        // Bodies drop invisible characters instead of spacing words apart.
        assert_eq!(
            sanitize_terminal_body("co\u{ad}operate\u{200b}"),
            "cooperate"
        );

        // Joiners and emoji tag sequences (flag of England) survive.
        let england = "\u{1f3f4}\u{e0067}\u{e0062}\u{e0065}\u{e006e}\u{e0067}\u{e007f}";
        for text in [
            england,
            "\u{1f469}\u{200d}\u{1f4bb}",
            "\u{645}\u{200c}\u{645}",
        ] {
            assert_eq!(sanitize_terminal_field(text), text);
        }
        // Tags after a completed flag sequence are not part of it.
        assert_eq!(
            sanitize_terminal_field(&format!("{england}\u{e0041}")),
            format!("{england} ")
        );
    }

    #[test]
    fn terminal_sequences_are_removed_with_their_payloads() {
        let cases = [
            ("a\u{1b}[31;1mred\u{1b}[0mb", "aredb"),
            ("a\u{9b}2Jb", "ab"),
            (
                "a\u{1b}]8;;https://evil\u{1b}\\link\u{1b}]8;;\u{1b}\\b",
                "alinkb",
            ),
            ("a\u{9d}0;title\u{9c}b", "ab"),
            ("a\u{1b}Pq#0;2;0;0;0\u{1b}\\b", "ab"),
            ("a\u{1b}_apc payload\u{7}still\u{1b}\\b", "ab"),
            ("a\u{90}dcs\u{7}more\u{9c}b", "ab"),
            ("a\u{1b}(Bb", "ab"),
            ("a\u{1b}]52;c;unterminated secret", "a"),
            ("a\u{1b}[?25", "a"),
        ];
        for (input, expected) in cases {
            assert_eq!(sanitize_terminal_field(input), expected, "{input:?}");
            assert_eq!(sanitize_terminal_body(input), expected, "{input:?}");
        }
    }

    #[test]
    fn body_sanitizer_keeps_line_structure_and_unicode() {
        let body = "Grüße 世界\tcol\r\nline two\rline three\n\u{1b}[2Jline four\u{7}\u{202e}";
        assert_eq!(
            sanitize_terminal_body(body),
            "Grüße 世界\tcol\nline two\nline three\nline four  "
        );
        assert_eq!(
            sanitize_terminal_field(body),
            "Grüße 世界 col  line two line three line four  "
        );
    }

    #[test]
    fn table_keeps_json_values_complete() {
        let subject = "S".repeat(70);
        let from = format!("{}@example.com", "f".repeat(50));
        let message = MessageRow {
            subject: subject.clone(),
            from: from.clone(),
            date: "Mon, 1 Apr 2026 10:00:00 +0200".into(),
            ..row(1)
        };
        assert_eq!(truncate_str(&message.subject, 60).chars().count(), 60);
        assert_eq!(table_date(&message.date), "Mon, 1 Apr 2026 10:00:00");
        let parsed: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&[message]).unwrap()).unwrap();
        assert_eq!(parsed[0]["subject"], subject);
        assert_eq!(parsed[0]["from"], from);
        assert_eq!(parsed[0]["date"], "Mon, 1 Apr 2026 10:00:00 +0200");
        assert!(parsed[0].get("uid_validity").is_none());
    }

    #[test]
    fn json_empty() {
        let messages: Vec<MessageRow> = vec![];
        let json = serde_json::to_string(&messages).unwrap();
        assert_eq!(json, "[]");
    }

    fn row(uid: u32) -> MessageRow {
        MessageRow {
            account: None,
            uid,
            folder: None,
            from: "alice@example.com".into(),
            subject: "Test".into(),
            date: "Mon, 1 Apr 2026".into(),
            timestamp: 1774000000,
            size: 1024,
            message_id: None,
            in_reply_to: Vec::new(),
            references: Vec::new(),
            seen: false,
            answered: false,
            flagged: false,
            uid_validity: None,
        }
    }

    #[test]
    fn json_single_message() {
        let messages = vec![row(42)];
        let json = serde_json::to_string(&messages).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed[0]["uid"], 42);
        assert_eq!(parsed[0]["from"], "alice@example.com");
        assert_eq!(parsed[0]["subject"], "Test");
        assert_eq!(parsed[0]["size"], 1024);
        assert!(parsed[0]["folder"].is_null());
        assert!(parsed[0].get("account").is_none());
        assert!(parsed[0]["message_id"].is_null());
        assert_eq!(parsed[0]["in_reply_to"], serde_json::json!([]));
        assert_eq!(parsed[0]["references"], serde_json::json!([]));
        assert_eq!(parsed[0]["seen"], false);
        assert_eq!(parsed[0]["answered"], false);
        assert_eq!(parsed[0]["flagged"], false);
    }

    #[test]
    fn json_with_folder() {
        let messages = vec![MessageRow {
            folder: Some("INBOX".into()),
            ..row(1)
        }];
        let json = serde_json::to_string(&messages).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed[0]["folder"], "INBOX");
    }

    #[test]
    fn json_with_account() {
        let messages = vec![MessageRow {
            account: Some("work".into()),
            ..row(1)
        }];
        let json = serde_json::to_string(&messages).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed[0]["account"], "work");
    }
}
