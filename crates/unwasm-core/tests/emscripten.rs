//! Modules built the way the real ones were.
//!
//! The clang fixtures cover the code generator; these cover the *toolchain*.
//! Emscripten links its own libc, its own `malloc`, and its own C++ runtime, so
//! a module built with it exercises tens of thousands of instructions that no
//! hand-written fixture reaches — and it is what the captured WhatsApp modules
//! are built with.
//!
//! `#[ignore]`d because emcc is a heavy dependency and the first build populates
//! its sysroot cache:
//!
//! ```sh
//! cargo test --test emscripten -- --ignored --nocapture
//! ```
//!
//! `-sSTANDALONE_WASM` is what makes the comparison possible: without it the
//! module expects the JavaScript glue, and neither side could call it directly.

mod common;

use common::{Arg::I32, Arg::I64, assert_agrees, call, compile_emscripten};

#[test]
#[ignore = "needs emcc"]
fn emscripten_c_agrees_at_every_optimisation_level() {
    const SOURCE: &str = r#"
        __attribute__((export_name("collatz")))
        int collatz(int n) {
            int steps = 0;
            while (n != 1 && steps < 1000) {
                n = (n & 1) ? 3 * n + 1 : n / 2;
                steps++;
            }
            return steps;
        }
        __attribute__((export_name("mix")))
        long long mix(long long a, long long b) {
            return a * b + (a >> 13) - (b << 7);
        }
    "#;
    for level in ["-O0", "-O2", "-Oz"] {
        let name = format!("em-levels{level}");
        let wasm = compile_emscripten(&name, SOURCE, "c", &[level]);
        let mut calls: Vec<_> = [1, 2, 27, 97, -5, 0]
            .into_iter()
            .map(|n| call("collatz", &[I32(n)]))
            .collect();
        for &(a, b) in &[(1i64, 2i64), (-1, 3), (i64::MAX, 2), (i64::MIN, -1)] {
            calls.push(call("mix", &[I64(a), I64(b)]));
        }
        assert_agrees(&name, &wasm, &calls);
    }
}

/// Emscripten's libc: `malloc`, `memcpy`, `strlen` and the rest, linked in and
/// run for real. This is the first fixture where the decompilation executes
/// thousands of functions it was never shown.
#[test]
#[ignore = "needs emcc"]
fn emscripten_libc_agrees() {
    const SOURCE: &str = r#"
        #include <stdlib.h>
        #include <string.h>

        __attribute__((export_name("heap_roundtrip")))
        int heap_roundtrip(int count) {
            if (count < 0 || count > 4096) return -1;
            char *buffer = malloc((size_t)count + 1);
            if (!buffer) return -2;
            for (int i = 0; i < count; i++) buffer[i] = (char)('a' + (i % 26));
            buffer[count] = '\0';

            char *copy = malloc((size_t)count + 1);
            memcpy(copy, buffer, (size_t)count + 1);
            int total = (int)strlen(copy);
            for (int i = 0; i < count; i++) total += copy[i];

            free(buffer);
            free(copy);
            return total;
        }

        __attribute__((export_name("sorted")))
        int sorted(int count) {
            if (count < 0 || count > 512) return -1;
            int *values = malloc(sizeof(int) * (size_t)count);
            for (int i = 0; i < count; i++) values[i] = (i * 7919) % 1000;
            // A small insertion sort: branch-heavy, and it reads back what it wrote.
            for (int i = 1; i < count; i++) {
                int key = values[i], j = i - 1;
                while (j >= 0 && values[j] > key) { values[j + 1] = values[j]; j--; }
                values[j + 1] = key;
            }
            int checksum = 0;
            for (int i = 0; i < count; i++) checksum = checksum * 31 + values[i];
            free(values);
            return checksum;
        }
    "#;
    let wasm = compile_emscripten("em-libc", SOURCE, "c", &["-O2"]);
    let mut calls = Vec::new();
    for count in [0, 1, 26, 100, 4096, -1, 5000] {
        calls.push(call("heap_roundtrip", &[I32(count)]));
    }
    for count in [0, 1, 10, 512, 513] {
        calls.push(call("sorted", &[I32(count)]));
    }
    assert_agrees("em-libc", &wasm, &calls);
}

/// C++ with virtual dispatch: the vtable lands in the element segment and the
/// call goes through `call_indirect`, which is the shape `oracle abi` calls a
/// trampoline in the real modules.
#[test]
#[ignore = "needs emcc"]
fn emscripten_cxx_virtual_dispatch_agrees() {
    const SOURCE: &str = r#"
        struct Shape {
            virtual int area(int size) const = 0;
            virtual ~Shape() {}
        };
        struct Square : Shape {
            int area(int size) const override { return size * size; }
        };
        struct Triangle : Shape {
            int area(int size) const override { return size * size / 2; }
        };

        extern "C" {
        __attribute__((export_name("area_of")))
        int area_of(int which, int size) {
            Shape *shape = which ? static_cast<Shape *>(new Triangle()) : new Square();
            int result = shape->area(size);
            delete shape;
            return result;
        }
        }
    "#;
    let wasm = compile_emscripten("em-cxx", SOURCE, "cpp", &["-O1"]);
    let mut calls = Vec::new();
    for which in [0, 1] {
        for size in [0, 3, 7, -4, 46341] {
            calls.push(call("area_of", &[I32(which), I32(size)]));
        }
    }
    assert_agrees("em-cxx", &wasm, &calls);
}

/// Floating point through Emscripten's libm.
#[test]
#[ignore = "needs emcc"]
fn emscripten_libm_agrees() {
    const SOURCE: &str = r#"
        #include <math.h>
        __attribute__((export_name("mixed")))
        double mixed(double x) {
            return sqrt(fabs(x)) + floor(x) - trunc(x / 3.0) + fmod(x, 7.0);
        }
        __attribute__((export_name("as_int")))
        int as_int(double x) { return (int)x; }
    "#;
    let wasm = compile_emscripten("em-libm", SOURCE, "c", &["-O2"]);
    let mut calls = Vec::new();
    for x in [
        0.0,
        -0.0,
        1.5,
        -2.25,
        1e300,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NAN,
        2147483648.0,
    ] {
        calls.push(call("mixed", &[common::Arg::F64(x)]));
        calls.push(call("as_int", &[common::Arg::F64(x)]));
    }
    assert_agrees("em-libm", &wasm, &calls);
}

/// What Emscripten produces that this version cannot model yet, refused by
/// name rather than mistranslated.
#[test]
#[ignore = "needs emcc"]
fn what_emscripten_emits_that_is_not_supported_is_named() {
    // C++ exceptions compile to the exception-handling proposal, whose tags
    // this version refuses.
    const SOURCE: &str = r#"
        extern "C" {
        __attribute__((export_name("risky")))
        int risky(int n) {
            try {
                if (n < 0) throw n;
                return n * 2;
            } catch (int caught) {
                return -caught;
            }
        }
        }
    "#;
    let wasm = compile_emscripten("em-eh", SOURCE, "cpp", &["-O1", "-fwasm-exceptions"]);
    match unwasm_core::Module::parse(&wasm) {
        Ok(module) => {
            // If the toolchain lowered exceptions without tags, it decompiles
            // and must then agree; there is nothing to refuse.
            unwasm_core::codegen::generate(&module).expect("it generates");
            eprintln!("exceptions lowered without tags; nothing refused");
        }
        Err(error) => {
            let message = error.to_string();
            assert!(message.contains("unsupported"), "{message}");
            eprintln!("refused as expected: {message}");
        }
    }
}
