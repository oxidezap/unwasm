//! Every numeric opcode, against the engine, one at a time.
//!
//! The differential tests over C fixtures cover what a compiler happens to
//! emit. That is not the same as covering the instruction set: `i64.rotl`,
//! `f32.copysign` and the saturating truncations barely appear in compiler
//! output, and those are exactly the templates a reader cannot check by eye.
//!
//! So each opcode gets a module of its own, written in wat, and is run on a
//! spread of values chosen for the places the semantics turn: zero, the
//! minimum, the maximum, a shift count past the width, NaN, the infinities and
//! both zeros.

mod common;

use common::{Arg, assert_agrees, call};
use unwasm_core::ValType;
use unwasm_core::ops::NumOp;

/// The values each type is probed with.
fn probes(ty: ValType) -> Vec<Arg> {
    match ty {
        ValType::I32 => [
            0,
            1,
            -1,
            2,
            -7,
            31,
            32,
            33,
            0x7FFF_FFFF,
            i32::MIN,
            0x0F0F_0F0F,
            -2147483647,
        ]
        .into_iter()
        .map(Arg::I32)
        .collect(),
        ValType::I64 => [
            0,
            1,
            -1,
            2,
            -7,
            63,
            64,
            65,
            i64::MAX,
            i64::MIN,
            0x0123_4567_89AB_CDEF,
        ]
        .into_iter()
        .map(Arg::I64)
        .collect(),
        ValType::F32 => [
            0.0,
            -0.0,
            1.0,
            -1.0,
            2.5,
            -2.5,
            3.5,
            0.5,
            f32::NAN,
            f32::INFINITY,
            f32::NEG_INFINITY,
            f32::MAX,
            f32::MIN_POSITIVE,
            4294967296.0,
            -2147483904.0,
        ]
        .into_iter()
        .map(Arg::F32)
        .collect(),
        ValType::F64 => [
            0.0,
            -0.0,
            1.0,
            -1.0,
            2.5,
            -2.5,
            3.5,
            f64::NAN,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::MAX,
            9223372036854775808.0,
            -2147483649.0,
        ]
        .into_iter()
        .map(Arg::F64)
        .collect(),
    }
}

/// The wat spelling of an opcode, derived from its variant name:
/// `I32TruncSatF32S` is `i32.trunc_sat_f32_s`.
fn wat_name(op: NumOp) -> String {
    let name = op.name();
    let (ty, rest) = name.split_at(3);
    let mut out = ty.to_ascii_lowercase();
    out.push('.');
    let mut previous_was_upper = true;
    for (at, ch) in rest.char_indices() {
        if ch.is_ascii_uppercase() && at > 0 && !previous_was_upper {
            out.push('_');
        }
        previous_was_upper = ch.is_ascii_uppercase();
        out.push(ch.to_ascii_lowercase());
    }
    // `I32TruncSatF32S` splits into `trunc_sat_f32_s` on the capital letters,
    // except that `Sat` runs into `F32` without one.
    out.replace("truncsat", "trunc_sat")
}

fn wat_type(ty: ValType) -> &'static str {
    ty.rust_name()
}

/// Builds a module exporting one opcode as `probe`.
fn module_for(op: NumOp) -> String {
    let params: Vec<String> = op
        .operands()
        .iter()
        .map(|ty| format!("(param {})", wat_type(*ty)))
        .collect();
    let gets: Vec<String> = (0..op.operands().len())
        .map(|at| format!("local.get {at}"))
        .collect();
    format!(
        "(module (func (export \"probe\") {} (result {}) {} {}))",
        params.join(" "),
        wat_type(op.result()),
        gets.join(" "),
        wat_name(op)
    )
}

/// Runs one opcode over the cross product of its operands' probe values.
fn check(op: NumOp) {
    let wat = module_for(op);
    let name = format!("op-{}", op.name());
    let wasm = common::assemble(&name, &wat);

    let operands = op.operands();
    let mut calls = Vec::new();
    match operands.len() {
        1 => {
            for value in probes(operands[0]) {
                calls.push(call("probe", &[value]));
            }
        }
        _ => {
            for left in probes(operands[0]) {
                for right in probes(operands[1]) {
                    calls.push(call("probe", &[left, right]));
                }
            }
        }
    }
    assert_agrees(&name, &wasm, &calls);
}

/// Splitting the sweep across several test functions lets cargo run them in
/// parallel; one test over 150 opcodes would run them one after another.
fn check_range(range: std::ops::Range<usize>) {
    for &op in &NumOp::ALL[range] {
        check(op);
    }
}

#[test]
fn opcodes_000_to_030_agree() {
    check_range(0..30);
}

#[test]
fn opcodes_030_to_060_agree() {
    check_range(30..60);
}

#[test]
fn opcodes_060_to_090_agree() {
    check_range(60..90);
}

#[test]
fn opcodes_090_to_120_agree() {
    check_range(90..120);
}

#[test]
fn opcodes_120_to_end_agree() {
    check_range(120..NumOp::ALL.len());
}

/// The sweep must actually cover the table. If an opcode is added and the
/// ranges above are not extended, this is what says so.
#[test]
fn the_sweep_covers_every_opcode() {
    assert_eq!(
        NumOp::ALL.len(),
        136,
        "the opcode table changed size; extend the ranges in this file to match"
    );
}

/// Control flow, memory and calls, spelled directly in wat rather than waited
/// for in compiler output.
#[test]
fn control_flow_shapes_agree() {
    const WAT: &str = r#"
(module
  (memory (export "memory") 1)
  (func $helper (param i32) (result i32) local.get 0 i32.const 3 i32.mul)
  (table 3 funcref)
  (elem (i32.const 0) $helper $helper $helper)
  (type $unary (func (param i32) (result i32)))

  ;; A block that yields a value from two different exits.
  (func (export "block_value") (param i32) (result i32)
    (block (result i32)
      local.get 0
      i32.const 10
      i32.lt_s
      if (result i32)
        i32.const 111
      else
        local.get 0
        i32.const 5
        i32.mul
      end))

  ;; A loop carrying an accumulator, exited by br_if.
  (func (export "loop_sum") (param i32) (result i32)
    (local i32) (local i32)
    (loop $again
      local.get 1
      local.get 2
      i32.add
      local.set 1
      local.get 2
      i32.const 1
      i32.add
      local.set 2
      local.get 2
      local.get 0
      i32.lt_s
      br_if $again)
    local.get 1)

  ;; br_table, including the default arm.
  (func (export "table_jump") (param i32) (result i32)
    (block $a
      (block $b
        (block $c
          local.get 0
          br_table $a $b $c)
        i32.const 300
        return)
      i32.const 200
      return)
    i32.const 100)

  ;; A branch out of two nested blocks at once.
  (func (export "deep_break") (param i32) (result i32)
    (block $outer (result i32)
      (block $inner
        local.get 0
        i32.eqz
        br_if $inner
        i32.const 7
        br $outer)
      i32.const 9))

  (func (export "indirect") (param i32) (param i32) (result i32)
    local.get 1
    local.get 0
    call_indirect (type $unary))

  (func (export "select_value") (param i32) (param i32) (param i32) (result i32)
    local.get 0
    local.get 1
    local.get 2
    select)

  (func (export "unreachable_after_return") (param i32) (result i32)
    local.get 0
    return
    unreachable)

  (func (export "grow_and_size") (param i32) (result i32)
    local.get 0
    memory.grow
    drop
    memory.size)

  (func (export "fill_and_copy") (param i32) (result i32)
    i32.const 0
    local.get 0
    i32.const 16
    memory.fill
    i32.const 64
    i32.const 0
    i32.const 16
    memory.copy
    i32.const 64
    i32.load8_u)
)
"#;
    let wasm = common::assemble("shapes", WAT);
    let mut calls = Vec::new();
    for n in [-1, 0, 1, 2, 3, 5, 9, 20] {
        calls.push(call("block_value", &[Arg::I32(n)]));
        calls.push(call("loop_sum", &[Arg::I32(n)]));
        calls.push(call("table_jump", &[Arg::I32(n)]));
        calls.push(call("deep_break", &[Arg::I32(n)]));
        calls.push(call("indirect", &[Arg::I32(n), Arg::I32(4)]));
        calls.push(call(
            "select_value",
            &[Arg::I32(11), Arg::I32(22), Arg::I32(n)],
        ));
        calls.push(call("unreachable_after_return", &[Arg::I32(n)]));
        calls.push(call("fill_and_copy", &[Arg::I32(n)]));
    }
    calls.push(call("grow_and_size", &[Arg::I32(1)]));
    calls.push(call("grow_and_size", &[Arg::I32(70000)]));
    assert_agrees("shapes", &wasm, &calls);
}

/// `unreachable` is a trap, and the code after a branch is code the type
/// checker must never see: wasm's stack is polymorphic there and Rust's is not.
#[test]
fn unreachable_code_traps_and_still_compiles() {
    const WAT: &str = r#"
(module
  (func (export "always_traps") (result i32)
    unreachable)
  (func (export "traps_in_a_branch") (param i32) (result i32)
    local.get 0
    if
      unreachable
    end
    i32.const 5)
  ;; After `br`, the stack is polymorphic: these instructions are unreachable
  ;; and would not type-check if they were emitted.
  (func (export "dead_code_after_br") (param i32) (result i32)
    (block (result i32)
      i32.const 1
      br 0
      i64.const 2
      f64.const 3
      drop
      drop
      i32.const 4))
)
"#;
    let wasm = common::assemble("dead", WAT);
    let calls = vec![
        call("always_traps", &[]),
        call("traps_in_a_branch", &[Arg::I32(0)]),
        call("traps_in_a_branch", &[Arg::I32(1)]),
        call("dead_code_after_br", &[Arg::I32(0)]),
    ];
    assert_agrees("dead", &wasm, &calls);
}

/// Globals, data segments and a start function: what instantiation has to get
/// right before any call runs.
#[test]
fn instantiation_agrees() {
    const WAT: &str = r#"
(module
  (memory (export "memory") 1)
  (global $counter (mut i32) (i32.const 41))
  (global $constant i32 (i32.const 7))
  (global (export "wide") i64 (i64.const -5))
  (data (i32.const 16) "hello, world")
  (data (i32.const 100) "\00\01\02\03")
  (start $init)
  (func $init
    global.get $counter
    i32.const 1
    i32.add
    global.set $counter)
  (func (export "counter") (result i32) global.get $counter)
  (func (export "constant") (result i32) global.get $constant)
  (func (export "byte_at") (param i32) (result i32)
    local.get 0
    i32.load8_u)
)
"#;
    let wasm = common::assemble("instantiation", WAT);
    let mut calls = vec![call("counter", &[]), call("constant", &[])];
    for at in [0, 16, 17, 27, 28, 100, 103] {
        calls.push(call("byte_at", &[Arg::I32(at)]));
    }
    assert_agrees("instantiation", &wasm, &calls);
}

/// Loads and stores at every width, including the sign-extending ones and the
/// static offset in the immediate.
#[test]
fn memory_widths_agree() {
    const WAT: &str = r#"
(module
  (memory (export "memory") 1)
  (func (export "roundtrip") (param i32) (result i32)
    local.get 0 i32.const -1 i32.store8
    local.get 0 i32.const -2 i32.store16 offset=4
    local.get 0 i32.const -3 i32.store offset=8
    local.get 0 i64.const -4 i64.store offset=16
    local.get 0 i32.load8_s
    local.get 0 i32.load8_u i32.add
    local.get 0 i32.load16_s offset=4 i32.add
    local.get 0 i32.load16_u offset=4 i32.add
    local.get 0 i32.load offset=8 i32.add
    local.get 0 i64.load8_s offset=16 i32.wrap_i64 i32.add
    local.get 0 i64.load8_u offset=16 i32.wrap_i64 i32.add
    local.get 0 i64.load16_s offset=16 i32.wrap_i64 i32.add
    local.get 0 i64.load16_u offset=16 i32.wrap_i64 i32.add
    local.get 0 i64.load32_s offset=16 i32.wrap_i64 i32.add
    local.get 0 i64.load32_u offset=16 i32.wrap_i64 i32.add
    local.get 0 i64.load offset=16 i32.wrap_i64 i32.add)
  (func (export "floats") (param i32) (param f32) (param f64) (result f64)
    local.get 0 local.get 1 f32.store
    local.get 0 local.get 2 f64.store offset=8
    local.get 0 f32.load f64.promote_f32
    local.get 0 f64.load offset=8
    f64.add)
)
"#;
    let wasm = common::assemble("widths", WAT);
    let mut calls = Vec::new();
    for at in [0, 1, 7, 1024, 65500, 65535] {
        calls.push(call("roundtrip", &[Arg::I32(at)]));
        calls.push(call(
            "floats",
            &[Arg::I32(at), Arg::F32(-1.5), Arg::F64(2.25)],
        ));
    }
    assert_agrees("widths", &wasm, &calls);
}
