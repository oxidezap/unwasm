//! Atomics, against the engine.
//!
//! A decompilation runs one thread, and under one thread almost every atomic
//! instruction does exactly what its plain counterpart does. "Almost" is what
//! these tests are for:
//!
//! - an atomic access must be naturally aligned or it traps, where a plain one
//!   is happy anywhere;
//! - a narrow read-modify-write wraps at the access width, not the value's;
//! - `memory.atomic.notify` wakes nobody, which is the right answer with one
//!   thread rather than a convenient one;
//! - `memory.atomic.wait*` is exactly right in two of its three outcomes, and
//!   traps in the third rather than inventing a notification.
//!
//! Node runs the reference side with a real shared memory, so the comparison is
//! against an engine that does have threads.

mod common;

use common::{Arg::I32, Arg::I64, assert_agrees, call};
use unwasm_core::{Module, codegen};

/// A module with a shared memory the harness supplies, as the VoIP module's
/// host does.
const HEADER: &str = r#"(module
    (import "env" "memory" (memory 1 4 shared))"#;

#[test]
fn atomic_loads_and_stores_agree() {
    let wat = format!(
        r#"{HEADER}
    (func (export "roundtrip") (param i32) (result i32)
        local.get 0 i32.const -1 i32.atomic.store
        local.get 0 i32.atomic.load)
    (func (export "narrow") (param i32) (result i32)
        local.get 0 i32.const 258 i32.atomic.store8
        local.get 0 i32.const 65534 i32.atomic.store16 offset=4
        local.get 0 i32.atomic.load8_u
        local.get 0 i32.atomic.load16_u offset=4
        i32.add)
    (func (export "wide") (param i32) (result i64)
        local.get 0 i64.const -2 i64.atomic.store
        local.get 0 i64.atomic.load)
    (func (export "wide_narrow") (param i32) (result i64)
        local.get 0 i64.const 300 i64.atomic.store32
        local.get 0 i64.atomic.load32_u
        local.get 0 i64.atomic.load8_u
        i64.add)
)"#
    );
    let wasm = common::assemble("atomic-mem", &wat);
    let mut calls = Vec::new();
    for address in [0, 8, 64, 4096] {
        calls.push(call("roundtrip", &[I32(address)]));
        calls.push(call("narrow", &[I32(address)]));
        calls.push(call("wide", &[I32(address)]));
        calls.push(call("wide_narrow", &[I32(address)]));
    }
    assert_agrees("atomic-mem", &wasm, &calls);
}

/// The one thing an atomic does that its plain counterpart does not.
#[test]
fn an_unaligned_atomic_traps_on_both_sides() {
    let wat = format!(
        r#"{HEADER}
    (func (export "plain") (param i32) (result i32)
        local.get 0 i32.const 7 i32.store
        local.get 0 i32.load)
    (func (export "atomic") (param i32) (result i32)
        local.get 0 i32.atomic.load)
    (func (export "atomic_16") (param i32) (result i32)
        local.get 0 i32.atomic.load16_u)
    (func (export "atomic_offset") (param i32) (result i32)
        local.get 0 i32.atomic.load offset=2)
)"#
    );
    let wasm = common::assemble("atomic-align", &wat);
    let mut calls = Vec::new();
    for address in [0, 1, 2, 3, 4, 8] {
        // The plain access succeeds at every one of these; the atomic does not.
        calls.push(call("plain", &[I32(address)]));
        calls.push(call("atomic", &[I32(address)]));
        calls.push(call("atomic_16", &[I32(address)]));
        calls.push(call("atomic_offset", &[I32(address)]));
    }
    assert_agrees("atomic-align", &wasm, &calls);
}

#[test]
fn read_modify_write_agrees_at_every_width() {
    let wat = format!(
        r#"{HEADER}
    (func (export "rmw32") (param i32) (param i32) (result i32)
        local.get 0 local.get 1 i32.atomic.store
        local.get 0 i32.const 5 i32.atomic.rmw.add drop
        local.get 0 i32.const 3 i32.atomic.rmw.sub drop
        local.get 0 i32.const 255 i32.atomic.rmw.and drop
        local.get 0 i32.const 256 i32.atomic.rmw.or drop
        local.get 0 i32.const 4095 i32.atomic.rmw.xor drop
        local.get 0 i32.atomic.load)
    (func (export "exchange") (param i32) (param i32) (result i32)
        local.get 0 local.get 1 i32.atomic.store
        local.get 0 i32.const 99 i32.atomic.rmw.xchg)
    (func (export "narrow_rmw") (param i32) (param i32) (result i32)
        local.get 0 local.get 1 i32.atomic.store8
        local.get 0 i32.const 10 i32.atomic.rmw8.add_u drop
        local.get 0 i32.atomic.load8_u)
    (func (export "wide_rmw") (param i32) (param i64) (result i64)
        local.get 0 local.get 1 i64.atomic.store
        local.get 0 i64.const 7 i64.atomic.rmw.add drop
        local.get 0 i64.const 1 i64.atomic.rmw32.xor_u drop
        local.get 0 i64.atomic.load)
)"#
    );
    let wasm = common::assemble("atomic-rmw", &wat);
    let mut calls = Vec::new();
    for value in [0, 1, -1, 250, i32::MAX, i32::MIN] {
        calls.push(call("rmw32", &[I32(0), I32(value)]));
        calls.push(call("exchange", &[I32(8), I32(value)]));
        calls.push(call("narrow_rmw", &[I32(16), I32(value)]));
        calls.push(call("wide_rmw", &[I32(24), I64(i64::from(value))]));
    }
    assert_agrees("atomic-rmw", &wasm, &calls);
}

#[test]
fn compare_and_exchange_agrees() {
    let wat = format!(
        r#"{HEADER}
    (func (export "swap") (param i32) (param i32) (param i32) (result i32)
        local.get 0 local.get 1 i32.atomic.store
        local.get 0 local.get 2 i32.const 42 i32.atomic.rmw.cmpxchg drop
        local.get 0 i32.atomic.load)
    (func (export "returns_old") (param i32) (param i32) (param i32) (result i32)
        local.get 0 local.get 1 i32.atomic.store
        local.get 0 local.get 2 i32.const 42 i32.atomic.rmw.cmpxchg)
    (func (export "narrow_swap") (param i32) (param i32) (result i32)
        local.get 0 i32.const 66 i32.atomic.store8
        local.get 0 local.get 1 i32.const 77 i32.atomic.rmw8.cmpxchg_u drop
        local.get 0 i32.atomic.load8_u)
)"#
    );
    let wasm = common::assemble("atomic-cmpxchg", &wat);
    let mut calls = Vec::new();
    for (stored, expected) in [(7, 7), (7, 8), (0, 0), (-1, -1), (-1, 0)] {
        calls.push(call("swap", &[I32(0), I32(stored), I32(expected)]));
        calls.push(call("returns_old", &[I32(4), I32(stored), I32(expected)]));
    }
    // The high bits of the expected value are not part of a one-byte compare —
    // 0xFF42 must not match a stored 0x42.
    for expected in [66, 0xFF42u32 as i32, 67] {
        calls.push(call("narrow_swap", &[I32(8), I32(expected)]));
    }
    assert_agrees("atomic-cmpxchg", &wasm, &calls);
}

/// The two outcomes of `wait` that need no second thread, and `notify`, which
/// has nobody to wake.
#[test]
fn waiting_and_notifying_agree_where_one_thread_can_answer() {
    let wat = format!(
        r#"{HEADER}
    ;; The value has already changed: not-equal, and no waiting happens.
    (func (export "wait_not_equal") (param i32) (result i32)
        local.get 0 i32.const 1 i32.atomic.store
        local.get 0 i32.const 999 i64.const -1 memory.atomic.wait32)
    ;; A zero timeout expires immediately however many threads are running.
    (func (export "wait_zero_timeout") (param i32) (result i32)
        local.get 0 i32.const 1 i32.atomic.store
        local.get 0 i32.const 1 i64.const 0 memory.atomic.wait32)
    (func (export "wait64_not_equal") (param i32) (result i32)
        local.get 0 i64.const 1 i64.atomic.store
        local.get 0 i64.const 999 i64.const -1 memory.atomic.wait64)
    ;; Nobody is waiting on it, so nobody is woken.
    (func (export "notify_nobody") (param i32) (result i32)
        local.get 0 i32.const 1 memory.atomic.notify)
    (func (export "fenced") (param i32) (result i32)
        atomic.fence
        local.get 0 i32.const 5 i32.atomic.store
        atomic.fence
        local.get 0 i32.atomic.load)
)"#
    );
    let wasm = common::assemble("atomic-wait", &wat);
    let mut calls = Vec::new();
    for address in [0, 8, 64] {
        calls.push(call("wait_not_equal", &[I32(address)]));
        calls.push(call("wait_zero_timeout", &[I32(address)]));
        calls.push(call("wait64_not_equal", &[I32(address)]));
        calls.push(call("notify_nobody", &[I32(address)]));
        calls.push(call("fenced", &[I32(address)]));
    }
    assert_agrees("atomic-wait", &wasm, &calls);
}

/// The outcome one thread cannot have: a wait that would only end when another
/// thread notifies.
#[test]
fn a_wait_that_would_block_forever_traps_and_says_why() {
    let wat = format!(
        r#"{HEADER}
    (func (export "wait_forever") (param i32) (result i32)
        local.get 0 i32.const 0 i64.const -1 memory.atomic.wait32)
)"#
    );
    let wasm = common::assemble("atomic-block", &wat);
    const DRIVER: &str = r#"
mod generated;
fn main() {
    std::panic::set_hook(Box::new(|_| {}));
    let mut instance = generated::Instance::new();
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| instance.wait_forever(0)));
    match outcome {
        Ok(value) => println!("returned {value}"),
        Err(payload) => println!("{}", payload.downcast_ref::<String>().cloned().unwrap_or_default()),
    }
}
"#;
    let output = common::run_with_driver("atomic-block", &wasm, DRIVER);
    assert!(
        output.contains("would block forever"),
        "a wait with no possible notifier must say so rather than time out: {output}"
    );
    assert!(
        output.contains("no other instance shares this memory"),
        "and must say why — which is now a fact about the handles on this \
         memory rather than an assumption about the model: {output}"
    );
}

/// A shared memory the module declares itself, rather than importing.
#[test]
fn a_module_that_declares_its_own_shared_memory_works_too() {
    let wasm = common::assemble(
        "own-shared",
        r#"(module
            (memory (export "memory") 1 2 shared)
            (func (export "bump") (param i32) (result i32)
                local.get 0 i32.const 1 i32.atomic.rmw.add))"#,
    );
    let calls: Vec<_> = (0..3).map(|_| call("bump", &[I32(0)])).collect();
    assert_agrees("own-shared", &wasm, &calls);
}

/// Every atomic instruction, one function each, against the engine.
///
/// The differential tests above cover the shapes that matter semantically.
/// This covers the *table*: sixty-three opcodes whose lowering is a line each,
/// and a line that names the wrong width or the wrong type would produce
/// plausible output and wrong answers.
#[test]
fn the_whole_atomic_table_agrees() {
    // (text name, what it pops after the address, whether it pushes)
    #[derive(Clone, Copy)]
    enum Shape {
        Load,
        Store(&'static str),
        Rmw(&'static str),
        Cmpxchg(&'static str),
        Wait(&'static str),
        Notify,
    }
    use Shape::{Cmpxchg, Load, Notify, Rmw, Store, Wait};

    let mut instructions: Vec<(String, Shape, &str)> = Vec::new();
    for (ty, widths) in [
        ("i32", vec!["", "8_u", "16_u"]),
        ("i64", vec!["", "8_u", "16_u", "32_u"]),
    ] {
        for width in widths {
            instructions.push((format!("{ty}.atomic.load{width}"), Load, ty));
        }
    }
    for (ty, widths) in [
        ("i32", vec!["", "8", "16"]),
        ("i64", vec!["", "8", "16", "32"]),
    ] {
        for width in widths {
            let value = if ty == "i32" {
                "i32.const 7"
            } else {
                "i64.const 7"
            };
            instructions.push((format!("{ty}.atomic.store{width}"), Store(value), ty));
        }
    }
    for (ty, widths) in [
        ("i32", vec![("", ""), ("8", "_u"), ("16", "_u")]),
        (
            "i64",
            vec![("", ""), ("8", "_u"), ("16", "_u"), ("32", "_u")],
        ),
    ] {
        for (width, suffix) in widths {
            let value = if ty == "i32" {
                "i32.const 3"
            } else {
                "i64.const 3"
            };
            for operation in ["add", "sub", "and", "or", "xor", "xchg"] {
                instructions.push((
                    format!("{ty}.atomic.rmw{width}.{operation}{suffix}"),
                    Rmw(value),
                    ty,
                ));
            }
            let pair = if ty == "i32" {
                "i32.const 0 i32.const 5"
            } else {
                "i64.const 0 i64.const 5"
            };
            instructions.push((
                format!("{ty}.atomic.rmw{width}.cmpxchg{suffix}"),
                Cmpxchg(pair),
                ty,
            ));
        }
    }
    instructions.push((
        "memory.atomic.wait32".to_string(),
        // An expected value the memory does not hold: not-equal, and no waiting.
        Wait("i32.const 987654 i64.const -1"),
        "i32",
    ));
    instructions.push((
        "memory.atomic.wait64".to_string(),
        Wait("i64.const 987654 i64.const -1"),
        "i32",
    ));
    instructions.push(("memory.atomic.notify".to_string(), Notify, "i32"));

    let mut wat = String::from("(module\n    (import \"env\" \"memory\" (memory 1 4 shared))\n");
    for (at, (name, shape, ty)) in instructions.iter().enumerate() {
        let result = match shape {
            Store(_) => "",
            Wait(_) | Notify => "i32",
            _ => ty,
        };
        let (operands, tail) = match shape {
            Load => (String::new(), String::new()),
            Store(value) | Rmw(value) => ((*value).to_string(), String::new()),
            Cmpxchg(pair) | Wait(pair) => ((*pair).to_string(), String::new()),
            Notify => ("i32.const 1".to_string(), String::new()),
        };
        wat.push_str(&format!(
            "    (func (export \"probe{at}\") (param i32){}\n        local.get 0 {operands} {name}{tail})\n",
            if result.is_empty() {
                String::new()
            } else {
                format!(" (result {result})")
            }
        ));
    }
    wat.push(')');

    let wasm = common::assemble("atomic-sweep", &wat);
    let module = unwasm_core::Module::parse(&wasm).expect("every instruction lowers");
    let lowered: usize = module
        .funcs
        .iter()
        .flat_map(|func| &func.body)
        .filter(|op| matches!(op, unwasm_core::module::Op::Atomic { .. }))
        .count();
    assert_eq!(
        lowered,
        instructions.len(),
        "an instruction was lowered to something other than an atomic"
    );
    // 7 loads + 7 stores + 49 read-modify-writes + wait32 + wait64 + notify.
    // `atomic.fence` is the sixty-seventh and takes no operands, so it is
    // covered separately.
    assert_eq!(instructions.len(), 66, "the table changed size");

    // Address 0 is aligned for every width, so each probe reaches its
    // instruction rather than the alignment trap.
    let calls: Vec<_> = (0..instructions.len())
        .map(|at| call(&format!("probe{at}"), &[I32(0)]))
        .collect();
    assert_agrees("atomic-sweep", &wasm, &calls);
}

// ---- threads ----

/// The model, end to end: two instances of one module over one memory.
///
/// This is what an Emscripten pthread is, and until there was a shared memory
/// here it could not be run at all — which is why a threaded module could be
/// read but never executed, and why "who wrote this address?" could not be
/// answered by running it.
#[test]
fn two_instances_over_one_memory_are_two_threads() {
    let wat = format!(
        r#"{HEADER}
    (global $sp (export "__stack_pointer") (mut i32) (i32.const 65536))
    (func (export "bump") (param i32) (result i32)
        local.get 0 i32.const 1 i32.atomic.rmw.add)
    (func (export "read") (param i32) (result i32)
        local.get 0 i32.atomic.load)
    (func (export "set_stack") (param i32) local.get 0 global.set $sp)
    (func (export "stack") (result i32) global.get $sp)
)"#
    );
    let wasm = common::assemble("threads-shared", &wat);
    let code = common::decompile(&wasm);
    assert!(
        code.contains("pub memory: rt::SharedMemory"),
        "a shared memory becomes a shared memory:\n{code}"
    );
    assert!(code.contains("pub fn spawn<G: Imports>"), "{code}");

    const DRIVER: &str = r#"
mod generated;
fn main() {
    let mut main_thread = generated::Instance::new();
    // Each thread gets its own stack pointer, which is the whole reason a
    // thread is a separate instance rather than a second entry point.
    main_thread.set_stack(0x10000);
    let workers: Vec<_> = (0..4)
        .map(|index| {
            let mut worker = main_thread.spawn(generated::NoImports);
            worker.set_stack(0x20000 + index * 0x1000);
            std::thread::spawn(move || {
                for _ in 0..1000 {
                    worker.bump(64);
                }
                worker.stack()
            })
        })
        .collect();
    let stacks: Vec<i32> = workers.into_iter().map(|w| w.join().expect("joins")).collect();
    println!("{}", main_thread.read(64));
    println!("{}", main_thread.stack());
    println!("{stacks:?}");
}
"#;
    let output = common::run_with_driver("threads-shared", &wasm, DRIVER);
    let lines: Vec<&str> = output.lines().collect();
    assert_eq!(
        lines[0], "4000",
        "four threads, a thousand atomic increments each, nothing lost"
    );
    assert_eq!(
        lines[1], "65536",
        "and the main thread's stack pointer is its own"
    );
    assert_eq!(lines[2], "[131072, 135168, 139264, 143360]");
}

#[test]
fn a_thread_joins_the_memory_rather_than_reinitialising_it() {
    // An engine does not place the data segments again for a new thread: the
    // bytes are already there, and placing them twice would undo whatever the
    // running program has done to them.
    let wat = format!(
        r#"{HEADER}
    (data (i32.const 128) "original")
    (func (export "overwrite") i32.const 128 i32.const 88 i32.const 8 memory.fill)
    (func (export "first") (result i32) i32.const 128 i32.load8_u)
)"#
    );
    let wasm = common::assemble("threads-data", &wat);

    const DRIVER: &str = r#"
mod generated;
fn main() {
    let mut instance = generated::Instance::new();
    println!("{}", instance.first());
    instance.overwrite();
    let worker = instance.spawn(generated::NoImports);
    println!("{}", instance.first());
    println!("{}", worker.data_dropped.len());
}
"#;
    let output = common::run_with_driver("threads-data", &wasm, DRIVER);
    let lines: Vec<&str> = output.lines().collect();
    assert_eq!(lines[0], "111", "`o` of \"original\"");
    assert_eq!(
        lines[1], "88",
        "spawning must not put the segment back over what the program wrote"
    );
    assert_eq!(lines[2], "1", "and the drop state travels with it");
}

#[test]
fn waiting_and_notifying_work_between_two_real_threads() {
    let wat = format!(
        r#"{HEADER}
    (func (export "wait") (param i32) (result i32)
        local.get 0 i32.const 0 i64.const -1 memory.atomic.wait32)
    (func (export "store_and_notify") (param i32) (param i32) (result i32)
        local.get 0 local.get 1 i32.atomic.store
        local.get 0 i32.const -1 memory.atomic.notify)
)"#
    );
    let wasm = common::assemble("threads-wait", &wat);

    const DRIVER: &str = r#"
mod generated;
fn main() {
    let main_thread = generated::Instance::new();
    let mut waiter = main_thread.spawn(generated::NoImports);
    let parked = std::thread::spawn(move || waiter.wait(0));
    // Store and notify until the waiter is actually parked. A notify that
    // arrives first is not lost — the value is compared under the parking
    // lock, so the waiter sees the change and returns 1 instead.
    let mut notifier = main_thread.spawn(generated::NoImports);
    let mut woken = 0;
    while woken == 0 {
        std::thread::yield_now();
        woken = notifier.store_and_notify(0, 0);
    }
    println!("{}", parked.join().expect("the waiter returns"));
}
"#;
    let output = common::run_with_driver("threads-wait", &wasm, DRIVER);
    assert_eq!(
        output.trim(),
        "0",
        "0 is woken-by-notify, which no single-threaded model can produce"
    );
}

#[test]
fn a_watchpoint_sees_the_thread_that_wrote() {
    // The question this whole model exists to answer: an address is cleared,
    // and nothing in the code says who did it. With threads over one memory
    // the watchpoint answers it from a run.
    let wat = format!(
        r#"{HEADER}
    (func (export "clear") i32.const 256 i32.const 0 i32.const 4 memory.fill)
    (func (export "poke") i32.const 512 i32.const 1 i32.store)
)"#
    );
    let wasm = common::assemble("threads-watch", &wat);
    let module = Module::parse(&wasm).expect("valid");
    let code = codegen::generate_options(
        &module,
        &codegen::Options {
            instrument_stores: true,
            ..codegen::Options::default()
        },
    )
    .expect("generates")[0]
        .contents
        .clone();

    const DRIVER: &str = r#"
mod generated;
fn main() {
    let mut instance = generated::Instance::new();
    instance.memory.watch(256, 4);
    let mut worker = instance.spawn(generated::NoImports);
    // The write happens on another thread entirely, and the hit still lands
    // in the one list.
    std::thread::spawn(move || worker.clear()).join().expect("joins");
    instance.poke();
    for hit in instance.memory.hits() {
        println!("{} {:?}", hit.site, hit.kind);
    }
}
"#;
    let output = common::run_with_generated("threads-watch", &code, DRIVER);
    assert_eq!(
        output.trim(),
        "0 Fill",
        "the memset on the other thread, and not the unwatched store"
    );
}

#[test]
fn a_threaded_module_hands_its_imports_the_shared_memory() {
    // The host's methods take whichever memory the module declared, so a host
    // written against `Caller` compiles either way — but the type it gets has
    // to be the right one, or nothing it reads is the memory the guest uses.
    let wat = format!(
        r#"{HEADER}
    (import "env" "log" (func (param i32)))
    (func (export "go") i32.const 1 call 0)
)"#
    );
    let wasm = common::assemble("threads-imports", &wat);
    let code = common::decompile(&wasm);
    assert!(
        code.contains(
            "fn env_log(&mut self, caller: &mut rt::Caller<'_, rt::SharedMemory>, p0: i32)"
        ),
        "{code}"
    );
    let host = codegen::generate_host(&Module::parse(&wasm).expect("valid")).expect("generates");
    assert!(
        host.contains("rt::Caller<'_, rt::SharedMemory>"),
        "and the skeleton agrees with it:\n{host}"
    );
}
