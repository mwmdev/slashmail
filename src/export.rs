use anyhow::{bail, Context, Result};
use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};

use crate::attachment::{create_output_file, write_created_file};
use crate::connection::ImapSession;
use crate::display::{sanitize_folder_name, sanitize_terminal_field, MessageRow};
use crate::search;

/// Longest file name component accepted by common filesystems.
const MAX_FILENAME_BYTES: usize = 255;

/// Reversibly encode a mailbox name for use as a file name component.
///
/// ASCII letters, digits, and `-` are kept; every other UTF-8 byte becomes
/// `%XX` (uppercase hex), including `%` and `_`, so distinct mailboxes never
/// share a file name and the `_` before the UID stays unambiguous. On
/// Windows, lowercase letters are also encoded so names that differ only by
/// case stay distinct on case-insensitive filesystems.
pub fn encode_folder_name(folder: &str) -> String {
    let mut encoded = String::with_capacity(folder.len());
    for byte in folder.bytes() {
        let keep = byte.is_ascii_digit()
            || byte == b'-'
            || byte.is_ascii_uppercase()
            || (byte.is_ascii_lowercase() && !cfg!(windows));
        if keep {
            encoded.push(char::from(byte));
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

fn export_filename(folder: &str, uid: u32) -> Result<String> {
    let filename = format!("{}_{uid}.eml", encode_folder_name(folder));
    if filename.len() > MAX_FILENAME_BYTES {
        bail!(
            "Export file name for UID {uid} in '{}' exceeds {MAX_FILENAME_BYTES} bytes",
            sanitize_folder_name(folder)
        );
    }
    Ok(filename)
}

type MessageKey<'a> = (&'a str, u32);

/// Writes export files while making sure no two planned messages resolve to
/// the same filesystem object (case-insensitive aliases, hard links).
#[derive(Default)]
struct ExportWriter<'a> {
    #[cfg(unix)]
    identities: HashMap<(u64, u64), MessageKey<'a>>,
    #[cfg(not(unix))]
    identities: std::marker::PhantomData<MessageKey<'a>>,
}

#[derive(Debug)]
enum WriteOutcome {
    Exported,
    /// The existing file already holds this message.
    Skipped,
    /// The existing file holds something else (for example a message from an
    /// earlier UIDVALIDITY epoch or another account); it is left untouched.
    Conflict,
}

impl<'a> ExportWriter<'a> {
    /// Associate the entry at `path` (not following symlinks) with `owner`,
    /// failing if it already belongs to a different planned message.
    fn claim_existing(&mut self, path: &Path, owner: MessageKey<'a>) -> Result<()> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            match std::fs::symlink_metadata(path) {
                Ok(metadata) => self.claim((metadata.dev(), metadata.ino()), owner)?,
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(error)
                        .with_context(|| format!("Failed to inspect '{}'", safe_path(path)))
                }
            }
        }
        #[cfg(not(unix))]
        let _ = (path, owner);
        Ok(())
    }

    #[cfg(unix)]
    fn claim(&mut self, identity: (u64, u64), owner: MessageKey<'a>) -> Result<()> {
        match self.identities.get(&identity) {
            Some(existing) if *existing != owner => {
                bail!("Export destinations alias different messages");
            }
            Some(_) => {}
            None => {
                self.identities.insert(identity, owner);
            }
        }
        Ok(())
    }

    fn write(
        &mut self,
        path: &Path,
        owner: MessageKey<'a>,
        bytes: &[u8],
        force: bool,
    ) -> Result<WriteOutcome> {
        if force {
            // Never replace an entry that another message in this run owns.
            self.claim_existing(path, owner)?;
        }
        let file = match create_output_file(path, force) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists && !force => {
                let metadata = std::fs::symlink_metadata(path)
                    .with_context(|| format!("Failed to inspect '{}'", safe_path(path)))?;
                let file_type = metadata.file_type();
                if !file_type.is_file() && !file_type.is_symlink() {
                    bail!(
                        "Export destination is not a regular file: '{}'",
                        safe_path(path)
                    );
                }
                self.claim_existing(path, owner)?;
                // A symlink is never followed, so its content cannot be
                // verified as this message.
                if file_type.is_file() && holds_same_message(path, bytes)? {
                    return Ok(WriteOutcome::Skipped);
                }
                return Ok(WriteOutcome::Conflict);
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("Failed to create '{}'", safe_path(path)));
            }
        };
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let metadata = file
                .metadata()
                .with_context(|| format!("Failed to inspect '{}'", safe_path(path)))?;
            // A fresh inode cannot be owned yet; record it for later aliases.
            self.identities
                .insert((metadata.dev(), metadata.ino()), owner);
        }
        write_created_file(file, path, bytes)
            .with_context(|| format!("Failed to write '{}'", safe_path(path)))?;
        Ok(WriteOutcome::Exported)
    }
}

/// Header bytes read from an existing export to find its Message-ID. Larger
/// header sections are treated as having none, which yields a conflict.
const MAX_EXISTING_HEADER_BYTES: u64 = 1 << 20;

/// Whether the existing export at `path` holds the fetched message: identical
/// bytes, or the same Message-ID (servers may rewrite headers on migration).
/// File names carry only folder and UID, so after a mailbox is recreated a
/// new message can reuse an old message's name.
///
/// The entry is opened without following symlinks or blocking on special
/// files, and at most the fetched size plus the header section is read.
fn holds_same_message(path: &Path, fetched: &[u8]) -> Result<bool> {
    use std::io::{BufRead, Read, Seek};

    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // Open a symlink itself (its metadata is then not a regular file).
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let file = match options.open(path) {
        Ok(file) => file,
        // A symlink swapped in after the entry was inspected is not followed.
        #[cfg(unix)]
        Err(error) if error.raw_os_error() == Some(libc::ELOOP) => return Ok(false),
        Err(error) => {
            return Err(error).with_context(|| format!("Failed to open '{}'", safe_path(path)))
        }
    };
    let read_error = || format!("Failed to read '{}'", safe_path(path));
    let metadata = file.metadata().with_context(read_error)?;
    if !metadata.is_file() {
        return Ok(false);
    }
    let mut reader = io::BufReader::new(file);

    if metadata.len() == fetched.len() as u64 {
        let mut existing = Vec::with_capacity(fetched.len());
        reader
            .by_ref()
            .take(fetched.len() as u64)
            .read_to_end(&mut existing)
            .with_context(read_error)?;
        if existing == fetched {
            return Ok(true);
        }
        reader.rewind().with_context(read_error)?;
    }

    let mut headers = Vec::new();
    let mut limited = reader.take(MAX_EXISTING_HEADER_BYTES);
    let complete = loop {
        let start = headers.len();
        if limited
            .read_until(b'\n', &mut headers)
            .with_context(read_error)?
            == 0
        {
            // End of file inside the cap: a header-only message.
            break limited.limit() > 0;
        }
        if matches!(&headers[start..], b"\r\n" | b"\n") {
            break true;
        }
    };
    if !complete {
        return Ok(false);
    }
    Ok(match (message_id(&headers), message_id(fetched)) {
        (Some(existing), Some(fetched)) => existing == fetched,
        _ => false,
    })
}

fn message_id(raw: &[u8]) -> Option<String> {
    use mailparse::MailHeaderMap;
    let (headers, _) = mailparse::parse_headers(raw).ok()?;
    let value = headers.get_first_value("Message-ID")?;
    search::message_ids(&value).into_iter().next()
}

fn safe_path(path: &Path) -> String {
    sanitize_terminal_field(&path.display().to_string())
}

/// Plan every destination before touching the server or filesystem.
fn plan_paths<'a>(
    groups: &std::collections::BTreeMap<&'a str, (u32, Vec<u32>)>,
    out_dir: &Path,
) -> Result<HashMap<MessageKey<'a>, PathBuf>> {
    let mut planned = HashMap::new();
    #[cfg(windows)]
    let mut folded = HashMap::new();
    for (folder, (_, uids)) in groups {
        for uid in uids {
            let filename = export_filename(folder, *uid)?;
            #[cfg(windows)]
            if let Some(other) = folded.insert(filename.to_lowercase(), (*folder, *uid)) {
                if other != (*folder, *uid) {
                    bail!("Export destinations alias different messages");
                }
            }
            planned.insert((*folder, *uid), out_dir.join(filename));
        }
    }
    Ok(planned)
}

/// Export messages to .eml files. Returns (exported, skipped) counts, where
/// skipped files already hold the same message. Existing files holding a
/// different message are never overwritten without `force`; they are
/// reported as an error after every other message has been exported.
pub fn export_messages(
    session: &mut ImapSession,
    messages: &[MessageRow],
    default_folder: &str,
    out_dir: &Path,
    force: bool,
) -> Result<(usize, usize)> {
    let groups = search::group_message_uids(messages, default_folder)?;
    let planned = plan_paths(&groups, out_dir)?;

    std::fs::create_dir_all(out_dir)
        .with_context(|| format!("Failed to create directory '{}'", safe_path(out_dir)))?;

    let mut writer = ExportWriter::default();
    let mut exported = 0usize;
    let mut skipped = 0usize;
    let mut conflicts = 0usize;

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
                let (Some(uid), Some(body)) = (fetch.uid, fetch.body()) else {
                    continue;
                };
                let Some(path) = planned.get(&(*folder, uid)) else {
                    continue;
                };
                match writer.write(path, (folder, uid), body, force)? {
                    WriteOutcome::Exported => exported += 1,
                    WriteOutcome::Skipped => skipped += 1,
                    WriteOutcome::Conflict => conflicts += 1,
                }
            }
        }
    }

    if conflicts > 0 {
        bail!(
            "{conflicts} existing export file(s) hold a different message and were left \
             unchanged ({exported} exported, {skipped} already exported); use --force to \
             replace them or export to a new directory"
        );
    }
    Ok((exported, skipped))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(not(windows))]
    #[test]
    fn encoded_folder_names_keep_letters_on_unix() {
        assert_eq!(encode_folder_name("Work/Projects"), "Work%2FProjects");
        assert_eq!(encode_folder_name("Work_Projects"), "Work%5FProjects");
        assert_eq!(encode_folder_name("Work%2FProjects"), "Work%252FProjects");
        assert_eq!(encode_folder_name("my-folder"), "my-folder");
        assert_eq!(encode_folder_name("Café"), "Caf%C3%A9");
    }

    #[cfg(windows)]
    #[test]
    fn encoded_folder_names_encode_lowercase_on_windows() {
        assert_eq!(encode_folder_name("Work/P"), "W%6F%72%6B%2FP");
    }

    #[test]
    fn encoded_folder_names_do_not_collide_even_case_insensitively() {
        assert_eq!(encode_folder_name("INBOX"), "INBOX");
        let folders = [
            "Work/Projects",
            "Work_Projects",
            "Work%2FProjects",
            "work/projects",
            "INBOX.Drafts",
            "[Gmail]/All Mail",
        ];
        let encoded: std::collections::HashSet<String> =
            folders.iter().map(|f| encode_folder_name(f)).collect();
        assert_eq!(encoded.len(), folders.len());
        if cfg!(windows) {
            let folded: std::collections::HashSet<String> =
                encoded.iter().map(|e| e.to_lowercase()).collect();
            assert_eq!(folded.len(), folders.len());
        }
    }

    #[test]
    fn encoded_folder_names_round_trip() {
        fn decode(encoded: &str) -> String {
            let bytes = encoded.as_bytes();
            let mut out = Vec::new();
            let mut index = 0;
            while index < bytes.len() {
                if bytes[index] == b'%' {
                    let hex = std::str::from_utf8(&bytes[index + 1..index + 3]).unwrap();
                    out.push(u8::from_str_radix(hex, 16).unwrap());
                    index += 3;
                } else {
                    out.push(bytes[index]);
                    index += 1;
                }
            }
            String::from_utf8(out).unwrap()
        }
        for folder in ["Work/Projects", "Work_Projects", "100%", "Ärger ☃", "a.b-c"] {
            let encoded = encode_folder_name(folder);
            assert!(!encoded.contains('_'));
            assert_eq!(decode(&encoded), folder);
        }
    }

    #[test]
    fn overlong_export_name_is_an_error_not_a_truncation() {
        let folder = "é".repeat(60);
        let error = export_filename(&folder, 1).unwrap_err();
        assert!(error.to_string().contains("exceeds"));
    }

    #[test]
    fn existing_file_is_skipped_only_when_it_holds_the_same_message() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("INBOX_1.eml");
        let mail = b"Message-ID: <a@example.com>\r\nSubject: A\r\n\r\nbody";
        let rewritten = b"X-Migrated: yes\r\nMessage-ID: <a@example.com>\r\nSubject: A\r\n\r\nbody";
        let other = b"Message-ID: <b@example.com>\r\nSubject: B\r\n\r\nother";
        std::fs::write(&path, mail).unwrap();

        for same in [&mail[..], &rewritten[..]] {
            let mut writer = ExportWriter::default();
            assert!(matches!(
                writer.write(&path, ("INBOX", 1), same, false).unwrap(),
                WriteOutcome::Skipped
            ));
        }
        for different in [&other[..], b"no headers"] {
            let mut writer = ExportWriter::default();
            assert!(matches!(
                writer.write(&path, ("INBOX", 1), different, false).unwrap(),
                WriteOutcome::Conflict
            ));
        }
        assert_eq!(std::fs::read(&path).unwrap(), mail);

        let mut writer = ExportWriter::default();
        assert!(matches!(
            writer.write(&path, ("INBOX", 1), other, true).unwrap(),
            WriteOutcome::Exported
        ));
        assert_eq!(std::fs::read(&path).unwrap(), other);
    }

    #[test]
    fn message_id_check_reads_headers_up_to_the_cap_exactly() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("INBOX_1.eml");
        let fetched = b"Message-ID: <a@example.com>\r\n\r\nrewritten body";
        let header_with_padding = |total: usize| {
            let prefix = b"Message-ID: <a@example.com>\r\nX-Pad: ";
            let padding = total - prefix.len() - b"\r\n\r\n".len();
            let mut raw = prefix.to_vec();
            raw.extend(std::iter::repeat_n(b'x', padding));
            raw.extend_from_slice(b"\r\n\r\nold body");
            raw
        };
        let cap = MAX_EXISTING_HEADER_BYTES as usize;

        std::fs::write(&path, header_with_padding(cap)).unwrap();
        assert!(holds_same_message(&path, fetched).unwrap());
        std::fs::write(&path, header_with_padding(cap + 1)).unwrap();
        assert!(!holds_same_message(&path, fetched).unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn new_exports_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("INBOX_1.eml");
        let mut writer = ExportWriter::default();
        writer.write(&path, ("INBOX", 1), b"body", false).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o077, 0, "mode {mode:o} grants group/other access");
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_are_never_followed() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().unwrap();
        let sentinel = directory.path().join("sentinel");
        std::fs::write(&sentinel, b"sentinel").unwrap();
        let dangling_target = directory.path().join("missing-target");

        for (name, target) in [
            ("live_1.eml", &sentinel),
            ("dangling_1.eml", &dangling_target),
        ] {
            let link = directory.path().join(name);
            symlink(target, &link).unwrap();

            let mut writer = ExportWriter::default();
            assert!(matches!(
                writer.write(&link, ("INBOX", 1), b"mail", false).unwrap(),
                WriteOutcome::Conflict
            ));
            assert_eq!(std::fs::read(&sentinel).unwrap(), b"sentinel");
            assert!(!dangling_target.exists());

            let mut writer = ExportWriter::default();
            writer.write(&link, ("INBOX", 1), b"mail", true).unwrap();
            assert!(!std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink());
            assert_eq!(std::fs::read(&link).unwrap(), b"mail");
            assert_eq!(std::fs::read(&sentinel).unwrap(), b"sentinel");
            assert!(!dangling_target.exists());
        }
    }

    #[test]
    fn directories_are_not_replaced() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("INBOX_1.eml");
        std::fs::create_dir(&path).unwrap();
        for force in [false, true] {
            let mut writer = ExportWriter::default();
            assert!(writer.write(&path, ("INBOX", 1), b"mail", force).is_err());
            assert!(path.is_dir());
        }
    }

    #[cfg(unix)]
    #[test]
    fn hard_linked_destinations_for_different_messages_are_rejected() {
        let directory = tempfile::tempdir().unwrap();
        let first = directory.path().join("A_1.eml");
        let second = directory.path().join("B_1.eml");
        std::fs::write(&first, b"original").unwrap();
        std::fs::hard_link(&first, &second).unwrap();

        for force in [false, true] {
            let mut writer = ExportWriter::default();
            writer.write(&first, ("A", 1), b"a", force).unwrap();
            let error = writer.write(&second, ("B", 1), b"b", force).unwrap_err();
            assert!(error
                .to_string()
                .contains("Export destinations alias different messages"));
            std::fs::remove_file(&second).ok();
            std::fs::hard_link(&first, &second).unwrap();
        }
        assert_eq!(
            std::fs::read(&second).unwrap(),
            std::fs::read(&first).unwrap()
        );
    }
}
