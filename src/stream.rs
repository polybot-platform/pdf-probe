//! Decoding stream data: the filter chain (Flate only for now) and PNG predictors.
use crate::lexer::{Dict, Obj};
use crate::meter::{Meter, cost};
use crate::{Failure, Limit, inflate};
use std::borrow::Cow;

type Res<T> = Result<T, Failure>;

fn bad<T>(why: &'static str) -> Res<T> {
    Err(Failure::Unparseable(why))
}

/// An inflate error: output past the limit is a limit, anything else is damage.
fn from_inflate(why: &'static str) -> Failure {
    if why == inflate::TOO_LARGE {
        Failure::LimitExceeded(Limit::Decompressed)
    } else {
        Failure::Unparseable(why)
    }
}

/// Decodes `raw` through the stream dictionary's `/Filter` chain, writing at most `max_out` bytes per stage.
/// Charges the meter for the work. With no filter to apply, `raw` is returned as given, owned or borrowed.
pub(crate) fn decode<'a>(
    dict: &Dict,
    raw: Cow<'a, [u8]>,
    max_out: usize,
    meter: &Meter,
) -> Res<Cow<'a, [u8]>> {
    let filters: &[Obj] = match dict.get(b"Filter") {
        None | Some(Obj::Null) => &[],
        Some(o @ Obj::Name(_)) => std::slice::from_ref(o),
        Some(Obj::Array(items)) => items,
        Some(_) => return bad("unsupported /Filter"),
    };
    if filters.iter().any(|o| !matches!(o, Obj::Name(_))) {
        return bad("unsupported /Filter entry");
    }
    let parms: &[Obj] = match dict.get(b"DecodeParms") {
        None | Some(Obj::Null) => &[],
        Some(o @ Obj::Dict(_)) => std::slice::from_ref(o),
        Some(Obj::Array(items)) => items,
        Some(_) => return bad("indirect /DecodeParms are not supported yet"),
    };
    if parms.iter().any(|o| !matches!(o, Obj::Dict(_) | Obj::Null)) {
        return bad("indirect /DecodeParms are not supported yet");
    }
    let mut data = raw;
    for (i, filter) in filters.iter().enumerate() {
        let Obj::Name(name) = filter else {
            unreachable!()
        };
        if &name[..] == b"Crypt" {
            continue; // handled by the security handler before decoding
        }
        if &name[..] != b"FlateDecode" && &name[..] != b"Fl" {
            return bad("unsupported filter");
        }
        meter.charge(cost::INFLATE_IN * data.len() as u64)?;
        let inflated = inflate::zlib(&data, max_out).map_err(from_inflate)?;
        meter.charge(cost::INFLATE_OUT * inflated.len() as u64)?;
        data = Cow::Owned(match parms.get(i) {
            Some(Obj::Dict(p)) => predict(p, inflated, meter)?,
            _ => inflated,
        });
    }
    // Each inflate stage is already capped at `max_out` and predictors only shrink their input. Unfiltered
    // data is the file's own bytes, so it is not counted against the decompression limit.
    Ok(data)
}

/// Undoes a PNG predictor (`/Predictor` 10 to 15). Predictor 2 (TIFF) is not supported yet.
fn predict(parms: &Dict, mut data: Vec<u8>, meter: &Meter) -> Res<Vec<u8>> {
    let int = |key: &[u8], default: i64| match parms.get(key) {
        None => Ok(default),
        Some(o) => o
            .as_int()
            .ok_or(Failure::Unparseable("bad /DecodeParms entry")),
    };
    let predictor = int(b"Predictor", 1)?;
    if predictor == 1 {
        return Ok(data);
    }
    if !(10..=15).contains(&predictor) {
        return bad("unsupported predictor");
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
        return bad("bad predictor parameters");
    }
    meter.charge(cost::PREDICT * data.len() as u64)?;
    let bits_per_pixel = (colors as u64) * (bpc as u64);
    let bpp = bits_per_pixel.div_ceil(8) as usize;
    let row_u64 = bits_per_pixel
        .checked_mul(columns as u64)
        .ok_or(Failure::Unparseable("bad predictor parameters"))?
        .div_ceil(8);
    let Ok(row) = usize::try_from(row_u64) else {
        return bad("bad predictor parameters");
    };
    let Some(stride) = row.checked_add(1) else {
        return bad("bad predictor parameters");
    };
    if data.len() < stride {
        return Ok(Vec::new());
    }
    // Removing each tag leaves the output behind the unread input, so rows can be decoded in place.
    let rows = data.len() / stride;
    for line in 0..rows {
        let read = line * stride;
        let write = line * row;
        let tag = data[read];
        for i in 0..row {
            let a = if i >= bpp { data[write + i - bpp] } else { 0 };
            let b = if line > 0 { data[write + i - row] } else { 0 };
            let c = if line > 0 && i >= bpp {
                data[write + i - row - bpp]
            } else {
                0
            };
            let x = data[read + 1 + i];
            data[write + i] = match tag {
                0 => x,
                1 => x.wrapping_add(a),
                2 => x.wrapping_add(b),
                3 => x.wrapping_add(((u16::from(a) + u16::from(b)) / 2) as u8),
                4 => x.wrapping_add(paeth(a, b, c)),
                _ => return bad("bad PNG predictor tag"),
            };
        }
    }
    data.truncate(rows * row);
    Ok(data)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::Lexer;

    fn dict(source: &str) -> Dict {
        let Some(Obj::Dict(d)) =
            Lexer::new(source.as_bytes(), 0, &Meter::new(u64::MAX)).parse_obj()
        else {
            panic!("dictionary");
        };
        d
    }

    #[test]
    fn png_predictors_decode_multiple_rows_and_bytes_per_pixel() {
        let expected = [10, 20, 250, 240, 15, 30, 5, 20, 20, 40, 60, 80];
        // Encoded independently, including wrapping differences and ties in Paeth's prediction.
        let cases = [
            (
                1,
                [0, 10, 20, 250, 240, 0, 15, 30, 5, 20, 0, 20, 40, 60, 80],
            ),
            (
                1,
                [1, 10, 10, 230, 246, 1, 15, 15, 231, 15, 1, 20, 20, 20, 20],
            ),
            (1, [2, 10, 20, 250, 240, 2, 5, 10, 11, 36, 2, 5, 10, 55, 60]),
            (
                1,
                [3, 10, 15, 240, 115, 3, 10, 13, 121, 154, 3, 13, 15, 38, 40],
            ),
            (1, [4, 10, 10, 230, 246, 4, 5, 10, 11, 15, 4, 5, 10, 55, 20]),
            (
                2,
                [0, 10, 20, 250, 240, 0, 15, 30, 5, 20, 0, 20, 40, 60, 80],
            ),
            (
                2,
                [1, 10, 20, 240, 220, 1, 15, 30, 246, 246, 1, 20, 40, 40, 40],
            ),
            (2, [2, 10, 20, 250, 240, 2, 5, 10, 11, 36, 2, 5, 10, 55, 60]),
            (
                2,
                [3, 10, 20, 245, 230, 3, 10, 20, 129, 141, 3, 13, 25, 48, 50],
            ),
            (2, [4, 10, 20, 240, 220, 4, 5, 10, 11, 36, 4, 5, 10, 55, 50]),
        ];
        for (colors, encoded) in cases {
            let parms = dict(&format!(
                "<< /Predictor 15 /Colors {colors} /Columns {} >>",
                4 / colors
            ));
            assert_eq!(
                predict(&parms, encoded.to_vec(), &Meter::new(u64::MAX)).unwrap(),
                expected
            );
        }
    }

    #[test]
    fn predictors_keep_short_rows_and_invalid_tags_compatible() {
        let parms = dict("<< /Predictor 12 /Columns 4 >>");
        let meter = Meter::new(u64::MAX);
        assert!(predict(&parms, vec![0, 1], &meter).unwrap().is_empty());
        assert_eq!(
            predict(&parms, vec![0, 1, 2, 3, 4, 255, 0], &meter).unwrap(),
            [1, 2, 3, 4]
        );
        assert_eq!(
            predict(&parms, vec![5, 1, 2, 3, 4], &meter),
            Err(Failure::Unparseable("bad PNG predictor tag"))
        );
    }

    #[test]
    fn unfiltered_streams_are_borrowed_and_not_capped() {
        let meter = Meter::new(u64::MAX);
        for source in ["<< >>", "<< /Filter /Crypt >>", "<< /Filter [] >>"] {
            let d = dict(source);
            for max_out in [3, 2, 0] {
                assert!(matches!(
                    decode(&d, Cow::Borrowed(b"abc"), max_out, &meter).unwrap(),
                    Cow::Borrowed(b"abc")
                ));
            }
        }
    }

    #[test]
    fn unfiltered_owned_input_is_passed_through_without_a_copy() {
        let data = b"decrypted".to_vec();
        let at = data.as_ptr();
        let out = decode(&dict("<< >>"), Cow::Owned(data), 0, &Meter::new(u64::MAX)).unwrap();
        assert!(matches!(&out, Cow::Owned(v) if v.as_ptr() == at));
    }

    #[test]
    fn filter_parameters_are_validated_before_decoding() {
        let d = dict("<< /Filter [/FlateDecode] /DecodeParms [null 1] >>");
        assert_eq!(
            decode(&d, Cow::Borrowed(b"bad zlib"), 10, &Meter::new(u64::MAX)),
            Err(Failure::Unparseable(
                "indirect /DecodeParms are not supported yet"
            ))
        );
        let d = dict("<< /Filter [/FlateDecode 1] >>");
        assert_eq!(
            decode(&d, Cow::Borrowed(b"bad zlib"), 10, &Meter::new(u64::MAX)),
            Err(Failure::Unparseable("unsupported /Filter entry"))
        );
    }
}
