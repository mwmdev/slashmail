//! Real-binary tests against a scripted, stateful IMAP server on loopback.
//!
//! These cover protocol boundaries GreenMail cannot exercise on demand:
//! missing capabilities, changing UIDVALIDITY, injected failures, and
//! malformed STATUS/QUOTA data. Every test runs the compiled CLI.

use parking_lot::Mutex;
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

#[derive(Clone)]
struct Message {
    uid: u32,
    raw: Vec<u8>,
    deleted: bool,
    seen: bool,
    flagged: bool,
}

struct Mailbox {
    name: String,
    attributes: String,
    /// Send the LIST name as a literal instead of a quoted string.
    literal: bool,
    /// UIDVALIDITY reported by the 1st, 2nd, ... SELECT (last value repeats).
    uid_validity: Vec<Option<u32>>,
    selects: usize,
    /// `(n, uid)`: another client expunges `uid` just before the nth open.
    vanish: Vec<(usize, u32)>,
    next_uid: u32,
    messages: Vec<Message>,
}

#[derive(Default)]
struct State {
    capabilities: String,
    mailboxes: Vec<Mailbox>,
    /// Raw untagged STATUS lines per mailbox; absent means `NO`.
    status: HashMap<String, String>,
    /// Raw untagged GETQUOTAROOT lines.
    quota: String,
    /// Command prefixes answered with `NO <text>`.
    fail: Vec<(String, String)>,
    commands: Vec<String>,
    selected: Option<usize>,
    /// The selected mailbox was opened with EXAMINE.
    read_only: bool,
    /// Raw untagged lines appended to every UID FETCH response, as a server
    /// reporting flag changes made by other clients would.
    unsolicited_fetch: String,
}

impl State {
    fn new(capabilities: &str) -> Self {
        State {
            capabilities: capabilities.to_string(),
            ..State::default()
        }
    }

    fn mailbox(mut self, name: &str, uid_validity: &[Option<u32>], messages: &[Message]) -> Self {
        self.mailboxes.push(Mailbox {
            name: name.to_string(),
            attributes: String::new(),
            literal: false,
            uid_validity: uid_validity.to_vec(),
            selects: 0,
            vanish: Vec::new(),
            next_uid: messages.iter().map(|m| m.uid).max().unwrap_or(0) + 1,
            messages: messages.to_vec(),
        });
        self
    }

    /// Add an empty mailbox whose LIST name is sent as an IMAP literal.
    fn literal(mut self, name: &str) -> Self {
        self = self.mailbox(name, &[Some(1)], &[]);
        self.mailboxes.last_mut().unwrap().literal = true;
        self
    }

    fn special(mut self, name: &str, attributes: &str) -> Self {
        self = self.mailbox(name, &[Some(1)], &[]);
        self.mailboxes.last_mut().unwrap().attributes = attributes.to_string();
        self
    }

    fn fail(mut self, prefix: &str, text: &str) -> Self {
        self.fail.push((prefix.to_string(), text.to_string()));
        self
    }

    fn find(&self, name: &str) -> Option<usize> {
        self.mailboxes.iter().position(|mailbox| {
            mailbox.name == name || (name.eq_ignore_ascii_case("INBOX") && mailbox.name == "INBOX")
        })
    }

    fn uids(&self, name: &str) -> Vec<u32> {
        let index = self.find(name).unwrap();
        self.mailboxes[index]
            .messages
            .iter()
            .map(|m| m.uid)
            .collect()
    }

    /// `(seen, flagged)` of one message.
    fn flags(&self, name: &str, uid: u32) -> (bool, bool) {
        let index = self.find(name).unwrap();
        let message = self.mailboxes[index]
            .messages
            .iter()
            .find(|m| m.uid == uid)
            .unwrap();
        (message.seen, message.flagged)
    }
}

fn message(uid: u32, subject: &str) -> Message {
    Message {
        uid,
        raw: format!(
            "From: sender@example.com\r\nTo: user@example.com\r\nSubject: {subject}\r\n\
             Date: Mon, 1 Apr 2026 10:00:0{} +0000\r\nMessage-ID: <{uid}@example.com>\r\n\r\n\
             Body of {subject}\r\n",
            uid % 10
        )
        .into_bytes(),
        deleted: false,
        seen: false,
        flagged: false,
    }
}

fn raw_message(uid: u32, raw: &[u8]) -> Message {
    Message {
        uid,
        raw: raw.to_vec(),
        deleted: false,
        seen: false,
        flagged: false,
    }
}

struct Server {
    port: u16,
    state: Arc<Mutex<State>>,
    /// Set once the CLI has exited, so a server that was never connected to
    /// stops waiting for a client.
    stop: Arc<AtomicBool>,
    handle: thread::JoinHandle<()>,
}

impl Server {
    fn start(state: State) -> Server {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let state = Arc::new(Mutex::new(state));
        let stop = Arc::new(AtomicBool::new(false));
        let shared = Arc::clone(&state);
        let stopped = Arc::clone(&stop);
        let handle = thread::spawn(move || loop {
            match listener.accept() {
                Ok((stream, _)) => {
                    stream.set_nonblocking(false).unwrap();
                    serve(stream, &shared);
                    return;
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    // The CLI waits for the greeting, so once it has exited
                    // no connection can still be pending.
                    if stopped.load(Ordering::SeqCst) {
                        return;
                    }
                    thread::sleep(Duration::from_millis(5));
                }
                Err(error) => panic!("accept failed: {error}"),
            }
        });
        Server {
            port,
            state,
            stop,
            handle,
        }
    }

    /// Call after the CLI has exited: wait for the session (if any) to end
    /// and return the server state.
    fn finish(self) -> State {
        self.stop.store(true, Ordering::SeqCst);
        self.handle.join().unwrap();
        Arc::try_unwrap(self.state).ok().unwrap().into_inner()
    }
}

fn read_command(reader: &mut BufReader<TcpStream>, writer: &mut TcpStream) -> Option<Vec<u8>> {
    let mut command = Vec::new();
    loop {
        let mut line = Vec::new();
        if reader.read_until(b'\n', &mut line).ok()? == 0 {
            return None;
        }
        command.extend_from_slice(&line);
        let Some(length) = literal_length(&line) else {
            break;
        };
        if !line.ends_with(b"+}\r\n") {
            writer.write_all(b"+ go ahead\r\n").ok()?;
        }
        let mut literal = vec![0; length];
        reader.read_exact(&mut literal).ok()?;
        command.extend_from_slice(&literal);
    }
    while command
        .last()
        .is_some_and(|byte| *byte == b'\n' || *byte == b'\r')
    {
        command.pop();
    }
    Some(command)
}

fn literal_length(line: &[u8]) -> Option<usize> {
    let text = std::str::from_utf8(line).ok()?.strip_suffix("}\r\n")?;
    let (_, digits) = text.rsplit_once('{')?;
    digits.trim_end_matches('+').parse().ok()
}

/// Parse one IMAP astring (quoted, literal, or atom) from the front of `input`.
fn take_astring(input: &str) -> (String, &str) {
    if let Some(rest) = input.strip_prefix('"') {
        let mut value = String::new();
        let mut chars = rest.char_indices();
        while let Some((index, character)) = chars.next() {
            match character {
                '\\' => value.push(chars.next().unwrap().1),
                '"' => return (value, &rest[index + 1..]),
                other => value.push(other),
            }
        }
        panic!("unterminated quoted string: {input}");
    }
    if input.starts_with('{') {
        let close = input.find('}').unwrap();
        let length: usize = input[1..close].trim_end_matches('+').parse().unwrap();
        let body = &input[close + 3..];
        return (body[..length].to_string(), &body[length..]);
    }
    let end = input.find(' ').unwrap_or(input.len());
    (input[..end].to_string(), &input[end..])
}

fn uid_set(set: &str) -> Vec<u32> {
    set.split(',')
        .flat_map(|part| match part.split_once(':') {
            Some((start, end)) => {
                (start.parse::<u32>().unwrap()..=end.parse::<u32>().unwrap()).collect()
            }
            None => vec![part.parse::<u32>().unwrap()],
        })
        .collect()
}

fn subject_of(raw: &[u8]) -> String {
    String::from_utf8_lossy(raw)
        .lines()
        .find_map(|line| line.strip_prefix("Subject: ").map(str::to_string))
        .unwrap_or_default()
}

fn matching_uids(messages: &[Message], query: &str) -> Vec<u32> {
    let query = query.strip_prefix("CHARSET UTF-8 ").unwrap_or(query);
    let mut subject = None;
    let mut uids = None;
    let mut rest = query;
    while !rest.is_empty() {
        rest = rest.trim_start();
        if let Some(after) = rest.strip_prefix("SUBJECT ") {
            let (value, remaining) = take_astring(after);
            subject = Some(value.to_lowercase());
            rest = remaining;
        } else if let Some(after) = rest.strip_prefix("UID ") {
            let (value, remaining) = take_astring(after);
            uids = Some(uid_set(&value));
            rest = remaining;
        } else {
            let (_, remaining) = take_astring(rest);
            rest = remaining;
        }
    }
    messages
        .iter()
        .filter(|m| {
            subject
                .as_ref()
                .is_none_or(|s| subject_of(&m.raw).to_lowercase().contains(s))
        })
        .filter(|m| uids.as_ref().is_none_or(|set| set.contains(&m.uid)))
        .map(|m| m.uid)
        .collect()
}

fn serve(stream: TcpStream, shared: &Mutex<State>) {
    stream
        .set_read_timeout(Some(Duration::from_secs(20)))
        .unwrap();
    let mut writer = stream.try_clone().unwrap();
    let mut reader = BufReader::new(stream);
    writer.write_all(b"* OK scripted IMAP ready\r\n").unwrap();

    while let Some(raw) = read_command(&mut reader, &mut writer) {
        let command = String::from_utf8(raw).unwrap();
        let (tag, rest) = command.split_once(' ').unwrap();
        let mut state = shared.lock();
        state.commands.push(rest.to_string());

        if let Some((_, text)) = state
            .fail
            .iter()
            .find(|(prefix, _)| rest.starts_with(prefix))
        {
            let reply = format!("{tag} NO {text}\r\n");
            drop(state);
            writer.write_all(reply.as_bytes()).unwrap();
            continue;
        }

        let mut out = Vec::new();
        let verb = rest.split(' ').next().unwrap().to_ascii_uppercase();
        let uid_verb = rest
            .strip_prefix("UID ")
            .map(|r| r.split(' ').next().unwrap().to_ascii_uppercase());
        let mutates = matches!(
            (verb.as_str(), uid_verb.as_deref()),
            ("EXPUNGE", _) | ("UID", Some("STORE" | "MOVE" | "EXPUNGE"))
        );
        if mutates && state.read_only {
            // Like a real server: EXAMINE opens the mailbox read-only.
            out.extend(format!("{tag} NO [READ-ONLY] mailbox is read-only\r\n").bytes());
            drop(state);
            writer.write_all(&out).unwrap();
            continue;
        }
        match (verb.as_str(), uid_verb.as_deref()) {
            ("LOGIN", _) => out.extend(format!("{tag} OK logged in\r\n").bytes()),
            ("CAPABILITY", _) => out.extend(
                format!("* CAPABILITY {}\r\n{tag} OK done\r\n", state.capabilities).bytes(),
            ),
            ("LIST", _) => {
                for mailbox in &state.mailboxes {
                    let name = if mailbox.literal {
                        format!("{{{}}}\r\n{}", mailbox.name.len(), mailbox.name)
                    } else {
                        let quoted = mailbox.name.replace('\\', "\\\\").replace('"', "\\\"");
                        format!("\"{quoted}\"")
                    };
                    out.extend(format!("* LIST ({}) \"/\" {name}\r\n", mailbox.attributes).bytes());
                }
                out.extend(format!("{tag} OK listed\r\n").bytes());
            }
            ("SELECT" | "EXAMINE", _) => {
                let (name, _) = take_astring(&rest[verb.len() + 1..]);
                match state.find(&name) {
                    Some(index) => {
                        state.selected = Some(index);
                        state.read_only = verb == "EXAMINE";
                        let access = if state.read_only {
                            "READ-ONLY"
                        } else {
                            "READ-WRITE"
                        };
                        let mailbox = &mut state.mailboxes[index];
                        mailbox.selects += 1;
                        let opened = mailbox.selects;
                        let gone: Vec<u32> = mailbox
                            .vanish
                            .iter()
                            .filter(|(n, _)| *n == opened)
                            .map(|(_, uid)| *uid)
                            .collect();
                        mailbox.messages.retain(|m| !gone.contains(&m.uid));
                        let validity = mailbox
                            .uid_validity
                            .get(mailbox.selects - 1)
                            .or(mailbox.uid_validity.last())
                            .copied()
                            .flatten();
                        out.extend(format!("* {} EXISTS\r\n", mailbox.messages.len()).bytes());
                        out.extend(b"* 0 RECENT\r\n");
                        if let Some(value) = validity {
                            out.extend(format!("* OK [UIDVALIDITY {value}] ok\r\n").bytes());
                        }
                        out.extend(format!("* OK [UIDNEXT {}] ok\r\n", mailbox.next_uid).bytes());
                        out.extend(format!("{tag} OK [{access}] selected\r\n").bytes());
                    }
                    None => out.extend(format!("{tag} NO no such mailbox\r\n").bytes()),
                }
            }
            ("UID", Some("SEARCH")) => {
                let index = state.selected.unwrap();
                let uids = matching_uids(&state.mailboxes[index].messages, &rest[11..]);
                let list: Vec<String> = uids.iter().map(u32::to_string).collect();
                out.extend(format!("* SEARCH {}\r\n{tag} OK searched\r\n", list.join(" ")).bytes());
            }
            ("UID", Some("SORT")) => {
                let index = state.selected.unwrap();
                let query = rest.strip_prefix("UID SORT (REVERSE DATE) UTF-8 ").unwrap();
                let mut uids = matching_uids(&state.mailboxes[index].messages, query);
                uids.sort_unstable_by(|a, b| b.cmp(a));
                let list: Vec<String> = uids.iter().map(u32::to_string).collect();
                out.extend(format!("* SORT {}\r\n{tag} OK sorted\r\n", list.join(" ")).bytes());
            }
            ("UID", Some("FETCH")) => {
                let index = state.selected.unwrap();
                let mut parts = rest[10..].splitn(2, ' ');
                let wanted = uid_set(parts.next().unwrap());
                let items = parts.next().unwrap();
                let section = items
                    .split_once("BODY.PEEK[")
                    .map(|(_, after)| after.split_once(']').unwrap().0.to_string())
                    .unwrap();
                for (position, message) in state.mailboxes[index].messages.iter().enumerate() {
                    if !wanted.contains(&message.uid) {
                        continue;
                    }
                    let payload: &[u8] = if section.is_empty() {
                        &message.raw
                    } else {
                        let end = message
                            .raw
                            .windows(4)
                            .position(|w| w == b"\r\n\r\n")
                            .map_or(message.raw.len(), |p| p + 4);
                        &message.raw[..end]
                    };
                    let flags: Vec<&str> = [
                        (message.deleted, "\\Deleted"),
                        (message.seen, "\\Seen"),
                        (message.flagged, "\\Flagged"),
                    ]
                    .into_iter()
                    .filter_map(|(set, name)| set.then_some(name))
                    .collect();
                    let flags = flags.join(" ");
                    out.extend(
                        format!(
                            "* {} FETCH (UID {} FLAGS ({flags}) RFC822.SIZE {} BODY[{section}] {{{}}}\r\n",
                            position + 1,
                            message.uid,
                            message.raw.len(),
                            payload.len()
                        )
                        .bytes(),
                    );
                    out.extend_from_slice(payload);
                    out.extend(b")\r\n");
                }
                out.extend(state.unsolicited_fetch.bytes());
                out.extend(format!("{tag} OK fetched\r\n").bytes());
            }
            ("UID", Some("STORE")) => {
                let index = state.selected.unwrap();
                let mut parts = rest[10..].splitn(2, ' ');
                let wanted = uid_set(parts.next().unwrap());
                let operation = parts.next().unwrap();
                let (value, flag) = match operation.split_once(" (") {
                    Some(("+FLAGS", flag)) => (true, flag.trim_end_matches(')')),
                    Some(("-FLAGS", flag)) => (false, flag.trim_end_matches(')')),
                    _ => panic!("unsupported STORE: {operation}"),
                };
                for message in &mut state.mailboxes[index].messages {
                    if wanted.contains(&message.uid) {
                        match flag {
                            "\\Deleted" => message.deleted = value,
                            "\\Seen" => message.seen = value,
                            "\\Flagged" => message.flagged = value,
                            other => panic!("unsupported flag: {other}"),
                        }
                    }
                }
                out.extend(format!("{tag} OK stored\r\n").bytes());
            }
            ("UID", Some(verb @ ("COPY" | "MOVE"))) => {
                let index = state.selected.unwrap();
                let after = &rest[verb.len() + 5..];
                let (set, target) = after.split_once(' ').unwrap();
                let wanted = uid_set(set);
                let (target, _) = take_astring(target);
                let target = state.find(&target).unwrap();
                let moved: Vec<Message> = state.mailboxes[index]
                    .messages
                    .iter()
                    .filter(|m| wanted.contains(&m.uid))
                    .cloned()
                    .collect();
                for mut copy in moved {
                    copy.uid = state.mailboxes[target].next_uid;
                    copy.deleted = false;
                    state.mailboxes[target].next_uid += 1;
                    state.mailboxes[target].messages.push(copy);
                }
                if verb == "MOVE" {
                    state.mailboxes[index]
                        .messages
                        .retain(|m| !wanted.contains(&m.uid));
                }
                out.extend(format!("{tag} OK done\r\n").bytes());
            }
            ("UID", Some("EXPUNGE")) => {
                let index = state.selected.unwrap();
                let wanted = uid_set(&rest[12..]);
                state.mailboxes[index]
                    .messages
                    .retain(|m| !(m.deleted && wanted.contains(&m.uid)));
                out.extend(format!("{tag} OK expunged\r\n").bytes());
            }
            ("EXPUNGE", _) => {
                let index = state.selected.unwrap();
                state.mailboxes[index].messages.retain(|m| !m.deleted);
                out.extend(format!("{tag} OK expunged\r\n").bytes());
            }
            ("STATUS", _) => {
                let (name, _) = take_astring(&rest[7..]);
                match state.status.get(&name) {
                    Some(line) => out.extend(format!("{line}\r\n{tag} OK status\r\n").bytes()),
                    None => out.extend(format!("{tag} NO no status\r\n").bytes()),
                }
            }
            ("GETQUOTAROOT", _) => {
                out.extend(format!("{}{tag} OK quota\r\n", state.quota).bytes());
            }
            ("LOGOUT", _) => {
                out.extend(format!("* BYE\r\n{tag} OK bye\r\n").bytes());
                drop(state);
                let _ = writer.write_all(&out);
                return;
            }
            _ => out.extend(format!("{tag} BAD unsupported\r\n").bytes()),
        }
        drop(state);
        writer.write_all(&out).unwrap();
    }
}

fn slashmail(port: u16, dir: &Path, args: &[&str]) -> Output {
    let config = dir.join("config.toml");
    std::fs::write(&config, "").unwrap();
    Command::new(env!("CARGO_BIN_EXE_slashmail"))
        .current_dir(dir)
        .arg("--config")
        .arg(&config)
        .args(["--host", "127.0.0.1", "--port", &port.to_string()])
        .args(["-u", "user@example.com"])
        .args(args)
        .env("SLASHMAIL_PASS", "secret")
        .env_remove("SLASHMAIL_USER")
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

fn run(state: State, args: &[&str]) -> (Output, State) {
    let dir = tempfile::tempdir().unwrap();
    run_in(state, dir.path(), args)
}

fn run_in(state: State, dir: &Path, args: &[&str]) -> (Output, State) {
    let server = Server::start(state);
    let output = slashmail(server.port, dir, args);
    (output, server.finish())
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "status {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        stdout(output),
        stderr(output)
    );
}

fn assert_failure(output: &Output, message: &str) {
    assert!(
        !output.status.success(),
        "unexpected success:\n{}",
        stdout(output)
    );
    assert!(
        stderr(output).contains(message),
        "stderr lacks {message:?}:\n{}",
        stderr(output)
    );
}

/// A handled error: exit status 1 with a rendered `Error:` line (a panic or
/// signal would exit differently).
fn assert_controlled_failure(output: &Output) {
    assert_eq!(output.status.code(), Some(1), "{}", stderr(output));
    assert!(stderr(output).starts_with("Error: "), "{}", stderr(output));
    assert!(!stderr(output).contains("panicked"), "{}", stderr(output));
}

fn mutations(commands: &[String]) -> Vec<&String> {
    commands
        .iter()
        .filter(|c| {
            [
                "UID STORE",
                "UID COPY",
                "UID MOVE",
                "UID EXPUNGE",
                "EXPUNGE",
                "APPEND",
            ]
            .iter()
            .any(|verb| c.starts_with(verb))
        })
        .collect()
}

fn opens_mailbox(command: &str) -> bool {
    command.starts_with("SELECT") || command.starts_with("EXAMINE")
}

fn inbox_with_target_and_deleted_neighbor(capabilities: &str) -> State {
    let mut neighbor = message(9, "Unrelated");
    neighbor.deleted = true;
    State::new(capabilities)
        .mailbox("INBOX", &[Some(10)], &[message(7, "Target"), neighbor])
        .mailbox("Trash", &[Some(20)], &[])
        .mailbox("Archive", &[Some(30)], &[])
}

#[test]
fn move_help_and_missing_destination_are_parsed_without_connecting() {
    let help = Command::new(env!("CARGO_BIN_EXE_slashmail"))
        .args(["move", "--help"])
        .output()
        .unwrap();
    assert_success(&help);
    let text = stdout(&help);
    assert!(text.contains("--dest <DEST>"), "{text}");
    assert!(text.contains("--to <TO>"), "{text}");

    let server = Server::start(State::new("IMAP4rev1 MOVE"));
    let dir = tempfile::tempdir().unwrap();
    let output = slashmail(
        server.port,
        dir.path(),
        &["move", "--to", "Archive", "--yes"],
    );
    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
    assert!(stderr(&output).contains("--dest"));
    assert!(server.finish().commands.is_empty());
}

#[test]
fn remote_plaintext_is_refused_before_password_or_network() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.toml");
    std::fs::write(&config, "").unwrap();
    for args in [vec!["search"], vec!["draft", "--to", "someone@example.com"]] {
        let output = Command::new(env!("CARGO_BIN_EXE_slashmail"))
            .arg("--config")
            .arg(&config)
            .args(["--host", "mail.example.invalid", "-u", "user@example.com"])
            .args(&args)
            .env_remove("SLASHMAIL_PASS")
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert_failure(
            &output,
            "IMAP connections to non-loopback hosts require TLS",
        );
        assert!(!stderr(&output).contains("password"), "{}", stderr(&output));
    }
}

#[test]
fn move_capability_moves_only_the_selected_uid() {
    let (output, state) = run(
        inbox_with_target_and_deleted_neighbor("IMAP4rev1 MOVE UIDPLUS"),
        &["move", "--subject", "Target", "--dest", "Archive", "--yes"],
    );
    assert_success(&output);
    assert_eq!(mutations(&state.commands), [r#"UID MOVE 7 "Archive""#]);
    assert_eq!(state.uids("INBOX"), [9]);
    assert_eq!(state.uids("Archive"), [1]);
}

#[test]
fn uidplus_fallback_expunges_only_the_moved_uid() {
    let (output, state) = run(
        inbox_with_target_and_deleted_neighbor("IMAP4rev1 UIDPLUS"),
        &["delete", "--subject", "Target", "--yes"],
    );
    assert_success(&output);
    assert_eq!(
        mutations(&state.commands),
        [
            r#"UID COPY 7 "Trash""#,
            r"UID STORE 7 +FLAGS (\Deleted)",
            "UID EXPUNGE 7"
        ]
    );
    assert_eq!(
        state.uids("INBOX"),
        [9],
        "unrelated \\Deleted message was expunged"
    );
    assert_eq!(state.uids("Trash"), [1]);
}

#[test]
fn servers_without_move_or_uidplus_are_refused_before_mutation() {
    let (output, state) = run(
        inbox_with_target_and_deleted_neighbor("IMAP4rev1"),
        &["move", "--subject", "Target", "--dest", "Archive", "--yes"],
    );
    assert_failure(
        &output,
        "Safe move requires server support for MOVE or UIDPLUS",
    );
    assert!(
        mutations(&state.commands).is_empty(),
        "{:?}",
        state.commands
    );
    assert_eq!(state.uids("INBOX"), [7, 9]);
}

#[test]
fn failure_after_copy_is_reported_once_without_retry() {
    let (output, state) = run(
        inbox_with_target_and_deleted_neighbor("IMAP4rev1 UIDPLUS")
            .fail("UID STORE", "store refused"),
        &["delete", "--subject", "Target", "--yes"],
    );
    assert_failure(&output, "failed after COPY");
    let copies = state
        .commands
        .iter()
        .filter(|c| c.starts_with("UID COPY"))
        .count();
    assert_eq!(copies, 1);
    assert!(!state.commands.iter().any(|c| c.contains("EXPUNGE")));
}

#[test]
fn changed_or_missing_uidvalidity_blocks_every_uid_action() {
    for later in [Some(11), None] {
        for args in [
            vec!["delete", "--subject", "Target", "--yes"],
            vec!["move", "--subject", "Target", "--dest", "Archive", "--yes"],
            vec!["mark", "--subject", "Target", "--read", "--yes"],
            vec!["export", "--subject", "Target", "--yes", "-o", "out"],
            vec!["read", "--subject", "Target"],
        ] {
            let state = State::new("IMAP4rev1 MOVE UIDPLUS")
                .mailbox("INBOX", &[Some(10), later], &[message(7, "Target")])
                .mailbox("Trash", &[Some(20)], &[])
                .mailbox("Archive", &[Some(30)], &[]);
            let dir = tempfile::tempdir().unwrap();
            let (output, state) = run_in(state, dir.path(), &args);
            assert_failure(
                &output,
                "Mailbox UIDVALIDITY changed; search again before acting",
            );
            assert!(
                mutations(&state.commands).is_empty(),
                "{args:?}: {:?}",
                state.commands
            );
            assert!(
                !state.commands.iter().any(|c| c.contains("BODY.PEEK[]")),
                "{args:?} fetched a body after the identity changed"
            );
            assert!(!dir.path().join("out").join("INBOX_7.eml").exists());
        }
    }
}

#[test]
fn unchanged_uidvalidity_allows_the_action() {
    let state = State::new("IMAP4rev1 MOVE")
        .mailbox("INBOX", &[Some(10), Some(10)], &[message(7, "Target")])
        .mailbox("Archive", &[Some(30)], &[]);
    let (output, state) = run(
        state,
        &["move", "--subject", "Target", "--dest", "Archive", "--yes"],
    );
    assert_success(&output);
    assert_eq!(state.uids("Archive"), [1]);
}

#[test]
fn non_ascii_search_uses_utf8_literals_with_literal_plus() {
    let target = message(3, "café menu");
    let state = || {
        State::new("IMAP4rev1 LITERAL+ SORT").mailbox(
            "INBOX",
            &[Some(1)],
            &[target.clone(), message(4, "cafe menu")],
        )
    };

    let (output, server) = run(state(), &["search", "--subject", "café", "--json"]);
    assert_success(&output);
    assert!(server
        .commands
        .contains(&"UID SORT (REVERSE DATE) UTF-8 SUBJECT {5+}\r\ncafé".to_string()));
    let rows: serde_json::Value = serde_json::from_str(&stdout(&output)).unwrap();
    assert_eq!(rows.as_array().unwrap().len(), 1);
    assert_eq!(rows[0]["uid"], 3);

    // SORT failure falls back to SEARCH with the same encoding and filters.
    let (output, server) = run(
        state().fail("UID SORT", "sort unavailable"),
        &["search", "--subject", "café", "--to", "a\"b", "--json"],
    );
    assert_success(&output);
    assert!(server
        .commands
        .contains(&"UID SEARCH CHARSET UTF-8 SUBJECT {5+}\r\ncafé TO \"a\\\"b\"".to_string()));

    let (output, server) = run(
        State::new("IMAP4rev1 LITERAL+").mailbox(
            "INBOX",
            &[Some(1)],
            std::slice::from_ref(&target),
        ),
        &["count", "--subject", "café", "--from", "Zoë"],
    );
    assert_success(&output);
    assert!(server
        .commands
        .contains(&"UID SEARCH CHARSET UTF-8 SUBJECT {5+}\r\ncafé FROM {4+}\r\nZoë".to_string()));
}

#[test]
fn non_ascii_search_without_literal_plus_fails_before_searching() {
    for args in [
        vec!["search", "--subject", "café"],
        vec!["search", "--subject", "café", "--all-folders"],
        vec!["count", "--subject", "café", "--all-folders"],
    ] {
        let (output, state) = run(
            State::new("IMAP4rev1 SORT").mailbox("INBOX", &[Some(1)], &[message(3, "café")]),
            &args,
        );
        assert_failure(
            &output,
            "Non-ASCII search requires server support for LITERAL+",
        );
        assert!(
            !state
                .commands
                .iter()
                .any(|c| c.starts_with("LIST") || opens_mailbox(c) || c.starts_with("UID")),
            "{args:?}: {:?}",
            state.commands
        );
    }

    let (output, _) = run(
        State::new("IMAP4rev1 SORT").mailbox("INBOX", &[Some(1)], &[message(3, "cafe")]),
        &["search", "--subject", "cafe"],
    );
    assert_success(&output);
    assert!(stdout(&output).contains("1 message(s)"));
}

#[test]
fn folder_lookup_is_exact_and_rejects_control_characters() {
    let name = r#"Projects "Q1" \ *%"#;
    let (output, state) = run(
        State::new("IMAP4rev1")
            .mailbox("INBOX", &[Some(1)], &[])
            .mailbox("Projects", &[Some(2)], &[])
            .mailbox(name, &[Some(3)], &[message(5, "Quarterly")]),
        &["search", "-f", name],
    );
    assert_success(&output);
    assert!(stdout(&output).contains("Quarterly"));
    assert!(state.commands.contains(&r#"LIST "" *"#.to_string()));
    assert!(state
        .commands
        .contains(&r#"EXAMINE "Projects \"Q1\" \\ *%""#.to_string()));

    let (output, state) = run(
        State::new("IMAP4rev1").mailbox("INBOX", &[Some(1)], &[]),
        &["search", "-f", "Projects"],
    );
    assert_failure(&output, "does not exist");
    assert!(!state.commands.iter().any(|c| opens_mailbox(c)));

    for folder in ["INBOX\r\nA1 DELETE INBOX", "INBOX\u{85}", "INBOX\u{1b}"] {
        for command in ["count", "search"] {
            let (output, state) = run(
                State::new("IMAP4rev1").mailbox("INBOX", &[Some(1)], &[]),
                &[command, "-f", folder],
            );
            assert_failure(&output, "Folder name must not contain control characters");
            assert!(!state
                .commands
                .iter()
                .any(|c| c.contains("DELETE") || opens_mailbox(c)));
        }
    }
}

#[test]
fn all_folders_skips_special_use_mailboxes_but_not_substring_matches() {
    let state = || {
        State::new("IMAP4rev1")
            .mailbox("INBOX", &[Some(1)], &[message(1, "Hello")])
            .mailbox("Small mail", &[Some(2)], &[message(1, "Hello small")])
            .special("Everything", "\\All")
            .special("Deleted Items", "\\Trash")
            .special("Junk Email", "\\HasNoChildren \\Junk")
    };
    let (output, server) = run(state(), &["count", "--all-folders", "--subject", "Hello"]);
    assert_success(&output);
    let opened: Vec<&String> = server
        .commands
        .iter()
        .filter(|c| opens_mailbox(c))
        .collect();
    // Read-only commands never open a mailbox read-write.
    assert_eq!(opened, [r#"EXAMINE "INBOX""#, r#"EXAMINE "Small mail""#]);
    assert!(stdout(&output).contains("1 message(s) in Small mail"));

    let (output, _) = run(state(), &["search", "--all-folders", "--json"]);
    assert_success(&output);
    let rows: serde_json::Value = serde_json::from_str(&stdout(&output)).unwrap();
    let folders: Vec<&str> = rows
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["folder"].as_str().unwrap())
        .collect();
    assert!(folders.contains(&"Small mail"));
    assert!(!folders.contains(&"Everything"));
}

#[test]
fn literal_and_escaped_mailbox_names_round_trip() {
    let literal = r"Work\Plans";
    let quoted = r#"A"B"#;
    let mut state = State::new("IMAP4rev1")
        .mailbox("INBOX", &[Some(1)], &[])
        .literal(literal)
        .mailbox(quoted, &[Some(2)], &[]);
    state.status.insert(
        literal.into(),
        "* STATUS {10}\r\nWork\\Plans (MESSAGES 3 UNSEEN 2 RECENT 1)".into(),
    );
    state.status.insert(
        quoted.into(),
        r#"* STATUS "A\"B" (MESSAGES 5 UNSEEN 1 RECENT 0)"#.into(),
    );
    let (output, server) = run(state, &["status"]);
    assert_success(&output);
    assert!(server
        .commands
        .contains(&r#"STATUS "Work\\Plans" (MESSAGES UNSEEN RECENT)"#.to_string()));
    let table = stdout(&output);
    let cells = |name: &str| -> Vec<String> {
        table
            .lines()
            .find(|line| line.contains(name))
            .unwrap_or_else(|| panic!("no row for {name}:\n{table}"))
            .split(['│', '┆'])
            .map(str::trim)
            .filter(|cell| !cell.is_empty())
            .map(str::to_string)
            .collect()
    };
    assert_eq!(cells(literal), [literal, "3", "2", "1"]);
    assert_eq!(cells(quoted), [quoted, "5", "1", "0"]);

    let (output, server) = run(
        State::new("IMAP4rev1")
            .mailbox("INBOX", &[Some(1)], &[])
            .literal(literal),
        &["count", "--all-folders"],
    );
    assert_success(&output);
    assert!(server
        .commands
        .contains(&r#"EXAMINE "Work\\Plans""#.to_string()));
}

#[test]
fn status_reports_exact_counts_and_unknown_for_malformed_responses() {
    let mut state = State::new("IMAP4rev1")
        .mailbox("Projects (2024)", &[Some(1)], &[])
        .mailbox("Broken", &[Some(2)], &[])
        .mailbox("Refused", &[Some(3)], &[]);
    state.status.insert(
        "Projects (2024)".into(),
        r#"* STATUS "Projects (2024)" (MESSAGES 5 UNSEEN 1 RECENT 0)"#.into(),
    );
    state
        .status
        .insert("Broken".into(), r#"* STATUS "Broken" (MESSAGES x)"#.into());
    let (output, _) = run(state, &["status"]);
    assert_success(&output);
    let table = stdout(&output);
    let row = |name: &str| {
        table
            .lines()
            .find(|line| line.contains(name))
            .unwrap_or_else(|| panic!("no row for {name}:\n{table}"))
            .split(['│', '┆'])
            .map(str::trim)
            .filter(|cell| !cell.is_empty())
            .map(str::to_string)
            .collect::<Vec<_>>()
    };
    assert_eq!(row("Projects (2024)"), ["Projects (2024)", "5", "1", "0"]);
    assert_eq!(row("Broken"), ["Broken", "?", "?", "?"]);
    assert_eq!(row("Refused"), ["Refused", "?", "?", "?"]);
}

#[test]
fn quota_reports_every_resource_and_propagates_malformed_responses() {
    let quota = |lines: &str| {
        let mut state = State::new("IMAP4rev1 QUOTA").mailbox("INBOX", &[Some(1)], &[]);
        state.quota = lines.to_string();
        state
    };

    let (output, _) = run(
        quota("* QUOTAROOT INBOX \"\"\r\n* QUOTA \"\" (STORAGE 100 1000 MESSAGE 999 1000)\r\n"),
        &["quota"],
    );
    assert_success(&output);
    let table = stdout(&output);
    let storage = table.lines().find(|l| l.contains("STORAGE")).unwrap();
    assert!(storage.contains("100K") && storage.contains("1000K") && storage.contains("10.0%"));
    let messages = table.lines().find(|l| l.contains("MESSAGE")).unwrap();
    assert!(messages.contains("999") && messages.contains("1000") && messages.contains("99.9%"));

    let (output, _) = run(
        quota("* QUOTAROOT INBOX \"\"\r\n* QUOTA \"\" ()\r\n"),
        &["quota"],
    );
    assert_success(&output);
    assert!(stdout(&output).contains("No quota information available."));

    let (output, _) = run(quota("* QUOTA \"\" (STORAGE ten 1000)\r\n"), &["quota"]);
    assert!(!output.status.success());
}

#[test]
fn server_errors_are_rendered_inertly() {
    let (output, _) = run(
        State::new("IMAP4rev1")
            .mailbox("INBOX", &[Some(1)], &[])
            .fail("EXAMINE", "\u{1b}]52;c;cHduZWQ=\u{7}\u{1b}[2Jdenied"),
        &["count"],
    );
    assert!(!output.status.success());
    let text = stderr(&output);
    assert!(text.starts_with("Error: "), "{text}");
    assert!(text.contains("denied"));
    assert!(
        !text.contains('\u{1b}') && !text.contains('\u{7}'),
        "{text:?}"
    );
}

fn nested_multipart(depth: usize) -> Vec<u8> {
    let mut message = b"Content-Type: text/plain\r\n\r\nDeep body".to_vec();
    for index in 0..depth {
        let boundary = format!("depth{index}");
        let mut outer =
            format!("Content-Type: multipart/mixed; boundary={boundary}\r\n\r\n--{boundary}\r\n")
                .into_bytes();
        outer.extend_from_slice(&message);
        outer.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
        message = outer;
    }
    let mut raw = b"From: sender@example.com\r\nSubject: Deep\r\nDate: Mon, 1 Apr 2026 10:00:00 +0000\r\nMessage-ID: <deep@example.com>\r\nMIME-Version: 1.0\r\n".to_vec();
    raw.extend_from_slice(&message);
    raw
}

#[test]
fn parser_depth_limit_is_a_controlled_failure_on_every_path() {
    let state = |depth| {
        State::new("IMAP4rev1 UIDPLUS")
            .mailbox(
                "INBOX",
                &[Some(1)],
                &[raw_message(4, &nested_multipart(depth))],
            )
            .special("Drafts", "\\Drafts")
    };

    let (output, _) = run(state(100), &["read", "--uid", "4", "--json"]);
    assert_success(&output);
    assert!(stdout(&output).contains("Deep body"));

    let (output, _) = run(state(101), &["read", "--uid", "4", "--json"]);
    assert_controlled_failure(&output);
    assert!(
        output.stdout.is_empty(),
        "partial JSON: {}",
        stdout(&output)
    );

    let (output, _) = run(state(101), &["read", "--uid", "4"]);
    assert_success(&output);
    assert!(stderr(&output).contains("Warning: failed to parse message"));
    assert!(stdout(&output).contains("Deep body"));

    let dir = tempfile::tempdir().unwrap();
    let (output, _) = run_in(
        state(101),
        dir.path(),
        &["attachments", "4", "--save", "-o", "saved"],
    );
    assert_controlled_failure(&output);
    assert!(!dir.path().join("saved").exists());

    let server = Server::start(state(101));
    let mut child = Command::new(env!("CARGO_BIN_EXE_slashmail"))
        .arg("--config")
        .arg({
            let config = dir.path().join("config.toml");
            std::fs::write(&config, "").unwrap();
            config
        })
        .current_dir(dir.path())
        .args(["--host", "127.0.0.1", "--port", &server.port.to_string()])
        .args(["-u", "user@example.com", "reply", "4"])
        .env("SLASHMAIL_PASS", "secret")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(b"Thanks").unwrap();
    let output = child.wait_with_output().unwrap();
    let state = server.finish();
    assert_controlled_failure(&output);
    assert!(!state.commands.iter().any(|c| c.starts_with("APPEND")));
}

#[test]
fn unsolicited_fetch_responses_never_join_the_action_set() {
    // Without SORT the result set comes from SEARCH plus FETCH, so a flag
    // change reported for another message must not become a row.
    let mut state = State::new("IMAP4rev1 MOVE")
        .mailbox(
            "INBOX",
            &[Some(10)],
            &[message(7, "Target"), message(9, "Unrelated")],
        )
        .mailbox("Archive", &[Some(30)], &[]);
    state.unsolicited_fetch = "* 2 FETCH (UID 9 FLAGS (\\Seen))\r\n".into();
    let (output, state) = run(
        state,
        &["move", "--subject", "Target", "--dest", "Archive", "--yes"],
    );
    assert_success(&output);
    assert_eq!(mutations(&state.commands), [r#"UID MOVE 7 "Archive""#]);
    assert_eq!(state.uids("INBOX"), [9]);
    assert!(stdout(&output).contains("Moved 1 message(s)"));

    // A late flags-only response for a matched message keeps its headers.
    let mut state =
        State::new("IMAP4rev1 SORT").mailbox("INBOX", &[Some(10)], &[message(7, "Target")]);
    state.unsolicited_fetch = "* 1 FETCH (UID 7 FLAGS (\\Seen))\r\n".into();
    let (output, _) = run(state, &["search", "--subject", "Target", "--json"]);
    assert_success(&output);
    let rows: serde_json::Value = serde_json::from_str(&stdout(&output)).unwrap();
    assert_eq!(rows.as_array().unwrap().len(), 1);
    assert_eq!(rows[0]["subject"], "Target");
    assert_eq!(rows[0]["message_id"], "<7@example.com>");
}

#[test]
fn receipts_count_only_messages_the_server_still_has() {
    let mut state = State::new("IMAP4rev1 MOVE")
        .mailbox(
            "INBOX",
            &[Some(10)],
            &[message(7, "Target"), message(8, "Target")],
        )
        .mailbox("Archive", &[Some(30)], &[]);
    // Another client expunges UID 8 between the search and the move.
    state.mailboxes[0].vanish.push((2, 8));
    let (output, state) = run(
        state,
        &["move", "--subject", "Target", "--dest", "Archive", "--yes"],
    );
    assert_success(&output);
    assert_eq!(mutations(&state.commands), [r#"UID MOVE 7 "Archive""#]);
    let text = stdout(&output);
    assert!(text.contains("Moved 1 message(s) to Archive."), "{text}");
    assert!(text.contains("1 message(s) no longer existed"), "{text}");

    // A failure in a later folder reports the updates already made.
    let (output, state) = run(
        State::new("IMAP4rev1")
            .mailbox("INBOX", &[Some(10)], &[message(7, "Target")])
            .mailbox("Work", &[Some(20)], &[message(8, "Target")])
            .fail("UID STORE 8 ", "store refused"),
        &[
            "mark",
            "--all-folders",
            "--subject",
            "Target",
            "--read",
            "--yes",
        ],
    );
    assert_failure(&output, "(1 already updated)");
    assert_eq!(state.flags("INBOX", 7), (true, false));
    assert_eq!(state.flags("Work", 8), (false, false));

    // A second flag change failing after the first succeeded is reported.
    let (output, state) = run(
        State::new("IMAP4rev1")
            .mailbox("INBOX", &[Some(10)], &[message(7, "Target")])
            .fail("UID STORE 7 +FLAGS (\\Flagged)", "store refused"),
        &[
            "mark",
            "--subject",
            "Target",
            "--read",
            "--set-flagged",
            "--yes",
        ],
    );
    assert_failure(&output, "(0 already updated; 1 more partly updated)");
    assert_eq!(state.flags("INBOX", 7), (true, false));

    // Without failures every requested change is applied to the matches only.
    let (output, state) = run(
        State::new("IMAP4rev1").mailbox(
            "INBOX",
            &[Some(10)],
            &[message(7, "Target"), message(9, "Other")],
        ),
        &[
            "mark",
            "--subject",
            "Target",
            "--read",
            "--set-flagged",
            "--yes",
        ],
    );
    assert_success(&output);
    assert!(stdout(&output).contains("Updated 1 message(s)."));
    assert_eq!(state.flags("INBOX", 7), (true, true));
    assert_eq!(state.flags("INBOX", 9), (false, false));
}

#[test]
fn empty_text_filters_are_rejected_before_opening_a_mailbox() {
    for args in [
        vec!["delete", "--from", "", "--yes"],
        vec!["mark", "--subject", "\u{1b}", "--read", "--yes"],
        vec!["search", "--to", "  "],
    ] {
        let (output, state) = run(
            inbox_with_target_and_deleted_neighbor("IMAP4rev1 MOVE"),
            &args,
        );
        assert_failure(&output, "must not be empty");
        assert!(
            !state.commands.iter().any(|c| opens_mailbox(c)),
            "{args:?}: {:?}",
            state.commands
        );
    }
}

#[test]
fn moves_never_take_messages_from_their_destination() {
    let state = || {
        State::new("IMAP4rev1 MOVE")
            .mailbox("INBOX", &[Some(10)], &[message(7, "Target")])
            .mailbox("Archive", &[Some(30)], &[message(3, "Target")])
            .special("Deleted Items", "\\Trash")
    };
    let (output, server) = run(
        state(),
        &[
            "move",
            "--all-folders",
            "--subject",
            "Target",
            "--dest",
            "Archive",
            "--dry-run",
        ],
    );
    assert_success(&output);
    assert!(stdout(&output).contains("1 message(s) would be moved"));
    // A dry run only examines mailboxes.
    let opened: Vec<&String> = server
        .commands
        .iter()
        .filter(|c| opens_mailbox(c))
        .collect();
    assert_eq!(opened, [r#"EXAMINE "INBOX""#]);

    let (output, server) = run(
        state(),
        &[
            "move",
            "--all-folders",
            "--subject",
            "Target",
            "--dest",
            "Archive",
            "--yes",
        ],
    );
    assert_success(&output);
    assert_eq!(mutations(&server.commands), [r#"UID MOVE 7 "Archive""#]);
    assert_eq!(server.uids("Archive"), [3, 4]);

    let (output, server) = run(
        state(),
        &[
            "move",
            "-f",
            "Archive",
            "--subject",
            "Target",
            "--dest",
            "Archive",
            "--yes",
        ],
    );
    assert_failure(&output, "Source and destination folder are the same");
    assert!(mutations(&server.commands).is_empty());
}

#[test]
fn folder_and_all_folders_conflict_without_connecting() {
    let server = Server::start(State::new("IMAP4rev1 MOVE"));
    let dir = tempfile::tempdir().unwrap();
    let output = slashmail(
        server.port,
        dir.path(),
        &["delete", "-f", "Newsletters", "--all-folders", "--yes"],
    );
    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
    assert!(
        stderr(&output).contains("--all-folders"),
        "{}",
        stderr(&output)
    );
    assert!(server.finish().commands.is_empty());
}
