//! What the module says about itself.
//!
//! Level 0 translates; this reads. Nothing here changes what the generated code
//! does — it changes what a person can tell from looking at it, which on a
//! minified module is the difference between `self.g0` and `the C stack
//! pointer`, and between `i32.const 211967` and the panic message it addresses.
//!
//! Every answer carries its evidence. A decompiler that says "this is the stack
//! pointer" without saying why has asked the reader to trust it, and the reader
//! has no way to check. So [`StackPointer`] records how it was found and how
//! many times the pattern appeared, and anything with no evidence comes back as
//! `None` rather than as the most likely index.

use crate::module::{ConstExpr, ExportKind, Module, Op};

/// How the C stack pointer was identified.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Evidence {
    /// The module exports it under the name the linker gives it. Conclusive,
    /// and present in unstripped builds.
    Exported,
    /// Found by its use: the function prologue that reserves a frame.
    ///
    /// ```wat
    /// global.get $sp
    /// i32.const 32
    /// i32.sub
    /// global.set $sp     ;; and usually local.tee first
    /// ```
    Prologue {
        /// How many functions open with it. One is a coincidence; hundreds are
        /// a calling convention.
        functions: usize,
    },
}

/// The global the C stack lives in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StackPointer {
    /// Its index in the global index space.
    pub global: u32,
    /// Why we believe it.
    pub evidence: Evidence,
}

/// What could be read out of a module.
#[derive(Debug, Clone, Default)]
pub struct Analysis {
    /// The C stack pointer, if the module gave a reason to name one.
    pub stack_pointer: Option<StackPointer>,
}

/// Reads a module for the things worth naming.
#[must_use]
pub fn analyse(module: &Module) -> Analysis {
    Analysis {
        stack_pointer: find_stack_pointer(module),
    }
}

/// The names a linker gives the stack pointer when it keeps names at all.
const STACK_POINTER_NAMES: &[&str] = &["__stack_pointer", "_stack_pointer", "stackPointer"];

fn find_stack_pointer(module: &Module) -> Option<StackPointer> {
    // An exported name settles it without any guessing.
    for export in &module.exports {
        if export.kind == ExportKind::Global && STACK_POINTER_NAMES.contains(&export.name.as_str())
        {
            return Some(StackPointer {
                global: export.index,
                evidence: Evidence::Exported,
            });
        }
    }

    // Otherwise, count prologues. A minified module keeps no names, but it
    // still has to reserve stack frames, and only one global is used that way.
    let mut counts = vec![0usize; module.globals.len()];
    for func in &module.funcs {
        if let Some(global) = prologue_global(&func.body)
            && let Some(count) = counts.get_mut(global as usize)
        {
            *count += 1;
        }
    }

    let (global, functions) = counts
        .iter()
        .enumerate()
        .max_by_key(|(_, count)| **count)
        .map(|(index, count)| (index as u32, *count))?;

    // One function reserving a frame proves nothing: a module with a single
    // arithmetic helper can match by accident. Two is the smallest number that
    // is a convention rather than a coincidence.
    if functions < 2 {
        return None;
    }
    // A stack pointer is written to. An immutable global that happens to be
    // read in the same shape is something else.
    if !module.globals.get(global as usize)?.mutable {
        return None;
    }
    Some(StackPointer {
        global,
        evidence: Evidence::Prologue { functions },
    })
}

/// Recognises `global.get G; i32.const N; i32.sub; ...; global.set G` at the
/// start of a function, and returns `G`.
///
/// The instructions between the subtraction and the store vary — clang emits a
/// `local.tee` to keep the frame address, and at `-O0` there is often nothing
/// at all — so the shape is matched at its two ends rather than exactly.
fn prologue_global(body: &[Op]) -> Option<u32> {
    let mut ops = body.iter();
    let global = match ops.next()? {
        Op::GlobalGet(index) => *index,
        _ => return None,
    };
    match ops.next()? {
        Op::I32Const(size) if *size > 0 => size,
        _ => return None,
    };
    if !matches!(ops.next()?, Op::Num(op) if op.name() == "I32Sub") {
        return None;
    }
    // The store back must be to the same global, and must come before anything
    // that could be a different function's business.
    for op in ops.take(3) {
        match op {
            Op::GlobalSet(index) if *index == global => return Some(global),
            Op::LocalTee(_) | Op::LocalSet(_) => {}
            _ => return None,
        }
    }
    None
}

/// The NUL-terminated text at an address, if a data segment puts text there.
///
/// This is what makes a minified module readable at all: the constants that
/// address static strings are often the only names left in it. The oracle work
/// in wa-wasm-oracle found mozjpeg's calling convention this way — a constant
/// resolved to `called \`Option::unwrap()\` on a \`None\` value`, and the panic
/// named the function.
///
/// Returns `None` unless the bytes really look like text: printable ASCII,
/// NUL-terminated, and long enough that it is not three bytes of a struct that
/// happen to be letters.
#[must_use]
pub fn static_text(module: &Module, address: i32) -> Option<String> {
    static_text_inner(module, address, None)
}

/// The text of a known length at an address.
///
/// Rust's strings carry a length instead of a terminator, so reading to the
/// next NUL runs straight through the next string and the one after it — which
/// is why an unbounded read of a Rust module returns things like
/// `"0123456789abcdefcalled \`Option::unwrap()\`…"`. The length is right there
/// in the code, though: a `&str` is passed as `i32.const ptr; i32.const len`,
/// so the instruction after the address says where the string stops.
///
/// Returns `None` if the length does not describe printable text, which is what
/// happens when the second constant was never a length at all.
#[must_use]
pub fn static_text_of_length(module: &Module, address: i32, length: u32) -> Option<String> {
    // A length of zero is an empty string, and one past a few hundred is not a
    // message being passed to something.
    if length == 0 || length > 512 {
        return None;
    }
    static_text_inner(module, address, Some(length as usize))
}

fn static_text_inner(module: &Module, address: i32, length: Option<usize>) -> Option<String> {
    const SHORTEST: usize = 4;
    const LONGEST: usize = 120;

    let address = address as u32;
    for segment in &module.datas {
        let Some(ConstExpr::I32(base)) = segment.offset else {
            continue;
        };
        let base = base as u32;
        if address < base {
            continue;
        }
        let at = (address - base) as usize;
        if at >= segment.bytes.len() {
            continue;
        }

        // With a length, the slice is exactly that long and needs no
        // terminator; without one, it runs to the next NUL.
        let available = &segment.bytes[at..];
        let slice = match length {
            Some(length) if length <= available.len() => &available[..length],
            Some(_) => return None,
            None => available,
        };

        let mut text = String::new();
        for &byte in slice {
            if byte == 0 && length.is_none() {
                return (text.len() >= SHORTEST).then_some(text);
            }
            // Printable ASCII, plus the whitespace that appears in messages.
            // Anything else means these bytes are not a string.
            if !(byte.is_ascii_graphic() || byte == b' ' || byte == b'\n' || byte == b'\t') {
                return None;
            }
            if text.len() == LONGEST {
                text.push('…');
                return Some(text);
            }
            text.push(byte as char);
        }
        return match length {
            // A length said where it ends, so it ended there.
            Some(_) => (text.len() >= SHORTEST).then_some(text),
            // Ran off the end of the segment without a terminator.
            None => None,
        };
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::module::{DataSegment, Func, FuncType, GlobalDef, ValType};
    use crate::ops::NumOp;

    fn module_with_globals(count: usize, mutable: bool) -> Module {
        Module {
            types: vec![FuncType::default()],
            globals: (0..count)
                .map(|_| GlobalDef {
                    ty: ValType::I32,
                    mutable,
                    init: ConstExpr::I32(65536),
                })
                .collect(),
            ..Module::default()
        }
    }

    fn prologue(global: u32) -> Vec<Op> {
        vec![
            Op::GlobalGet(global),
            Op::I32Const(32),
            Op::Num(NumOp::I32Sub),
            Op::LocalTee(0),
            Op::GlobalSet(global),
        ]
    }

    fn with_bodies(mut module: Module, bodies: Vec<Vec<Op>>) -> Module {
        module.funcs = bodies
            .into_iter()
            .map(|body| Func {
                type_index: 0,
                locals: Vec::new(),
                body,
            })
            .collect();
        module
    }

    #[test]
    fn an_exported_name_settles_it() {
        let mut module = module_with_globals(3, true);
        module.exports.push(crate::module::Export {
            name: "__stack_pointer".into(),
            kind: ExportKind::Global,
            index: 2,
        });
        let found = analyse(&module).stack_pointer.expect("named");
        assert_eq!(found.global, 2);
        assert_eq!(found.evidence, Evidence::Exported);
    }

    #[test]
    fn a_global_exported_under_another_name_is_not_taken_for_it() {
        let mut module = module_with_globals(1, true);
        module.exports.push(crate::module::Export {
            name: "__heap_base".into(),
            kind: ExportKind::Global,
            index: 0,
        });
        assert!(analyse(&module).stack_pointer.is_none());
    }

    #[test]
    fn the_prologue_identifies_it_in_a_stripped_module() {
        let module = with_bodies(
            module_with_globals(2, true),
            vec![prologue(1), prologue(1), vec![Op::Nop]],
        );
        let found = analyse(&module).stack_pointer.expect("found by its use");
        assert_eq!(found.global, 1);
        assert_eq!(found.evidence, Evidence::Prologue { functions: 2 });
    }

    #[test]
    fn one_prologue_is_a_coincidence_and_is_not_reported() {
        let module = with_bodies(module_with_globals(1, true), vec![prologue(0)]);
        assert!(
            analyse(&module).stack_pointer.is_none(),
            "a single match is not a calling convention"
        );
    }

    #[test]
    fn an_immutable_global_is_never_the_stack_pointer() {
        // The shape can appear around a constant — `base - 32` — and a stack
        // pointer that cannot be written to is not one.
        let module = with_bodies(
            module_with_globals(1, false),
            vec![prologue(0), prologue(0)],
        );
        assert!(analyse(&module).stack_pointer.is_none());
    }

    #[test]
    fn a_prologue_that_stores_somewhere_else_does_not_count() {
        let body = vec![
            Op::GlobalGet(0),
            Op::I32Const(32),
            Op::Num(NumOp::I32Sub),
            Op::GlobalSet(1),
        ];
        let module = with_bodies(module_with_globals(2, true), vec![body.clone(), body]);
        assert!(analyse(&module).stack_pointer.is_none());
    }

    #[test]
    fn adding_to_a_global_is_an_epilogue_not_a_prologue() {
        let body = vec![
            Op::GlobalGet(0),
            Op::I32Const(32),
            Op::Num(NumOp::I32Add),
            Op::GlobalSet(0),
        ];
        let module = with_bodies(module_with_globals(1, true), vec![body.clone(), body]);
        assert!(analyse(&module).stack_pointer.is_none());
    }

    #[test]
    fn a_module_with_no_globals_reports_nothing() {
        assert!(analyse(&Module::default()).stack_pointer.is_none());
        let module = with_bodies(module_with_globals(0, true), vec![prologue(0)]);
        assert!(analyse(&module).stack_pointer.is_none());
    }

    fn module_with_data(offset: i32, bytes: &[u8]) -> Module {
        Module {
            datas: vec![DataSegment {
                offset: Some(ConstExpr::I32(offset)),
                bytes: bytes.to_vec(),
            }],
            ..Module::default()
        }
    }

    #[test]
    fn a_constant_that_addresses_text_reads_back_as_that_text() {
        let module = module_with_data(1024, b"hello, world\0and more\0");
        assert_eq!(static_text(&module, 1024).as_deref(), Some("hello, world"));
        // Mid-string is still text: it is where a suffix would start.
        assert_eq!(static_text(&module, 1031).as_deref(), Some("world"));
        assert_eq!(static_text(&module, 1037).as_deref(), Some("and more"));
    }

    #[test]
    fn bytes_that_are_not_text_are_not_reported_as_text() {
        // A struct whose first field happens to be small integers.
        let module = module_with_data(0, &[0x01, 0x02, 0x03, 0x04, 0x00]);
        assert_eq!(static_text(&module, 0), None);
        // Text that is too short to be a name rather than a coincidence.
        let module = module_with_data(0, b"ab\0");
        assert_eq!(static_text(&module, 0), None);
        // High bytes: UTF-8 or binary, either way not something to quote raw.
        let module = module_with_data(0, &[b'a', b'b', b'c', 0xE2, 0x82, 0xAC, 0]);
        assert_eq!(static_text(&module, 0), None);
    }

    #[test]
    fn text_without_a_terminator_is_not_reported() {
        let module = module_with_data(0, b"unterminated");
        assert_eq!(static_text(&module, 0), None);
    }

    #[test]
    fn a_very_long_string_is_cut_rather_than_pasted_whole() {
        let long = "x".repeat(300);
        let mut bytes = long.into_bytes();
        bytes.push(0);
        let module = module_with_data(0, &bytes);
        let text = static_text(&module, 0).expect("text");
        assert!(text.ends_with('…'), "{text}");
        assert!(text.chars().count() <= 121);
    }

    #[test]
    fn an_address_outside_every_segment_reads_back_as_nothing() {
        let module = module_with_data(1024, b"hello\0");
        assert_eq!(static_text(&module, 0), None);
        assert_eq!(static_text(&module, 2048), None);
        // Passive segments are not at any address until `memory.init` puts them
        // somewhere, so they cannot answer this question.
        let module = Module {
            datas: vec![DataSegment {
                offset: None,
                bytes: b"hello\0".to_vec(),
            }],
            ..Module::default()
        };
        assert_eq!(static_text(&module, 0), None);
    }
}
