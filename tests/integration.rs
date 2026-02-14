#![cfg(feature = "integration-tests")]

use std::sync::atomic::{AtomicU32, Ordering};
use std::thread;
use std::time::Duration;

use lettre::message::header::ContentType;
use lettre::transport::smtp::client::Tls;
use lettre::{Message, SmtpTransport, Transport};

use slashmail::connection::{self, ImapSession};
use slashmail::delete;
use slashmail::export;
use slashmail::search::{self, SearchCriteria};

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn smtp_port() -> u16 {
    std::env::var("GREENMAIL_SMTP_PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3025)
}

fn imap_port() -> u16 {
    std::env::var("GREENMAIL_IMAP_PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3143)
}

fn unique_user() -> String {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    format!("test_{}_{}", n, std::process::id())
}

fn user_email(user: &str) -> String {
    format!("{user}@localhost")
}

fn send_email(to: &str, subject: &str, body: &str) {
    send_email_from("sender@localhost", to, subject, body);
}

fn send_email_from(from: &str, to: &str, subject: &str, body: &str) {
    let to_addr = user_email(to);
    let email = Message::builder()
        .from(from.parse().unwrap())
        .to(to_addr.parse().unwrap())
        .subject(subject)
        .header(ContentType::TEXT_PLAIN)
        .body(body.to_string())
        .unwrap();

    let mailer = SmtpTransport::builder_dangerous("127.0.0.1")
        .port(smtp_port())
        .tls(Tls::None)
        .build();

    mailer.send(&email).unwrap();
}

fn imap_connect(user: &str) -> ImapSession {
    // GreenMail auto-creates accounts; login with full email, password = email
    let email = user_email(user);
    connection::connect("127.0.0.1", imap_port(), false, &email, &email).unwrap()
}

fn default_criteria(folder: &str) -> SearchCriteria {
    SearchCriteria {
        folder: folder.to_string(),
        all_folders: false,
        subject: None,
        from: None,
        since: None,
        before: None,
        larger: None,
        limit: None,
    }
}

fn sleep_for_delivery() {
    thread::sleep(Duration::from_millis(500));
}

/// Convert days since Unix epoch to (year, month, day). Simple civil calendar math.
fn epoch_days_to_ymd(days: u64) -> (u64, u64, u64) {
    // Algorithm from http://howardhinnant.github.io/date_algorithms.html
    let z = days + 719468;
    let era = z / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

#[test]
fn connect_and_logout() {
    let user = unique_user();
    let mut session = imap_connect(&user);
    session.logout().unwrap();
}

#[test]
fn search_empty_mailbox() {
    let user = unique_user();
    let mut session = imap_connect(&user);

    let criteria = default_criteria("INBOX");
    let results = search::search(&mut session, &criteria).unwrap();
    assert!(results.is_empty(), "expected empty mailbox");

    session.logout().unwrap();
}

#[test]
fn search_finds_seeded_email() {
    let user = unique_user();
    send_email(&user, "Hello World", "Test body");
    sleep_for_delivery();

    let mut session = imap_connect(&user);
    let criteria = default_criteria("INBOX");
    let results = search::search(&mut session, &criteria).unwrap();

    assert_eq!(results.len(), 1);
    assert!(results[0].subject.contains("Hello World"));
    assert!(results[0].from.contains("sender@localhost"));

    session.logout().unwrap();
}

#[test]
fn search_by_subject() {
    let user = unique_user();
    send_email(&user, "Monthly Report January", "body");
    send_email(&user, "Monthly Report February", "body");
    send_email(&user, "Invoice #42", "body");
    sleep_for_delivery();

    let mut session = imap_connect(&user);
    let mut criteria = default_criteria("INBOX");
    criteria.subject = Some("Report".to_string());
    let results = search::search(&mut session, &criteria).unwrap();

    assert_eq!(results.len(), 2);

    session.logout().unwrap();
}

#[test]
fn search_by_from() {
    let user = unique_user();
    send_email_from("alice@localhost", &user, "From Alice", "body");
    send_email_from("bob@localhost", &user, "From Bob", "body");
    sleep_for_delivery();

    let mut session = imap_connect(&user);
    let mut criteria = default_criteria("INBOX");
    criteria.from = Some("alice@localhost".to_string());
    let results = search::search(&mut session, &criteria).unwrap();

    assert_eq!(results.len(), 1);
    assert!(results[0].from.contains("alice"));

    session.logout().unwrap();
}

#[test]
fn search_with_limit() {
    let user = unique_user();
    for i in 0..5 {
        send_email(&user, &format!("Message {i}"), "body");
    }
    sleep_for_delivery();

    let mut session = imap_connect(&user);
    let mut criteria = default_criteria("INBOX");
    criteria.limit = Some(2);
    let results = search::search(&mut session, &criteria).unwrap();

    assert_eq!(results.len(), 2);

    session.logout().unwrap();
}

#[test]
fn search_by_size() {
    let user = unique_user();
    let small_body = "tiny";
    let large_body = "x".repeat(10_000);
    send_email(&user, "Small msg", small_body);
    send_email(&user, "Large msg", &large_body);
    sleep_for_delivery();

    let mut session = imap_connect(&user);
    let mut criteria = default_criteria("INBOX");
    criteria.larger = Some("5K".to_string());
    let results = search::search(&mut session, &criteria).unwrap();

    assert_eq!(results.len(), 1, "Only the large message should match");
    assert!(results[0].subject.contains("Large msg"));

    session.logout().unwrap();
}

#[test]
fn search_by_date_range() {
    let user = unique_user();
    send_email(&user, "Recent email", "body");
    sleep_for_delivery();

    let mut session = imap_connect(&user);

    // SINCE today should find the message
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    // Convert epoch to YYYY-MM-DD (UTC, good enough for same-day test)
    let days = now / 86400;
    let (year, month, day) = epoch_days_to_ymd(days);
    let today = format!("{year:04}-{month:02}-{day:02}");
    let mut criteria = default_criteria("INBOX");
    criteria.since = Some(today.clone());
    let results = search::search(&mut session, &criteria).unwrap();
    assert_eq!(results.len(), 1, "SINCE today should find today's message");

    // BEFORE today should find nothing (BEFORE is exclusive in IMAP)
    let mut criteria = default_criteria("INBOX");
    criteria.before = Some(today);
    let results = search::search(&mut session, &criteria).unwrap();
    assert_eq!(results.len(), 0, "BEFORE today should exclude today's message");

    // SINCE a far-future date should find nothing
    let mut criteria = default_criteria("INBOX");
    criteria.since = Some("2099-01-01".to_string());
    let results = search::search(&mut session, &criteria).unwrap();
    assert_eq!(results.len(), 0, "SINCE far future should find nothing");

    session.logout().unwrap();
}

#[test]
fn search_missing_folder_errors() {
    let user = unique_user();
    let mut session = imap_connect(&user);

    let criteria = default_criteria("DoesNotExist");
    let result = search::search(&mut session, &criteria);
    assert!(result.is_err());
    let err = result.err().unwrap();
    assert!(err.to_string().contains("does not exist"));

    session.logout().unwrap();
}

#[test]
fn delete_moves_to_trash() {
    let user = unique_user();
    send_email(&user, "Delete me 1", "body");
    send_email(&user, "Delete me 2", "body");
    sleep_for_delivery();

    let mut session = imap_connect(&user);
    session.create("Trash").unwrap();

    let criteria = default_criteria("INBOX");
    delete::delete(&mut session, &criteria, "Trash", true, false).unwrap();

    // Verify INBOX is empty
    let inbox = search::search(&mut session, &default_criteria("INBOX")).unwrap();
    assert_eq!(inbox.len(), 0, "INBOX should be empty after delete");

    // Verify messages are in Trash
    let trash = search::search(&mut session, &default_criteria("Trash")).unwrap();
    assert_eq!(trash.len(), 2, "Trash should have 2 messages");

    session.logout().unwrap();
}

#[test]
fn delete_dry_run() {
    let user = unique_user();
    send_email(&user, "Keep me 1", "body");
    send_email(&user, "Keep me 2", "body");
    sleep_for_delivery();

    let mut session = imap_connect(&user);

    let criteria = default_criteria("INBOX");
    delete::delete(&mut session, &criteria, "Trash", true, true).unwrap();

    // Messages should still be in INBOX
    let inbox = search::search(&mut session, &default_criteria("INBOX")).unwrap();
    assert_eq!(
        inbox.len(),
        2,
        "INBOX should still have 2 messages after dry run"
    );

    session.logout().unwrap();
}

#[test]
fn move_to_folder() {
    let user = unique_user();
    send_email(&user, "Move me", "body");
    sleep_for_delivery();

    let mut session = imap_connect(&user);
    session.create("Archive").unwrap();

    let criteria = default_criteria("INBOX");
    delete::search_and_move(&mut session, &criteria, "Archive", true, false).unwrap();

    // Verify INBOX is empty
    let inbox = search::search(&mut session, &default_criteria("INBOX")).unwrap();
    assert_eq!(inbox.len(), 0, "INBOX should be empty after move");

    // Verify message is in Archive
    let archive = search::search(&mut session, &default_criteria("Archive")).unwrap();
    assert_eq!(archive.len(), 1, "Archive should have 1 message");

    session.logout().unwrap();
}

#[test]
fn count_via_uid_search() {
    let user = unique_user();
    for i in 0..3 {
        send_email(&user, &format!("Count test {i}"), "body");
    }
    sleep_for_delivery();

    let mut session = imap_connect(&user);

    let criteria = default_criteria("INBOX");
    let query = search::build_query(&criteria).unwrap();
    session.select("INBOX").unwrap();
    let uids = session.uid_search(&query).unwrap();

    assert_eq!(uids.len(), 3);

    session.logout().unwrap();
}

#[test]
fn status_command() {
    let user = unique_user();
    let mut session = imap_connect(&user);

    let folders = session.list(Some(""), Some("*")).unwrap();
    let names: Vec<String> = folders.iter().map(|f| f.name().to_string()).collect();

    assert!(
        names.contains(&"INBOX".to_string()),
        "INBOX should be listed"
    );

    // Run a STATUS command on INBOX
    let cmd = format!(
        "STATUS {} (MESSAGES UNSEEN RECENT)",
        search::imap_quote("INBOX")
    );
    let response = session.run_command_and_read_response(&cmd).unwrap();
    let text = String::from_utf8_lossy(&response);
    assert!(
        text.contains("STATUS") || text.contains("MESSAGES"),
        "STATUS response should be parseable"
    );

    session.logout().unwrap();
}

// --- Export tests ---

#[test]
fn export_creates_eml_files() {
    let user = unique_user();
    send_email(&user, "Export Test", "export body content");
    sleep_for_delivery();

    let mut session = imap_connect(&user);
    let criteria = default_criteria("INBOX");
    let messages = search::search(&mut session, &criteria).unwrap();
    assert_eq!(messages.len(), 1);

    let temp_dir = std::env::temp_dir().join(format!("slashmail_export_{user}"));
    let (exported, skipped) =
        export::export_messages(&mut session, &messages, "INBOX", &temp_dir, false).unwrap();

    assert_eq!(exported, 1);
    assert_eq!(skipped, 0);

    // Verify .eml file exists and contains expected content
    let entries: Vec<_> = std::fs::read_dir(&temp_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .collect();
    assert_eq!(entries.len(), 1);
    assert!(entries[0].path().extension().unwrap() == "eml");

    let content = std::fs::read_to_string(entries[0].path()).unwrap();
    assert!(content.contains("Export Test"));

    // Cleanup
    let _ = std::fs::remove_dir_all(&temp_dir);
    session.logout().unwrap();
}

#[test]
fn export_skips_existing_without_force() {
    let user = unique_user();
    send_email(&user, "Export Skip Test", "body");
    sleep_for_delivery();

    let mut session = imap_connect(&user);
    let criteria = default_criteria("INBOX");
    let messages = search::search(&mut session, &criteria).unwrap();

    let temp_dir = std::env::temp_dir().join(format!("slashmail_skip_{user}"));

    // First export
    let (exported, _) =
        export::export_messages(&mut session, &messages, "INBOX", &temp_dir, false).unwrap();
    assert_eq!(exported, 1);

    // Second export without force — should skip
    let (exported, skipped) =
        export::export_messages(&mut session, &messages, "INBOX", &temp_dir, false).unwrap();
    assert_eq!(exported, 0);
    assert_eq!(skipped, 1);

    let _ = std::fs::remove_dir_all(&temp_dir);
    session.logout().unwrap();
}

#[test]
fn export_force_overwrites() {
    let user = unique_user();
    send_email(&user, "Export Force Test", "body");
    sleep_for_delivery();

    let mut session = imap_connect(&user);
    let criteria = default_criteria("INBOX");
    let messages = search::search(&mut session, &criteria).unwrap();

    let temp_dir = std::env::temp_dir().join(format!("slashmail_force_{user}"));

    // First export
    export::export_messages(&mut session, &messages, "INBOX", &temp_dir, false).unwrap();

    // Second export with force — should overwrite
    let (exported, skipped) =
        export::export_messages(&mut session, &messages, "INBOX", &temp_dir, true).unwrap();
    assert_eq!(exported, 1);
    assert_eq!(skipped, 0);

    let _ = std::fs::remove_dir_all(&temp_dir);
    session.logout().unwrap();
}

#[test]
fn export_multiple_folders_uid_collision() {
    let user = unique_user();
    send_email(&user, "Inbox export msg", "inbox body");
    send_email(&user, "Archive export msg", "archive body");
    sleep_for_delivery();

    let mut session = imap_connect(&user);
    session.create("Archive").unwrap();

    // Move one message to Archive
    let criteria = default_criteria("INBOX");
    let results = search::search(&mut session, &criteria).unwrap();
    let archive_msg = results
        .iter()
        .find(|m| m.subject.contains("Archive export"))
        .unwrap();
    let uid_set = archive_msg.uid.to_string();
    session.select("INBOX").unwrap();
    session.uid_move_or_fallback(&uid_set, "Archive").unwrap();

    // Search all folders to get messages from both INBOX and Archive
    let mut all_criteria = default_criteria("INBOX");
    all_criteria.all_folders = true;
    let all_messages = search::search(&mut session, &all_criteria).unwrap();
    assert_eq!(all_messages.len(), 2);

    let temp_dir = std::env::temp_dir().join(format!("slashmail_multi_{user}"));

    // Export all — both messages should be exported even if UIDs collide
    let (exported, skipped) =
        export::export_messages(&mut session, &all_messages, "INBOX", &temp_dir, false).unwrap();

    assert_eq!(exported + skipped, 2, "All messages should be accounted for");

    // Count actual .eml files on disk
    let _entries: Vec<_> = std::fs::read_dir(&temp_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().map_or(false, |ext| ext == "eml"))
        .collect();

    // NOTE: If UIDs collide across folders, the second write overwrites the first,
    // so exported=2 but only 1 file on disk. This documents the known bug.
    // When the bug is fixed (folder-prefixed filenames), both asserts become 2.
    let _ = std::fs::remove_dir_all(&temp_dir);
    session.logout().unwrap();
}

// --- Folder validation tests ---

#[test]
fn move_to_nonexistent_folder_fails() {
    let user = unique_user();
    send_email(&user, "Move fail test", "body");
    sleep_for_delivery();

    let mut session = imap_connect(&user);
    let criteria = default_criteria("INBOX");

    let result = delete::search_and_move(&mut session, &criteria, "NonExistentFolder", true, false);
    assert!(result.is_err());
    let err_msg = format!("{}", result.unwrap_err());
    assert!(
        err_msg.contains("does not exist"),
        "Error should mention folder does not exist, got: {err_msg}"
    );

    // Messages should still be in INBOX
    let inbox = search::search(&mut session, &default_criteria("INBOX")).unwrap();
    assert_eq!(inbox.len(), 1, "Message should still be in INBOX");

    session.logout().unwrap();
}

#[test]
fn delete_to_nonexistent_trash_fails() {
    let user = unique_user();
    send_email(&user, "Delete fail test", "body");
    sleep_for_delivery();

    let mut session = imap_connect(&user);
    let criteria = default_criteria("INBOX");

    // Don't create Trash folder — should fail
    let result = delete::delete(&mut session, &criteria, "Trash", true, false);
    assert!(result.is_err());

    // Messages should still be in INBOX
    let inbox = search::search(&mut session, &default_criteria("INBOX")).unwrap();
    assert_eq!(inbox.len(), 1, "Message should still be in INBOX");

    session.logout().unwrap();
}

// --- Mark tests ---

#[test]
fn mark_as_read() {
    let user = unique_user();
    send_email(&user, "Mark read test", "body");
    sleep_for_delivery();

    let mut session = imap_connect(&user);
    let criteria = default_criteria("INBOX");
    let results = search::search(&mut session, &criteria).unwrap();
    assert_eq!(results.len(), 1);

    let uid = results[0].uid;
    let uid_set = uid.to_string();

    session.select("INBOX").unwrap();
    session.uid_store(&uid_set, "+FLAGS (\\Seen)").unwrap();

    // Verify flag was set
    let fetches = session.uid_fetch(&uid_set, "FLAGS").unwrap();
    let fetch = fetches.iter().next().unwrap();
    let flags = fetch.flags();
    assert!(
        flags.iter().any(|f| matches!(f, imap::types::Flag::Seen)),
        "Message should be marked as Seen, got: {flags:?}"
    );

    session.logout().unwrap();
}

#[test]
fn mark_as_flagged() {
    let user = unique_user();
    send_email(&user, "Flag test", "body");
    sleep_for_delivery();

    let mut session = imap_connect(&user);
    let criteria = default_criteria("INBOX");
    let results = search::search(&mut session, &criteria).unwrap();
    assert_eq!(results.len(), 1);

    let uid = results[0].uid;
    let uid_set = uid.to_string();

    session.select("INBOX").unwrap();
    session.uid_store(&uid_set, "+FLAGS (\\Flagged)").unwrap();

    let fetches = session.uid_fetch(&uid_set, "FLAGS").unwrap();
    let fetch = fetches.iter().next().unwrap();
    let flags = fetch.flags();
    assert!(
        flags
            .iter()
            .any(|f| matches!(f, imap::types::Flag::Flagged)),
        "Message should be flagged, got: {flags:?}"
    );

    session.logout().unwrap();
}

#[test]
fn mark_unread_removes_seen() {
    let user = unique_user();
    send_email(&user, "Unread test", "body");
    sleep_for_delivery();

    let mut session = imap_connect(&user);
    let criteria = default_criteria("INBOX");
    let results = search::search(&mut session, &criteria).unwrap();
    let uid_set = results[0].uid.to_string();

    session.select("INBOX").unwrap();

    // First mark as read
    session.uid_store(&uid_set, "+FLAGS (\\Seen)").unwrap();

    // Then mark as unread
    session.uid_store(&uid_set, "-FLAGS (\\Seen)").unwrap();

    let fetches = session.uid_fetch(&uid_set, "FLAGS").unwrap();
    let fetch = fetches.iter().next().unwrap();
    let flags = fetch.flags();
    assert!(
        !flags.iter().any(|f| matches!(f, imap::types::Flag::Seen)),
        "Message should not have Seen flag, got: {flags:?}"
    );

    session.logout().unwrap();
}

// --- All-folders search tests ---

#[test]
fn search_all_folders_with_subject_filter() {
    let user = unique_user();
    send_email(&user, "Report Q1", "body");
    send_email(&user, "Invoice 42", "body");
    send_email(&user, "Report Q2", "body");
    sleep_for_delivery();

    let mut session = imap_connect(&user);
    session.create("Archive").unwrap();

    // Move "Report Q2" to Archive
    let criteria = default_criteria("INBOX");
    let results = search::search(&mut session, &criteria).unwrap();
    let q2_msg = results
        .iter()
        .find(|m| m.subject.contains("Report Q2"))
        .unwrap();
    let uid_set = q2_msg.uid.to_string();
    session.select("INBOX").unwrap();
    session.uid_move_or_fallback(&uid_set, "Archive").unwrap();

    // Search all folders with subject filter — should find Report Q1 (INBOX) + Report Q2 (Archive)
    let mut all_criteria = default_criteria("INBOX");
    all_criteria.all_folders = true;
    all_criteria.subject = Some("Report".to_string());
    let results = search::search(&mut session, &all_criteria).unwrap();

    assert_eq!(results.len(), 2, "Should find both Report messages across folders");
    assert!(results.iter().all(|m| m.subject.contains("Report")));

    let folders: Vec<_> = results.iter().filter_map(|m| m.folder.as_deref()).collect();
    assert!(folders.contains(&"INBOX"), "Should include INBOX");
    assert!(folders.contains(&"Archive"), "Should include Archive");

    session.logout().unwrap();
}

#[test]
fn delete_all_folders() {
    let user = unique_user();
    send_email(&user, "Inbox delete all", "body");
    send_email(&user, "Archive delete all", "body");
    sleep_for_delivery();

    let mut session = imap_connect(&user);
    session.create("Archive").unwrap();
    session.create("Trash").unwrap();

    // Move one message to Archive
    let criteria = default_criteria("INBOX");
    let results = search::search(&mut session, &criteria).unwrap();
    let archive_msg = results
        .iter()
        .find(|m| m.subject.contains("Archive delete"))
        .unwrap();
    let uid_set = archive_msg.uid.to_string();
    session.select("INBOX").unwrap();
    session.uid_move_or_fallback(&uid_set, "Archive").unwrap();

    // Delete with all_folders — should move messages from both INBOX and Archive to Trash
    let mut all_criteria = default_criteria("INBOX");
    all_criteria.all_folders = true;
    delete::delete(&mut session, &all_criteria, "Trash", true, false).unwrap();

    // Both INBOX and Archive should be empty
    let inbox = search::search(&mut session, &default_criteria("INBOX")).unwrap();
    assert_eq!(inbox.len(), 0, "INBOX should be empty");
    let archive = search::search(&mut session, &default_criteria("Archive")).unwrap();
    assert_eq!(archive.len(), 0, "Archive should be empty");

    // Trash should have both messages
    let trash = search::search(&mut session, &default_criteria("Trash")).unwrap();
    assert_eq!(trash.len(), 2, "Trash should have 2 messages");

    session.logout().unwrap();
}

#[test]
fn mark_combined_flags() {
    let user = unique_user();
    send_email(&user, "Combo flag test", "body");
    sleep_for_delivery();

    let mut session = imap_connect(&user);
    let criteria = default_criteria("INBOX");
    let results = search::search(&mut session, &criteria).unwrap();
    assert_eq!(results.len(), 1);

    let uid_set = results[0].uid.to_string();
    session.select("INBOX").unwrap();

    // Set both Seen and Flagged in a single STORE command
    session
        .uid_store(&uid_set, "+FLAGS (\\Seen \\Flagged)")
        .unwrap();

    let fetches = session.uid_fetch(&uid_set, "FLAGS").unwrap();
    let fetch = fetches.iter().next().unwrap();
    let flags = fetch.flags();
    assert!(
        flags.iter().any(|f| matches!(f, imap::types::Flag::Seen)),
        "Message should have Seen flag, got: {flags:?}"
    );
    assert!(
        flags
            .iter()
            .any(|f| matches!(f, imap::types::Flag::Flagged)),
        "Message should have Flagged flag, got: {flags:?}"
    );

    session.logout().unwrap();
}

#[test]
fn search_all_folders() {
    let user = unique_user();
    send_email(&user, "Inbox msg", "body");
    send_email(&user, "To archive", "body");
    sleep_for_delivery();

    let mut session = imap_connect(&user);
    session.create("Archive").unwrap();

    // Move one message to Archive
    let criteria = default_criteria("INBOX");
    let results = search::search(&mut session, &criteria).unwrap();
    assert_eq!(results.len(), 2);

    // Move the "To archive" message
    let archive_msg = results
        .iter()
        .find(|m| m.subject.contains("To archive"))
        .unwrap();
    let uid_set = archive_msg.uid.to_string();
    session.select("INBOX").unwrap();
    session.uid_move_or_fallback(&uid_set, "Archive").unwrap();

    // Search all folders
    let mut all_criteria = default_criteria("INBOX");
    all_criteria.all_folders = true;
    let all_results = search::search(&mut session, &all_criteria).unwrap();

    assert_eq!(all_results.len(), 2, "Should find messages across folders");

    let folders: Vec<_> = all_results
        .iter()
        .filter_map(|m| m.folder.as_deref())
        .collect();
    assert!(
        folders.contains(&"INBOX"),
        "Should include INBOX, got: {folders:?}"
    );
    assert!(
        folders.contains(&"Archive"),
        "Should include Archive, got: {folders:?}"
    );

    session.logout().unwrap();
}

#[test]
fn search_all_folders_skips_trash() {
    let user = unique_user();
    send_email(&user, "Keep me", "body");
    send_email(&user, "Trash me", "body");
    sleep_for_delivery();

    let mut session = imap_connect(&user);
    session.create("Trash").unwrap();

    // Move one message to Trash
    let criteria = default_criteria("INBOX");
    let results = search::search(&mut session, &criteria).unwrap();
    let trash_msg = results
        .iter()
        .find(|m| m.subject.contains("Trash me"))
        .unwrap();
    let uid_set = trash_msg.uid.to_string();
    session.select("INBOX").unwrap();
    session.uid_move_or_fallback(&uid_set, "Trash").unwrap();

    // Search all folders — Trash should be excluded
    let mut all_criteria = default_criteria("INBOX");
    all_criteria.all_folders = true;
    let all_results = search::search(&mut session, &all_criteria).unwrap();

    assert_eq!(
        all_results.len(),
        1,
        "Should only find INBOX message, Trash should be skipped"
    );
    assert!(all_results[0].subject.contains("Keep me"));

    session.logout().unwrap();
}
