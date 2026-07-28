//! Names guessed from the messages a function logs.
//!
//! A stripped module has no name section and no mangled symbols. What it does
//! have is its own log messages — `parse_xmpp_offer: invalid call-creator jid`
//! — and a function that references one is very probably that function.
//!
//! Everything here is about the "very probably". A guess with no evidence is
//! worse than no guess, so the index stays in the emitted name, the message
//! travels in the doc comment, and a message shared by two functions names
//! neither of them.

mod common;

use unwasm_core::{Module, analysis, codegen};

fn analyse(name: &str, wat: &str) -> (Module, analysis::Analysis) {
    let wasm = common::assemble(name, wat);
    let module = Module::parse(&wasm).expect("valid");
    let analysis = analysis::analyse(&module);
    (module, analysis)
}

#[test]
fn a_function_is_named_after_the_message_only_it_logs() {
    let (_, analysis) = analyse(
        "named",
        r#"(module
            (import "env" "log" (func $log (param i32)))
            (memory 1)
            (data (i32.const 100) "parse_xmpp_offer: invalid call-creator jid\00")
            (data (i32.const 200) "fill_relay_info failed to copy uuid\00")
            (func (export "a") i32.const 100 call $log)
            (func (export "b") i32.const 200 call $log))"#,
    );
    let first = analysis.derived_names.get(&1).expect("named");
    assert_eq!(first.name, "parse_xmpp_offer");
    assert!(first.evidence.contains("invalid call-creator"));
    assert_eq!(analysis.derived_names[&2].name, "fill_relay_info");
}

#[test]
fn a_message_two_functions_log_names_neither() {
    // The rule that makes the guess worth making: a shared message
    // distinguishes nothing. `index out of bounds` is in most of a Rust module.
    let (_, analysis) = analyse(
        "shared-message",
        r#"(module
            (import "env" "log" (func $log (param i32)))
            (memory 1)
            (data (i32.const 100) "shared_helper: something went wrong\00")
            (func (export "a") i32.const 100 call $log)
            (func (export "b") i32.const 100 call $log))"#,
    );
    assert!(
        analysis.derived_names.is_empty(),
        "{:?}",
        analysis.derived_names
    );
}

#[test]
fn a_message_that_does_not_start_with_an_identifier_names_nothing() {
    let (_, analysis) = analyse(
        "not-identifiers",
        r#"(module
            (import "env" "log" (func $log (param i32)))
            (memory 1)
            (data (i32.const 100) "error while parsing the thing\00")
            (data (i32.const 200) "Something Went Wrong: badly\00")
            (data (i32.const 300) "short: x\00")
            (data (i32.const 400) "%s: %d failed\00")
            (func (export "a") i32.const 100 call $log)
            (func (export "b") i32.const 200 call $log)
            (func (export "c") i32.const 300 call $log)
            (func (export "d") i32.const 400 call $log))"#,
    );
    // `error` has no underscore, `Something` is capitalised, `short` is too
    // short to be a name rather than a word, and `%s` is not an identifier.
    assert!(
        analysis.derived_names.is_empty(),
        "{:?}",
        analysis.derived_names
    );
}

#[test]
fn a_function_the_module_already_names_keeps_its_name() {
    let (module, analysis) = analyse(
        "already-named",
        r#"(module
            (import "env" "log" (func $log (param i32)))
            (memory 1)
            (data (i32.const 100) "some_other_function: complained\00")
            (func $the_real_name (export "a") i32.const 100 call $log))"#,
    );
    assert_eq!(module.func_name(1), Some("the_real_name"));
    assert!(
        analysis.derived_names.is_empty(),
        "a guess must not override what the module says"
    );
}

#[test]
fn the_name_keeps_the_index_and_the_evidence_reaches_the_output() {
    let wasm = common::assemble(
        "named-output",
        r#"(module
            (import "env" "log" (func $log (param i32)))
            (memory (export "memory") 1)
            (data (i32.const 100) "handle_incoming_offer: no participants\00")
            (func (export "entry") i32.const 100 call $log))"#,
    );
    let code = common::decompile(&wasm);
    assert!(
        code.contains("fn f1_handle_incoming_offer("),
        "the index leads and the guess follows:\n{code}"
    );
    assert!(code.contains("so it is probably `handle_incoming_offer`"));
    assert!(
        code.contains("a message no other function"),
        "the evidence has to say why it is evidence"
    );
    // The export still reaches it.
    assert!(code.contains("self.f1_handle_incoming_offer()"));
}

/// Naming is a comment and an identifier. It must not change behaviour.
#[test]
fn naming_a_function_changes_nothing_about_what_it_does() {
    let wasm = common::assemble(
        "named-behaviour",
        r#"(module
            (memory (export "memory") 1)
            (data (i32.const 100) "compute_checksum: overflow\00")
            (func (export "compute") (param i32) (result i32)
                i32.const 100
                i32.load8_u
                local.get 0
                i32.add))"#,
    );
    let code = common::decompile(&wasm);
    assert!(code.contains("_compute_checksum"), "{code}");
    common::assert_agrees(
        "named-behaviour",
        &wasm,
        &[
            common::call("compute", &[common::Arg::I32(0)]),
            common::call("compute", &[common::Arg::I32(-1)]),
        ],
    );
}

// ---- placements: the reason any of this works on a threaded module ----

#[test]
fn a_passive_segment_placed_at_a_constant_address_becomes_readable() {
    // What a threaded module does: the segments carry no address, and the main
    // thread copies them into place with `memory.init`. Until the placement is
    // resolved, every string in such a module is unreachable.
    let wasm = common::assemble(
        "placed",
        r#"(module
            (import "env" "log" (func $log (param i32)))
            (memory (export "memory") 1)
            (data $messages "resolve_placement: it worked\00")
            (func (export "init_memory")
                i32.const 4096
                i32.const 0
                i32.const 29
                memory.init $messages)
            (func (export "use_it") i32.const 4096 call $log))"#,
    );
    let module = Module::parse(&wasm).expect("valid");
    let analysis = analysis::analyse(&module);
    assert_eq!(analysis.placements.len(), 1);
    assert_eq!(analysis.placements[&0].address, 4096);

    let code = codegen::generate(&module).expect("generates");
    assert!(
        code.contains(r#"4096i32 /* "resolve_placement: it worked" */"#),
        "{code}"
    );
    assert_eq!(analysis.derived_names[&2].name, "resolve_placement");
}

#[test]
fn a_segment_placed_at_a_computed_address_is_not_claimed_to_be_anywhere() {
    // Per-thread storage is placed at `base + offset`, where base is not known
    // until it runs. Recording one of the addresses would make every string in
    // that segment resolve to the wrong text.
    let wasm = common::assemble(
        "computed-placement",
        r#"(module
            (memory (export "memory") 1)
            (global $base (mut i32) (i32.const 2048))
            (data $messages "thread_local_thing: here\00")
            (func (export "init_memory")
                global.get $base
                i32.const 32
                i32.add
                i32.const 0
                i32.const 25
                memory.init $messages))"#,
    );
    let module = Module::parse(&wasm).expect("valid");
    assert!(analysis::analyse(&module).placements.is_empty());
}

#[test]
fn a_segment_placed_twice_at_different_addresses_is_dropped() {
    // Both placements are true, so neither can be used to read an address back
    // to text.
    let wasm = common::assemble(
        "double-placement",
        r#"(module
            (memory (export "memory") 1)
            (data $messages "somewhere_or_other: text\00")
            (func (export "init_memory")
                i32.const 1024
                i32.const 0
                i32.const 25
                memory.init $messages
                i32.const 2048
                i32.const 0
                i32.const 25
                memory.init $messages))"#,
    );
    let module = Module::parse(&wasm).expect("valid");
    assert!(analysis::analyse(&module).placements.is_empty());
}

#[test]
fn placing_the_same_segment_twice_at_one_address_is_not_ambiguous() {
    let wasm = common::assemble(
        "same-placement",
        r#"(module
            (memory (export "memory") 1)
            (data $messages "repeatable_placement: text\00")
            (func (export "init_memory")
                i32.const 1024
                i32.const 0
                i32.const 27
                memory.init $messages
                i32.const 1024
                i32.const 0
                i32.const 27
                memory.init $messages))"#,
    );
    let module = Module::parse(&wasm).expect("valid");
    let analysis = analysis::analyse(&module);
    assert_eq!(analysis.placements[&0].address, 1024);
}

/// The module this was built for.
#[test]
#[ignore = "reads the capture directory"]
fn the_voip_module_names_a_fifth_of_its_functions() {
    let Some(bytes) = common::captured("D5pLH9sfOOl") else {
        panic!("D5pLH9sfOOl is not available; set WA_WASM_DIR");
    };
    let module = Module::parse(&bytes).expect("parses");
    let analysis = analysis::analyse(&module);

    assert_eq!(
        analysis.placements.len(),
        module.datas.len(),
        "every segment in this module is passive and placed by memory.init"
    );
    assert!(
        analysis.derived_names.len() > 2000,
        "expected thousands of names, got {}",
        analysis.derived_names.len()
    );
    // Names that were checked by hand against the module's log messages.
    let names: Vec<&str> = analysis
        .derived_names
        .values()
        .map(|derived| derived.name.as_str())
        .collect();
    for expected in [
        "fill_user_info_from_participant",
        "fill_relay_info",
        "wa_call_string_get_size",
    ] {
        assert!(names.contains(&expected), "{expected} was not derived");
    }
    eprintln!(
        "D5pLH9sfOOl: {} of {} functions named from their own messages",
        analysis.derived_names.len(),
        module.funcs.len()
    );
}
