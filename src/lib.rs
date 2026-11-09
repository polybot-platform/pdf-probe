//! pdf-probe: how many pages does an untrusted PDF have, and how big is each page?
//!
//! **First thin slice.** Classic cross-reference tables with `/Prev`, uncompressed objects, the page tree,
//! and page sizes. Cross-reference streams, object streams, Flate, encryption, hybrid files and the
//! recovery scan are not implemented yet; a file that needs any of them fails closed. See
//! `docs/how-it-works.md` for the full design.
#![forbid(unsafe_code)]

mod lexer;
mod pages;
mod xref;

use lexer::{Lexer, Obj};
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
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            max_pages: 10_000,
            max_objects: 1_000_000,
            max_xref_sections: 256,
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

pub(crate) struct Doc<'a> {
    data: &'a [u8],
    pub(crate) xref: Xref,
    limits: &'a Limits,
    fetched: usize,
}

impl Doc<'_> {
    /// The object with this number, or `null` if the cross-reference has no usable entry for it.
    pub(crate) fn fetch(&mut self, num: u32) -> Result<Obj, Failure> {
        let offset = match self.xref.entries.get(&num) {
            Some(Entry::Offset(o)) => *o,
            _ => return Ok(Obj::Null),
        };
        self.fetched += 1;
        if self.fetched > self.limits.max_objects {
            return Err(Failure::Unparseable("object budget exceeded"));
        }
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
    let mut doc = Doc {
        data,
        xref,
        limits,
        fetched: 0,
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

    #[test]
    fn garbage_and_empty_input_fail_closed() {
        assert!(probe(b"", &Limits::default()).is_err());
        assert!(probe(b"not a pdf at all", &Limits::default()).is_err());
    }
}
