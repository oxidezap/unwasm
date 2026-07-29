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
use unwasm_core::{Module, codegen};

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

/// The whole thing, end to end: a C program with a `printf` in it, compiled by
/// Emscripten, decompiled to Rust, given the generated host, and run.
///
/// What this exercises is not the arithmetic — the differential tests cover
/// that — but the boundary. `printf` goes through musl's stdio, which formats
/// into a buffer and calls `fd_write` with an iovec array; the host has to
/// follow the pointers into linear memory to see any of it. Before the host
/// library existed, the furthest a decompiled Emscripten module could get was
/// instantiation.
#[test]
#[ignore = "needs emcc"]
fn a_decompiled_emscripten_program_prints_through_the_generated_host() {
    let wasm = compile_emscripten(
        "em-printf",
        r#"#include <stdio.h>
           #include <string.h>
           #include <stdlib.h>
           __attribute__((export_name("go")))
           int go(int n) {
               char *text = malloc(32);
               strcpy(text, "hello");
               printf("%s %d %.3f\n", text, n * 2, 1.5);
               fprintf(stderr, "done\n");
               free(text);
               return (int)strlen(text);
           }"#,
        "c",
        &["-O1"],
    );
    let module = Module::parse(&wasm).expect("the module decodes");
    let generated = codegen::generate(&module).expect("generates");
    let host = codegen::generate_host(&module).expect("generates a host");
    assert!(
        !host.contains("todo!(\""),
        "every import of this module is a mechanical one:\n{host}"
    );

    const DRIVER: &str = r#"
mod generated;
mod host;
fn main() {
    let mut instance = generated::Instance::with_host(host::Host::default());
    let returned = instance.go(21);
    print!("{}", instance.host.wasi.stdout_text());
    print!("{}", instance.host.wasi.stderr_text());
    println!("returned {returned}");
}
"#;
    let output = common::run_with_host("em-printf", &generated, &host, DRIVER);
    assert_eq!(
        output, "hello 42 1.500\ndone\nreturned 5\n",
        "the guest's own libc formatted every one of those"
    );
}

/// The thread model, against the toolchain rather than against a fixture.
///
/// Everything about `SharedMemory` and `Instance::spawn` was tested with wat
/// written by hand. This is a C program Emscripten compiled with `-pthread`:
/// its atomics are the ones its own libc uses, its memory is the shared one
/// the toolchain declares, and the threads are OS threads over one instance
/// each — which is exactly what an Emscripten pthread is.
///
/// It does not use the guest's `pthread_create`: that is an import, and
/// answering it needs a way back into the instance from a host. This drives
/// the threads from the Rust side, which is the half that exists.
#[test]
#[ignore = "needs emcc"]
fn threads_over_one_shared_memory_agree_with_the_toolchains_own_atomics() {
    let wasm = compile_emscripten(
        "em-threads",
        r#"#include <stdatomic.h>
           _Atomic int counter = 0;
           __attribute__((export_name("bump")))
           void bump(int times) {
               for (int i = 0; i < times; i++) atomic_fetch_add(&counter, 1);
           }
           __attribute__((export_name("read")))
           int read_counter(void) { return atomic_load(&counter); }
           // A frame worth colliding over: an array the compiler must put in
           // the shadow stack, written, left alone for a while, and checked.
           __attribute__((export_name("frame_survives")))
           int frame_survives(int seed, int spins) {
               volatile int scratch[64];
               for (int i = 0; i < 64; i++) scratch[i] = seed + i;
               for (volatile int s = 0; s < spins; s++) { }
               int intact = 0;
               for (int i = 0; i < 64; i++) intact += (scratch[i] == seed + i);
               return intact;
           }
        "#,
        "c",
        &["-O1", "-pthread"],
    );
    let module = Module::parse(&wasm).expect("the module decodes");
    assert!(
        module.memory.as_ref().is_some_and(|memory| memory.shared),
        "`-pthread` gives a shared memory, which is what makes this a thread test"
    );
    let generated = codegen::generate(&module).expect("generates");
    assert!(generated.contains("pub memory: rt::SharedMemory"), "shared");

    const DRIVER: &str = r#"
mod generated;
fn main() {
    let mut main_thread = generated::Instance::new();
    // Each worker gets a stack of its own, 64 KiB apart. Leaving them all on
    // the module's initial stack pointer is the bug this model exists to make
    // visible: they would write over each other's frames.
    let workers: Vec<_> = (0..4)
        .map(|index| {
            let mut worker = main_thread.spawn(generated::NoImports);
            worker.g0_stack_pointer = 0x200000 + index * 0x10000;
            std::thread::spawn(move || worker.bump(2000))
        })
        .collect();
    for worker in workers {
        worker.join().expect("each thread finishes");
    }
    println!("{}", main_thread.read());
}
"#;
    let output = common::run_with_generated("em-threads", &generated, DRIVER);
    assert_eq!(
        output.trim(),
        "8000",
        "four threads, two thousand atomic increments each, through the \
         toolchain's own `atomic_fetch_add` — nothing lost"
    );
}

/// A thread needs its own stack, and this is what happens when it has one.
///
/// The globals are per instance and the memory is not, so a thread's
/// `__stack_pointer` is its own — but only if somebody sets it. Left at the
/// module's initial value every thread carves its frame out of the same bytes,
/// and a local array in one is overwritten by another. That failure is
/// non-deterministic, so what this asserts is the other side of it: with a
/// stack each, a frame survives every time.
#[test]
#[ignore = "needs emcc"]
fn a_thread_with_its_own_stack_keeps_its_frame() {
    let wasm = compile_emscripten(
        "em-threads",
        r#"#include <stdatomic.h>
           _Atomic int counter = 0;
           __attribute__((export_name("bump")))
           void bump(int times) {
               for (int i = 0; i < times; i++) atomic_fetch_add(&counter, 1);
           }
           __attribute__((export_name("read")))
           int read_counter(void) { return atomic_load(&counter); }
           __attribute__((export_name("frame_survives")))
           int frame_survives(int seed, int spins) {
               volatile int scratch[64];
               for (int i = 0; i < 64; i++) scratch[i] = seed + i;
               for (volatile int s = 0; s < spins; s++) { }
               int intact = 0;
               for (int i = 0; i < 64; i++) intact += (scratch[i] == seed + i);
               return intact;
           }
        "#,
        "c",
        &["-O1", "-pthread"],
    );
    let module = Module::parse(&wasm).expect("the module decodes");
    let generated = codegen::generate(&module).expect("generates");

    const DRIVER: &str = r#"
mod generated;
fn main() {
    let main_thread = generated::Instance::new();
    let mut worst = 64;
    for round in 0..20 {
        let workers: Vec<_> = (0..4)
            .map(|index| {
                let mut worker = main_thread.spawn(generated::NoImports);
                // 64 KiB apart, which is more than this frame needs.
                worker.g0_stack_pointer = 0x300000 + index * 0x10000;
                std::thread::spawn(move || worker.frame_survives(round * 100 + index, 20000))
            })
            .collect();
        for worker in workers {
            worst = worst.min(worker.join().expect("each thread finishes"));
        }
    }
    println!("{worst}");
}
"#;
    let output = common::run_with_generated("em-threads-stack", &generated, DRIVER);
    assert_eq!(
        output.trim(),
        "64",
        "eighty frames, four at a time, none of them touched by another thread"
    );
}

/// The guest's own `pthread_create`, running.
///
/// This is the piece that could not be a host method: `__pthread_create_js`
/// has to reach back into the instance — spawn a thread, join it to this
/// memory, give it the stack the guest allocated, and call the start routine
/// through the table. So it is generated, like the `invoke_*` trampolines and
/// for the same reason: every part of it is already here.
///
/// What the test is really pinning is the order, which is the glue's own and
/// none of which is optional. Leave out the stack and threads destroy each
/// other's frames; leave out `_emscripten_thread_exit` and `pthread_join`
/// waits forever, which is exactly what happened while this was being written.
#[test]
#[ignore = "needs emcc"]
fn a_thread_the_guest_creates_itself_runs_and_joins() {
    let wasm = compile_emscripten(
        "em-pthread",
        r#"#include <pthread.h>
           #include <stdatomic.h>
           _Atomic int counter = 0;
           static void *bump(void *arg) {
               for (int i = 0; i < 1000; i++) atomic_fetch_add(&counter, 1);
               return arg;
           }
           __attribute__((export_name("go")))
           int go(void) {
               pthread_t thread;
               if (pthread_create(&thread, 0, bump, 0) != 0) return -1;
               if (pthread_join(thread, 0) != 0) return -2;
               return atomic_load(&counter);
           }"#,
        "c",
        &["-O1", "-pthread"],
    );
    let module = Module::parse(&wasm).expect("the module decodes");
    let analysis = unwasm_core::analysis::analyse(&module);
    let spawn = analysis.spawn.expect("the pthread glue is all there");
    assert!(
        spawn.thread_exit.is_some(),
        "without it a join never returns"
    );

    let generated = codegen::generate(&module).expect("generates");
    // The stack, then its limits, then thread_init, then the routine, then the
    // exit that releases the join.
    assert!(
        generated.contains("worker.g0_stack_pointer = high;"),
        "{generated}"
    );
    assert!(generated.contains("let mut worker = self.spawn(self.host.clone());"));
    let host = codegen::generate_host(&module).expect("generates a host");
    assert!(
        !host.contains("__pthread_create_js"),
        "it is generated, not asked of the host:\n{host}"
    );
    assert!(
        host.contains("pub wasi: std::sync::Arc<std::sync::Mutex<runtime::Wasi>>"),
        "and a threaded module's host is shared between its threads:\n{host}"
    );

    const DRIVER: &str = r#"
mod generated;
mod host;
fn main() {
    let mut instance = generated::Instance::with_host(host::Host::default());
    println!("{}", instance.go());
}
"#;
    let output = common::run_with_host("em-pthread", &generated, &host, DRIVER);
    assert_eq!(
        output.trim(),
        "1000",
        "the guest created a thread, it ran a thousand atomic increments on a \
         stack of its own, and the join returned"
    );
}
