use anyhow::{bail, Context, Result};
use regex::Regex;
use std::collections::{BTreeMap, HashMap, HashSet};

use crate::connection::ImapSession;
use crate::display::{
    sanitize_folder_name, sanitize_terminal_field, MessageRow, ARRIVAL_SLACK_SECS,
};
use crate::draft::MailboxListing;
use imap::types::Flag;

pub const NON_ASCII_SEARCH_UNSUPPORTED: &str =
    "Non-ASCII search requires server support for LITERAL+ or LITERAL-";
pub const LONG_LITERAL_UNSUPPORTED: &str =
    "Non-ASCII search terms over 4096 bytes require server support for LITERAL+";

/// Largest non-synchronizing literal a LITERAL- server accepts (RFC 7888 §5).
const LITERAL_MINUS_MAX: usize = 4096;

pub struct SearchCriteria {
    pub folder: String,
    pub all_folders: bool,
    pub uid: Option<u32>,
    pub subject: Option<String>,
    pub from: Option<String>,
    pub to: Option<String>,
    pub cc: Option<String>,
    pub body: Option<String>,
    pub text: Option<String>,
    pub seen: bool,
    pub unseen: bool,
    pub since: Option<String>,
    pub before: Option<String>,
    pub larger: Option<String>,
    pub smaller: Option<String>,
    pub flagged: bool,
    pub unflagged: bool,
    pub answered: bool,
    pub draft: bool,
    pub limit: Option<usize>,
    /// Order rows by [`MessageRow::sort_time`] on the client instead of with
    /// server SORT, because they will be merged with other accounts' rows by
    /// that key (`--all-accounts`); SORT orders by the uncapped Date header.
    pub client_order: bool,
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

/// Serialize a user search term. ASCII terms become quoted strings; other
/// terms become UTF-8 non-synchronizing literals (RFC 7888), whose length
/// is the byte count of the sanitized text.
fn quote_search_term(value: &str) -> String {
    let clean = sanitize(value);
    if clean.is_ascii() {
        imap_quote(&clean)
    } else {
        format!("{{{}+}}\r\n{clean}", clean.len())
    }
}

/// Reject a built query the connected server cannot receive. Non-ASCII
/// terms are sent as non-synchronizing literals: any size with LITERAL+, at
/// most 4096 bytes with LITERAL-. Without either the search fails here
/// rather than silently matching nothing.
pub fn ensure_query_supported(session: &ImapSession, query: &str) -> Result<()> {
    if query.is_ascii() || session.has_capability("LITERAL+") {
        return Ok(());
    }
    if !session.has_capability("LITERAL-") {
        bail!(NON_ASCII_SEARCH_UNSUPPORTED);
    }
    if literal_lengths(query).any(|length| length > LITERAL_MINUS_MAX) {
        bail!(LONG_LITERAL_UNSUPPORTED);
    }
    Ok(())
}

/// Byte lengths of the `{N+}` literals in a built query. Terms are stripped
/// of control characters, so every CRLF ends a literal prefix; text after
/// the last CRLF is literal data or a quoted term, never a prefix.
fn literal_lengths(query: &str) -> impl Iterator<Item = usize> + '_ {
    let prefixes = query.rsplit_once("\r\n").map_or("", |(before, _)| before);
    prefixes.split("\r\n").filter_map(|before| {
        let (_, digits) = before.strip_suffix("+}")?.rsplit_once('{')?;
        digits.parse().ok()
    })
}

/// Mailbox names are sent to the server unchanged; reject names that cannot
/// be sent safely instead of silently altering them.
pub fn validate_folder_name(folder: &str) -> Result<()> {
    if folder.is_empty() {
        bail!("Folder name must not be empty");
    }
    if folder.chars().any(char::is_control) {
        bail!("Folder name must not contain control characters");
    }
    Ok(())
}

const MONTH_ABBRS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

// RFC 3501 states servers should accept at least 8000 octets per command line.
// Keep UID set chunks comfortably below that floor.
const MAX_UID_SET_LENGTH: usize = 4000;

fn format_imap_date(day: u32, month: u32, year: i64) -> Result<String> {
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        bail!("Invalid computed date: {year}-{month:02}-{day:02}");
    }
    Ok(format!(
        "{}-{}-{}",
        day,
        MONTH_ABBRS[(month - 1) as usize],
        year
    ))
}

/// Convert seconds since epoch to (year, month, day) using civil calendar math.
fn epoch_to_date(secs: i64) -> (i64, u32, u32) {
    // Algorithm from Howard Hinnant's chrono-Compatible Low-Level Date Algorithms
    let z = secs / 86400 + 719468;
    let era = (if z >= 0 { z } else { z - 146096 }) / 146097;
    let doe = (z - era * 146097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

/// Resolve a relative date shorthand (e.g. "7d", "2w", "3m", "1y") to an IMAP date.
fn resolve_relative_date(s: &str) -> Option<Result<String>> {
    let re = Regex::new(r"^(\d+)([dwmy])$").unwrap();
    let caps = re.captures(s)?;
    let n: u32 = match caps[1].parse() {
        Ok(v) => v,
        Err(e) => return Some(Err(e.into())),
    };
    let unit = &caps[2];

    let now_secs = now_epoch_secs();

    Some(match unit {
        "d" | "w" => {
            let days = if unit == "w" { n as i64 * 7 } else { n as i64 };
            let (y, m, d) = epoch_to_date(now_secs - days * 86400);
            format_imap_date(d, m, y)
        }
        "m" => {
            let (year, month, day) = epoch_to_date(now_secs);
            let total_months = (year * 12 + month as i64 - 1) - n as i64;
            let y = total_months.div_euclid(12);
            let m = (total_months.rem_euclid(12) + 1) as u32;
            let d = day.min(days_in_month(y, m));
            format_imap_date(d, m, y)
        }
        "y" => {
            let (year, month, day) = epoch_to_date(now_secs);
            let y = year - n as i64;
            let d = day.min(days_in_month(y, month));
            format_imap_date(d, month, y)
        }
        _ => unreachable!(),
    })
}

fn now_epoch_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

fn days_in_month(year: i64, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) {
                29
            } else {
                28
            }
        }
        _ => 30,
    }
}

/// Parse date into IMAP format (D-Mon-YYYY).
/// Accepts ISO 8601 (YYYY-MM-DD) or relative shorthand (7d, 2w, 3m, 1y).
fn parse_date(s: &str) -> Result<String> {
    if let Some(result) = resolve_relative_date(s) {
        return result;
    }

    let re = Regex::new(r"^(\d{4})-(\d{2})-(\d{2})$").unwrap();
    let caps = re.captures(s).ok_or_else(|| {
        anyhow::anyhow!(
            "Invalid date '{}' (expected YYYY-MM-DD or relative like 7d, 2w, 3m, 1y)",
            s
        )
    })?;

    let year: i64 = caps[1].parse()?;
    let month: u32 = caps[2].parse()?;
    let day: u32 = caps[3].parse()?;

    format_imap_date(day, month, year).map_err(|_| {
        anyhow::anyhow!(
            "Invalid date '{}' (expected YYYY-MM-DD, e.g. 2025-01-31)",
            s
        )
    })
}

pub fn build_query(criteria: &SearchCriteria) -> Result<String> {
    let mut parts = Vec::new();

    if let Some(uid) = criteria.uid {
        parts.push(format!("UID {uid}"));
    }
    let text_terms = [
        ("SUBJECT", "subject", &criteria.subject),
        ("FROM", "from", &criteria.from),
        ("TO", "to", &criteria.to),
        ("CC", "cc", &criteria.cc),
        ("BODY", "body", &criteria.body),
        ("TEXT", "text", &criteria.text),
    ];
    for (key, flag, value) in text_terms {
        if let Some(value) = value {
            // An empty IMAP substring matches every message, so a filter
            // emptied by an unset variable or stripped controls must not
            // silently widen the selection.
            if sanitize(value).trim().is_empty() {
                bail!("--{flag} must not be empty");
            }
            parts.push(format!("{key} {}", quote_search_term(value)));
        }
    }
    if criteria.seen {
        parts.push("SEEN".to_string());
    }
    if criteria.unseen {
        parts.push("UNSEEN".to_string());
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
        let bytes = parse_size(larger)?;
        parts.push(format!("LARGER {bytes}"));
    }
    if let Some(ref smaller) = criteria.smaller {
        let bytes = parse_size(smaller)?;
        parts.push(format!("SMALLER {bytes}"));
    }
    if criteria.flagged {
        parts.push("FLAGGED".to_string());
    }
    if criteria.unflagged {
        parts.push("UNFLAGGED".to_string());
    }
    if criteria.answered {
        parts.push("ANSWERED".to_string());
    }
    if criteria.draft {
        parts.push("DRAFT".to_string());
    }

    if parts.is_empty() {
        Ok("ALL".to_string())
    } else {
        Ok(parts.join(" "))
    }
}

fn parse_size(s: &str) -> Result<u64> {
    let s = s.trim();
    if s.is_empty() {
        bail!("Invalid size '' (expected bytes, or value with K/M suffix such as 10K or 5M)");
    }

    let (raw, multiplier) = if let Some(n) = s.strip_suffix('M').or_else(|| s.strip_suffix('m')) {
        (n.trim(), 1_048_576_u64)
    } else if let Some(n) = s.strip_suffix('K').or_else(|| s.strip_suffix('k')) {
        (n.trim(), 1024_u64)
    } else {
        (s, 1_u64)
    };

    let value = raw.parse::<u64>().with_context(|| {
        format!("Invalid size '{s}' (expected bytes, or value with K/M suffix such as 10K or 5M)")
    })?;

    value
        .checked_mul(multiplier)
        .ok_or_else(|| anyhow::anyhow!("Size '{s}' is too large"))
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
        // Check for error in tagged response (format: "tag NO ..." or "tag BAD ...")
        if !line.starts_with('*') {
            let mut tokens = line.splitn(3, ' ');
            if let (Some(_tag), Some(status)) = (tokens.next(), tokens.next()) {
                if status == "NO" || status == "BAD" {
                    bail!("SORT command rejected by server: {line}");
                }
            }
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
            eprintln!(
                "SORT failed, falling back to SEARCH: {}",
                sanitize_terminal_field(&e.to_string())
            );
            Ok(None)
        }
    }
}

/// Number of UIDs in a set built by `build_uid_set` (sorted, deduplicated).
pub fn uid_set_len(set: &str) -> usize {
    set.split(',')
        .map(|part| match part.split_once(':') {
            Some((start, end)) => {
                let start: usize = start.parse().unwrap_or(0);
                let end: usize = end.parse().unwrap_or(0);
                end.saturating_sub(start) + 1
            }
            None => 1,
        })
        .sum()
}

/// Build UID set strings with range compression, chunked to stay under IMAP command length limits.
/// Consecutive UIDs are compressed into `start:end` ranges.
/// Each returned string stays under MAX_UID_SET_LENGTH chars.
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

    // Chunk into strings under MAX_UID_SET_LENGTH chars.
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
        } else if current.len() + 1 + part.len() > MAX_UID_SET_LENGTH {
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

/// Rows of one folder matching `query`, newest first, at most `limit`.
/// With `merged` (one folder of `--all-folders`), rows carry their folder.
/// With `server_sort`, the server's SORT order is used when available;
/// otherwise rows are ordered by [`MessageRow::sort_time`], the key results
/// are merged by, so cutting each folder or account to `limit` keeps the
/// overall newest.
fn fetch_messages(
    session: &mut ImapSession,
    folder: &str,
    query: &str,
    merged: bool,
    server_sort: bool,
    limit: Option<usize>,
) -> Result<Vec<MessageRow>> {
    validate_folder_name(folder)?;
    let opened = session.examine(folder).with_context(|| {
        format!(
            "Failed to examine folder '{}'",
            sanitize_folder_name(folder)
        )
    })?;
    let rows = RowFetch {
        folder,
        include_folder: merged,
        uid_validity: opened.uid_validity,
    };

    let sorted = if server_sort {
        try_uid_sort(session, query)?
    } else {
        None
    };
    let mut messages = match sorted {
        // Server SORT order is kept, and the limit applies before FETCH.
        Some(mut uids) => {
            if let Some(n) = limit {
                uids.truncate(n);
            }
            let mut by_uid = rows.fetch(session, &uids)?;
            uids.iter().filter_map(|uid| by_uid.remove(uid)).collect()
        }
        None => newest_by_date(session, query, limit, &rows)?,
    };

    // Gmail lists a message in every label folder; multi-folder results
    // carry its message ID so `search_excluding` can keep one row.
    if merged && !messages.is_empty() && session.has_capability("X-GM-EXT-1") {
        let uids: Vec<u32> = messages.iter().map(|message| message.uid).collect();
        let mut ids = HashMap::new();
        for chunk in &build_uid_set(&uids) {
            ids.extend(
                session
                    .gmail_message_ids(chunk)
                    .context("IMAP FETCH X-GM-MSGID failed")?,
            );
        }
        for message in &mut messages {
            message.gmail_msgid = ids.get(&message.uid).copied();
        }
    }
    Ok(messages)
}

/// Below this many matches, fetching every header is cheaper than extra
/// SEARCH round trips.
const NARROW_ABOVE: usize = 500;
/// Recent windows tried, in days, before fetching every match.
const RECENT_WINDOW_DAYS: [i64; 3] = [7, 31, 366];
const DAY_SECS: i64 = 86_400;

/// Without SORT: matching rows, newest first by [`MessageRow::sort_time`]
/// (ties: newest UID first), truncated to `limit`. Headers cost time per
/// message (about 1 ms each on Gmail), so with a limit and many matches the
/// search is first narrowed with SINCE to recent windows (SINCE uses the
/// arrival date, which servers index; SENTSINCE parses every Date header
/// and took 15 s on a 147,000-message Proton Bridge folder). A message
/// outside a window starting on day `d` arrived before `d` + 1 day (its
/// zone shifts the day by less than one) and sorts at most
/// [`ARRIVAL_SLACK_SECS`] after arriving. Once `limit` rows sort at or after
/// `d` + 1 day + that slack, they are the newest.
fn newest_by_date(
    session: &mut ImapSession,
    query: &str,
    limit: Option<usize>,
    rows: &RowFetch,
) -> Result<Vec<MessageRow>> {
    if limit == Some(0) {
        return Ok(Vec::new());
    }
    let uids: Vec<u32> = session
        .uid_search(query)
        .context("IMAP SEARCH failed")?
        .into_iter()
        .collect();
    let mut fetched = HashMap::new();

    if let Some(n) = limit.filter(|&n| uids.len() > n.max(NARROW_ABOVE)) {
        let now = now_epoch_secs();
        for days in RECENT_WINDOW_DAYS {
            let day_start = (now - days * DAY_SECS).div_euclid(DAY_SECS) * DAY_SECS;
            let (year, month, day) = epoch_to_date(day_start);
            let window = session
                .uid_search(&format!(
                    "{query} SINCE {}",
                    format_imap_date(day, month, year)?
                ))
                .context("IMAP SEARCH failed")?;
            if window.len() < n {
                continue;
            }
            let missing: Vec<u32> = window
                .iter()
                .copied()
                .filter(|uid| !fetched.contains_key(uid))
                .collect();
            fetched.extend(rows.fetch(session, &missing)?);
            let mut newest: Vec<(i64, u32)> = window
                .iter()
                .filter_map(|uid| fetched.get(uid).map(|row| (row.sort_time(), *uid)))
                .collect();
            newest.sort_unstable_by(|a, b| b.cmp(a));
            if newest
                .get(n - 1)
                .is_some_and(|(time, _)| *time >= day_start + DAY_SECS + ARRIVAL_SLACK_SECS)
            {
                return Ok(newest[..n]
                    .iter()
                    .filter_map(|(_, uid)| fetched.remove(uid))
                    .collect());
            }
        }
    }

    let missing: Vec<u32> = uids
        .into_iter()
        .filter(|uid| !fetched.contains_key(uid))
        .collect();
    fetched.extend(rows.fetch(session, &missing)?);
    let mut messages: Vec<MessageRow> = fetched.into_values().collect();
    messages.sort_unstable_by_key(|message| std::cmp::Reverse((message.sort_time(), message.uid)));
    if let Some(n) = limit {
        messages.truncate(n);
    }
    Ok(messages)
}

/// Builds header rows for UIDs of the selected mailbox.
struct RowFetch<'a> {
    folder: &'a str,
    include_folder: bool,
    uid_validity: Option<u32>,
}

impl RowFetch<'_> {
    /// Rows for `uids`, keyed by UID. Servers may interleave unsolicited
    /// FETCH responses (flag changes made by other clients): only requested
    /// UIDs become rows, and a flags-only response never replaces a row that
    /// has headers.
    fn fetch(&self, session: &mut ImapSession, uids: &[u32]) -> Result<HashMap<u32, MessageRow>> {
        let requested: HashSet<u32> = uids.iter().copied().collect();
        let mut by_uid = HashMap::new();
        for chunk in &build_uid_set(uids) {
            let mut warned_invalid_uid = false;
            let fetches = session
                .uid_fetch(
                    chunk,
                    "(UID FLAGS RFC822.SIZE INTERNALDATE BODY.PEEK[HEADER.FIELDS (Subject From Date Message-ID In-Reply-To References)])",
                )
                .context("IMAP FETCH failed")?;

            for fetch in fetches.iter() {
                let uid = match fetch.uid {
                    Some(u) if u > 0 => u,
                    _ => {
                        if !warned_invalid_uid {
                            eprintln!(
                                "Warning: skipping fetched message(s) with missing/invalid UID in '{}'",
                                sanitize_folder_name(self.folder)
                            );
                            warned_invalid_uid = true;
                        }
                        continue;
                    }
                };
                if !requested.contains(&uid)
                    || (fetch.header().is_none() && by_uid.contains_key(&uid))
                {
                    continue;
                }
                by_uid.insert(uid, self.row(uid, fetch));
            }
        }
        Ok(by_uid)
    }

    fn row(&self, uid: u32, fetch: &imap::types::Fetch<'_>) -> MessageRow {
        let header_bytes = fetch.header().unwrap_or(b"");
        let (mut subject, mut from, mut date) = (String::new(), String::new(), String::new());
        let (mut message_id, mut in_reply_to, mut references) = (None, Vec::new(), Vec::new());

        if let Ok((headers, _)) = mailparse::parse_headers(header_bytes) {
            for h in &headers {
                match h.get_key().to_lowercase().as_str() {
                    "subject" => subject = h.get_value(),
                    "from" => from = h.get_value(),
                    "date" => date = h.get_value(),
                    "message-id" => message_id = message_ids(&h.get_value()).into_iter().next(),
                    "in-reply-to" => in_reply_to = message_ids(&h.get_value()),
                    "references" => references = message_ids(&h.get_value()),
                    _ => {}
                }
            }
        } else {
            for line in String::from_utf8_lossy(header_bytes).lines() {
                if let Some(v) = line.strip_prefix("Subject: ") {
                    subject = v.to_string();
                } else if let Some(v) = line.strip_prefix("From: ") {
                    from = v.to_string();
                } else if let Some(v) = line.strip_prefix("Date: ") {
                    date = v.to_string();
                } else if let Some(v) = line.strip_prefix("Message-ID: ") {
                    message_id = message_ids(v).into_iter().next();
                } else if let Some(v) = line.strip_prefix("In-Reply-To: ") {
                    in_reply_to = message_ids(v);
                } else if let Some(v) = line.strip_prefix("References: ") {
                    references = message_ids(v);
                }
            }
        }

        let flags = fetch.flags();
        let has_flag = |wanted, name| has_system_flag(flags, wanted, name);
        let timestamp = mailparse::dateparse(&date).unwrap_or(0);
        MessageRow {
            account: None,
            uid,
            folder: self.include_folder.then(|| self.folder.to_string()),
            from,
            subject,
            date,
            timestamp,
            size: fetch.size.unwrap_or(0),
            message_id,
            in_reply_to,
            references,
            seen: has_flag(Flag::Seen, "\\Seen"),
            answered: has_flag(Flag::Answered, "\\Answered"),
            flagged: has_flag(Flag::Flagged, "\\Flagged"),
            uid_validity: self.uid_validity,
            gmail_msgid: None,
            arrival: fetch.internal_date().map(|date| date.timestamp()),
        }
    }
}

/// IMAP flag names are case-insensitive, but the imap crate only maps the
/// canonical spellings to system variants; others arrive as `Flag::Custom`.
fn has_system_flag(flags: &[Flag<'_>], wanted: Flag<'_>, name: &str) -> bool {
    flags.iter().any(|flag| {
        *flag == wanted || matches!(flag, Flag::Custom(custom) if custom.eq_ignore_ascii_case(name))
    })
}

/// Extract every angle-bracketed message ID from a Message-ID, In-Reply-To,
/// or References header value, keeping the brackets. Brackets inside
/// comments `(...)` or quoted strings outside an ID are ignored, as are
/// commas, legacy phrases, and malformed tokens.
pub fn message_ids(value: &str) -> Vec<String> {
    let mut ids = Vec::new();
    let mut comment_depth = 0usize;
    let mut in_quote = false;
    let mut escaped = false;
    // Byte offset just after the current '<', and whether the ID so far is valid.
    let mut open: Option<(usize, bool)> = None;

    for (index, character) in value.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if character == '\\' && (in_quote || comment_depth > 0) {
            escaped = true;
            continue;
        }
        if comment_depth > 0 {
            match character {
                '(' => comment_depth += 1,
                ')' => comment_depth -= 1,
                _ => {}
            }
            continue;
        }
        if character == '"' {
            in_quote = !in_quote;
            continue;
        }
        if in_quote {
            if character.is_control() {
                if let Some((_, valid)) = open.as_mut() {
                    *valid = false;
                }
            }
            continue;
        }
        match (character, open.as_mut()) {
            ('<', _) => open = Some((index + 1, true)),
            ('>', Some((start, valid))) => {
                let inner = &value[*start..index];
                if *valid && !inner.is_empty() {
                    ids.push(format!("<{inner}>"));
                }
                open = None;
            }
            ('(', None) => comment_depth = 1,
            (c, Some((_, valid))) if c.is_whitespace() || c.is_control() => *valid = false,
            _ => {}
        }
    }
    ids
}

/// Trash, spam, and aggregate mailboxes `--all-folders` never searches:
/// special-use `\All`, `\Trash`, and `\Junk` mailboxes, and well-known
/// names (exact, case-insensitive) for servers without special-use
/// attributes. On Proton Mail Bridge, also the label views: the `Labels`
/// container and `\Flagged` (`Starred`), since every message there also
/// sits in exactly one regular folder.
fn is_excluded_special(mailbox: &MailboxListing, proton_bridge: bool) -> bool {
    if ["\\All", "\\Trash", "\\Junk"]
        .iter()
        .any(|special| mailbox.has_attribute(special))
    {
        return true;
    }
    if proton_bridge && (mailbox.name == "Labels" || mailbox.has_attribute("\\Flagged")) {
        return true;
    }
    let lower = mailbox.name.to_lowercase();
    matches!(
        lower.as_str(),
        "all mail"
            | "[gmail]/all mail"
            | "trash"
            | "spam"
            | "junk"
            | "[gmail]/spam"
            | "[gmail]/trash"
    )
}

/// The listed names `--all-folders` searches: selectable mailboxes that are
/// neither excluded special mailboxes nor nested in one (`Trash/Old`).
/// Children of other containers that cannot be opened, such as Gmail's
/// `[Gmail]`, are searched.
fn searchable(folders: &[MailboxListing], proton_bridge: bool) -> Vec<String> {
    let excluded: Vec<&MailboxListing> = folders
        .iter()
        .filter(|mailbox| is_excluded_special(mailbox, proton_bridge))
        .collect();
    folders
        .iter()
        .filter(|mailbox| {
            mailbox.is_selectable()
                && !is_excluded_special(mailbox, proton_bridge)
                && !excluded.iter().any(|parent| parent.contains(mailbox))
        })
        .map(|mailbox| mailbox.name.clone())
        .collect()
}

/// Whether two mailbox names refer to the same mailbox: exact comparison,
/// except INBOX, which IMAP defines as case-insensitive.
pub fn same_mailbox(a: &str, b: &str) -> bool {
    a == b || (a.eq_ignore_ascii_case("INBOX") && b.eq_ignore_ascii_case("INBOX"))
}

/// List every mailbox name included in `--all-folders` operations.
pub fn searchable_folders(session: &mut ImapSession) -> Result<Vec<String>> {
    let folders = session.list_all().context("Failed to list folders")?;
    Ok(searchable(&folders, session.is_proton_bridge()))
}

/// Search per `criteria`. A single source folder is first resolved to the
/// server's listed name (see [`resolve_folder`]) and `criteria.folder` is
/// updated, so later UID actions use the same mailbox.
pub fn search(session: &mut ImapSession, criteria: &mut SearchCriteria) -> Result<Vec<MessageRow>> {
    search_excluding(session, criteria, None)
}

/// Search for messages to move to `exclude` (a listed name): with
/// `--all-folders` that mailbox is not searched, and naming it as the single
/// source is an error.
pub fn search_excluding(
    session: &mut ImapSession,
    criteria: &mut SearchCriteria,
    exclude: Option<&str>,
) -> Result<Vec<MessageRow>> {
    let query = build_query(criteria)?;
    ensure_query_supported(session, &query)?;

    if criteria.all_folders {
        // INBOX first: of a Gmail message's label copies the first one found
        // is kept, and the INBOX copy is the natural one to show and act on.
        let mut folder_names = searchable_folders(session)?;
        folder_names.sort_by_key(|name| !same_mailbox(name, "INBOX"));

        // Every folder is cut to `limit` in sort-time order, the same key as
        // the merge below (stable, so folder order breaks ties), so the cut
        // rows include every row of the overall newest `limit`, even after
        // Gmail label duplicates are dropped.
        let mut all_messages = Vec::new();
        for folder in &folder_names {
            if exclude.is_some_and(|excluded| same_mailbox(folder, excluded)) {
                continue;
            }
            match fetch_messages(session, folder, &query, true, false, criteria.limit) {
                Ok(msgs) => all_messages.extend(msgs),
                Err(e) => {
                    eprintln!(
                        "Warning: skipping folder '{}': {}",
                        sanitize_folder_name(folder),
                        sanitize_terminal_field(&format!("{e:#}"))
                    );
                }
            }
        }
        let mut all_messages = drop_gmail_label_duplicates(all_messages);
        all_messages.sort_by_key(|message| std::cmp::Reverse(message.sort_time()));
        if let Some(n) = criteria.limit {
            all_messages.truncate(n);
        }
        Ok(all_messages)
    } else {
        criteria.folder = resolve_folder(session, &criteria.folder)?;
        if exclude.is_some_and(|excluded| same_mailbox(&criteria.folder, excluded)) {
            bail!(
                "Source and destination folder are the same ('{}')",
                sanitize_folder_name(&criteria.folder)
            );
        }
        fetch_messages(
            session,
            &criteria.folder,
            &query,
            false,
            !criteria.client_order,
            criteria.limit,
        )
    }
}

/// Gmail lists a message once per label folder. Keep the first row per
/// Gmail message ID (INBOX is searched first). Rows without an ID (other
/// servers) are all kept.
fn drop_gmail_label_duplicates(messages: Vec<MessageRow>) -> Vec<MessageRow> {
    let mut seen = HashSet::new();
    messages
        .into_iter()
        .filter(|message| message.gmail_msgid.is_none_or(|id| seen.insert(id)))
        .collect()
}

/// The server's listed name for a user-supplied folder, or `None` if it is
/// not listed. Accepts the listed name itself or its decoded form
/// ([`MailboxListing::find`]). The LIST pattern is the constant `*` so user
/// input never acts as a wildcard pattern.
pub fn lookup_folder(session: &mut ImapSession, requested: &str) -> Result<Option<String>> {
    validate_folder_name(requested)?;
    let folders = session.list_all().context("Failed to list folders")?;
    Ok(MailboxListing::find(&folders, requested).map(|mailbox| mailbox.name.clone()))
}

/// Like [`lookup_folder`], but a folder that is not listed is an error.
pub fn resolve_folder(session: &mut ImapSession, requested: &str) -> Result<String> {
    lookup_folder(session, requested)?.ok_or_else(|| missing_folder(requested))
}

/// The error for a user-supplied folder that is not listed. The name is
/// shown as typed, not decoded.
pub fn missing_folder(requested: &str) -> anyhow::Error {
    anyhow::anyhow!(
        "Folder '{}' does not exist. Use `slashmail status` to list available folders.",
        sanitize_terminal_field(requested)
    )
}

/// Group searched rows by source mailbox with the UIDVALIDITY captured at
/// search time. Every row of a group must carry the same nonzero identity.
pub fn group_message_uids<'a>(
    messages: &'a [MessageRow],
    default_folder: &'a str,
) -> Result<BTreeMap<&'a str, (u32, Vec<u32>)>> {
    let mut groups: BTreeMap<&'a str, (u32, Vec<u32>)> = BTreeMap::new();
    for message in messages {
        let folder = message.folder.as_deref().unwrap_or(default_folder);
        let uid_validity = match message.uid_validity {
            Some(value) if value != 0 => value,
            _ => bail!(
                "Missing UIDVALIDITY for searched mailbox '{}'",
                sanitize_folder_name(folder)
            ),
        };
        let (expected, uids) = groups
            .entry(folder)
            .or_insert_with(|| (uid_validity, Vec::new()));
        if *expected != uid_validity {
            bail!(
                "Searched messages from '{}' have inconsistent UIDVALIDITY",
                sanitize_folder_name(folder)
            );
        }
        uids.push(message.uid);
    }
    Ok(groups)
}

/// SELECT `folder` read-write and require the UIDVALIDITY recorded when it
/// was searched, so UID mutations never act on a different mailbox incarnation.
pub fn select_verified(
    session: &mut ImapSession,
    folder: &str,
    expected_uid_validity: u32,
) -> Result<()> {
    open_verified(session, folder, expected_uid_validity, true)
}

/// EXAMINE `folder` read-only with the same UIDVALIDITY check, for body
/// fetches that must not change mailbox state.
pub fn examine_verified(
    session: &mut ImapSession,
    folder: &str,
    expected_uid_validity: u32,
) -> Result<()> {
    open_verified(session, folder, expected_uid_validity, false)
}

fn open_verified(
    session: &mut ImapSession,
    folder: &str,
    expected_uid_validity: u32,
    writable: bool,
) -> Result<()> {
    validate_folder_name(folder)?;
    let safe_folder = sanitize_folder_name(folder);
    let opened = if writable {
        session.select(folder)
    } else {
        session.examine(folder)
    };
    let opened = opened.with_context(|| {
        let verb = if writable { "select" } else { "examine" };
        format!("Failed to {verb} '{safe_folder}'")
    })?;
    if opened.uid_validity != Some(expected_uid_validity) {
        bail!("Mailbox UIDVALIDITY changed; search again before acting ('{safe_folder}')");
    }
    Ok(())
}

/// The UIDs of `uid_set` that still exist in the selected mailbox. UID
/// commands silently ignore missing UIDs, so actions run on and report only
/// these.
pub fn existing_uids(session: &mut ImapSession, uid_set: &str) -> Result<Vec<u32>> {
    let mut uids: Vec<u32> = session
        .uid_search(&format!("UID {uid_set}"))
        .context("IMAP SEARCH failed")?
        .into_iter()
        .collect();
    uids.sort_unstable();
    Ok(uids)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literal_lengths_measure_only_literal_prefixes() {
        let query = format!(
            "SUBJECT {} FROM {} TO {}",
            quote_search_term("a {9999+}"),
            quote_search_term("Zoë {9999+}"),
            quote_search_term("é {5000+}")
        );
        assert_eq!(literal_lengths(&query).collect::<Vec<_>>(), [12, 10]);
    }

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
    fn parse_date_relative_returns_valid_imap_date() {
        // We can't assert exact dates since they depend on "now",
        // but we can verify the format is valid IMAP date (D-Mon-YYYY)
        let re = Regex::new(r"^\d{1,2}-(Jan|Feb|Mar|Apr|May|Jun|Jul|Aug|Sep|Oct|Nov|Dec)-\d{4}$")
            .unwrap();
        for input in ["7d", "14d", "2w", "1m", "3m", "6m", "1y", "2y"] {
            let result = parse_date(input).unwrap();
            assert!(
                re.is_match(&result),
                "'{input}' produced invalid IMAP date: '{result}'"
            );
        }
    }

    #[test]
    fn parse_date_relative_zero_days_is_today() {
        // 0d should produce today's date
        let result = parse_date("0d").unwrap();
        // Verify against epoch_to_date(now)
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let (y, m, d) = epoch_to_date(now);
        let expected = format_imap_date(d, m, y).unwrap();
        assert_eq!(result, expected);
    }

    #[test]
    fn epoch_to_date_known_values() {
        // 2025-01-01 00:00:00 UTC = 1735689600
        assert_eq!(epoch_to_date(1_735_689_600), (2025, 1, 1));
        // Unix epoch
        assert_eq!(epoch_to_date(0), (1970, 1, 1));
        // 2000-02-29 (leap year)
        assert_eq!(epoch_to_date(951_782_400), (2000, 2, 29));
    }

    #[test]
    fn days_in_month_leap_year() {
        assert_eq!(days_in_month(2024, 2), 29);
        assert_eq!(days_in_month(2023, 2), 28);
        assert_eq!(days_in_month(2000, 2), 29); // century leap
        assert_eq!(days_in_month(1900, 2), 28); // century non-leap
    }

    #[test]
    fn parse_date_relative_month_clamps_day() {
        // If today is Jan 31 and we subtract 1 month, we get Dec 31 (not "Feb 31")
        // We can't control "today" in tests, but we can verify it doesn't error
        assert!(parse_date("1m").is_ok());
        assert!(parse_date("12m").is_ok());
        assert!(parse_date("24m").is_ok());
    }

    #[test]
    fn parse_date_relative_large_values() {
        assert!(parse_date("365d").is_ok());
        assert!(parse_date("52w").is_ok());
        assert!(parse_date("120m").is_ok());
        assert!(parse_date("50y").is_ok());
    }

    #[test]
    fn parse_date_relative_rejects_invalid_unit() {
        assert!(parse_date("7x").is_err());
        assert!(parse_date("3h").is_err());
        assert!(parse_date("d").is_err());
        assert!(parse_date("7").is_err());
    }

    #[test]
    fn parse_size_plain_bytes() {
        assert_eq!(parse_size("1024").unwrap(), 1024);
        assert_eq!(parse_size("0").unwrap(), 0);
    }

    #[test]
    fn parse_size_kilobytes() {
        assert_eq!(parse_size("1K").unwrap(), 1024);
        assert_eq!(parse_size("1k").unwrap(), 1024);
        assert_eq!(parse_size("10K").unwrap(), 10240);
    }

    #[test]
    fn parse_size_megabytes() {
        assert_eq!(parse_size("1M").unwrap(), 1_048_576);
        assert_eq!(parse_size("1m").unwrap(), 1_048_576);
        assert_eq!(parse_size("5M").unwrap(), 5_242_880);
    }

    #[test]
    fn parse_size_invalid_errors() {
        assert!(parse_size("abc").is_err());
        assert!(parse_size("").is_err());
    }

    #[test]
    fn parse_size_overflow_errors() {
        assert!(parse_size("18446744073709551615M").is_err());
    }

    #[test]
    fn build_query_no_criteria_returns_all() {
        let c = default_test_criteria();
        assert_eq!(build_query(&c).unwrap(), "ALL");
    }

    #[test]
    fn build_query_subject_only() {
        let mut c = default_test_criteria();
        c.subject = Some("test".into());
        assert_eq!(build_query(&c).unwrap(), "SUBJECT \"test\"");
    }

    #[test]
    fn build_query_combined_fields() {
        let mut c = default_test_criteria();
        c.subject = Some("invoice".into());
        c.from = Some("user@example.com".into());
        assert_eq!(
            build_query(&c).unwrap(),
            "SUBJECT \"invoice\" FROM \"user@example.com\""
        );
    }

    #[test]
    fn build_query_to_and_cc() {
        let mut c = default_test_criteria();
        c.to = Some("alice@example.com".into());
        c.cc = Some("bob@example.com".into());
        assert_eq!(
            build_query(&c).unwrap(),
            "TO \"alice@example.com\" CC \"bob@example.com\""
        );
    }

    #[test]
    fn build_query_seen() {
        let mut c = default_test_criteria();
        c.seen = true;
        assert_eq!(build_query(&c).unwrap(), "SEEN");
    }

    #[test]
    fn build_query_unseen_with_from() {
        let mut c = default_test_criteria();
        c.from = Some("alice@example.com".into());
        c.unseen = true;
        assert_eq!(
            build_query(&c).unwrap(),
            "FROM \"alice@example.com\" UNSEEN"
        );
    }

    #[test]
    fn build_query_date_range() {
        let mut c = default_test_criteria();
        c.since = Some("2025-01-01".into());
        c.before = Some("2025-12-31".into());
        assert_eq!(
            build_query(&c).unwrap(),
            "SINCE 1-Jan-2025 BEFORE 31-Dec-2025"
        );
    }

    #[test]
    fn build_query_size_filter() {
        let mut c = default_test_criteria();
        c.larger = Some("1M".into());
        assert_eq!(build_query(&c).unwrap(), "LARGER 1048576");
    }

    #[test]
    fn build_query_invalid_date_errors() {
        let mut c = default_test_criteria();
        c.since = Some("not-a-date".into());
        assert!(build_query(&c).is_err());
    }

    #[test]
    fn build_query_invalid_size_errors() {
        let mut c = default_test_criteria();
        c.larger = Some("abc".into());
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
        let data = b"A001 NO SORT not supported\r\n";
        assert!(parse_sort_response(data).is_err());
    }

    #[test]
    fn parse_sort_response_ok_with_bad_substring() {
        // "BADCHARSET" contains "BAD" but the status token is "OK"
        let data = b"* SORT 1 2 3\r\nA001 OK [BADCHARSET] Completed\r\n";
        let uids = parse_sort_response(data).unwrap();
        assert_eq!(uids, vec![1, 2, 3]);
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
        // Generate enough UIDs to exceed MAX_UID_SET_LENGTH chars.
        let uids: Vec<u32> = (0..2000).map(|i| i * 3).collect();
        let chunks = build_uid_set(&uids);
        assert!(chunks.len() > 1);
        for chunk in &chunks {
            assert!(chunk.len() <= MAX_UID_SET_LENGTH);
        }
    }

    #[test]
    fn build_query_body_only() {
        let mut c = default_test_criteria();
        c.body = Some("invoice".into());
        assert_eq!(build_query(&c).unwrap(), "BODY \"invoice\"");
    }

    #[test]
    fn build_query_text_only() {
        let mut c = default_test_criteria();
        c.text = Some("meeting".into());
        assert_eq!(build_query(&c).unwrap(), "TEXT \"meeting\"");
    }

    #[test]
    fn build_query_body_with_subject() {
        let mut c = default_test_criteria();
        c.subject = Some("report".into());
        c.body = Some("quarterly".into());
        assert_eq!(
            build_query(&c).unwrap(),
            "SUBJECT \"report\" BODY \"quarterly\""
        );
    }

    #[test]
    fn build_query_uid_narrows_other_filters() {
        let mut c = default_test_criteria();
        c.uid = Some(42);
        assert_eq!(build_query(&c).unwrap(), "UID 42");
        c.subject = Some("report".into());
        assert_eq!(build_query(&c).unwrap(), "UID 42 SUBJECT \"report\"");
    }

    #[test]
    fn non_ascii_terms_become_byte_counted_literals() {
        let mut c = default_test_criteria();
        c.subject = Some("café".into());
        assert_eq!(build_query(&c).unwrap(), "SUBJECT {5+}\r\ncafé");

        c.from = Some("Zoë \"Q\"\r\n".into());
        c.to = Some("a\"b\\c".into());
        c.unseen = true;
        assert_eq!(
            build_query(&c).unwrap(),
            "SUBJECT {5+}\r\ncafé FROM {8+}\r\nZoë \"Q\" TO \"a\\\"b\\\\c\" UNSEEN"
        );
    }

    #[test]
    fn folder_names_are_validated_not_rewritten() {
        for valid in ["INBOX", "Projects \"Q1\" *%", "Back\\slash", "Ärger"] {
            assert!(validate_folder_name(valid).is_ok(), "{valid:?}");
        }
        for invalid in ["", "Bad\rName", "Bad\nName", "Bad\0Name", "Bad\u{85}Name"] {
            assert!(validate_folder_name(invalid).is_err(), "{invalid:?}");
        }
    }

    fn searched_row(folder: Option<&str>, uid: u32, uid_validity: Option<u32>) -> MessageRow {
        MessageRow {
            account: None,
            uid,
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
            uid_validity,
            gmail_msgid: None,
            arrival: None,
        }
    }

    #[test]
    fn grouping_binds_uids_to_their_searched_mailbox_identity() {
        assert!(group_message_uids(&[], "INBOX").unwrap().is_empty());

        let rows = [
            searched_row(None, 3, Some(10)),
            searched_row(Some("Archive"), 3, Some(20)),
            searched_row(None, 5, Some(10)),
        ];
        let groups = group_message_uids(&rows, "INBOX").unwrap();
        assert_eq!(groups["INBOX"], (10, vec![3, 5]));
        assert_eq!(groups["Archive"], (20, vec![3]));

        for identity in [None, Some(0)] {
            let rows = [searched_row(None, 1, identity)];
            let error = group_message_uids(&rows, "INBOX").unwrap_err();
            assert!(error
                .to_string()
                .contains("Missing UIDVALIDITY for searched mailbox"));
        }

        let inconsistent = [
            searched_row(None, 1, Some(10)),
            searched_row(None, 2, Some(11)),
        ];
        assert!(group_message_uids(&inconsistent, "INBOX").is_err());
    }

    #[test]
    fn system_flags_match_case_insensitively() {
        let flags = [Flag::Custom("\\SEEN".into()), Flag::Answered];
        assert!(has_system_flag(&flags, Flag::Seen, "\\Seen"));
        assert!(has_system_flag(&flags, Flag::Answered, "\\Answered"));
        assert!(!has_system_flag(&flags, Flag::Flagged, "\\Flagged"));
    }

    #[test]
    fn message_ids_extracts_bracketed_ids_and_skips_noise() {
        assert_eq!(
            message_ids("<root@example.com> <mid@example.com>"),
            ["<root@example.com>", "<mid@example.com>"]
        );
        assert_eq!(
            message_ids("<a@x>,<b@y>  (Alice's message of Monday)"),
            ["<a@x>", "<b@y>"]
        );
        assert_eq!(
            message_ids("junk <broken <real@example.com> <> <sp ace@x> <tail"),
            ["<real@example.com>"]
        );
        assert!(message_ids("not-a-message-id").is_empty());
        assert_eq!(
            message_ids(r#"<id@x> (from "Bob" <bob@example.com>) "Re: <fake@x>" <next@x>"#),
            ["<id@x>", "<next@x>"]
        );
        assert_eq!(
            message_ids(r#"<"john doe"@example.com> (nested (<no@x>) \) ok) <after@x>"#),
            [r#"<"john doe"@example.com>"#, "<after@x>"]
        );
    }

    /// Helper to build a default SearchCriteria with all fields set to None/false.
    fn default_test_criteria() -> SearchCriteria {
        SearchCriteria {
            folder: "INBOX".into(),
            all_folders: false,
            uid: None,
            subject: None,
            from: None,
            to: None,
            cc: None,
            body: None,
            text: None,
            seen: false,
            unseen: false,
            since: None,
            before: None,
            larger: None,
            smaller: None,
            flagged: false,
            unflagged: false,
            answered: false,
            draft: false,
            limit: None,
            client_order: false,
        }
    }

    #[test]
    fn build_query_smaller_only() {
        let mut c = default_test_criteria();
        c.smaller = Some("1M".into());
        assert_eq!(build_query(&c).unwrap(), "SMALLER 1048576");
    }

    #[test]
    fn build_query_size_range() {
        let mut c = default_test_criteria();
        c.larger = Some("1K".into());
        c.smaller = Some("1M".into());
        assert_eq!(build_query(&c).unwrap(), "LARGER 1024 SMALLER 1048576");
    }

    #[test]
    fn build_query_flagged() {
        let mut c = default_test_criteria();
        c.flagged = true;
        assert_eq!(build_query(&c).unwrap(), "FLAGGED");
    }

    #[test]
    fn build_query_unflagged() {
        let mut c = default_test_criteria();
        c.unflagged = true;
        assert_eq!(build_query(&c).unwrap(), "UNFLAGGED");
    }

    #[test]
    fn build_query_answered() {
        let mut c = default_test_criteria();
        c.answered = true;
        assert_eq!(build_query(&c).unwrap(), "ANSWERED");
    }

    #[test]
    fn build_query_draft() {
        let mut c = default_test_criteria();
        c.draft = true;
        assert_eq!(build_query(&c).unwrap(), "DRAFT");
    }

    #[test]
    fn empty_or_control_only_text_filters_are_rejected() {
        for value in ["", "   ", "\u{1b}", "\t\r\n"] {
            let mut c = default_test_criteria();
            c.from = Some(value.into());
            let error = build_query(&c).unwrap_err();
            assert_eq!(error.to_string(), "--from must not be empty", "{value:?}");
        }
        let mut c = default_test_criteria();
        c.text = Some("\u{0}".into());
        assert_eq!(
            build_query(&c).unwrap_err().to_string(),
            "--text must not be empty"
        );
        c.text = Some(" x ".into());
        assert_eq!(build_query(&c).unwrap(), "TEXT \" x \"");
    }

    #[test]
    fn all_folders_skips_trash_junk_and_aggregates_with_their_children() {
        let listing = |name: &str, attributes: &[&str]| MailboxListing {
            name: name.into(),
            attributes: attributes.iter().map(|a| a.to_string()).collect(),
            delimiter: Some("/".into()),
        };
        let folders = [
            listing("INBOX", &[]),
            listing("Deleted Items", &["\\Trash"]),
            listing("Deleted Items/Old", &[]),
            listing("Deleted Items Archive", &[]),
            listing("Junk Email", &["\\HasNoChildren", "\\junk"]),
            listing("Everything", &["\\All"]),
            listing("[Gmail]", &["\\HasChildren", "\\Noselect"]),
            listing("[Gmail]/Starred", &["\\Flagged"]),
            listing("[Gmail]/Trash", &["\\HasChildren", "\\Trash"]),
            listing("[Gmail]/Trash/MWM", &[]),
            listing("Trash", &[]),
            listing("Trash/2024", &[]),
            listing("Archive", &["\\Archive"]),
            listing("Small mail", &[]),
        ];
        assert_eq!(
            searchable(&folders, false),
            [
                "INBOX",
                "Deleted Items Archive",
                "[Gmail]/Starred",
                "Archive",
                "Small mail"
            ]
        );
    }

    #[test]
    fn mailbox_identity_is_exact_except_inbox() {
        assert!(same_mailbox("inbox", "INBOX"));
        assert!(same_mailbox("Archive", "Archive"));
        assert!(!same_mailbox("archive", "Archive"));
    }
}
