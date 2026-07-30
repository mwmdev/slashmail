use std::collections::{HashMap, HashSet};
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use anyhow::{anyhow, bail, Context, Result};
use comfy_table::{presets::UTF8_FULL_CONDENSED, Cell, ContentArrangement, Table};
use mailparse::DispositionType;
use serde::Serialize;

use crate::display;

const MAX_FILENAME_BYTES: usize = 200;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct PartId(Vec<usize>);

impl fmt::Display for PartId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, segment) in self.0.iter().enumerate() {
            if index > 0 {
                formatter.write_str(".")?;
            }
            write!(formatter, "{segment}")?;
        }
        Ok(())
    }
}

impl FromStr for PartId {
    type Err = String;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        if value.is_empty() {
            return Err("part ID cannot be empty".to_string());
        }

        let mut segments = Vec::new();
        for segment in value.split('.') {
            if segment.is_empty()
                || !segment.bytes().all(|byte| byte.is_ascii_digit())
                || segment.starts_with('0')
            {
                return Err(format!("invalid attachment part ID '{value}'"));
            }
            let number = segment
                .parse::<usize>()
                .map_err(|_| format!("invalid attachment part ID '{value}'"))?;
            if number == 0 {
                return Err(format!("invalid attachment part ID '{value}'"));
            }
            segments.push(number);
        }

        Ok(Self(segments))
    }
}

#[derive(Clone, Debug)]
pub struct ReceivedAttachment {
    pub part: PartId,
    pub filename: String,
    pub content_type: String,
    bytes: Vec<u8>,
}

impl ReceivedAttachment {
    pub fn size(&self) -> u64 {
        self.bytes.len() as u64
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SavedAttachment {
    pub part: PartId,
    pub path: PathBuf,
    pub size: u64,
}

pub(crate) fn is_attachment(part: &mailparse::ParsedMail<'_>) -> bool {
    part.get_content_disposition().disposition == DispositionType::Attachment
}

pub fn is_unsafe_filename_character(character: char) -> bool {
    character.is_control()
        || matches!(
            character,
            '\u{061c}'
                | '\u{200e}'
                | '\u{200f}'
                | '\u{2028}'..='\u{202e}'
                | '\u{2066}'..='\u{2069}'
        )
}

fn attachment_filename(part: &mailparse::ParsedMail<'_>) -> String {
    let disposition = part.get_content_disposition();
    disposition
        .params
        .get("filename")
        .cloned()
        .or_else(|| part.ctype.params.get("name").cloned())
        .unwrap_or_else(|| "unnamed".to_string())
}

fn walk_attachment_parts<'a, F>(parsed: &'a mailparse::ParsedMail<'a>, mut visit: F)
where
    F: FnMut(PartId, &'a mailparse::ParsedMail<'a>),
{
    fn walk<'a, F>(part: &'a mailparse::ParsedMail<'a>, path: &mut Vec<usize>, visit: &mut F)
    where
        F: FnMut(PartId, &'a mailparse::ParsedMail<'a>),
    {
        if is_attachment(part) {
            visit(PartId(path.clone()), part);
            return;
        }

        for (index, child) in part.subparts.iter().enumerate() {
            path.push(index + 1);
            walk(child, path, visit);
            path.pop();
        }
    }

    if is_attachment(parsed) {
        visit(PartId(vec![1]), parsed);
        return;
    }

    for (index, child) in parsed.subparts.iter().enumerate() {
        let mut path = vec![index + 1];
        walk(child, &mut path, &mut visit);
    }
}

pub fn attachment_names(parsed: &mailparse::ParsedMail<'_>) -> Vec<String> {
    let mut names = Vec::new();
    walk_attachment_parts(parsed, |_, part| names.push(attachment_filename(part)));
    names
}

pub fn attachments_from_message(raw: &[u8]) -> Result<Vec<ReceivedAttachment>> {
    let parsed = mailparse::parse_mail(raw).context("Failed to parse message MIME")?;
    let mut attachments = Vec::new();
    let mut decode_error = None;

    walk_attachment_parts(&parsed, |part, parsed_part| {
        if decode_error.is_some() {
            return;
        }
        let filename = attachment_filename(parsed_part);
        match parsed_part.get_body_raw() {
            Ok(bytes) => attachments.push(ReceivedAttachment {
                part,
                filename,
                content_type: parsed_part.ctype.mimetype.to_lowercase(),
                bytes,
            }),
            Err(error) => {
                let safe_filename = display::sanitize_terminal_field(&filename);
                decode_error = Some(anyhow!(error).context(format!(
                    "Failed to decode attachment part {part} ('{safe_filename}')"
                )));
            }
        }
    });

    if let Some(error) = decode_error {
        return Err(error);
    }
    Ok(attachments)
}

pub fn render_attachment_table(attachments: &[ReceivedAttachment]) -> Option<String> {
    if attachments.is_empty() {
        return None;
    }

    let mut table = Table::new();
    table.load_preset(UTF8_FULL_CONDENSED);
    table.set_content_arrangement(ContentArrangement::Dynamic);
    table.set_header(["Part", "Filename", "Type", "Size"]);
    for attachment in attachments {
        table.add_row([
            Cell::new(attachment.part.to_string()),
            Cell::new(display::sanitize_terminal_field(&attachment.filename)),
            Cell::new(display::sanitize_terminal_field(&attachment.content_type)),
            Cell::new(display::format_size(attachment.size())),
        ]);
    }
    Some(table.to_string())
}

#[derive(Serialize)]
struct AttachmentJson<'a> {
    part: String,
    filename: &'a str,
    content_type: &'a str,
    size: u64,
}

pub fn render_attachments_json(attachments: &[ReceivedAttachment]) -> Result<String> {
    let rows = attachments
        .iter()
        .map(|attachment| AttachmentJson {
            part: attachment.part.to_string(),
            filename: &attachment.filename,
            content_type: &attachment.content_type,
            size: attachment.size(),
        })
        .collect::<Vec<_>>();
    serde_json::to_string(&rows).context("Failed to serialize attachments as JSON")
}

pub fn render_saved_receipt(saved: &SavedAttachment) -> String {
    format!(
        "Saved attachment: Part={} | File={} | Size={}",
        saved.part,
        safe_path(&saved.path),
        saved.size
    )
}

fn safe_path(path: &Path) -> String {
    display::sanitize_terminal_field(&path.as_os_str().to_string_lossy())
}

fn is_windows_invalid(character: char) -> bool {
    matches!(
        character,
        '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*'
    )
}

fn is_windows_device_name(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or(name);
    let upper = stem.to_ascii_uppercase();
    matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || upper
            .strip_prefix("COM")
            .or_else(|| upper.strip_prefix("LPT"))
            .is_some_and(|number| {
                matches!(number, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9")
            })
}

fn truncate_utf8(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_string();
    }
    let mut boundary = max_bytes;
    while !value.is_char_boundary(boundary) {
        boundary -= 1;
    }
    value[..boundary].to_string()
}

fn split_extension(name: &str) -> (&str, &str) {
    match name.rfind('.') {
        Some(index) if index > 0 && name.len() - index <= 32 => (&name[..index], &name[index..]),
        _ => (name, ""),
    }
}

fn filename_with_suffix(name: &str, duplicate: Option<usize>) -> String {
    let (stem, extension) = split_extension(name);
    let suffix = duplicate
        .map(|number| format!(" ({number})"))
        .unwrap_or_default();
    let max_stem = MAX_FILENAME_BYTES - extension.len() - suffix.len();
    format!("{}{}{}", truncate_utf8(stem, max_stem), suffix, extension)
}

fn sanitized_filename(attachment: &ReceivedAttachment) -> String {
    let component = attachment
        .filename
        .split(['/', '\\'])
        .rfind(|component| !component.is_empty())
        .unwrap_or("");
    let mut name = component
        .chars()
        .map(|character| {
            if is_unsafe_filename_character(character) || is_windows_invalid(character) {
                '_'
            } else {
                character
            }
        })
        .collect::<String>();
    name.truncate(name.trim_end_matches([' ', '.']).len());

    if name.is_empty() || matches!(name.as_str(), "." | "..") {
        name = format!("attachment-{}", attachment.part);
    }
    if name.starts_with('.') || is_windows_device_name(&name) {
        name.insert(0, '_');
    }
    filename_with_suffix(&name, None)
}

fn selected_attachments<'a>(
    attachments: &'a [ReceivedAttachment],
    selected: &[PartId],
) -> Result<Vec<&'a ReceivedAttachment>> {
    if selected.is_empty() {
        return Ok(attachments.iter().collect());
    }

    let by_part = attachments
        .iter()
        .map(|attachment| (&attachment.part, attachment))
        .collect::<HashMap<_, _>>();
    let mut seen = HashSet::new();
    let mut resolved = Vec::with_capacity(selected.len());
    for part in selected {
        if !seen.insert(part) {
            bail!("Attachment part {part} was selected more than once");
        }
        let attachment = by_part
            .get(part)
            .copied()
            .ok_or_else(|| anyhow!("Attachment part {part} was not found"))?;
        resolved.push(attachment);
    }
    Ok(resolved)
}

fn plan_destinations<'a>(
    attachments: &'a [ReceivedAttachment],
    selected: &[PartId],
    output_dir: &Path,
) -> Result<Vec<(&'a ReceivedAttachment, PathBuf)>> {
    let selected = selected_attachments(attachments, selected)?;
    let mut allocated = HashSet::new();
    let mut planned = Vec::with_capacity(selected.len());

    for attachment in selected {
        let base = sanitized_filename(attachment);
        let mut duplicate = None;
        let filename = loop {
            let candidate = filename_with_suffix(&base, duplicate);
            if allocated.insert(candidate.to_lowercase()) {
                break candidate;
            }
            duplicate = Some(duplicate.map_or(2, |number| number + 1));
        };
        planned.push((attachment, output_dir.join(filename)));
    }
    Ok(planned)
}

pub fn save_attachments(
    attachments: &[ReceivedAttachment],
    selected: &[PartId],
    output_dir: &Path,
    force: bool,
) -> Result<Vec<SavedAttachment>> {
    let planned = plan_destinations(attachments, selected, output_dir)?;
    if planned.is_empty() {
        return Ok(Vec::new());
    }

    fs::create_dir_all(output_dir).with_context(|| {
        format!(
            "Failed to create attachment output directory {}",
            safe_path(output_dir)
        )
    })?;

    for (_, path) in &planned {
        match fs::symlink_metadata(path) {
            Ok(_) if !force => {
                bail!(
                    "Attachment destination already exists: {}; use --force to replace it",
                    safe_path(path)
                );
            }
            Ok(metadata)
                if !metadata.file_type().is_file() && !metadata.file_type().is_symlink() =>
            {
                bail!(
                    "Attachment destination is not a replaceable file: {}",
                    safe_path(path)
                );
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "Failed to inspect attachment destination {}",
                        safe_path(path)
                    )
                });
            }
        }
    }

    let mut saved = Vec::with_capacity(planned.len());
    for (attachment, path) in planned {
        if force {
            match fs::symlink_metadata(&path) {
                Ok(metadata)
                    if metadata.file_type().is_file() || metadata.file_type().is_symlink() =>
                {
                    fs::remove_file(&path).with_context(|| {
                        format!(
                            "Failed to replace attachment destination {}",
                            safe_path(&path)
                        )
                    })?;
                }
                Ok(_) => {
                    bail!(
                        "Attachment destination is not a replaceable file: {}",
                        safe_path(&path)
                    );
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(error).with_context(|| {
                        format!(
                            "Failed to inspect attachment destination {}",
                            safe_path(&path)
                        )
                    });
                }
            }
        }

        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .with_context(|| format!("Failed to create attachment {}", safe_path(&path)))?;
        if let Err(error) = file.write_all(&attachment.bytes) {
            drop(file);
            let _ = fs::remove_file(&path);
            return Err(error)
                .with_context(|| format!("Failed to write attachment {}", safe_path(&path)));
        }
        saved.push(SavedAttachment {
            part: attachment.part.clone(),
            path,
            size: attachment.size(),
        });
    }

    Ok(saved)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn part(value: &str) -> PartId {
        value.parse().unwrap()
    }

    fn received(id: &str, filename: &str, bytes: &[u8]) -> ReceivedAttachment {
        ReceivedAttachment {
            part: part(id),
            filename: filename.to_string(),
            content_type: "application/octet-stream".to_string(),
            bytes: bytes.to_vec(),
        }
    }

    #[test]
    fn part_ids_are_canonical_one_based_paths() {
        for valid in ["1", "2", "2.1"] {
            assert_eq!(valid.parse::<PartId>().unwrap().to_string(), valid);
        }
        for invalid in [
            "", "0", "0.1", "1.0", ".1", "1.", "1..2", "01", "1.02", "+1", "-1", " 1", "1 ",
        ] {
            assert!(invalid.parse::<PartId>().is_err(), "{invalid}");
        }
    }

    #[test]
    fn nested_mime_attachments_have_stable_ids_names_types_and_sizes() {
        let raw = b"Content-Type: multipart/mixed; boundary=outer\r\n\
\r\n\
--outer\r\n\
Content-Type: multipart/mixed; boundary=inner\r\n\
\r\n\
--inner\r\n\
Content-Type: text/plain\r\n\
\r\n\
body\r\n\
--inner\r\n\
Content-Type: application/octet-stream\r\n\
Content-Disposition: attachment; filename*=utf-8''r%C3%A9sum%C3%A9.bin\r\n\
Content-Transfer-Encoding: base64\r\n\
\r\n\
AAE=\r\n\
--inner--\r\n\
--outer\r\n\
Content-Type: IMAGE/PNG; name=legacy.png\r\n\
Content-Disposition: attachment\r\n\
\r\n\
png\r\n\
--outer\r\n\
Content-Type: application/pdf\r\n\
Content-Disposition: attachment\r\n\
\r\n\
x\r\n\
--outer\r\n\
Content-Type: image/jpeg; name=inline.jpg\r\n\
Content-Disposition: inline; filename=inline.jpg\r\n\
\r\n\
jpeg\r\n\
--outer--\r\n";

        let attachments = attachments_from_message(raw).unwrap();
        assert_eq!(
            attachments
                .iter()
                .map(|attachment| attachment.part.to_string())
                .collect::<Vec<_>>(),
            ["1.2", "2", "3"]
        );
        assert_eq!(
            attachments
                .iter()
                .map(|attachment| attachment.filename.as_str())
                .collect::<Vec<_>>(),
            ["résumé.bin", "legacy.png", "unnamed"]
        );
        assert_eq!(
            attachments
                .iter()
                .map(|attachment| attachment.content_type.as_str())
                .collect::<Vec<_>>(),
            ["application/octet-stream", "image/png", "application/pdf"]
        );
        assert_eq!(
            attachments
                .iter()
                .map(ReceivedAttachment::size)
                .collect::<Vec<_>>(),
            [2, 5, 3]
        );

        let parsed = mailparse::parse_mail(raw).unwrap();
        assert_eq!(
            attachment_names(&parsed),
            ["résumé.bin", "legacy.png", "unnamed"]
        );
    }

    #[test]
    fn binary_transfer_decoding_is_byte_exact_and_errors_are_contextual() {
        let raw = b"Content-Type: application/octet-stream\r\n\
Content-Disposition: attachment; filename=data.bin\r\n\
Content-Transfer-Encoding: base64\r\n\
\r\n\
AP8RgA0K";
        let attachments = attachments_from_message(raw).unwrap();
        assert_eq!(attachments[0].bytes, [0x00, 0xff, 0x11, 0x80, 0x0d, 0x0a]);

        let malformed = b"Content-Type: application/octet-stream\r\n\
Content-Disposition: attachment; filename=bad.bin\r\n\
Content-Transfer-Encoding: base64\r\n\
\r\n\
%%%";
        let error = attachments_from_message(malformed).unwrap_err().to_string();
        assert!(error.contains("part 1"));
        assert!(error.contains("bad.bin"));
    }

    #[test]
    fn renderers_sanitize_human_fields_and_preserve_json_data() {
        let attachments = vec![ReceivedAttachment {
            part: part("2.1"),
            filename: "safe\u{1b}]52;c;secret\u{7}\u{202e}name".to_string(),
            content_type: "application/\u{1b}[31mevil".to_string(),
            bytes: vec![1, 2, 3],
        }];

        let table = render_attachment_table(&attachments).unwrap();
        assert!(table.contains("Part"));
        assert!(table.contains("Filename"));
        assert!(table.contains("Type"));
        assert!(table.contains("Size"));
        assert!(!table.contains("secret"));
        assert!(!table.contains('\u{1b}'));
        assert!(!table.contains('\u{202e}'));

        let json = render_attachments_json(&attachments).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        let object = value[0].as_object().unwrap();
        assert_eq!(object.len(), 4);
        assert_eq!(object["part"], "2.1");
        assert_eq!(object["filename"], attachments[0].filename);
        assert_eq!(object["content_type"], attachments[0].content_type);
        assert_eq!(object["size"], 3);
        assert_eq!(render_attachments_json(&[]).unwrap(), "[]");
        assert!(render_attachment_table(&[]).is_none());
    }

    #[test]
    fn planned_names_are_direct_bounded_nonhidden_and_case_unique() {
        let overlong = "éééééééééé".repeat(30);
        let names = [
            "../escape",
            "a/b",
            "a\\b",
            ".",
            "..",
            ".hidden",
            "",
            "///",
            overlong.as_str(),
            "bad\u{1b}\u{202e}.txt",
            "CON.txt",
            "CON.tar.gz",
            "con.TXT",
            "same.txt",
            "SAME.TXT",
        ];
        let attachments = names
            .iter()
            .enumerate()
            .map(|(index, name)| received(&(index + 1).to_string(), name, b""))
            .collect::<Vec<_>>();
        let output = Path::new("output");
        let planned = plan_destinations(&attachments, &[], output).unwrap();
        let mut lowercase = HashSet::new();

        for (_, path) in &planned {
            assert_eq!(path.parent(), Some(output));
            let name = path.file_name().unwrap().to_str().unwrap();
            assert!(!name.starts_with('.'), "{name}");
            assert!(name.len() <= MAX_FILENAME_BYTES, "{name}");
            assert!(lowercase.insert(name.to_lowercase()), "{name}");
        }
        assert_eq!(planned[0].1.file_name().unwrap(), "escape");
        assert_eq!(planned[1].1.file_name().unwrap(), "b");
        assert_eq!(planned[2].1.file_name().unwrap(), "b (2)");
        assert_eq!(planned[5].1.file_name().unwrap(), "_.hidden");
        assert_eq!(planned[10].1.file_name().unwrap(), "_CON.txt");
        assert_eq!(planned[11].1.file_name().unwrap(), "_CON.tar.gz");
        assert_eq!(planned[12].1.file_name().unwrap(), "_con (2).TXT");
        assert_eq!(planned[14].1.file_name().unwrap(), "SAME (2).TXT");
    }

    #[test]
    fn selector_errors_create_nothing() {
        let directory = tempfile::tempdir().unwrap();
        let missing = directory.path().join("missing");
        let attachments = vec![received("1", "one.bin", b"one")];

        assert!(save_attachments(&attachments, &[part("2")], &missing, false).is_err());
        assert!(!missing.exists());
        assert!(save_attachments(&attachments, &[part("1"), part("1")], &missing, false).is_err());
        assert!(!missing.exists());
    }

    #[test]
    fn collision_preflight_prevents_partial_writes_and_force_is_explicit() {
        let directory = tempfile::tempdir().unwrap();
        let attachments = vec![
            received("1", "one.bin", b"one"),
            received("2", "two.bin", b"two"),
        ];
        fs::write(directory.path().join("two.bin"), b"sentinel").unwrap();

        let error = save_attachments(&attachments, &[], directory.path(), false)
            .unwrap_err()
            .to_string();
        assert!(error.contains("already exists"));
        assert!(!directory.path().join("one.bin").exists());
        assert_eq!(
            fs::read(directory.path().join("two.bin")).unwrap(),
            b"sentinel"
        );

        let saved = save_attachments(&attachments, &[], directory.path(), true).unwrap();
        assert_eq!(saved.len(), 2);
        assert_eq!(fs::read(directory.path().join("one.bin")).unwrap(), b"one");
        assert_eq!(fs::read(directory.path().join("two.bin")).unwrap(), b"two");
    }

    #[test]
    fn force_replaces_symlinks_but_rejects_nonfiles() {
        let directory = tempfile::tempdir().unwrap();
        let attachment = received("1", "target.bin", &[0x00, 0xff, 0x11]);
        let destination = directory.path().join("target.bin");

        fs::create_dir(&destination).unwrap();
        let error = save_attachments(
            std::slice::from_ref(&attachment),
            &[],
            directory.path(),
            true,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("not a replaceable file"));
        fs::remove_dir(&destination).unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::net::UnixListener;

            let listener = UnixListener::bind(&destination).unwrap();
            let error = save_attachments(
                std::slice::from_ref(&attachment),
                &[],
                directory.path(),
                true,
            )
            .unwrap_err()
            .to_string();
            assert!(error.contains("not a replaceable file"));
            drop(listener);
            fs::remove_file(&destination).unwrap();

            std::os::unix::fs::symlink(directory.path().join("outside"), &destination).unwrap();
            save_attachments(
                std::slice::from_ref(&attachment),
                &[],
                directory.path(),
                true,
            )
            .unwrap();
            assert!(!fs::symlink_metadata(&destination)
                .unwrap()
                .file_type()
                .is_symlink());
        }

        assert_eq!(fs::read(destination).unwrap(), [0x00, 0xff, 0x11]);
    }
}
