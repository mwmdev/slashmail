use comfy_table::{presets::UTF8_FULL_CONDENSED, Cell, ContentArrangement, Table};

#[derive(serde::Serialize)]
pub struct MessageRow {
    pub uid: u32,
    pub folder: Option<String>,
    pub from: String,
    pub subject: String,
    pub date: String,
    pub timestamp: i64,
    pub size: u32,
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

#[cfg(test)]
mod tests {
    use super::*;

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
    fn json_empty() {
        let messages: Vec<MessageRow> = vec![];
        let json = serde_json::to_string(&messages).unwrap();
        assert_eq!(json, "[]");
    }

    #[test]
    fn json_single_message() {
        let messages = vec![MessageRow {
            uid: 42,
            folder: None,
            from: "alice@example.com".into(),
            subject: "Test".into(),
            date: "Mon, 1 Apr 2026".into(),
            timestamp: 1774000000,
            size: 1024,
        }];
        let json = serde_json::to_string(&messages).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed[0]["uid"], 42);
        assert_eq!(parsed[0]["from"], "alice@example.com");
        assert_eq!(parsed[0]["subject"], "Test");
        assert_eq!(parsed[0]["size"], 1024);
        assert!(parsed[0]["folder"].is_null());
    }

    #[test]
    fn json_with_folder() {
        let messages = vec![MessageRow {
            uid: 1,
            folder: Some("INBOX".into()),
            from: "bob@example.com".into(),
            subject: "Hi".into(),
            date: "Tue, 2 Apr 2026".into(),
            timestamp: 1774100000,
            size: 512,
        }];
        let json = serde_json::to_string(&messages).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed[0]["folder"], "INBOX");
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

    let has_folder = messages.iter().any(|m| m.folder.is_some());
    let mut table = Table::new();
    table.load_preset(UTF8_FULL_CONDENSED);
    table.set_content_arrangement(ContentArrangement::Dynamic);

    let mut header = vec!["UID", "From", "Subject", "Date", "Size"];
    if has_folder {
        header.insert(1, "Folder");
    }
    table.set_header(header);

    for msg in messages {
        let mut row: Vec<Cell> = vec![Cell::new(msg.uid)];
        if has_folder {
            row.push(Cell::new(msg.folder.as_deref().unwrap_or("")));
        }
        row.push(Cell::new(&msg.from));
        row.push(Cell::new(&msg.subject));
        row.push(Cell::new(&msg.date));
        row.push(Cell::new(format_size(msg.size as u64)));
        table.add_row(row);
    }

    println!("{table}");
    println!("{} message(s)", messages.len());
}
