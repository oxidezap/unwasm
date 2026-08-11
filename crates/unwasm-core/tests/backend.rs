//! What the emitted Rust looks like, and the shapes that used to break it.
//!
//! The differential tests answer "does it agree with the engine". These answer
//! the questions that come before that: does it compile at all, does a host get
//! a usable trait, and does a module whose names are hostile — a minified `a`,
//! a `loop`, an emoji — still produce legal Rust.

mod common;

use unwasm_core::{Error, Module, codegen};

#[test]
fn the_embedded_runtime_carries_the_code_and_not_the_tests() {
    let source = codegen::runtime_source();
    assert!(source.contains("pub struct Memory"), "the runtime is there");
    assert!(source.contains("pub fn i32_div_s"), "and its trapping ops");
    assert!(
        !source.contains("#[cfg(test)]"),
        "but not four hundred lines of tests"
    );
    // Cutting at the marker must not cut mid-item.
    assert_eq!(source.matches('{').count(), source.matches('}').count());
}

#[test]
fn a_minified_module_still_produces_legal_identifiers() {
    // What a shipped module actually looks like: single letters, symbols, and
    // names that collide with Rust keywords.
    let wasm = common::assemble(
        "minified",
        r#"(module
            (memory (export "x") 1)
            (func (export "a") (result i32) i32.const 1)
            (func (export "$") (result i32) i32.const 2)
            (func (export "loop") (result i32) i32.const 3)
            (func (export "0start") (result i32) i32.const 4)
            (func (export "with space") (result i32) i32.const 5)
            (func (export "café") (result i32) i32.const 6)
            (func (export "") (result i32) i32.const 7))"#,
    );
    let code = common::decompile(&wasm);
    // `loop` is a keyword and must not be emitted bare.
    assert!(code.contains("pub fn loop_("), "{code}");
    assert!(code.contains("pub fn _empty("));
    assert!(code.contains("pub fn _0start("));
    common::assert_compiles("minified", &wasm);
}

#[test]
fn two_exports_that_sanitise_alike_do_not_collide() {
    let wasm = common::assemble(
        "collide",
        r#"(module
            (func (export "a b") (result i32) i32.const 1)
            (func (export "a_b") (result i32) i32.const 2))"#,
    );
    common::assert_compiles("collide", &wasm);
}

#[test]
fn a_host_can_answer_the_imports_it_is_asked_for() {
    let wasm = common::assemble(
        "host",
        r#"(module
            (import "env" "add" (func $add (param i32 i32) (result i32)))
            (import "env" "log" (func $log (param i32)))
            (func (export "use_host") (param i32) (result i32)
                local.get 0
                call $log
                local.get 0
                i32.const 10
                call $add))"#,
    );
    const DRIVER: &str = r#"
mod generated;

struct Host { logged: Vec<i32> }

impl generated::Imports for Host {
    fn env_add(&mut self, _c: &mut generated::rt::Caller<'_>, p0: i32, p1: i32) -> i32 { p0 + p1 }
    fn env_log(&mut self, _c: &mut generated::rt::Caller<'_>, p0: i32) { self.logged.push(p0); }
}

fn main() {
    let mut instance = generated::Instance::with_host(Host { logged: Vec::new() });
    println!("{}", instance.use_host(5));
    println!("{:?}", instance.host.logged);
}
"#;
    let output = common::run_with_driver("host", &wasm, DRIVER);
    assert_eq!(output, "15\n[5]\n");
}

#[test]
fn an_unanswered_import_traps_instead_of_returning_zero() {
    let wasm = common::assemble(
        "no-host",
        r#"(module
            (import "env" "missing" (func $missing (result i32)))
            (func (export "call_it") (result i32) call $missing))"#,
    );
    const DRIVER: &str = r#"
mod generated;

fn main() {
    std::panic::set_hook(Box::new(|_| {}));
    let mut instance = generated::Instance::new();
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| instance.call_it()));
    match outcome {
        Ok(value) => println!("returned {value}"),
        Err(payload) => {
            let message = payload
                .downcast_ref::<String>()
                .cloned()
                .unwrap_or_default();
            println!("{message}");
        }
    }
}
"#;
    let output = common::run_with_driver("no-host", &wasm, DRIVER);
    assert!(
        output.contains("unimplemented import: env::missing"),
        "a missing host must be distinguishable from a host that answered 0: {output}"
    );
}

#[test]
fn the_extreme_integer_literals_survive_the_round_trip() {
    // `-2147483648` in Rust is a negation applied to a literal that does not
    // fit in i32, so it has to be emitted as `i32::MIN`.
    let wasm = common::assemble(
        "limits",
        r#"(module
            (func (export "min32") (result i32) i32.const -2147483648)
            (func (export "min64") (result i64) i64.const -9223372036854775808)
            (func (export "max32") (result i32) i32.const 2147483647))"#,
    );
    let code = common::decompile(&wasm);
    assert!(code.contains("i32::MIN"), "{code}");
    assert!(code.contains("i64::MIN"));
    common::assert_agrees(
        "limits",
        &wasm,
        &[
            common::call("min32", &[]),
            common::call("min64", &[]),
            common::call("max32", &[]),
        ],
    );
}

#[test]
fn float_constants_are_written_by_their_bits() {
    // A decimal literal would be a rounding of the module's constant, and NaN
    // and the infinities have no literal form at all.
    let wasm = common::assemble(
        "floats",
        r#"(module
            (func (export "pi") (result f64) f64.const 3.141592653589793)
            (func (export "inf") (result f32) f32.const inf)
            (func (export "nan") (result f32) f32.const nan)
            (func (export "tiny") (result f64) f64.const 5e-324))"#,
    );
    let code = common::decompile(&wasm);
    assert!(code.contains("f64::from_bits("), "{code}");
    assert!(code.contains("f32::from_bits("));
    common::assert_agrees(
        "floats",
        &wasm,
        &[
            common::call("pi", &[]),
            common::call("inf", &[]),
            common::call("nan", &[]),
            common::call("tiny", &[]),
        ],
    );
}

#[test]
fn a_value_produced_inside_a_block_and_used_after_it_stays_in_scope() {
    // The first thing a real module broke on. wasm's operand stack crosses a
    // block boundary; a Rust `let` inside the block does not, so the value has
    // to be named before the block opens.
    let wasm = common::assemble(
        "scope",
        r#"(module
            (memory (export "memory") 1)
            (func (export "across") (param i32) (result i32)
                local.get 0          ;; pushed before the block
                (block
                    local.get 0
                    i32.const 4
                    i32.store)       ;; a store inside forces a spill
                i32.const 1
                i32.add))"#,
    );
    common::assert_agrees(
        "scope",
        &wasm,
        &[
            common::call("across", &[common::Arg::I32(0)]),
            common::call("across", &[common::Arg::I32(64)]),
        ],
    );
}

#[test]
fn a_folded_local_is_spilled_before_the_local_is_written() {
    // `local.get 0` folds into its consumer, but if `local.set 0` runs first the
    // fold would read the new value. This is the test that would catch it.
    let wasm = common::assemble(
        "fold",
        r#"(module
            (func (export "order") (param i32) (result i32)
                local.get 0        ;; the old value
                i32.const 99
                local.set 0        ;; overwrite it
                local.get 0        ;; the new value
                i32.sub))          ;; old - new; wrong if the fold went stale
"#,
    );
    common::assert_agrees(
        "fold",
        &wasm,
        &[
            common::call("order", &[common::Arg::I32(5)]),
            common::call("order", &[common::Arg::I32(-1)]),
        ],
    );
}

#[test]
fn a_function_with_several_results_is_refused_by_the_backend() {
    let wasm = common::assemble(
        "multivalue",
        "(module (func (export \"pair\") (result i32 i32) i32.const 1 i32.const 2))",
    );
    let module = Module::parse(&wasm).expect("multi-value parses");
    let error = codegen::generate(&module).expect_err("but does not generate");
    assert!(matches!(error, Error::Unsupported { .. }));
    assert!(error.to_string().contains("multiple return values"));
}

#[test]
fn a_block_with_parameters_is_refused_by_the_backend() {
    let wasm = common::assemble(
        "blockparams",
        "(module (type $t (func (param i32) (result i32)))
            (func (export \"f\") (param i32) (result i32)
                local.get 0
                (block (type $t) i32.const 1 i32.add)))",
    );
    let module = Module::parse(&wasm).expect("parses");
    let error = codegen::generate(&module).expect_err("but does not generate");
    assert!(
        error.to_string().contains("block with parameters"),
        "{error}"
    );
}

#[test]
fn a_block_typed_by_a_signature_with_one_result_is_accepted() {
    let wasm = common::assemble(
        "blocktype",
        "(module (type $t (func (result i32)))
            (func (export \"f\") (result i32) (block (type $t) i32.const 7)))",
    );
    common::assert_agrees("blocktype", &wasm, &[common::call("f", &[])]);
}

#[test]
fn a_module_with_no_memory_still_compiles() {
    let wasm = common::assemble(
        "nomem",
        "(module (func (export \"pure\") (param i32) (result i32) local.get 0))",
    );
    common::assert_agrees(
        "nomem",
        &wasm,
        &[common::call("pure", &[common::Arg::I32(3)])],
    );
}

#[test]
fn an_indirect_call_to_the_wrong_signature_traps_on_both_sides() {
    let wasm = common::assemble(
        "mismatch",
        r#"(module
            (type $unary (func (param i32) (result i32)))
            (type $nullary (func (result i32)))
            (func $unary_fn (param i32) (result i32) local.get 0)
            (func $nullary_fn (result i32) i32.const 5)
            (table 4 funcref)
            (elem (i32.const 0) $unary_fn $nullary_fn)
            (func (export "as_unary") (param i32) (result i32)
                i32.const 7
                local.get 0
                call_indirect (type $unary)))"#,
    );
    common::assert_agrees(
        "mismatch",
        &wasm,
        &[
            // Slot 0 matches, slot 1 is the wrong type, slot 2 is empty, and
            // slot 9 is past the table.
            common::call("as_unary", &[common::Arg::I32(0)]),
            common::call("as_unary", &[common::Arg::I32(1)]),
            common::call("as_unary", &[common::Arg::I32(2)]),
            common::call("as_unary", &[common::Arg::I32(9)]),
        ],
    );
}

#[test]
fn an_exported_global_is_readable_from_the_generated_api() {
    let wasm = common::assemble(
        "globalexport",
        "(module (global (export \"answer\") i32 (i32.const 42)))",
    );
    const DRIVER: &str = r#"
mod generated;
fn main() {
    let instance = generated::Instance::new();
    println!("{}", instance.exported_global_answer());
}
"#;
    assert_eq!(
        common::run_with_driver("globalexport", &wasm, DRIVER),
        "42\n"
    );
}

#[test]
fn the_generated_module_is_usable_through_default() {
    let wasm = common::assemble(
        "default",
        "(module (memory 1) (func (export \"f\") (result i32) i32.const 1))",
    );
    const DRIVER: &str = r#"
mod generated;
fn main() {
    let mut instance = generated::Instance::default();
    println!("{}", instance.f());
}
"#;
    assert_eq!(common::run_with_driver("default", &wasm, DRIVER), "1\n");
}

#[test]
fn passive_segments_reach_memory_through_memory_init() {
    let wasm = common::assemble(
        "meminit",
        r#"(module
            (memory (export "memory") 1)
            (data $greeting "hello")
            (func (export "place") (param i32) (result i32)
                (memory.init $greeting (local.get 0) (i32.const 1) (i32.const 3))
                (data.drop $greeting)
                local.get 0
                i32.load8_u))"#,
    );
    common::assert_agrees(
        "meminit",
        &wasm,
        &[
            common::call("place", &[common::Arg::I32(0)]),
            common::call("place", &[common::Arg::I32(64)]),
            // Past the end: a trap on both sides.
            common::call("place", &[common::Arg::I32(65535)]),
        ],
    );
}

#[test]
fn a_void_function_returns_and_is_called_for_its_effect() {
    let wasm = common::assemble(
        "void",
        r#"(module
            (memory (export "memory") 1)
            (global $count (mut i32) (i32.const 0))
            (func $bump (param i32)
                local.get 0
                i32.eqz
                if
                    return
                end
                global.get $count
                local.get 0
                i32.add
                global.set $count)
            (func (export "run") (param i32) (result i32)
                local.get 0
                call $bump
                local.get 0
                call $bump
                global.get $count))"#,
    );
    let calls: Vec<_> = [0, 1, 5, -3]
        .into_iter()
        .map(|n| common::call("run", &[common::Arg::I32(n)]))
        .collect();
    common::assert_agrees("void", &wasm, &calls);
}

#[test]
fn a_block_nested_inside_dead_code_is_skipped_whole() {
    // After `br`, wasm's stack is polymorphic and the instructions that follow
    // cannot be emitted at all — including the blocks among them, whose `end`
    // must not be mistaken for the enclosing block's.
    let wasm = common::assemble(
        "deadnest",
        r#"(module
            (func (export "f") (param i32) (result i32)
                (block $out (result i32)
                    i32.const 1
                    br $out
                    (block
                        (loop
                            i32.const 2
                            drop
                            br 1)
                        i32.const 3
                        drop)
                    (if (i32.const 0) (then i32.const 4 drop) (else i32.const 5 drop))
                    i32.const 6)))"#,
    );
    common::assert_agrees(
        "deadnest",
        &wasm,
        &[common::call("f", &[common::Arg::I32(0)])],
    );
}

#[test]
fn an_if_inside_dead_code_keeps_its_else_straight() {
    let wasm = common::assemble(
        "deadelse",
        r#"(module
            (func (export "f") (result i32)
                unreachable
                (if (i32.const 1) (then nop) (else nop))
                i32.const 7))"#,
    );
    common::assert_agrees("deadelse", &wasm, &[common::call("f", &[])]);
}

#[test]
fn locals_of_every_type_start_at_zero() {
    let wasm = common::assemble(
        "locals",
        r#"(module
            (func (export "sum") (result f64)
                (local i32) (local i64) (local f32) (local f64)
                local.get 0 f64.convert_i32_s
                local.get 1 f64.convert_i64_s f64.add
                local.get 2 f64.promote_f32 f64.add
                local.get 3 f64.add))"#,
    );
    common::assert_agrees("locals", &wasm, &[common::call("sum", &[])]);
}

#[test]
fn the_narrow_i64_stores_write_the_bytes_they_say() {
    let wasm = common::assemble(
        "i64stores",
        r#"(module
            (memory (export "memory") 1)
            (func (export "write") (param i32) (param i64) (result i64)
                local.get 0 local.get 1 i64.store8
                local.get 0 local.get 1 i64.store16 offset=8
                local.get 0 local.get 1 i64.store32 offset=16
                local.get 0 i64.load8_u
                local.get 0 i64.load16_u offset=8 i64.add
                local.get 0 i64.load32_u offset=16 i64.add))"#,
    );
    let calls: Vec<_> = [-1i64, 0, 0x0123_4567_89AB_CDEF, i64::MIN]
        .into_iter()
        .map(|value| common::call("write", &[common::Arg::I32(32), common::Arg::I64(value)]))
        .collect();
    common::assert_agrees("i64stores", &wasm, &calls);
}

#[test]
fn exports_that_sanitise_onto_a_keyword_do_not_collide() {
    // `loop` becomes `loop_`, which is also what `loop_` is. The second one has
    // to move rather than shadow the first.
    let wasm = common::assemble(
        "keyword-collide",
        r#"(module
            (func (export "loop") (result i32) i32.const 1)
            (func (export "loop_") (result i32) i32.const 2))"#,
    );
    let code = common::decompile(&wasm);
    assert!(code.contains("pub fn loop_("), "{code}");
    assert!(code.contains("pub fn loop__1("), "{code}");
    common::assert_agrees(
        "keyword-collide",
        &wasm,
        &[common::call("loop", &[]), common::call("loop_", &[])],
    );
}

#[test]
fn an_indirect_call_can_reach_an_imported_function() {
    let wasm = common::assemble(
        "indirect-import",
        r#"(module
            (type $unary (func (param i32) (result i32)))
            (import "env" "double" (func $double (type $unary)))
            (func $triple (type $unary) local.get 0 i32.const 3 i32.mul)
            (table 2 funcref)
            (elem (i32.const 0) $double $triple)
            (func (export "through") (param i32) (param i32) (result i32)
                local.get 1
                local.get 0
                call_indirect (type $unary)))"#,
    );
    const DRIVER: &str = r#"
mod generated;
struct Host;
impl generated::Imports for Host {
    fn env_double(&mut self, _c: &mut generated::rt::Caller<'_>, p0: i32) -> i32 { p0 * 2 }
}
fn main() {
    let mut instance = generated::Instance::with_host(Host);
    println!("{}", instance.through(0, 21));
    println!("{}", instance.through(1, 21));
}
"#;
    assert_eq!(
        common::run_with_driver("indirect-import", &wasm, DRIVER),
        "42\n63\n"
    );
}

#[test]
fn an_indirect_call_that_returns_nothing_is_a_statement() {
    let wasm = common::assemble(
        "indirect-void",
        r#"(module
            (type $sink (func (param i32)))
            (type $other (func (result i32)))
            (import "env" "unused" (func $unused (type $other)))
            (memory (export "memory") 1)
            (func $store_it (type $sink) local.get 0 local.get 0 i32.store)
            (table 2 funcref)
            (elem (i32.const 0) $store_it)
            (func (export "through") (param i32) (param i32) (result i32)
                local.get 1
                local.get 0
                call_indirect (type $sink)
                local.get 1
                i32.load))"#,
    );
    const DRIVER: &str = r#"
mod generated;
struct Host;
impl generated::Imports for Host {
    fn env_unused(&mut self, _c: &mut generated::rt::Caller<'_>) -> i32 { 0 }
}
fn main() {
    let mut instance = generated::Instance::with_host(Host);
    println!("{}", instance.through(0, 32));
}
"#;
    assert_eq!(
        common::run_with_driver("indirect-void", &wasm, DRIVER),
        "32\n"
    );
}

#[test]
fn a_table_sized_by_a_declared_segment_is_still_empty() {
    // `elem declare` contributes no slots, so it must not stretch the table.
    let wasm = common::assemble(
        "declared-table",
        r#"(module
            (type $unary (func (param i32) (result i32)))
            (func $f (type $unary) local.get 0)
            (table 4 funcref)
            (elem declare func $f)
            (func (export "call_empty") (param i32) (result i32)
                local.get 0
                local.get 0
                call_indirect (type $unary)))"#,
    );
    // Every slot is empty, so every index traps — the same on both sides.
    common::assert_agrees(
        "declared-table",
        &wasm,
        &[
            common::call("call_empty", &[common::Arg::I32(0)]),
            common::call("call_empty", &[common::Arg::I32(3)]),
        ],
    );
}

#[test]
fn an_if_whose_consequent_cannot_finish_still_yields_a_value() {
    // The `else` branch is the only one that can produce the result, and the
    // assignment for the consequent must not be emitted at all.
    let wasm = common::assemble(
        "if-dead-consequent",
        r#"(module
            (func (export "pick") (param i32) (result i32)
                local.get 0
                if (result i32)
                    unreachable
                else
                    i32.const 42
                end))"#,
    );
    common::assert_agrees(
        "if-dead-consequent",
        &wasm,
        &[
            common::call("pick", &[common::Arg::I32(0)]),
            common::call("pick", &[common::Arg::I32(1)]),
        ],
    );
}

/// Splitting the output across modules must not change what it does.
///
/// The same fixtures, decompiled into part files instead of one, still have to
/// agree with the engine — including calls that cross a part boundary, which is
/// what `pub(crate)` on the generated methods is for.
#[test]
fn a_split_layout_behaves_exactly_like_a_single_file() {
    let wasm = common::assemble(
        "split",
        r#"(module
            (memory (export "memory") 1)
            (global $counter (mut i32) (i32.const 0))
            (type $unary (func (param i32) (result i32)))
            (func $double (type $unary) local.get 0 i32.const 2 i32.mul)
            (func $triple (type $unary) local.get 0 i32.const 3 i32.mul)
            (func $bump (param i32) global.get $counter local.get 0 i32.add global.set $counter)
            (table 2 funcref)
            (elem (i32.const 0) $double $triple)
            (func (export "chain") (param i32) (param i32) (result i32)
                local.get 0
                call $double        ;; a call into another part
                local.get 1
                call_indirect (type $unary)
                call $triple
                local.tee 0
                call $bump          ;; a void call into another part
                local.get 0
                global.get $counter
                i32.add)
            (func (export "counter") (result i32) global.get $counter))"#,
    );
    let mut calls = Vec::new();
    for n in [0, 1, 5, -3] {
        calls.push(common::call(
            "chain",
            &[common::Arg::I32(n), common::Arg::I32(0)],
        ));
        calls.push(common::call(
            "chain",
            &[common::Arg::I32(n), common::Arg::I32(1)],
        ));
        calls.push(common::call("counter", &[]));
    }
    // One function per file puts every call across a module boundary.
    common::assert_agrees_with_layout(
        "split",
        &wasm,
        &calls,
        unwasm_core::codegen::Layout::Split { lines_per_file: 1 },
    );
}

#[test]
fn a_split_layout_produces_a_mod_file_and_one_part_per_group() {
    let wasm = common::assemble(
        "split-shape",
        r#"(module
            (func (export "a") (result i32) i32.const 1)
            (func (export "b") (result i32) i32.const 2)
            (func (export "c") (result i32) i32.const 3)
            (func (export "d") (result i32) i32.const 4)
            (func (export "e") (result i32) i32.const 5))"#,
    );
    let module = Module::parse(&wasm).expect("valid");
    // A budget below the size of a single function puts each in its own file.
    let files = codegen::generate_files(&module, codegen::Layout::Split { lines_per_file: 2 })
        .expect("generating");

    let names: Vec<&str> = files.iter().map(|file| file.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "mod.rs",
            "part0.rs",
            "part1.rs",
            "part2.rs",
            "part3.rs",
            "part4.rs",
            "names.json"
        ],
        "five functions, none of which fits with another, plus the index"
    );

    let root = &files[0].contents;
    assert!(root.contains("mod part0;\nmod part1;\n"), "{root}");
    assert!(root.contains("pub fn a("), "the exports stay in mod.rs");
    assert!(!root.contains("pub(crate) fn f0("), "the functions do not");

    for part in files[1..].iter().filter(|file| file.name.ends_with(".rs")) {
        assert!(part.contents.contains("use super::*;"), "{}", part.name);
        assert!(part.contents.contains("impl<H: Imports> Instance<H> {"));
        // Each part carries its own lint allowances: they are per-file.
        assert!(part.contents.contains("#![allow("));
    }
    assert_eq!(files[3].contents.matches("pub(crate) fn f").count(), 1);
}

#[test]
fn the_layout_a_module_gets_by_default_follows_its_size() {
    let small = common::assemble(
        "layout-small",
        "(module (func (export \"f\") (result i32) i32.const 1))",
    );
    let module = Module::parse(&small).expect("valid");
    assert_eq!(
        codegen::Layout::for_module(&module),
        codegen::Layout::Single
    );

    // A module past the threshold, built rather than hand-written.
    let mut wat = String::from("(module");
    for index in 0..600 {
        wat.push_str(&format!(
            " (func (export \"f{index}\") (result i32) i32.const {index})"
        ));
    }
    wat.push(')');
    let large = common::assemble("layout-large", &wat);
    let module = Module::parse(&large).expect("valid");
    assert_eq!(
        codegen::Layout::for_module(&module),
        codegen::Layout::Split {
            lines_per_file: codegen::Layout::LINES_PER_FILE
        }
    );
    let files =
        codegen::generate_files(&module, codegen::Layout::for_module(&module)).expect("generating");
    assert!(files.len() > 2, "a large module splits");
    // No part is much over the budget — that is what the budget is for. The
    // exception is a single function larger than the whole budget, which
    // cannot be divided any further.
    for part in files[1..].iter().filter(|file| file.name.ends_with(".rs")) {
        let lines = part.contents.lines().count();
        let functions = part.contents.matches("pub(crate) fn f").count();
        assert!(
            lines <= codegen::Layout::LINES_PER_FILE * 2 || functions == 1,
            "{} has {lines} lines across {functions} functions",
            part.name
        );
    }
}

/// Indentation stops growing long before the nesting does.
///
/// A `br_table` dispatch in the VoIP module nests 2466 blocks, and indenting
/// each one put 7964 spaces in front of every line inside it: one part file came
/// to 650 MB, 644 MB of which was whitespace, and the module to 1.8 GB. Nothing
/// could read that depth off the leading space anyway — the labels are what say
/// where a `break` goes — so past a limit the space stops and the labels carry
/// it.
#[test]
fn indentation_stops_growing_and_the_module_still_agrees() {
    const DEPTH: usize = 200;
    let mut wat = String::from(
        "(module (memory (export \"memory\") 1)\n  (func (export \"deep\") (param i32) (result i32)\n",
    );
    for at in 0..DEPTH {
        wat.push_str(&format!(
            "    block\n    local.get 0\n    i32.const {at}\n    i32.eq\n    br_if 0\n"
        ));
    }
    wat.push_str("    local.get 0\n    i32.const 1000\n    i32.add\n    return\n");
    for _ in 0..DEPTH {
        wat.push_str("    end\n");
    }
    wat.push_str("    local.get 0\n    i32.const 2\n    i32.mul))\n");

    let wasm = common::assemble("deep-nesting", &wat);
    let code = common::decompile(&wasm);
    let widest = code
        .lines()
        .map(|line| line.len() - line.trim_start_matches(' ').len())
        .max()
        .expect("the module generated something");
    assert!(
        widest <= 4 * 32,
        "a line was indented {widest} spaces, so the cap is not holding"
    );
    // The nesting is really there — this is a cap on the whitespace, not on the
    // blocks — and the labels are what say so.
    assert!(
        code.contains(&format!("'b{}", DEPTH - 1)),
        "the blocks themselves were lost"
    );

    // And the only thing that changed is whitespace, which the engine settles.
    let calls: Vec<_> = [0, 7, 199, 500, -1]
        .into_iter()
        .map(|n| common::call("deep", &[common::Arg::I32(n)]))
        .collect();
    common::assert_agrees("deep-nesting", &wasm, &calls);
}

#[test]
fn a_zero_sized_split_still_produces_one_function_per_file() {
    // A guard on the arithmetic rather than on the caller: zero per file would
    // divide by zero and produce an unbounded number of empty modules.
    let wasm = common::assemble(
        "split-zero",
        "(module (func (export \"a\") (result i32) i32.const 1)
                 (func (export \"b\") (result i32) i32.const 2))",
    );
    let module = Module::parse(&wasm).expect("valid");
    let files = codegen::generate_files(&module, codegen::Layout::Split { lines_per_file: 0 })
        .expect("generating");
    assert_eq!(
        files.len(),
        4,
        "mod.rs, one part per function, and the index"
    );
}

#[test]
fn a_module_with_no_functions_splits_into_just_a_mod_file() {
    let wasm = common::assemble("split-empty", "(module (memory (export \"memory\") 1))");
    let module = Module::parse(&wasm).expect("valid");
    let files = codegen::generate_files(&module, codegen::Layout::Split { lines_per_file: 4 })
        .expect("generating");
    assert_eq!(files.len(), 2, "mod.rs and the index");
    assert!(!files[0].contents.contains("mod part"));
}

/// Annotations must be readable and must change nothing.
///
/// Naming a global and quoting a string are claims about meaning, not about
/// behaviour — so the same fixture still has to agree with the engine, and that
/// is what makes them safe to add.
#[test]
fn the_stack_pointer_is_named_and_the_module_still_agrees() {
    const SOURCE: &str = r#"
        struct Point { int x; int y; int z; };
        int through_the_stack(int a, int b) {
            struct Point point = { a, b, a ^ b };
            struct Point *alias = &point;
            alias->z += alias->x;
            return alias->x + alias->y + alias->z;
        }
    "#;
    // -O0 keeps the struct in the shadow stack, which is what makes the
    // prologue appear.
    let wasm = common::compile_c("sp", SOURCE, "-O0");
    let code = common::decompile(&wasm);
    assert!(
        code.contains("g0_stack_pointer"),
        "the stack pointer was not recognised:\n{}",
        &code[..code.len().min(4000)]
    );
    assert!(
        code.contains("The C stack pointer"),
        "the name arrived without its evidence"
    );

    let calls: Vec<_> = [(0, 0), (1, 2), (-3, 7)]
        .into_iter()
        .map(|(a, b)| {
            common::call(
                "through_the_stack",
                &[common::Arg::I32(a), common::Arg::I32(b)],
            )
        })
        .collect();
    common::assert_agrees("sp", &wasm, &calls);
}

#[test]
fn a_constant_that_addresses_a_string_carries_it_into_the_output() {
    let wasm = common::assemble(
        "quoted",
        r#"(module
            (memory (export "memory") 1)
            (data (i32.const 100) "hello, world\00")
            (data (i32.const 200) "\01\02\03\04\00")
            (func (export "text_at") (param i32) (result i32)
                i32.const 100
                local.get 0
                i32.add
                i32.load8_u)
            (func (export "binary_at") (result i32)
                i32.const 200
                i32.load8_u))"#,
    );
    let code = common::decompile(&wasm);
    assert!(code.contains(r#"100i32 /* "hello, world" */"#), "{code}");
    // Bytes that are not text are not quoted as if they were.
    assert!(!code.contains("200i32 /*"), "{code}");
    common::assert_agrees(
        "quoted",
        &wasm,
        &[
            common::call("text_at", &[common::Arg::I32(0)]),
            common::call("text_at", &[common::Arg::I32(7)]),
            common::call("binary_at", &[]),
        ],
    );
}

#[test]
fn a_string_with_a_length_beside_it_stops_where_the_length_says() {
    // How Rust passes `&str`: a pointer and a length, with no terminator. Read
    // to the next NUL instead and the text runs into the string after it.
    let wasm = common::assemble(
        "sliced",
        r#"(module
            (import "env" "take" (func $take (param i32 i32)))
            (memory (export "memory") 1)
            (data (i32.const 64) "first messagesecond message\00")
            (func (export "send")
                i32.const 64
                i32.const 13
                call $take))"#,
    );
    let code = common::decompile(&wasm);
    assert!(code.contains(r#"64i32 /* "first message" */"#), "{code}");
    // Only the annotations: the data segment itself is a byte-string literal
    // and contains the module's text verbatim, which is the point of it.
    let annotations = annotations(&code);
    assert!(
        !annotations.contains("first messagesecond"),
        "the length was ignored:\n{annotations}"
    );
}

/// The annotations: the lines where the emitter quotes the module's own text
/// back at the reader, which is where it has to be escaped.
///
/// Not "every line with a `/*` in it": the data segments are byte-string
/// literals now and contain the module's text verbatim, which is the point of
/// them and is not a comment.
fn annotations(code: &str) -> String {
    code.lines()
        .filter(|line| line.trim_start().starts_with("//") || line.contains("i32 /* "))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn a_string_containing_a_comment_terminator_cannot_break_the_output() {
    // The one way an annotation could stop the output compiling: text
    // containing `*/` would close its own comment.
    let wasm = common::assemble(
        "hostile",
        r#"(module
            (memory (export "memory") 1)
            (data (i32.const 32) "end */ then int x = 1; /* more\00")
            (func (export "at") (result i32) i32.const 32 i32.load8_u))"#,
    );
    let code = common::decompile(&wasm);
    let annotations = annotations(&code);
    assert!(
        !annotations.contains("*/ then"),
        "the comment was closed early:\n{annotations}"
    );
    assert!(
        annotations.contains("∗∕ then"),
        "the text is still readable, with the terminator disarmed:\n{annotations}"
    );
    // And it still builds and runs, which is the property that matters.
    common::assert_agrees("hostile", &wasm, &[common::call("at", &[])]);
}

/// The frame summary is a reading of the function, and reading must not change
/// what it does.
#[test]
fn a_frame_is_summarised_and_the_function_still_agrees() {
    const SOURCE: &str = r#"
        struct Point { int x; short y; char tag; };
        int through_a_struct(int a, int b) {
            struct Point point = { a, (short)b, (char)(a ^ b) };
            point.x += point.y;
            return point.x + point.tag;
        }
        int through_an_array(int n) {
            int values[8];
            for (int i = 0; i < 8; i++) values[i] = i * n;
            int total = 0;
            for (int i = 0; i < 8; i++) total += values[i];
            return total;
        }
    "#;
    // -O0 is where the shadow stack shows: at -O1 the struct is promoted into
    // wasm locals and there is no frame at all.
    let wasm = common::compile_c("frames", SOURCE, "-O0");
    let code = common::decompile(&wasm);

    assert!(
        code.contains("Stack frame:"),
        "no frame was reported:\n{code}"
    );
    assert!(code.contains("based at `frame`"));
    assert!(code.contains("| offset | width | reads | writes |"));
    // The frame base gets the name, and the other locals keep their indices.
    assert!(code.contains("let mut frame: i32"), "{code}");

    let mut calls = Vec::new();
    for (a, b) in [(0, 0), (5, -3), (1, 70000)] {
        calls.push(common::call(
            "through_a_struct",
            &[common::Arg::I32(a), common::Arg::I32(b)],
        ));
    }
    for n in [0, 3, -7] {
        calls.push(common::call("through_an_array", &[common::Arg::I32(n)]));
    }
    common::assert_agrees("frames", &wasm, &calls);
}

#[test]
fn the_summary_says_when_the_frame_address_leaves_the_function() {
    // `memcpy` takes the address, so nothing can be claimed about what happens
    // to the slots after that — and the summary has to say so rather than list
    // the accesses as if they were all of them.
    const SOURCE: &str = r#"
        void copy_bytes(void *destination, const void *source, unsigned long count);
        int leaks_its_frame(int n) {
            char buffer[16];
            for (int i = 0; i < 16; i++) buffer[i] = (char)(n + i);
            char other[16];
            copy_bytes(other, buffer, 16);
            return other[0] + other[15];
        }
    "#;
    let wasm = common::compile_c("escaping", SOURCE, "-O0");
    let code = common::decompile(&wasm);
    assert!(
        code.contains("The address leaves this function"),
        "an escape was not reported:\n{code}"
    );
}

#[test]
fn a_leaf_frame_is_marked_as_not_published() {
    // A function that calls nothing does not write the reserved address back
    // to the stack pointer — clang skips it, and requiring the write is how an
    // analysis misses every leaf function in a module.
    const SOURCE: &str = r#"
        int leaf(int a) {
            volatile int slot = a * 3;
            return slot + 1;
        }
    "#;
    let wasm = common::compile_c("leaf", SOURCE, "-O0");
    let code = common::decompile(&wasm);
    assert!(
        code.contains("not published"),
        "the leaf frame was misread:\n{code}"
    );
    common::assert_agrees(
        "leaf",
        &wasm,
        &[
            common::call("leaf", &[common::Arg::I32(0)]),
            common::call("leaf", &[common::Arg::I32(7)]),
        ],
    );
}

/// Level 1: the frame slots that can be placed become Rust bindings.
///
/// This is the level that says it is giving something up. The struct in the C
/// source below lives in the shadow stack at `-O0`, and at level 1 its fields
/// are `let mut` bindings — so the function computes the same answers and stops
/// leaving those bytes in linear memory. The differential test is what makes
/// that sentence checkable: the *answers* are compared against the engine, and
/// the memory is compared and reported rather than asserted.
#[test]
fn level_1_promotes_a_struct_out_of_the_shadow_stack() {
    const SOURCE: &str = r#"
        struct Point { int x; int y; int z; };
        __attribute__((export_name("through_the_stack")))
        int through_the_stack(int a, int b) {
            struct Point point = { a, b, a ^ b };
            point.z += point.x;
            return point.x + point.y + point.z;
        }
    "#;
    let wasm = common::compile_c("level1-struct", SOURCE, "-O0");

    let plain = common::decompile(&wasm);
    assert!(
        plain.contains("self.memory.store32"),
        "at level 0 the struct is memory:\n{}",
        &plain[..plain.len().min(400)]
    );

    let module = Module::parse(&wasm).expect("valid");
    let files = codegen::generate_options(
        &module,
        &codegen::Options {
            layout: codegen::Layout::Single,
            promote_frames: true,
            ..codegen::Options::default()
        },
    )
    .expect("generates");
    let code = &files[0].contents;

    // The bindings, and the note that says what they cost.
    assert!(code.contains("let mut s12: i32 = 0;"), "{code}");
    assert!(code.contains("**Level 1**:"), "{code}");
    assert!(
        code.contains("never written back"),
        "the cost is stated at the function:\n{code}"
    );
    // Filled from the frame on entry, so a slot read before it is written sees
    // what was there.
    assert!(
        code.contains("s12 = self.memory.load32(frame, 12);"),
        "{code}"
    );

    let calls: Vec<_> = [(0, 0), (1, 2), (-3, 7), (i32::MIN, -1)]
        .into_iter()
        .map(|(a, b)| {
            common::call(
                "through_the_stack",
                &[common::Arg::I32(a), common::Arg::I32(b)],
            )
        })
        .collect();
    let memory_matches = common::assert_agrees_at_level_1("level1-struct", &wasm, &calls);
    assert!(
        !memory_matches,
        "the bytes really did stop being written — if they still match, nothing was promoted"
    );
}

/// The narrow fields too, which is where a promotion is easiest to get wrong.
///
/// A `short` and a `char` in the frame are one and two-byte accesses. The
/// binding keeps them zero-extended, exactly as `load8_u` would return them, so
/// a signed read has to sign-extend and a store has to mask. Either one wrong
/// is a wrong answer, and the engine says which.
#[test]
fn level_1_gets_the_narrow_fields_right() {
    const SOURCE: &str = r#"
        struct Packed { int x; short y; signed char tag; unsigned char flag; };
        __attribute__((export_name("through_a_packed_struct")))
        int through_a_packed_struct(int a, int b) {
            struct Packed p = { a, (short)b, (signed char)(a ^ b), (unsigned char)b };
            p.y += (short)p.x;
            p.tag = (signed char)(p.tag - 1);
            return p.x + p.y + p.tag + p.flag;
        }
    "#;
    let wasm = common::compile_c("level1-narrow", SOURCE, "-O0");
    let calls: Vec<_> = [(0, 0), (5, -3), (1, 70000), (-1, 255), (300, -32768)]
        .into_iter()
        .map(|(a, b)| {
            common::call(
                "through_a_packed_struct",
                &[common::Arg::I32(a), common::Arg::I32(b)],
            )
        })
        .collect();
    common::assert_agrees_at_level_1("level1-narrow", &wasm, &calls);
}

/// A frame level 1 refuses, and says why.
///
/// `memcpy` takes the address, so nothing can be claimed about what happens to
/// the slots afterwards — and a slot promoted out of memory under an escaping
/// address would be read stale by whatever the callee did.
#[test]
fn level_1_refuses_a_frame_it_cannot_see_every_access_to() {
    const SOURCE: &str = r#"
        void copy_bytes(void *destination, const void *source, unsigned long count);
        __attribute__((export_name("leaks_its_frame")))
        int leaks_its_frame(int n) {
            char buffer[16];
            for (int i = 0; i < 16; i++) buffer[i] = (char)(n + i);
            char other[16];
            copy_bytes(other, buffer, 16);
            return other[0] + other[15];
        }
    "#;
    let wasm = common::compile_c("level1-escaping", SOURCE, "-O0");
    let module = Module::parse(&wasm).expect("valid");
    let files = codegen::generate_options(
        &module,
        &codegen::Options {
            layout: codegen::Layout::Single,
            promote_frames: true,
            ..codegen::Options::default()
        },
    )
    .expect("generates");
    let code = &files[0].contents;
    assert!(
        code.contains("Level 1 promoted nothing here: the address leaves this function"),
        "the refusal names the reason:\n{code}"
    );
    // And the frame is still memory, so the function is still exact.
    assert!(code.contains("self.memory.store8"), "{code}");
}

/// Level 0 is unchanged by any of this.
#[test]
fn level_0_is_what_it_was() {
    const SOURCE: &str = r#"
        struct Point { int x; int y; };
        __attribute__((export_name("sum")))
        int sum(int a, int b) {
            struct Point point = { a, b };
            return point.x + point.y;
        }
    "#;
    let wasm = common::compile_c("level1-off", SOURCE, "-O0");
    let module = Module::parse(&wasm).expect("valid");
    let plain = codegen::generate_files(&module, codegen::Layout::Single).expect("generates");
    let explicit = codegen::generate_options(
        &module,
        &codegen::Options {
            layout: codegen::Layout::Single,
            promote_frames: false,
            ..codegen::Options::default()
        },
    )
    .expect("generates");
    assert_eq!(plain[0].contents, explicit[0].contents);
    assert!(
        !plain[0].contents.contains("Level 1"),
        "and says nothing about a level it did not run"
    );
    common::assert_agrees(
        "level1-off",
        &wasm,
        &[common::call(
            "sum",
            &[common::Arg::I32(3), common::Arg::I32(4)],
        )],
    );
}

#[test]
fn a_module_with_no_stack_pointer_has_no_frames_and_still_works() {
    let wasm = common::assemble(
        "framless",
        "(module (func (export \"f\") (param i32) (result i32) local.get 0 i32.const 2 i32.mul))",
    );
    let code = common::decompile(&wasm);
    assert!(!code.contains("Stack frame:"), "{code}");
    // The declaration, not the word: the embedded runtime's own documentation
    // mentions `__stack_pointer`, and always will.
    assert!(!code.contains("stack_pointer:"), "{code}");
    assert!(!code.contains("g0_stack_pointer"), "{code}");
    common::assert_agrees(
        "framless",
        &wasm,
        &[common::call("f", &[common::Arg::I32(21)])],
    );
}

/// Folding must never lose an effect or a trap.
///
/// A folded value that is dropped is never emitted at all, so anything that
/// *had* to happen cannot be folded. The first version of this folded
/// everything and the differential tests caught it in a minute: a dropped
/// division stopped trapping, and a dropped atomic stopped writing.
#[test]
fn a_dropped_division_still_traps() {
    let wasm = common::assemble(
        "fold-drop-div",
        r#"(module
            (func (export "divide_and_discard") (param i32) (param i32) (result i32)
                local.get 0
                local.get 1
                i32.div_s
                drop
                i32.const 7))"#,
    );
    common::assert_agrees(
        "fold-drop-div",
        &wasm,
        &[
            common::call(
                "divide_and_discard",
                &[common::Arg::I32(10), common::Arg::I32(2)],
            ),
            // The divisor is zero: the result is discarded, and the trap is not.
            common::call(
                "divide_and_discard",
                &[common::Arg::I32(10), common::Arg::I32(0)],
            ),
        ],
    );
}

#[test]
fn a_dropped_load_still_traps() {
    let wasm = common::assemble(
        "fold-drop-load",
        r#"(module
            (memory (export "memory") 1)
            (func (export "read_and_discard") (param i32) (result i32)
                local.get 0
                i32.load
                drop
                i32.const 7))"#,
    );
    common::assert_agrees(
        "fold-drop-load",
        &wasm,
        &[
            common::call("read_and_discard", &[common::Arg::I32(0)]),
            common::call("read_and_discard", &[common::Arg::I32(65534)]),
        ],
    );
}

#[test]
fn a_dropped_atomic_still_writes() {
    let wasm = common::assemble(
        "fold-drop-atomic",
        r#"(module
            (memory (export "memory") 1 1 shared)
            (func (export "bump_and_discard") (param i32) (result i32)
                local.get 0
                i32.const 5
                i32.atomic.rmw.add
                drop
                local.get 0
                i32.atomic.load))"#,
    );
    let calls: Vec<_> = (0..3)
        .map(|_| common::call("bump_and_discard", &[common::Arg::I32(0)]))
        .collect();
    common::assert_agrees("fold-drop-atomic", &wasm, &calls);
}

/// Folding changes where an expression is written, never when it happens.
#[test]
fn a_folded_value_is_still_read_before_the_local_changes() {
    let wasm = common::assemble(
        "fold-order",
        r#"(module
            (func (export "order") (param i32) (result i32)
                local.get 0
                i32.const 3
                i32.mul        ;; folded: `l0 * 3`
                i32.const 99
                local.set 0    ;; l0 changes before the product is used
                local.get 0
                i32.add))"#,
    );
    common::assert_agrees(
        "fold-order",
        &wasm,
        &[
            common::call("order", &[common::Arg::I32(5)]),
            common::call("order", &[common::Arg::I32(-2)]),
        ],
    );
}

/// Precedence: Rust's is not wasm's, and folding is what makes it matter.
#[test]
fn folding_keeps_the_arithmetic_it_was_given() {
    // `(a + b) as u32 >> 1` folded carelessly becomes `a + (b as u32 >> 1)`.
    // Every one of these is a shape where a missing bracket changes the answer.
    let wasm = common::assemble(
        "fold-precedence",
        r#"(module
            (func (export "cast_of_sum") (param i32) (param i32) (result i32)
                local.get 0
                local.get 1
                i32.add
                i32.const 1
                i32.shr_u)
            (func (export "sum_of_shifts") (param i32) (param i32) (result i32)
                local.get 0
                i32.const 2
                i32.shl
                local.get 1
                i32.const 3
                i32.shr_s
                i32.mul)
            (func (export "compare_of_products") (param i32) (param i32) (result i32)
                local.get 0
                local.get 1
                i32.mul
                local.get 1
                local.get 0
                i32.sub
                i32.lt_u)
            (func (export "negated_sum") (param f64) (param f64) (result f64)
                local.get 0
                local.get 1
                f64.add
                f64.neg
                local.get 0
                f64.mul))"#,
    );
    let mut calls = Vec::new();
    for (a, b) in [(0, 0), (1, 2), (-1, 3), (i32::MIN, -1), (0x7FFF_FFFF, 1)] {
        calls.push(common::call(
            "cast_of_sum",
            &[common::Arg::I32(a), common::Arg::I32(b)],
        ));
        calls.push(common::call(
            "sum_of_shifts",
            &[common::Arg::I32(a), common::Arg::I32(b)],
        ));
        calls.push(common::call(
            "compare_of_products",
            &[common::Arg::I32(a), common::Arg::I32(b)],
        ));
    }
    for (a, b) in [(1.5, 2.5), (-0.0, 0.0), (f64::INFINITY, 1.0)] {
        calls.push(common::call(
            "negated_sum",
            &[common::Arg::F64(a), common::Arg::F64(b)],
        ));
    }
    common::assert_agrees("fold-precedence", &wasm, &calls);
}

#[test]
fn a_long_chain_of_arithmetic_is_named_rather_than_nested_forever() {
    // Folding without a limit turns a hundred additions into one line.
    let mut wat =
        String::from("(module (func (export \"chain\") (param i32) (result i32) local.get 0");
    for _ in 0..40 {
        wat.push_str(" i32.const 3 i32.add");
    }
    wat.push_str("))");
    let wasm = common::assemble("fold-limit", &wat);
    let code = common::decompile(&wasm);
    let longest = code.lines().map(str::len).max().unwrap_or(0);
    assert!(
        longest < 400,
        "the expression was never given a name: longest line is {longest}"
    );
    common::assert_agrees(
        "fold-limit",
        &wasm,
        &[common::call("chain", &[common::Arg::I32(1)])],
    );
}

#[test]
fn an_operand_that_reads_a_global_is_named_before_the_call_that_takes_it() {
    // `self.f1(self.g0)` borrows `self` twice, and the second one is a method
    // call. Anything mentioning `self` is named first; a plain local is not.
    let wasm = common::assemble(
        "spill-global",
        r#"(module
            (memory (export "memory") 1)
            (global $counter (mut i32) (i32.const 7))
            (func $double (param i32) (result i32) local.get 0 i32.const 2 i32.mul)
            (func (export "through_a_global") (param i32) (result i32)
                global.get $counter
                local.get 0
                i32.add
                call $double
                global.get $counter
                i32.add)
            (func (export "store_a_global") (param i32) (result i32)
                local.get 0
                global.get $counter
                i32.store
                local.get 0
                i32.load))"#,
    );
    let code = common::decompile(&wasm);
    assert!(
        code.contains("let t"),
        "an operand mentioning self must be named:\n{code}"
    );
    common::assert_agrees(
        "spill-global",
        &wasm,
        &[
            common::call("through_a_global", &[common::Arg::I32(3)]),
            common::call("store_a_global", &[common::Arg::I32(64)]),
        ],
    );
}

#[test]
fn a_table_with_both_an_active_and_a_declared_segment_fills_only_the_active_one() {
    let wasm = common::assemble(
        "elem-mixed",
        r#"(module
            (type $unary (func (param i32) (result i32)))
            (func $double (type $unary) local.get 0 i32.const 2 i32.mul)
            (func $triple (type $unary) local.get 0 i32.const 3 i32.mul)
            (table 4 funcref)
            (elem (i32.const 0) $double)
            (elem declare func $triple)
            (func (export "through") (param i32) (param i32) (result i32)
                local.get 1
                local.get 0
                call_indirect (type $unary)))"#,
    );
    common::assert_agrees(
        "elem-mixed",
        &wasm,
        &[
            // Slot 0 was filled by the active segment; the declared one filled
            // nothing, so slot 1 is empty and traps.
            common::call("through", &[common::Arg::I32(0), common::Arg::I32(21)]),
            common::call("through", &[common::Arg::I32(1), common::Arg::I32(21)]),
        ],
    );
}

#[test]
fn a_function_with_more_locals_than_the_bitset_holds_is_still_correct() {
    // Which locals a folded expression reads is a 64-bit set, and a function
    // can have more locals than that. Past the 64th the answer is
    // conservative — any high write invalidates it — and this is the module
    // that proves the conservative path is still right.
    let mut wat = String::from(
        "(module (memory (export \"memory\") 1)\n  (func (export \"go\") (result i32)\n",
    );
    for _ in 0..80 {
        wat.push_str("    (local i32)\n");
    }
    // Read local 70, write local 70, and read it again: the value pushed
    // before the write must not survive it.
    wat.push_str(
        "    i32.const 5 local.set 70\n\
         \x20   local.get 70\n\
         \x20   i32.const 9 local.set 70\n\
         \x20   local.get 70\n\
         \x20   i32.add))",
    );
    let wasm = common::assemble("many-locals", &wat);
    common::assert_agrees("many-locals", &wasm, &[common::call("go", &[])]);
}

#[test]
fn a_data_segment_of_spaces_keeps_every_one_of_them() {
    // A line continuation in Rust eats the newline *and every space after it*,
    // so a data byte that is a space immediately after one disappears. This is
    // what that looks like from the outside: a segment of nothing but spaces,
    // long enough to wrap many times.
    //
    // It cost 626 bytes of one segment of the VoIP module, and the earlier
    // all-256-values test passed the whole time — its wrap points happened
    // never to land before a space.
    let spaces = " ".repeat(1000);
    let wasm = common::assemble(
        "spaces",
        &format!(
            r#"(module
                (memory (export "memory") 1)
                (data (i32.const 0) "{spaces}")
                (func (export "at") (param i32) (result i32)
                    local.get 0 i32.load8_u)
                (func (export "sum") (result i32)
                    (local $at i32) (local $total i32)
                    (loop $next
                        local.get $total local.get $at i32.load8_u i32.add
                        local.set $total
                        local.get $at i32.const 1 i32.add local.set $at
                        local.get $at i32.const 1000 i32.lt_u br_if $next)
                    local.get $total))"#
        ),
    );
    // Every one of the thousand is a space, so the sum is 32 * 1000. One eaten
    // byte shifts the rest and the sum changes.
    common::assert_agrees(
        "spaces",
        &wasm,
        &[
            common::call("sum", &[]),
            common::call("at", &[common::Arg::I32(999)]),
        ],
    );
}

#[test]
fn every_byte_survives_the_data_literal() {
    // The segments are byte-string literals rather than arrays of `0x41`, and
    // the whole point of that is speed — so the thing to pin is that it costs
    // no fidelity. All 256 values, including the quote, the backslash, the
    // newline and everything above 0x7F.
    // Every value, and then every value again preceded by a space, so that a
    // wrap lands before a space whatever the wrap width is.
    let mut bytes = String::new();
    for byte in 0u32..256 {
        bytes.push_str(&format!("\\{byte:02x}"));
    }
    for byte in 0u32..256 {
        bytes.push_str(&format!("\\20\\{byte:02x}"));
    }
    let wasm = common::assemble(
        "every-byte",
        &format!(
            r#"(module
                (memory (export "memory") 1)
                (data (i32.const 0) "{bytes}")
                (func (export "at") (param i32) (result i32)
                    local.get 0 i32.load8_u))"#
        ),
    );
    let code = common::decompile(&wasm);
    assert!(code.contains(r#"const DATA_0: &[u8] = b""#), "{code}");

    const DRIVER: &str = r#"
mod generated;
fn main() {
    let mut instance = generated::Instance::new();
    let all: Vec<i32> = (0..768).map(|at| instance.at(at)).collect();
    println!("{}", all.iter().map(|byte| byte.to_string()).collect::<Vec<_>>().join(","));
}
"#;
    let output = common::run_with_driver("every-byte", &wasm, DRIVER);
    let mut expected: Vec<String> = (0..256).map(|byte| byte.to_string()).collect();
    for byte in 0..256 {
        expected.push("32".to_string());
        expected.push(byte.to_string());
    }
    assert_eq!(output.trim(), expected.join(","));
}
