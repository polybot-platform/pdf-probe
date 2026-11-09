//! The standard security handler, with an empty user password: key derivation, the password check, and
//! decryption of the streams this crate reads (object streams). RC4 (40 to 128 bit), AES-128 (V4) and
//! AES-256 (V5, revisions 5 and 6). A file that needs a password fails closed.
//!
//! Behaviour follows pdf.js where the specification is silent; each such place says so.
use crate::crypto::{aes, md5::md5, rc4::rc4, sha2};
use crate::lexer::{Dict, Obj};
use crate::xref::Entry;
use crate::{Doc, Failure};
use std::borrow::Cow;

const PAD: [u8; 32] = [
    0x28, 0xbf, 0x4e, 0x5e, 0x4e, 0x75, 0x8a, 0x41, 0x64, 0x00, 0x4e, 0x56, 0xff, 0xfa, 0x01, 0x08,
    0x2e, 0x2e, 0x00, 0xb6, 0xd0, 0x68, 0x3e, 0x80, 0x2f, 0x0c, 0xa9, 0xfe, 0x64, 0x53, 0x69, 0x7a,
];

fn fail<T>(why: &'static str) -> Result<T, Failure> {
    Err(Failure::Unparseable(why))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Method {
    Identity,
    Rc4,
    AesV2,
    AesV3,
}

pub(crate) struct Crypto {
    /// The file encryption key. `None` only when password prompting is deferred to embedded files.
    key: Option<Vec<u8>>,
    /// The default for streams (`/StmF`, or RC4 before version 4).
    stmf: Method,
    /// Named crypt filters (`/CF`), for a stream that picks one with `/Filter /Crypt`.
    filters: Vec<(Vec<u8>, Method)>,
}

fn name_of(o: Option<&Obj>) -> Option<&[u8]> {
    match o {
        Some(Obj::Name(n)) => Some(n.as_slice()),
        _ => None,
    }
}

fn method_of(cf: &Dict, name: &[u8]) -> Method {
    if name == b"Identity" {
        return Method::Identity;
    }
    match cf.get(name) {
        Some(Obj::Dict(d)) => match name_of(d.get(b"CFM")) {
            Some(b"V2") => Method::Rc4,
            Some(b"AESV2") => Method::AesV2,
            Some(b"AESV3") => Method::AesV3,
            // `None`, a missing /CFM or anything unknown: no encryption, as in pdf.js.
            _ => Method::Identity,
        },
        _ => Method::Identity,
    }
}

/// Reads `/Encrypt` and derives the key for an empty password. `Ok(None)` for an unencrypted file.
pub(crate) fn setup(doc: &mut Doc) -> Result<Option<Crypto>, Failure> {
    let Some(enc) = doc.xref.trailer.get(b"Encrypt").cloned() else {
        return Ok(None);
    };
    if let Obj::Ref(n, _) = enc
        && matches!(doc.xref.entries.get(&n), Some(Entry::InStream { .. }))
    {
        return fail("the encryption dictionary is in an object stream");
    }
    let Obj::Dict(d) = doc.resolve(&enc)? else {
        return fail("the encryption dictionary is not a dictionary");
    };
    if name_of(d.get(b"Filter")) != Some(b"Standard") {
        return fail("unsupported security handler");
    }
    let int = |k: &[u8]| d.get(k).and_then(Obj::as_int);
    let (Some(v), Some(r)) = (int(b"V"), int(b"R")) else {
        return fail("bad encryption dictionary");
    };
    if !matches!(v, 1 | 2 | 4 | 5)
        || (v == 5 && !matches!(r, 5 | 6))
        || (v != 5 && !(2..=4).contains(&r))
    {
        return fail("unsupported encryption version");
    }
    let bytes = |k: &[u8]| match d.get(k) {
        Some(Obj::Str(s)) => s.clone(),
        _ => Vec::new(),
    };
    let (o, u) = (bytes(b"O"), bytes(b"U"));

    // Crypt filters (version 4 and 5).
    let mut cf = Dict::default();
    let (mut stmf_name, mut strf_name, mut eff_name) =
        (b"Identity".to_vec(), b"Identity".to_vec(), None);
    if v >= 4 {
        if let Some(c) = d.get(b"CF")
            && let Obj::Dict(c) = doc.resolve(c)?
        {
            // The values may be indirect; resolve them so the dictionary is self-contained.
            for (k, val) in c.0 {
                cf.0.push((k, doc.resolve(&val)?));
            }
        }
        stmf_name = name_of(d.get(b"StmF")).unwrap_or(b"Identity").to_vec();
        strf_name = name_of(d.get(b"StrF")).unwrap_or(b"Identity").to_vec();
        eff_name = name_of(d.get(b"EFF")).map(<[u8]>::to_vec);
    }
    let stmf = if v >= 4 {
        method_of(&cf, &stmf_name)
    } else {
        Method::Rc4
    };
    let filters: Vec<(Vec<u8>, Method)> =
        cf.0.iter()
            .map(|(k, _)| (k.clone(), method_of(&cf, k)))
            .collect();

    let id0 = match doc.xref.trailer.get(b"ID") {
        Some(Obj::Array(a)) => match a.first() {
            Some(Obj::Str(s)) => s.clone(),
            _ => Vec::new(),
        },
        _ => Vec::new(),
    };

    let key = if v == 5 {
        v5_key(&d, r, &u)?
    } else {
        // Key length in bits (pdf.js: /Length, else 40 before version 4, else the stream filter's /Length).
        let mut bits = int(b"Length").unwrap_or(0);
        if bits == 0 {
            if v <= 3 {
                bits = 40;
            } else {
                let handler = match cf.get(&stmf_name) {
                    Some(Obj::Dict(h)) => h.get(b"Length").and_then(Obj::as_int),
                    _ => return fail("invalid key length"),
                };
                bits = handler.filter(|n| *n != 0).unwrap_or(128);
                if bits < 40 {
                    bits <<= 3; // some generators give bytes, not bits
                }
            }
        }
        if bits < 40 || bits % 8 != 0 || bits > 128 {
            return fail("invalid key length");
        }
        let encrypt_metadata = !matches!(d.get(b"EncryptMetadata"), Some(Obj::Bool(false)));
        let p = int(b"P").unwrap_or(0);
        let key = legacy_key(
            &o,
            &u,
            p,
            &id0,
            r,
            (bits / 8) as usize,
            v >= 4 && !encrypt_metadata,
        );
        match key {
            // Not in the specification, and pdf.js does it for exactly these files: under V4 a key shorter
            // than 16 bytes is zero-extended to 16 before the per-object keys are derived.
            Some(mut k) if v == 4 && k.len() < 16 => {
                k.resize(16, 0);
                Some(k)
            }
            other => other,
        }
    };
    if let Some(key) = key {
        return Ok(Some(Crypto {
            key: Some(key),
            stmf,
            filters,
        }));
    }
    // The password check failed. pdf.js lets a file through when the default filters are Identity and only
    // embedded files (/EFF, opened on demand) are encrypted; nothing this crate reads is encrypted then.
    let eff_opens_on_demand = v >= 4
        && stmf == Method::Identity
        && method_of(&cf, &strf_name) == Method::Identity
        && match cf.get(eff_name.as_deref().unwrap_or(&stmf_name)) {
            Some(Obj::Dict(h)) => name_of(h.get(b"AuthEvent")) == Some(b"EFOpen"),
            _ => false,
        };
    if eff_opens_on_demand {
        return Ok(Some(Crypto {
            key: None,
            stmf,
            filters,
        }));
    }
    fail("a password is required")
}

/// Algorithm 2 and the user-password check (algorithms 4 and 5), revisions 2 to 4, empty password.
fn legacy_key(
    o: &[u8],
    u: &[u8],
    p: i64,
    id0: &[u8],
    r: i64,
    n: usize,
    no_metadata: bool,
) -> Option<Vec<u8>> {
    if n > 16 {
        return None;
    }
    let mut input = PAD.to_vec();
    input.extend_from_slice(&o[..o.len().min(32)]);
    input.extend_from_slice(&((p & 0xffff_ffff) as u32).to_le_bytes());
    input.extend_from_slice(id0);
    if r >= 4 && no_metadata {
        input.extend_from_slice(&[0xff; 4]);
    }
    let mut hash = md5(&input).to_vec();
    if r >= 3 {
        for _ in 0..50 {
            hash = md5(&hash[..n]).to_vec();
        }
    }
    let key = hash[..n].to_vec();
    let ok = if r >= 3 {
        let mut data = PAD.to_vec();
        data.extend_from_slice(id0);
        let mut check = rc4(&key, &md5(&data));
        for j in 1..=19u8 {
            let k: Vec<u8> = key.iter().map(|b| b ^ j).collect();
            check = rc4(&k, &check);
        }
        u.len() >= 16 && check[..] == u[..16]
    } else {
        let check = rc4(&key, &PAD);
        u.len() >= 32 && check[..] == u[..32]
    };
    ok.then_some(key)
}

/// Algorithm 2.A for the user password (empty): revision 5 hashes with SHA-256, revision 6 with 2.B.
fn v5_key(d: &Dict, r: i64, u: &[u8]) -> Result<Option<Vec<u8>>, Failure> {
    let ue = match d.get(b"UE") {
        Some(Obj::Str(s)) => s.as_slice(),
        _ => &[],
    };
    if u.len() < 48 || ue.len() < 32 {
        return fail("bad encryption dictionary");
    }
    let hash = |salt: &[u8]| -> Vec<u8> {
        if r == 6 {
            hash_2b(&[], salt, &[])
        } else {
            sha2::sha256(salt).to_vec()
        }
    };
    if hash(&u[32..40])[..32] != u[..32] {
        return Ok(None);
    }
    let ikey = hash(&u[40..48]);
    Ok(aes::cbc_decrypt(&ikey[..32], &[0; 16], &ue[..32]))
}

/// Algorithm 2.B (ISO 32000-2): the revision 6 hash.
fn hash_2b(password: &[u8], salt: &[u8], udata: &[u8]) -> Vec<u8> {
    let mut seed = password.to_vec();
    seed.extend_from_slice(salt);
    seed.extend_from_slice(udata);
    let mut k = sha2::sha256(&seed).to_vec();
    let mut round = 0usize;
    loop {
        let mut unit = password.to_vec();
        unit.extend_from_slice(&k);
        unit.extend_from_slice(udata);
        let repeated = unit.repeat(64);
        let iv: [u8; 16] = core::array::from_fn(|i| k[16 + i]);
        let Some(e) = aes::cbc_encrypt(&k[..16], &iv, &repeated) else {
            return k;
        };
        // The first 16 bytes as a big-endian number, mod 3, equals their sum mod 3 (256 is 1 mod 3).
        let pick: u32 = e[..16].iter().map(|b| u32::from(*b)).sum::<u32>() % 3;
        k = match pick {
            0 => sha2::sha256(&e).to_vec(),
            1 => sha2::sha384(&e).to_vec(),
            _ => sha2::sha512(&e).to_vec(),
        };
        round += 1;
        let last = i64::from(*e.last().unwrap_or(&0));
        if round >= 64 && last <= round as i64 - 32 {
            return k;
        }
    }
}

impl Crypto {
    /// The crypt filter for this stream: a leading `/Crypt` filter names one, otherwise `/StmF` applies.
    fn method_for(&self, dict: &Dict) -> Method {
        let first = match dict.get(b"Filter") {
            Some(Obj::Name(n)) => Some((n.as_slice(), 0usize)),
            Some(Obj::Array(a)) => name_of(a.first()).map(|n| (n, 0)),
            _ => None,
        };
        if !matches!(first, Some((b"Crypt", _))) {
            return self.stmf;
        }
        let parms = match dict.get(b"DecodeParms") {
            Some(Obj::Dict(p)) => Some(p),
            Some(Obj::Array(a)) => match a.first() {
                Some(Obj::Dict(p)) => Some(p),
                _ => None,
            },
            _ => None,
        };
        let name = parms
            .and_then(|p| name_of(p.get(b"Name")))
            .unwrap_or(b"Identity");
        self.filters
            .iter()
            .find(|(k, _)| k == name)
            .map_or(Method::Identity, |(_, m)| *m)
    }

    /// The per-object key (algorithm 1) for RC4 and AES-128.
    fn object_key(&self, key: &[u8], num: u32, gen_: u16, aes128: bool) -> Vec<u8> {
        let mut input = key.to_vec();
        input.extend_from_slice(&num.to_le_bytes()[..3]);
        input.extend_from_slice(&gen_.to_le_bytes());
        if aes128 {
            input.extend_from_slice(b"sAlT");
        }
        md5(&input)[..(key.len() + 5).min(16)].to_vec()
    }

    /// Decrypts the raw data of the stream object `num` `gen_`.
    pub fn decrypt_stream<'a>(
        &self,
        num: u32,
        gen_: u16,
        dict: &Dict,
        raw: &'a [u8],
    ) -> Result<Cow<'a, [u8]>, Failure> {
        let method = self.method_for(dict);
        if method == Method::Identity {
            return Ok(Cow::Borrowed(raw));
        }
        let Some(key) = &self.key else {
            return fail("a password is required");
        };
        match method {
            Method::Identity => Ok(Cow::Borrowed(raw)),
            Method::Rc4 => Ok(Cow::Owned(rc4(
                &self.object_key(key, num, gen_, false),
                raw,
            ))),
            Method::AesV2 => {
                aes_cbc_unpad(&self.object_key(key, num, gen_, true), raw).map(Cow::Owned)
            }
            Method::AesV3 => aes_cbc_unpad(key, raw).map(Cow::Owned),
        }
    }
}

/// `iv (16 bytes) || AES-CBC ciphertext`, then PKCS#5 padding removed (kept as is when it is not valid, as
/// in pdf.js).
fn aes_cbc_unpad(key: &[u8], data: &[u8]) -> Result<Vec<u8>, Failure> {
    if data.len() < 16 || !(data.len() - 16).is_multiple_of(16) {
        return fail("bad AES data length");
    }
    let iv: [u8; 16] = core::array::from_fn(|i| data[i]);
    let Some(mut plain) = aes::cbc_decrypt(key, &iv, &data[16..]) else {
        return fail("bad AES key");
    };
    if let Some(&n) = plain.last()
        && (1..=16).contains(&n)
        && usize::from(n) <= plain.len()
    {
        plain.truncate(plain.len() - usize::from(n));
    }
    Ok(plain)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A revision 5 (SHA-256) user entry for the empty password and the given file key, as a generator
    /// would write it: `U` is hash || validation salt || key salt, `UE` is the file key encrypted with the
    /// hash of the key salt.
    fn r5_entries(file_key: &[u8; 32]) -> (Vec<u8>, Vec<u8>) {
        let (vsalt, ksalt) = (
            [1u8, 2, 3, 4, 5, 6, 7, 8],
            [9u8, 10, 11, 12, 13, 14, 15, 16],
        );
        let mut u = sha2::sha256(&vsalt).to_vec();
        u.extend_from_slice(&vsalt);
        u.extend_from_slice(&ksalt);
        let ue = aes::cbc_encrypt(&sha2::sha256(&ksalt), &[0; 16], file_key).unwrap();
        (u, ue)
    }

    fn dict_with_ue(ue: Vec<u8>) -> Dict {
        Dict(vec![(b"UE".to_vec(), Obj::Str(ue))])
    }

    #[test]
    fn revision_5_recovers_the_file_key_for_an_empty_password() {
        let file_key: [u8; 32] = core::array::from_fn(|i| (i * 3 + 1) as u8);
        let (u, ue) = r5_entries(&file_key);
        assert_eq!(
            v5_key(&dict_with_ue(ue), 5, &u).unwrap(),
            Some(file_key.to_vec())
        );
    }

    #[test]
    fn revision_5_with_a_password_set_gives_no_key() {
        let file_key = [7u8; 32];
        let (mut u, ue) = r5_entries(&file_key);
        u[0] ^= 1; // the validation hash was made from some other password
        assert_eq!(v5_key(&dict_with_ue(ue), 5, &u).unwrap(), None);
    }

    #[test]
    fn a_short_encryption_entry_is_refused() {
        assert!(v5_key(&dict_with_ue(vec![0; 32]), 6, &[0; 40]).is_err());
    }

    #[test]
    fn aes_padding_is_removed_and_bad_lengths_are_refused() {
        let key = [3u8; 16];
        let iv = [5u8; 16];
        let plain = b"hello pdf-probe\x01"; // 15 bytes of text and one byte of padding
        let ct = aes::cbc_encrypt(&key, &iv, plain).unwrap();
        let mut data = iv.to_vec();
        data.extend(ct);
        assert_eq!(aes_cbc_unpad(&key, &data).unwrap(), b"hello pdf-probe");
        assert!(aes_cbc_unpad(&key, &data[..20]).is_err());
        assert!(aes_cbc_unpad(&key, &[0; 8]).is_err());
    }

    #[test]
    fn the_object_key_is_extended_by_the_object_number() {
        let c = Crypto {
            key: Some(vec![1; 5]),
            stmf: Method::Rc4,
            filters: Vec::new(),
        };
        assert_eq!(c.object_key(&[1; 5], 7, 0, false).len(), 10);
        assert_eq!(c.object_key(&[1; 16], 7, 0, true).len(), 16);
        assert_ne!(
            c.object_key(&[1; 5], 7, 0, false),
            c.object_key(&[1; 5], 8, 0, false)
        );
    }
}
