//! MD5 (RFC 1321).
use super::md5_tables::K;

const S: [u32; 64] = [
    7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9,
    14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10, 15,
    21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
];

fn compress(state: &mut [u32; 4], chunk: &[u8]) {
    let mut m = [0u32; 16];
    for (w, b) in m.iter_mut().zip(chunk.chunks_exact(4)) {
        *w = u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
    }
    let [mut a, mut b, mut c, mut d] = *state;
    for i in 0..64 {
        let (f, g) = match i / 16 {
            0 => ((b & c) | (!b & d), i),
            1 => ((d & b) | (!d & c), (5 * i + 1) % 16),
            2 => (b ^ c ^ d, (3 * i + 5) % 16),
            _ => (c ^ (b | !d), (7 * i) % 16),
        };
        let f = f.wrapping_add(a).wrapping_add(K[i]).wrapping_add(m[g]);
        a = d;
        d = c;
        c = b;
        b = b.wrapping_add(f.rotate_left(S[i]));
    }
    state[0] = state[0].wrapping_add(a);
    state[1] = state[1].wrapping_add(b);
    state[2] = state[2].wrapping_add(c);
    state[3] = state[3].wrapping_add(d);
}

pub(crate) fn md5(data: &[u8]) -> [u8; 16] {
    let mut state: [u32; 4] = [0x6745_2301, 0xefcd_ab89, 0x98ba_dcfe, 0x1032_5476];
    let chunks = data.chunks_exact(64);
    let rem = chunks.remainder();
    for chunk in chunks {
        compress(&mut state, chunk);
    }
    let mut tail = [0u8; 128];
    tail[..rem.len()].copy_from_slice(rem);
    tail[rem.len()] = 0x80;
    let total = if rem.len() < 56 { 64 } else { 128 };
    let bits = (data.len() as u64).wrapping_mul(8).to_le_bytes();
    tail[total - 8..total].copy_from_slice(&bits);
    for chunk in tail[..total].chunks_exact(64) {
        compress(&mut state, chunk);
    }
    let mut out = [0u8; 16];
    for (o, w) in out.chunks_exact_mut(4).zip(state) {
        o.copy_from_slice(&w.to_le_bytes());
    }
    out
}
