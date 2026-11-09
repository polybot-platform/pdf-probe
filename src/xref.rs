//! Classic cross-reference tables and the `/Prev` chain. Xref streams, hybrid files and recovery are not
//! supported yet: they fail closed.
use crate::lexer::{Dict, Lexer, Obj};
use crate::{Failure, Limits};
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, Copy)]
pub(crate) enum Entry {
    Free,
    Offset(u64),
}

pub(crate) struct Xref {
    pub entries: HashMap<u32, Entry>,
    /// The newest trailer: `/Root` and friends.
    pub trailer: Dict,
}

fn fail<T>(why: &'static str) -> Result<T, Failure> {
    Err(Failure::Unparseable(why))
}

/// The byte offset named by the last `startxref` in the final 1 KiB of the file.
fn find_startxref(data: &[u8]) -> Result<usize, Failure> {
    let tail_start = data.len().saturating_sub(1024);
    let tail = &data[tail_start..];
    let key = b"startxref";
    let at = tail.windows(key.len()).rposition(|w| w == key);
    let Some(at) = at else {
        return fail("no startxref");
    };
    let mut lx = Lexer::new(data, tail_start + at + key.len());
    let Some(offset) = lx.read_uint() else {
        return fail("bad startxref");
    };
    usize::try_from(offset).or(fail("bad startxref"))
}

pub(crate) fn read(data: &[u8], limits: &Limits) -> Result<Xref, Failure> {
    let mut offset = find_startxref(data)?;
    let mut entries: HashMap<u32, Entry> = HashMap::new();
    let mut newest: Option<Dict> = None;
    let mut seen = HashSet::new();
    loop {
        if !seen.insert(offset) {
            return fail("xref chain loops");
        }
        if seen.len() > limits.max_xref_sections {
            return fail("xref chain too long");
        }
        let trailer = read_section(data, offset, &mut entries, limits)?;
        if trailer.get(b"XRefStm").is_some() {
            return fail("hybrid files are not supported yet");
        }
        let prev = trailer.get(b"Prev").cloned();
        newest.get_or_insert(trailer);
        match prev {
            None => break,
            Some(p) => match p.as_int().and_then(|n| usize::try_from(n).ok()) {
                Some(n) => offset = n,
                None => return fail("bad /Prev"),
            },
        }
    }
    let trailer = newest.ok_or(Failure::Unparseable("no trailer"))?;
    if trailer.get(b"Encrypt").is_some() {
        return fail("encryption is not supported yet");
    }
    Ok(Xref { entries, trailer })
}

/// Reads one classic section at `offset`, adding entries not already known (the newest section wins).
fn read_section(
    data: &[u8],
    offset: usize,
    entries: &mut HashMap<u32, Entry>,
    limits: &Limits,
) -> Result<Dict, Failure> {
    if offset >= data.len() {
        return fail("xref offset past the end");
    }
    let mut lx = Lexer::new(data, offset);
    if !lx.eat_keyword(b"xref") {
        return fail("xref streams are not supported yet");
    }
    loop {
        if lx.eat_keyword(b"trailer") {
            break;
        }
        let (Some(first), Some(count)) = (lx.read_uint(), lx.read_uint()) else {
            return fail("bad xref subsection");
        };
        for i in 0..count {
            if entries.len() >= limits.max_objects {
                return fail("too many xref entries");
            }
            let (Some(off), Some(_gen)) = (lx.read_uint(), lx.read_uint()) else {
                return fail("bad xref entry");
            };
            let entry = if lx.eat_keyword(b"n") {
                Entry::Offset(off)
            } else if lx.eat_keyword(b"f") {
                Entry::Free
            } else {
                return fail("bad xref entry");
            };
            let Some(num) = first.checked_add(i).and_then(|n| u32::try_from(n).ok()) else {
                return fail("bad xref object number");
            };
            entries.entry(num).or_insert(entry);
        }
    }
    match lx.parse_obj() {
        Some(Obj::Dict(d)) => Ok(d),
        _ => fail("bad trailer"),
    }
}
