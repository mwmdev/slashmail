use crate::draft::{AppendAttempt, DraftMailboxSession, HeaderFetch, MailboxListing};
use crate::search;
use anyhow::{Context, Result};
use imap::Session;
use std::collections::HashSet;
use std::io::{self, ErrorKind};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

pub type PlainSession = Session<TcpStream>;
pub type TlsSession = Session<native_tls::TlsStream<TcpStream>>;

const IMAP_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const IMAP_IO_TIMEOUT: Duration = Duration::from_secs(30);

enum Inner {
    Plain(PlainSession),
    Tls(TlsSession),
}

pub struct ImapSession {
    inner: Inner,
    capabilities: HashSet<String>,
}

impl ImapSession {
    /// List every mailbox (`LIST "" *`). Quoted names are unescaped; names
    /// sent as literals are kept byte-for-byte.
    pub fn list_all(&mut self) -> Result<Vec<MailboxListing>> {
        let data = self.run_command_and_read_response(r#"LIST "" *"#)?;
        parse_list_response(&data)
    }

    pub fn create(&mut self, mailbox: &str) -> imap::error::Result<()> {
        match &mut self.inner {
            Inner::Plain(s) => s.create(mailbox),
            Inner::Tls(s) => s.create(mailbox),
        }
    }

    pub fn select(&mut self, mailbox: &str) -> imap::error::Result<imap::types::Mailbox> {
        match &mut self.inner {
            Inner::Plain(s) => s.select(mailbox),
            Inner::Tls(s) => s.select(mailbox),
        }
    }

    pub fn examine(&mut self, mailbox: &str) -> imap::error::Result<imap::types::Mailbox> {
        match &mut self.inner {
            Inner::Plain(s) => s.examine(mailbox),
            Inner::Tls(s) => s.examine(mailbox),
        }
    }

    /// Run `UID SEARCH`. Queries containing UTF-8 literals declare
    /// `CHARSET UTF-8` and require LITERAL+ so no continuation is needed.
    pub fn uid_search(&mut self, query: &str) -> Result<HashSet<u32>> {
        let command = if query.is_ascii() {
            std::borrow::Cow::Borrowed(query)
        } else {
            if !self.has_capability("LITERAL+") {
                anyhow::bail!(search::LITERAL_PLUS_REQUIRED);
            }
            std::borrow::Cow::Owned(format!("CHARSET UTF-8 {query}"))
        };
        let uids = match &mut self.inner {
            Inner::Plain(s) => s.uid_search(command.as_ref()),
            Inner::Tls(s) => s.uid_search(command.as_ref()),
        }?;
        Ok(uids)
    }

    pub fn uid_fetch(
        &mut self,
        uid_set: &str,
        query: &str,
    ) -> imap::error::Result<imap::types::Fetches> {
        match &mut self.inner {
            Inner::Plain(s) => s.uid_fetch(uid_set, query),
            Inner::Tls(s) => s.uid_fetch(uid_set, query),
        }
    }

    pub fn uid_mv(&mut self, uid_set: &str, dest: &str) -> imap::error::Result<()> {
        match &mut self.inner {
            Inner::Plain(s) => s.uid_mv(uid_set, dest),
            Inner::Tls(s) => s.uid_mv(uid_set, dest),
        }
    }

    /// UID COPY with the destination sent as an escaped quoted string (the
    /// dependency passes this argument through verbatim).
    pub fn uid_copy(&mut self, uid_set: &str, dest: &str) -> Result<()> {
        let mailbox = format!("\"{}\"", escape_mailbox_name(dest)?);
        match &mut self.inner {
            Inner::Plain(s) => s.uid_copy(uid_set, &mailbox)?,
            Inner::Tls(s) => s.uid_copy(uid_set, &mailbox)?,
        }
        Ok(())
    }

    pub fn uid_store(&mut self, uid_set: &str, query: &str) -> imap::error::Result<()> {
        match &mut self.inner {
            Inner::Plain(s) => {
                s.uid_store(uid_set, query)?;
                Ok(())
            }
            Inner::Tls(s) => {
                s.uid_store(uid_set, query)?;
                Ok(())
            }
        }
    }

    /// Expunge only the given UIDs (RFC 4315 UID EXPUNGE).
    pub fn uid_expunge(&mut self, uid_set: &str) -> imap::error::Result<()> {
        match &mut self.inner {
            Inner::Plain(s) => {
                s.uid_expunge(uid_set)?;
                Ok(())
            }
            Inner::Tls(s) => {
                s.uid_expunge(uid_set)?;
                Ok(())
            }
        }
    }

    /// Message, unseen, and recent counts for one mailbox. Counts the server
    /// omits stay `None`.
    pub fn status_counts(&mut self, mailbox: &str) -> Result<MailboxCounts> {
        let command = format!(
            "STATUS \"{}\" (MESSAGES UNSEEN RECENT)",
            escape_mailbox_name(mailbox)?
        );
        let data = self.run_command_and_read_response(&command)?;
        parse_status_response(&data)
    }

    pub fn get_quota_root(
        &mut self,
        mailbox: &str,
    ) -> imap::error::Result<imap::types::QuotaRootResponse> {
        match &mut self.inner {
            Inner::Plain(s) => s.get_quota_root(mailbox),
            Inner::Tls(s) => s.get_quota_root(mailbox),
        }
    }

    pub fn logout(&mut self) -> imap::error::Result<()> {
        match &mut self.inner {
            Inner::Plain(s) => s.logout(),
            Inner::Tls(s) => s.logout(),
        }
    }

    pub fn has_capability(&self, cap: &str) -> bool {
        self.capabilities.contains(&cap.to_uppercase())
    }

    pub fn run_command_and_read_response(&mut self, command: &str) -> imap::error::Result<Vec<u8>> {
        match &mut self.inner {
            Inner::Plain(s) => s.run_command_and_read_response(command),
            Inner::Tls(s) => s.run_command_and_read_response(command),
        }
    }

    /// Fail unless the server can move messages without touching unrelated
    /// `\Deleted` messages: MOVE, or UIDPLUS for a UID-scoped fallback.
    pub fn ensure_safe_move_supported(&self) -> anyhow::Result<()> {
        if self.has_capability("MOVE") || self.has_capability("UIDPLUS") {
            return Ok(());
        }
        anyhow::bail!("Safe move requires server support for MOVE or UIDPLUS")
    }

    /// Move UIDs to dest. Without MOVE, falls back to UID COPY, STORE
    /// `\Deleted`, and UID EXPUNGE of exactly the same UID set. The fallback is
    /// not atomic: a failure is reported with its phase and never retried.
    pub fn uid_move_or_fallback(&mut self, uid_set: &str, dest: &str) -> anyhow::Result<()> {
        self.ensure_safe_move_supported()?;
        if self.has_capability("MOVE") {
            self.uid_mv(uid_set, dest).context("UID MOVE failed")?;
        } else {
            self.uid_copy(uid_set, dest).context("UID COPY failed")?;
            self.uid_store(uid_set, "+FLAGS (\\Deleted)")
                .context("UID STORE +FLAGS (\\Deleted) failed after COPY")?;
            self.uid_expunge(uid_set)
                .context("UID EXPUNGE failed after COPY and STORE")?;
        }
        Ok(())
    }
}

impl DraftMailboxSession for ImapSession {
    fn list_mailboxes(&mut self) -> Result<Vec<MailboxListing>> {
        self.list_all()
            .map_err(|_| anyhow::anyhow!("Failed to enumerate mailboxes"))
    }

    fn append_draft(&mut self, folder: &str, bytes: &[u8]) -> AppendAttempt {
        let mailbox = match escape_mailbox_name(folder) {
            Ok(mailbox) => mailbox,
            Err(_) => return AppendAttempt::PreLiteralFailure,
        };
        let result = match &mut self.inner {
            Inner::Plain(session) => session
                .append(&mailbox, bytes)
                .flag(imap::types::Flag::Draft)
                .finish(),
            Inner::Tls(session) => session
                .append(&mailbox, bytes)
                .flag(imap::types::Flag::Draft)
                .finish(),
        };

        match result {
            Ok(appended) => match single_append_uid(appended.uids.as_deref()) {
                Ok(uid) => AppendAttempt::Saved { uid },
                Err(()) => AppendAttempt::SavedWithInvalidUidSet,
            },
            Err(imap::error::Error::Append) => AppendAttempt::PreLiteralFailure,
            Err(imap::error::Error::No(_) | imap::error::Error::Bad(_)) => AppendAttempt::Rejected,
            Err(_) => AppendAttempt::Indeterminate,
        }
    }

    fn select_mailbox(&mut self, folder: &str) -> Result<()> {
        self.select(folder)
            .map(|_| ())
            .map_err(|_| anyhow::anyhow!("Failed to select the Drafts destination"))
    }

    fn search_message_id(&mut self, message_id: &str) -> Result<Vec<u32>> {
        let query = format!("HEADER Message-ID {}", search::imap_quote(message_id));
        let mut uids = self
            .uid_search(&query)
            .map_err(|_| anyhow::anyhow!("Failed to search for the saved draft identity"))?
            .into_iter()
            .collect::<Vec<_>>();
        uids.sort_unstable();
        Ok(uids)
    }

    fn fetch_message_id_header(&mut self, uid: u32) -> Result<Vec<HeaderFetch>> {
        let fetches = self
            .uid_fetch(&uid.to_string(), "BODY.PEEK[HEADER.FIELDS (MESSAGE-ID)]")
            .map_err(|_| anyhow::anyhow!("Failed to verify a saved draft identity"))?;
        Ok(fetches
            .iter()
            .map(|fetch| HeaderFetch {
                uid: fetch.uid,
                header: fetch.header().map(<[u8]>::to_vec),
            })
            .collect())
    }
}

fn name_attribute_text(attribute: &imap_proto::NameAttribute<'_>) -> String {
    match attribute {
        imap_proto::NameAttribute::NoInferiors => "\\Noinferiors".to_string(),
        imap_proto::NameAttribute::NoSelect => "\\Noselect".to_string(),
        imap_proto::NameAttribute::Marked => "\\Marked".to_string(),
        imap_proto::NameAttribute::Unmarked => "\\Unmarked".to_string(),
        imap_proto::NameAttribute::All => "\\All".to_string(),
        imap_proto::NameAttribute::Archive => "\\Archive".to_string(),
        imap_proto::NameAttribute::Drafts => "\\Drafts".to_string(),
        imap_proto::NameAttribute::Flagged => "\\Flagged".to_string(),
        imap_proto::NameAttribute::Junk => "\\Junk".to_string(),
        imap_proto::NameAttribute::Sent => "\\Sent".to_string(),
        imap_proto::NameAttribute::Trash => "\\Trash".to_string(),
        imap_proto::NameAttribute::Extension(value) => value.to_string(),
        _ => String::new(),
    }
}

/// Escape a mailbox name for the inside of an IMAP quoted string, rejecting
/// control characters rather than altering the name.
fn escape_mailbox_name(mailbox: &str) -> Result<String> {
    let mut escaped = String::with_capacity(mailbox.len());
    for character in mailbox.chars() {
        match character {
            character if character.is_control() => {
                anyhow::bail!("Mailbox name contains a control character");
            }
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            _ => escaped.push(character),
        }
    }
    Ok(escaped)
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MailboxCounts {
    pub messages: Option<u32>,
    pub unseen: Option<u32>,
    pub recent: Option<u32>,
}

/// Parse untagged responses one at a time, keeping each response's raw bytes.
fn parse_responses<'a>(
    data: &'a [u8],
    mut visit: impl FnMut(&'a [u8], imap_proto::Response<'a>) -> bool,
) -> Result<()> {
    let mut rest = data;
    while !rest.is_empty() {
        let (remaining, response) = imap_proto::parser::parse_response(rest)
            .map_err(|_| anyhow::anyhow!("Failed to parse the server response"))?;
        if !visit(&rest[..rest.len() - remaining.len()], response) {
            break;
        }
        rest = remaining;
    }
    Ok(())
}

fn parse_list_response(data: &[u8]) -> Result<Vec<MailboxListing>> {
    let mut listed = Vec::new();
    parse_responses(data, |raw, response| {
        if let imap_proto::Response::MailboxData(imap_proto::MailboxDatum::List {
            name_attributes,
            name,
            ..
        }) = response
        {
            listed.push(MailboxListing {
                name: listed_mailbox_name(raw, &name),
                attributes: name_attributes.iter().map(name_attribute_text).collect(),
            });
        }
        true
    })?;
    Ok(listed)
}

/// imap-proto returns a quoted mailbox name with its `\\` and `\"` escapes
/// still in place but a literal name verbatim, without recording which form
/// was used. Decide from the raw response line, which ends with the name.
fn listed_mailbox_name(raw: &[u8], name: &str) -> String {
    let line = raw.strip_suffix(b"\r\n").unwrap_or(raw);
    let is_literal = line
        .strip_suffix(name.as_bytes())
        .and_then(|prefix| prefix.strip_suffix(b"}\r\n"))
        .and_then(|prefix| {
            let open = prefix.iter().rposition(|byte| *byte == b'{')?;
            let digits = std::str::from_utf8(&prefix[open + 1..]).ok()?;
            // Literal lengths may be zero-padded (`{04}`).
            let length: usize = digits.parse().ok()?;
            (digits.bytes().all(|byte| byte.is_ascii_digit()) && length == name.len()).then_some(())
        })
        .is_some();
    if is_literal || !line.ends_with(b"\"") {
        return name.to_string();
    }
    let mut decoded = String::with_capacity(name.len());
    let mut characters = name.chars();
    while let Some(character) = characters.next() {
        match character {
            '\\' => decoded.extend(characters.next()),
            character => decoded.push(character),
        }
    }
    decoded
}

/// Take the counts from the STATUS response to the single STATUS command
/// just sent (its echoed name is not compared: it may be escaped).
fn parse_status_response(data: &[u8]) -> Result<MailboxCounts> {
    let mut counts = None;
    parse_responses(data, |_, response| {
        if let imap_proto::Response::MailboxData(imap_proto::MailboxDatum::Status {
            status, ..
        }) = response
        {
            let mut found = MailboxCounts::default();
            for attribute in status {
                match attribute {
                    imap_proto::StatusAttribute::Messages(value) => found.messages = Some(value),
                    imap_proto::StatusAttribute::Unseen(value) => found.unseen = Some(value),
                    imap_proto::StatusAttribute::Recent(value) => found.recent = Some(value),
                    _ => {}
                }
            }
            counts = Some(found);
            return false;
        }
        true
    })?;
    counts.ok_or_else(|| anyhow::anyhow!("Server sent no STATUS response"))
}

fn single_append_uid(uids: Option<&[imap_proto::UidSetMember]>) -> Result<Option<u32>, ()> {
    match uids {
        None => Ok(None),
        Some([imap_proto::UidSetMember::Uid(uid)]) if *uid != 0 => Ok(Some(*uid)),
        Some([imap_proto::UidSetMember::UidRange(range)])
            if range.start() == range.end() && *range.start() != 0 =>
        {
            Ok(Some(*range.start()))
        }
        Some(_) => Err(()),
    }
}

pub fn is_loopback_host(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|address| address.is_loopback())
}

/// Refuse plaintext IMAP to any host that is not loopback. Callers invoke
/// this before prompting for passwords or opening sockets.
pub fn ensure_secure_transport(host: &str, tls: bool) -> Result<()> {
    if tls || is_loopback_host(host) {
        return Ok(());
    }
    anyhow::bail!("IMAP connections to non-loopback hosts require TLS")
}

pub fn connect(host: &str, port: u16, tls: bool, user: &str, pass: &str) -> Result<ImapSession> {
    ensure_secure_transport(host, tls)?;

    let mut session = if tls {
        let tls_connector = native_tls::TlsConnector::builder()
            .min_protocol_version(Some(native_tls::Protocol::Tlsv12))
            .danger_accept_invalid_certs(false)
            .danger_accept_invalid_hostnames(false)
            .build()
            .context("Failed to create TLS connector")?;
        let tcp = connect_tcp(host, port, IMAP_CONNECT_TIMEOUT, IMAP_IO_TIMEOUT)
            .with_context(|| format!("Failed to connect to {host}:{port}"))?;
        let tls = tls_connector
            .connect(host, tcp)
            .with_context(|| format!("Failed to TLS-connect to {host}:{port}"))?;
        let mut client = imap::Client::new(tls);
        client
            .read_greeting()
            .context("Failed to read the IMAP greeting")?;
        let s = client
            .login(user, pass)
            .map_err(|e| e.0)
            .context("IMAP login failed")?;
        Inner::Tls(s)
    } else {
        let tcp = connect_tcp(host, port, IMAP_CONNECT_TIMEOUT, IMAP_IO_TIMEOUT)
            .with_context(|| format!("Failed to connect to {host}:{port}"))?;
        let mut client = imap::Client::new(tcp);
        client
            .read_greeting()
            .context("Failed to read the IMAP greeting")?;
        let s = client
            .login(user, pass)
            .map_err(|e| e.0)
            .context("IMAP login failed")?;
        Inner::Plain(s)
    };

    let caps = match &mut session {
        Inner::Plain(s) => s.capabilities(),
        Inner::Tls(s) => s.capabilities(),
    }
    .context("Failed to fetch capabilities")?;
    let capabilities = ["SORT", "MOVE", "QUOTA", "UIDPLUS", "LITERAL+"]
        .iter()
        .filter(|c| caps.has_str(**c))
        .map(|c| c.to_string())
        .collect();
    drop(caps);

    Ok(ImapSession {
        inner: session,
        capabilities,
    })
}

fn connect_tcp(
    host: &str,
    port: u16,
    connect_timeout: Duration,
    io_timeout: Duration,
) -> io::Result<TcpStream> {
    let addresses = (host, port).to_socket_addrs()?;
    let started = Instant::now();
    let mut last_error = None;

    for address in addresses {
        let remaining = connect_timeout.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            break;
        }

        match TcpStream::connect_timeout(&address, remaining) {
            Ok(stream) => {
                stream.set_read_timeout(Some(io_timeout))?;
                stream.set_write_timeout(Some(io_timeout))?;
                return Ok(stream);
            }
            Err(error) => last_error = Some(error),
        }
    }

    Err(last_error.unwrap_or_else(|| {
        io::Error::new(
            ErrorKind::TimedOut,
            format!("could not connect within {connect_timeout:?}"),
        )
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::thread;

    fn command_tag(command: &str) -> &str {
        command.split_whitespace().next().unwrap()
    }

    fn read_command(reader: &mut BufReader<TcpStream>) -> String {
        let mut command = String::new();
        reader.read_line(&mut command).unwrap();
        command
    }

    fn literal_length(command: &str) -> usize {
        command
            .rsplit_once('{')
            .unwrap()
            .1
            .trim_end()
            .trim_end_matches('}')
            .parse()
            .unwrap()
    }

    fn scripted_session<F>(append_handler: F) -> (ImapSession, thread::JoinHandle<()>)
    where
        F: FnOnce(&mut BufReader<TcpStream>, &mut TcpStream) + Send + 'static,
    {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());

            stream.write_all(b"* OK scripted IMAP server\r\n").unwrap();

            let login = read_command(&mut reader);
            assert!(login.contains(" LOGIN "));
            writeln!(stream, "{} OK logged in\r", command_tag(&login)).unwrap();

            let capability = read_command(&mut reader);
            assert!(capability.contains(" CAPABILITY"));
            write!(
                stream,
                "* CAPABILITY IMAP4rev1 UIDPLUS\r\n{} OK capabilities\r\n",
                command_tag(&capability)
            )
            .unwrap();

            append_handler(&mut reader, &mut stream);
        });

        let session = connect("127.0.0.1", port, false, "user", "password").unwrap();
        (session, server)
    }

    #[test]
    fn is_loopback_ipv4() {
        assert!(is_loopback_host("127.0.0.1"));
        assert!(is_loopback_host("127.42.0.9"));
    }

    #[test]
    fn is_loopback_ipv6() {
        assert!(is_loopback_host("::1"));
    }

    #[test]
    fn is_loopback_localhost() {
        assert!(is_loopback_host("localhost"));
        assert!(is_loopback_host("LOCALHOST"));
    }

    #[test]
    fn is_loopback_remote_host() {
        assert!(!is_loopback_host("example.com"));
    }

    #[test]
    fn append_mailbox_is_escaped_for_the_upstream_builder() {
        assert_eq!(
            escape_mailbox_name("Drafts \"2026\"\\saved").unwrap(),
            "Drafts \\\"2026\\\"\\\\saved"
        );
    }

    #[test]
    fn append_mailbox_rejects_controls() {
        for mailbox in ["Drafts\nInjected", "Drafts\rInjected", "Drafts\0Injected"] {
            assert!(escape_mailbox_name(mailbox).is_err());
        }
    }

    #[test]
    fn appenduid_requires_exactly_one_uid() {
        use imap_proto::UidSetMember::{Uid, UidRange};

        assert_eq!(single_append_uid(None), Ok(None));
        assert_eq!(single_append_uid(Some(&[Uid(42)])), Ok(Some(42)));
        assert_eq!(single_append_uid(Some(&[UidRange(42..=42)])), Ok(Some(42)));
        assert_eq!(single_append_uid(Some(&[Uid(0)])), Err(()));
        assert_eq!(single_append_uid(Some(&[UidRange(0..=0)])), Err(()));
        assert_eq!(single_append_uid(Some(&[UidRange(42..=43)])), Err(()));
        assert_eq!(single_append_uid(Some(&[Uid(42), Uid(43)])), Err(()));
    }

    #[test]
    fn append_rejection_before_literal_is_a_definite_failure() {
        let (mut session, server) = scripted_session(|reader, stream| {
            let append = read_command(reader);
            assert!(append.contains(" APPEND "));
            writeln!(stream, "{} NO rejected\r", command_tag(&append)).unwrap();
        });

        assert_eq!(
            session.append_draft("Drafts", b"body"),
            AppendAttempt::PreLiteralFailure
        );
        server.join().unwrap();
    }

    #[test]
    fn append_disconnect_after_literal_is_indeterminate() {
        let (mut session, server) = scripted_session(|reader, stream| {
            let append = read_command(reader);
            let length = literal_length(&append);
            stream.write_all(b"+ continue\r\n").unwrap();
            let mut literal = vec![0; length + 2];
            reader.read_exact(&mut literal).unwrap();
            assert_eq!(&literal[length..], b"\r\n");
        });

        assert_eq!(
            session.append_draft("Drafts", b"body"),
            AppendAttempt::Indeterminate
        );
        server.join().unwrap();
    }

    #[test]
    fn append_escapes_mailbox_on_the_wire_and_accepts_one_uid() {
        let (mut session, server) = scripted_session(|reader, stream| {
            let append = read_command(reader);
            assert!(append.contains(r#" APPEND "Drafts \"2026\"\\saved" (\Draft) {4}"#));
            let length = literal_length(&append);
            stream.write_all(b"+ continue\r\n").unwrap();
            let mut literal = vec![0; length + 2];
            reader.read_exact(&mut literal).unwrap();
            assert_eq!(&literal, b"body\r\n");
            writeln!(
                stream,
                "{} OK [APPENDUID 1 42] appended\r",
                command_tag(&append)
            )
            .unwrap();
        });

        assert_eq!(
            session.append_draft("Drafts \"2026\"\\saved", b"body"),
            AppendAttempt::Saved { uid: Some(42) }
        );
        server.join().unwrap();
    }

    #[test]
    fn special_use_attributes_keep_drafts_discovery_working() {
        assert_eq!(
            name_attribute_text(&imap_proto::NameAttribute::Drafts),
            "\\Drafts"
        );
        assert_eq!(
            name_attribute_text(&imap_proto::NameAttribute::NoSelect),
            "\\Noselect"
        );
    }

    #[test]
    fn is_loopback_private_ip() {
        assert!(!is_loopback_host("192.168.1.1"));
    }

    #[test]
    fn listed_names_decode_quoted_escapes_but_keep_literals_verbatim() {
        let response = b"* LIST (\\HasNoChildren) \"/\" \"Quote \\\"Q\\\" \\\\ back\"\r\n\
* LIST () \"/\" {10}\r\nWork\\Plans\r\n\
* LIST () \"/\" {5}\r\nEnds\"\r\n\
* LIST () \"/\" {007}\r\nA\\B \\\"\"\r\n\
* LIST (\\All) \"/\" Everything\r\n\
* LIST () \"/\" inbox\r\n";
        let listed = parse_list_response(response).unwrap();
        let names: Vec<&str> = listed.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(
            names,
            [
                r#"Quote "Q" \ back"#,
                r"Work\Plans",
                "Ends\"",
                "A\\B \\\"\"",
                "Everything",
                "INBOX"
            ]
        );
        assert_eq!(listed[4].attributes, ["\\All"]);
    }

    #[test]
    fn status_counts_ignore_the_echoed_name_and_keep_missing_counts_unknown() {
        let escaped = b"* STATUS \"A\\\"B\" (MESSAGES 5 UNSEEN 1 RECENT 0)\r\n";
        assert_eq!(
            parse_status_response(escaped).unwrap(),
            MailboxCounts {
                messages: Some(5),
                unseen: Some(1),
                recent: Some(0)
            }
        );
        let partial = b"* STATUS \"Projects (2024)\" (UNSEEN 2)\r\n";
        assert_eq!(
            parse_status_response(partial).unwrap(),
            MailboxCounts {
                messages: None,
                unseen: Some(2),
                recent: None
            }
        );
        assert!(parse_status_response(b"* STATUS \"B\" (MESSAGES x)\r\n").is_err());
        assert!(parse_status_response(b"").is_err());
    }

    #[test]
    fn plaintext_is_refused_for_remote_hosts_before_resolution() {
        for host in ["mail.example.invalid", "192.168.1.1", "10.0.0.1"] {
            let error = connect(host, 143, false, "user", "password")
                .err()
                .unwrap()
                .to_string();
            assert_eq!(error, "IMAP connections to non-loopback hosts require TLS");
        }
        for host in ["localhost", "127.0.0.1", "::1"] {
            assert!(ensure_secure_transport(host, false).is_ok());
        }
        assert!(ensure_secure_transport("mail.example.invalid", true).is_ok());
    }

    #[test]
    fn configured_tcp_stream_has_read_and_write_deadlines() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let accepting = thread::spawn(move || listener.accept().unwrap());
        let io_timeout = Duration::from_millis(1_234);

        let stream = connect_tcp(
            "127.0.0.1",
            address.port(),
            Duration::from_secs(1),
            io_timeout,
        )
        .unwrap();

        assert_eq!(stream.read_timeout().unwrap(), Some(io_timeout));
        assert_eq!(stream.write_timeout().unwrap(), Some(io_timeout));
        drop(stream);
        accepting.join().unwrap();
    }
}
