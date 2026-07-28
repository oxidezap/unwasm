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

use unwasm_core::{Module, codegen};

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
        "fn wasi_snapshot_preview1_fd_write(&mut self, p0: i32, p1: i32, p2: i32, p3: i32) -> i32",
        "fn env___cxa_throw(&mut self, p0: i32, p1: i32, p2: i32)",
        "fn env_emscripten_resize_heap(&mut self, p0: i32) -> i32",
        "fn env__embind_register_class(&mut self, p0: i32, p1: i32)",
        "fn env__emval_incref(&mut self, p0: i32)",
        "fn env_on_call_event(&mut self, p0: i32, p1: i32)",
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

#[test]
fn every_body_is_a_todo_naming_its_import() {
    let wasm = common::assemble("host-todos", SAMPLE);
    let module = Module::parse(&wasm).expect("valid");
    let skeleton = codegen::generate_host(&module).expect("generates");

    let methods = skeleton.matches("    fn ").count();
    assert_eq!(methods, 6);
    assert_eq!(
        // With the quote: the header comment mentions `todo!()` too.
        skeleton.matches("todo!(\"").count(),
        methods,
        "a method without a todo is a method that silently does nothing"
    );
    assert!(skeleton.contains(r#"todo!("wasi_snapshot_preview1::fd_write")"#));
    assert!(
        !skeleton.contains("0i32"),
        "nothing here may return a plausible value"
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
        "fn env_add_ten(&mut self, p0: i32) -> i32 {\n        todo!(\"env::add_ten\")",
        "fn env_add_ten(&mut self, p0: i32) -> i32 {\n        return p0 + 10;",
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

#[test]
fn a_module_that_needs_no_host_gets_an_empty_skeleton() {
    let wasm = common::assemble(
        "host-none",
        "(module (func (export \"f\") (result i32) i32.const 1))",
    );
    let module = Module::parse(&wasm).expect("valid");
    let skeleton = codegen::generate_host(&module).expect("generates");
    assert!(skeleton.contains("impl Imports for Host"));
    assert_eq!(skeleton.matches("    fn ").count(), 0);
    assert!(skeleton.contains("0 methods"));
}

#[test]
#[ignore = "reads the capture directory"]
fn the_voip_module_leaves_a_hundred_and_two_methods() {
    let Some(bytes) = common::captured("D5pLH9sfOOl") else {
        panic!("D5pLH9sfOOl is not available; set WA_WASM_DIR");
    };
    let module = Module::parse(&bytes).expect("parses");
    let skeleton = codegen::generate_host(&module).expect("generates");
    let methods = skeleton.matches("    fn ").count();
    assert_eq!(methods, 102, "227 imports, 125 of them trampolines");

    // The forty that only the application can answer are the ones that matter,
    // and they are the last group.
    let application = &skeleton[skeleton
        .find("---- The application")
        .expect("the module has callbacks of its own")..];
    let their_methods = application.matches("    fn ").count();
    eprintln!("D5pLH9sfOOl: {methods} methods for a host, {their_methods} of them WhatsApp's own");
    assert!(their_methods >= 30);
}
