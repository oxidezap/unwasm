//! The decompilation, run against the module it came from.
//!
//! Every test here compiles a fixture to wasm, decompiles it, compiles the
//! result with rustc, and runs both. Passing means the two agreed on every
//! returned value, every trap, and the final contents of linear memory.

mod common;

use common::{Arg::I32, Arg::I64, assert_agrees, call, compile_c};

/// Arithmetic, and the four cases where Rust and wasm genuinely differ:
/// overflow wraps, shifts are masked, signed division of the minimum traps,
/// and unsigned comparison is not signed comparison.
#[test]
fn integer_arithmetic_agrees_including_the_edges() {
    const SOURCE: &str = r#"
        int add(int a, int b)   { return a + b; }
        int mul(int a, int b)   { return a * b; }
        int divide(int a, int b){ return a / b; }
        int modulo(int a, int b){ return a % b; }
        unsigned shift(unsigned a, unsigned b) { return a << b; }
        unsigned shift_right(unsigned a, unsigned b) { return a >> b; }
        int less_signed(int a, int b) { return a < b; }
        int less_unsigned(unsigned a, unsigned b) { return a < b; }
    "#;
    let wasm = compile_c("arith", SOURCE, "-O2");
    let mut calls = Vec::new();
    for &(a, b) in &[
        (1, 2),
        (-1, 2),
        (i32::MAX, 1),
        (i32::MIN, -1),
        (0, 0),
        (-7, 2),
        (1, 33),
        (-1, 31),
    ] {
        for export in [
            "add",
            "mul",
            "divide",
            "modulo",
            "shift",
            "shift_right",
            "less_signed",
            "less_unsigned",
        ] {
            calls.push(call(export, &[I32(a), I32(b)]));
        }
    }
    assert_agrees("arith", &wasm, &calls);
}

/// Loops and branches: what `block`, `loop`, `br_if` and `br_table` become.
#[test]
fn control_flow_agrees() {
    const SOURCE: &str = r#"
        int count_down(int n) {
            int total = 0;
            while (n > 0) { total += n; n -= 1; }
            return total;
        }
        int classify(int n) {
            switch (n) {
                case 0: return 100;
                case 1: return 200;
                case 2: return 300;
                case 7: return 700;
                default: return -1;
            }
        }
        int nested(int a, int b) {
            int total = 0;
            for (int i = 0; i < a; i++) {
                for (int j = 0; j < b; j++) {
                    if ((i ^ j) & 1) continue;
                    if (i * j > 40) break;
                    total += i * j;
                }
            }
            return total;
        }
    "#;
    let wasm = compile_c("control", SOURCE, "-O1");
    let mut calls = Vec::new();
    for n in [-1, 0, 1, 2, 5, 7, 20] {
        calls.push(call("count_down", &[I32(n)]));
        calls.push(call("classify", &[I32(n)]));
        for m in [0, 3, 9] {
            calls.push(call("nested", &[I32(n), I32(m)]));
        }
    }
    assert_agrees("control", &wasm, &calls);
}

/// Memory: the shadow stack, struct layout, and out-of-bounds as a trap.
#[test]
fn linear_memory_agrees_and_out_of_bounds_traps_on_both_sides() {
    const SOURCE: &str = r#"
        static int table[64];
        struct Point { int x; short y; char tag; };

        int fill(int count) {
            for (int i = 0; i < count; i++) { table[i] = i * i; }
            int total = 0;
            for (int i = 0; i < 64; i++) { total += table[i]; }
            return total;
        }
        int through_a_struct(int x, int y) {
            struct Point point = { x, (short)y, (char)(x ^ y) };
            struct Point *alias = &point;
            alias->x += alias->y;
            return alias->x + alias->tag;
        }
        int read_at(int address) { return *(int *)address; }
        void write_at(int address, int value) { *(int *)address = value; }
    "#;
    let wasm = compile_c("memory", SOURCE, "-O1");
    let mut calls = vec![
        call("fill", &[I32(8)]),
        call("fill", &[I32(64)]),
        call("through_a_struct", &[I32(5), I32(-3)]),
        call("through_a_struct", &[I32(0), I32(70000)]),
        call("read_at", &[I32(0)]),
        call("write_at", &[I32(1024), I32(0x1234_5678)]),
        call("read_at", &[I32(1024)]),
    ];
    // Far past the end of a one-page memory: a trap on both sides, and the
    // interesting case, since a Rust `Vec` would otherwise panic differently or
    // an unchecked version would read the wrong bytes.
    calls.push(call("read_at", &[I32(0x7FFF_0000)]));
    calls.push(call("write_at", &[I32(-4), I32(1)]));
    assert_agrees("memory", &wasm, &calls);
}

/// 64-bit arithmetic, where JavaScript needs BigInt and a careless harness
/// loses the low bits.
#[test]
fn sixty_four_bit_arithmetic_agrees() {
    const SOURCE: &str = r#"
        long long mix(long long a, long long b) { return a * b + (a >> 13) - (b << 7); }
        long long divide(long long a, long long b) { return a / b; }
        unsigned long long shift(unsigned long long a, unsigned long long b) { return a >> b; }
        int narrow(long long a) { return (int)a; }
        long long widen(int a) { return (long long)a; }
    "#;
    let wasm = compile_c("wide", SOURCE, "-O2");
    let mut calls = Vec::new();
    for &(a, b) in &[
        (1i64, 2i64),
        (-1, 3),
        (i64::MAX, 2),
        (i64::MIN, -1),
        (0, 0),
        (0x0123_4567_89AB_CDEF, 65),
    ] {
        calls.push(call("mix", &[I64(a), I64(b)]));
        calls.push(call("divide", &[I64(a), I64(b)]));
        calls.push(call("shift", &[I64(a), I64(b)]));
        calls.push(call("narrow", &[I64(a)]));
    }
    for a in [0, -1, i32::MIN, i32::MAX] {
        calls.push(call("widen", &[I32(a)]));
    }
    assert_agrees("wide", &wasm, &calls);
}

/// Calls: direct, recursive, and through a function pointer, which is where
/// `call_indirect` and the element segment come in.
#[test]
fn calls_agree_including_indirect_ones() {
    const SOURCE: &str = r#"
        static int twice(int x)  { return x * 2; }
        static int square(int x) { return x * x; }
        static int negate(int x) { return -x; }
        static int (*const dispatch[3])(int) = { twice, square, negate };

        int apply(int which, int value) {
            if (which < 0 || which > 2) return -1;
            return dispatch[which](value);
        }
        int factorial(int n) { return n <= 1 ? 1 : n * factorial(n - 1); }
    "#;
    let wasm = compile_c("calls", SOURCE, "-O0");
    let mut calls = Vec::new();
    for which in [-1, 0, 1, 2, 3] {
        for value in [0, 7, -5] {
            calls.push(call("apply", &[I32(which), I32(value)]));
        }
    }
    for n in [0, 1, 5, 12] {
        calls.push(call("factorial", &[I32(n)]));
    }
    assert_agrees("calls", &wasm, &calls);
}

/// The same source at every optimisation level. -O0 keeps everything in the
/// shadow stack and -Oz restructures the control flow, so the two exercise
/// different halves of the backend.
#[test]
fn every_optimisation_level_agrees() {
    const SOURCE: &str = r#"
        int collatz(int n) {
            int steps = 0;
            while (n != 1 && steps < 1000) {
                n = (n & 1) ? 3 * n + 1 : n / 2;
                steps++;
            }
            return steps;
        }
    "#;
    for level in ["-O0", "-O1", "-O2", "-Os", "-Oz"] {
        let wasm = compile_c(&format!("levels{level}"), SOURCE, level);
        let calls: Vec<_> = [1, 2, 3, 6, 27, 97, -5, 0]
            .into_iter()
            .map(|n| call("collatz", &[I32(n)]))
            .collect();
        assert_agrees(&format!("levels{level}"), &wasm, &calls);
    }
}
