//! Recognising library code by the shape of its body.
//!
//! A shipped module has no name section, but the library halves of it were
//! built from the same sources as a module you can build yourself with names
//! kept. A fingerprint is what survives that difference: the opcode sequence
//! with the indices, offsets and constants — the things that move between
//! builds — left out.
//!
//! What that buys is measured, and it is narrow. See the doc comment on
//! `analysis::fingerprint`: ~91% across builds of the same toolchain, and a
//! handful across different emscripten versions.

mod common;

use unwasm_core::{Module, analysis, codegen};

/// A body long enough to be worth fingerprinting, parameterised by the operator
/// so two of them can be made to differ in shape rather than in constants.
fn body(operator: &str) -> String {
    let mut text = String::new();
    for _ in 0..8 {
        text.push_str(&format!(" local.get 0 i32.const 1 {operator} local.set 0"));
    }
    text.push_str(" local.get 0");
    text
}

fn reference() -> Vec<u8> {
    let wat = format!(
        "(module (func $wibble_copy (export \"copy\") (param i32) (result i32){})\
         (func $wibble_fill (export \"fill\") (param i32) (result i32){}))",
        body("i32.add"),
        body("i32.mul")
    );
    common::assemble("signatures-reference", &wat)
}

/// The same two functions, from a module that kept no names.
fn stripped() -> Vec<u8> {
    let wat = format!(
        "(module (func (export \"a\") (param i32) (result i32){})\
         (func (export \"b\") (param i32) (result i32){}))",
        body("i32.add"),
        body("i32.mul")
    );
    common::assemble("signatures-stripped", &wat)
}

fn catalogue() -> codegen::Signatures {
    let module = Module::parse(&reference()).expect("valid");
    codegen::extract_signatures(&module)
}

#[test]
fn a_catalogue_holds_one_entry_per_named_function() {
    let catalogue = catalogue();
    assert_eq!(catalogue.len(), 2, "{catalogue:?}");
    assert!(catalogue.values().any(|name| name == "wibble_copy"));
}

#[test]
fn a_module_without_names_contributes_nothing() {
    let module = Module::parse(&stripped()).expect("valid");
    assert!(codegen::extract_signatures(&module).is_empty());
}

#[test]
fn an_unnamed_function_takes_the_name_of_what_it_fingerprints_as() {
    let module = Module::parse(&stripped()).expect("valid");
    let files = codegen::generate_with_signatures(&module, codegen::Layout::Single, &catalogue())
        .expect("generates");
    let code = &files[0].contents;

    assert!(code.contains("fn f0_wibble_copy"), "{code}");
    assert!(code.contains("fn f1_wibble_fill"), "{code}");
    // And says where the name is from, because a catalogue is only as good as
    // the build it came from.
    assert!(
        code.contains("recognised as `wibble_copy`"),
        "the doc comment names its source:\n{code}"
    );
    assert!(code.contains("fingerprints"), "{code}");
}

#[test]
fn a_recognised_name_reaches_every_place_the_function_is_mentioned() {
    // The exported wrapper and the call site have to agree with the definition,
    // or the output stops compiling — which is the property everything else
    // here rests on.
    let wat = format!(
        "(module (func (export \"a\") (param i32) (result i32){})\
         (func (export \"caller\") (param i32) (result i32) local.get 0 call 0))",
        body("i32.add")
    );
    let wasm = common::assemble("signatures-callsite", &wat);
    let module = Module::parse(&wasm).expect("valid");
    let files = codegen::generate_with_signatures(&module, codegen::Layout::Single, &catalogue())
        .expect("generates");
    let code = &files[0].contents;
    assert!(code.contains("self.f0_wibble_copy("), "{code}");

    const DRIVER: &str = r#"
mod generated;
fn main() {
    let mut instance = generated::Instance::new();
    println!("{}", instance.caller(0));
}
"#;
    let output = common::run_with_generated("signatures-callsite", code, DRIVER);
    assert_eq!(output.trim(), "8", "and it still runs");
}

/// Leaving a recognised body out is what makes a catalogue worth having on a
/// 9 MiB module: the reader wants the application, and the library halves are
/// already readable under their own names somewhere else.
///
/// What must survive is everything a *reader* uses — the name, the signature,
/// the call sites, the index — so the result still compiles and still says what
/// calls what. What goes is the body, and the stub says why.
#[test]
fn a_recognised_body_can_be_left_out_and_the_rest_still_compiles() {
    let wat = format!(
        "(module (func (export \"a\") (param i32) (result i32){})\
         (func (export \"caller\") (param i32) (result i32) local.get 0 call 0))",
        body("i32.add")
    );
    let wasm = common::assemble("signatures-stubbed", &wat);
    let module = Module::parse(&wasm).expect("valid");

    // What the catalogue names, asked before the output rather than counted
    // out of it.
    let recognised = codegen::recognised_functions(&module, &catalogue());
    assert_eq!(recognised.len(), 1, "{recognised:?}");
    assert_eq!(recognised[&0], "wibble_copy");

    let files = codegen::generate_options(
        &module,
        &codegen::Options {
            layout: codegen::Layout::Single,
            signatures: catalogue(),
            stub_recognised: true,
            ..codegen::Options::default()
        },
    )
    .expect("generates");
    let code = &files[0].contents;

    // The name, the signature and the call site are all still there.
    assert!(
        code.contains("fn f0_wibble_copy(&mut self, p0: i32) -> i32"),
        "{code}"
    );
    assert!(code.contains("self.f0_wibble_copy("), "{code}");
    assert!(
        code.contains(
            "unimplemented!(\"function #0 is `wibble_copy`, left out by --stub-recognised\")"
        ),
        "the stub says which function and why:\n{code}"
    );
    // And the body is gone rather than merely renamed.
    assert!(!code.contains("wrapping_add(1i32)"), "{code}");
    // The one that was not recognised keeps its body.
    assert!(
        code.contains("wrapping_add(1i32)") || code.contains("fn f1"),
        "{code}"
    );

    // It still compiles, and the part that was kept still runs.
    const DRIVER: &str = r#"
mod generated;
fn main() {
    std::panic::set_hook(Box::new(|_| {}));
    let mut instance = generated::Instance::new();
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| instance.caller(0)));
    println!("{}", if outcome.is_err() { "stopped at the stub" } else { "ran" });
}
"#;
    let output = common::run_with_generated("signatures-stubbed", code, DRIVER);
    assert_eq!(
        output.trim(),
        "stopped at the stub",
        "a caller of a stub stops there, loudly"
    );
}

#[test]
fn nothing_is_left_out_without_a_catalogue_to_recognise_it() {
    let module = Module::parse(&stripped()).expect("valid");
    assert!(codegen::recognised_functions(&module, &codegen::Signatures::new()).is_empty());
    let files = codegen::generate_options(
        &module,
        &codegen::Options {
            layout: codegen::Layout::Single,
            stub_recognised: true,
            ..codegen::Options::default()
        },
    )
    .expect("generates");
    assert!(
        !files[0].contents.contains("--stub-recognised"),
        "with no catalogue there is nothing to leave out"
    );
}

#[test]
fn the_index_records_that_a_name_came_from_a_catalogue() {
    let module = Module::parse(&stripped()).expect("valid");
    let files = codegen::generate_with_signatures(&module, codegen::Layout::Single, &catalogue())
        .expect("generates");
    let json = &files
        .iter()
        .find(|file| file.name == "names.json")
        .expect("present")
        .contents;
    assert!(json.contains(r#""named_by": "signature""#), "{json}");
    common::assert_valid_json(json);
}

#[test]
fn what_the_module_says_about_itself_wins() {
    // A module that kept its names does not have them second-guessed by a
    // catalogue built elsewhere: the name section is the module's own statement
    // about the function, and a fingerprint match is an inference about it.
    let wat = format!(
        "(module (func $its_own_name (export \"a\") (param i32) (result i32){}))",
        body("i32.add")
    );
    let wasm = common::assemble("signatures-own-name", &wat);
    let module = Module::parse(&wasm).expect("valid");
    let files = codegen::generate_with_signatures(&module, codegen::Layout::Single, &catalogue())
        .expect("generates");
    let code = &files[0].contents;
    assert!(code.contains("/// `its_own_name`"), "{code}");
    assert!(
        !code.contains("wibble_copy"),
        "the catalogue does not overwrite it:\n{code}"
    );
}

#[test]
fn two_functions_of_the_same_shape_are_left_unnamed() {
    // Their constants are what tell them apart, and the constants are what a
    // fingerprint drops. Claiming either name would be wrong half the time.
    let wat = format!(
        "(module (func $one (export \"a\") (param i32) (result i32){})\
         (func $two (export \"b\") (param i32) (result i32){}))",
        body("i32.add"),
        body("i32.add")
    );
    let wasm = common::assemble("signatures-ambiguous", &wat);
    let module = Module::parse(&wasm).expect("valid");
    assert!(
        codegen::extract_signatures(&module).is_empty(),
        "an ambiguous fingerprint is dropped, not resolved"
    );
}

#[test]
fn the_same_function_catalogued_twice_is_not_ambiguous() {
    // Two exports of one body under one name is agreement, not a collision.
    // Two wat identifiers, one name section entry each way: `@name` is how
    // wasm-tools spells a name that is not the symbolic identifier.
    let wat = format!(
        "(module (func $one (@name \"same\") (export \"a\") (param i32) (result i32){})\
         (func $two (@name \"same\") (export \"b\") (param i32) (result i32){}))",
        body("i32.add"),
        body("i32.add")
    );
    let wasm = common::assemble("signatures-same-name", &wat);
    let module = Module::parse(&wasm).expect("valid");
    let catalogue = codegen::extract_signatures(&module);
    assert_eq!(catalogue.len(), 1, "{catalogue:?}");
    assert_eq!(catalogue.values().next().unwrap(), "same");
}

#[test]
fn a_short_function_is_never_recognised() {
    // Short bodies collide constantly — every `return the first argument` in
    // the module fingerprints alike — so below the floor nothing is claimed.
    let short = "(module (func $tiny (export \"a\") (param i32) (result i32) local.get 0))";
    let wasm = common::assemble("signatures-short", short);
    let module = Module::parse(&wasm).expect("valid");
    assert!(
        codegen::extract_signatures(&module).is_empty(),
        "not catalogued"
    );

    let anonymous = common::assemble(
        "signatures-short-anonymous",
        "(module (func (export \"a\") (param i32) (result i32) local.get 0))",
    );
    let module = Module::parse(&anonymous).expect("valid");
    // Even given a catalogue that does contain its fingerprint, it is not used.
    let mut forced = codegen::Signatures::new();
    forced.insert(
        analysis::fingerprint(&module, &module.funcs[0]),
        "tiny".to_string(),
    );
    let files = codegen::generate_with_signatures(&module, codegen::Layout::Single, &forced)
        .expect("generates");
    assert!(!files[0].contents.contains("f0_tiny"), "below the floor");
}

#[test]
fn a_function_that_computes_something_else_is_not_recognised() {
    let wat = format!(
        "(module (func (export \"a\") (param i32) (result i32){} i32.const 7 i32.mul))",
        body("i32.add")
    );
    let wasm = common::assemble("signatures-different", &wat);
    let module = Module::parse(&wasm).expect("valid");
    let files = codegen::generate_with_signatures(&module, codegen::Layout::Single, &catalogue())
        .expect("generates");
    assert!(
        !files[0].contents.contains("wibble"),
        "a longer body differs"
    );
}

#[test]
fn constants_and_offsets_do_not_enter_the_fingerprint() {
    // This is the whole point: the same source rebuilt moves its addresses and
    // its function indices, and must still be recognised.
    let one = common::assemble(
        "fingerprint-one",
        "(module (memory 1) (func (result i32) i32.const 100 i32.load i32.const 4 i32.add))",
    );
    let two = common::assemble(
        "fingerprint-two",
        "(module (memory 1) (func (result i32) i32.const 900 i32.load offset=8 i32.const 5 i32.add))",
    );
    let one = Module::parse(&one).expect("valid");
    let two = Module::parse(&two).expect("valid");
    assert_eq!(
        analysis::fingerprint(&one, &one.funcs[0]),
        analysis::fingerprint(&two, &two.funcs[0])
    );
}

#[test]
fn the_kind_of_access_does_enter_it() {
    // A byte load and a word load are not the same function, however alike the
    // rest reads.
    let one = common::assemble(
        "fingerprint-load-word",
        "(module (memory 1) (func (result i32) i32.const 0 i32.load))",
    );
    let two = common::assemble(
        "fingerprint-load-byte",
        "(module (memory 1) (func (result i32) i32.const 0 i32.load8_u))",
    );
    let one = Module::parse(&one).expect("valid");
    let two = Module::parse(&two).expect("valid");
    assert_ne!(
        analysis::fingerprint(&one, &one.funcs[0]),
        analysis::fingerprint(&two, &two.funcs[0])
    );
}

#[test]
fn an_empty_catalogue_changes_nothing() {
    let module = Module::parse(&stripped()).expect("valid");
    let plain = codegen::generate_files(&module, codegen::Layout::Single).expect("generates");
    let with_none = codegen::generate_with_signatures(
        &module,
        codegen::Layout::Single,
        &codegen::Signatures::new(),
    )
    .expect("generates");
    assert_eq!(plain[0].contents, with_none[0].contents);
}

#[test]
fn a_catalogue_names_the_functions_that_were_left_out_too() {
    // `--only` and `--signatures` are answers to different questions, and a
    // stub that keeps a recognised name is what makes the pair worth having:
    // the one function being read says which library function it calls.
    let module = Module::parse(&stripped()).expect("valid");
    let only = [1u32].into_iter().collect();
    let files = codegen::generate_options(
        &module,
        &codegen::Options {
            only: Some(only),
            signatures: catalogue(),
            ..codegen::Options::default()
        },
    )
    .expect("generates");
    let code = &files[0].contents;
    assert!(code.contains("fn f0_wibble_copy"), "{code}");
    assert!(
        code.contains(r#"unimplemented!("function #0 was not decompiled"#),
        "named, and still a stub:\n{code}"
    );
    assert!(code.contains("fn f1_wibble_fill"), "{code}");
}

/// A body touching every kind of operand a fingerprint deliberately drops,
/// parameterised by the operands themselves.
fn every_moving_part(constant: &str, offset: &str, callee: &str, segment: &str) -> String {
    format!(
        "(module
            (memory 1)
            (global $g (mut i32) (i32.const 0))
            (table 2 funcref)
            (type $unary (func (param i32) (result i32)))
            (data $d0 \"ab\")
            (data $d1 \"cd\")
            (func $one (param i32) (result i32) local.get 0)
            (func $two (param i32) (result i32) local.get 0)
            (func (export \"a\") (param i32) (result i32)
                i32.const {constant}
                i64.const {constant} drop
                f32.const {constant} drop
                f64.const {constant} drop
                global.get $g drop
                i32.const 0 global.set $g
                i32.const 0 i32.load offset={offset} drop
                i32.const 0 i32.const 0 i32.store offset={offset}
                i32.const 0 i32.const 0 i32.const 2 memory.init ${segment}
                data.drop ${segment}
                call ${callee}
                i32.const 0 i32.const 0 call_indirect (type $unary)))"
    )
}

#[test]
fn every_operand_that_moves_between_builds_is_dropped() {
    // Constants of all four widths, load and store offsets, the callee of a
    // direct call, a table type index, a global index and a data segment: all
    // of them are renumbered by a rebuild, and none may reach the fingerprint.
    let one = common::assemble(
        "fingerprint-moving-one",
        &every_moving_part("1", "0", "one", "d0"),
    );
    let two = common::assemble(
        "fingerprint-moving-two",
        &every_moving_part("2", "16", "two", "d1"),
    );
    let one = Module::parse(&one).expect("valid");
    let two = Module::parse(&two).expect("valid");
    assert_eq!(
        analysis::fingerprint(&one, &one.funcs[2]),
        analysis::fingerprint(&two, &two.funcs[2])
    );
    // And the body is long enough that this is a fingerprint anyone would use.
    assert!(one.funcs[2].body.len() >= analysis::FINGERPRINT_FLOOR);
}

#[test]
fn a_shape_two_functions_in_the_target_share_names_neither() {
    // The catalogue's own collisions are dropped, and so are the target's, for
    // the same reason: a fingerprint is a deliberately coarse equivalence
    // class, and two functions in it are two functions. Deduplicating only the
    // catalogue would put one library name on every member of the class.
    let wat = format!(
        "(module (func (export \"a\") (param i32) (result i32){})\
         (func (export \"b\") (param i32) (result i32){}))",
        body("i32.add"),
        body("i32.add")
    );
    let wasm = common::assemble("signatures-target-collision", &wat);
    let module = Module::parse(&wasm).expect("valid");
    let files = codegen::generate_with_signatures(&module, codegen::Layout::Single, &catalogue())
        .expect("generates");
    assert!(
        !files[0].contents.contains("wibble_copy"),
        "two candidates, no name:\n{}",
        files[0].contents
    );
}

#[test]
fn the_signature_is_part_of_the_fingerprint() {
    // Two bodies of the same shape that take different arguments are not the
    // same function, and leaving the type out let a catalogue name one after
    // the other.
    let one = common::assemble(
        "fingerprint-type-one",
        &format!(
            "(module (func (export \"a\") (param i32) (result i32){}))",
            body("i32.add")
        ),
    );
    let two = common::assemble(
        "fingerprint-type-two",
        &format!(
            "(module (func (export \"a\") (param i32 i32) (result i32){}))",
            body("i32.add")
        ),
    );
    let one = Module::parse(&one).expect("valid");
    let two = Module::parse(&two).expect("valid");
    assert_ne!(
        analysis::fingerprint(&one, &one.funcs[0]),
        analysis::fingerprint(&two, &two.funcs[0])
    );
}
