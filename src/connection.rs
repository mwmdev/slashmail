use anyhow::{Context, Result};
use imap::Session;
use std::net::TcpStream;

pub type PlainSession = Session<TcpStream>;
pub type TlsSession = Session<native_tls::TlsStream<TcpStream>>;

pub enum ImapSession {
    Plain(PlainSession),
    Tls(TlsSession),
}

impl ImapSession {
    pub fn list(
        &mut self,
        reference: Option<&str>,
        pattern: Option<&str>,
    ) -> imap::error::Result<imap::types::ZeroCopy<Vec<imap::types::Name>>> {
        match self {
            ImapSession::Plain(s) => s.list(reference, pattern),
            ImapSession::Tls(s) => s.list(reference, pattern),
        }
    }

    pub fn create(&mut self, mailbox: &str) -> imap::error::Result<()> {
        match self {
            ImapSession::Plain(s) => s.create(mailbox),
            ImapSession::Tls(s) => s.create(mailbox),
        }
    }

    pub fn select(&mut self, mailbox: &str) -> imap::error::Result<imap::types::Mailbox> {
        match self {
            ImapSession::Plain(s) => s.select(mailbox),
            ImapSession::Tls(s) => s.select(mailbox),
        }
    }

    pub fn uid_search(
        &mut self,
        query: &str,
    ) -> imap::error::Result<std::collections::HashSet<u32>> {
        match self {
            ImapSession::Plain(s) => s.uid_search(query),
            ImapSession::Tls(s) => s.uid_search(query),
        }
    }

    pub fn uid_fetch(
        &mut self,
        uid_set: &str,
        query: &str,
    ) -> imap::error::Result<imap::types::ZeroCopy<Vec<imap::types::Fetch>>> {
        match self {
            ImapSession::Plain(s) => s.uid_fetch(uid_set, query),
            ImapSession::Tls(s) => s.uid_fetch(uid_set, query),
        }
    }

    pub fn uid_mv(&mut self, uid_set: &str, dest: &str) -> imap::error::Result<()> {
        match self {
            ImapSession::Plain(s) => s.uid_mv(uid_set, dest),
            ImapSession::Tls(s) => s.uid_mv(uid_set, dest),
        }
    }

    pub fn uid_copy(&mut self, uid_set: &str, dest: &str) -> imap::error::Result<()> {
        match self {
            ImapSession::Plain(s) => {
                s.uid_copy(uid_set, dest)?;
                Ok(())
            }
            ImapSession::Tls(s) => {
                s.uid_copy(uid_set, dest)?;
                Ok(())
            }
        }
    }

    pub fn uid_store(&mut self, uid_set: &str, query: &str) -> imap::error::Result<()> {
        match self {
            ImapSession::Plain(s) => {
                s.uid_store(uid_set, query)?;
                Ok(())
            }
            ImapSession::Tls(s) => {
                s.uid_store(uid_set, query)?;
                Ok(())
            }
        }
    }

    pub fn expunge(&mut self) -> imap::error::Result<()> {
        match self {
            ImapSession::Plain(s) => {
                s.expunge()?;
                Ok(())
            }
            ImapSession::Tls(s) => {
                s.expunge()?;
                Ok(())
            }
        }
    }

    pub fn logout(&mut self) -> imap::error::Result<()> {
        match self {
            ImapSession::Plain(s) => s.logout(),
            ImapSession::Tls(s) => s.logout(),
        }
    }

    pub fn has_capability(&mut self, cap: &str) -> bool {
        let caps = match self {
            ImapSession::Plain(s) => s.capabilities(),
            ImapSession::Tls(s) => s.capabilities(),
        };
        caps.map(|c| c.has_str(cap)).unwrap_or(false)
    }

    pub fn run_command_and_read_response(&mut self, command: &str) -> imap::error::Result<Vec<u8>> {
        match self {
            ImapSession::Plain(s) => s.run_command_and_read_response(command),
            ImapSession::Tls(s) => s.run_command_and_read_response(command),
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

    let addr = format!("{host}:{port}");

    if tls {
        let tls_connector = native_tls::TlsConnector::builder()
            .min_protocol_version(Some(native_tls::Protocol::Tlsv12))
            .build()
            .context("Failed to create TLS connector")?;
        let client = imap::connect((&*addr, port), host, &tls_connector)
            .context("Failed to connect via TLS")?;
        let session = client
            .login(user, pass)
            .map_err(|e| e.0)
            .context("IMAP login failed")?;
        Ok(ImapSession::Tls(session))
    } else {
        let tcp = TcpStream::connect(&addr).context(format!("Failed to connect to {addr}"))?;
        let client = imap::Client::new(tcp);
        let session = client
            .login(user, pass)
            .map_err(|e| e.0)
            .context("IMAP login failed")?;
        Ok(ImapSession::Plain(session))
    }
}
