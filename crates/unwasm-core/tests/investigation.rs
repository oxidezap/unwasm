//! The parts that exist for reading a module rather than translating it.
//!
//! Decompiling a 9 MiB module gives 365 MB of Rust, and looking at three
//! functions in it should not. These are the affordances that came out of
//! actually doing that: decompile a few functions, find them by index, see
//! which table slot reaches a function, and notice when the same address turns
//! up in fifty places.

mod common;

use unwasm_core::{Module, analysis, codegen};

/// Deliberately without `$names`: those become a name section, and the point
/// here is a module that has none — which is every shipped one.
const SAMPLE: &str = r#"(module
    (import "env" "log" (func (param i32)))
    (memory (export "memory") 1)
    (data (i32.const 100) "first_function: here\00")
    (data (i32.const 200) "second_function: there\00")
    (type $unary (func (param i32) (result i32)))
    (func (export "first") (param i32) (result i32)
        i32.const 100 call 0
        local.get 0 i32.const 2 i32.mul)
    (func (export "second") (param i32) (result i32)
        i32.const 200 call 0
        local.get 0 i32.const 3 i32.mul)
    (func (export "third") (param i32) (result i32)
        local.get 0)
    (table 4 funcref)
    (elem (i32.const 2) func 2 3))"#;

#[test]
fn only_the_functions_asked_for_are_decompiled() {
    let wasm = common::assemble("only", SAMPLE);
    let module = Module::parse(&wasm).expect("valid");
    // Function 2 is `second`, which multiplies by three. (Function 0 is the
    // import, so the defined ones start at 1.)
    let only = [2u32].into_iter().collect();
    let files = codegen::generate_only(&module, codegen::Layout::Single, &only).expect("generates");
    let code = &files[0].contents;

    assert!(
        code.contains("wrapping_mul(3i32)"),
        "the one asked for is there in full:\n{code}"
    );
    assert!(
        !code.contains("wrapping_mul(2i32)"),
        "and the one that was not asked for is not"
    );
    // The others keep their signature and say they were left out.
    assert!(
        code.contains(r#"unimplemented!("function #1 was not decompiled: --only")"#),
        "{code}"
    );
    assert!(
        code.contains("fn f1_first_function"),
        "a function left out keeps its name, so the index still finds it"
    );
}

#[test]
fn what_was_left_out_still_compiles() {
    // The point of stubbing rather than omitting: callers still resolve, so the
    // result builds and can be read in an editor that understands it.
    let wasm = common::assemble("only-compiles", SAMPLE);
    let module = Module::parse(&wasm).expect("valid");
    let only = [1u32].into_iter().collect();
    let files = codegen::generate_only(&module, codegen::Layout::Single, &only).expect("generates");

    const DRIVER: &str = r#"
mod generated;
struct Host;
impl generated::Imports for Host {
    fn env_log(&mut self, _caller: &mut generated::rt::Caller<'_>, _p0: i32) {}
}
fn main() {
    let mut instance = generated::Instance::with_host(Host);
    println!("{}", instance.first(21));
}
"#;
    let output = common::run_with_generated("only-compiles", &files[0].contents, DRIVER);
    assert_eq!(output.trim(), "42");
}

#[test]
fn the_index_says_where_every_function_ended_up() {
    let wasm = common::assemble("index", SAMPLE);
    let module = Module::parse(&wasm).expect("valid");
    let files = codegen::generate_files(
        &module,
        codegen::Layout::Split {
            lines_per_file: 1, // one function per file, so the mapping is visible
        },
    )
    .expect("generates");

    let index = files
        .iter()
        .find(|file| file.name == "names.json")
        .expect("the index is written");
    let json = &index.contents;

    // Every function is in it, with where it is and how it got its name.
    assert_eq!(json.matches("\"index\":").count(), module.funcs.len());
    assert!(json.contains(r#""name": "f1_first_function""#), "{json}");
    assert!(json.contains(r#""named_by": "message""#));
    assert!(json.contains(r#""file": "part0.rs""#));
    // The table slots are in it too — that is the lookup that is otherwise
    // impossible from a call site.
    assert!(json.contains(r#""table_slots": [2]"#), "{json}");
}

#[test]
fn the_index_is_json_a_parser_will_accept() {
    let wasm = common::assemble("index-json", SAMPLE);
    let module = Module::parse(&wasm).expect("valid");
    let files = codegen::generate_files(&module, codegen::Layout::Single).expect("generates");
    let json = &files
        .iter()
        .find(|file| file.name == "names.json")
        .expect("present")
        .contents;

    // No trailing comma before the closing bracket, and the braces balance.
    assert!(!json.contains(",\n  ]"), "trailing comma:\n{json}");
    assert_eq!(json.matches('{').count(), json.matches('}').count());
    assert_eq!(json.matches('[').count(), json.matches(']').count());
    // Parsed by something that is not us.
    common::assert_valid_json(json);
}

#[test]
fn a_function_says_which_table_slot_reaches_it() {
    let wasm = common::assemble("slots", SAMPLE);
    let code = common::decompile(&wasm);
    assert!(
        code.contains("In the function table at slot 2"),
        "a call_indirect takes a table index, and this is what says which one:\n{code}"
    );
    assert!(code.contains("In the function table at slot 3"));
}

#[test]
fn the_table_records_what_each_slot_holds() {
    let wasm = common::assemble("table-map", SAMPLE);
    let module = Module::parse(&wasm).expect("valid");
    let table = analysis::analyse(&module).table;
    assert_eq!(table.len(), 2);
    assert_eq!(table[&2], 2, "slot 2 holds function 2");
    assert_eq!(table[&3], 3);
    assert!(!table.contains_key(&0), "nothing was put in slot 0");
}

#[test]
fn an_address_many_functions_share_is_pointed_out() {
    // A context pointer looks like a bare number in every function that takes
    // it, and reads as noise until you notice it is the same number.
    let mut wat = String::from(
        "(module (memory (export \"memory\") 1) (data (i32.const 4096) \"\\00\\01\\02\\03\")",
    );
    for index in 0..12 {
        wat.push_str(&format!(
            " (func (export \"f{index}\") (result i32) i32.const 4098 i32.load8_u)"
        ));
    }
    wat.push(')');
    let wasm = common::assemble("hot-address", &wat);
    let module = Module::parse(&wasm).expect("valid");
    let hot = analysis::analyse(&module).hot_addresses;
    assert_eq!(hot.get(&4098), Some(&12), "{hot:?}");

    let code = common::decompile(&wasm);
    assert!(
        code.contains("4098i32 /* address, in 12 functions */"),
        "{code}"
    );
    assert!(
        code.contains("Addresses shared across the module"),
        "and collected at the top:\n{code}"
    );
}

#[test]
fn an_address_only_a_few_functions_share_is_left_alone() {
    let mut wat = String::from("(module (memory 1) (data (i32.const 4096) \"\\00\\01\\02\\03\")");
    for index in 0..3 {
        wat.push_str(&format!(
            " (func (export \"f{index}\") (result i32) i32.const 4098 i32.load8_u)"
        ));
    }
    wat.push(')');
    let wasm = common::assemble("cold-address", &wat);
    let module = Module::parse(&wasm).expect("valid");
    assert!(analysis::analyse(&module).hot_addresses.is_empty());
}

#[test]
fn arithmetic_that_is_not_an_address_is_not_called_one() {
    // `2147483647` and `4096` came out as the most widely shared "addresses"
    // before this was decided by the module's own data layout.
    let mut wat = String::from("(module (memory 1) (data (i32.const 1024) \"xy\")");
    for index in 0..12 {
        wat.push_str(&format!(
            " (func (export \"f{index}\") (result i32) i32.const 2147483647 i32.const 1 i32.add)"
        ));
    }
    wat.push(')');
    let wasm = common::assemble("not-an-address", &wat);
    let module = Module::parse(&wasm).expect("valid");
    let hot = analysis::analyse(&module).hot_addresses;
    assert!(
        hot.is_empty(),
        "outside the span the data segments occupy: {hot:?}"
    );
}

// ---- cross-reference ----

/// A module with a call chain, an indirect call, and an unreachable function.
const CALLERS: &str = r#"(module
    (import "env" "host" (func (param i32)))
    (memory (export "memory") 1)
    (type $unary (func (param i32) (result i32)))
    (func (export "entry") (param i32) (result i32)
        local.get 0
        call 2)
    (func (param i32) (result i32)
        local.get 0
        call 3
        i32.const 0
        call 0
        local.get 0
        i32.const 0
        call_indirect (type $unary))
    (func (param i32) (result i32) local.get 0 i32.const 2 i32.mul)
    (func (param i32) (result i32) local.get 0)
    (table 2 funcref)
    (elem (i32.const 0) func 4))"#;

#[test]
fn the_call_graph_runs_in_both_directions() {
    let wasm = common::assemble("xref", CALLERS);
    let module = Module::parse(&wasm).expect("valid");
    let graph = analysis::analyse(&module).call_graph;

    // f1 (entry) calls f2; f2 calls f3 and the import.
    assert!(graph.calls_from(1).contains(&2));
    assert!(graph.calls_from(2).contains(&3));
    assert!(graph.calls_from(2).contains(&0), "the import counts too");
    // And upwards, which is the direction the module does not record.
    assert!(graph.callers_of(2).contains(&1));
    assert_eq!(graph.callers_of(1).len(), 0, "entry is called by nobody");
    // An indirect call names a type, not a target.
    assert!(graph.calls_indirectly[&2].contains(&0));
    assert!(
        !graph.calls_from(2).contains(&4),
        "what the table holds is not a direct call"
    );
}

#[test]
fn a_function_says_who_calls_it_and_what_it_calls() {
    let wasm = common::assemble("xref-output", CALLERS);
    let code = common::decompile(&wasm);
    assert!(code.contains("Called by 1: `f1`"), "{code}");
    assert!(code.contains("Calls 2: `f0`, `f3`"), "{code}");
    assert!(
        code.contains("through the table: (i32) -> (i32)"),
        "an indirect call is not a direct one, and says so:\n{code}"
    );
}

#[test]
fn a_function_nothing_reaches_says_which_kind_of_nothing() {
    let wasm = common::assemble("xref-unreached", CALLERS);
    let code = common::decompile(&wasm);
    // Exported: reachable from outside.
    assert!(code.contains("No direct callers: it is exported"), "{code}");
    // In the table: reachable indirectly.
    assert!(
        code.contains("No direct callers: the table reaches it"),
        "{code}"
    );
}

#[test]
fn a_function_that_truly_nothing_reaches_is_named_as_such() {
    let wasm = common::assemble(
        "xref-dead",
        r#"(module
            (func (export "used") (result i32) i32.const 1)
            (func (result i32) i32.const 2))"#,
    );
    let code = common::decompile(&wasm);
    assert!(
        code.contains("an entry point, or dead"),
        "not exported, not in the table, not called:\n{code}"
    );
}

#[test]
fn the_index_carries_the_call_graph() {
    let wasm = common::assemble("xref-index", CALLERS);
    let module = Module::parse(&wasm).expect("valid");
    let files = codegen::generate_files(&module, codegen::Layout::Single).expect("generates");
    let json = &files
        .iter()
        .find(|file| file.name == "names.json")
        .expect("present")
        .contents;
    assert!(json.contains(r#""calls": [2]"#), "{json}");
    assert!(json.contains(r#""called_by": [1]"#), "{json}");
    common::assert_valid_json(json);
}
