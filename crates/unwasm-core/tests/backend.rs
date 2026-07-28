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
    fn env_add(&mut self, p0: i32, p1: i32) -> i32 { p0 + p1 }
    fn env_log(&mut self, p0: i32) { self.logged.push(p0); }
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
    fn env_double(&mut self, p0: i32) -> i32 { p0 * 2 }
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
    fn env_unused(&mut self) -> i32 { 0 }
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
