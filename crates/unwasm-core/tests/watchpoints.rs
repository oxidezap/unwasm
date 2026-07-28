//! Asking the running code who wrote an address.
//!
//! The question that costs a reader a day is "who zeroed 0x24bed0?", and the
//! way it is usually answered is by patching bytecode: pick a candidate, find
//! a dead assert body, compute the patch, run, repeat. Fifteen measurements to
//! close in on one function.
//!
//! The output of this decompiler *runs*, which makes that a search the machine
//! can do. `--instrument-stores` routes every write through the runtime's
//! watchpoint check; a host sets a range, runs, and gets the function index of
//! whoever wrote it — or a panic at the write, whose backtrace is the guest's
//! own call chain.

mod common;

use unwasm_core::{Module, codegen};

fn instrumented(module: &Module) -> String {
    codegen::generate_options(
        module,
        &codegen::Options {
            instrument_stores: true,
            ..codegen::Options::default()
        },
    )
    .expect("generates")[0]
        .contents
        .clone()
}

/// Three functions that write to the same address in three different ways.
const WRITERS: &str = r#"(module
    (memory (export "memory") 1)
    (func (export "store_it") (param i32)
        i32.const 256 local.get 0 i32.store)
    (func (export "zero_it")
        i32.const 256 i32.const 0 i32.const 4 memory.fill)
    (func (export "copy_it")
        i32.const 256 i32.const 512 i32.const 4 memory.copy)
    (func (export "elsewhere")
        i32.const 1024 i32.const 7 i32.store))"#;

#[test]
fn without_the_flag_nothing_is_instrumented() {
    let wasm = common::assemble("watch-plain", WRITERS);
    let code = common::decompile(&wasm);
    assert!(code.contains("self.memory.store32("), "{code}");
    assert!(
        !code.contains("self.memory.store32_at("),
        "the plain output pays nothing for a feature it does not have"
    );
    // The runtime still carries the methods — it is embedded whole — but
    // nothing calls them.
    assert!(code.contains("pub fn store32_at"), "{code}");
}

#[test]
fn instrumented_writes_carry_the_function_that_did_them() {
    let wasm = common::assemble("watch-instrumented", WRITERS);
    let module = Module::parse(&wasm).expect("valid");
    let code = instrumented(&module);
    // The site is the function index, which is what names.json looks up.
    assert!(code.contains("self.memory.store32_at(0,"), "{code}");
    assert!(code.contains("self.memory.fill_at(1,"), "{code}");
    assert!(code.contains("self.memory.copy_at(2,"), "{code}");
}

#[test]
fn a_watchpoint_names_the_function_that_wrote_the_address() {
    let wasm = common::assemble("watch-run", WRITERS);
    let module = Module::parse(&wasm).expect("valid");
    let code = instrumented(&module);

    const DRIVER: &str = r#"
mod generated;
fn main() {
    let mut instance = generated::Instance::new();
    instance.memory.watch(256, 4);
    instance.store_it(9);
    instance.zero_it();
    instance.copy_it();
    // Not watched, and must not appear.
    instance.elsewhere();
    for hit in instance.memory.hits() {
        println!("{} {:?} {:#x} {}", hit.site, hit.kind, hit.address, hit.bytes);
    }
}
"#;
    let output = common::run_with_generated("watch-run", &code, DRIVER);
    assert_eq!(
        output.trim().lines().collect::<Vec<_>>(),
        vec!["0 Store 0x100 4", "1 Fill 0x100 4", "2 Copy 0x100 4",],
        "who wrote it, how, and in what order"
    );
}

#[test]
fn stopping_at_the_write_gives_a_backtrace_through_the_guest() {
    // A panic rather than a log, so the stack shows the call chain that
    // reached the write — which is the thing a hit list does not tell you.
    let wasm = common::assemble(
        "watch-stop",
        r#"(module
            (memory (export "memory") 1)
            (func $inner i32.const 256 i32.const 1 i32.store)
            (func (export "outer") call $inner))"#,
    );
    let module = Module::parse(&wasm).expect("valid");
    let code = instrumented(&module);

    const DRIVER: &str = r#"
mod generated;
fn main() {
    let mut instance = generated::Instance::new();
    instance.memory.watch(256, 4);
    instance.memory.watch.stop = true;
    let stopped = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| instance.outer()));
    let payload = stopped.expect_err("the watchpoint stops the run");
    let message = payload
        .downcast_ref::<String>()
        .cloned()
        .unwrap_or_default();
    println!("{message}");
}
"#;
    let output = common::run_with_generated("watch-stop", &code, DRIVER);
    assert!(
        output.contains("watchpoint: function #0 wrote 4 bytes at 0x100"),
        "{output}"
    );
}

#[test]
fn instrumenting_does_not_change_what_the_module_computes() {
    // The whole point of an annotation rule applied to a runtime feature: if
    // routing a write through the check ever changed a result, this is what
    // would catch it.
    let wasm = common::assemble(
        "watch-agrees",
        r#"(module
            (memory (export "memory") 1)
            (data (i32.const 0) "abcd")
            (func (export "work") (param i32) (result i32)
                i32.const 16 local.get 0 i32.store
                i32.const 32 i32.const 255 i32.const 8 memory.fill
                i32.const 48 i32.const 0 i32.const 4 memory.copy
                i32.const 16 i32.load))"#,
    );
    let module = Module::parse(&wasm).expect("valid");

    const DRIVER: &str = r#"
mod generated;
fn main() {
    let mut instance = generated::Instance::new();
    println!("{}", instance.work(1234));
    println!("{:?}", &instance.memory.data[0..56]);
}
"#;
    let plain = common::run_with_generated(
        "watch-agrees-plain",
        &codegen::generate(&module).expect("generates"),
        DRIVER,
    );
    let watched = common::run_with_generated("watch-agrees-on", &instrumented(&module), DRIVER);
    assert_eq!(plain, watched);
    assert!(plain.starts_with("1234"), "{plain}");
}

#[test]
fn an_atomic_write_is_watched_too() {
    // Threads are where a "who wrote this" question usually ends up, and an
    // atomic store is not a store as far as the emitter is concerned.
    let wasm = common::assemble(
        "watch-atomic",
        r#"(module
            (memory (export "memory") 1 1 shared)
            (func (export "bump")
                i32.const 256 i32.const 1 i32.atomic.rmw.add drop))"#,
    );
    let module = Module::parse(&wasm).expect("valid");
    let code = instrumented(&module);
    assert!(code.contains("atomic_rmw_at(0,"), "{code}");

    const DRIVER: &str = r#"
mod generated;
fn main() {
    let mut instance = generated::Instance::new();
    instance.memory.watch(256, 4);
    instance.bump();
    println!("{:?}", instance.memory.hits());
}
"#;
    let output = common::run_with_generated("watch-atomic", &code, DRIVER);
    assert!(output.contains("site: 0"), "{output}");
    assert!(output.contains("Atomic"), "{output}");
}
