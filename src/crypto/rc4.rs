//! RC4. An empty key returns the data unchanged.
pub(crate) fn rc4(key: &[u8], data: &[u8]) -> Vec<u8> {
    if key.is_empty() {
        return data.to_vec();
    }
    let mut s: [u8; 256] = core::array::from_fn(|i| i as u8);
    let mut j = 0usize;
    for i in 0..256 {
        j = (j + usize::from(s[i]) + usize::from(key[i % key.len()])) & 255;
        s.swap(i, j);
    }
    let (mut a, mut b) = (0usize, 0usize);
    data.iter()
        .map(|&byte| {
            a = (a + 1) & 255;
            b = (b + usize::from(s[a])) & 255;
            s.swap(a, b);
            byte ^ s[(usize::from(s[a]) + usize::from(s[b])) & 255]
        })
        .collect()
}
