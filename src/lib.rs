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
use std::borrow::Cow;
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
    /// Items of parsed arrays and dictionaries alive at once ([`Limit::Values`]). Bounds memory: a small
    /// compressed stream can inflate to millions of tiny values. An item costs about 100 bytes at worst (a
    /// dictionary entry with its own key allocation and growth slack), so at the default the parsed structure
    /// stays near 100 MB. Not counted here: the bytes of names and strings, copied from the file, and the
    /// decompressed streams themselves, both bounded by the input size and
    /// [`max_decompressed`](Self::max_decompressed). The worst file we built peaks at about 170 MB. Items
    /// count only while the object holding them is alive, and the shared-object cache may hold at most half
    /// of them, so a long document does not add up against this; real files keep at most a few tens of
    /// thousands alive.
    pub max_values: usize,
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
            max_values: 1_000_000,
        }
    }
}

/// The default work budget. See [`Limits::max_work`].
pub const DEFAULT_MAX_WORK: u64 = 1 << 30;

/// Which limit was hit. More may be added, so a `match` needs a wildcard arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
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
    /// More than [`Limits::max_values`] items of arrays and dictionaries alive at once.
    Values,
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
            Limit::Values => "values",
        }
    }
}

/// A page's effective size in points (1/72 inch), after CropBox, `/Rotate` and `/UserUnit`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PageSize {
    pub width: f64,
    pub height: f64,
}

/// The answer: the page count, every page's size, and document metadata read along the way.
#[derive(Debug, Clone, PartialEq)]
pub struct Probe {
    pub pages: usize,
    pub sizes: Vec<PageSize>,
    /// The work units this call used, out of [`Limits::max_work`]. Useful for choosing a budget.
    pub work: u64,
    /// The effective PDF version `(major, minor)` from the `%PDF-X.Y` header or the catalog's `/Version` entry
    /// (whichever is newer), or `None` if neither is a valid version.
    pub version: Option<(u8, u8)>,
    /// Whether the file has an `/Encrypt` dictionary (opened with an empty user password).
    pub encrypted: bool,
    /// Whether the cross-reference was missing or damaged and the file was recovered by scanning.
    pub repaired: bool,
    /// Whether any page's effective visible box (`MediaBox ∩ CropBox`) is smaller than its `MediaBox`.
    pub cropped: bool,
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
struct ObjStm<'a> {
    /// Borrowed from the file when the stream is neither encrypted nor compressed.
    data: Cow<'a, [u8]>,
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
    objstms: HashMap<u32, ObjStm<'a>>,
    crypto: Option<Crypto>,
    /// Objects reached through a reference from another object (a shared `/MediaBox`, say), parsed once.
    shared: HashMap<u32, Obj>,
    shared_weight: usize,
}

impl<'a> Doc<'a> {
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
                let mut lx = Lexer::new(self.data, offset, self.meter);
                let parsed = lx.parse_indirect();
                self.meter.check_values()?;
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
        let found = match listed {
            Some(entry) => Some(entry),
            None => {
                self.meter.charge(stm.index.len() as u64)?;
                stm.index.iter().copied().find(|(n, _)| *n == num)
            }
        };
        let Some((_, rel)) = found else {
            return Err(Failure::Unparseable(
                "object missing from its object stream",
            ));
        };
        let start = stm.first.saturating_add(rel);
        if start >= stm.data.len() {
            return Err(Failure::Unparseable("compressed object past the end"));
        }
        let mut lx = Lexer::new(&stm.data, start, self.meter);
        let parsed = lx.parse_obj();
        self.meter.check_values()?;
        self.meter.charge(cost::PARSE * (lx.pos() - start) as u64)?;
        parsed.ok_or(Failure::Unparseable("unreadable compressed object"))
    }

    fn load_objstm(&mut self, stream: u32) -> Result<ObjStm<'a>, Failure> {
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
        let parsed = Lexer::new(self.data, offset, self.meter).parse_indirect_full();
        self.meter.check_values()?;
        let Some(ind) = parsed else {
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
        let raw: &'a [u8] = &self.data[range];
        let raw = match &self.crypto {
            Some(crypto) => crypto.decrypt_stream(stream, ind.gen_, &dict, raw, self.meter)?,
            None => Cow::Borrowed(raw),
        };
        let bytes = stream::decode(&dict, raw, remaining, self.meter)?;
        self.decoded += bytes.len();
        let mut lx = Lexer::new(&bytes, 0, self.meter);
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
            // Arrays and dictionaries are shared, so this copies pointers; any copy made counts towards the
            // limit on items alive.
            let obj = obj.clone();
            self.meter.check_values()?;
            return Ok(obj);
        }
        let obj = self.fetch(num)?;
        let weight = weight(&obj);
        // What the cache keeps stays alive for the whole call, so it may fill at most half the limit.
        let budget = SHARED_BUDGET.min(self.limits.max_values / 2);
        if self.shared_weight + weight <= budget {
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

/// How much parsed data [`Doc::fetch_shared`] keeps, counted in object nodes plus name and string bytes.
/// Cached values share their storage with callers.
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

fn header_version(data: &[u8]) -> Option<(u8, u8)> {
    let v = data.strip_prefix(b"%PDF-")?;
    if v.len() >= 3 && matches!(v[0], b'1' | b'2') && v[1] == b'.' && v[2].is_ascii_digit() {
        Some((v[0] - b'0', v[2] - b'0'))
    } else {
        None
    }
}

/// Reads the page count and page sizes of `data`.
pub fn probe(data: &[u8], limits: &Limits) -> Result<Probe, Failure> {
    if data.len() > limits.max_input_bytes {
        return Err(Failure::LimitExceeded(Limit::InputBytes));
    }
    let meter = Meter::new(limits.max_work).with_max_values(limits.max_values);
    // Offsets are relative to the `%PDF-` header, which may follow some leading junk.
    let head = &data[..data.len().min(1024)];
    let base = head.windows(5).position(|w| w == b"%PDF-").unwrap_or(0);
    let data = &data[base..];

    let first = match xref::read(data, limits, &meter) {
        Ok(xref) => run(data, limits, &meter, xref, false),
        Err(failure) => {
            let recover = !is_limit_error(&failure);
            Err(Fault { failure, recover })
        }
    };
    let result = match first {
        Ok(probe) => Ok(probe),
        Err(Fault { recover: true, .. }) => recover(data, limits, &meter)
            .and_then(|xref| run(data, limits, &meter, xref, true).map_err(|fault| fault.failure)),
        Err(fault) => Err(fault.failure),
    };
    // Parse sites report the value limit as a limit, which ends the call. This is a backstop for a site that
    // tolerated a failed parse before checking.
    if meter.values_exceeded() {
        return Err(Failure::LimitExceeded(Limit::Values));
    }
    result
}

fn run(
    data: &[u8],
    limits: &Limits,
    meter: &Meter,
    xref: Xref,
    repaired: bool,
) -> Result<Probe, Fault> {
    let mut doc = Doc::new(data, xref, limits, meter);
    doc.crypto = encrypt::setup(&mut doc)?;
    let encrypted = doc.crypto.is_some();
    let collected = pages::collect(&mut doc, limits.max_pages)?;
    if collected.sizes.len() > limits.max_pages {
        return Err(Failure::TooManyPages {
            pages: collected.count.unwrap_or(collected.sizes.len()),
        }
        .into());
    }
    if collected.sizes.is_empty() {
        return Err(Failure::Empty.into());
    }
    let version = match (header_version(data), collected.catalog_version) {
        (Some(h), Some(c)) => Some(h.max(c)),
        (h, c) => h.or(c),
    };
    Ok(Probe {
        pages: collected.sizes.len(),
        sizes: collected.sizes,
        work: meter.used(),
        version,
        encrypted,
        repaired,
        cropped: collected.cropped,
    })
}

/// Whether a trailer is usable, as pdf.js judges it while rescanning: `/Root` and its `/Pages` must be
/// dictionaries. `Some(true)` when `/Count` is also an integer.
fn trailer_candidate(doc: &mut Doc, trailer: &lexer::Dict) -> Result<Option<bool>, Failure> {
    let Some(root) = trailer.get(b"Root").cloned() else {
        return Ok(None);
    };
    let root = match doc.resolve(&root) {
        Ok(Obj::Dict(root)) => root,
        Ok(_) => return Ok(None),
        Err(e @ Failure::LimitExceeded(_)) => return Err(e),
        Err(_) => return Ok(None),
    };
    let Some(pages) = root.get(b"Pages").cloned() else {
        return Ok(None);
    };
    let pages = match doc.resolve(&pages) {
        Ok(Obj::Dict(pages)) => pages,
        Ok(_) => return Ok(None),
        Err(e @ Failure::LimitExceeded(_)) => return Err(e),
        Err(_) => return Ok(None),
    };
    let count = match pages.get(b"Count").cloned() {
        Some(c) => match doc.resolve(&c) {
            Ok(o) => o.as_int().is_some(),
            Err(e @ Failure::LimitExceeded(_)) => return Err(e),
            Err(_) => false,
        },
        None => false,
    };
    Ok(Some(count))
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
        let Some(valid_pages) = trailer_candidate(&mut doc, trailer)? else {
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
            match doc.fetch(num) {
                Ok(Obj::Dict(d)) if d.get(b"Root").is_some() => {
                    trailer = Some(d);
                    break;
                }
                Err(e @ Failure::LimitExceeded(_)) => return Err(e),
                _ => {}
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
    fn parsed_values_are_limited() {
        let pdf = big_inherited_box(1, 10_000);
        assert_eq!(probe(&pdf, &Limits::default()).unwrap().pages, 1);
        let limits = Limits {
            max_values: 5_000,
            ..Limits::default()
        };
        assert_eq!(
            probe(&pdf, &limits),
            Err(Failure::LimitExceeded(Limit::Values))
        );
    }

    #[test]
    fn values_of_objects_already_dropped_do_not_count() {
        // 300 pages of about 110 values each: 33,000 parsed in total, but only one page alive at a time.
        let annots = format!("[{}]", "0 ".repeat(100));
        let pages: Vec<(u32, String)> = (0..300)
            .map(|i| {
                let page = format!(
                    "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Annots {annots} >>"
                );
                (3 + i, page)
            })
            .collect();
        let kids: String = (0..300).map(|i| format!("{} 0 R ", 3 + i)).collect();
        let mut objects = vec![
            (1, "<< /Type /Catalog /Pages 2 0 R >>".to_string()),
            (2, format!("<< /Type /Pages /Kids [{kids}] /Count 300 >>")),
        ];
        objects.extend(pages);
        let objects: Vec<(u32, &str)> = objects.iter().map(|(n, b)| (*n, b.as_str())).collect();
        let pdf = build(&objects, 1);
        let limits = Limits {
            max_values: 5_000,
            ..Limits::default()
        };
        assert_eq!(probe(&pdf, &limits).unwrap().pages, 300);
        // A damaged copy is read twice, by the first pass and by the rescan; neither counts against the other.
        let repaired = probe(&with_startxref(pdf, "1"), &limits).unwrap();
        assert_eq!((repaired.pages, repaired.repaired), (300, true));
    }

    #[test]
    fn the_value_limit_is_reported_when_the_rescan_hits_it() {
        // A wrong startxref sends the call to the rescan, which skips objects it cannot parse.
        let pdf = with_startxref(big_inherited_box(1, 10_000), "1");
        let p = probe(&pdf, &Limits::default()).unwrap();
        assert!(p.repaired);
        let limits = Limits {
            max_values: 5_000,
            ..Limits::default()
        };
        assert_eq!(
            probe(&pdf, &limits),
            Err(Failure::LimitExceeded(Limit::Values))
        );
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
        assert_eq!(p.version, Some((1, 4)));
        assert!(!p.encrypted);
        assert!(!p.repaired);
        assert!(!p.cropped);
    }

    #[test]
    fn reports_catalog_version_override_repaired_and_cropped() {
        let pdf = build(
            &[
                (1, "<< /Type /Catalog /Version /1.7 /Pages 2 0 R >>"),
                (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
                (
                    3,
                    "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 800] /CropBox [0 0 300 400] >>",
                ),
            ],
            1,
        );
        let p = probe(&pdf, &Limits::default()).unwrap();
        assert_eq!(p.version, Some((1, 7)));
        assert!(!p.repaired);
        assert!(p.cropped);
        assert!(!p.encrypted);

        let damaged = with_startxref(pdf, "999999");
        let p_repaired = probe(&damaged, &Limits::default()).unwrap();
        assert!(p_repaired.repaired);
        assert!(p_repaired.cropped);
        assert_eq!(p_repaired.version, Some((1, 7)));
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
    fn the_shared_cache_fills_at_most_half_the_value_limit() {
        // Each page's /MediaBox is its own indirect array, so every one is cached: 1,200 items would stay
        // alive, more than the limit of 1,000, if the cache could take all of them.
        let kids: String = (0..300).map(|i| format!("{} 0 R ", 3 + i)).collect();
        let mut objects = vec![
            (1, "<< /Type /Catalog /Pages 2 0 R >>".to_string()),
            (2, format!("<< /Type /Pages /Kids [{kids}] /Count 300 >>")),
        ];
        for i in 0..300 {
            objects.push((
                3 + i,
                format!("<< /Type /Page /Parent 2 0 R /MediaBox {} 0 R >>", 303 + i),
            ));
            objects.push((303 + i, format!("[0 0 612 {}]", 700 + i)));
        }
        let objects: Vec<(u32, &str)> = objects.iter().map(|(n, b)| (*n, b.as_str())).collect();
        let limits = Limits {
            max_values: 1_000,
            ..Limits::default()
        };
        let p = probe(&build(&objects, 1), &limits).unwrap();
        assert_eq!(p.pages, 300);
        assert_eq!(p.sizes[299].height, 999.0);
    }

    #[test]
    fn values_of_incremental_trailers_dropped_do_not_count() {
        // Ten updates, each with a trailer holding 1,000 extra values: 10,000 parsed, a few thousand alive.
        let mut pdf = one_page("/MediaBox [0 0 612 792]");
        let junk = format!("[{}]", "0 ".repeat(1_000));
        for update in 0..10 {
            let prev = {
                let at = pdf.windows(10).rposition(|w| w == b"startxref\n").unwrap() + 10;
                let end = pdf[at..].iter().position(|b| *b == b'\n').unwrap();
                String::from_utf8(pdf[at..at + end].to_vec()).unwrap()
            };
            let page = pdf.len();
            pdf.extend(
                format!(
                    "3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 {}] >>\nendobj\n",
                    700 + update
                )
                .bytes(),
            );
            let xref = pdf.len();
            pdf.extend(
                format!(
                    "xref\n3 1\n{page:010} 00000 n \ntrailer\n<< /Size 4 /Root 1 0 R /Prev {prev} /Junk {junk} >>\nstartxref\n{xref}\n%%EOF\n"
                )
                .bytes(),
            );
        }
        let limits = Limits {
            max_values: 5_000,
            ..Limits::default()
        };
        let p = probe(&pdf, &limits).unwrap();
        assert_eq!(p.sizes[0].height, 709.0);
        assert!(!p.repaired);
    }

    #[test]
    fn values_of_object_stream_headers_dropped_do_not_count() {
        // The catalog and the page tree root sit in two object streams whose headers hold 1,000 extra values
        // each, both read before the page tree: 2,000 parsed, about 1,000 alive at once.
        let junk = format!("[{}]", "0 ".repeat(1_000));
        let mut out = b"%PDF-1.5\n".to_vec();
        let page = out.len();
        out.extend(b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 400] >>\nendobj\n");
        let mut streams = Vec::new();
        for (num, body) in [
            (1, "<< /Type /Catalog /Pages 2 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
        ] {
            let header = format!("{num} 0 ");
            let z = stored_zlib(format!("{header}{body}").as_bytes());
            streams.push(out.len());
            out.extend(
                format!(
                    "{} 0 obj\n<< /Type /ObjStm /N 1 /First {} /Filter /FlateDecode /Length {} /Junk {junk} >>\nstream\n",
                    3 + num,
                    header.len(),
                    z.len()
                )
                .bytes(),
            );
            out.extend(z);
            out.extend(b"\nendstream\nendobj\n");
        }
        let xs = out.len();
        let row = |t: u8, a: usize, b: u8| [t, (a >> 8) as u8, a as u8, b];
        let rows: Vec<u8> = [
            row(2, 4, 0),
            row(2, 5, 0),
            row(1, page, 0),
            row(1, streams[0], 0),
            row(1, streams[1], 0),
            row(1, xs, 0),
        ]
        .concat();
        let z = stored_zlib(&rows);
        out.extend(
            format!(
                "6 0 obj\n<< /Type /XRef /Size 7 /W [1 2 1] /Index [1 6] /Root 1 0 R /Filter /FlateDecode /Length {} >>\nstream\n",
                z.len()
            )
            .bytes(),
        );
        out.extend(z);
        out.extend(format!("\nendstream\nendobj\nstartxref\n{xs}\n%%EOF\n").bytes());
        let limits = Limits {
            max_values: 1_500,
            ..Limits::default()
        };
        assert_eq!(probe(&out, &limits).unwrap().pages, 1);
        let tight = Limits {
            max_values: 900,
            ..Limits::default()
        };
        assert_eq!(
            probe(&out, &tight),
            Err(Failure::LimitExceeded(Limit::Values))
        );
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
        // More than the cache may keep (half the value limit), so it is parsed again for every page and the work
        // budget stops it.
        let pdf = big_inherited_box(100, 900_000);
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

    #[test]
    fn a_predictor_with_a_huge_row_does_not_overflow_or_allocate_for_short_data() {
        let z = stored_zlib(&[0]);
        let mut pdf = b"%PDF-1.5\n".to_vec();
        let at = pdf.len();
        pdf.extend(
            format!(
                "1 0 obj\n<< /Type /XRef /Size 1 /W [1 1 1] /Filter /FlateDecode \
                 /DecodeParms << /Predictor 12 /Colors 64 /BitsPerComponent 16 /Columns {} >> \
                 /Length {} >>\nstream\n",
                1 << 24,
                z.len()
            )
            .bytes(),
        );
        pdf.extend(z);
        pdf.extend(format!("\nendstream\nendobj\nstartxref\n{at}\n%%EOF\n").bytes());
        assert!(probe(&pdf, &Limits::default()).is_err());
    }

    #[test]
    fn empty_deflate_blocks_are_bounded() {
        let mut bits = Vec::new();
        let mut acc = 0u32;
        let mut n = 0u32;
        let mut put = |v: u32, width: u32| {
            for i in 0..width {
                acc |= ((v >> i) & 1) << n;
                n += 1;
                if n == 8 {
                    bits.push(acc as u8);
                    acc = 0;
                    n = 0;
                }
            }
        };
        for _ in 0..1000 {
            put(0, 1);
            put(1, 2);
            put(0, 7);
        }
        put(1, 1);
        put(1, 2);
        put(0, 7);
        put(0, 7);
        let mut z = vec![0x78, 0x01];
        z.extend(bits);
        assert_eq!(inflate::zlib(&z, 1024), Err("too many deflate blocks"));
    }

    #[test]
    fn repeated_unterminated_objects_and_trailers_in_recovery_scan_hit_the_work_budget() {
        let mut dup = b"%PDF-1.4\n".to_vec();
        for _ in 0..2000 {
            dup.extend(b"1 0 obj\n(\n");
        }
        let tight = Limits {
            max_work: 500_000,
            ..Limits::default()
        };
        assert_eq!(
            probe(&dup, &tight),
            Err(Failure::LimitExceeded(Limit::Work))
        );

        let mut trailers = b"%PDF-1.4\n".to_vec();
        for _ in 0..2000 {
            trailers.extend(b"trailer<</A(\n1 0 obj\nendobj\n");
        }
        assert_eq!(
            probe(&trailers, &tight),
            Err(Failure::LimitExceeded(Limit::Work))
        );
    }

    #[test]
    fn object_stream_fallback_search_charges_the_work_budget() {
        let mut pdf = b"%PDF-1.5\n".to_vec();
        let cat = pdf.len();
        pdf.extend(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
        let pages = pdf.len();
        let kids = "3 0 R ".repeat(500);
        pdf.extend(
            format!("2 0 obj\n<< /Type /Pages /Kids [{kids}] /Count 500 >>\nendobj\n").bytes(),
        );
        let m = 2000usize;
        let mut header = "5 0 ".repeat(m - 1);
        header.push_str("3 0 ");
        let body = "<< /Type /Page /MediaBox [0 0 100 100] >>";
        let data = format!("{header}{body}");
        let stm = pdf.len();
        pdf.extend(
            format!(
                "4 0 obj\n<< /Type /ObjStm /N {m} /First {} /Length {} >>\nstream\n{data}\nendstream\nendobj\n",
                header.len(),
                data.len()
            )
            .bytes(),
        );
        let xs = pdf.len();
        let row = |t: u8, a: usize, b: u32| {
            let mut r = vec![t];
            r.extend(&(a as u32).to_be_bytes());
            r.extend(&b.to_be_bytes());
            r
        };
        let rows: Vec<u8> = [
            row(0, 0, 0),
            row(1, cat, 0),
            row(1, pages, 0),
            row(2, 4, 999_999),
            row(1, stm, 0),
            row(1, xs, 0),
        ]
        .concat();
        pdf.extend(
            format!(
                "6 0 obj\n<< /Type /XRef /Size 6 /W [1 4 4] /Root 1 0 R /Length {} >>\nstream\n",
                rows.len()
            )
            .bytes(),
        );
        pdf.extend(rows);
        pdf.extend(format!("\nendstream\nendobj\nstartxref\n{xs}\n%%EOF\n").bytes());
        let tight = Limits {
            max_work: 500_000,
            ..Limits::default()
        };
        assert_eq!(
            probe(&pdf, &tight),
            Err(Failure::LimitExceeded(Limit::Work))
        );
    }
}
