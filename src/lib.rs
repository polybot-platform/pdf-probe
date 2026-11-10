//! pdf-probe: how many pages does an untrusted PDF have, and how big is each page?
//!
//! One function, [`probe`], reads cross-reference tables and streams (hybrid files and `/Prev` chains
//! included), object streams, Flate with PNG predictors, encryption with an empty user password, the page
//! tree and page sizes, and rescans a damaged file for its objects as pdf.js does. A file it cannot read with
//! confidence is a [`Failure`], never a guess. The input is hostile, so the work is bounded by [`Limits`].
//! See `docs/how-it-works.md` for the design.
#![forbid(unsafe_code)]

mod crypto;
mod encrypt;
mod inflate;
mod lexer;
mod meter;
mod pages;
mod stream;
mod xref;

use encrypt::Crypto;
use lexer::{Lexer, Obj};
use meter::{Meter, cost};
use std::collections::HashMap;
use xref::{Entry, Xref};

/// Limits on the work one call may do. The input is hostile, so every loop in the library is bounded by one
/// of these, and a limit that is hit is reported as [`Failure::LimitExceeded`] naming which one.
#[derive(Debug, Clone)]
pub struct Limits {
    /// More pages than this is reported as [`Failure::TooManyPages`].
    pub max_pages: usize,
    /// The largest input accepted, in bytes. Checked before anything is parsed ([`Limit::InputBytes`]).
    pub max_input_bytes: usize,
    /// A budget of work units for the whole call ([`Limit::Work`]). A unit is roughly a nanosecond of work on
    /// a modern core, and the count is the same on every machine. Catches work that is large only in total,
    /// such as one object re-parsed for every page.
    pub max_work: u64,
    /// Cross-reference entries read plus objects fetched ([`Limit::Objects`]).
    pub max_objects: usize,
    /// Cross-reference sections followed through `/Prev` ([`Limit::XrefSections`]).
    pub max_xref_sections: usize,
    /// Total bytes inflated from all streams read ([`Limit::Decompressed`]).
    pub max_decompressed: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            max_pages: 10_000,
            max_input_bytes: 10 << 20,
            max_work: DEFAULT_MAX_WORK,
            max_objects: 1_000_000,
            max_xref_sections: 256,
            max_decompressed: 64 << 20,
        }
    }
}

/// The default work budget. See [`Limits::max_work`].
pub const DEFAULT_MAX_WORK: u64 = 1 << 30;

/// Which limit was hit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Limit {
    /// The input is larger than [`Limits::max_input_bytes`].
    InputBytes,
    /// The work budget ([`Limits::max_work`]) ran out.
    Work,
    /// More than [`Limits::max_objects`] cross-reference entries or objects.
    Objects,
    /// A `/Prev` chain longer than [`Limits::max_xref_sections`].
    XrefSections,
    /// More than [`Limits::max_decompressed`] bytes inflated.
    Decompressed,
}

impl Limit {
    /// A stable name, used in the C ABI and the npm package.
    pub fn name(self) -> &'static str {
        match self {
            Limit::InputBytes => "inputBytes",
            Limit::Work => "work",
            Limit::Objects => "objects",
            Limit::XrefSections => "xrefSections",
            Limit::Decompressed => "decompressed",
        }
    }
}

/// A page's effective size in points (1/72 inch), after CropBox, `/Rotate` and `/UserUnit`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PageSize {
    pub width: f64,
    pub height: f64,
}

/// The answer: the page count and every page's size.
#[derive(Debug, Clone, PartialEq)]
pub struct Probe {
    pub pages: usize,
    pub sizes: Vec<PageSize>,
    /// The work units this call used, out of [`Limits::max_work`]. Useful for choosing a budget.
    pub work: u64,
}

/// Why no answer was given. The input is never guessed at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// Damaged, unsupported, or encrypted with a password. The text says which.
    Unparseable(&'static str),
    /// Well-formed, but with no pages.
    Empty,
    /// More pages than [`Limits::max_pages`].
    TooManyPages { pages: usize },
    /// A resource limit was hit. The file may be fine; it is more than this call will do.
    LimitExceeded(Limit),
}

/// A decoded object stream: where each object starts, relative to `first`.
struct ObjStm {
    data: Vec<u8>,
    first: usize,
    index: Vec<(u32, usize)>,
}

pub(crate) struct Doc<'a> {
    data: &'a [u8],
    pub(crate) xref: Xref,
    limits: &'a Limits,
    meter: &'a Meter,
    fetched: usize,
    decoded: usize,
    objstms: HashMap<u32, ObjStm>,
    crypto: Option<Crypto>,
    /// Objects reached through a reference from another object (a shared `/MediaBox`, say), parsed once.
    shared: HashMap<u32, Obj>,
    shared_weight: usize,
}

impl Doc<'_> {
    fn spend(&mut self) -> Result<(), Failure> {
        self.fetched += 1;
        if self.fetched > self.limits.max_objects {
            return Err(Failure::LimitExceeded(Limit::Objects));
        }
        Ok(())
    }

    /// The object with this number, or `null` if the cross-reference has no usable entry for it.
    pub(crate) fn fetch(&mut self, num: u32) -> Result<Obj, Failure> {
        let Some(&entry) = self.xref.entries.get(&num) else {
            return Ok(Obj::Null);
        };
        match entry {
            Entry::Free => Ok(Obj::Null),
            Entry::Offset(offset) => {
                self.spend()?;
                let offset = usize::try_from(offset)
                    .ok()
                    .filter(|o| *o < self.data.len());
                let Some(offset) = offset else {
                    return Err(Failure::Unparseable("object offset past the end"));
                };
                let mut lx = Lexer::new(self.data, offset);
                let parsed = lx.parse_indirect();
                self.meter
                    .charge(cost::PARSE * (lx.pos() - offset) as u64)?;
                match parsed {
                    Some((n, _, obj)) if n == num => Ok(obj),
                    Some(_) => Err(Failure::Unparseable("object number mismatch")),
                    None => Err(Failure::Unparseable("unreadable object")),
                }
            }
            Entry::InStream { stream, index } => {
                self.spend()?;
                self.fetch_compressed(num, stream, index)
            }
        }
    }

    fn fetch_compressed(&mut self, num: u32, stream: u32, index: u32) -> Result<Obj, Failure> {
        if !self.objstms.contains_key(&stream) {
            let loaded = self.load_objstm(stream)?;
            self.objstms.insert(stream, loaded);
        }
        let Some(stm) = self.objstms.get(&stream) else {
            return Err(Failure::Unparseable("object stream missing"));
        };
        let listed = stm
            .index
            .get(index as usize)
            .copied()
            .filter(|(n, _)| *n == num);
        let found = listed.or_else(|| stm.index.iter().copied().find(|(n, _)| *n == num));
        let Some((_, rel)) = found else {
            return Err(Failure::Unparseable(
                "object missing from its object stream",
            ));
        };
        let start = stm.first.saturating_add(rel);
        if start >= stm.data.len() {
            return Err(Failure::Unparseable("compressed object past the end"));
        }
        let mut lx = Lexer::new(&stm.data, start);
        let parsed = lx.parse_obj();
        self.meter.charge(cost::PARSE * (lx.pos() - start) as u64)?;
        parsed.ok_or(Failure::Unparseable("unreadable compressed object"))
    }

    fn load_objstm(&mut self, stream: u32) -> Result<ObjStm, Failure> {
        let Some(Entry::Offset(offset)) = self.xref.entries.get(&stream).copied() else {
            return Err(Failure::Unparseable(
                "object stream is missing or itself compressed",
            ));
        };
        let offset = usize::try_from(offset)
            .ok()
            .filter(|o| *o < self.data.len());
        let Some(offset) = offset else {
            return Err(Failure::Unparseable("object stream offset past the end"));
        };
        let Some(ind) = Lexer::new(self.data, offset).parse_indirect_full() else {
            return Err(Failure::Unparseable("unreadable object stream"));
        };
        let (Obj::Dict(dict), Some(range)) = (ind.obj, ind.stream) else {
            return Err(Failure::Unparseable("not an object stream"));
        };
        self.meter
            .charge(cost::SCAN * (range.end - offset) as u64)?;
        if ind.num != stream {
            return Err(Failure::Unparseable("object number mismatch"));
        }
        let get = |key: &[u8]| {
            dict.get(key)
                .and_then(Obj::as_int)
                .and_then(|n| usize::try_from(n).ok())
        };
        let (Some(n), Some(first)) = (get(b"N"), get(b"First")) else {
            return Err(Failure::Unparseable("bad object stream header"));
        };
        if n > self.limits.max_objects {
            return Err(Failure::LimitExceeded(Limit::Objects));
        }
        let remaining = self.limits.max_decompressed.saturating_sub(self.decoded);
        let raw = &self.data[range];
        let raw = match &self.crypto {
            Some(crypto) => crypto.decrypt_stream(stream, ind.gen_, &dict, raw, self.meter)?,
            None => std::borrow::Cow::Borrowed(raw),
        };
        let bytes = stream::decode(&dict, &raw, remaining, self.meter)?;
        self.decoded += bytes.len();
        let mut lx = Lexer::new(&bytes, 0);
        let mut index = Vec::new();
        for _ in 0..n {
            let (Some(num), Some(rel)) = (lx.read_uint(), lx.read_uint()) else {
                return Err(Failure::Unparseable("bad object stream header"));
            };
            let (Ok(num), Ok(rel)) = (u32::try_from(num), usize::try_from(rel)) else {
                return Err(Failure::Unparseable("bad object stream header"));
            };
            index.push((num, rel));
        }
        self.meter.charge(cost::PARSE * lx.pos() as u64)?;
        Ok(ObjStm {
            data: bytes,
            first,
            index,
        })
    }

    /// Like [`Doc::fetch`], but remembers the result, within a fixed budget. Without this, a value that
    /// many pages refer to is parsed again for every page, so the work grows with pages times its size.
    /// Page tree nodes are read once and go through `fetch`, so they do not use up the budget.
    fn fetch_shared(&mut self, num: u32) -> Result<Obj, Failure> {
        if let Some(obj) = self.shared.get(&num) {
            return Ok(obj.clone());
        }
        let obj = self.fetch(num)?;
        let weight = weight(&obj);
        if self.shared_weight + weight <= SHARED_BUDGET {
            self.shared_weight += weight;
            self.shared.insert(num, obj.clone());
        }
        Ok(obj)
    }

    /// Follows references until a direct object is reached.
    pub(crate) fn resolve(&mut self, obj: &Obj) -> Result<Obj, Failure> {
        let mut current = obj.clone();
        for _ in 0..16 {
            match current {
                Obj::Ref(n, _) => current = self.fetch_shared(n)?,
                other => return Ok(other),
            }
        }
        Err(Failure::Unparseable("reference chain too long"))
    }
}

/// How much parsed data [`Doc::fetch_shared`] keeps, counted in object nodes (about 32 bytes each, so
/// roughly 32 MB at most).
const SHARED_BUDGET: usize = 1 << 20;

/// The size of a parsed object, in nodes plus the bytes of its names and strings.
fn weight(obj: &Obj) -> usize {
    match obj {
        Obj::Name(b) | Obj::Str(b) => 1 + b.len(),
        Obj::Array(items) => 1 + items.iter().map(weight).sum::<usize>(),
        Obj::Dict(d) => 1 + d.0.iter().map(|(k, v)| k.len() + weight(v)).sum::<usize>(),
        _ => 1,
    }
}

/// An error, and whether pdf.js would answer it by rescanning the whole file for objects.
pub(crate) struct Fault {
    pub failure: Failure,
    pub recover: bool,
}

impl From<Failure> for Fault {
    fn from(failure: Failure) -> Self {
        Fault {
            failure,
            recover: false,
        }
    }
}

/// Errors reading an object through its cross-reference entry (pdf.js's `XRefEntryException`).
pub(crate) fn is_entry_error(f: &Failure) -> bool {
    const ENTRY_ERRORS: [&str; 9] = [
        "object offset past the end",
        "object number mismatch",
        "unreadable object",
        "object missing from its object stream",
        "compressed object past the end",
        "unreadable compressed object",
        "object stream is missing or itself compressed",
        "object stream offset past the end",
        "unreadable object stream",
    ];
    matches!(f, Failure::Unparseable(why) if ENTRY_ERRORS.contains(why))
}

/// A resource limit was hit. That is final: it is never answered by rescanning the file, and the error is
/// not replaced by a later one.
pub(crate) fn is_limit_error(f: &Failure) -> bool {
    matches!(f, Failure::LimitExceeded(_))
}

impl<'a> Doc<'a> {
    fn new(data: &'a [u8], xref: Xref, limits: &'a Limits, meter: &'a Meter) -> Doc<'a> {
        let decoded = xref.decoded;
        Doc {
            data,
            xref,
            limits,
            meter,
            fetched: 0,
            decoded,
            objstms: HashMap::new(),
            crypto: None,
            shared: HashMap::new(),
            shared_weight: 0,
        }
    }
}

/// Reads the page count and page sizes of `data`.
pub fn probe(data: &[u8], limits: &Limits) -> Result<Probe, Failure> {
    if data.len() > limits.max_input_bytes {
        return Err(Failure::LimitExceeded(Limit::InputBytes));
    }
    let meter = Meter::new(limits.max_work);
    // Offsets are relative to the `%PDF-` header, which may follow some leading junk.
    let head = &data[..data.len().min(1024)];
    let base = head.windows(5).position(|w| w == b"%PDF-").unwrap_or(0);
    let data = &data[base..];

    let first = match xref::read(data, limits, &meter) {
        Ok(xref) => run(data, limits, &meter, xref),
        Err(failure) => {
            let recover = !is_limit_error(&failure);
            Err(Fault { failure, recover })
        }
    };
    match first {
        Ok(probe) => Ok(probe),
        Err(Fault { recover: true, .. }) => {
            let xref = recover(data, limits, &meter)?;
            run(data, limits, &meter, xref).map_err(|fault| fault.failure)
        }
        Err(fault) => Err(fault.failure),
    }
}

fn run(data: &[u8], limits: &Limits, meter: &Meter, xref: Xref) -> Result<Probe, Fault> {
    let mut doc = Doc::new(data, xref, limits, meter);
    doc.crypto = encrypt::setup(&mut doc)?;
    let (sizes, count) = pages::collect(&mut doc, limits.max_pages)?;
    if sizes.len() > limits.max_pages {
        return Err(Failure::TooManyPages {
            pages: count.unwrap_or(sizes.len()),
        }
        .into());
    }
    if sizes.is_empty() {
        return Err(Failure::Empty.into());
    }
    Ok(Probe {
        pages: sizes.len(),
        sizes,
        work: meter.used(),
    })
}

/// Whether a trailer is usable, as pdf.js judges it while rescanning: `/Root` and its `/Pages` must be
/// dictionaries. `Some(true)` when `/Count` is also an integer.
fn trailer_candidate(doc: &mut Doc, trailer: &lexer::Dict) -> Option<bool> {
    let root = trailer.get(b"Root")?.clone();
    let Ok(Obj::Dict(root)) = doc.resolve(&root) else {
        return None;
    };
    let pages = root.get(b"Pages")?.clone();
    let Ok(Obj::Dict(pages)) = doc.resolve(&pages) else {
        return None;
    };
    let count = pages.get(b"Count").cloned();
    Some(count.is_some_and(|c| doc.resolve(&c).ok().and_then(|o| o.as_int()).is_some()))
}

/// Rebuilds the cross-reference by scanning the file, and picks the trailer the way pdf.js does: the first
/// trailer with a usable page tree and an `/ID` (and `/Encrypt`, if any trailer has one); else the last
/// trailer with a usable page tree; else the first cross-reference dictionary; else any object with `/Root`.
fn recover(data: &[u8], limits: &Limits, meter: &Meter) -> Result<Xref, Failure> {
    let scan = xref::scan(data, limits, meter)?;
    let encrypted = scan.trailers.iter().any(|t| t.get(b"Encrypt").is_some());
    let mut doc = Doc::new(
        data,
        Xref {
            entries: scan.entries,
            trailer: lexer::Dict::default(),
            decoded: scan.decoded,
        },
        limits,
        meter,
    );
    let mut candidate = None;
    let mut chosen = None;
    for trailer in &scan.trailers {
        let Some(valid_pages) = trailer_candidate(&mut doc, trailer) else {
            continue;
        };
        if valid_pages
            && (!encrypted || trailer.get(b"Encrypt").is_some())
            && trailer.get(b"ID").is_some()
        {
            chosen = Some(trailer.clone());
            break;
        }
        candidate = Some(trailer.clone());
    }
    let mut trailer = chosen.or(candidate).or(scan.top);
    if trailer.is_none() && scan.trailers.is_empty() {
        let mut nums: Vec<u32> = doc.xref.entries.keys().copied().collect();
        nums.sort_unstable();
        for num in nums.into_iter().take(limits.max_objects) {
            if let Ok(Obj::Dict(d)) = doc.fetch(num)
                && d.get(b"Root").is_some()
            {
                trailer = Some(d);
                break;
            }
        }
    }
    let Some(trailer) = trailer else {
        return Err(Failure::Unparseable("no usable trailer"));
    };
    doc.xref.trailer = trailer;
    Ok(doc.xref)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a classic PDF from `(number, body)` objects with a correct cross-reference table.
    fn build(objects: &[(u32, &str)], root: u32) -> Vec<u8> {
        let mut out = b"%PDF-1.4\n".to_vec();
        let mut offsets = vec![(0u32, 0usize)];
        for (n, body) in objects {
            offsets.push((*n, out.len()));
            out.extend(format!("{n} 0 obj\n{body}\nendobj\n").bytes());
        }
        let size = objects.iter().map(|(n, _)| *n).max().unwrap_or(0) + 1;
        let xref = out.len();
        out.extend(format!("xref\n0 {size}\n").bytes());
        for n in 0..size {
            match offsets.iter().find(|(m, _)| *m == n) {
                Some((0, _)) | None => out.extend(b"0000000000 65535 f \n"),
                Some((_, off)) => out.extend(format!("{off:010} 00000 n \n").bytes()),
            }
        }
        out.extend(
            format!("trailer\n<< /Size {size} /Root {root} 0 R >>\nstartxref\n{xref}\n%%EOF\n")
                .bytes(),
        );
        out
    }

    fn one_page(extra: &str) -> Vec<u8> {
        build(
            &[
                (1, "<< /Type /Catalog /Pages 2 0 R >>"),
                (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
                (3, &format!("<< /Type /Page /Parent 2 0 R {extra} >>")),
            ],
            1,
        )
    }

    #[test]
    fn reads_a_one_page_file() {
        let p = probe(&one_page("/MediaBox [0 0 612 792]"), &Limits::default()).unwrap();
        assert_eq!(p.pages, 1);
        assert_eq!(
            p.sizes,
            [PageSize {
                width: 612.0,
                height: 792.0
            }]
        );
    }

    #[test]
    fn applies_rotate_and_user_unit() {
        let p = probe(
            &one_page("/MediaBox [0 0 100 200] /Rotate 90 /UserUnit 2"),
            &Limits::default(),
        )
        .unwrap();
        assert_eq!(
            p.sizes,
            [PageSize {
                width: 400.0,
                height: 200.0
            }]
        );
    }

    #[test]
    fn crop_box_with_no_overlap_falls_back_to_the_media_box() {
        let p = probe(
            &one_page("/MediaBox [0 0 600 800] /CropBox [600 800 1000 1000]"),
            &Limits::default(),
        )
        .unwrap();
        assert_eq!(
            p.sizes,
            [PageSize {
                width: 600.0,
                height: 800.0
            }]
        );
    }

    #[test]
    fn an_empty_page_tree_is_empty() {
        let pdf = build(
            &[
                (1, "<< /Type /Catalog /Pages 2 0 R >>"),
                (2, "<< /Type /Pages /Kids [] /Count 0 >>"),
            ],
            1,
        );
        assert_eq!(probe(&pdf, &Limits::default()), Err(Failure::Empty));
    }

    #[test]
    fn a_page_tree_loop_fails_closed() {
        let pdf = build(
            &[
                (1, "<< /Type /Catalog /Pages 2 0 R >>"),
                (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
                (3, "<< /Type /Pages /Kids [2 0 R] /Count 1 >>"),
            ],
            1,
        );
        assert_eq!(
            probe(&pdf, &Limits::default()),
            Err(Failure::Unparseable("the page tree loops"))
        );
    }

    #[test]
    fn count_is_trusted_when_low_and_corrected_when_high() {
        let kids = "3 0 R 4 0 R 5 0 R";
        let make = |count: usize| {
            build(
                &[
                    (1, "<< /Type /Catalog /Pages 2 0 R >>"),
                    (
                        2,
                        &format!("<< /Type /Pages /Kids [{kids}] /Count {count} >>"),
                    ),
                    (3, "<< /Type /Page /Parent 2 0 R >>"),
                    (4, "<< /Type /Page /Parent 2 0 R >>"),
                    (5, "<< /Type /Page /Parent 2 0 R >>"),
                ],
                1,
            )
        };
        assert_eq!(probe(&make(2), &Limits::default()).unwrap().pages, 2);
        assert_eq!(probe(&make(500), &Limits::default()).unwrap().pages, 3);
    }

    #[test]
    fn more_pages_than_the_cap_is_reported() {
        let limits = Limits {
            max_pages: 1,
            ..Limits::default()
        };
        let pdf = build(
            &[
                (1, "<< /Type /Catalog /Pages 2 0 R >>"),
                (2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>"),
                (3, "<< /Type /Page /Parent 2 0 R >>"),
                (4, "<< /Type /Page /Parent 2 0 R >>"),
            ],
            1,
        );
        assert_eq!(
            probe(&pdf, &limits),
            Err(Failure::TooManyPages { pages: 2 })
        );
    }

    /// A zlib stream holding `data` in one stored block (no compression), for building test files.
    fn stored_zlib(data: &[u8]) -> Vec<u8> {
        let len = data.len() as u16;
        let mut z = vec![0x78, 0x01, 0x01];
        z.extend(len.to_le_bytes());
        z.extend((!len).to_le_bytes());
        z.extend(data);
        z.extend([0, 0, 0, 0]);
        z
    }

    /// A PDF whose catalog and page tree are in an object stream and whose cross-reference is a stream.
    fn modern_pdf() -> Vec<u8> {
        let mut out = b"%PDF-1.5\n".to_vec();
        let page = out.len();
        out.extend(b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 400] >>\nendobj\n");
        let (cat, pages) = (
            "<< /Type /Catalog /Pages 2 0 R >>",
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        );
        let header = format!("1 0 2 {} ", cat.len() + 1);
        let first = header.len();
        let z = stored_zlib(format!("{header}{cat}\n{pages}\n").as_bytes());
        let stm = out.len();
        out.extend(
            format!("4 0 obj\n<< /Type /ObjStm /N 2 /First {first} /Filter /FlateDecode /Length {} >>\nstream\n", z.len())
                .bytes(),
        );
        out.extend(z);
        out.extend(b"\nendstream\nendobj\n");
        let xs = out.len();
        let row = |t: u8, a: usize, b: u8| [t, (a >> 8) as u8, a as u8, b];
        let rows: Vec<u8> = [
            row(2, 4, 0),
            row(2, 4, 1),
            row(1, page, 0),
            row(1, stm, 0),
            row(1, xs, 0),
        ]
        .concat();
        let z = stored_zlib(&rows);
        out.extend(
            format!(
                "5 0 obj\n<< /Type /XRef /Size 6 /W [1 2 1] /Index [1 5] /Root 1 0 R /Filter /FlateDecode /Length {} >>\nstream\n",
                z.len()
            )
            .bytes(),
        );
        out.extend(z);
        out.extend(format!("\nendstream\nendobj\nstartxref\n{xs}\n%%EOF\n").bytes());
        out
    }

    #[test]
    fn reads_a_cross_reference_stream_and_an_object_stream() {
        let p = probe(&modern_pdf(), &Limits::default()).unwrap();
        assert_eq!(
            p.sizes,
            [PageSize {
                width: 300.0,
                height: 400.0
            }]
        );
    }

    #[test]
    fn a_decompression_budget_that_is_too_small_fails_closed() {
        let limits = Limits {
            max_decompressed: 8,
            ..Limits::default()
        };
        assert_eq!(
            probe(&modern_pdf(), &limits),
            Err(Failure::LimitExceeded(Limit::Decompressed))
        );
    }

    #[test]
    fn an_input_over_the_cap_is_refused_before_anything_is_parsed() {
        let pdf = one_page("/MediaBox [0 0 612 792]");
        let at = |max_input_bytes| Limits {
            max_input_bytes,
            ..Limits::default()
        };
        assert_eq!(
            probe(&pdf, &at(pdf.len() - 1)),
            Err(Failure::LimitExceeded(Limit::InputBytes))
        );
        assert!(probe(&pdf, &at(pdf.len())).is_ok());
        // Garbage over the cap is a limit, not a parse error: nothing was looked at.
        assert_eq!(
            probe(&[b'x'; 100], &at(10)),
            Err(Failure::LimitExceeded(Limit::InputBytes))
        );
        // The default is 10 MiB.
        assert_eq!(Limits::default().max_input_bytes, 10 << 20);
    }

    #[test]
    fn the_work_a_call_used_is_reported_and_is_the_same_every_time() {
        let pdf = modern_pdf();
        let first = probe(&pdf, &Limits::default()).unwrap();
        assert!(first.work > 0);
        assert_eq!(probe(&pdf, &Limits::default()).unwrap().work, first.work);
        assert_eq!(
            probe(&modern_pdf(), &Limits::default()).unwrap().work,
            first.work
        );
    }

    #[test]
    fn the_work_budget_is_exact() {
        let pdf = modern_pdf();
        let used = probe(&pdf, &Limits::default()).unwrap().work;
        let with = |max_work| Limits {
            max_work,
            ..Limits::default()
        };
        assert!(probe(&pdf, &with(used)).is_ok());
        assert_eq!(
            probe(&pdf, &with(used - 1)),
            Err(Failure::LimitExceeded(Limit::Work))
        );
        assert_eq!(
            probe(&pdf, &with(0)),
            Err(Failure::LimitExceeded(Limit::Work))
        );
    }

    #[test]
    fn each_structural_limit_names_itself() {
        let pdf = one_page("/MediaBox [0 0 612 792]");
        let hit = |limits: Limits| probe(&pdf, &limits);
        assert_eq!(
            hit(Limits {
                max_objects: 2,
                ..Limits::default()
            }),
            Err(Failure::LimitExceeded(Limit::Objects))
        );
        assert_eq!(
            hit(Limits {
                max_xref_sections: 0,
                ..Limits::default()
            }),
            Err(Failure::LimitExceeded(Limit::XrefSections))
        );
    }

    #[test]
    fn a_limit_is_final_even_for_a_file_that_would_be_rescanned() {
        // A wrong startxref sends the reader to the rescan; a budget too small for it must still be a limit,
        // not a parse error and not a second try.
        let pdf = with_startxref(one_page("/MediaBox [0 0 612 792]"), "999999");
        let tight = Limits {
            max_work: 100,
            ..Limits::default()
        };
        assert_eq!(
            probe(&pdf, &tight),
            Err(Failure::LimitExceeded(Limit::Work))
        );
        assert!(probe(&pdf, &Limits::default()).is_ok());
    }

    /// Object 3 is a big array that the root Pages node inherits as `/MediaBox`, so every page re-parses it:
    /// the cost is pages x array size while the file stays small. See `examples/hostile.rs`.
    fn big_inherited_box(pages: usize, elements: usize) -> Vec<u8> {
        let kids: String = (0..pages).map(|i| format!("{} 0 R ", 4 + i)).collect();
        let array = format!("[{}]", "0 ".repeat(elements));
        let page = "<< /Type /Page /Parent 2 0 R >>".to_string();
        let tree = format!("<< /Type /Pages /Kids [{kids}] /Count {pages} /MediaBox 3 0 R >>");
        let mut objects: Vec<(u32, &str)> = vec![
            (1, "<< /Type /Catalog /Pages 2 0 R >>"),
            (2, &tree),
            (3, &array),
        ];
        let numbers: Vec<u32> = (0..pages as u32).map(|i| 4 + i).collect();
        for n in &numbers {
            objects.push((*n, &page));
        }
        build(&objects, 1)
    }

    #[test]
    fn an_object_shared_by_every_page_is_parsed_once() {
        let pdf = big_inherited_box(200, 20_000);
        assert!(
            pdf.len() < 600_000,
            "the file is small: {} bytes",
            pdf.len()
        );
        // One parse of the array is about 400 KB x 10 units; re-parsing it for each of 200 pages was over 800M.
        let p = probe(&pdf, &Limits::default()).unwrap();
        assert_eq!(p.pages, 200);
        assert!(p.work < 20_000_000, "it cost {} units", p.work);
    }

    #[test]
    fn an_object_too_big_to_keep_still_runs_out_of_budget() {
        // More nodes than the shared budget, so it is parsed again for every page and the work budget stops it.
        let pdf = big_inherited_box(100, SHARED_BUDGET + 100_000);
        let budget = Limits {
            max_work: 100_000_000,
            ..Limits::default()
        };
        assert_eq!(
            probe(&pdf, &budget),
            Err(Failure::LimitExceeded(Limit::Work))
        );
    }

    /// The same file with its `startxref` value replaced.
    fn with_startxref(mut pdf: Vec<u8>, value: &str) -> Vec<u8> {
        let at = pdf.windows(10).rposition(|w| w == b"startxref\n").unwrap();
        pdf.truncate(at + 10);
        pdf.extend(format!("{value}\n%%EOF\n").bytes());
        pdf
    }

    #[test]
    fn a_wrong_startxref_is_recovered_by_scanning() {
        let good = one_page("/MediaBox [0 0 612 792]");
        for bad in ["999999", "0", "7"] {
            let p = probe(&with_startxref(good.clone(), bad), &Limits::default()).unwrap();
            assert_eq!(
                p.sizes,
                [PageSize {
                    width: 612.0,
                    height: 792.0
                }],
                "startxref {bad}"
            );
        }
    }

    #[test]
    fn junk_before_the_header_shifts_every_offset() {
        let mut pdf = b"JUNK JUNK\n".to_vec();
        pdf.extend(one_page("/MediaBox [0 0 200 300]"));
        assert_eq!(
            probe(&pdf, &Limits::default()).unwrap().sizes,
            [PageSize {
                width: 200.0,
                height: 300.0
            }]
        );
    }

    #[test]
    fn a_file_with_no_trailer_fails_closed() {
        let pdf = b"%PDF-1.4\n1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n2 0 obj\n<< /Kids [] /Count 0 >>\nendobj\n";
        assert_eq!(
            probe(pdf, &Limits::default()),
            Err(Failure::Unparseable("no usable trailer"))
        );
    }

    #[test]
    fn the_scan_finds_objects_after_a_wrong_startxref_in_a_modern_file() {
        let p = probe(&with_startxref(modern_pdf(), "123456"), &Limits::default()).unwrap();
        assert_eq!(
            p.sizes,
            [PageSize {
                width: 300.0,
                height: 400.0
            }]
        );
    }

    #[test]
    fn garbage_and_empty_input_fail_closed() {
        assert!(probe(b"", &Limits::default()).is_err());
        assert!(probe(b"not a pdf at all", &Limits::default()).is_err());
    }
}
