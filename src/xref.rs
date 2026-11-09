//! Cross-reference data: classic tables, cross-reference streams, hybrid files and the `/Prev` chain.
//! A damaged or missing cross-reference (the recovery scan) is not supported yet: it fails closed.
use crate::lexer::{Dict, Lexer, Obj};
use crate::{Failure, Limits, stream};
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, Copy)]
pub(crate) enum Entry {
    Free,
    Offset(u64),
    /// Object `index` of the object stream numbered `stream`.
    InStream {
        stream: u32,
        index: u32,
    },
}

pub(crate) struct Xref {
    pub entries: HashMap<u32, Entry>,
    /// The newest trailer (the dictionary of the newest cross-reference stream, if it is one): `/Root` etc.
    pub trailer: Dict,
    /// Bytes inflated so far, counted against [`Limits::max_decompressed`].
    pub decoded: usize,
}

type Section = HashMap<u32, Entry>;

fn fail<T>(why: &'static str) -> Result<T, Failure> {
    Err(Failure::Unparseable(why))
}

/// The byte offset named by the last `startxref` in the final 1 KiB of the file.
fn find_startxref(data: &[u8]) -> Result<usize, Failure> {
    let tail_start = data.len().saturating_sub(1024);
    let key = b"startxref";
    let at = data[tail_start..]
        .windows(key.len())
        .rposition(|w| w == key);
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
    let mut decoded = 0usize;
    loop {
        if !seen.insert(offset) {
            return fail("xref chain loops");
        }
        if seen.len() > limits.max_xref_sections {
            return fail("xref chain too long");
        }
        let (section, trailer) = read_section(data, offset, limits, &mut decoded)?;
        for (num, entry) in section {
            if entries.len() >= limits.max_objects && !entries.contains_key(&num) {
                return fail("too many xref entries");
            }
            // The newest section wins, free entries included (a removed object stays removed).
            entries.entry(num).or_insert(entry);
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
    Ok(Xref {
        entries,
        trailer,
        decoded,
    })
}

/// One cross-reference section: a classic table (with its `/XRefStm`, for a hybrid file) or a stream.
fn read_section(
    data: &[u8],
    offset: usize,
    limits: &Limits,
    decoded: &mut usize,
) -> Result<(Section, Dict), Failure> {
    if offset >= data.len() {
        return fail("xref offset past the end");
    }
    let mut lx = Lexer::new(data, offset);
    if !lx.eat_keyword(b"xref") {
        return read_xref_stream(data, offset, limits, decoded);
    }
    let mut section = Section::new();
    loop {
        if lx.eat_keyword(b"trailer") {
            break;
        }
        let (Some(first), Some(count)) = (lx.read_uint(), lx.read_uint()) else {
            return fail("bad xref subsection");
        };
        for i in 0..count {
            if section.len() >= limits.max_objects {
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
            section.entry(num).or_insert(entry);
        }
    }
    let Some(Obj::Dict(trailer)) = lx.parse_obj() else {
        return fail("bad trailer");
    };
    // Hybrid file: the stream holds the entries for objects the table lists as free or leaves out. The
    // table wins for any object it lists as in use.
    if let Some(stm) = trailer.get(b"XRefStm") {
        let Some(at) = stm.as_int().and_then(|n| usize::try_from(n).ok()) else {
            return fail("bad /XRefStm");
        };
        let (from_stream, _) = read_xref_stream(data, at, limits, decoded)?;
        for (num, entry) in from_stream {
            if matches!(section.get(&num), None | Some(Entry::Free)) {
                section.insert(num, entry);
            }
        }
    }
    Ok((section, trailer))
}

/// A field of `width` bytes, big-endian. A zero width means the field is absent.
fn field(row: &[u8], at: usize, width: usize) -> u64 {
    row[at..at + width]
        .iter()
        .fold(0u64, |acc, &b| (acc << 8) | u64::from(b))
}

fn read_xref_stream(
    data: &[u8],
    offset: usize,
    limits: &Limits,
    decoded: &mut usize,
) -> Result<(Section, Dict), Failure> {
    let Some(ind) = Lexer::new(data, offset).parse_indirect_full() else {
        return fail("unreadable cross-reference");
    };
    let (Obj::Dict(dict), Some(range)) = (ind.obj, ind.stream) else {
        return fail("not a cross-reference stream");
    };
    if !matches!(dict.get(b"Type"), Some(Obj::Name(n)) if n == b"XRef") {
        return fail("not a cross-reference stream");
    }
    let remaining = limits.max_decompressed.saturating_sub(*decoded);
    let bytes = stream::decode(&dict, &data[range], remaining).map_err(Failure::Unparseable)?;
    *decoded += bytes.len();

    let widths: Vec<usize> = match dict.get(b"W") {
        Some(Obj::Array(a)) if a.len() == 3 => a
            .iter()
            .map(|o| {
                o.as_int()
                    .and_then(|n| usize::try_from(n).ok())
                    .filter(|n| *n <= 8)
            })
            .collect::<Option<_>>()
            .ok_or(Failure::Unparseable("bad /W"))?,
        _ => return fail("bad /W"),
    };
    let (w0, w1, w2) = (widths[0], widths[1], widths[2]);
    let row_len = w0 + w1 + w2;
    if row_len == 0 {
        return fail("bad /W");
    }
    let index: Vec<(u64, u64)> = match dict.get(b"Index") {
        None => {
            let size = dict
                .get(b"Size")
                .and_then(Obj::as_int)
                .and_then(|n| u64::try_from(n).ok());
            vec![(0, size.ok_or(Failure::Unparseable("no /Size"))?)]
        }
        Some(Obj::Array(a)) if a.len() % 2 == 0 => a
            .chunks(2)
            .map(|p| {
                Some((
                    u64::try_from(p[0].as_int()?).ok()?,
                    u64::try_from(p[1].as_int()?).ok()?,
                ))
            })
            .collect::<Option<_>>()
            .ok_or(Failure::Unparseable("bad /Index"))?,
        Some(_) => return fail("bad /Index"),
    };
    let mut section = Section::new();
    let mut rows = bytes.chunks_exact(row_len);
    for (first, count) in index {
        for i in 0..count {
            if section.len() >= limits.max_objects {
                return fail("too many xref entries");
            }
            let Some(row) = rows.next() else {
                return fail("cross-reference stream is too short");
            };
            let kind = if w0 == 0 { 1 } else { field(row, 0, w0) };
            let (f2, f3) = (field(row, w0, w1), field(row, w0 + w1, w2));
            let entry = match kind {
                1 => Entry::Offset(f2),
                2 => match (u32::try_from(f2), u32::try_from(f3)) {
                    (Ok(stream), Ok(index)) => Entry::InStream { stream, index },
                    _ => return fail("bad compressed xref entry"),
                },
                // Type 0 is free; unknown types are treated as null, as the specification says.
                _ => Entry::Free,
            };
            let Some(num) = first.checked_add(i).and_then(|n| u32::try_from(n).ok()) else {
                return fail("bad xref object number");
            };
            section.entry(num).or_insert(entry);
        }
    }
    Ok((section, dict))
}
