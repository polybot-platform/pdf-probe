//! The coverage-guided fuzz target for the C ABI, `pdf_probe_check` in `ffi/`: the only `unsafe` code in the
//! project. Meant for AddressSanitizer: `cargo +nightly fuzz run ffi` (cargo-fuzz's default sanitizer).
//!
//! The first 4 bytes choose the output buffer's size and the page cap; the rest is the PDF. Input and output
//! are heap buffers of exactly their length, so ASan reports any read or write past either. A panic in the
//! parser is a crash here too: libfuzzer-sys aborts on a panic before the library's `catch_unwind` sees it.
#![no_main]

use libfuzzer_sys::fuzz_target;
// Links the library, which exports `pdf_probe_check`.
use pdf_probe_ffi as _;

unsafe extern "C" {
    fn pdf_probe_check(
        data: *const u8,
        len: usize,
        max_pages: u32,
        max_input_bytes: u32,
        max_work_millions: u32,
        out: *mut u8,
        out_cap: usize,
    ) -> i32;
}

const HEADER: usize = 16;
const SENTINEL: u8 = 0xA5;

fn call(data: &[u8], max_pages: u32, out: &mut [u8]) -> i32 {
    // An empty input goes in as a null pointer, which the ABI allows with a length of 0.
    let ptr = if data.is_empty() {
        std::ptr::null()
    } else {
        data.as_ptr()
    };
    let out_ptr = if out.is_empty() {
        std::ptr::null_mut()
    } else {
        out.as_mut_ptr()
    };
    // SAFETY: the pointers and lengths describe live, non-overlapping buffers (or are null with length 0).
    unsafe { pdf_probe_check(ptr, data.len(), max_pages, 0, 0, out_ptr, out.len()) }
}

fuzz_target!(|input: &[u8]| {
    let Some((head, pdf)) = input.split_first_chunk::<4>() else {
        return;
    };
    // Mostly small buffers, so the "too small, retry" path is common; sometimes up to 64 KiB.
    let cap = usize::from(u16::from_le_bytes([head[0], head[1]])) >> (head[2] & 7);
    let max_pages = u32::from(head[3]) * 4;
    let data = pdf.to_vec().into_boxed_slice();

    let mut out = vec![SENTINEL; cap].into_boxed_slice();
    let needed = call(&data, max_pages, &mut out);
    assert!(needed >= HEADER as i32, "needed {needed}");
    let needed = needed as usize;
    if needed > cap {
        // Too small: nothing written, and a buffer of the size asked for gets the answer.
        assert!(
            out.iter().all(|&b| b == SENTINEL),
            "a too-small buffer was written to"
        );
        out = vec![SENTINEL; needed].into_boxed_slice();
        assert_eq!(
            call(&data, max_pages, &mut out),
            needed as i32,
            "the answer changed size"
        );
    }
    let status = u32::from_le_bytes(out[..4].try_into().unwrap());
    assert!(status <= 5 && status != 4, "status {status}");
    // Bytes past the answer are left alone.
    assert!(
        out[needed..].iter().all(|&b| b == SENTINEL),
        "written past the answer"
    );
});
