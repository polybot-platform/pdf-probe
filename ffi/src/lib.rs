//! A C ABI for pdf-probe. One function does the work; the host passes the PDF bytes and a buffer, and gets
//! the answer back in the buffer. There is no allocation across the boundary, no global state, and no way
//! for a panic to unwind into the host: a panic becomes the `PANIC` status.
//!
//! ## Result layout (little-endian)
//!
//! | Offset | Type | Meaning |
//! |---|---|---|
//! | 0 | u32 | status: 0 ok, 1 unparseable, 2 empty, 3 too many pages, 4 internal error (a panic) |
//! | 4 | u32 | page count (status 0 and 3), else 0 |
//! | 8 | u32 | number of page sizes that follow (status 0), else 0 |
//! | 12 | u32 | length of the reason text, in bytes |
//! | 16 | f64 x 2 each | width and height of each page in points |
//! | after | bytes | the reason, UTF-8 (status 1 and 4), else empty |
//!
//! The function returns the number of bytes the result needs. If that is larger than `out_cap`, nothing was
//! written and the caller retries with a bigger buffer. A negative return is a bad argument.
#![deny(unsafe_code)]

use pdf_probe::{Failure, Limits, Probe, probe};
use std::panic::{AssertUnwindSafe, catch_unwind};

/// Bumped when the result layout or the function signature changes.
pub const ABI_VERSION: u32 = 1;

pub const STATUS_OK: u32 = 0;
pub const STATUS_UNPARSEABLE: u32 = 1;
pub const STATUS_EMPTY: u32 = 2;
pub const STATUS_TOO_MANY_PAGES: u32 = 3;
pub const STATUS_PANIC: u32 = 4;

/// The size of the fixed header.
pub const HEADER: usize = 16;

/// More pages than this is never requested: it bounds the output buffer a host has to allocate.
const MAX_PAGES_CEILING: u32 = 1_000_000;

fn header(status: u32, pages: usize, sizes: usize, reason: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(HEADER);
    out.extend_from_slice(&status.to_le_bytes());
    for v in [pages, sizes, reason] {
        out.extend_from_slice(&u32::try_from(v).unwrap_or(u32::MAX).to_le_bytes());
    }
    out
}

fn with_reason(status: u32, pages: usize, reason: &str) -> Vec<u8> {
    let mut out = header(status, pages, 0, reason.len());
    out.extend_from_slice(reason.as_bytes());
    out
}

/// Encodes an outcome in the layout above. `Err` is a caught panic.
pub fn encode(outcome: std::thread::Result<Result<Probe, Failure>>) -> Vec<u8> {
    match outcome {
        Ok(Ok(p)) => {
            let mut out = header(STATUS_OK, p.pages, p.sizes.len(), 0);
            for s in &p.sizes {
                out.extend_from_slice(&s.width.to_le_bytes());
                out.extend_from_slice(&s.height.to_le_bytes());
            }
            out
        }
        Ok(Err(Failure::Unparseable(why))) => with_reason(STATUS_UNPARSEABLE, 0, why),
        Ok(Err(Failure::Empty)) => with_reason(STATUS_EMPTY, 0, ""),
        Ok(Err(Failure::TooManyPages { pages })) => with_reason(STATUS_TOO_MANY_PAGES, pages, ""),
        Err(payload) => {
            let msg = payload
                .downcast_ref::<&str>()
                .map(|s| (*s).to_string())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "panic".to_string());
            with_reason(STATUS_PANIC, 0, &msg)
        }
    }
}

/// Runs the parser on `data` and returns the encoded answer. Never panics.
pub fn check(data: &[u8], max_pages: u32) -> Vec<u8> {
    let limits = Limits {
        max_pages: max_pages.min(MAX_PAGES_CEILING) as usize,
        ..Limits::default()
    };
    encode(catch_unwind(AssertUnwindSafe(|| probe(data, &limits))))
}

/// The only unsafe code in the project: turning the host's pointers into slices.
#[allow(unsafe_code)]
mod abi {
    use super::{ABI_VERSION, check};

    #[unsafe(no_mangle)]
    pub extern "C" fn pdf_probe_abi_version() -> u32 {
        ABI_VERSION
    }

    /// Checks one PDF. Returns the number of bytes the result needs, or a negative number for a bad argument
    /// (-1: a null pointer with a non-zero length; -2: the result is too large for an `i32`).
    ///
    /// # Safety
    ///
    /// `data` must point to `len` readable bytes (it may be null only if `len` is 0). `out` must point to
    /// `out_cap` writable bytes (it may be null only if `out_cap` is 0). The two regions must not overlap.
    /// Neither is used after the call returns.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn pdf_probe_check(
        data: *const u8,
        len: usize,
        max_pages: u32,
        out: *mut u8,
        out_cap: usize,
    ) -> i32 {
        if (data.is_null() && len != 0) || (out.is_null() && out_cap != 0) {
            return -1;
        }
        // SAFETY: the caller guarantees `data` points to `len` readable bytes; a zero length needs no read.
        let input: &[u8] = if len == 0 {
            &[]
        } else {
            unsafe { std::slice::from_raw_parts(data, len) }
        };
        let encoded = check(input, max_pages);
        let Ok(needed) = i32::try_from(encoded.len()) else {
            return -2;
        };
        if encoded.len() <= out_cap {
            // SAFETY: `encoded.len() <= out_cap`, and the caller guarantees `out` points to `out_cap`
            // writable bytes that do not overlap `data`. `out` is non-null here because `out_cap > 0`.
            let dst = unsafe { std::slice::from_raw_parts_mut(out, encoded.len()) };
            dst.copy_from_slice(&encoded);
        }
        needed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pdf_probe::PageSize;

    fn u32_at(b: &[u8], at: usize) -> u32 {
        u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
    }

    fn f64_at(b: &[u8], at: usize) -> f64 {
        f64::from_le_bytes(b[at..at + 8].try_into().unwrap())
    }

    #[test]
    fn a_successful_answer_has_a_header_and_the_sizes() {
        let probe = Probe {
            pages: 2,
            sizes: vec![
                PageSize {
                    width: 612.0,
                    height: 792.0,
                },
                PageSize {
                    width: 100.5,
                    height: 200.25,
                },
            ],
        };
        let b = encode(Ok(Ok(probe)));
        assert_eq!(b.len(), HEADER + 2 * 16);
        assert_eq!(
            [u32_at(&b, 0), u32_at(&b, 4), u32_at(&b, 8), u32_at(&b, 12)],
            [STATUS_OK, 2, 2, 0]
        );
        assert_eq!((f64_at(&b, 16), f64_at(&b, 24)), (612.0, 792.0));
        assert_eq!((f64_at(&b, 32), f64_at(&b, 40)), (100.5, 200.25));
    }

    #[test]
    fn failures_carry_their_status_and_reason() {
        let b = encode(Ok(Err(Failure::Unparseable("a password is required"))));
        assert_eq!(u32_at(&b, 0), STATUS_UNPARSEABLE);
        assert_eq!(&b[HEADER..], b"a password is required");
        assert_eq!(u32_at(&b, 12) as usize, b.len() - HEADER);
        assert_eq!(u32_at(&encode(Ok(Err(Failure::Empty))), 0), STATUS_EMPTY);
        let many = encode(Ok(Err(Failure::TooManyPages { pages: 501 })));
        assert_eq!(
            (u32_at(&many, 0), u32_at(&many, 4)),
            (STATUS_TOO_MANY_PAGES, 501)
        );
    }

    #[test]
    fn a_caught_panic_becomes_a_status_not_an_unwind() {
        let caught = catch_unwind(|| -> Result<Probe, Failure> { panic!("boom") });
        let b = encode(caught);
        assert_eq!(u32_at(&b, 0), STATUS_PANIC);
        assert_eq!(&b[HEADER..], b"boom");
    }

    #[test]
    fn check_reads_a_real_file_end_to_end() {
        let pdf = b"%PDF-1.4\n1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 400] >>\nendobj\ntrailer\n<< /Root 1 0 R >>\n";
        let b = check(pdf, 10);
        assert_eq!((u32_at(&b, 0), u32_at(&b, 4)), (STATUS_OK, 1));
        assert_eq!((f64_at(&b, 16), f64_at(&b, 24)), (300.0, 400.0));
        assert_eq!(u32_at(&check(b"not a pdf", 10), 0), STATUS_UNPARSEABLE);
    }

    #[test]
    #[allow(unsafe_code)]
    fn the_c_function_handles_sizes_and_bad_arguments() {
        let mut out = [0u8; 256];
        let data = b"not a pdf";
        // SAFETY: the pointers and lengths describe live arrays.
        let needed =
            unsafe { abi_check(data.as_ptr(), data.len(), 10, out.as_mut_ptr(), out.len()) };
        assert!(needed > HEADER as i32);
        assert_eq!(u32_at(&out, 0), STATUS_UNPARSEABLE);
        // A buffer that is too small is left untouched and the needed size is returned.
        let mut tiny = [0xAAu8; 4];
        let again =
            unsafe { abi_check(data.as_ptr(), data.len(), 10, tiny.as_mut_ptr(), tiny.len()) };
        assert_eq!(again, needed);
        assert_eq!(tiny, [0xAA; 4]);
        // Null pointers with a length are refused; an empty input is an ordinary failure.
        assert_eq!(
            unsafe { abi_check(std::ptr::null(), 5, 10, out.as_mut_ptr(), out.len()) },
            -1
        );
        assert_eq!(
            unsafe { abi_check(data.as_ptr(), 1, 10, std::ptr::null_mut(), 8) },
            -1
        );
        assert!(unsafe { abi_check(std::ptr::null(), 0, 10, out.as_mut_ptr(), out.len()) } > 0);
        assert_eq!(abi_version(), ABI_VERSION);
    }

    #[allow(unsafe_code)]
    unsafe fn abi_check(data: *const u8, len: usize, max: u32, out: *mut u8, cap: usize) -> i32 {
        unsafe extern "C" {
            fn pdf_probe_check(
                data: *const u8,
                len: usize,
                max: u32,
                out: *mut u8,
                cap: usize,
            ) -> i32;
        }
        unsafe { pdf_probe_check(data, len, max, out, cap) }
    }

    #[allow(unsafe_code)]
    fn abi_version() -> u32 {
        unsafe extern "C" {
            fn pdf_probe_abi_version() -> u32;
        }
        unsafe { pdf_probe_abi_version() }
    }
}
