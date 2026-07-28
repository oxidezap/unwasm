//! Every numeric instruction, declared once.
//!
//! A wasm opcode needs three things said about it: which wasmparser operator it
//! comes from, what it does to the stack, and how it is written in Rust. Saying
//! them in three places is how a decompiler ends up lowering `i32.shr_u` and
//! emitting an arithmetic shift. The [`num_ops!`] table below says all three on
//! one line, and generates the lowering, the signature and the template from it.
//!
//! The templates are where wasm and Rust genuinely disagree, and each
//! disagreement is deliberate:
//!
//! - arithmetic wraps (`wrapping_add`), because Rust's `+` panics in debug;
//! - shift counts are masked (`& 31`), because Rust's `<<` panics past the
//!   width while wasm defines it modulo the width;
//! - unsigned comparisons and shifts cast through `u32`/`u64`, because the
//!   value type carries no sign in wasm and the operator does;
//! - anything that can trap goes through [`crate::rt`], not through an operator.

use crate::ValType;

/// Substitutes `{0}`, `{1}`, `{2}` in a template with the given operands.
///
/// A hand-rolled two-line substitution rather than a formatting crate: the
/// templates are ours, so the only placeholders that exist are the ones we
/// wrote.
#[must_use]
pub fn render(template: &str, operands: &[String]) -> String {
    let mut out = String::with_capacity(template.len() + 16);
    let mut rest = template;
    while let Some(at) = rest.find('{') {
        out.push_str(&rest[..at]);
        let close = rest[at..]
            .find('}')
            .expect("template placeholder is unterminated");
        let index: usize = rest[at + 1..at + close]
            .parse()
            .expect("template placeholder is not a number");
        out.push_str(&operands[index]);
        rest = &rest[at + close + 1..];
    }
    out.push_str(rest);
    out
}

macro_rules! num_ops {
    ($($variant:ident, [$($operand:ident),*] -> $result:tt, $template:literal;)*) => {
        /// A numeric instruction: everything that only reads and writes the
        /// operand stack, with no control flow and no memory of its own.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        #[allow(missing_docs, reason = "one variant per wasm opcode; the opcode is the documentation")]
        pub enum NumOp {
            $($variant,)*
        }

        impl NumOp {
            /// The wasm name, as it appears in the text format.
            #[must_use]
            pub fn name(self) -> &'static str {
                match self {
                    $(Self::$variant => stringify!($variant),)*
                }
            }

            /// What it pops, deepest operand first.
            #[must_use]
            pub fn operands(self) -> &'static [ValType] {
                match self {
                    $(Self::$variant => &[$(ValType::$operand),*],)*
                }
            }

            /// What it pushes. Every numeric op pushes exactly one value.
            #[must_use]
            pub fn result(self) -> ValType {
                match self {
                    $(Self::$variant => num_ops!(@ty $result),)*
                }
            }

            /// The Rust expression, with `{n}` standing for operand `n`.
            #[must_use]
            pub fn template(self) -> &'static str {
                match self {
                    $(Self::$variant => $template,)*
                }
            }

            /// Every op, for tests that must not miss one.
            pub const ALL: &'static [NumOp] = &[$(NumOp::$variant),*];
        }

        /// Recognises the numeric operators. Returns `None` for everything
        /// else, which the caller reports as unsupported rather than skipping.
        #[must_use]
        pub fn lower(operator: &wasmparser::Operator<'_>) -> Option<NumOp> {
            match operator {
                $(wasmparser::Operator::$variant => Some(NumOp::$variant),)*
                _ => None,
            }
        }
    };
    (@ty I32) => { ValType::I32 };
    (@ty I64) => { ValType::I64 };
    (@ty F32) => { ValType::F32 };
    (@ty F64) => { ValType::F64 };
}

num_ops! {
    // ---- i32 comparison. Result is 0 or 1, typed i32. ----
    I32Eqz,  [I32]      -> I32, "(({0} == 0) as i32)";
    I32Eq,   [I32, I32] -> I32, "(({0} == {1}) as i32)";
    I32Ne,   [I32, I32] -> I32, "(({0} != {1}) as i32)";
    I32LtS,  [I32, I32] -> I32, "(({0} < {1}) as i32)";
    I32LtU,  [I32, I32] -> I32, "((({0} as u32) < ({1} as u32)) as i32)";
    I32GtS,  [I32, I32] -> I32, "(({0} > {1}) as i32)";
    I32GtU,  [I32, I32] -> I32, "((({0} as u32) > ({1} as u32)) as i32)";
    I32LeS,  [I32, I32] -> I32, "(({0} <= {1}) as i32)";
    I32LeU,  [I32, I32] -> I32, "((({0} as u32) <= ({1} as u32)) as i32)";
    I32GeS,  [I32, I32] -> I32, "(({0} >= {1}) as i32)";
    I32GeU,  [I32, I32] -> I32, "((({0} as u32) >= ({1} as u32)) as i32)";

    // ---- i32 arithmetic. Wrapping, because wasm has no overflow trap. ----
    I32Clz,    [I32]      -> I32, "({0}.leading_zeros() as i32)";
    I32Ctz,    [I32]      -> I32, "({0}.trailing_zeros() as i32)";
    I32Popcnt, [I32]      -> I32, "({0}.count_ones() as i32)";
    I32Add,    [I32, I32] -> I32, "{0}.wrapping_add({1})";
    I32Sub,    [I32, I32] -> I32, "{0}.wrapping_sub({1})";
    I32Mul,    [I32, I32] -> I32, "{0}.wrapping_mul({1})";
    I32DivS,   [I32, I32] -> I32, "rt::i32_div_s({0}, {1})";
    I32DivU,   [I32, I32] -> I32, "rt::i32_div_u({0}, {1})";
    I32RemS,   [I32, I32] -> I32, "rt::i32_rem_s({0}, {1})";
    I32RemU,   [I32, I32] -> I32, "rt::i32_rem_u({0}, {1})";
    I32And,    [I32, I32] -> I32, "({0} & {1})";
    I32Or,     [I32, I32] -> I32, "({0} | {1})";
    I32Xor,    [I32, I32] -> I32, "({0} ^ {1})";
    // The mask is the difference between wasm's defined behaviour and a Rust
    // panic: `i32.shl` by 32 is a shift by 0, not an error.
    I32Shl,    [I32, I32] -> I32, "({0} << ({1} & 31))";
    I32ShrS,   [I32, I32] -> I32, "({0} >> ({1} & 31))";
    I32ShrU,   [I32, I32] -> I32, "((({0} as u32) >> ({1} as u32 & 31)) as i32)";
    I32Rotl,   [I32, I32] -> I32, "{0}.rotate_left({1} as u32 & 31)";
    I32Rotr,   [I32, I32] -> I32, "{0}.rotate_right({1} as u32 & 31)";

    // ---- i64 comparison ----
    I64Eqz,  [I64]      -> I32, "(({0} == 0) as i32)";
    I64Eq,   [I64, I64] -> I32, "(({0} == {1}) as i32)";
    I64Ne,   [I64, I64] -> I32, "(({0} != {1}) as i32)";
    I64LtS,  [I64, I64] -> I32, "(({0} < {1}) as i32)";
    I64LtU,  [I64, I64] -> I32, "((({0} as u64) < ({1} as u64)) as i32)";
    I64GtS,  [I64, I64] -> I32, "(({0} > {1}) as i32)";
    I64GtU,  [I64, I64] -> I32, "((({0} as u64) > ({1} as u64)) as i32)";
    I64LeS,  [I64, I64] -> I32, "(({0} <= {1}) as i32)";
    I64LeU,  [I64, I64] -> I32, "((({0} as u64) <= ({1} as u64)) as i32)";
    I64GeS,  [I64, I64] -> I32, "(({0} >= {1}) as i32)";
    I64GeU,  [I64, I64] -> I32, "((({0} as u64) >= ({1} as u64)) as i32)";

    // ---- i64 arithmetic ----
    I64Clz,    [I64]      -> I64, "({0}.leading_zeros() as i64)";
    I64Ctz,    [I64]      -> I64, "({0}.trailing_zeros() as i64)";
    I64Popcnt, [I64]      -> I64, "({0}.count_ones() as i64)";
    I64Add,    [I64, I64] -> I64, "{0}.wrapping_add({1})";
    I64Sub,    [I64, I64] -> I64, "{0}.wrapping_sub({1})";
    I64Mul,    [I64, I64] -> I64, "{0}.wrapping_mul({1})";
    I64DivS,   [I64, I64] -> I64, "rt::i64_div_s({0}, {1})";
    I64DivU,   [I64, I64] -> I64, "rt::i64_div_u({0}, {1})";
    I64RemS,   [I64, I64] -> I64, "rt::i64_rem_s({0}, {1})";
    I64RemU,   [I64, I64] -> I64, "rt::i64_rem_u({0}, {1})";
    I64And,    [I64, I64] -> I64, "({0} & {1})";
    I64Or,     [I64, I64] -> I64, "({0} | {1})";
    I64Xor,    [I64, I64] -> I64, "({0} ^ {1})";
    I64Shl,    [I64, I64] -> I64, "({0} << ({1} & 63))";
    I64ShrS,   [I64, I64] -> I64, "({0} >> ({1} & 63))";
    I64ShrU,   [I64, I64] -> I64, "((({0} as u64) >> ({1} as u64 & 63)) as i64)";
    I64Rotl,   [I64, I64] -> I64, "{0}.rotate_left({1} as u64 as u32 & 63)";
    I64Rotr,   [I64, I64] -> I64, "{0}.rotate_right({1} as u64 as u32 & 63)";

    // ---- f32 ----
    F32Eq, [F32, F32] -> I32, "(({0} == {1}) as i32)";
    F32Ne, [F32, F32] -> I32, "(({0} != {1}) as i32)";
    F32Lt, [F32, F32] -> I32, "(({0} < {1}) as i32)";
    F32Gt, [F32, F32] -> I32, "(({0} > {1}) as i32)";
    F32Le, [F32, F32] -> I32, "(({0} <= {1}) as i32)";
    F32Ge, [F32, F32] -> I32, "(({0} >= {1}) as i32)";
    F32Abs,      [F32]      -> F32, "{0}.abs()";
    F32Neg,      [F32]      -> F32, "(-{0})";
    F32Ceil,     [F32]      -> F32, "{0}.ceil()";
    F32Floor,    [F32]      -> F32, "{0}.floor()";
    F32Trunc,    [F32]      -> F32, "{0}.trunc()";
    F32Nearest,  [F32]      -> F32, "rt::f32_nearest({0})";
    F32Sqrt,     [F32]      -> F32, "{0}.sqrt()";
    F32Add,      [F32, F32] -> F32, "({0} + {1})";
    F32Sub,      [F32, F32] -> F32, "({0} - {1})";
    F32Mul,      [F32, F32] -> F32, "({0} * {1})";
    F32Div,      [F32, F32] -> F32, "({0} / {1})";
    F32Min,      [F32, F32] -> F32, "rt::f32_min({0}, {1})";
    F32Max,      [F32, F32] -> F32, "rt::f32_max({0}, {1})";
    F32Copysign, [F32, F32] -> F32, "{0}.copysign({1})";

    // ---- f64 ----
    F64Eq, [F64, F64] -> I32, "(({0} == {1}) as i32)";
    F64Ne, [F64, F64] -> I32, "(({0} != {1}) as i32)";
    F64Lt, [F64, F64] -> I32, "(({0} < {1}) as i32)";
    F64Gt, [F64, F64] -> I32, "(({0} > {1}) as i32)";
    F64Le, [F64, F64] -> I32, "(({0} <= {1}) as i32)";
    F64Ge, [F64, F64] -> I32, "(({0} >= {1}) as i32)";
    F64Abs,      [F64]      -> F64, "{0}.abs()";
    F64Neg,      [F64]      -> F64, "(-{0})";
    F64Ceil,     [F64]      -> F64, "{0}.ceil()";
    F64Floor,    [F64]      -> F64, "{0}.floor()";
    F64Trunc,    [F64]      -> F64, "{0}.trunc()";
    F64Nearest,  [F64]      -> F64, "rt::f64_nearest({0})";
    F64Sqrt,     [F64]      -> F64, "{0}.sqrt()";
    F64Add,      [F64, F64] -> F64, "({0} + {1})";
    F64Sub,      [F64, F64] -> F64, "({0} - {1})";
    F64Mul,      [F64, F64] -> F64, "({0} * {1})";
    F64Div,      [F64, F64] -> F64, "({0} / {1})";
    F64Min,      [F64, F64] -> F64, "rt::f64_min({0}, {1})";
    F64Max,      [F64, F64] -> F64, "rt::f64_max({0}, {1})";
    F64Copysign, [F64, F64] -> F64, "{0}.copysign({1})";

    // ---- conversions. The trapping ones go through rt; `as` would saturate. ----
    I32WrapI64,        [I64] -> I32, "({0} as i32)";
    I32TruncF32S,      [F32] -> I32, "rt::i32_trunc_f32_s({0})";
    I32TruncF32U,      [F32] -> I32, "rt::i32_trunc_f32_u({0})";
    I32TruncF64S,      [F64] -> I32, "rt::i32_trunc_f64_s({0})";
    I32TruncF64U,      [F64] -> I32, "rt::i32_trunc_f64_u({0})";
    I64ExtendI32S,     [I32] -> I64, "({0} as i64)";
    I64ExtendI32U,     [I32] -> I64, "({0} as u32 as i64)";
    I64TruncF32S,      [F32] -> I64, "rt::i64_trunc_f32_s({0})";
    I64TruncF32U,      [F32] -> I64, "rt::i64_trunc_f32_u({0})";
    I64TruncF64S,      [F64] -> I64, "rt::i64_trunc_f64_s({0})";
    I64TruncF64U,      [F64] -> I64, "rt::i64_trunc_f64_u({0})";
    F32ConvertI32S,    [I32] -> F32, "({0} as f32)";
    F32ConvertI32U,    [I32] -> F32, "({0} as u32 as f32)";
    F32ConvertI64S,    [I64] -> F32, "({0} as f32)";
    F32ConvertI64U,    [I64] -> F32, "({0} as u64 as f32)";
    F32DemoteF64,      [F64] -> F32, "({0} as f32)";
    F64ConvertI32S,    [I32] -> F64, "({0} as f64)";
    F64ConvertI32U,    [I32] -> F64, "({0} as u32 as f64)";
    F64ConvertI64S,    [I64] -> F64, "({0} as f64)";
    F64ConvertI64U,    [I64] -> F64, "({0} as u64 as f64)";
    F64PromoteF32,     [F32] -> F64, "({0} as f64)";
    I32ReinterpretF32, [F32] -> I32, "({0}.to_bits() as i32)";
    I64ReinterpretF64, [F64] -> I64, "({0}.to_bits() as i64)";
    F32ReinterpretI32, [I32] -> F32, "f32::from_bits({0} as u32)";
    F64ReinterpretI64, [I64] -> F64, "f64::from_bits({0} as u64)";

    // ---- sign extension ----
    I32Extend8S,  [I32] -> I32, "({0} as i8 as i32)";
    I32Extend16S, [I32] -> I32, "({0} as i16 as i32)";
    I64Extend8S,  [I64] -> I64, "({0} as i8 as i64)";
    I64Extend16S, [I64] -> I64, "({0} as i16 as i64)";
    I64Extend32S, [I64] -> I64, "({0} as i32 as i64)";

    // ---- saturating truncation. Rust's `as` is already saturating with
    // NaN mapping to zero, which is exactly this proposal's semantics. ----
    I32TruncSatF32S, [F32] -> I32, "({0} as i32)";
    I32TruncSatF32U, [F32] -> I32, "({0} as u32 as i32)";
    I32TruncSatF64S, [F64] -> I32, "({0} as i32)";
    I32TruncSatF64U, [F64] -> I32, "({0} as u32 as i32)";
    I64TruncSatF32S, [F32] -> I64, "({0} as i64)";
    I64TruncSatF32U, [F32] -> I64, "({0} as u64 as i64)";
    I64TruncSatF64S, [F64] -> I64, "({0} as i64)";
    I64TruncSatF64U, [F64] -> I64, "({0} as u64 as i64)";
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_substitutes_operands_in_order() {
        assert_eq!(
            render("rt::i32_div_s({0}, {1})", &["a".into(), "b".into()]),
            "rt::i32_div_s(a, b)"
        );
        assert_eq!(render("{0}.abs()", &["x".into()]), "x.abs()");
        // A template may use an operand more than once, and none at all.
        assert_eq!(render("({0} + {0})", &["y".into()]), "(y + y)");
        assert_eq!(render("0", &[]), "0");
    }

    #[test]
    fn lowering_maps_the_operator_of_the_same_name() {
        assert_eq!(lower(&wasmparser::Operator::I32Add), Some(NumOp::I32Add));
        assert_eq!(
            lower(&wasmparser::Operator::F64Copysign),
            Some(NumOp::F64Copysign)
        );
        // Control flow is not a numeric op, and must not be silently accepted.
        assert_eq!(lower(&wasmparser::Operator::Return), None);
        assert_eq!(lower(&wasmparser::Operator::Nop), None);
    }

    #[test]
    fn every_op_has_a_template_matching_its_arity() {
        for &op in NumOp::ALL {
            let arity = op.operands().len();
            assert!(
                (1..=2).contains(&arity),
                "{} has arity {arity}, which no numeric op has",
                op.name()
            );
            for index in 0..arity {
                assert!(
                    op.template().contains(&format!("{{{index}}}")),
                    "{} never uses operand {index}",
                    op.name()
                );
            }
            // A placeholder past the arity would render out of bounds.
            assert!(
                !op.template().contains(&format!("{{{arity}}}")),
                "{} uses an operand it does not pop",
                op.name()
            );
        }
    }

    #[test]
    fn names_are_unique() {
        let mut names: Vec<&str> = NumOp::ALL.iter().map(|op| op.name()).collect();
        let total = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), total, "two ops share a name");
    }

    #[test]
    fn unsigned_operations_cast_before_they_compute() {
        // The bug this guards is emitting a signed shift or comparison for an
        // unsigned opcode: it agrees with wasm on small numbers and disagrees
        // on every value with the high bit set.
        for &op in NumOp::ALL {
            if !op.name().ends_with('U') || op.name().contains("Trunc") {
                continue;
            }
            let template = op.template();
            assert!(
                template.contains("as u32")
                    || template.contains("as u64")
                    // `i32.div_u` and friends cast inside the runtime, which is
                    // where the trap check has to live anyway.
                    || template.starts_with("rt::"),
                "{} is unsigned but neither casts nor delegates",
                op.name()
            );
        }
    }

    #[test]
    fn shifts_mask_their_count() {
        for &op in NumOp::ALL {
            let name = op.name();
            if name.contains("Shl") || name.contains("Shr") || name.contains("Rot") {
                let width = if name.starts_with("I32") { "31" } else { "63" };
                assert!(
                    op.template().contains(width),
                    "{name} does not mask its shift count"
                );
            }
        }
    }

    #[test]
    fn anything_that_can_trap_goes_through_the_runtime() {
        for &op in NumOp::ALL {
            let name = op.name();
            // A float→int truncation traps on NaN and out of range. `f32.trunc`
            // also contains "Trunc" and traps at nothing: it rounds a float to a
            // float. The distinction is which type it produces.
            let float_to_int = (name.starts_with("I32Trunc") || name.starts_with("I64Trunc"))
                && !name.contains("Sat");
            // Integer division traps on zero; float division gives an infinity
            // and carries on, so `f32.div` belongs to the operator, not the
            // runtime.
            let integer = name.starts_with("I32") || name.starts_with("I64");
            let trapping =
                (integer && (name.contains("Div") || name.contains("Rem"))) || float_to_int;
            if trapping {
                assert!(
                    op.template().starts_with("rt::"),
                    "{name} can trap but does not call the runtime"
                );
            }
        }
    }
}
