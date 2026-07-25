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
    pub fn list(
        &mut self,
        reference: Option<&str>,
        pattern: Option<&str>,
    ) -> imap::error::Result<imap::types::Names> {
        match &mut self.inner {
            Inner::Plain(s) => s.list(reference, pattern),
            Inner::Tls(s) => s.list(reference, pattern),
        }
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

    pub fn uid_search(
        &mut self,
        query: &str,
    ) -> imap::error::Result<std::collections::HashSet<u32>> {
        match &mut self.inner {
            Inner::Plain(s) => s.uid_search(query),
            Inner::Tls(s) => s.uid_search(query),
        }
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

    pub fn uid_copy(&mut self, uid_set: &str, dest: &str) -> imap::error::Result<()> {
        match &mut self.inner {
            Inner::Plain(s) => {
                s.uid_copy(uid_set, dest)?;
                Ok(())
            }
            Inner::Tls(s) => {
                s.uid_copy(uid_set, dest)?;
                Ok(())
            }
        }
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

    pub fn expunge(&mut self) -> imap::error::Result<()> {
        match &mut self.inner {
            Inner::Plain(s) => {
                s.expunge()?;
                Ok(())
            }
            Inner::Tls(s) => {
                s.expunge()?;
                Ok(())
            }
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

    /// Move UIDs to dest, falling back to COPY+DELETE+EXPUNGE if MOVE is unsupported.
    pub fn uid_move_or_fallback(&mut self, uid_set: &str, dest: &str) -> anyhow::Result<()> {
        if self.has_capability("MOVE") {
            self.uid_mv(uid_set, dest).context("UID MOVE failed")?;
        } else {
            self.uid_copy(uid_set, dest).context("UID COPY failed")?;
            self.uid_store(uid_set, "+FLAGS (\\Deleted)")
                .context("UID STORE +FLAGS failed")?;
            self.expunge().context("EXPUNGE failed")?;
        }
        Ok(())
    }
}

impl DraftMailboxSession for ImapSession {
    fn list_mailboxes(&mut self) -> Result<Vec<MailboxListing>> {
        let listed = self
            .list(Some(""), Some("*"))
            .map_err(|_| anyhow::anyhow!("Failed to enumerate mailboxes"))?;
        Ok(listed
            .iter()
            .map(|mailbox| MailboxListing {
                name: mailbox.name().to_string(),
                attributes: mailbox
                    .attributes()
                    .iter()
                    .map(name_attribute_text)
                    .collect(),
            })
            .collect())
    }

    fn append_draft(&mut self, folder: &str, bytes: &[u8]) -> AppendAttempt {
        let mailbox = match prepare_append_mailbox(folder) {
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

fn prepare_append_mailbox(mailbox: &str) -> Result<String> {
    if mailbox.chars().any(char::is_control) {
        anyhow::bail!("Drafts folder contains a control character");
    }
    Ok(mailbox.replace('\\', "\\\\").replace('"', "\\\""))
}

fn single_append_uid(uids: Option<&[imap_proto::UidSetMember]>) -> Result<Option<u32>, ()> {
    match uids {
        None => Ok(None),
        Some([imap_proto::UidSetMember::Uid(uid)]) => Ok(Some(*uid)),
        Some([imap_proto::UidSetMember::UidRange(range)]) if range.start() == range.end() => {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::thread;

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
            prepare_append_mailbox("Drafts \"2026\"\\saved").unwrap(),
            "Drafts \\\"2026\\\"\\\\saved"
        );
    }

    #[test]
    fn append_mailbox_rejects_controls() {
        for mailbox in ["Drafts\nInjected", "Drafts\rInjected", "Drafts\0Injected"] {
            assert!(prepare_append_mailbox(mailbox).is_err());
        }
    }

    #[test]
    fn appenduid_requires_exactly_one_uid() {
        use imap_proto::UidSetMember::{Uid, UidRange};

        assert_eq!(single_append_uid(None), Ok(None));
        assert_eq!(single_append_uid(Some(&[Uid(42)])), Ok(Some(42)));
        assert_eq!(single_append_uid(Some(&[UidRange(42..=42)])), Ok(Some(42)));
        assert_eq!(single_append_uid(Some(&[UidRange(42..=43)])), Err(()));
        assert_eq!(single_append_uid(Some(&[Uid(42), Uid(43)])), Err(()));
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

pub fn connect(host: &str, port: u16, tls: bool, user: &str, pass: &str) -> Result<ImapSession> {
    if !tls && !is_loopback_host(host) {
        eprintln!(
            "Warning: connecting to {} without TLS. Credentials will be sent in plaintext.",
            host
        );
        eprintln!("         Use --tls for remote servers.");
    }

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
    let capabilities = ["SORT", "MOVE", "QUOTA", "UIDPLUS"]
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
