//! IMAP modified UTF-7 mailbox names (RFC 3501 §5.1.3).
//!
//! Servers list non-ASCII mailbox names in this encoding (`Envoy&AOk-s`).
//! Slashmail keeps listed names unchanged internally and in JSON, decodes
//! them only for terminal display, and matches user-supplied names against
//! the listing with [`encode`] (see `MailboxListing::find`).

use std::borrow::Cow;

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+,";

fn is_printable_ascii(character: char) -> bool {
    (' '..='~').contains(&character)
}

/// Decode a wire name, or `None` when it is not canonical modified UTF-7 or
/// would display misleadingly. Rejected: shifted runs that encode printable
/// ASCII or control characters, runs with nothing visible (only spaces,
/// joiners, or invisible or bidi formatting), and adjacent runs (null
/// shifts). So a name with an encoded run never displays as a plain-ASCII
/// name. A rejected name is shown as listed, so on a server that also lists
/// names unencoded (raw UTF-8 or a bare `&`), two names can look the same;
/// likewise two non-ASCII names can look alike (homoglyphs, joiners next to
/// visible characters).
pub fn decode(name: &str) -> Option<String> {
    let mut decoded = String::with_capacity(name.len());
    let mut rest = name;
    while let Some(character) = rest.chars().next() {
        if !is_printable_ascii(character) {
            return None;
        }
        rest = &rest[1..];
        if character != '&' {
            decoded.push(character);
            continue;
        }
        let (run, after) = rest.split_once('-')?;
        if run.is_empty() {
            decoded.push('&');
        } else {
            decode_run(run.as_bytes(), &mut decoded)?;
            if after.starts_with('&') && !after.starts_with("&-") {
                return None;
            }
        }
        rest = after;
    }
    Some(decoded)
}

fn decode_run(run: &[u8], decoded: &mut String) -> Option<()> {
    let mut units = Vec::with_capacity(run.len() * 6 / 16);
    let mut bits: u32 = 0;
    let mut bit_count = 0;
    for &byte in run {
        let value = ALPHABET.iter().position(|&symbol| symbol == byte)? as u32;
        bits = (bits << 6) | value;
        bit_count += 6;
        if bit_count >= 16 {
            bit_count -= 16;
            units.push((bits >> bit_count) as u16);
            bits &= (1 << bit_count) - 1;
        }
    }
    // Only zero padding bits, fewer than one base64 digit, may remain.
    if bit_count >= 6 || bits != 0 {
        return None;
    }
    let text = char::decode_utf16(units)
        .collect::<Result<String, _>>()
        .ok()?;
    if text
        .chars()
        .any(|character| is_printable_ascii(character) || character.is_control())
        || text.chars().all(crate::display::renders_blank)
    {
        return None;
    }
    decoded.push_str(&text);
    Some(())
}

/// Encode a name: printable ASCII is kept (`&` becomes `&-`) and every other
/// run becomes base64 UTF-16.
pub fn encode(name: &str) -> String {
    let mut encoded = String::with_capacity(name.len());
    let mut pending = Vec::new();
    for character in name.chars() {
        if is_printable_ascii(character) {
            flush_run(&mut pending, &mut encoded);
            match character {
                '&' => encoded.push_str("&-"),
                _ => encoded.push(character),
            }
        } else {
            let mut buffer = [0; 2];
            pending.extend_from_slice(character.encode_utf16(&mut buffer));
        }
    }
    flush_run(&mut pending, &mut encoded);
    encoded
}

fn flush_run(units: &mut Vec<u16>, encoded: &mut String) {
    if units.is_empty() {
        return;
    }
    encoded.push('&');
    let mut bits: u32 = 0;
    let mut bit_count = 0;
    for unit in units.drain(..) {
        bits = (bits << 16) | u32::from(unit);
        bit_count += 16;
        while bit_count >= 6 {
            bit_count -= 6;
            encoded.push(ALPHABET[((bits >> bit_count) & 0x3f) as usize] as char);
        }
        bits &= (1 << bit_count) - 1;
    }
    if bit_count > 0 {
        encoded.push(ALPHABET[((bits << (6 - bit_count)) & 0x3f) as usize] as char);
    }
    encoded.push('-');
}

/// A listed name for display: decoded when valid, otherwise unchanged.
pub fn display(name: &str) -> Cow<'_, str> {
    match decode(name) {
        Some(decoded) => Cow::Owned(decoded),
        None => Cow::Borrowed(name),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc_3501_example_round_trips() {
        let wire = "~peter/mail/&U,BTFw-/&ZeVnLIqe-";
        let text = "~peter/mail/台北/日本語";
        assert_eq!(decode(wire).as_deref(), Some(text));
        assert_eq!(encode(text), wire);
    }

    #[test]
    fn padding_lengths_and_surrogate_pairs_round_trip() {
        for text in [
            "é",
            "Envoyés",
            "éé",
            "ééé",
            "R&D",
            "&",
            "é&",
            "📧 Mail",
            "a😀b€",
            "❤\u{fe0f} Family",
        ] {
            let wire = encode(text);
            assert!(wire.is_ascii(), "{wire}");
            assert_eq!(decode(&wire).as_deref(), Some(text), "{wire}");
        }
        assert_eq!(encode("Messages envoyés"), "Messages envoy&AOk-s");
        assert_eq!(encode("R&D"), "R&-D");
        assert_eq!(encode("é&"), "&AOk-&-");
    }

    #[test]
    fn invalid_or_non_canonical_names_do_not_decode() {
        for wire in [
            "&AOk",        // unterminated shift
            "&AOk!-",      // character outside the alphabet
            "&AOl-",       // nonzero padding bits
            "&AOkA-",      // a whole spare base64 digit
            "&2D0-",       // lone high surrogate
            "&AFQ-rash",   // encodes printable ASCII ("T")
            "&ABs-AINBOX", // encodes ESC, which would hide "A"
            "&AJs-@Trash", // encodes C1 CSI, which would hide "@"
            "&AOk-&AOk-",  // null shift between runs
            "Envoyés",     // raw non-ASCII
            "Bad\rName",   // control character
        ] {
            assert_eq!(decode(wire), None, "{wire:?}");
        }
    }

    #[test]
    fn runs_with_nothing_visible_do_not_decode() {
        // Each would otherwise display like "Trash" or "Tr ash".
        for text in [
            "Trash\u{200c}",
            "Tr\u{200b}ash",
            "\u{202e}Trash",
            "Trash\u{a0}",
            "Trash\u{3164}",
            "Tr\u{2800}ash",
            "Trash\u{180b}",
        ] {
            assert_eq!(decode(&encode(text)), None, "{text:?}");
            assert_eq!(display(&encode(text)), encode(text));
        }
    }

    #[test]
    fn display_falls_back_to_the_listed_name() {
        assert_eq!(display("Messages envoy&AOk-s"), "Messages envoyés");
        assert_eq!(display("&AFQ-rash"), "&AFQ-rash");
        assert_eq!(display("Ärger"), "Ärger");
    }
}
