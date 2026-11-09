//! Decoding stream data: the filter chain (Flate only for now) and PNG predictors.
use crate::inflate;
use crate::lexer::{Dict, Obj};
use std::borrow::Cow;

type Res<T> = Result<T, &'static str>;

/// Decodes `raw` through the stream dictionary's `/Filter` chain, writing at most `max_out` bytes per stage.
pub(crate) fn decode(dict: &Dict, raw: &[u8], max_out: usize) -> Res<Vec<u8>> {
    let filters: Vec<&[u8]> = match dict.get(b"Filter") {
        None | Some(Obj::Null) => Vec::new(),
        Some(Obj::Name(n)) => vec![n.as_slice()],
        Some(Obj::Array(items)) => items
            .iter()
            .map(|o| match o {
                Obj::Name(n) => Ok(n.as_slice()),
                _ => Err("unsupported /Filter entry"),
            })
            .collect::<Res<_>>()?,
        Some(_) => return Err("unsupported /Filter"),
    };
    let parms: Vec<Option<&Dict>> = match dict.get(b"DecodeParms") {
        None | Some(Obj::Null) => Vec::new(),
        Some(Obj::Dict(d)) => vec![Some(d)],
        Some(Obj::Array(items)) => items
            .iter()
            .map(|o| match o {
                Obj::Dict(d) => Ok(Some(d)),
                Obj::Null => Ok(None),
                _ => Err("indirect /DecodeParms are not supported yet"),
            })
            .collect::<Res<_>>()?,
        Some(_) => return Err("indirect /DecodeParms are not supported yet"),
    };
    let mut data: Cow<[u8]> = Cow::Borrowed(raw);
    for (i, name) in filters.iter().enumerate() {
        if *name != b"FlateDecode" && *name != b"Fl" {
            return Err("unsupported filter");
        }
        let inflated = inflate::zlib(&data, max_out)?;
        data = Cow::Owned(match parms.get(i).copied().flatten() {
            Some(p) => predict(p, inflated)?,
            None => inflated,
        });
    }
    Ok(data.into_owned())
}

/// Undoes a PNG predictor (`/Predictor` 10 to 15). Predictor 2 (TIFF) is not supported yet.
fn predict(parms: &Dict, data: Vec<u8>) -> Res<Vec<u8>> {
    let int = |key: &[u8], default: i64| match parms.get(key) {
        None => Ok(default),
        Some(o) => o.as_int().ok_or("bad /DecodeParms entry"),
    };
    let predictor = int(b"Predictor", 1)?;
    if predictor == 1 {
        return Ok(data);
    }
    if !(10..=15).contains(&predictor) {
        return Err("unsupported predictor");
    }
    let (colors, bpc, columns) = (
        int(b"Colors", 1)?,
        int(b"BitsPerComponent", 8)?,
        int(b"Columns", 1)?,
    );
    if !(1..=64).contains(&colors)
        || ![1, 2, 4, 8, 16].contains(&bpc)
        || !(1..=(1 << 24)).contains(&columns)
    {
        return Err("bad predictor parameters");
    }
    let (colors, bpc, columns) = (colors as usize, bpc as usize, columns as usize);
    let bpp = (colors * bpc).div_ceil(8);
    let row = (colors * bpc * columns).div_ceil(8);
    let stride = row + 1;
    let mut out = Vec::with_capacity(data.len() / stride * row);
    let mut prev = vec![0u8; row];
    let mut cur = vec![0u8; row];
    for line in data.chunks_exact(stride) {
        let (tag, bytes) = (line[0], &line[1..]);
        for i in 0..row {
            let a = if i >= bpp { cur[i - bpp] } else { 0 };
            let b = prev[i];
            let c = if i >= bpp { prev[i - bpp] } else { 0 };
            let x = bytes[i];
            cur[i] = match tag {
                0 => x,
                1 => x.wrapping_add(a),
                2 => x.wrapping_add(b),
                3 => x.wrapping_add(((u16::from(a) + u16::from(b)) / 2) as u8),
                4 => x.wrapping_add(paeth(a, b, c)),
                _ => return Err("bad PNG predictor tag"),
            };
        }
        out.extend_from_slice(&cur);
        std::mem::swap(&mut prev, &mut cur);
    }
    Ok(out)
}

fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let (ia, ib, ic) = (i16::from(a), i16::from(b), i16::from(c));
    let p = ia + ib - ic;
    let (pa, pb, pc) = ((p - ia).abs(), (p - ib).abs(), (p - ic).abs());
    if pa <= pb && pa <= pc {
        a
    } else if pb <= pc {
        b
    } else {
        c
    }
}
