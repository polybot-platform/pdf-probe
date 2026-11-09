//! AES-128, AES-192 and AES-256 (FIPS 197), with CBC mode. Block state is column-major, as in the standard.

const fn xtime(x: u8) -> u8 {
    (x << 1) ^ if x & 0x80 != 0 { 0x1b } else { 0 }
}

/// The S-box and its inverse, built at compile time (the standard generator-3 construction).
const fn tables() -> ([u8; 256], [u8; 256]) {
    let mut sbox = [0u8; 256];
    let mut inv = [0u8; 256];
    let (mut p, mut q) = (1u8, 1u8);
    loop {
        p ^= xtime(p); // p *= 3
        q ^= q << 1; // q /= 3
        q ^= q << 2;
        q ^= q << 4;
        if q & 0x80 != 0 {
            q ^= 0x09;
        }
        let x = q ^ q.rotate_left(1) ^ q.rotate_left(2) ^ q.rotate_left(3) ^ q.rotate_left(4);
        sbox[p as usize] = x ^ 0x63;
        if p == 1 {
            break;
        }
    }
    sbox[0] = 0x63;
    let mut i = 0;
    while i < 256 {
        inv[sbox[i] as usize] = i as u8;
        i += 1;
    }
    (sbox, inv)
}

const TABLES: ([u8; 256], [u8; 256]) = tables();
const SBOX: [u8; 256] = TABLES.0;
const INV_SBOX: [u8; 256] = TABLES.1;

fn gmul(mut a: u8, mut b: u8) -> u8 {
    let mut p = 0;
    while b != 0 {
        if b & 1 != 0 {
            p ^= a;
        }
        a = xtime(a);
        b >>= 1;
    }
    p
}

pub(crate) struct Aes {
    round_keys: Vec<[u8; 16]>,
}

impl Aes {
    /// `None` unless the key is 16, 24 or 32 bytes.
    pub fn new(key: &[u8]) -> Option<Aes> {
        let nk = match key.len() {
            16 | 24 | 32 => key.len() / 4,
            _ => return None,
        };
        let rounds = nk + 6;
        let mut w: Vec<[u8; 4]> = key
            .chunks_exact(4)
            .map(|c| [c[0], c[1], c[2], c[3]])
            .collect();
        let mut rcon = 1u8;
        for i in nk..4 * (rounds + 1) {
            let mut t = w[i - 1];
            if i % nk == 0 {
                t = [
                    SBOX[usize::from(t[1])] ^ rcon,
                    SBOX[usize::from(t[2])],
                    SBOX[usize::from(t[3])],
                    SBOX[usize::from(t[0])],
                ];
                rcon = xtime(rcon);
            } else if nk > 6 && i % nk == 4 {
                t = t.map(|b| SBOX[usize::from(b)]);
            }
            let prev = w[i - nk];
            w.push([
                prev[0] ^ t[0],
                prev[1] ^ t[1],
                prev[2] ^ t[2],
                prev[3] ^ t[3],
            ]);
        }
        let round_keys = w
            .chunks_exact(4)
            .map(|c| {
                let mut k = [0u8; 16];
                for (i, word) in c.iter().enumerate() {
                    k[4 * i..4 * i + 4].copy_from_slice(word);
                }
                k
            })
            .collect();
        Some(Aes { round_keys })
    }

    fn rounds(&self) -> usize {
        self.round_keys.len() - 1
    }

    fn add_round_key(s: &mut [u8; 16], k: &[u8; 16]) {
        for (a, b) in s.iter_mut().zip(k) {
            *a ^= b;
        }
    }

    pub fn encrypt_block(&self, s: &mut [u8; 16]) {
        Self::add_round_key(s, &self.round_keys[0]);
        for round in 1..=self.rounds() {
            for b in s.iter_mut() {
                *b = SBOX[usize::from(*b)];
            }
            let old = *s;
            for c in 0..4 {
                for r in 0..4 {
                    s[4 * c + r] = old[4 * ((c + r) % 4) + r];
                }
            }
            if round != self.rounds() {
                for c in 0..4 {
                    let a = [s[4 * c], s[4 * c + 1], s[4 * c + 2], s[4 * c + 3]];
                    s[4 * c] = xtime(a[0]) ^ (xtime(a[1]) ^ a[1]) ^ a[2] ^ a[3];
                    s[4 * c + 1] = a[0] ^ xtime(a[1]) ^ (xtime(a[2]) ^ a[2]) ^ a[3];
                    s[4 * c + 2] = a[0] ^ a[1] ^ xtime(a[2]) ^ (xtime(a[3]) ^ a[3]);
                    s[4 * c + 3] = (xtime(a[0]) ^ a[0]) ^ a[1] ^ a[2] ^ xtime(a[3]);
                }
            }
            Self::add_round_key(s, &self.round_keys[round]);
        }
    }

    pub fn decrypt_block(&self, s: &mut [u8; 16]) {
        Self::add_round_key(s, &self.round_keys[self.rounds()]);
        for round in (0..self.rounds()).rev() {
            let old = *s;
            for c in 0..4 {
                for r in 0..4 {
                    s[4 * ((c + r) % 4) + r] = old[4 * c + r];
                }
            }
            for b in s.iter_mut() {
                *b = INV_SBOX[usize::from(*b)];
            }
            Self::add_round_key(s, &self.round_keys[round]);
            if round != 0 {
                for c in 0..4 {
                    let a = [s[4 * c], s[4 * c + 1], s[4 * c + 2], s[4 * c + 3]];
                    s[4 * c] = gmul(a[0], 14) ^ gmul(a[1], 11) ^ gmul(a[2], 13) ^ gmul(a[3], 9);
                    s[4 * c + 1] = gmul(a[0], 9) ^ gmul(a[1], 14) ^ gmul(a[2], 11) ^ gmul(a[3], 13);
                    s[4 * c + 2] = gmul(a[0], 13) ^ gmul(a[1], 9) ^ gmul(a[2], 14) ^ gmul(a[3], 11);
                    s[4 * c + 3] = gmul(a[0], 11) ^ gmul(a[1], 13) ^ gmul(a[2], 9) ^ gmul(a[3], 14);
                }
            }
        }
    }
}

/// CBC decryption with no padding handling. `None` if the key or the data length is wrong.
pub(crate) fn cbc_decrypt(key: &[u8], iv: &[u8; 16], data: &[u8]) -> Option<Vec<u8>> {
    let aes = Aes::new(key)?;
    if !data.len().is_multiple_of(16) {
        return None;
    }
    let mut out = Vec::with_capacity(data.len());
    let mut prev = *iv;
    for block in data.chunks_exact(16) {
        let mut b = [0u8; 16];
        b.copy_from_slice(block);
        let cipher = b;
        aes.decrypt_block(&mut b);
        for (x, y) in b.iter_mut().zip(prev) {
            *x ^= y;
        }
        out.extend_from_slice(&b);
        prev = cipher;
    }
    Some(out)
}

/// CBC encryption with no padding. `None` if the key or the data length is wrong.
pub(crate) fn cbc_encrypt(key: &[u8], iv: &[u8; 16], data: &[u8]) -> Option<Vec<u8>> {
    let aes = Aes::new(key)?;
    if !data.len().is_multiple_of(16) {
        return None;
    }
    let mut out = Vec::with_capacity(data.len());
    let mut prev = *iv;
    for block in data.chunks_exact(16) {
        let mut b = [0u8; 16];
        for (i, x) in b.iter_mut().enumerate() {
            *x = block[i] ^ prev[i];
        }
        aes.encrypt_block(&mut b);
        out.extend_from_slice(&b);
        prev = b;
    }
    Some(out)
}
