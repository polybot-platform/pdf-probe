//! The ciphers and hashes the standard security handler needs, written here so the crate has no dependencies
//! and no `unsafe`. Only decryption and key derivation are needed; nothing here is constant-time, which does
//! not matter for an empty-password check on a file the caller already holds.
#![allow(clippy::chunks_exact_to_as_chunks)] // these are runtime slices, not arrays; as_chunks would not read better

pub(crate) mod aes;
pub(crate) mod md5;
mod md5_tables;
pub(crate) mod rc4;
pub(crate) mod sha2;
mod sha2_tables;

#[cfg(test)]
mod tests {
    use super::aes::{Aes, cbc_decrypt, cbc_encrypt};
    use super::md5::md5;
    use super::rc4::rc4;
    use super::sha2::{sha256, sha384, sha512};

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    fn unhex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    fn msg300() -> Vec<u8> {
        (0..300).map(|i: usize| ((i * 7 + 3) % 256) as u8).collect()
    }

    // Digests from Python's hashlib.
    #[test]
    fn md5_matches_known_digests() {
        assert_eq!(hex(&md5(b"")), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(hex(&md5(b"abc")), "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(hex(&md5(&msg300())), "3ea74b65673e1390b90da3e462c02f9f");
    }

    #[test]
    fn sha256_matches_known_digests() {
        assert_eq!(
            hex(&sha256(b"")),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            hex(&sha256(b"abc")),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            hex(&sha256(&msg300())),
            "04773f8726c81cafcfa1a09a82664b98b00d2021031a1715bca1154f2dad3472"
        );
    }

    #[test]
    fn sha384_matches_known_digests() {
        assert_eq!(
            hex(&sha384(b"")),
            "38b060a751ac96384cd9327eb1b1e36a21fdb71114be07434c0cc7bf63f6e1da274edebfe76f65fbd51ad2f14898b95b"
        );
        assert_eq!(
            hex(&sha384(b"abc")),
            "cb00753f45a35e8bb5a03d699ac65007272c32ab0eded1631a8b605a43ff5bed8086072ba1e7cc2358baeca134c825a7"
        );
        assert_eq!(
            hex(&sha384(&msg300())),
            "060eb8e1c01cc6c6c8c03fcb7898155f2b836fb97c9d331a7a296a1a209aac9d261b2b268c4ba3c7299a3dd12b5bf81f"
        );
    }

    #[test]
    fn sha512_matches_known_digests() {
        assert_eq!(
            hex(&sha512(b"")),
            "cf83e1357eefb8bdf1542850d66d8007d620e4050b5715dc83f4a921d36ce9ce47d0d13c5d85f2b0ff8318d2877eec2f63b931bd47417a81a538327af927da3e"
        );
        assert_eq!(
            hex(&sha512(b"abc")),
            "ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f"
        );
        assert_eq!(
            hex(&sha512(&msg300())),
            "46e56ad30db9ef50f8b6762ba55839737f3fba34ab47863c9daff7b3f58f97fe3465a52dd364560db47f802909ced49093322621ea0aebf8e0696b85ca8f81f0"
        );
    }

    #[test]
    fn rc4_matches_the_classic_vector() {
        assert_eq!(hex(&rc4(b"Key", b"Plaintext")), "bbf316e8d940af0ad3");
    }

    // FIPS-197 appendix C.
    #[test]
    fn aes_matches_fips_197() {
        let plain: [u8; 16] = unhex("00112233445566778899aabbccddeeff")
            .try_into()
            .unwrap();
        for (key, cipher) in [
            (
                "000102030405060708090a0b0c0d0e0f",
                "69c4e0d86a7b0430d8cdb78070b4c55a",
            ),
            (
                "000102030405060708090a0b0c0d0e0f1011121314151617",
                "dda97ca4864cdfe06eaf70a0ec0d7191",
            ),
            (
                "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f",
                "8ea2b7ca516745bfeafc49904b496089",
            ),
        ] {
            let aes = Aes::new(&unhex(key)).unwrap();
            let mut block = plain;
            aes.encrypt_block(&mut block);
            assert_eq!(hex(&block), cipher);
            aes.decrypt_block(&mut block);
            assert_eq!(block, plain);
        }
        assert!(Aes::new(&[0u8; 15]).is_none());
    }

    // Ciphertext from Node's crypto (aes-*-cbc, no padding).
    #[test]
    fn aes_cbc_matches_node() {
        let key: Vec<u8> = (0..32).map(|i: usize| ((i * 5 + 1) % 256) as u8).collect();
        let iv: [u8; 16] = core::array::from_fn(|i| ((i * 11 + 2) % 256) as u8);
        let data: Vec<u8> = (0..64).map(|i: usize| ((i * 13 + 7) % 256) as u8).collect();
        for (k, expected) in [
            (
                &key[..16],
                "9a660ff33f0caa4572272b459019acafb01c01b1afba02874012d04f1453d678baddbde48def359202dcd4dd7674ea0859a9e3bc0a1f6db6fdd5ae9acb64cf4e",
            ),
            (
                &key[..],
                "d6948c114f02465a1c4e72a451716cbc1753591e1b9555d061452b2348f90297d18569232eab94b6ad6b632b505cfbde3d24229a47084e69080de2e26d1e48ea",
            ),
        ] {
            let ct = cbc_encrypt(k, &iv, &data).unwrap();
            assert_eq!(hex(&ct), expected);
            assert_eq!(cbc_decrypt(k, &iv, &ct).unwrap(), data);
        }
        assert!(cbc_decrypt(&key, &iv, &data[..15]).is_none());
    }
}
