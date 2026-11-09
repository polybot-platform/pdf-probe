//! The page tree: count, inheritance, and the effective size of each page.
use crate::lexer::{Dict, Obj};
use crate::{Doc, Failure, PageSize};

/// Deepest page-tree nesting followed.
const MAX_TREE_DEPTH: usize = 64;

fn fail<T>(why: &'static str) -> Result<T, Failure> {
    Err(Failure::Unparseable(why))
}

/// Attributes a page inherits from its ancestors; the nearest one wins.
#[derive(Clone, Default)]
struct Inherit {
    media: Option<Obj>,
    crop: Option<Obj>,
    rotate: Option<Obj>,
}

/// Walks the tree in document order and returns up to `want` page sizes.
pub(crate) fn collect(
    doc: &mut Doc,
    cap: usize,
) -> Result<(Vec<PageSize>, Option<usize>), Failure> {
    let root_obj = doc
        .xref
        .trailer
        .get(b"Root")
        .cloned()
        .ok_or(Failure::Unparseable("no /Root"))?;
    let Obj::Dict(catalog) = doc.resolve(&root_obj)? else {
        return fail("the catalog is not a dictionary");
    };
    let Some(pages_obj) = catalog.get(b"Pages").cloned() else {
        return fail("the catalog has no /Pages");
    };
    let root_num = if let Obj::Ref(n, _) = pages_obj {
        Some(n)
    } else {
        None
    };
    let Obj::Dict(root) = doc.resolve(&pages_obj)? else {
        return fail("the page tree root is not a dictionary");
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
    let mut path: Vec<u32> = root_num.into_iter().collect();
    walk(
        doc,
        &root,
        &Inherit::default(),
        0,
        &mut path,
        &mut out,
        limit,
    )?;
    Ok((out, count))
}

fn walk(
    doc: &mut Doc,
    node: &Dict,
    inherit: &Inherit,
    depth: usize,
    path: &mut Vec<u32>,
    out: &mut Vec<PageSize>,
    limit: usize,
) -> Result<(), Failure> {
    if out.len() >= limit {
        return Ok(());
    }
    if depth > MAX_TREE_DEPTH {
        return fail("the page tree is too deep");
    }
    let Some(kids) = node.get(b"Kids").cloned() else {
        // No /Kids: this node is a page.
        out.push(page_size(doc, node, inherit)?);
        return Ok(());
    };
    let Obj::Array(kids) = doc.resolve(&kids)? else {
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
    for kid in &kids {
        if out.len() >= limit {
            break;
        }
        match kid {
            Obj::Ref(n, _) => {
                if path.contains(n) {
                    return fail("the page tree loops");
                }
                let Obj::Dict(child) = doc.fetch(*n)? else {
                    return fail("a page tree kid is not a dictionary");
                };
                path.push(*n);
                walk(doc, &child, &here, depth + 1, path, out, limit)?;
                path.pop();
            }
            Obj::Dict(child) => walk(doc, child, &here, depth + 1, path, out, limit)?,
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
    for (slot, item) in v.iter_mut().zip(&items) {
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

fn page_size(doc: &mut Doc, page: &Dict, inherit: &Inherit) -> Result<PageSize, Failure> {
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
    Ok(PageSize { width, height })
}
