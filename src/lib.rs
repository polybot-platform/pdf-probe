//! pdf-probe: how many pages does an untrusted PDF have, and how big is each page?
//!
//! **Second thin slice.** Classic cross-reference tables and streams, hybrid files, `/Prev` chains, object
//! streams, Flate with PNG predictors, the page tree, and page sizes. Encryption and the recovery scan for a
//! damaged file are not implemented yet; a file that needs either fails closed. See `docs/how-it-works.md`
//! for the full design.
#![forbid(unsafe_code)]

mod inflate;
mod lexer;
mod pages;
mod stream;
mod xref;

use lexer::{Lexer, Obj};
use std::collections::HashMap;
use xref::{Entry, Xref};

/// Work limits. The input is hostile, so every loop in the library is bounded by one of these.
#[derive(Debug, Clone)]
pub struct Limits {
    /// More pages than this is reported as [`Failure::TooManyPages`].
    pub max_pages: usize,
    /// Cross-reference entries read plus objects fetched.
    pub max_objects: usize,
    /// Cross-reference sections followed through `/Prev`.
    pub max_xref_sections: usize,
    /// Total bytes inflated from all streams read (cross-reference and object streams).
    pub max_decompressed: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            max_pages: 10_000,
            max_objects: 1_000_000,
            max_xref_sections: 256,
            max_decompressed: 64 << 20,
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
}

/// Why no answer was given. The input is never guessed at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// Damaged, unsupported, encrypted with a password, or a limit was hit. The text says which.
    Unparseable(&'static str),
    /// Well-formed, but with no pages.
    Empty,
    /// More pages than [`Limits::max_pages`].
    TooManyPages { pages: usize },
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
    fetched: usize,
    decoded: usize,
    objstms: HashMap<u32, ObjStm>,
}

impl Doc<'_> {
    fn spend(&mut self) -> Result<(), Failure> {
        self.fetched += 1;
        if self.fetched > self.limits.max_objects {
            return Err(Failure::Unparseable("object budget exceeded"));
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
                match Lexer::new(self.data, offset).parse_indirect() {
                    Some((n, obj)) if n == num => Ok(obj),
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
        Lexer::new(&stm.data, start)
            .parse_obj()
            .ok_or(Failure::Unparseable("unreadable compressed object"))
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
            return Err(Failure::Unparseable("too many objects in an object stream"));
        }
        let remaining = self.limits.max_decompressed.saturating_sub(self.decoded);
        let bytes =
            stream::decode(&dict, &self.data[range], remaining).map_err(Failure::Unparseable)?;
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
        Ok(ObjStm {
            data: bytes,
            first,
            index,
        })
    }

    /// Follows references until a direct object is reached.
    pub(crate) fn resolve(&mut self, obj: &Obj) -> Result<Obj, Failure> {
        let mut current = obj.clone();
        for _ in 0..16 {
            match current {
                Obj::Ref(n, _) => current = self.fetch(n)?,
                other => return Ok(other),
            }
        }
        Err(Failure::Unparseable("reference chain too long"))
    }
}

/// Reads the page count and page sizes of `data`.
pub fn probe(data: &[u8], limits: &Limits) -> Result<Probe, Failure> {
    let xref = xref::read(data, limits)?;
    let decoded = xref.decoded;
    let mut doc = Doc {
        data,
        xref,
        limits,
        fetched: 0,
        decoded,
        objstms: HashMap::new(),
    };
    let (sizes, count) = pages::collect(&mut doc, limits.max_pages)?;
    if sizes.len() > limits.max_pages {
        return Err(Failure::TooManyPages {
            pages: count.unwrap_or(sizes.len()),
        });
    }
    if sizes.is_empty() {
        return Err(Failure::Empty);
    }
    Ok(Probe {
        pages: sizes.len(),
        sizes,
    })
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
            Err(Failure::Unparseable("decompressed data too large"))
        );
    }

    #[test]
    fn garbage_and_empty_input_fail_closed() {
        assert!(probe(b"", &Limits::default()).is_err());
        assert!(probe(b"not a pdf at all", &Limits::default()).is_err());
    }
}
