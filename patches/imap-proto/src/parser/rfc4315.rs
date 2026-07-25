//!
//! RFC 4315 UIDPLUS response-code parsing.
//!

#![cfg_attr(rustfmt, rustfmt_skip)]

use crate::core::number;
use crate::types::{ResponseCode, UidSet, UidSetMember};

fn nonzero(value: u32) -> Result<u32, ()> {
    if value == 0 {
        Err(())
    } else {
        Ok(value)
    }
}

named!(nz_number<u32>, map_res!(number, nonzero));

named!(uid_set_member<UidSetMember>, do_parse!(
    start: nz_number >>
    end: opt!(preceded!(tag!(":"), nz_number)) >>
    (match end {
        Some(end) => UidSetMember::Range(start, end),
        None => UidSetMember::Uid(start),
    })
));

named!(uid_set<UidSet>, map!(
    separated_nonempty_list!(tag!(","), uid_set_member),
    UidSet
));

named!(pub(crate) resp_text_code_append_uid<ResponseCode>, do_parse!(
    tag_no_case!("APPENDUID ") >>
    uid_validity: nz_number >>
    tag!(" ") >>
    uids: uid_set >>
    (ResponseCode::AppendUid {
        uid_validity,
        uids,
    })
));
