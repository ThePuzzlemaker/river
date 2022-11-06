//! Utilities for pre-paging println! debugging

use core::{
    mem::{self, MaybeUninit},
    ptr, slice, str,
};

use crate::uart::Ns16650Inner;

// stolen from std //

static DEC_DIGITS_LUT: &[u8; 200] = b"0001020304050607080910111213141516171819\
      2021222324252627282930313233343536373839\
      4041424344454647484950515253545556575859\
      6061626364656667686970717273747576777879\
      8081828384858687888990919293949596979899";

impl Ns16650Inner {
    pub fn early_print_u64(&mut self, mut n: u64) {
        // 2^128 is about 3*10^38, so 39 gives an extra byte of space
        let mut buf = [MaybeUninit::<u8>::uninit(); 39];
        let mut curr = buf.len() as isize;
        let buf_ptr = MaybeUninit::slice_as_mut_ptr(&mut buf);
        let lut_ptr = DEC_DIGITS_LUT.as_ptr();

        // SAFETY: Since `d1` and `d2` are always less than or equal to `198`, we
        // can copy from `lut_ptr[d1..d1 + 1]` and `lut_ptr[d2..d2 + 1]`. To show
        // that it's OK to copy into `buf_ptr`, notice that at the beginning
        // `curr == buf.len() == 39 > log(n)` since `n < 2^128 < 10^39`, and at
        // each step this is kept the same as `n` is divided. Since `n` is always
        // non-negative, this means that `curr > 0` so `buf_ptr[curr..curr + 1]`
        // is safe to access.
        unsafe {
            // need at least 16 bits for the 4-characters-at-a-time to work.
            assert!(mem::size_of::<u64>() >= 2);

            // eagerly decode 4 characters at a time
            while n >= 10000 {
                let rem = (n % 10000) as isize;
                n /= 10000;

                let d1 = (rem / 100) << 1;
                let d2 = (rem % 100) << 1;
                curr -= 4;

                // We are allowed to copy to `buf_ptr[curr..curr + 3]` here since
                // otherwise `curr < 0`. But then `n` was originally at least `10000^10`
                // which is `10^40 > 2^128 > n`.
                ptr::copy_nonoverlapping(lut_ptr.offset(d1), buf_ptr.offset(curr), 2);
                ptr::copy_nonoverlapping(lut_ptr.offset(d2), buf_ptr.offset(curr + 2), 2);
            }

            // if we reach here numbers are <= 9999, so at most 4 chars long
            let mut n = n as isize; // possibly reduce 64bit math

            // decode 2 more chars, if > 2 chars
            if n >= 100 {
                let d1 = (n % 100) << 1;
                n /= 100;
                curr -= 2;
                ptr::copy_nonoverlapping(lut_ptr.offset(d1), buf_ptr.offset(curr), 2);
            }

            // decode last 1 or 2 chars
            if n < 10 {
                curr -= 1;
                *buf_ptr.offset(curr) = (n as u8) + b'0';
            } else {
                let d1 = n << 1;
                curr -= 2;
                ptr::copy_nonoverlapping(lut_ptr.offset(d1), buf_ptr.offset(curr), 2);
            }
        }

        // SAFETY: `curr` > 0 (since we made `buf` large enough), and all the chars are valid
        // UTF-8 since `DEC_DIGITS_LUT` is
        let buf_slice = unsafe {
            str::from_utf8_unchecked(slice::from_raw_parts(
                buf_ptr.offset(curr),
                buf.len() - curr as usize,
            ))
        };
        self.print_str_sync(buf_slice);
    }

    pub fn early_print_u64_hex(&mut self, mut x: u64) {
        // The radix can be as low as 2, so we need a buffer of at least 128
        // characters for a base 2 number.
        let zero = 0;
        let is_nonnegative = x >= zero;
        let mut buf = [MaybeUninit::<u8>::uninit(); 128];
        let mut curr = buf.len();
        let base = 16;
        if is_nonnegative {
            // Accumulate each digit of the number from the least significant
            // to the most significant figure.
            for byte in buf.iter_mut().rev() {
                let n = x % base; // Get the current place value.
                x /= base; // Deaccumulate the number.
                byte.write(digit_hex(n as u8)); // Store the digit in the buffer.
                curr -= 1;
                if x == zero {
                    // No more digits left to accumulate.
                    break;
                };
            }
        } else {
            // Do the same as above, but accounting for two's complement.
            for byte in buf.iter_mut().rev() {
                let n = zero - (x % base); // Get the current place value.
                x /= base; // Deaccumulate the number.
                byte.write(digit_hex(n as u8)); // Store the digit in the buffer.
                curr -= 1;
                if x == zero {
                    // No more digits left to accumulate.
                    break;
                };
            }
        }
        let buf = &buf[curr..];
        // SAFETY: The only chars in `buf` are created by `Self::digit` which are assumed to be
        // valid UTF-8
        let buf = unsafe {
            str::from_utf8_unchecked(slice::from_raw_parts(
                MaybeUninit::slice_as_ptr(buf),
                buf.len(),
            ))
        };
        self.print_str_sync(buf);
    }
}

fn digit_hex(x: u8) -> u8 {
    match x {
        x @ 0..=9 => b'0' + x,
        x @ 10..=15 => b'A' + (x - 10),
        x => panic!("number not in the range 0..=15: {}", x),
    }
}

// end stolen from std //

#[track_caller]
pub fn round_up_pow2(x: usize, n: usize) -> usize {
    debug_assert_eq!(
        n & (n - 1),
        0,
        "util::round_up_pow2: n was not a power of two"
    );
    (x + n - 1) & !(n - 1)
}
