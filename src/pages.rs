//! The page tree: count, inheritance, and the effective size of each page.
use crate::lexer::{Dict, Obj};
use crate::meter::cost;
use crate::{Doc, Failure, Fault, PageSize, is_entry_error, is_limit_error};

/// Deepest page-tree nesting followed.
const MAX_TREE_DEPTH: usize = 64;

fn fail<T>(why: &'static str) -> Result<T, Fault> {
    Err(Fault::from(Failure::Unparseable(why)))
}

/// A fault that sends the reader to the recovery scan, as pdf.js does when the root or the page tree
/// root is not a dictionary.
fn rescan(f: Failure) -> Fault {
    Fault {
        recover: !is_limit_error(&f),
        failure: f,
    }
}

/// An error fetching part of the page tree. Only for the first and the last page does pdf.js treat a bad
/// cross-reference entry as a reason to rescan the file; elsewhere it is an error.
fn tree_fault(f: Failure, pages_so_far: usize, last: Option<usize>) -> Fault {
    let at_edge = pages_so_far == 0 || Some(pages_so_far) == last;
    Fault {
        recover: at_edge && is_entry_error(&f) && !is_limit_error(&f),
        failure: f,
    }
}

/// Attributes a page inherits from its ancestors; the nearest one wins.
#[derive(Clone, Default)]
struct Inherit {
    media: Option<Obj>,
    crop: Option<Obj>,
    rotate: Option<Obj>,
}

pub(crate) struct Collected {
    pub sizes: Vec<PageSize>,
    pub count: Option<usize>,
    pub catalog_version: Option<(u8, u8)>,
    pub cropped: bool,
}

fn parse_version(b: &[u8]) -> Option<(u8, u8)> {
    if b.len() == 3 && matches!(b[0], b'1' | b'2') && b[1] == b'.' && b[2].is_ascii_digit() {
        Some((b[0] - b'0', b[2] - b'0'))
    } else {
        None
    }
}

/// Walks the tree in document order and returns up to `want` page sizes.
pub(crate) fn collect(doc: &mut Doc, cap: usize) -> Result<Collected, Fault> {
    let root_obj = doc
        .xref
        .trailer
        .get(b"Root")
        .cloned()
        .ok_or(rescan(Failure::Unparseable("no /Root")))?;
    let catalog = match doc.resolve(&root_obj) {
        Ok(Obj::Dict(c)) => c,
        Ok(_) => {
            return Err(rescan(Failure::Unparseable(
                "the catalog is not a dictionary",
            )));
        }
        Err(f) => return Err(rescan(f)),
    };
    let catalog_version = match catalog.get(b"Version").cloned() {
        Some(v) => doc.resolve(&v).ok().and_then(|o| match o {
            Obj::Name(n) => parse_version(&n),
            _ => None,
        }),
        None => None,
    };
    let Some(pages_obj) = catalog.get(b"Pages").cloned() else {
        return Err(rescan(Failure::Unparseable("the catalog has no /Pages")));
    };
    let root_num = if let Obj::Ref(n, _) = pages_obj {
        Some(n)
    } else {
        None
    };
    let root = match doc.resolve(&pages_obj) {
        Ok(Obj::Dict(r)) => r,
        Ok(_) => {
            return Err(rescan(Failure::Unparseable(
                "the page tree root is not a dictionary",
            )));
        }
        Err(f) => return Err(rescan(f)),
    };

    // A usable /Count is a non-negative integer. It is trusted, except that a tree with fewer pages gives
    // the real, lower count (pdf.js does the same).
    let count = match root.get(b"Count").cloned() {
        Some(c) => doc
            .resolve(&c)?
            .as_int()
            .and_then(|n| usize::try_from(n).ok()),
        None => None,
    };
    // Never read more than one page past the cap: that is enough to know the cap was exceeded.
    let limit = count.unwrap_or(usize::MAX).min(cap.saturating_add(1));
    let mut out = Vec::new();
    let mut cropped = false;
    let mut path: Vec<u32> = root_num.into_iter().collect();
    let last = count.map(|c| c.saturating_sub(1));
    walk(
        doc,
        &root,
        &Inherit::default(),
        0,
        &mut path,
        &mut out,
        &mut cropped,
        (limit, last),
    )?;
    Ok(Collected {
        sizes: out,
        count,
        catalog_version,
        cropped,
    })
}

#[allow(clippy::too_many_arguments)]
fn walk(
    doc: &mut Doc,
    node: &Dict,
    inherit: &Inherit,
    depth: usize,
    path: &mut Vec<u32>,
    out: &mut Vec<PageSize>,
    cropped: &mut bool,
    (limit, last): (usize, Option<usize>),
) -> Result<(), Fault> {
    if out.len() >= limit {
        return Ok(());
    }
    if depth > MAX_TREE_DEPTH {
        return fail("the page tree is too deep");
    }
    let Some(kids) = node.get(b"Kids").cloned() else {
        // No /Kids: this node is a page.
        let (size, page_cropped) = page_size(doc, node, inherit)?;
        *cropped |= page_cropped;
        out.push(size);
        return Ok(());
    };
    let kids = doc
        .resolve(&kids)
        .map_err(|f| tree_fault(f, out.len(), last))?;
    let Obj::Array(kids) = kids else {
        return fail("/Kids is not an array");
    };
    let mut here = inherit.clone();
    for (key, slot) in [
        (&b"MediaBox"[..], &mut here.media),
        (&b"CropBox"[..], &mut here.crop),
        (&b"Rotate"[..], &mut here.rotate),
    ] {
        if let Some(v) = node.get(key) {
            *slot = Some(v.clone());
        }
    }
    for kid in kids.iter() {
        if out.len() >= limit {
            break;
        }
        doc.meter.charge(cost::NODE)?;
        match kid {
            Obj::Ref(n, _) => {
                if path.contains(n) {
                    return fail("the page tree loops");
                }
                let fetched = doc.fetch(*n).map_err(|f| tree_fault(f, out.len(), last))?;
                let Obj::Dict(child) = fetched else {
                    return fail("a page tree kid is not a dictionary");
                };
                path.push(*n);
                walk(
                    doc,
                    &child,
                    &here,
                    depth + 1,
                    path,
                    out,
                    cropped,
                    (limit, last),
                )?;
                path.pop();
            }
            Obj::Dict(child) => walk(
                doc,
                child,
                &here,
                depth + 1,
                path,
                out,
                cropped,
                (limit, last),
            )?,
            _ => return fail("a page tree kid is not a reference"),
        }
    }
    Ok(())
}

type Rect = [f64; 4];

/// A four-number array, normalised, with a positive width and height; `None` otherwise.
fn rect(doc: &mut Doc, obj: Option<&Obj>) -> Result<Option<Rect>, Failure> {
    let Some(obj) = obj else { return Ok(None) };
    let Obj::Array(items) = doc.resolve(obj)? else {
        return Ok(None);
    };
    if items.len() != 4 {
        return Ok(None);
    }
    let mut v = [0.0; 4];
    for (slot, item) in v.iter_mut().zip(items.iter()) {
        match doc.resolve(item)?.as_f64() {
            Some(n) => *slot = n,
            None => return Ok(None),
        }
    }
    let r = [
        v[0].min(v[2]),
        v[1].min(v[3]),
        v[0].max(v[2]),
        v[1].max(v[3]),
    ];
    Ok((r[2] - r[0] > 0.0 && r[3] - r[1] > 0.0).then_some(r))
}

fn number(doc: &mut Doc, obj: Option<&Obj>) -> Result<Option<f64>, Failure> {
    match obj {
        Some(o) => Ok(doc.resolve(o)?.as_f64()),
        None => Ok(None),
    }
}

fn page_size(doc: &mut Doc, page: &Dict, inherit: &Inherit) -> Result<(PageSize, bool), Failure> {
    let own_or =
        |key: &[u8], inherited: &Option<Obj>| page.get(key).cloned().or_else(|| inherited.clone());
    // A missing or invalid MediaBox is US Letter, as in pdf.js.
    let media = rect(doc, own_or(b"MediaBox", &inherit.media).as_ref())?
        .unwrap_or([0.0, 0.0, 612.0, 792.0]);
    let crop = rect(doc, own_or(b"CropBox", &inherit.crop).as_ref())?.unwrap_or(media);
    // The visible box is CropBox ∩ MediaBox; an empty intersection falls back to the MediaBox (the larger,
    // safer answer for a size limit), never to a zero-size page.
    let view = if crop == media {
        media
    } else {
        let i = [
            crop[0].max(media[0]),
            crop[1].max(media[1]),
            crop[2].min(media[2]),
            crop[3].min(media[3]),
        ];
        if i[2] - i[0] > 0.0 && i[3] - i[1] > 0.0 {
            i
        } else {
            media
        }
    };
    let cropped = view != media;
    let rotate = match number(doc, own_or(b"Rotate", &inherit.rotate).as_ref())? {
        Some(r) if r.fract() == 0.0 && r.abs() < 1.0e9 && (r as i64) % 90 == 0 => {
            ((r as i64 % 360) + 360) % 360
        }
        _ => 0,
    };
    let unit = match number(doc, page.get(b"UserUnit"))? {
        Some(u) if u > 0.0 => u,
        _ => 1.0,
    };
    let (mut width, mut height) = ((view[2] - view[0]) * unit, (view[3] - view[1]) * unit);
    if rotate == 90 || rotate == 270 {
        std::mem::swap(&mut width, &mut height);
    }
    Ok((PageSize { width, height }, cropped))
}
