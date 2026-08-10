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

/// The real thing: 134 of the VoIP module's imports.
#[test]
#[ignore = "reads the capture directory"]
fn the_voip_module_generates_all_of_its_trampolines() {
    let id = common::captures::VOIP;
    let Some(bytes) = common::captured(id) else {
        panic!("{}", common::missing_capture(id));
    };
    let module = Module::parse(&bytes).expect("parses");
    let analysis = analysis::analyse(&module);

    let named: usize = module
        .func_imports
        .iter()
        .filter(|import| import.field.starts_with("invoke_"))
        .count();
    assert_eq!(named, 134, "the module's own count of trampolines");
    assert_eq!(
        analysis.invokes.len(),
        named,
        "every one of them has a dispatch type in the module"
    );

    // An `invoke_*` is not the only import that gets generated rather than
    // asked for. `__pthread_create_js` and the main thread's initialiser have
    // to reach back into the instance — the table, the stack pointer, another
    // instance over the same memory — which the `Imports` trait cannot express,
    // so `import_thunks` generates those two the same way.
    //
    // Counting only the `invoke_*` ones was right until those were added, and
    // this assertion has been off by exactly two ever since. Nothing caught it
    // because no documented command runs this tier.
    let generated = named
        + usize::from(analysis.spawn.is_some())
        + usize::from(analysis.init_main_thread.is_some());

    let code = codegen::generate(&module).expect("generates");
    let asked_of_the_host = code
        .lines()
        .skip_while(|line| !line.contains("pub trait Imports"))
        .take_while(|line| !line.starts_with('}'))
        .filter(|line| line.trim_start().starts_with("fn "))
        .count();
    assert_eq!(
        asked_of_the_host,
        module.func_imports.len() - generated,
        "the host is asked for exactly the imports that are not generated"
    );
    eprintln!(
        "{id}: {} imports, {generated} generated ({named} of them `invoke_*`), \
         {asked_of_the_host} left for the host",
        module.func_imports.len()
    );
}

// ---- the pthread trampoline ----

/// A module shaped like an Emscripten `-pthread` one: a shared memory, the
/// `__pthread_create_js` import, and the three exports the glue calls on a new
/// thread.
///
/// `_emscripten_thread_init` here does what the real one does as far as this
/// is concerned — it records that it ran — and the start routine writes where
/// the test can see it.
const THREADED: &str = r#"(module
    (import "env" "__pthread_create_js" (func $create (param i32 i32 i32 i32) (result i32)))
    (import "env" "__emscripten_init_main_thread_js" (func $init_main (param i32)))
    (import "wasi_snapshot_preview1" "fd_write"
        (func $fd_write (param i32 i32 i32 i32) (result i32)))
    (import "env" "memory" (memory 1 4 shared))
    (global $sp (export "__stack_pointer") (mut i32) (i32.const 65536))
    (type $entry (func (param i32) (result i32)))
    (func (export "_emscripten_thread_init") (param i32 i32 i32 i32 i32 i32)
        i32.const 16 local.get 1 i32.store)
    (func (export "emscripten_stack_set_limits") (param i32 i32)
        i32.const 20 local.get 0 i32.store
        i32.const 24 local.get 1 i32.store)
    (func (export "_emscripten_thread_exit") (param i32)
        i32.const 28 local.get 0 i32.store)
    (func $routine (type $entry)
        i32.const 32 local.get 0 i32.store
        i32.const 36 global.get $sp i32.store
        local.get 0 i32.const 1 i32.add)
    (func (export "create") (param i32) (result i32)
        local.get 0 i32.const 0 i32.const 0 i32.const 7 call $create)
    (func (export "write") (result i32)
        i32.const 1 i32.const 64 i32.const 1 i32.const 80 call $fd_write)
    (func (export "init_main") (param i32) local.get 0 call $init_main)
    (table 1 funcref)
    (elem (i32.const 0) func 6))"#;

#[test]
fn the_pthread_glue_is_recognised_when_all_of_it_is_there() {
    let wasm = common::assemble("pthread", THREADED);
    let module = Module::parse(&wasm).expect("valid");
    let spawn = analysis::analyse(&module)
        .spawn
        .expect("every part of it is present");
    assert_eq!(spawn.import, 0);
    assert_eq!(spawn.thread_init_arity, 6);
    // The main thread's own setup is the same shape and the same reason.
    let init = analysis::analyse(&module)
        .init_main_thread
        .expect("the module asks for it");
    assert_eq!(init.thread_init, spawn.thread_init);
    assert!(spawn.stack_set_limits.is_some());
    assert!(spawn.thread_exit.is_some());
}

#[test]
fn the_pthread_glue_is_refused_when_a_part_is_missing() {
    // Each of these is a module that looks like it has threads and does not,
    // and generating a trampoline for any of them would be inventing one.
    let without_shared_memory = THREADED.replace(
        r#"(import "env" "memory" (memory 1 4 shared))"#,
        "(memory 1)",
    );
    let without_init = THREADED.replace(r#"(export "_emscripten_thread_init") "#, "");
    let wrong_shape = THREADED.replace(
        r#"(func $create (param i32 i32 i32 i32) (result i32))"#,
        r#"(func $create (param i32 i32) (result i32))"#,
    );
    for (name, wat) in [
        ("no-shared", without_shared_memory),
        ("no-init", without_init),
        ("wrong-shape", wrong_shape),
    ] {
        let wasm = common::assemble(&format!("pthread-{name}"), &wat);
        let module = Module::parse(&wasm).expect("valid");
        assert!(
            analysis::analyse(&module).spawn.is_none(),
            "{name}: a trampoline here would be invented"
        );
        // And what is not generated is asked of the host instead, rather than
        // quietly disappearing.
        if name != "wrong-shape" {
            let host = codegen::generate_host(&module).expect("generates");
            assert!(
                host.contains(r#"todo!("env::__pthread_create_js")"#),
                "{name}: {host}"
            );
        }
    }
}

#[test]
fn a_thread_the_guest_creates_gets_its_own_stack_and_is_joined() {
    // The order is the glue's own and none of it is optional: the stack out of
    // the pthread struct, its limits, `_emscripten_thread_init`, the routine,
    // and the exit a `pthread_join` is waiting for.
    let wasm = common::assemble("pthread-run", THREADED);
    let code = common::decompile(&wasm);
    assert!(
        code.contains("let mut worker = self.spawn(self.host.clone());"),
        "{code}"
    );
    assert!(code.contains("worker.g0_stack_pointer = high;"), "{code}");

    const DRIVER: &str = r#"
mod generated;
fn main() {
    let mut instance = generated::Instance::with_host(generated::NoImports);
    // A pthread struct at 1024: its stack is 8 KiB ending at 0x8000, written
    // where Emscripten's own `establishStackSpace` reads it.
    instance.memory.store32(1024, 48, 0x8000);
    instance.memory.store32(1024, 52, 0x2000);
    instance.create(1024);
    // The thread runs on another OS thread; wait for the exit it reports.
    for _ in 0..1000 {
        if instance.memory.load32(28, 0) != 0 { break; }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    println!("init ran: {}", instance.memory.load32(16, 0));
    println!("limits: {:#x} {:#x}", instance.memory.load32(20, 0), instance.memory.load32(24, 0));
    println!("routine saw arg {} on stack {:#x}", instance.memory.load32(32, 0), instance.memory.load32(36, 0));
    println!("exit reported {}", instance.memory.load32(28, 0));
}
"#;
    let output = common::run_with_driver("pthread-run", &wasm, DRIVER);
    let lines: Vec<&str> = output.trim().lines().collect();
    assert_eq!(lines[0], "init ran: 0", "is_main is 0 for a new thread");
    assert_eq!(
        lines[1], "limits: 0x8000 0x6000",
        "the stack it was given, and its low end"
    );
    assert_eq!(
        lines[2], "routine saw arg 7 on stack 0x8000",
        "its argument, and a stack pointer of its own rather than the main \
         thread's 0x10000"
    );
    assert_eq!(lines[3], "exit reported 8", "what the routine returned");
}

#[test]
fn a_thread_without_the_optional_exports_still_runs() {
    // `emscripten_stack_set_limits` and `_emscripten_thread_exit` are what the
    // glue calls when they are there. A module without them still gets a
    // thread — it just cannot report its limits or release a join, which is
    // the module's own shape rather than something to refuse over.
    let wat = THREADED
        .replace(r#"(export "emscripten_stack_set_limits") "#, "")
        .replace(r#"(export "_emscripten_thread_exit") "#, "");
    let wasm = common::assemble("pthread-minimal", &wat);
    let module = Module::parse(&wasm).expect("valid");
    let spawn = analysis::analyse(&module).spawn.expect("still recognised");
    assert!(spawn.stack_set_limits.is_none());
    assert!(spawn.thread_exit.is_none());

    let code = common::decompile(&wasm);
    assert!(
        code.contains("let _ = low;"),
        "nothing to report it to:\n{code}"
    );
    assert!(
        !code.contains("let result = worker.call_indirect_"),
        "and nothing to hand the result to"
    );
}

#[test]
fn a_thread_gets_no_stack_when_the_module_names_no_stack_pointer() {
    // Without one there is nowhere to put the stack the guest allocated, and
    // the thread runs on whatever the instance started with. The trampoline is
    // still generated — the alternative is no thread at all — and it simply
    // does not set what it cannot find.
    // The global stays; what goes is the export that names it, which is the
    // only evidence this module offers — its functions have no prologues.
    let wat = THREADED.replace(
        r#"(global $sp (export "__stack_pointer") (mut i32)"#,
        r#"(global $sp (mut i32)"#,
    );
    let wasm = common::assemble("pthread-no-sp", &wat);
    let module = Module::parse(&wasm).expect("valid");
    let analysis = analysis::analyse(&module);
    assert!(analysis.stack_pointer.is_none());
    assert!(analysis.spawn.is_some(), "still a thread");

    let code = common::decompile(&wasm);
    assert!(code.contains("let mut worker = self.spawn("), "{code}");
    assert!(
        !code.contains("let high = worker.memory.load32"),
        "nothing to read it into:\n{code}"
    );
}

#[test]
fn a_thread_init_that_is_not_the_one_this_knows_is_refused() {
    // The name is right and the shape is not: passing zeros to something that
    // takes a float would be a guess about a different function.
    let wat = THREADED.replace(
        r#"(func (export "_emscripten_thread_init") (param i32 i32 i32 i32 i32 i32)"#,
        r#"(func (export "_emscripten_thread_init") (param i32 f64 i32 i32 i32 i32)"#,
    );
    let wasm = common::assemble("pthread-odd-init", &wat);
    let module = Module::parse(&wasm).expect("valid");
    assert!(analysis::analyse(&module).spawn.is_none());
}

#[test]
fn the_main_thread_initialises_itself_through_the_module() {
    // The glue answers `__emscripten_init_main_thread_js` by calling the
    // module's own `_emscripten_thread_init` with `is_main` set. It reaches
    // back into the instance, so a host cannot do it — and until it was
    // generated it was the one import a run of the VoIP module had to answer
    // with a default.
    let wasm = common::assemble("pthread-main", THREADED);
    let module = Module::parse(&wasm).expect("valid");
    let host = codegen::generate_host(&module).expect("generates");
    assert!(
        !host.contains("__emscripten_init_main_thread_js"),
        "generated, not asked for:\n{host}"
    );

    const DRIVER: &str = r#"
mod generated;
fn main() {
    let mut instance = generated::Instance::with_host(generated::NoImports);
    instance.init_main(4096);
    // `_emscripten_thread_init` here records its `is_main` argument, which the
    // glue passes as 1 for the main thread and 0 for a worker.
    println!("is_main {}", instance.memory.load32(16, 0));
}
"#;
    let output = common::run_with_driver("pthread-main", &wasm, DRIVER);
    assert_eq!(output.trim(), "is_main 1");
}

#[test]
fn an_init_main_thread_of_another_shape_is_left_to_the_host() {
    // The name is right and the signature is not: passing a pthread pointer to
    // something that takes two arguments would be a guess about a different
    // function.
    let wat = THREADED
        .replace(
            r#"(func $init_main (param i32))"#,
            r#"(func $init_main (param i32 i32))"#,
        )
        .replace(
            "(func (export \"init_main\") (param i32) local.get 0 call $init_main)",
            "(func (export \"init_main\") (param i32) local.get 0 local.get 0 call $init_main)",
        );
    let wasm = common::assemble("pthread-main-shape", &wat);
    let module = Module::parse(&wasm).expect("valid");
    assert!(analysis::analyse(&module).init_main_thread.is_none());
}

#[test]
fn an_init_main_thread_without_the_module_half_is_left_to_the_host() {
    let wat = THREADED.replace(r#"(export "_emscripten_thread_init") "#, "");
    let wasm = common::assemble("pthread-main-none", &wat);
    let module = Module::parse(&wasm).expect("valid");
    assert!(analysis::analyse(&module).init_main_thread.is_none());
    let host = codegen::generate_host(&module).expect("generates");
    assert!(
        host.contains(r#"todo!("env::__emscripten_init_main_thread_js")"#),
        "{host}"
    );
}

#[test]
fn a_threaded_module_asks_its_host_to_cross_threads() {
    let wasm = common::assemble("pthread-host", THREADED);
    let module = Module::parse(&wasm).expect("valid");
    let code = common::decompile(&wasm);
    assert!(
        code.contains("pub trait Imports: Clone + Send + 'static"),
        "a thread needs a host of its own:\n{code}"
    );

    let host = codegen::generate_host(&module).expect("generates");
    assert!(
        host.contains("pub wasi: std::sync::Arc<std::sync::Mutex<runtime::Wasi>>"),
        "and a copy per thread would be four filesystems that agree about \
         nothing:\n{host}"
    );
    assert!(host.contains("#[derive(Debug, Default, Clone)]"), "{host}");
    // And a mechanical body reaches that state through the lock, rather than
    // there being a second table of what each import does.
    assert!(
        host.contains(
            "self.wasi.lock().unwrap_or_else(|held| held.into_inner())\n            .fd_write(caller, p0, p1, p2, p3)"
        ) || host.contains(
            "self.wasi.lock().unwrap_or_else(|held| held.into_inner()).fd_write(caller, p0, p1, p2, p3)"
        ),
        "{host}"
    );
    assert!(
        !host.contains("__pthread_create_js"),
        "the trampoline is generated, not asked for"
    );
}
