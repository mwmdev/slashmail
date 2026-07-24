use crate::draft::{
    AppendAttempt, DraftMailboxSession, HeaderFetch, IndeterminatePhase, MailboxListing,
};
use crate::search;
use anyhow::{Context, Result};
use imap::Session;
use std::collections::HashSet;
use std::net::TcpStream;

pub type PlainSession = Session<TcpStream>;
pub type TlsSession = Session<native_tls::TlsStream<TcpStream>>;

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
    ) -> imap::error::Result<imap::types::ZeroCopy<Vec<imap::types::Name>>> {
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
    ) -> imap::error::Result<imap::types::ZeroCopy<Vec<imap::types::Fetch>>> {
        match &mut self.inner {
            Inner::Plain(s) => s.uid_fetch(uid_set, query),
            Inner::Tls(s) => s.uid_fetch(uid_set, query),
        }
    }

    pub fn append_with_flags_result(
        &mut self,
        mailbox: &str,
        content: &[u8],
        flags: &[imap::types::Flag<'_>],
    ) -> imap::types::AppendResult {
        match &mut self.inner {
            Inner::Plain(s) => s.append_with_flags_result(mailbox, content, flags),
            Inner::Tls(s) => s.append_with_flags_result(mailbox, content, flags),
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
        use imap::types::AppendError;

        match self.append_with_flags_result(folder, bytes, &[imap::types::Flag::Draft]) {
            Ok(completion) => AppendAttempt::Saved {
                uid: completion.uid.map(|identity| identity.uid),
            },
            Err(AppendError::InvalidUidSet { .. }) => AppendAttempt::SavedWithInvalidUidSet,
            Err(AppendError::PreLiteral(_)) => AppendAttempt::PreLiteralFailure,
            Err(AppendError::Rejected { .. }) => AppendAttempt::Rejected,
            Err(AppendError::Indeterminate { phase, .. }) => match phase {
                imap::types::AppendPhase::BeforeLiteral => AppendAttempt::PreLiteralFailure,
                imap::types::AppendPhase::LiteralWrite => AppendAttempt::Indeterminate {
                    phase: IndeterminatePhase::LiteralWrite,
                },
                imap::types::AppendPhase::Completion => AppendAttempt::Indeterminate {
                    phase: IndeterminatePhase::Completion,
                },
            },
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

fn name_attribute_text(attribute: &imap::types::NameAttribute<'_>) -> String {
    match attribute {
        imap::types::NameAttribute::NoInferiors => "\\Noinferiors".to_string(),
        imap::types::NameAttribute::NoSelect => "\\Noselect".to_string(),
        imap::types::NameAttribute::Marked => "\\Marked".to_string(),
        imap::types::NameAttribute::Unmarked => "\\Unmarked".to_string(),
        imap::types::NameAttribute::Custom(value) => value.to_string(),
    }
}

fn is_loopback(host: &str) -> bool {
    host == "127.0.0.1" || host == "::1" || host == "localhost"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_loopback_ipv4() {
        assert!(is_loopback("127.0.0.1"));
    }

    #[test]
    fn is_loopback_ipv6() {
        assert!(is_loopback("::1"));
    }

    #[test]
    fn is_loopback_localhost() {
        assert!(is_loopback("localhost"));
    }

    #[test]
    fn is_loopback_remote_host() {
        assert!(!is_loopback("example.com"));
    }

    #[test]
    fn is_loopback_private_ip() {
        assert!(!is_loopback("192.168.1.1"));
    }
}

pub fn connect(host: &str, port: u16, tls: bool, user: &str, pass: &str) -> Result<ImapSession> {
    if !tls && !is_loopback(host) {
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
        let client = imap::connect((host, port), host, &tls_connector)
            .context(format!("Failed to TLS-connect to {host}:{port}"))?;
        let s = client
            .login(user, pass)
            .map_err(|e| e.0)
            .context("IMAP login failed")?;
        Inner::Tls(s)
    } else {
        let tcp = TcpStream::connect(format!("{host}:{port}"))
            .context(format!("Failed to connect to {host}:{port}"))?;
        let client = imap::Client::new(tcp);
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
