//! Cross-reference data: classic tables, cross-reference streams, hybrid files and the `/Prev` chain.
//! A damaged or missing cross-reference (the recovery scan) is not supported yet: it fails closed.
use crate::lexer::{Dict, Lexer, Obj};
use crate::meter::{Meter, cost};
use crate::{Failure, Limit, Limits, stream};
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

/// The offset after the last `startxref` anywhere in the file, read as pdf.js does: skip whitespace, take the
/// bytes from space to `9`, and parse the leading integer. Zero when there is none (which sends the reader to
/// the recovery scan).
fn find_startxref(data: &[u8]) -> usize {
    let key = b"startxref";
    let Some(at) = data.windows(key.len()).rposition(|w| w == key) else {
        return 0;
    };
    let mut p = at + key.len();
    while data.get(p).is_some_and(|b| matches!(b, 9 | 10 | 13 | 32)) {
        p += 1;
    }
    let text: Vec<u8> = data[p.min(data.len())..]
        .iter()
        .copied()
        .take_while(|b| (0x20..=0x39).contains(b))
        .collect();
    let text = String::from_utf8_lossy(&text);
    let digits: String = text
        .trim_start()
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse().unwrap_or(0)
}

pub(crate) fn read(data: &[u8], limits: &Limits, meter: &Meter) -> Result<Xref, Failure> {
    // Finding `startxref` searches backwards through the whole file.
    meter.charge(cost::SCAN * data.len() as u64)?;
    let mut offset = find_startxref(data);
    let mut entries: HashMap<u32, Entry> = HashMap::new();
    let mut newest: Option<Dict> = None;
    let mut seen = HashSet::new();
    let mut decoded = 0usize;
    loop {
        if !seen.insert(offset) {
            break; // already read: the chain ends here, as in pdf.js
        }
        if seen.len() > limits.max_xref_sections {
            return Err(Failure::LimitExceeded(Limit::XrefSections));
        }
        // A section that cannot be read ends the chain; the reader goes on with what it has, as pdf.js does.
        // With no usable section at all, the caller falls back to the recovery scan.
        let (section, trailer) = match read_section(data, offset, limits, &mut decoded, meter) {
            Ok(read) => read,
            Err(e) if newest.is_none() => return Err(e),
            Err(_) => break,
        };
        for (num, entry) in section {
            if entries.len() >= limits.max_objects && !entries.contains_key(&num) {
                return Err(Failure::LimitExceeded(Limit::Objects));
            }
            // The newest section wins, free entries included (a removed object stays removed).
            entries.entry(num).or_insert(entry);
        }
        let prev = prev_offset(&trailer);
        newest.get_or_insert(trailer);
        match prev {
            Some(n) => offset = n,
            None => break,
        }
    }
    let trailer = newest.ok_or(Failure::Unparseable("no trailer"))?;
    Ok(Xref {
        entries,
        trailer,
        decoded,
    })
}

/// `/Prev` as an offset. pdf.js also accepts the non-compliant `/Prev N 0 R`.
fn prev_offset(trailer: &Dict) -> Option<usize> {
    match trailer.get(b"Prev")? {
        Obj::Ref(n, _) => Some(*n as usize),
        other => other.as_int().and_then(|n| usize::try_from(n).ok()),
    }
}

/// One cross-reference section: a classic table (with its `/XRefStm`, for a hybrid file) or a stream.
fn read_section(
    data: &[u8],
    offset: usize,
    limits: &Limits,
    decoded: &mut usize,
    meter: &Meter,
) -> Result<(Section, Dict), Failure> {
    if offset >= data.len() {
        return fail("xref offset past the end");
    }
    let mut lx = Lexer::new(data, offset);
    if !lx.eat_keyword(b"xref") {
        return read_xref_stream(data, offset, limits, decoded, meter);
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
                return Err(Failure::LimitExceeded(Limit::Objects));
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
    let parsed = lx.parse_obj();
    meter.charge(cost::PARSE * (lx.pos() - offset) as u64)?;
    let Some(Obj::Dict(trailer)) = parsed else {
        return fail("bad trailer");
    };
    // Hybrid file: the stream holds the entries for objects the table lists as free or leaves out. The
    // table wins for any object it lists as in use.
    if let Some(stm) = trailer.get(b"XRefStm") {
        let Some(at) = stm.as_int().and_then(|n| usize::try_from(n).ok()) else {
            return fail("bad /XRefStm");
        };
        let (from_stream, _) = read_xref_stream(data, at, limits, decoded, meter)?;
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
    meter: &Meter,
) -> Result<(Section, Dict), Failure> {
    let Some(ind) = Lexer::new(data, offset).parse_indirect_full() else {
        return fail("unreadable cross-reference");
    };
    let (Obj::Dict(dict), Some(range)) = (ind.obj, ind.stream) else {
        return fail("not a cross-reference stream");
    };
    meter.charge(cost::SCAN * (range.end - offset) as u64)?;
    if !matches!(dict.get(b"Type"), Some(Obj::Name(n)) if n == b"XRef") {
        return fail("not a cross-reference stream");
    }
    let remaining = limits.max_decompressed.saturating_sub(*decoded);
    let bytes = stream::decode(&dict, &data[range], remaining, meter)?;
    *decoded += bytes.len();
    meter.charge(bytes.len() as u64)?; // reading the entries

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
                return Err(Failure::LimitExceeded(Limit::Objects));
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

/// What the recovery scan found.
pub(crate) struct Scan {
    pub entries: HashMap<u32, Entry>,
    /// Trailer dictionaries, in file order.
    pub trailers: Vec<Dict>,
    /// The first cross-reference dictionary read from a stream or table found by the scan.
    pub top: Option<Dict>,
    pub decoded: usize,
}

fn is_js_space(b: u8) -> bool {
    matches!(b, 9 | 10 | 11 | 12 | 13 | 32)
}

fn is_word(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// The regular-expression word boundary `\b` at `i`.
fn boundary(d: &[u8], i: usize) -> bool {
    let before = i > 0 && d.get(i - 1).is_some_and(|b| is_word(*b));
    let after = d.get(i).is_some_and(|b| is_word(*b));
    before != after
}

/// `\d+\s+\d+\s+obj\b` starting at `i`; the end of the match.
fn obj_header_at(d: &[u8], i: usize) -> Option<usize> {
    let run = |mut p: usize, f: fn(u8) -> bool| {
        let start = p;
        while d.get(p).is_some_and(|b| f(*b)) {
            p += 1;
        }
        (p > start).then_some(p)
    };
    let p = run(i, |b| b.is_ascii_digit())?;
    let p = run(p, is_js_space)?;
    let p = run(p, |b| b.is_ascii_digit())?;
    let p = run(p, is_js_space)?;
    if !d[p..].starts_with(b"obj") {
        return None;
    }
    boundary(d, p + 3).then_some(p + 3)
}

/// The first match of `\b(endobj|\d+\s+\d+\s+obj|xref|trailer\s*<<)\b` at or after `from`:
/// (start, end, it is `endobj`).
fn find_obj_end(d: &[u8], from: usize) -> Option<(usize, usize, bool)> {
    for i in from..d.len() {
        match d[i] {
            b'e' if d[i..].starts_with(b"endobj") && boundary(d, i) && boundary(d, i + 6) => {
                return Some((i, i + 6, true));
            }
            b'0'..=b'9' if boundary(d, i) => {
                if let Some(e) = obj_header_at(d, i) {
                    return Some((i, e, false));
                }
            }
            b'x' if d[i..].starts_with(b"xref") && boundary(d, i) && boundary(d, i + 4) => {
                return Some((i, i + 4, false));
            }
            b't' if d[i..].starts_with(b"trailer") && boundary(d, i) => {
                let mut p = i + 7;
                while d.get(p).is_some_and(|b| is_js_space(*b)) {
                    p += 1;
                }
                if d[p.min(d.len())..].starts_with(b"<<") && boundary(d, p + 2) {
                    return Some((i, p + 2, false));
                }
            }
            _ => {}
        }
    }
    None
}

/// The first match of `\b(startxref|\d+\s+\d+\s+obj)\b` at or after `from`: (start, end, it is `startxref`).
fn find_after_trailer(d: &[u8], from: usize) -> Option<(usize, usize, bool)> {
    for i in from..d.len() {
        match d[i] {
            b's' if d[i..].starts_with(b"startxref") && boundary(d, i) && boundary(d, i + 9) => {
                return Some((i, i + 9, true));
            }
            b'0'..=b'9' if boundary(d, i) => {
                if let Some(e) = obj_header_at(d, i) {
                    return Some((i, e, false));
                }
            }
            _ => {}
        }
    }
    None
}

/// How many bytes from `offset` until `what` (or to the end of the data).
fn skip_until(d: &[u8], offset: usize, what: &[u8]) -> usize {
    let tail = &d[offset.min(d.len())..];
    tail.windows(what.len())
        .position(|w| w == what)
        .unwrap_or(tail.len())
}

/// A "token" in pdf.js's scan: the bytes up to the next line break or `<`. Like pdf.js, a token that runs to
/// the end of the data loses its last byte.
fn token_end(d: &[u8], pos: usize) -> usize {
    let (mut offset, mut end) = (pos, pos);
    while let Some(&b) = d.get(offset) {
        if matches!(b, 10 | 13 | b'<') {
            break;
        }
        offset += 1;
        if offset >= d.len() {
            break;
        }
        end = offset;
    }
    end
}

/// `^(\d+)\s+(\d+)\s+obj\b` on a token: the object and generation numbers.
fn obj_header(token: &[u8]) -> Option<(u32, u32)> {
    let end = obj_header_at(token, 0)?;
    let text = std::str::from_utf8(&token[..end - 3]).ok()?;
    let mut parts = text.split_ascii_whitespace();
    let num = parts.next()?.parse::<u64>().ok()?;
    let gen_ = parts.next()?.parse::<u64>().ok()?;
    Some((
        u32::try_from(num).ok()?,
        u32::try_from(gen_).unwrap_or(u32::MAX),
    ))
}

/// The recovery scan: index every `N G obj` at the start of a line, every trailer, and every
/// cross-reference stream, the way pdf.js does when the cross-reference data is missing or wrong.
pub(crate) fn scan(data: &[u8], limits: &Limits, meter: &Meter) -> Result<Scan, Failure> {
    // The scan reads the whole file once; keyword searches ahead of it only move the same cursor forward.
    meter.charge(cost::SCAN * data.len() as u64)?;
    let n = data.len();
    let mut entries: HashMap<u32, Entry> = HashMap::new();
    let mut gens: HashMap<u32, u32> = HashMap::new();
    let (mut trailer_at, mut xref_streams) = (Vec::new(), Vec::new());
    let mut pos = 0usize;
    while pos < n {
        let ch = data[pos];
        if matches!(ch, 9 | 10 | 13 | 32) {
            pos += 1;
            continue;
        }
        if ch == b'%' {
            loop {
                pos += 1;
                if pos >= n || matches!(data[pos], 10 | 13) {
                    break;
                }
            }
            continue;
        }
        let token = &data[pos..token_end(data, pos)];
        if token.starts_with(b"xref") && (token.len() == 4 || is_js_space(token[4])) {
            pos += skip_until(data, pos, b"trailer");
            trailer_at.push(pos);
            pos += skip_until(data, pos, b"startxref");
        } else if let Some((num, gen_)) = obj_header(token) {
            let start = pos + token.len();
            let update = match entries.get(&num) {
                None => true,
                Some(_) if gens.get(&num) == Some(&gen_) => {
                    // Do not let a later copy that fails at once replace a good one (pdf.js issue13783).
                    let mut lx = Lexer::new(data, start);
                    lx.parse_obj().is_some() || lx.pos() < n
                }
                Some(_) => false,
            };
            if update {
                if entries.len() >= limits.max_objects && !entries.contains_key(&num) {
                    return Err(Failure::LimitExceeded(Limit::Objects));
                }
                entries.insert(num, Entry::Offset(pos as u64));
                gens.insert(num, gen_);
            }
            // The object runs to the next `endobj`, or to the next `obj` (a file with missing `endobj`s).
            let length = match find_obj_end(data, start) {
                Some((s, e, endobj)) => {
                    let len = e + 1 - pos;
                    if endobj { len } else { len - ((e - s) + 1) }
                }
                None => n - pos,
            };
            let content = &data[pos..(pos + length).min(n)];
            let tag = skip_until(content, 0, b"/XRef");
            if tag < length && content.get(tag + 5).is_some_and(|b| *b < 64) {
                xref_streams.push(pos);
            }
            pos += length;
        } else if token.starts_with(b"trailer") && (token.len() == 7 || is_js_space(token[7])) {
            trailer_at.push(pos);
            let length = match find_after_trailer(data, pos + token.len()) {
                Some((s, e, startxref)) => {
                    let len = e + 1 - pos;
                    if startxref { len } else { len - ((e - s) + 1) }
                }
                None => n - pos,
            };
            pos += length;
        } else {
            pos += token.len() + 1;
        }
    }

    // Cross-reference streams found by the scan add entries for objects the scan did not see (those in
    // object streams, mostly). Each is followed through `/Prev`; errors are ignored.
    let mut decoded = 0usize;
    let mut top: Option<Dict> = None;
    for start in xref_streams {
        let (mut queue, mut seen) = (vec![start], HashSet::new());
        while !queue.is_empty() {
            let at = queue.remove(0);
            if !seen.insert(at) || seen.len() > limits.max_xref_sections {
                continue;
            }
            let Ok((section, dict)) = read_section(data, at, limits, &mut decoded, meter) else {
                continue;
            };
            for (num, entry) in section {
                if entries.len() < limits.max_objects || entries.contains_key(&num) {
                    entries.entry(num).or_insert(entry);
                }
            }
            queue.extend(prev_offset(&dict));
            top.get_or_insert(dict);
        }
    }

    let trailers = trailer_at
        .into_iter()
        .filter_map(|at| {
            let mut lx = Lexer::new(data, at);
            if !lx.eat_keyword(b"trailer") {
                return None;
            }
            match lx.parse_obj() {
                Some(Obj::Dict(d)) => Some(d),
                _ => None,
            }
        })
        .collect();
    Ok(Scan {
        entries,
        trailers,
        top,
        decoded,
    })
}
