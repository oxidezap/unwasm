//! The `invoke_*` trampolines, generated rather than asked of the host.
//!
//! Emscripten routes any call that might throw through one of these. There are
//! 125 in the VoIP module — more than half its imports — and not one of them
//! says anything the module does not already say: the table is there, the stack
//! pointer is there, and `setThrew` is exported. So they are generated.
//!
//! The part worth testing is not that they call through the table. It is that
//! they catch a C++ exception and *do not* catch a trap: the glue's
//! `if (e !== e+0) throw e` is what keeps a crash from being quietly handled,
//! and a decompiler that lost it would turn every trap inside a `try` into a
//! silently swallowed error.

mod common;

use unwasm_core::{Module, analysis, codegen};

/// A module shaped like an Emscripten one: a table, a stack pointer, an
/// exported `setThrew`, and an `invoke_*` import.
fn module_with_trampoline() -> Vec<u8> {
    common::assemble(
        "trampoline",
        r#"(module
            (import "env" "invoke_vi" (func $invoke_vi (param i32 i32)))
            (import "env" "throwing" (func $throwing (param i32)))
            (memory (export "memory") 1)
            (global $sp (export "__stack_pointer") (mut i32) (i32.const 65536))
            (global $threw (mut i32) (i32.const 0))
            (type $unary_void (func (param i32)))

            ;; A function that reserves a frame, calls something through the
            ;; trampoline, and reports whether the call threw.
            (func (export "guarded") (param i32) (result i32)
                (local i32)
                global.get $sp
                i32.const 32
                i32.sub
                local.tee 1
                global.set $sp
                ;; frame[0] = 1234, so a lost stack pointer is visible
                local.get 1
                i32.const 1234
                i32.store
                ;; invoke_vi(table index 0, argument)
                i32.const 0
                local.get 0
                call $invoke_vi
                ;; the frame must still be ours
                local.get 1
                i32.load
                global.get $threw
                i32.add
                local.get 1
                i32.const 32
                i32.add
                global.set $sp)

            ;; What the table holds: throws when asked to.
            (func $target (type $unary_void)
                local.get 0
                call $throwing)
            (table 1 funcref)
            (elem (i32.const 0) $target)

            (func (export "setThrew") (param i32) (param i32)
                local.get 0
                global.set $threw)
            (func (export "stack_pointer_now") (result i32) global.get $sp)
            (func (export "threw") (result i32) global.get $threw))"#,
    )
}

#[test]
fn a_trampoline_is_recognised_and_taken_out_of_the_trait() {
    let wasm = module_with_trampoline();
    let module = Module::parse(&wasm).expect("valid");
    let analysis = analysis::analyse(&module);

    assert_eq!(analysis.invokes.len(), 1);
    assert_eq!(analysis.invokes[0].import, 0);
    assert!(analysis.set_threw.is_some(), "setThrew is exported");

    let code = codegen::generate(&module).expect("generates");
    assert!(
        !code.contains("fn env_invoke_vi"),
        "the trampoline is still being asked of the host:\n{code}"
    );
    assert!(code.contains("generated, not delegated"));
    // The other import is untouched.
    assert!(code.contains("fn env_throwing"));
}

/// The whole point: a thrown exception is caught, the stack pointer is put
/// back, and the module hears about it through its own `setThrew`.
#[test]
fn a_guest_exception_is_caught_and_reported_through_set_threw() {
    let wasm = module_with_trampoline();
    const DRIVER: &str = r#"
mod generated;

struct Host;

impl generated::Imports for Host {
    fn env_throwing(&mut self, _c: &mut generated::rt::Caller<'_>, p0: i32) {
        if p0 != 0 {
            // What a host's `__cxa_throw` does.
            generated::rt::throw(p0, 0);
        }
    }
}

fn main() {
    let mut instance = generated::Instance::with_host(Host);
    // 0: the callee returns normally. frame[0] + threw = 1234 + 0.
    println!("{}", instance.guarded(0));
    println!("threw={} sp={}", instance.threw(), instance.stack_pointer_now());

    let mut instance = generated::Instance::with_host(Host);
    // 7: the callee throws. The trampoline catches it, restores the stack
    // pointer, and sets threw — so the frame is still readable afterwards.
    println!("{}", instance.guarded(7));
    println!("threw={} sp={}", instance.threw(), instance.stack_pointer_now());
}
"#;
    let output = common::run_with_driver("trampoline-throw", &wasm, DRIVER);
    let lines: Vec<&str> = output.lines().collect();
    assert_eq!(
        lines[0], "1234",
        "a normal call returns and the frame survives"
    );
    assert_eq!(lines[1], "threw=0 sp=65536");
    // 1234 from the frame plus 1 from `threw`: the frame was not lost, and the
    // module learned that the call threw.
    assert_eq!(lines[2], "1235", "the exception was caught and reported");
    assert_eq!(lines[3], "threw=1 sp=65536");
}

/// A trap is not an exception. The glue re-throws anything that is not its own,
/// and so must this.
#[test]
fn a_trap_is_re_raised_rather_than_swallowed() {
    let wasm = module_with_trampoline();
    const DRIVER: &str = r#"
mod generated;

struct Host;

impl generated::Imports for Host {
    fn env_throwing(&mut self, _c: &mut generated::rt::Caller<'_>, p0: i32) {
        if p0 != 0 {
            // Not an exception: a trap, as an out-of-bounds access would be.
            generated::rt::trap("something went badly wrong");
        }
    }
}

fn main() {
    std::panic::set_hook(Box::new(|_| {}));
    let mut instance = generated::Instance::with_host(Host);
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| instance.guarded(7)));
    match outcome {
        Ok(value) => println!("swallowed, returned {value}"),
        Err(payload) => println!(
            "propagated: {}",
            payload.downcast_ref::<String>().cloned().unwrap_or_default()
        ),
    }
}
"#;
    let output = common::run_with_driver("trampoline-trap", &wasm, DRIVER);
    assert!(
        output.contains("propagated"),
        "a trap inside a trampoline must not be handled as an exception: {output}"
    );
    assert!(output.contains("something went badly wrong"), "{output}");
}

/// An indirect call to an empty table slot traps, and that trap is a trap —
/// not something the trampoline turns into a caught exception.
#[test]
fn a_missing_table_entry_still_traps_through_a_trampoline() {
    let wasm = common::assemble(
        "trampoline-empty",
        r#"(module
            (import "env" "invoke_v" (func $invoke_v (param i32)))
            (global $sp (export "__stack_pointer") (mut i32) (i32.const 65536))
            (type $nullary (func))
            (table 4 funcref)
            (func (export "call_slot") (param i32)
                local.get 0
                call $invoke_v)
            (func (export "setThrew") (param i32) (param i32))
            (func $filler (type $nullary)))"#,
    );
    const DRIVER: &str = r#"
mod generated;
fn main() {
    std::panic::set_hook(Box::new(|_| {}));
    let mut instance = generated::Instance::new();
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| instance.call_slot(2)));
    println!("{}", if outcome.is_ok() { "returned" } else { "trapped" });
}
"#;
    let output = common::run_with_driver("trampoline-empty", &wasm, DRIVER);
    assert_eq!(output.trim(), "trapped");
}

/// Without somewhere to report a caught exception, a trampoline cannot be
/// generated — and guessing would be worse than leaving it to the host.
#[test]
fn no_set_threw_means_no_generated_trampoline() {
    let wasm = common::assemble(
        "trampoline-nosetthrew",
        r#"(module
            (import "env" "invoke_v" (func $invoke_v (param i32)))
            (global $sp (export "__stack_pointer") (mut i32) (i32.const 65536))
            (type $nullary (func))
            (table 1 funcref)
            (func (export "f") (param i32) local.get 0 call $invoke_v)
            (func $filler (type $nullary)))"#,
    );
    let module = Module::parse(&wasm).expect("valid");
    let analysis = analysis::analyse(&module);
    assert!(analysis.set_threw.is_none());
    assert!(analysis.invokes.is_empty(), "nothing to report through");

    let code = codegen::generate(&module).expect("generates");
    assert!(
        code.contains("fn env_invoke_v"),
        "it stays an import the host must supply:\n{code}"
    );
}

#[test]
fn an_import_that_only_looks_like_a_trampoline_is_left_alone() {
    // The name matches, but the signature does not: no leading table index
    // means there is nothing to dispatch through.
    let wasm = common::assemble(
        "trampoline-shape",
        r#"(module
            (import "env" "invoke_later" (func $not_one (result i32)))
            (global $sp (export "__stack_pointer") (mut i32) (i32.const 65536))
            (func (export "setThrew") (param i32) (param i32))
            (func (export "f") (result i32) call $not_one))"#,
    );
    let module = Module::parse(&wasm).expect("valid");
    assert!(analysis::analyse(&module).invokes.is_empty());
    let code = codegen::generate(&module).expect("generates");
    assert!(code.contains("fn env_invoke_later"), "{code}");
}

/// The real thing: 125 of the VoIP module's imports.
#[test]
#[ignore = "reads the capture directory"]
fn the_voip_module_generates_all_of_its_trampolines() {
    let Some(bytes) = common::captured("D5pLH9sfOOl") else {
        panic!("D5pLH9sfOOl is not available; set WA_WASM_DIR");
    };
    let module = Module::parse(&bytes).expect("parses");
    let analysis = analysis::analyse(&module);

    let named: usize = module
        .func_imports
        .iter()
        .filter(|import| import.field.starts_with("invoke_"))
        .count();
    assert_eq!(named, 125, "the module's own count of trampolines");
    assert_eq!(
        analysis.invokes.len(),
        named,
        "every one of them has a dispatch type in the module"
    );

    let code = codegen::generate(&module).expect("generates");
    let asked_of_the_host = code
        .lines()
        .skip_while(|line| !line.contains("pub trait Imports"))
        .take_while(|line| !line.starts_with('}'))
        .filter(|line| line.trim_start().starts_with("fn "))
        .count();
    assert_eq!(
        asked_of_the_host,
        module.func_imports.len() - named,
        "the host is asked for exactly the imports that are not trampolines"
    );
    eprintln!(
        "D5pLH9sfOOl: {} imports, {named} generated, {asked_of_the_host} left for the host",
        module.func_imports.len()
    );
}
