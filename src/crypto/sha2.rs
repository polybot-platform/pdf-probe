//! SHA-256, SHA-384 and SHA-512 (FIPS 180-4).
use super::sha2_tables::{H256, H384, H512, K256, K512};

fn compress256(h: &mut [u32; 8], chunk: &[u8]) {
    let mut w = [0u32; 64];
    for (x, b) in w.iter_mut().zip(chunk.chunks_exact(4)) {
        *x = u32::from_be_bytes([b[0], b[1], b[2], b[3]]);
    }
    for i in 16..64 {
        let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
        let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
        w[i] = w[i - 16]
            .wrapping_add(s0)
            .wrapping_add(w[i - 7])
            .wrapping_add(s1);
    }
    let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = *h;
    for i in 0..64 {
        let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
        let ch = (e & f) ^ (!e & g);
        let t1 = hh
            .wrapping_add(s1)
            .wrapping_add(ch)
            .wrapping_add(K256[i])
            .wrapping_add(w[i]);
        let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
        let maj = (a & b) ^ (a & c) ^ (b & c);
        let t2 = s0.wrapping_add(maj);
        hh = g;
        g = f;
        f = e;
        e = d.wrapping_add(t1);
        d = c;
        c = b;
        b = a;
        a = t1.wrapping_add(t2);
    }
    for (x, y) in h.iter_mut().zip([a, b, c, d, e, f, g, hh]) {
        *x = x.wrapping_add(y);
    }
}

pub(crate) fn sha256(data: &[u8]) -> [u8; 32] {
    let mut h = H256;
    let chunks = data.chunks_exact(64);
    let rem = chunks.remainder();
    for chunk in chunks {
        compress256(&mut h, chunk);
    }
    let mut tail = [0u8; 128];
    tail[..rem.len()].copy_from_slice(rem);
    tail[rem.len()] = 0x80;
    let total = if rem.len() < 56 { 64 } else { 128 };
    let bits = (data.len() as u64).wrapping_mul(8).to_be_bytes();
    tail[total - 8..total].copy_from_slice(&bits);
    for chunk in tail[..total].chunks_exact(64) {
        compress256(&mut h, chunk);
    }
    let mut out = [0u8; 32];
    for (o, w) in out.chunks_exact_mut(4).zip(h) {
        o.copy_from_slice(&w.to_be_bytes());
    }
    out
}

fn compress512(h: &mut [u64; 8], chunk: &[u8]) {
    let mut w = [0u64; 80];
    for (x, b) in w.iter_mut().zip(chunk.chunks_exact(8)) {
        *x = u64::from_be_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]);
    }
    for i in 16..80 {
        let s0 = w[i - 15].rotate_right(1) ^ w[i - 15].rotate_right(8) ^ (w[i - 15] >> 7);
        let s1 = w[i - 2].rotate_right(19) ^ w[i - 2].rotate_right(61) ^ (w[i - 2] >> 6);
        w[i] = w[i - 16]
            .wrapping_add(s0)
            .wrapping_add(w[i - 7])
            .wrapping_add(s1);
    }
    let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = *h;
    for i in 0..80 {
        let s1 = e.rotate_right(14) ^ e.rotate_right(18) ^ e.rotate_right(41);
        let ch = (e & f) ^ (!e & g);
        let t1 = hh
            .wrapping_add(s1)
            .wrapping_add(ch)
            .wrapping_add(K512[i])
            .wrapping_add(w[i]);
        let s0 = a.rotate_right(28) ^ a.rotate_right(34) ^ a.rotate_right(39);
        let maj = (a & b) ^ (a & c) ^ (b & c);
        let t2 = s0.wrapping_add(maj);
        hh = g;
        g = f;
        f = e;
        e = d.wrapping_add(t1);
        d = c;
        c = b;
        b = a;
        a = t1.wrapping_add(t2);
    }
    for (x, y) in h.iter_mut().zip([a, b, c, d, e, f, g, hh]) {
        *x = x.wrapping_add(y);
    }
}

fn sha512_core(data: &[u8], iv: [u64; 8]) -> [u8; 64] {
    let mut h = iv;
    let chunks = data.chunks_exact(128);
    let rem = chunks.remainder();
    for chunk in chunks {
        compress512(&mut h, chunk);
    }
    let mut tail = [0u8; 256];
    tail[..rem.len()].copy_from_slice(rem);
    tail[rem.len()] = 0x80;
    let total = if rem.len() < 112 { 128 } else { 256 };
    let bits = (data.len() as u128).wrapping_mul(8).to_be_bytes();
    tail[total - 16..total].copy_from_slice(&bits);
    for chunk in tail[..total].chunks_exact(128) {
        compress512(&mut h, chunk);
    }
    let mut out = [0u8; 64];
    for (o, w) in out.chunks_exact_mut(8).zip(h) {
        o.copy_from_slice(&w.to_be_bytes());
    }
    out
}

pub(crate) fn sha512(data: &[u8]) -> [u8; 64] {
    sha512_core(data, H512)
}

pub(crate) fn sha384(data: &[u8]) -> [u8; 48] {
    let full = sha512_core(data, H384);
    let mut out = [0u8; 48];
    out.copy_from_slice(&full[..48]);
    out
}
