//! The host skeleton.
//!
//! What is left after decompiling a module is the part only a host can answer.
//! Writing 102 method signatures by hand is transcription, and the module
//! already describes every one of them — so the skeleton is generated, grouped
//! by where each import comes from.
//!
//! Every body is `todo!()`. That is the whole design: a skeleton returning zero
//! would compile, run, and be wrong, and the module could not tell "not written
//! yet" from "answered 0". The same reason `NoImports` traps.

mod common;

use unwasm_core::{Module, analysis, codegen};

const SAMPLE: &str = r#"(module
    (import "wasi_snapshot_preview1" "fd_write" (func (param i32 i32 i32 i32) (result i32)))
    (import "env" "__cxa_throw" (func (param i32 i32 i32)))
    (import "env" "emscripten_resize_heap" (func (param i32) (result i32)))
    (import "env" "_embind_register_class" (func (param i32 i32)))
    (import "env" "_emval_incref" (func (param i32)))
    (import "env" "on_call_event" (func (param i32 i32)))
    (func (export "go") (result i32) i32.const 1))"#;

#[test]
fn every_import_the_host_must_answer_appears_once() {
    let wasm = common::assemble("host-skeleton", SAMPLE);
    let module = Module::parse(&wasm).expect("valid");
    let skeleton = codegen::generate_host(&module).expect("generates");

    for method in [
        "fn wasi_snapshot_preview1_fd_write(&mut self, caller: &mut rt::Caller<'_>, p0: i32, p1: i32, p2: i32, p3: i32) -> i32",
        "fn env___cxa_throw(&mut self, caller: &mut rt::Caller<'_>, p0: i32, p1: i32, p2: i32)",
        "fn env_emscripten_resize_heap(&mut self, caller: &mut rt::Caller<'_>, p0: i32) -> i32",
        "fn env__embind_register_class(&mut self, _caller: &mut rt::Caller<'_>, p0: i32, p1: i32)",
        "fn env__emval_incref(&mut self, caller: &mut rt::Caller<'_>, p0: i32)",
        "fn env_on_call_event(&mut self, _caller: &mut rt::Caller<'_>, p0: i32, p1: i32)",
    ] {
        assert_eq!(
            skeleton.matches(method).count(),
            1,
            "expected exactly one `{method}` in:\n{skeleton}"
        );
    }
}

#[test]
fn imports_are_grouped_by_where_they_come_from() {
    let wasm = common::assemble("host-groups", SAMPLE);
    let module = Module::parse(&wasm).expect("valid");
    let skeleton = codegen::generate_host(&module).expect("generates");

    for heading in [
        "---- WASI",
        "---- The C++ runtime and syscalls",
        "---- Emscripten's runtime",
        "---- embind",
        "---- emval",
        "---- The application's own callbacks",
    ] {
        assert!(skeleton.contains(heading), "missing {heading}:\n{skeleton}");
    }
    // The application's callbacks come last: they are the ones with no
    // mechanical answer, and the ones a reader is looking for.
    let application = skeleton.find("---- The application").expect("present");
    let wasi = skeleton.find("---- WASI").expect("present");
    assert!(application > wasi);
}

/// The trait's own methods, without the library embedded above them.
fn trait_body(skeleton: &str) -> &str {
    skeleton
        .split_once("impl Imports for Host {")
        .expect("the trait is implemented")
        .1
}

#[test]
fn the_mechanical_imports_are_answered_and_the_rest_are_todos() {
    let wasm = common::assemble("host-todos", SAMPLE);
    let module = Module::parse(&wasm).expect("valid");
    let skeleton = codegen::generate_host(&module).expect("generates");
    let body = trait_body(&skeleton);

    assert_eq!(body.matches("\n    fn ").count(), 6);
    // Five of the six are the same for every Emscripten module, and are
    // written rather than left to the reader.
    assert!(
        body.contains("self.wasi.fd_write(caller, p0, p1, p2, p3)"),
        "{body}"
    );
    assert!(body.contains("self.cxx.throw(p0, p1)"), "{body}");
    assert!(
        body.contains("runtime::Emscripten::resize_heap(caller, p0)"),
        "{body}"
    );
    assert!(body.contains("self.embind.incref(p0)"), "{body}");

    // Two are left. `on_call_event` is the application's own, and nothing but
    // the application can answer it.
    assert_eq!(body.matches("todo!(\"").count(), 2, "{body}");
    assert!(body.contains(r#"todo!("env::on_call_event")"#), "{body}");
    // And this fixture's `_embind_register_class` takes two arguments where the
    // real one takes thirteen. A body written for the real shape would read the
    // wrong argument as the name, so the signature has to match exactly before
    // anything is claimed.
    assert!(
        body.contains(r#"todo!("env::_embind_register_class")"#),
        "{body}"
    );
    assert!(
        !body.contains("0i32"),
        "and nothing here returns a plausible value"
    );
}

#[test]
fn generated_trampolines_are_not_asked_of_the_host() {
    let wasm = common::assemble(
        "host-trampolines",
        r#"(module
            (import "env" "invoke_vi" (func $invoke_vi (param i32 i32)))
            (import "env" "real_import" (func (param i32)))
            (global $sp (export "__stack_pointer") (mut i32) (i32.const 65536))
            (type $unary (func (param i32)))
            (table 1 funcref)
            (func (export "setThrew") (param i32) (param i32))
            (func (export "f") (param i32) i32.const 0 local.get 0 call $invoke_vi)
            (func $filler (type $unary)))"#,
    );
    let module = Module::parse(&wasm).expect("valid");
    let skeleton = codegen::generate_host(&module).expect("generates");
    assert!(!skeleton.contains("invoke_vi"), "{skeleton}");
    assert!(skeleton.contains("env_real_import"));
    assert!(
        skeleton.contains("1 of the module's 2 imports"),
        "{skeleton}"
    );
}

/// The skeleton has to compile against the module it was generated for —
/// otherwise it is a list, not a starting point.
#[test]
fn the_skeleton_compiles_against_the_module_and_runs_what_it_can() {
    let wasm = common::assemble(
        "host-compiles",
        r#"(module
            (import "env" "add_ten" (func $add_ten (param i32) (result i32)))
            (import "env" "unused_callback" (func (param i32 i32)))
            (func (export "go") (param i32) (result i32)
                local.get 0
                call $add_ten))"#,
    );
    let module = Module::parse(&wasm).expect("valid");
    let skeleton = codegen::generate_host(&module).expect("generates");

    // The driver takes the skeleton as written, fills in the one method it
    // needs, and leaves the rest as `todo!()` — which is how it will actually
    // be used.
    let filled = skeleton.replace(
        "fn env_add_ten(&mut self, _caller: &mut rt::Caller<'_>, p0: i32) -> i32 {\n        todo!(\"env::add_ten\")",
        "fn env_add_ten(&mut self, _caller: &mut rt::Caller<'_>, p0: i32) -> i32 {\n        return p0 + 10;",
    );
    assert_ne!(filled, skeleton, "the method to fill was not found");

    let driver = format!(
        "mod generated;\nmod host {{\n{}\n}}\n\nfn main() {{\n    let mut instance = generated::Instance::with_host(host::Host::default());\n    println!(\"{{}}\", instance.go(5));\n}}\n",
        filled.replace(
            "use crate::generated::{self, Imports};",
            "use crate::generated::Imports;"
        )
    );
    let output = common::run_with_driver("host-compiles", &wasm, &driver);
    assert_eq!(output.trim(), "15");
}

/// The point of the `Caller`: an import that is handed a pointer can follow it.
///
/// This is `fd_write`'s shape in miniature — a string address and a place to
/// put the answer — and it is the thing that was impossible when an import
/// received only its arguments.
#[test]
fn a_host_reads_and_writes_the_memory_its_arguments_point_into() {
    let wasm = common::assemble(
        "host-caller",
        r#"(module
            (import "env" "shout" (func $shout (param i32 i32) (result i32)))
            (memory (export "memory") 1)
            (data (i32.const 64) "hello\00")
            ;; The guest passes an address and a place to write the answer, and
            ;; reads the answer back out of memory itself.
            (func (export "go") (result i32)
                i32.const 64
                i32.const 128
                call $shout
                drop
                i32.const 128
                i32.load))"#,
    );

    const DRIVER: &str = r#"
mod generated;
use generated::{rt, Imports};

#[derive(Default)]
struct Host {
    seen: String,
}

impl Imports for Host {
    fn env_shout(&mut self, caller: &mut rt::Caller<'_>, text: i32, out: i32) -> i32 {
        let bytes = caller.cstring(text).to_vec();
        self.seen = String::from_utf8(bytes.clone()).expect("utf-8");
        // Uppercase it in place, and report the length where the guest asked.
        let shouted: Vec<u8> = bytes.iter().map(|byte| byte.to_ascii_uppercase()).collect();
        caller.write(text, &shouted);
        caller.memory.store32(out, 0, shouted.len() as i64);
        0
    }
}

fn main() {
    let mut instance = generated::Instance::with_host(Host::default());
    let length = instance.go();
    let text = instance.host.seen.clone();
    let rewritten = String::from_utf8(instance.memory.data[64..69].to_vec()).expect("utf-8");
    println!("{text} {length} {rewritten}");
}
"#;
    let output = common::run_with_driver("host-caller", &wasm, DRIVER);
    assert_eq!(output.trim(), "hello 5 HELLO");
}

#[test]
fn a_module_that_needs_no_host_gets_an_empty_skeleton() {
    let wasm = common::assemble(
        "host-none",
        "(module (func (export \"f\") (result i32) i32.const 1))",
    );
    let module = Module::parse(&wasm).expect("valid");
    let skeleton = codegen::generate_host(&module).expect("generates");
    assert!(skeleton.contains("impl Imports for Host"));
    assert_eq!(trait_body(&skeleton).matches("\n    fn ").count(), 0);
    assert!(skeleton.contains("0 methods"));
}

/// Counts host methods from the start of `region` to the `}` that closes the
/// block it is inside, so it can be handed either the whole `impl` or one group
/// within it.
///
/// Both halves of this are load-bearing, and getting either wrong cancelled
/// against the other for a while:
///
/// - it counts *lines* at one indentation, not occurrences of `    fn `, which
///   is a substring of `        fn ` and so matched every nested helper;
/// - it is given the `impl` block rather than the whole skeleton, which embeds
///   `hostlib.rs` indented by four spaces inside `pub mod runtime` — putting
///   that file's top-level functions at exactly a trait method's indentation.
///
/// `hostlib.rs` contributes two (`civil_from_days`, `days_from_civil`), and
/// two imports are generated rather than asked for without being `invoke_*`
/// (`__pthread_create_js` and the main thread's initialiser). The count was
/// over by two and the expected value was over by two, so the assertion held
/// while both sides were wrong.
fn trait_methods(region: &str) -> usize {
    region
        .lines()
        .take_while(|line| !line.starts_with('}'))
        .filter(|line| line.starts_with("    fn "))
        .count()
}

/// The `impl Imports for Host` block, which is what the host actually has to
/// write. Everything above it is the embedded runtime.
fn imports_impl(skeleton: &str) -> &str {
    let at = skeleton
        .find("\nimpl Imports for Host")
        .expect("the skeleton implements the trait");
    &skeleton[at + 1..]
}

#[test]
#[ignore = "reads the capture directory"]
fn the_voip_module_leaves_every_import_that_is_not_a_trampoline() {
    let id = common::captures::VOIP;
    let Some(bytes) = common::captured(id) else {
        panic!("{}", common::missing_capture(id));
    };
    let module = Module::parse(&bytes).expect("parses");
    let analysis = analysis::analyse(&module);
    let skeleton = codegen::generate_host(&module).expect("generates");
    let implementation = imports_impl(&skeleton);
    let methods = trait_methods(implementation);

    // Stated as the arithmetic rather than as a number read off a run: 242
    // imports, 136 of them generated — 134 `invoke_*` plus the two that have to
    // reach back into the instance — and the host is asked for the other 106. A
    // capture that rolls changes both sides together, which a bare
    // `assert_eq!(methods, 106)` would not have said.
    let generated = analysis.invokes.len()
        + usize::from(analysis.spawn.is_some())
        + usize::from(analysis.init_main_thread.is_some());
    assert_eq!(
        methods,
        module.func_imports.len() - generated,
        "{} imports, {generated} of them generated",
        module.func_imports.len()
    );

    // The ones only the application can answer are what matter, and they are
    // the last group.
    let application = &implementation[implementation
        .find("---- The application")
        .expect("the module has callbacks of its own")..];
    let their_methods = trait_methods(application);
    eprintln!("{id}: {methods} methods for a host, {their_methods} of them WhatsApp's own");
    assert!(their_methods >= 30, "{their_methods} of the module's own");
}

#[test]
fn the_less_obvious_runtime_imports_are_grouped_with_the_runtime() {
    // `__assert_fail` and `__resumeException` belong to the C++ runtime as much
    // as `__cxa_throw` does, and neither starts with `__cxa_`.
    let wasm = common::assemble(
        "host-runtime-names",
        r#"(module
            (import "env" "__assert_fail" (func (param i32 i32 i32 i32)))
            (import "env" "__resumeException" (func (param i32)))
            (import "env" "__syscall_openat" (func (param i32 i32 i32 i32) (result i32)))
            (import "env" "getentropy" (func (param i32 i32) (result i32)))
            (import "env" "abort" (func))
            (func (export "f")))"#,
    );
    let module = Module::parse(&wasm).expect("valid");
    let skeleton = codegen::generate_host(&module).expect("generates");
    let runtime = skeleton
        .find("---- The C++ runtime")
        .expect("the group exists");
    let after = &skeleton[runtime..];
    for method in [
        "env___assert_fail",
        "env___resumeException",
        "env___syscall_openat",
        "env_getentropy",
        "env_abort",
    ] {
        assert!(after.contains(method), "{method} was grouped elsewhere");
    }
}

#[test]
fn a_registration_with_the_real_shape_is_answered() {
    // The thirteen-argument `_embind_register_class`, whose name is its
    // eleventh argument — the same table `unwasm inspect` reads statically, so
    // what a run reports and what the static reader claims cannot drift.
    let wasm = common::assemble(
        "host-embind-real",
        r#"(module
            (import "env" "_embind_register_class"
                (func (param i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32)))
            (import "env" "_embind_register_bigint"
                (func (param i32 i32 i32 i64 i64)))
            (func (export "go")))"#,
    );
    let module = Module::parse(&wasm).expect("valid");
    let body = trait_body(&codegen::generate_host(&module).expect("generates")).to_string();
    assert!(
        body.contains(r#"self.embind.register(caller, "class", p10, &[p0, p1, p2"#),
        "{body}"
    );
    // The i64 bounds of a bigint registration are not addresses and are left
    // out rather than cast into the list.
    assert!(
        body.contains(r#"self.embind.register(caller, "bigint", p1, &[p0, p2])"#),
        "{body}"
    );
}

#[test]
fn a_registration_that_carries_no_name_is_still_recorded() {
    // A class constructor has no name of its own — it is the class's. What it
    // registers, and in which order, is still the shape of the API, so it is
    // recorded rather than left as a `todo!()`.
    let wasm = common::assemble(
        "host-embind-unnamed",
        r#"(module
            (import "env" "_embind_register_class_constructor"
                (func (param i32 i32 i32 i32 i32 i32)))
            (func (export "go")))"#,
    );
    let module = Module::parse(&wasm).expect("valid");
    let body = trait_body(&codegen::generate_host(&module).expect("generates")).to_string();
    assert!(
        body.contains(
            r#"self.embind.register_unnamed("class_constructor", &[p0, p1, p2, p3, p4, p5])"#
        ),
        "{body}"
    );
    assert!(!body.contains("todo!(\""), "{body}");
}

#[test]
fn an_import_whose_signature_is_not_the_expected_one_is_left_alone() {
    // `fd_write` with three arguments is not the `fd_write` this host knows,
    // and guessing which three would be a body that compiles and lies.
    let wasm = common::assemble(
        "host-wrong-shape",
        r#"(module
            (import "wasi_snapshot_preview1" "fd_write" (func (param i32 i32 i32) (result i32)))
            (func (export "go")))"#,
    );
    let module = Module::parse(&wasm).expect("valid");
    let body = trait_body(&codegen::generate_host(&module).expect("generates")).to_string();
    assert!(
        body.contains(r#"todo!("wasi_snapshot_preview1::fd_write")"#),
        "{body}"
    );
}

#[test]
fn the_library_travels_with_the_skeleton() {
    // Embedded rather than depended on, exactly as the runtime is: a host file
    // that needed a crate from us would be a decompiler with a runtime
    // dependency, which is the thing this project does not have.
    let wasm = common::assemble("host-library", SAMPLE);
    let module = Module::parse(&wasm).expect("valid");
    let skeleton = codegen::generate_host(&module).expect("generates");
    assert!(skeleton.contains("pub mod runtime {"), "{skeleton}");
    assert!(skeleton.contains("pub struct Wasi"), "{skeleton}");
    assert!(
        !skeleton.contains("#[cfg(test)]"),
        "the library's own tests do not travel with it"
    );
    assert!(
        skeleton.contains("pub wasi: runtime::Wasi"),
        "and the host is wired to it:\n{skeleton}"
    );
}

#[test]
fn the_whole_mechanical_set_is_answered_with_the_real_signatures() {
    // One module importing everything the library claims to answer, at the
    // shapes an Emscripten toolchain actually emits. It is a list rather than
    // a story because the value is in the coverage: a table entry with a typo
    // in its signature silently stops matching, and nothing else would notice.
    let wasm = common::assemble(
        "host-mechanical",
        r#"(module
            (import "wasi_snapshot_preview1" "fd_read" (func (param i32 i32 i32 i32) (result i32)))
            (import "wasi_snapshot_preview1" "fd_pread" (func (param i32 i32 i32 i64 i32) (result i32)))
            (import "wasi_snapshot_preview1" "fd_seek" (func (param i32 i64 i32 i32) (result i32)))
            (import "wasi_snapshot_preview1" "fd_close" (func (param i32) (result i32)))
            (import "wasi_snapshot_preview1" "environ_sizes_get" (func (param i32 i32) (result i32)))
            (import "wasi_snapshot_preview1" "environ_get" (func (param i32 i32) (result i32)))
            (import "wasi_snapshot_preview1" "args_sizes_get" (func (param i32 i32) (result i32)))
            (import "wasi_snapshot_preview1" "args_get" (func (param i32 i32) (result i32)))
            (import "wasi_snapshot_preview1" "random_get" (func (param i32 i32) (result i32)))
            (import "wasi_snapshot_preview1" "clock_time_get" (func (param i32 i64 i32) (result i32)))
            (import "wasi_snapshot_preview1" "proc_exit" (func (param i32)))
            (import "env" "getentropy" (func (param i32 i32) (result i32)))
            (import "env" "exit" (func (param i32)))
            (import "env" "abort" (func))
            (import "env" "__assert_fail" (func (param i32 i32 i32 i32)))
            (import "env" "__cxa_begin_catch" (func (param i32) (result i32)))
            (import "env" "__cxa_end_catch" (func))
            (import "env" "__cxa_rethrow" (func))
            (import "env" "__resumeException" (func (param i32)))
            (import "env" "__cxa_uncaught_exceptions" (func (result i32)))
            (import "env" "__cxa_get_exception_ptr" (func (param i32) (result i32)))
            (import "env" "__cxa_find_matching_catch_2" (func (result i32)))
            (import "env" "__cxa_find_matching_catch_3" (func (param i32) (result i32)))
            (import "env" "__cxa_find_matching_catch_4" (func (param i32 i32) (result i32)))
            (import "env" "emscripten_get_heap_max" (func (result i32)))
            (import "env" "emscripten_date_now" (func (result f64)))
            (import "env" "emscripten_get_now" (func (result f64)))
            (import "env" "_emscripten_get_now_is_monotonic" (func (result i32)))
            (import "env" "emscripten_num_logical_cores" (func (result i32)))
            (import "env" "emscripten_console_error" (func (param i32)))
            (import "env" "emscripten_memcpy_js" (func (param i32 i32 i32)))
            (import "env" "emscripten_runtime_keepalive_push" (func))
            (import "env" "emscripten_runtime_keepalive_pop" (func))
            (import "env" "emscripten_async_call" (func (param i32 i32 i32)))
            (import "env" "emscripten_exit_with_live_runtime" (func))
            (import "env" "emscripten_check_blocking_allowed" (func))
            (import "env" "__syscall_openat" (func (param i32 i32 i32 i32) (result i32)))
            (import "env" "__syscall_unlinkat" (func (param i32 i32 i32) (result i32)))
            (import "env" "__syscall_ioctl" (func (param i32 i32 i32) (result i32)))
            (import "env" "__syscall_stat64" (func (param i32 i32) (result i32)))
            (import "env" "__syscall_lstat64" (func (param i32 i32) (result i32)))
            (import "env" "__syscall_fstat64" (func (param i32 i32) (result i32)))
            (import "env" "__syscall_newfstatat" (func (param i32 i32 i32 i32) (result i32)))
            (import "env" "_localtime_js" (func (param i64 i32)))
            (import "env" "_gmtime_js" (func (param i64 i32) (result i32)))
            (import "env" "_tzset_js" (func (param i32 i32 i32 i32)))
            (import "other" "_tzset_js" (func (param i32 i32 i32)))
            (import "env" "_emval_decref" (func (param i32)))
            (import "env" "_emval_take_value" (func (param i32 i32) (result i32)))
            (import "env" "on_frame" (func (param f32 f64)))
            (func (export "go")))"#,
    );
    let module = Module::parse(&wasm).expect("valid");
    let skeleton = codegen::generate_host(&module).expect("generates");
    let body = trait_body(&skeleton).to_string();

    // The application's own is left, and so are the four C++ catch-matching
    // imports: matching a throw against a catch clause is not mechanical, and
    // answering it with the exception in flight selects the wrong handler for
    // `throw Derived; catch (Base&)`.
    assert_eq!(body.matches("todo!(\"").count(), 5, "{body}");
    assert!(body.contains(r#"todo!("env::on_frame")"#), "{body}");
    assert!(
        body.contains(r#"todo!("env::__cxa_find_matching_catch_2")"#),
        "{body}"
    );
    assert!(
        body.contains(r#"todo!("env::__cxa_get_exception_ptr")"#),
        "{body}"
    );
    for expected in [
        "self.wasi.fd_read(caller, p0, p1, p2, p3)",
        "self.wasi.fd_pread(caller, p0, p1, p2, p3, p4)",
        "self.wasi.fd_seek(caller, p0, p1, p2, p3)",
        "self.wasi.fd_close(p0)",
        "self.wasi.environ_sizes_get(caller, p0, p1)",
        "self.wasi.args_get(caller, p0, p1)",
        "self.wasi.random_get(caller, p0, p1)",
        "self.wasi.clock_time_get(caller, p2)",
        "runtime::exit(p0)",
        "rt::trap(\"the module called abort\")",
        "runtime::assert_fail(caller, p0, p1, p2, p3)",
        "self.cxx.begin_catch(p0)",
        "self.cxx.end_catch()",
        "self.cxx.rethrow()",
        "self.cxx.uncaught()",
        "runtime::Emscripten::heap_max(caller)",
        "self.wasi.now_milliseconds",
        "self.emscripten.cores",
        "self.emscripten.console_error(caller, &mut self.wasi, p0)",
        "caller.memory.copy(p0, p1, p2)",
        "self.emscripten.keepalive += 1",
        "self.emscripten.keepalive -= 1",
        "self.emscripten.scheduled.push((p0, p1, p2))",
        "self.emscripten.exited_with_live_runtime = true",
        "self.wasi.openat_at(caller, p1, p2)",
        "self.wasi.stat_path(caller, p0, p1)",
        "self.wasi.stat_fd(caller, p0, p1)",
        "self.wasi.stat_path(caller, p1, p2)",
        "let _ = runtime::write_tm(caller, p1, p0);",
        "runtime::write_tm(caller, p1, p0)",
        "runtime::tzset(caller, p0, p1, p2, p3)",
        // The older three-argument spelling, which has no second name.
        "runtime::tzset(caller, p0, p1, p2, 0)",
        "self.wasi.unlinkat(caller, p1)",
        "-runtime::errno::NOTTY",
        "self.embind.decref(p0)",
        "self.embind.take_value()",
    ] {
        assert!(body.contains(expected), "missing `{expected}`:\n{body}");
    }
}

#[test]
fn the_skeleton_does_not_warn_about_the_arguments_it_does_not_use() {
    // A `todo!()` body uses none of its arguments. Compiling the VoIP module's
    // host produced 162 warnings, which buries the output a reader needs — and
    // the names have to stay, because they are what the body gets filled in
    // with.
    let wasm = common::assemble("host-warnings", SAMPLE);
    let module = Module::parse(&wasm).expect("valid");
    let skeleton = codegen::generate_host(&module).expect("generates");
    assert!(
        skeleton.contains("#[allow(unused_variables)]\nimpl Imports for Host"),
        "{skeleton}"
    );
}

#[test]
fn defaults_answer_what_nothing_can_and_say_that_they_did() {
    // A stub returning zero is a hypothesis. This does not make it safe — it
    // makes it visible: the run comes with the list of what it rested on.
    let wasm = common::assemble("host-defaults", SAMPLE);
    let module = Module::parse(&wasm).expect("valid");

    let strict = codegen::generate_host(&module).expect("generates");
    assert!(
        strict.contains(r#"todo!("env::on_call_event")"#),
        "{strict}"
    );

    let lenient = codegen::generate_host_with(&module, true).expect("generates");
    let body = trait_body(&lenient).to_string();
    assert!(!body.contains("todo!(\""), "{body}");
    assert!(
        body.contains(
            r#"self.unanswered.record("env::on_call_event", &[i64::from(p0), i64::from(p1)]);"#
        ),
        "the name and the arguments it was given:\n{body}"
    );
    // One that returns something answers with the zero of its type, after
    // recording — not instead of it.
    assert!(
        body.contains(r#"record("env::_embind_register_class""#),
        "{body}"
    );
    assert!(
        lenient.contains("pub unanswered: runtime::Unanswered"),
        "and the host has somewhere to keep it:\n{lenient}"
    );
}

#[test]
fn a_threaded_module_records_its_defaults_through_the_lock() {
    // The record is shared like the rest of a threaded host's state: a thread
    // that fell back to a default has to tell the thread that reads the list.
    // Threaded means the module creates threads — a shared memory alone does
    // not, and asking every host of one to be `Clone + Send` would be a
    // constraint nothing needs.
    let wasm = common::assemble(
        "host-defaults-threaded",
        r#"(module
            (import "env" "__pthread_create_js"
                (func (param i32 i32 i32 i32) (result i32)))
            (import "env" "memory" (memory 1 4 shared))
            (import "env" "callback" (func (param i32)))
            (global $sp (export "__stack_pointer") (mut i32) (i32.const 65536))
            (type $entry (func (param i32) (result i32)))
            (func (export "_emscripten_thread_init") (param i32 i32 i32 i32 i32 i32))
            (func (type $entry) local.get 0)
            (func (export "go") i32.const 1 call 1)
            (table 1 funcref)
            (elem (i32.const 0) func 3))"#,
    );
    let module = Module::parse(&wasm).expect("valid");
    let host = codegen::generate_host_with(&module, true).expect("generates");
    assert!(
        host.contains(
            r#"self.unanswered.lock().unwrap_or_else(|held| held.into_inner()).record("env::callback""#
        ),
        "{host}"
    );
    assert!(
        host.contains("pub unanswered: std::sync::Arc<std::sync::Mutex<runtime::Unanswered>>"),
        "{host}"
    );
    // Locked once. The locked form contains the text being replaced, so a
    // second pass over it produced `…into_inner()).lock()…`, which is not a
    // thing — and only showed up fifteen minutes into compiling a 9 MiB
    // module.
    assert_eq!(host.matches("into_inner()).lock()").count(), 0, "{host}");
}

#[test]
fn a_float_argument_is_recorded_as_its_bits() {
    // The record is a list of i64 and a cast would round: what a reader wants
    // back is the argument the guest passed.
    let wasm = common::assemble(
        "host-defaults-float",
        r#"(module
            (import "env" "render" (func (param i32 f64 f32 i64)))
            (func (export "go") i32.const 1 f64.const 2 f32.const 3 i64.const 4 call 0))"#,
    );
    let module = Module::parse(&wasm).expect("valid");
    let host = codegen::generate_host_with(&module, true).expect("generates");
    assert!(
        host.contains(
            r#"record("env::render", &[i64::from(p0), p1.to_bits() as i64, i64::from(p2.to_bits()), p3]);"#
        ),
        "{host}"
    );
}

#[test]
fn a_threaded_body_that_takes_a_reference_locks_once() {
    // `console_error` takes `&mut self.wasi` as well as reading another field,
    // which is the body that exposed the double lock.
    let wasm = common::assemble(
        "host-defaults-console",
        r#"(module
            (import "env" "__pthread_create_js"
                (func (param i32 i32 i32 i32) (result i32)))
            (import "env" "memory" (memory 1 4 shared))
            (import "env" "emscripten_console_error" (func (param i32)))
            (global $sp (export "__stack_pointer") (mut i32) (i32.const 65536))
            (type $entry (func (param i32) (result i32)))
            (func (export "_emscripten_thread_init") (param i32 i32 i32 i32 i32 i32))
            (func (type $entry) local.get 0)
            (func (export "go") i32.const 1 call 1)
            (table 1 funcref)
            (elem (i32.const 0) func 3))"#,
    );
    let module = Module::parse(&wasm).expect("valid");
    let host = codegen::generate_host(&module).expect("generates");
    assert!(
        host.contains(
            "self.emscripten.lock().unwrap_or_else(|held| held.into_inner()).console_error(caller, &mut *self.wasi.lock().unwrap_or_else(|held| held.into_inner()), p0)"
        ),
        "{host}"
    );
    assert_eq!(host.matches("into_inner()).lock()").count(), 0, "{host}");
}

#[test]
fn a_run_on_defaults_reports_what_it_rested_on() {
    let wasm = common::assemble(
        "host-defaults-run",
        r#"(module
            (import "env" "ask" (func $ask (param i32) (result i32)))
            (import "env" "tell" (func $tell (param i32 i32)))
            (memory (export "memory") 1)
            (func (export "go") (result i32)
                i32.const 7 i32.const 8 call $tell
                i32.const 9 i32.const 10 call $tell
                i32.const 3 call $ask))"#,
    );
    let module = Module::parse(&wasm).expect("valid");
    let generated = codegen::generate(&module).expect("generates");
    let host = codegen::generate_host_with(&module, true).expect("generates");

    const DRIVER: &str = r#"
mod generated;
mod host;
fn main() {
    let mut instance = generated::Instance::with_host(host::Host::default());
    println!("go returned {}", instance.go());
    print!("{}", instance.host.unanswered.report());
}
"#;
    let output = common::run_with_host("host-defaults-run", &generated, &host, DRIVER);
    let lines: Vec<&str> = output.trim().lines().collect();
    assert_eq!(
        lines[0], "go returned 0",
        "the default is the zero of the type, and it is a hypothesis"
    );
    assert_eq!(
        lines[1],
        "2 imports were answered with a default rather than an answer:"
    );
    // In the order they were reached, with the arguments of the first call —
    // which is usually the story.
    assert_eq!(lines[2], "  env::tell  2x, first (7, 8)");
    assert_eq!(lines[3], "  env::ask  1x, first (3)");
}
