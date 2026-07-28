//! The runtime the generated code stands on.
//!
//! This file is compiled twice: once as part of this crate, where its tests run,
//! and once as text — [`crate::codegen`] embeds it verbatim into every module it
//! emits, so generated code has no dependency at all. Anything wasm defines and
//! Rust spells differently lives here rather than in the emitter, which keeps
//! the semantics in one place and under test.
//!
//! The differences are not cosmetic. `i32.div_s` traps where Rust panics only in
//! debug, `f32.min` propagates NaN where Rust's `f32::min` swallows it, and
//! `i32.trunc_f32_s` traps on NaN where `as` saturates. Each one is a wrong
//! answer waiting to happen, and each one is a test below.

/// A wasm trap. The generated code panics, which is the observable behaviour a
/// trap has for a caller that is not catching it: execution stops, and nothing
/// downstream sees an invented value.
///
/// The message matches the text wasm engines use, so a differential run against
/// a real engine can compare failures and not just successes.
#[cold]
#[inline(never)]
pub fn trap(message: &str) -> ! {
    panic!("wasm trap: {message}");
}

/// Bytes in a wasm page.
pub const PAGE_SIZE: usize = 65536;

/// A wasm linear memory: a byte vector, bounds-checked on every access.
///
/// No `unsafe`, no raw pointers. A decompiler that needs them to model a
/// sandbox has lost the property that made the sandbox worth reading.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Memory {
    /// The raw bytes. Public because generated code reads and writes it, and
    /// because a differential harness compares it against an engine's.
    pub data: Vec<u8>,
    /// Page limit from the module's memory type, if it declared one.
    pub max_pages: Option<u32>,
}

impl Memory {
    /// A memory of `min_pages`, zero-filled, as instantiation leaves it.
    #[must_use]
    pub fn new(min_pages: u32, max_pages: Option<u32>) -> Self {
        Self {
            data: vec![0; min_pages as usize * PAGE_SIZE],
            max_pages,
        }
    }

    /// `memory.size`: the current size in pages.
    #[must_use]
    pub fn size(&self) -> i32 {
        (self.data.len() / PAGE_SIZE) as i32
    }

    /// `memory.grow`: returns the previous size in pages, or `-1` if refused.
    pub fn grow(&mut self, delta_pages: u32) -> i32 {
        let old_pages = self.data.len() / PAGE_SIZE;
        let new_pages = old_pages as u64 + u64::from(delta_pages);
        // 65536 pages is the 4 GiB ceiling of a 32-bit memory.
        if new_pages > u64::from(self.max_pages.unwrap_or(65536)) || new_pages > 65536 {
            return -1;
        }
        self.data.resize(new_pages as usize * PAGE_SIZE, 0);
        old_pages as i32
    }

    /// Resolves `addr + offset` and checks `size` bytes fit, in `u64` so the
    /// sum itself cannot wrap into a valid-looking address.
    fn range(&self, addr: i32, offset: u64, size: u64) -> usize {
        let end = u64::from(addr as u32) + offset + size;
        if end > self.data.len() as u64 {
            trap("out of bounds memory access");
        }
        (u64::from(addr as u32) + offset) as usize
    }

    /// Reads `N` bytes. The width is the type's, not the address's.
    fn read<const N: usize>(&self, addr: i32, offset: u64) -> [u8; N] {
        let at = self.range(addr, offset, N as u64);
        let mut bytes = [0u8; N];
        bytes.copy_from_slice(&self.data[at..at + N]);
        bytes
    }

    fn write<const N: usize>(&mut self, addr: i32, offset: u64, bytes: [u8; N]) {
        let at = self.range(addr, offset, N as u64);
        self.data[at..at + N].copy_from_slice(&bytes);
    }

    /// `i32.load8_u` / the unsigned byte read every wider load is built from.
    pub fn load8_u(&self, addr: i32, offset: u64) -> i32 {
        i32::from(self.read::<1>(addr, offset)[0])
    }
    /// `i32.load8_s`
    pub fn load8_s(&self, addr: i32, offset: u64) -> i32 {
        i32::from(self.read::<1>(addr, offset)[0] as i8)
    }
    /// `i32.load16_u`
    pub fn load16_u(&self, addr: i32, offset: u64) -> i32 {
        i32::from(u16::from_le_bytes(self.read(addr, offset)))
    }
    /// `i32.load16_s`
    pub fn load16_s(&self, addr: i32, offset: u64) -> i32 {
        i32::from(i16::from_le_bytes(self.read(addr, offset)))
    }
    /// `i32.load`
    pub fn load32(&self, addr: i32, offset: u64) -> i32 {
        i32::from_le_bytes(self.read(addr, offset))
    }
    /// `i64.load`
    pub fn load64(&self, addr: i32, offset: u64) -> i64 {
        i64::from_le_bytes(self.read(addr, offset))
    }
    /// `f32.load`
    pub fn load_f32(&self, addr: i32, offset: u64) -> f32 {
        f32::from_le_bytes(self.read(addr, offset))
    }
    /// `f64.load`
    pub fn load_f64(&self, addr: i32, offset: u64) -> f64 {
        f64::from_le_bytes(self.read(addr, offset))
    }

    /// `i64.load8_s`. The narrow i64 loads read the same bytes as their i32
    /// counterparts and sign- or zero-extend to 64 bits.
    pub fn load8_s_i64(&self, addr: i32, offset: u64) -> i64 {
        i64::from(self.read::<1>(addr, offset)[0] as i8)
    }
    /// `i64.load8_u`
    pub fn load8_u_i64(&self, addr: i32, offset: u64) -> i64 {
        i64::from(self.read::<1>(addr, offset)[0])
    }
    /// `i64.load16_s`
    pub fn load16_s_i64(&self, addr: i32, offset: u64) -> i64 {
        i64::from(i16::from_le_bytes(self.read(addr, offset)))
    }
    /// `i64.load16_u`
    pub fn load16_u_i64(&self, addr: i32, offset: u64) -> i64 {
        i64::from(u16::from_le_bytes(self.read(addr, offset)))
    }
    /// `i64.load32_s`
    pub fn load32_s_i64(&self, addr: i32, offset: u64) -> i64 {
        i64::from(i32::from_le_bytes(self.read(addr, offset)))
    }
    /// `i64.load32_u`
    pub fn load32_u_i64(&self, addr: i32, offset: u64) -> i64 {
        i64::from(u32::from_le_bytes(self.read(addr, offset)))
    }

    /// `i32.store8` / `i64.store8`
    pub fn store8(&mut self, addr: i32, offset: u64, value: i64) {
        self.write(addr, offset, [value as u8]);
    }
    /// `i32.store16` / `i64.store16`
    pub fn store16(&mut self, addr: i32, offset: u64, value: i64) {
        self.write(addr, offset, (value as u16).to_le_bytes());
    }
    /// `i32.store` / `i64.store32`
    pub fn store32(&mut self, addr: i32, offset: u64, value: i64) {
        self.write(addr, offset, (value as u32).to_le_bytes());
    }
    /// `i64.store`
    pub fn store64(&mut self, addr: i32, offset: u64, value: i64) {
        self.write(addr, offset, value.to_le_bytes());
    }
    /// `f32.store`
    pub fn store_f32(&mut self, addr: i32, offset: u64, value: f32) {
        self.write(addr, offset, value.to_le_bytes());
    }
    /// `f64.store`
    pub fn store_f64(&mut self, addr: i32, offset: u64, value: f64) {
        self.write(addr, offset, value.to_le_bytes());
    }

    /// `memory.fill`. A zero-length fill at the end of memory is legal; one
    /// past it is not, which is why the check runs even when `len` is zero.
    pub fn fill(&mut self, addr: i32, value: i32, len: i32) {
        let at = self.range(addr, 0, u64::from(len as u32));
        let end = at + len as u32 as usize;
        self.data[at..end].fill(value as u8);
    }

    /// `memory.copy`. Overlapping ranges move as `memmove` does.
    pub fn copy(&mut self, dst: i32, src: i32, len: i32) {
        let len = len as u32 as usize;
        let to = self.range(dst, 0, len as u64);
        let from = self.range(src, 0, len as u64);
        self.data.copy_within(from..from + len, to);
    }

    /// `memory.init`, and how a passive data segment reaches memory.
    pub fn init(&mut self, dst: i32, segment: &[u8], src: i32, len: i32) {
        let len = len as u32 as usize;
        let src = src as u32 as usize;
        if src + len > segment.len() {
            trap("out of bounds memory access");
        }
        let to = self.range(dst, 0, len as u64);
        self.data[to..to + len].copy_from_slice(&segment[src..src + len]);
    }
}

/// `i32.div_s`. Traps on zero and on `i32::MIN / -1`, which has no result.
pub fn i32_div_s(lhs: i32, rhs: i32) -> i32 {
    if rhs == 0 {
        trap("integer divide by zero");
    }
    match lhs.checked_div(rhs) {
        Some(result) => result,
        None => trap("integer overflow"),
    }
}

/// `i32.div_u`
pub fn i32_div_u(lhs: i32, rhs: i32) -> i32 {
    if rhs == 0 {
        trap("integer divide by zero");
    }
    ((lhs as u32) / (rhs as u32)) as i32
}

/// `i32.rem_s`. `i32::MIN % -1` is 0 in wasm and a panic in Rust, hence
/// `wrapping_rem`.
pub fn i32_rem_s(lhs: i32, rhs: i32) -> i32 {
    if rhs == 0 {
        trap("integer divide by zero");
    }
    lhs.wrapping_rem(rhs)
}

/// `i32.rem_u`
pub fn i32_rem_u(lhs: i32, rhs: i32) -> i32 {
    if rhs == 0 {
        trap("integer divide by zero");
    }
    ((lhs as u32) % (rhs as u32)) as i32
}

/// `i64.div_s`
pub fn i64_div_s(lhs: i64, rhs: i64) -> i64 {
    if rhs == 0 {
        trap("integer divide by zero");
    }
    match lhs.checked_div(rhs) {
        Some(result) => result,
        None => trap("integer overflow"),
    }
}

/// `i64.div_u`
pub fn i64_div_u(lhs: i64, rhs: i64) -> i64 {
    if rhs == 0 {
        trap("integer divide by zero");
    }
    ((lhs as u64) / (rhs as u64)) as i64
}

/// `i64.rem_s`
pub fn i64_rem_s(lhs: i64, rhs: i64) -> i64 {
    if rhs == 0 {
        trap("integer divide by zero");
    }
    lhs.wrapping_rem(rhs)
}

/// `i64.rem_u`
pub fn i64_rem_u(lhs: i64, rhs: i64) -> i64 {
    if rhs == 0 {
        trap("integer divide by zero");
    }
    ((lhs as u64) % (rhs as u64)) as i64
}

/// `f32.min`. Wasm propagates NaN; `f32::min` returns the other operand
/// instead, and `min(-0.0, 0.0)` must be `-0.0` rather than either.
pub fn f32_min(lhs: f32, rhs: f32) -> f32 {
    if lhs.is_nan() || rhs.is_nan() {
        return f32::NAN;
    }
    if lhs == 0.0 && rhs == 0.0 {
        return if lhs.is_sign_negative() { lhs } else { rhs };
    }
    if lhs < rhs { lhs } else { rhs }
}

/// `f32.max`
pub fn f32_max(lhs: f32, rhs: f32) -> f32 {
    if lhs.is_nan() || rhs.is_nan() {
        return f32::NAN;
    }
    if lhs == 0.0 && rhs == 0.0 {
        return if lhs.is_sign_positive() { lhs } else { rhs };
    }
    if lhs > rhs { lhs } else { rhs }
}

/// `f64.min`
pub fn f64_min(lhs: f64, rhs: f64) -> f64 {
    if lhs.is_nan() || rhs.is_nan() {
        return f64::NAN;
    }
    if lhs == 0.0 && rhs == 0.0 {
        return if lhs.is_sign_negative() { lhs } else { rhs };
    }
    if lhs < rhs { lhs } else { rhs }
}

/// `f64.max`
pub fn f64_max(lhs: f64, rhs: f64) -> f64 {
    if lhs.is_nan() || rhs.is_nan() {
        return f64::NAN;
    }
    if lhs == 0.0 && rhs == 0.0 {
        return if lhs.is_sign_positive() { lhs } else { rhs };
    }
    if lhs > rhs { lhs } else { rhs }
}

/// Traps when a float cannot be truncated into the target integer.
///
/// Shared by every `trunc` below: NaN has no integer value, and a value outside
/// the range would otherwise saturate silently under Rust's `as`.
fn check_trunc(is_nan: bool, in_range: bool) {
    if is_nan {
        trap("invalid conversion to integer");
    }
    if !in_range {
        trap("integer overflow");
    }
}

/// `i32.trunc_f32_s`
pub fn i32_trunc_f32_s(value: f32) -> i32 {
    check_trunc(
        value.is_nan(),
        value > -2147483904.0 && value < 2147483648.0,
    );
    value as i32
}
/// `i32.trunc_f32_u`
pub fn i32_trunc_f32_u(value: f32) -> i32 {
    check_trunc(value.is_nan(), value > -1.0 && value < 4294967296.0);
    value as u32 as i32
}
/// `i32.trunc_f64_s`
pub fn i32_trunc_f64_s(value: f64) -> i32 {
    check_trunc(
        value.is_nan(),
        value > -2147483649.0 && value < 2147483648.0,
    );
    value as i32
}
/// `i32.trunc_f64_u`
pub fn i32_trunc_f64_u(value: f64) -> i32 {
    check_trunc(value.is_nan(), value > -1.0 && value < 4294967296.0);
    value as u32 as i32
}
/// `i64.trunc_f32_s`
pub fn i64_trunc_f32_s(value: f32) -> i64 {
    check_trunc(
        value.is_nan(),
        (-9223372036854775808.0..9223372036854775808.0).contains(&value),
    );
    value as i64
}
/// `i64.trunc_f32_u`
pub fn i64_trunc_f32_u(value: f32) -> i64 {
    check_trunc(
        value.is_nan(),
        value > -1.0 && value < 18446744073709551616.0,
    );
    value as u64 as i64
}
/// `i64.trunc_f64_s`
pub fn i64_trunc_f64_s(value: f64) -> i64 {
    check_trunc(
        value.is_nan(),
        (-9223372036854775808.0..9223372036854775808.0).contains(&value),
    );
    value as i64
}
/// `i64.trunc_f64_u`
pub fn i64_trunc_f64_u(value: f64) -> i64 {
    check_trunc(
        value.is_nan(),
        value > -1.0 && value < 18446744073709551616.0,
    );
    value as u64 as i64
}

/// `f32.nearest` / `f64.nearest`: round half to even, not away from zero.
pub fn f32_nearest(value: f32) -> f32 {
    value.round_ties_even()
}
/// `f64.nearest`
pub fn f64_nearest(value: f64) -> f64 {
    value.round_ties_even()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_starts_zeroed_and_sized_in_pages() {
        let memory = Memory::new(2, None);
        assert_eq!(memory.size(), 2);
        assert_eq!(memory.data.len(), 2 * PAGE_SIZE);
        assert!(memory.data.iter().all(|&byte| byte == 0));
    }

    #[test]
    fn grow_reports_old_size_and_refuses_past_the_maximum() {
        let mut memory = Memory::new(1, Some(2));
        assert_eq!(memory.grow(1), 1);
        assert_eq!(memory.size(), 2);
        assert_eq!(memory.grow(1), -1);
        assert_eq!(memory.size(), 2);
        // Refusal must not have resized anything.
        assert_eq!(memory.data.len(), 2 * PAGE_SIZE);
    }

    #[test]
    fn grow_refuses_beyond_the_four_gigabyte_ceiling() {
        let mut memory = Memory::new(1, None);
        assert_eq!(memory.grow(65536), -1);
    }

    #[test]
    fn loads_and_stores_round_trip_at_every_width() {
        let mut memory = Memory::new(1, None);
        memory.store8(0, 0, 0xFF);
        memory.store16(0, 8, 0xBEEF);
        memory.store32(0, 16, 0x1234_5678);
        memory.store64(0, 24, 0x0102_0304_0506_0708);
        memory.store_f32(0, 40, 1.5);
        memory.store_f64(0, 48, -2.25);

        assert_eq!(memory.load8_u(0, 0), 255);
        assert_eq!(memory.load8_s(0, 0), -1);
        assert_eq!(memory.load16_u(0, 8), 0xBEEF);
        assert_eq!(memory.load16_s(0, 8), 0xBEEF_u16 as i16 as i32);
        assert_eq!(memory.load32(0, 16), 0x1234_5678);
        assert_eq!(memory.load64(0, 24), 0x0102_0304_0506_0708);
        assert_eq!(memory.load_f32(0, 40), 1.5);
        assert_eq!(memory.load_f64(0, 48), -2.25);
    }

    #[test]
    fn the_narrow_i64_loads_extend_by_their_sign() {
        let mut memory = Memory::new(1, None);
        memory.store8(0, 0, -1);
        memory.store16(0, 2, -2);
        memory.store32(0, 4, -3);
        assert_eq!(memory.load8_s_i64(0, 0), -1);
        assert_eq!(memory.load8_u_i64(0, 0), 255);
        assert_eq!(memory.load16_s_i64(0, 2), -2);
        assert_eq!(memory.load16_u_i64(0, 2), 65534);
        assert_eq!(memory.load32_s_i64(0, 4), -3);
        assert_eq!(memory.load32_u_i64(0, 4), 4294967293);
    }

    #[test]
    #[should_panic(expected = "out of bounds memory access")]
    fn the_address_is_unsigned_even_though_it_is_typed_i32() {
        // wasm addresses are u32, so -4 is 4294967292 — far past a one-page
        // memory, and a trap. Treating the i32 as signed would instead index
        // four bytes *before* the buffer, which is the bug this guards.
        Memory::new(1, None).load32(-4, 0);
    }

    #[test]
    #[should_panic(expected = "out of bounds memory access")]
    fn a_load_past_the_end_traps() {
        Memory::new(1, None).load32((PAGE_SIZE - 2) as i32, 0);
    }

    #[test]
    #[should_panic(expected = "out of bounds memory access")]
    fn the_offset_cannot_wrap_into_a_valid_address() {
        // addr + offset is computed in u64 precisely so this traps instead of
        // wrapping to a small, in-bounds address.
        Memory::new(1, None).load32(-1, u64::from(u32::MAX));
    }

    #[test]
    #[should_panic(expected = "out of bounds memory access")]
    fn a_store_past_the_end_traps() {
        Memory::new(1, None).store8(PAGE_SIZE as i32, 0, 1);
    }

    #[test]
    fn fill_and_copy_move_bytes_the_way_memmove_does() {
        let mut memory = Memory::new(1, None);
        memory.fill(0, 0xAB, 4);
        assert_eq!(memory.data[..5], [0xAB, 0xAB, 0xAB, 0xAB, 0x00]);
        // Overlapping forward copy: a naive byte loop would smear the source.
        memory.fill(0, 0, 16);
        memory.store32(0, 0, 0x0403_0201);
        memory.copy(2, 0, 4);
        assert_eq!(memory.data[..8], [1, 2, 1, 2, 3, 4, 0, 0]);
    }

    #[test]
    fn a_zero_length_fill_at_the_very_end_is_legal() {
        let mut memory = Memory::new(1, None);
        memory.fill(PAGE_SIZE as i32, 0, 0);
    }

    #[test]
    #[should_panic(expected = "out of bounds memory access")]
    fn a_zero_length_fill_past_the_end_is_not() {
        Memory::new(1, None).fill(PAGE_SIZE as i32 + 1, 0, 0);
    }

    #[test]
    #[should_panic(expected = "out of bounds memory access")]
    fn copy_checks_the_source_as_well_as_the_destination() {
        Memory::new(1, None).copy(0, (PAGE_SIZE - 2) as i32, 4);
    }

    #[test]
    fn init_copies_a_slice_of_a_segment() {
        let mut memory = Memory::new(1, None);
        memory.init(4, b"hello", 1, 3);
        assert_eq!(&memory.data[4..7], b"ell");
    }

    #[test]
    #[should_panic(expected = "out of bounds memory access")]
    fn init_checks_the_segment_bounds() {
        Memory::new(1, None).init(0, b"hi", 1, 4);
    }

    #[test]
    fn signed_division_traps_where_wasm_says_it_must() {
        assert_eq!(i32_div_s(7, 2), 3);
        assert_eq!(i32_div_s(-7, 2), -3);
        assert_eq!(i32_div_u(-1, 2), 0x7FFF_FFFF);
        assert_eq!(i64_div_s(-7, 2), -3);
        assert_eq!(i64_div_u(-1, 2), 0x7FFF_FFFF_FFFF_FFFF);
    }

    #[test]
    fn remainder_of_the_minimum_by_minus_one_is_zero_not_a_panic() {
        // The one case where Rust's `%` panics and wasm defines a result.
        assert_eq!(i32_rem_s(i32::MIN, -1), 0);
        assert_eq!(i64_rem_s(i64::MIN, -1), 0);
        assert_eq!(i32_rem_s(-7, 2), -1);
        assert_eq!(i32_rem_u(-1, 3), 0);
        assert_eq!(i64_rem_s(-7, 2), -1);
        assert_eq!(i64_rem_u(-1, 3), 0);
    }

    #[test]
    #[should_panic(expected = "integer divide by zero")]
    fn i32_division_by_zero_traps() {
        i32_div_s(1, 0);
    }

    #[test]
    #[should_panic(expected = "integer divide by zero")]
    fn i32_unsigned_division_by_zero_traps() {
        i32_div_u(1, 0);
    }

    #[test]
    #[should_panic(expected = "integer divide by zero")]
    fn i32_remainder_by_zero_traps() {
        i32_rem_s(1, 0);
    }

    #[test]
    #[should_panic(expected = "integer divide by zero")]
    fn i32_unsigned_remainder_by_zero_traps() {
        i32_rem_u(1, 0);
    }

    #[test]
    #[should_panic(expected = "integer divide by zero")]
    fn i64_division_by_zero_traps() {
        i64_div_s(1, 0);
    }

    #[test]
    #[should_panic(expected = "integer divide by zero")]
    fn i64_unsigned_division_by_zero_traps() {
        i64_div_u(1, 0);
    }

    #[test]
    #[should_panic(expected = "integer divide by zero")]
    fn i64_remainder_by_zero_traps() {
        i64_rem_s(1, 0);
    }

    #[test]
    #[should_panic(expected = "integer divide by zero")]
    fn i64_unsigned_remainder_by_zero_traps() {
        i64_rem_u(1, 0);
    }

    #[test]
    #[should_panic(expected = "integer overflow")]
    fn i32_minimum_divided_by_minus_one_traps() {
        i32_div_s(i32::MIN, -1);
    }

    #[test]
    #[should_panic(expected = "integer overflow")]
    fn i64_minimum_divided_by_minus_one_traps() {
        i64_div_s(i64::MIN, -1);
    }

    #[test]
    fn float_min_and_max_propagate_nan_and_keep_the_sign_of_zero() {
        assert!(f32_min(f32::NAN, 1.0).is_nan());
        assert!(f32_max(1.0, f32::NAN).is_nan());
        assert!(f64_min(f64::NAN, 1.0).is_nan());
        assert!(f64_max(1.0, f64::NAN).is_nan());
        assert!(f32_min(0.0, -0.0).is_sign_negative());
        assert!(f32_min(-0.0, 0.0).is_sign_negative());
        assert!(f32_max(0.0, -0.0).is_sign_positive());
        assert!(f32_max(-0.0, 0.0).is_sign_positive());
        assert!(f64_min(0.0, -0.0).is_sign_negative());
        assert!(f64_min(-0.0, 0.0).is_sign_negative());
        assert!(f64_max(0.0, -0.0).is_sign_positive());
        assert!(f64_max(-0.0, 0.0).is_sign_positive());
        assert_eq!(f32_min(1.0, 2.0), 1.0);
        assert_eq!(f32_max(1.0, 2.0), 2.0);
        assert_eq!(f64_min(1.0, 2.0), 1.0);
        assert_eq!(f64_max(1.0, 2.0), 2.0);
    }

    #[test]
    fn truncation_matches_wasm_at_the_boundaries() {
        assert_eq!(i32_trunc_f32_s(-1.9), -1);
        assert_eq!(i32_trunc_f32_u(4294967040.0), -256);
        assert_eq!(i32_trunc_f64_s(-2147483648.0), i32::MIN);
        assert_eq!(i32_trunc_f64_u(4294967295.0), -1);
        assert_eq!(i64_trunc_f32_s(-1.9), -1);
        assert_eq!(i64_trunc_f32_u(1.5), 1);
        assert_eq!(i64_trunc_f64_s(9223372036854774784.0), 9223372036854774784);
        assert_eq!(i64_trunc_f64_u(1.5), 1);
    }

    #[test]
    #[should_panic(expected = "invalid conversion to integer")]
    fn truncating_a_nan_traps_rather_than_saturating() {
        // `f32 as i32` would quietly give 0 here.
        i32_trunc_f32_s(f32::NAN);
    }

    #[test]
    #[should_panic(expected = "integer overflow")]
    fn truncating_out_of_range_traps() {
        i32_trunc_f32_s(3.0e9);
    }

    #[test]
    #[should_panic(expected = "invalid conversion to integer")]
    fn unsigned_f32_truncation_rejects_nan() {
        i32_trunc_f32_u(f32::NAN);
    }

    #[test]
    #[should_panic(expected = "integer overflow")]
    fn unsigned_f32_truncation_rejects_negatives() {
        i32_trunc_f32_u(-1.0);
    }

    #[test]
    #[should_panic(expected = "invalid conversion to integer")]
    fn signed_f64_truncation_rejects_nan() {
        i32_trunc_f64_s(f64::NAN);
    }

    #[test]
    #[should_panic(expected = "integer overflow")]
    fn signed_f64_truncation_rejects_out_of_range() {
        i32_trunc_f64_s(-2147483649.0);
    }

    #[test]
    #[should_panic(expected = "invalid conversion to integer")]
    fn unsigned_f64_truncation_rejects_nan() {
        i32_trunc_f64_u(f64::NAN);
    }

    #[test]
    #[should_panic(expected = "integer overflow")]
    fn unsigned_f64_truncation_rejects_out_of_range() {
        i32_trunc_f64_u(4294967296.0);
    }

    #[test]
    #[should_panic(expected = "invalid conversion to integer")]
    fn i64_from_f32_rejects_nan() {
        i64_trunc_f32_s(f32::NAN);
    }

    #[test]
    #[should_panic(expected = "integer overflow")]
    fn i64_from_f32_rejects_out_of_range() {
        i64_trunc_f32_s(1.0e19);
    }

    #[test]
    #[should_panic(expected = "invalid conversion to integer")]
    fn i64_unsigned_from_f32_rejects_nan() {
        i64_trunc_f32_u(f32::NAN);
    }

    #[test]
    #[should_panic(expected = "integer overflow")]
    fn i64_unsigned_from_f32_rejects_negatives() {
        i64_trunc_f32_u(-1.0);
    }

    #[test]
    #[should_panic(expected = "invalid conversion to integer")]
    fn i64_from_f64_rejects_nan() {
        i64_trunc_f64_s(f64::NAN);
    }

    #[test]
    #[should_panic(expected = "integer overflow")]
    fn i64_from_f64_rejects_out_of_range() {
        i64_trunc_f64_s(1.0e19);
    }

    #[test]
    #[should_panic(expected = "invalid conversion to integer")]
    fn i64_unsigned_from_f64_rejects_nan() {
        i64_trunc_f64_u(f64::NAN);
    }

    #[test]
    #[should_panic(expected = "integer overflow")]
    fn i64_unsigned_from_f64_rejects_out_of_range() {
        i64_trunc_f64_u(-1.0);
    }

    #[test]
    fn nearest_rounds_halves_to_even() {
        // 2.5 rounds to 2, not 3: `f32::round` would give 3.
        assert_eq!(f32_nearest(2.5), 2.0);
        assert_eq!(f32_nearest(3.5), 4.0);
        assert_eq!(f32_nearest(-2.5), -2.0);
        assert_eq!(f64_nearest(2.5), 2.0);
        assert_eq!(f64_nearest(-3.5), -4.0);
    }

    #[test]
    #[should_panic(expected = "wasm trap: nothing here")]
    fn trap_names_itself_as_a_trap() {
        trap("nothing here");
    }
}
