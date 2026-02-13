use anyhow::{bail, Context, Result};
use regex::Regex;

use crate::connection::ImapSession;
use crate::display::MessageRow;

pub struct SearchCriteria {
    pub folder: String,
    pub all_folders: bool,
    pub subject: Option<String>,
    pub from: Option<String>,
    pub since: Option<String>,
    pub before: Option<String>,
    pub larger: Option<String>,
    pub limit: Option<usize>,
}

/// Strip CRLF and control chars to prevent IMAP command injection.
fn sanitize(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).collect()
}

/// Escape a string for use inside IMAP quoted strings (RFC 9051 §4.3).
pub fn imap_quote(s: &str) -> String {
    let clean = sanitize(s);
    let escaped = clean.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

/// Parse ISO 8601 date (YYYY-MM-DD) into IMAP date format (D-Mon-YYYY).
fn parse_date(s: &str) -> Result<String> {
    let re = Regex::new(r"^(\d{4})-(\d{2})-(\d{2})$").unwrap();
    let caps = re.captures(s).ok_or_else(|| {
        anyhow::anyhow!(
            "Invalid date '{}' (expected YYYY-MM-DD, e.g. 2025-01-31)",
            s
        )
    })?;

    let month_num: u32 = caps[2].parse()?;
    let day: u32 = caps[3].parse()?;

    let month_abbr = match month_num {
        1 => "Jan",
        2 => "Feb",
        3 => "Mar",
        4 => "Apr",
        5 => "May",
        6 => "Jun",
        7 => "Jul",
        8 => "Aug",
        9 => "Sep",
        10 => "Oct",
        11 => "Nov",
        12 => "Dec",
        _ => bail!(
            "Invalid date '{}' (expected YYYY-MM-DD, e.g. 2025-01-31)",
            s
        ),
    };

    if !(1..=31).contains(&day) {
        bail!(
            "Invalid date '{}' (expected YYYY-MM-DD, e.g. 2025-01-31)",
            s
        );
    }

    Ok(format!("{}-{}-{}", day, month_abbr, &caps[1]))
}

pub fn build_query(criteria: &SearchCriteria) -> Result<String> {
    let mut parts = Vec::new();

    if let Some(ref subj) = criteria.subject {
        parts.push(format!("SUBJECT {}", imap_quote(subj)));
    }
    if let Some(ref from) = criteria.from {
        parts.push(format!("FROM {}", imap_quote(from)));
    }
    if let Some(ref since) = criteria.since {
        let date = parse_date(since)?;
        parts.push(format!("SINCE {date}"));
    }
    if let Some(ref before) = criteria.before {
        let date = parse_date(before)?;
        parts.push(format!("BEFORE {date}"));
    }
    if let Some(ref larger) = criteria.larger {
        let bytes = parse_size(larger);
        parts.push(format!("LARGER {bytes}"));
    }

    if parts.is_empty() {
        Ok("ALL".to_string())
    } else {
        Ok(parts.join(" "))
    }
}

fn parse_size(s: &str) -> u64 {
    let s = s.trim();
    if let Some(n) = s.strip_suffix('M').or_else(|| s.strip_suffix('m')) {
        n.trim()
            .parse::<u64>()
            .unwrap_or(0)
            .saturating_mul(1_048_576)
    } else if let Some(n) = s.strip_suffix('K').or_else(|| s.strip_suffix('k')) {
        n.trim().parse::<u64>().unwrap_or(0).saturating_mul(1024)
    } else {
        s.parse::<u64>().unwrap_or(0)
    }
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

/// Parse SORT response bytes into a Vec of UIDs (preserving server order).
fn parse_sort_response(data: &[u8]) -> Result<Vec<u32>> {
    let text = String::from_utf8_lossy(data);
    let mut uids = Vec::new();
    let mut saw_sort = false;

    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("* SORT ") {
            saw_sort = true;
            for tok in rest.split_whitespace() {
                if let Ok(uid) = tok.parse::<u32>() {
                    uids.push(uid);
                }
            }
        }
        // Check for error in tagged response
        if (line.contains("BAD") || line.contains("NO")) && !line.starts_with('*') {
            bail!("SORT command rejected by server: {line}");
        }
    }

    // Empty SORT response (no matches) is valid — server sends "* SORT" with no UIDs
    // or may omit the line entirely
    if !saw_sort && !uids.is_empty() {
        bail!("Unexpected SORT response format");
    }

    Ok(uids)
}

/// Try UID SORT (REVERSE DATE), returns Ok(Some(ordered_uids)) if server supports SORT,
/// Ok(None) if not, or Err on failure.
fn try_uid_sort(session: &mut ImapSession, query: &str) -> Result<Option<Vec<u32>>> {
    if !session.has_capability("SORT") {
        return Ok(None);
    }

    let cmd = format!("UID SORT (REVERSE DATE) UTF-8 {query}");
    match session.run_command_and_read_response(&cmd) {
        Ok(data) => {
            let uids = parse_sort_response(&data)?;
            Ok(Some(uids))
        }
        Err(e) => {
            eprintln!("SORT failed, falling back to SEARCH: {e}");
            Ok(None)
        }
    }
}

/// Build UID set strings with range compression, chunked to stay under IMAP command length limits.
/// Consecutive UIDs are compressed into `start:end` ranges. Each returned string stays under 4000 chars.
pub fn build_uid_set(uids: &[u32]) -> Vec<String> {
    if uids.is_empty() {
        return Vec::new();
    }

    let mut sorted: Vec<u32> = uids.to_vec();
    sorted.sort_unstable();
    sorted.dedup();

    // Build ranges
    let mut ranges: Vec<(u32, u32)> = Vec::new();
    let mut start = sorted[0];
    let mut end = sorted[0];
    for &uid in &sorted[1..] {
        if uid == end + 1 {
            end = uid;
        } else {
            ranges.push((start, end));
            start = uid;
            end = uid;
        }
    }
    ranges.push((start, end));

    // Chunk into strings under 4000 chars
    let mut chunks = Vec::new();
    let mut current = String::new();
    for (s, e) in &ranges {
        let part = if s == e {
            format!("{s}")
        } else {
            format!("{s}:{e}")
        };
        if current.is_empty() {
            current = part;
        } else if current.len() + 1 + part.len() > 4000 {
            chunks.push(std::mem::take(&mut current));
            current = part;
        } else {
            current.push(',');
            current.push_str(&part);
        }
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

fn fetch_messages(
    session: &mut ImapSession,
    folder: &str,
    query: &str,
    include_folder: bool,
    limit: Option<usize>,
) -> Result<Vec<MessageRow>> {
    // Sanitize folder name for the raw SORT path
    let clean_folder = sanitize(folder);
    session
        .select(&clean_folder)
        .with_context(|| format!("Failed to select folder '{clean_folder}'"))?;

    // Try server-side SORT first, fall back to SEARCH + client sort
    let (ordered_uids, pre_sorted) = match try_uid_sort(session, query)? {
        Some(mut uids) => {
            // With server SORT, we can truncate before FETCH
            if let Some(n) = limit {
                uids.truncate(n);
            }
            (uids, true)
        }
        None => {
            let uid_set = session.uid_search(query).context("IMAP SEARCH failed")?;
            let mut uids: Vec<u32> = uid_set.into_iter().collect();
            uids.sort();
            (uids, false)
        }
    };

    if ordered_uids.is_empty() {
        return Ok(Vec::new());
    }

    let uid_chunks = build_uid_set(&ordered_uids);

    // FETCH results may come back in arbitrary order; index by UID
    let mut by_uid = std::collections::HashMap::new();
    for chunk in &uid_chunks {
        let fetches = session
            .uid_fetch(
                chunk,
                "(UID FLAGS RFC822.SIZE BODY.PEEK[HEADER.FIELDS (Subject From Date)])",
            )
            .context("IMAP FETCH failed")?;

        for fetch in fetches.iter() {
            let uid = match fetch.uid {
                Some(u) if u > 0 => u,
                _ => continue, // Skip invalid UIDs
            };
            let size = fetch.size.unwrap_or(0);
            let header_bytes = fetch.header().unwrap_or(b"");
            let header_str = String::from_utf8_lossy(header_bytes);

            let (mut subject, mut from, mut date) = (String::new(), String::new(), String::new());

            let parsed = mailparse::parse_headers(header_bytes);
            if let Ok((headers, _)) = parsed {
                for h in &headers {
                    match h.get_key().to_lowercase().as_str() {
                        "subject" => subject = h.get_value(),
                        "from" => from = h.get_value(),
                        "date" => date = h.get_value(),
                        _ => {}
                    }
                }
            } else {
                for line in header_str.lines() {
                    if let Some(v) = line.strip_prefix("Subject: ") {
                        subject = v.to_string();
                    } else if let Some(v) = line.strip_prefix("From: ") {
                        from = v.to_string();
                    } else if let Some(v) = line.strip_prefix("Date: ") {
                        date = v.to_string();
                    }
                }
            }

            from = truncate_str(&from, 40);
            subject = truncate_str(&subject, 60);
            let timestamp = mailparse::dateparse(&date).unwrap_or(0);

            if let Some(pos) = date.find(" +").or_else(|| date.find(" -")) {
                date.truncate(pos);
            }

            by_uid.insert(
                uid,
                MessageRow {
                    uid,
                    folder: if include_folder {
                        Some(clean_folder.clone())
                    } else {
                        None
                    },
                    from,
                    subject,
                    date,
                    timestamp,
                    size,
                },
            );
        }
    }

    if pre_sorted {
        // Preserve server SORT order
        Ok(ordered_uids
            .into_iter()
            .filter_map(|uid| by_uid.remove(&uid))
            .collect())
    } else {
        let mut messages: Vec<MessageRow> = by_uid.into_values().collect();
        messages.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
        if let Some(n) = limit {
            messages.truncate(n);
        }
        Ok(messages)
    }
}

pub fn folders_to_skip(name: &str) -> bool {
    let lower = name.to_lowercase();
    lower == "trash"
        || lower == "spam"
        || lower == "junk"
        || lower.contains("all mail")
        || lower == "[gmail]/all mail"
        || lower == "[gmail]/spam"
        || lower == "[gmail]/trash"
}

pub fn search(session: &mut ImapSession, criteria: &SearchCriteria) -> Result<Vec<MessageRow>> {
    let query = build_query(criteria)?;

    if criteria.all_folders {
        let folders = session
            .list(Some(""), Some("*"))
            .context("Failed to list folders")?;
        let folder_names: Vec<String> = folders
            .iter()
            .map(|f| f.name().to_string())
            .filter(|n| !folders_to_skip(n))
            .collect();

        let mut all_messages = Vec::new();
        for folder in &folder_names {
            match fetch_messages(session, folder, &query, true, None) {
                Ok(msgs) => all_messages.extend(msgs),
                Err(e) => {
                    eprintln!("Warning: skipping folder '{folder}': {e}");
                }
            }
        }
        all_messages.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
        if let Some(n) = criteria.limit {
            all_messages.truncate(n);
        }
        Ok(all_messages)
    } else {
        fetch_messages(session, &criteria.folder, &query, false, criteria.limit)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_removes_control_chars() {
        assert_eq!(sanitize("hello"), "hello");
        assert_eq!(sanitize("he\nllo"), "hello");
        assert_eq!(sanitize("he\rllo"), "hello");
        assert_eq!(sanitize("he\r\nllo"), "hello");
        assert_eq!(sanitize("he\x00llo"), "hello");
        assert_eq!(sanitize(""), "");
    }

    #[test]
    fn sanitize_preserves_unicode() {
        assert_eq!(sanitize("héllo wörld"), "héllo wörld");
    }

    #[test]
    fn imap_quote_wraps_in_quotes() {
        assert_eq!(imap_quote("hello"), "\"hello\"");
    }

    #[test]
    fn imap_quote_escapes_backslash() {
        assert_eq!(imap_quote("he\\llo"), "\"he\\\\llo\"");
    }

    #[test]
    fn imap_quote_escapes_double_quote() {
        assert_eq!(imap_quote("he\"llo"), "\"he\\\"llo\"");
    }

    #[test]
    fn imap_quote_strips_control_chars() {
        assert_eq!(imap_quote("he\nllo"), "\"hello\"");
    }

    #[test]
    fn parse_date_converts_iso_to_imap() {
        assert_eq!(parse_date("2025-01-01").unwrap(), "1-Jan-2025");
        assert_eq!(parse_date("2025-01-31").unwrap(), "31-Jan-2025");
        assert_eq!(parse_date("2024-12-31").unwrap(), "31-Dec-2024");
        assert_eq!(parse_date("2025-06-15").unwrap(), "15-Jun-2025");
    }

    #[test]
    fn parse_date_rejects_invalid_formats() {
        assert!(parse_date("1-Jan-2025").is_err());
        assert!(parse_date("Jan-1-2025").is_err());
        assert!(parse_date("").is_err());
        assert!(parse_date("2025-13-01").is_err());
        assert!(parse_date("2025-00-01").is_err());
        assert!(parse_date("2025-01-00").is_err());
        assert!(parse_date("2025-01-32").is_err());
    }

    #[test]
    fn parse_size_plain_bytes() {
        assert_eq!(parse_size("1024"), 1024);
        assert_eq!(parse_size("0"), 0);
    }

    #[test]
    fn parse_size_kilobytes() {
        assert_eq!(parse_size("1K"), 1024);
        assert_eq!(parse_size("1k"), 1024);
        assert_eq!(parse_size("10K"), 10240);
    }

    #[test]
    fn parse_size_megabytes() {
        assert_eq!(parse_size("1M"), 1_048_576);
        assert_eq!(parse_size("1m"), 1_048_576);
        assert_eq!(parse_size("5M"), 5_242_880);
    }

    #[test]
    fn parse_size_invalid_returns_zero() {
        assert_eq!(parse_size("abc"), 0);
        assert_eq!(parse_size(""), 0);
    }

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
    fn build_query_no_criteria_returns_all() {
        let c = SearchCriteria {
            folder: "INBOX".into(),
            all_folders: false,
            subject: None,
            from: None,
            since: None,
            before: None,
            larger: None,
            limit: None,
        };
        assert_eq!(build_query(&c).unwrap(), "ALL");
    }

    #[test]
    fn build_query_subject_only() {
        let c = SearchCriteria {
            folder: "INBOX".into(),
            all_folders: false,
            subject: Some("test".into()),
            from: None,
            since: None,
            before: None,
            larger: None,
            limit: None,
        };
        assert_eq!(build_query(&c).unwrap(), "SUBJECT \"test\"");
    }

    #[test]
    fn build_query_combined_fields() {
        let c = SearchCriteria {
            folder: "INBOX".into(),
            all_folders: false,
            subject: Some("invoice".into()),
            from: Some("user@example.com".into()),
            since: None,
            before: None,
            larger: None,
            limit: None,
        };
        assert_eq!(
            build_query(&c).unwrap(),
            "SUBJECT \"invoice\" FROM \"user@example.com\""
        );
    }

    #[test]
    fn build_query_date_range() {
        let c = SearchCriteria {
            folder: "INBOX".into(),
            all_folders: false,
            subject: None,
            from: None,
            since: Some("2025-01-01".into()),
            before: Some("2025-12-31".into()),
            larger: None,
            limit: None,
        };
        assert_eq!(
            build_query(&c).unwrap(),
            "SINCE 1-Jan-2025 BEFORE 31-Dec-2025"
        );
    }

    #[test]
    fn build_query_size_filter() {
        let c = SearchCriteria {
            folder: "INBOX".into(),
            all_folders: false,
            subject: None,
            from: None,
            since: None,
            before: None,
            larger: Some("1M".into()),
            limit: None,
        };
        assert_eq!(build_query(&c).unwrap(), "LARGER 1048576");
    }

    #[test]
    fn build_query_invalid_date_errors() {
        let c = SearchCriteria {
            folder: "INBOX".into(),
            all_folders: false,
            subject: None,
            from: None,
            since: Some("not-a-date".into()),
            before: None,
            larger: None,
            limit: None,
        };
        assert!(build_query(&c).is_err());
    }

    #[test]
    fn parse_sort_response_basic() {
        let data = b"* SORT 5 3 1\r\nA001 OK SORT completed\r\n";
        let uids = parse_sort_response(data).unwrap();
        assert_eq!(uids, vec![5, 3, 1]);
    }

    #[test]
    fn parse_sort_response_empty() {
        let data = b"A001 OK SORT completed\r\n";
        let uids = parse_sort_response(data).unwrap();
        assert!(uids.is_empty());
    }

    #[test]
    fn parse_sort_response_server_error() {
        let data = b"A001 BAD Unknown command\r\n";
        assert!(parse_sort_response(data).is_err());
    }

    #[test]
    fn folders_to_skip_filters_correctly() {
        assert!(folders_to_skip("Trash"));
        assert!(folders_to_skip("Spam"));
        assert!(folders_to_skip("Junk"));
        assert!(folders_to_skip("[Gmail]/All Mail"));
        assert!(folders_to_skip("[Gmail]/Spam"));
        assert!(folders_to_skip("[Gmail]/Trash"));
        assert!(!folders_to_skip("INBOX"));
        assert!(!folders_to_skip("Archive"));
        assert!(!folders_to_skip("Sent"));
    }

    #[test]
    fn build_uid_set_empty() {
        assert!(build_uid_set(&[]).is_empty());
    }

    #[test]
    fn build_uid_set_single() {
        assert_eq!(build_uid_set(&[42]), vec!["42"]);
    }

    #[test]
    fn build_uid_set_compresses_ranges() {
        assert_eq!(build_uid_set(&[1, 2, 3, 5, 7, 8, 9]), vec!["1:3,5,7:9"]);
    }

    #[test]
    fn build_uid_set_unsorted_input() {
        assert_eq!(build_uid_set(&[5, 3, 1, 2, 4]), vec!["1:5"]);
    }

    #[test]
    fn build_uid_set_deduplicates() {
        assert_eq!(build_uid_set(&[1, 1, 2, 2, 3]), vec!["1:3"]);
    }

    #[test]
    fn build_uid_set_chunks_large_sets() {
        // Generate enough UIDs to exceed 4000 chars (non-consecutive to prevent compression)
        let uids: Vec<u32> = (0..2000).map(|i| i * 3).collect();
        let chunks = build_uid_set(&uids);
        assert!(chunks.len() > 1);
        for chunk in &chunks {
            assert!(chunk.len() <= 4000);
        }
    }
}
