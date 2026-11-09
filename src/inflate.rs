//! A small, safe, bounded zlib/DEFLATE decoder (RFC 1950 and 1951). It never writes more than `max_out`
//! bytes (plus at most one match of 258) and never indexes out of range.

type Res<T> = Result<T, &'static str>;

const LBASE: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258,
];
const LEXT: [u8; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
const DBASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
const DEXT: [u8; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];
const CLORDER: [usize; 19] = [
    16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
];

/// Decodes a zlib stream (2-byte header, DEFLATE data; the Adler-32 trailer is not checked, as in pdf.js).
pub(crate) fn zlib(input: &[u8], max_out: usize) -> Res<Vec<u8>> {
    let (Some(&cmf), Some(&flg)) = (input.first(), input.get(1)) else {
        return Err("zlib stream too short");
    };
    if cmf & 0x0f != 8 || ((u16::from(cmf) << 8) | u16::from(flg)) % 31 != 0 || flg & 0x20 != 0 {
        return Err("bad zlib header");
    }
    inflate(&input[2..], max_out)
}

struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
    bitbuf: u32,
    bitcnt: u32,
}

impl Reader<'_> {
    /// The next `n` (at most 16) bits, least significant first.
    fn bits(&mut self, n: u32) -> Res<u32> {
        while self.bitcnt < n {
            let b = *self
                .data
                .get(self.pos)
                .ok_or("unexpected end of deflate data")?;
            self.pos += 1;
            self.bitbuf |= u32::from(b) << self.bitcnt;
            self.bitcnt += 8;
        }
        let v = self.bitbuf & ((1u32 << n) - 1);
        self.bitbuf >>= n;
        self.bitcnt -= n;
        Ok(v)
    }

    fn decode(&mut self, h: &Huffman) -> Res<u16> {
        let (mut code, mut first, mut index) = (0i32, 0i32, 0i32);
        for len in 1..16 {
            code |= self.bits(1)? as i32;
            let count = i32::from(h.count[len]);
            if code - count < first {
                return h
                    .symbol
                    .get((index + (code - first)) as usize)
                    .copied()
                    .ok_or("bad huffman code");
            }
            index += count;
            first += count;
            first <<= 1;
            code <<= 1;
        }
        Err("invalid huffman code")
    }
}

struct Huffman {
    count: [u16; 16],
    symbol: Vec<u16>,
}

/// A canonical Huffman code from code lengths (each at most 15). Over-subscribed codes are rejected.
fn build(lengths: &[u8]) -> Res<Huffman> {
    let mut count = [0u16; 16];
    for &l in lengths {
        count[usize::from(l)] += 1;
    }
    count[0] = 0;
    let mut left = 1i32;
    for c in &count[1..] {
        left <<= 1;
        left -= i32::from(*c);
        if left < 0 {
            return Err("over-subscribed huffman code");
        }
    }
    let mut offs = [0u16; 16];
    for len in 1..15 {
        offs[len + 1] = offs[len] + count[len];
    }
    let mut symbol = vec![0u16; lengths.len()];
    for (sym, &l) in lengths.iter().enumerate() {
        if l != 0 {
            let slot = &mut offs[usize::from(l)];
            symbol[usize::from(*slot)] = sym as u16;
            *slot += 1;
        }
    }
    Ok(Huffman { count, symbol })
}

pub(crate) fn inflate(input: &[u8], max_out: usize) -> Res<Vec<u8>> {
    let mut r = Reader {
        data: input,
        pos: 0,
        bitbuf: 0,
        bitcnt: 0,
    };
    let mut out: Vec<u8> = Vec::new();
    loop {
        let last = r.bits(1)?;
        match r.bits(2)? {
            0 => stored(&mut r, &mut out, max_out)?,
            1 => {
                let mut lengths = [8u8; 288];
                lengths[144..256].fill(9);
                lengths[256..280].fill(7);
                let lit = build(&lengths)?;
                let dist = build(&[5u8; 30])?;
                codes(&mut r, &mut out, &lit, &dist, max_out)?;
            }
            2 => {
                let (lit, dist) = dynamic_tables(&mut r)?;
                codes(&mut r, &mut out, &lit, &dist, max_out)?;
            }
            _ => return Err("bad deflate block type"),
        }
        if last == 1 {
            return Ok(out);
        }
    }
}

fn stored(r: &mut Reader, out: &mut Vec<u8>, max_out: usize) -> Res<()> {
    r.bitbuf = 0;
    r.bitcnt = 0;
    let head = r
        .data
        .get(r.pos..r.pos + 4)
        .ok_or("unexpected end of deflate data")?;
    let len = usize::from(u16::from_le_bytes([head[0], head[1]]));
    let nlen = u16::from_le_bytes([head[2], head[3]]);
    if nlen != !(len as u16) {
        return Err("bad stored block length");
    }
    r.pos += 4;
    let body = r
        .data
        .get(r.pos..r.pos + len)
        .ok_or("unexpected end of deflate data")?;
    if out.len() + len > max_out {
        return Err("decompressed data too large");
    }
    out.extend_from_slice(body);
    r.pos += len;
    Ok(())
}

fn dynamic_tables(r: &mut Reader) -> Res<(Huffman, Huffman)> {
    let nlen = r.bits(5)? as usize + 257;
    let ndist = r.bits(5)? as usize + 1;
    let ncode = r.bits(4)? as usize + 4;
    if nlen > 286 || ndist > 30 {
        return Err("bad deflate table sizes");
    }
    let mut cl = [0u8; 19];
    for &slot in CLORDER.iter().take(ncode) {
        cl[slot] = r.bits(3)? as u8;
    }
    let lencode = build(&cl)?;
    let mut lengths = [0u8; 316];
    let total = nlen + ndist;
    let mut i = 0;
    while i < total {
        let sym = r.decode(&lencode)?;
        if sym < 16 {
            lengths[i] = sym as u8;
            i += 1;
            continue;
        }
        let (prev, rep) = match sym {
            16 => (
                *i.checked_sub(1)
                    .map(|p| &lengths[p])
                    .ok_or("repeat with no previous length")?,
                3 + r.bits(2)?,
            ),
            17 => (0, 3 + r.bits(3)?),
            _ => (0, 11 + r.bits(7)?),
        };
        let rep = rep as usize;
        if i + rep > total {
            return Err("too many code lengths");
        }
        lengths[i..i + rep].fill(prev);
        i += rep;
    }
    if lengths[256] == 0 {
        return Err("no end-of-block code");
    }
    Ok((build(&lengths[..nlen])?, build(&lengths[nlen..total])?))
}

fn codes(
    r: &mut Reader,
    out: &mut Vec<u8>,
    lit: &Huffman,
    dist: &Huffman,
    max_out: usize,
) -> Res<()> {
    loop {
        let sym = usize::from(r.decode(lit)?);
        if sym < 256 {
            out.push(sym as u8);
        } else if sym == 256 {
            return Ok(());
        } else {
            let s = sym - 257;
            let (&lb, &le) = (LBASE.get(s).ok_or("bad length code")?, &LEXT[s]);
            let len = usize::from(lb) + r.bits(u32::from(le))? as usize;
            let d = usize::from(r.decode(dist)?);
            let (&db, &de) = (DBASE.get(d).ok_or("bad distance code")?, &DEXT[d]);
            let distance = usize::from(db) + r.bits(u32::from(de))? as usize;
            if distance > out.len() {
                return Err("distance too far back");
            }
            for _ in 0..len {
                out.push(out[out.len() - distance]);
            }
        }
        if out.len() > max_out {
            return Err("decompressed data too large");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unhex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    // Vectors made with Python's zlib.compress (level 9, and level 0 for the stored block).
    const FIXED: &str = "78dacb48cdc9c957c8c04e0200a3100ae5";
    const STORED: &str = "7801010c00f3ff73746f7265642062797465731fcf04d9";
    const DYNAMIC: &str = "78da85945d8ec2300c84afd2ab25da68412abb48f4a9a747e0b6fe2699c24bda24fe198fc769f7c775feff9bdaf65ddb52a6df72bb9569b9bcfed76389d358df07bbcf4f9b5fbb6577897d99ef9732551abebde23c2c6b3ac5b1c46de7d83265f8c59e08236aac7d967ae43e7e180650044fba558d9db8704ea78cb501df2e2353b856573aeb6a8664d30d614b8b240723d2fe1bb7995bb280c8cd82f536d39c6cd96e3a080a0cc50a09d23856927746ed0e249393b7b041bb4715080906b7a6636b2c8da6ef43d140a231fac6c1bfd775d5c257ab9814b053a01b552fcfe33a25e73421ece9ae8e250321fa4fe270a98c1b04a0a11f04061e46e174e46c407651f3f33572370837c4479d6cc8308126877f8d947b3ebc9f3498134ac81290b9bf0f81604a6d7328c5843a148faa32e6103f01c6578061";
    // One million zero bytes.
    const BOMB: &str = "78daedc101010000008220ffaf6e4840010000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000af0643210001";

    #[test]
    fn decodes_a_fixed_huffman_block() {
        assert_eq!(
            zlib(&unhex(FIXED), 1000).unwrap(),
            b"hello hello hello hello hello"
        );
    }

    #[test]
    fn decodes_a_stored_block() {
        assert_eq!(zlib(&unhex(STORED), 1000).unwrap(), b"stored bytes");
    }

    #[test]
    fn decodes_a_dynamic_huffman_block() {
        let words = [
            "alpha", "beta", "gamma", "delta", "epsilon", "zeta", "eta", "theta",
        ];
        let mut x: u64 = 12345;
        let mut expected = Vec::new();
        for _ in 0..300 {
            x = (x * 1_103_515_245 + 12345) & 0x7fff_ffff;
            expected.extend(words[((x >> 16) % 8) as usize].bytes());
            expected.push(b' ');
        }
        assert_eq!(zlib(&unhex(DYNAMIC), 100_000).unwrap(), expected);
    }

    #[test]
    fn stops_at_the_output_limit() {
        let bomb = unhex(BOMB);
        assert!(bomb.len() < 2000);
        assert_eq!(zlib(&bomb, 10_000), Err("decompressed data too large"));
        assert_eq!(zlib(&bomb, 2_000_000).unwrap().len(), 1_000_000);
    }

    #[test]
    fn rejects_garbage_and_truncation() {
        assert!(zlib(b"", 100).is_err());
        assert!(zlib(b"\x78\xda\xff\xff\xff", 100).is_err());
        let z = unhex(DYNAMIC);
        assert!(zlib(&z[..z.len() / 2], 100_000).is_err());
    }
}
