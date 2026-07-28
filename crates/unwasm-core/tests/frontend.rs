//! What the decoder accepts, and what it refuses to guess at.
//!
//! The refusals matter as much as the acceptances. Every case below that ends
//! in an error is a construct with no faithful Rust form at this level, and the
//! test exists to pin that it comes back named rather than quietly dropped.

mod common;

use unwasm_core::module::{ConstExpr, ExportKind, LoadKind, Op, StoreKind};
use unwasm_core::{Error, Module, ValType};

fn parse(name: &str, wat: &str) -> Result<Module, Error> {
    Module::parse(&common::assemble(name, wat))
}

fn unsupported(name: &str, wat: &str) -> String {
    match parse(name, wat) {
        Err(error @ Error::Unsupported { .. }) => error.to_string(),
        Err(other) => panic!("expected an unsupported-construct error, got: {other}"),
        Ok(_) => panic!("expected {name} to be refused, but it parsed"),
    }
}

#[test]
fn an_empty_module_parses_to_nothing() {
    let module = parse("empty", "(module)").expect("an empty module is valid");
    assert!(module.funcs.is_empty());
    assert!(module.exports.is_empty());
    assert!(module.memory.is_none());
    assert!(module.start.is_none());
}

#[test]
fn functions_carry_their_signature_locals_and_body() {
    let module = parse(
        "shape",
        "(module (func (export \"f\") (param i32 i64) (result f64)
            (local f32) (local i32 i32)
            f64.const 1.5))",
    )
    .expect("valid");
    assert_eq!(module.funcs.len(), 1);
    let func = &module.funcs[0];
    assert_eq!(
        func.locals,
        vec![ValType::F32, ValType::I32, ValType::I32],
        "locals are flattened out of their run-length encoding"
    );
    let ty = module.func_type(0).expect("the function has a type");
    assert_eq!(ty.params, vec![ValType::I32, ValType::I64]);
    assert_eq!(ty.results, vec![ValType::F64]);
    // The trailing `end` belongs to the function, not to the body.
    assert_eq!(func.body.len(), 1, "{:?}", func.body);
}

#[test]
fn imported_functions_take_the_low_indices() {
    let module = parse(
        "imports",
        "(module
            (import \"env\" \"first\" (func (param i32)))
            (import \"other\" \"second\" (func (result i32)))
            (func (export \"own\") (result i32) i32.const 1))",
    )
    .expect("valid");
    assert_eq!(module.func_imports.len(), 2);
    assert_eq!(module.func_imports[0].module, "env");
    assert_eq!(module.func_imports[1].field, "second");
    // Index 2 is the first defined function, after the two imports.
    assert_eq!(
        module.func_type(2).expect("defined").results,
        vec![ValType::I32]
    );
    assert!(
        module.func_type(9).is_none(),
        "a missing index is None, not a panic"
    );
}

#[test]
fn the_name_section_is_read_when_present() {
    // wasm-tools keeps `$names` in the name section.
    let module = parse(
        "names",
        "(module (func $add_two (param i32) (result i32) local.get 0))",
    )
    .expect("valid");
    assert_eq!(module.func_name(0), Some("add_two"));
    assert_eq!(module.func_name(7), None);
}

#[test]
fn globals_data_and_element_segments_are_collected() {
    let module = parse(
        "sections",
        "(module
            (memory 1)
            (table 4 funcref)
            (global (mut i32) (i32.const 42))
            (global f64 (f64.const 0.5))
            (global i64 (i64.const -1))
            (global f32 (f32.const 1))
            (data (i32.const 8) \"abc\")
            (data \"passive\")
            (func $target)
            (elem (i32.const 1) $target $target))",
    )
    .expect("valid");

    assert_eq!(module.globals.len(), 4);
    assert!(module.globals[0].mutable);
    assert_eq!(module.globals[0].init, ConstExpr::I32(42));
    assert_eq!(module.globals[1].init, ConstExpr::F64(0.5));
    assert_eq!(module.globals[2].init, ConstExpr::I64(-1));
    assert_eq!(module.globals[3].init, ConstExpr::F32(1.0));

    assert_eq!(module.datas.len(), 2);
    assert_eq!(module.datas[0].offset, Some(ConstExpr::I32(8)));
    assert_eq!(module.datas[0].bytes, b"abc");
    assert_eq!(
        module.datas[1].offset, None,
        "a passive segment has no offset"
    );

    assert_eq!(module.elems.len(), 1);
    assert_eq!(module.elems[0].offset, Some(ConstExpr::I32(1)));
    assert_eq!(module.elems[0].funcs, vec![0, 0]);

    assert_eq!(module.memory.expect("declared").min_pages, 1);
}

#[test]
fn an_element_segment_written_as_expressions_reads_the_same() {
    let module = parse(
        "elem-expr",
        "(module (table 2 funcref) (func $f) (elem (i32.const 0) funcref (ref.func $f)))",
    )
    .expect("valid");
    assert_eq!(module.elems[0].funcs, vec![0]);
}

#[test]
fn every_export_kind_is_recorded() {
    let module = parse(
        "exports",
        "(module
            (memory (export \"mem\") 1 4)
            (table (export \"tab\") 1 funcref)
            (global (export \"glob\") i32 (i32.const 0))
            (func (export \"fun\")))",
    )
    .expect("valid");
    let kinds: Vec<ExportKind> = module.exports.iter().map(|export| export.kind).collect();
    assert!(kinds.contains(&ExportKind::Memory));
    assert!(kinds.contains(&ExportKind::Table));
    assert!(kinds.contains(&ExportKind::Global));
    assert!(kinds.contains(&ExportKind::Func));
    assert_eq!(module.memory.expect("declared").max_pages, Some(4));
}

#[test]
fn the_start_function_is_recorded() {
    let module = parse("start", "(module (func $init) (start $init))").expect("valid");
    assert_eq!(module.start, Some(0));
}

#[test]
fn loads_and_stores_keep_their_width_and_offset() {
    let module = parse(
        "memops",
        "(module (memory 1)
            (func (param i32)
                local.get 0 i32.load8_s offset=3 drop
                local.get 0 i64.load32_u offset=7 drop
                local.get 0 f32.load drop
                local.get 0 i64.const 1 i64.store16 offset=9))",
    )
    .expect("valid");
    let body = &module.funcs[0].body;
    assert!(body.iter().any(|op| matches!(
        op,
        Op::Load { kind: LoadKind::I32Load8S, mem } if mem.offset == 3
    )));
    assert!(body.iter().any(|op| matches!(
        op,
        Op::Load { kind: LoadKind::I64Load32U, mem } if mem.offset == 7
    )));
    assert!(body.iter().any(|op| matches!(
        op,
        Op::Load {
            kind: LoadKind::F32,
            ..
        }
    )));
    assert!(body.iter().any(|op| matches!(
        op,
        Op::Store { kind: StoreKind::I64Store16, mem } if mem.offset == 9
    )));
}

#[test]
fn a_typed_select_reads_as_a_select() {
    let module = parse(
        "typed-select",
        "(module (func (param i32) (result i32)
            local.get 0 local.get 0 local.get 0 (select (result i32))))",
    )
    .expect("valid");
    assert!(module.funcs[0].body.contains(&Op::Select));
}

#[test]
fn passive_segments_and_their_instructions_survive() {
    let module = parse(
        "passive",
        "(module (memory 1) (data $seg \"xyz\")
            (func (memory.init $seg (i32.const 0) (i32.const 0) (i32.const 3)) (data.drop $seg)))",
    )
    .expect("valid");
    assert!(module.funcs[0].body.contains(&Op::MemoryInit(0)));
    assert!(module.funcs[0].body.contains(&Op::DataDrop(0)));
}

// ---- the refusals ----

#[test]
fn simd_is_refused_by_name() {
    let message = unsupported(
        "simd",
        "(module (func (result v128) v128.const i32x4 0 0 0 0))",
    );
    assert!(message.contains("v128"), "{message}");
}

#[test]
fn reference_types_are_refused() {
    let message = unsupported("reftypes", "(module (func (result funcref) ref.null func))");
    assert!(message.contains("reference type"), "{message}");
}

#[test]
fn an_imported_memory_is_read_with_the_limits_it_declares() {
    // What the 9.4 MiB VoIP module does: the host supplies the memory. The
    // limits are still the module's, and they are what the generated code
    // instantiates with.
    let module = parse(
        "import-mem",
        "(module (import \"env\" \"memory\" (memory 160 32768 shared)))",
    )
    .expect("valid");
    let memory = module.memory.expect("declared by the import");
    assert_eq!(memory.min_pages, 160);
    assert_eq!(memory.max_pages, Some(32768));
    assert!(memory.shared);
    assert_eq!(
        memory.imported,
        Some(("env".to_string(), "memory".to_string()))
    );
}

#[test]
fn an_imported_global_or_table_is_refused() {
    let global = unsupported(
        "import-global",
        "(module (import \"env\" \"g\" (global i32)))",
    );
    assert!(global.contains("imported global"), "{global}");
    let table = unsupported(
        "import-table",
        "(module (import \"env\" \"t\" (table 1 funcref)))",
    );
    assert!(table.contains("imported table"), "{table}");
}

#[test]
fn a_shared_memory_is_read_and_marked_as_shared() {
    // Shared says the module was built for threads. The decompilation runs
    // one — a limit on the execution, not a reason to refuse to read the code.
    let module = parse("shared", "(module (memory 1 1 shared))").expect("valid");
    let memory = module.memory.expect("declared");
    assert!(memory.shared);
    assert!(memory.imported.is_none());
}

#[test]
fn a_sixty_four_bit_memory_is_refused() {
    let message = unsupported("mem64", "(module (memory i64 1))");
    assert!(message.contains("64-bit memory"), "{message}");
}

#[test]
fn several_memories_are_refused() {
    let message = unsupported("multi-mem", "(module (memory 1) (memory 1))");
    assert!(message.contains("multiple memories"), "{message}");
}

#[test]
fn exception_handling_is_refused() {
    let message = unsupported(
        "tags",
        "(module (tag $oops (param i32)) (func (throw $oops (i32.const 1))))",
    );
    assert!(!message.is_empty(), "the refusal names something");
}

#[test]
fn every_atomic_instruction_is_recognised() {
    // Sixty-four opcodes, and the module that needs them is the VoIP one.
    let module = parse(
        "atomics",
        r#"(module (memory 1 1 shared)
            (func (param i32) (result i32)
                local.get 0 i32.atomic.load
                local.get 0 i32.atomic.load8_u i32.add
                local.get 0 i32.atomic.load16_u i32.add
                local.get 0 local.get 0 i32.atomic.store
                local.get 0 local.get 0 i32.atomic.store8
                local.get 0 i32.const 1 i32.atomic.rmw.add i32.add
                local.get 0 i32.const 1 i32.atomic.rmw.sub i32.add
                local.get 0 i32.const 1 i32.atomic.rmw.and i32.add
                local.get 0 i32.const 1 i32.atomic.rmw.or i32.add
                local.get 0 i32.const 1 i32.atomic.rmw.xor i32.add
                local.get 0 i32.const 1 i32.atomic.rmw.xchg i32.add
                local.get 0 i32.const 1 i32.const 2 i32.atomic.rmw.cmpxchg i32.add
                local.get 0 i32.const 1 i32.const 2 i32.atomic.rmw8.cmpxchg_u i32.add
                local.get 0 i64.atomic.load i32.wrap_i64 i32.add
                local.get 0 i64.const 1 i64.atomic.rmw32.add_u i32.wrap_i64 i32.add
                local.get 0 i32.const 1 i64.const 0 memory.atomic.wait32 i32.add
                local.get 0 i64.const 1 i64.const 0 memory.atomic.wait64 i32.add
                local.get 0 i32.const 1 memory.atomic.notify i32.add
                atomic.fence))"#,
    )
    .expect("valid");
    let body = &module.funcs[0].body;
    let atomics = body
        .iter()
        .filter(|op| matches!(op, Op::Atomic { .. }))
        .count();
    assert_eq!(atomics, 18, "{body:?}");
    assert!(body.contains(&Op::AtomicFence));
}

#[test]
fn random_bytes_fail_to_decode() {
    let error = Module::parse(b"\0asm not really").expect_err("not a module");
    assert!(matches!(error, Error::Decode(_)));
}

#[test]
fn a_truncated_module_fails_to_decode() {
    let bytes = common::assemble("truncate", "(module (func (result i32) i32.const 1))");
    let error = Module::parse(&bytes[..bytes.len() - 2]).expect_err("truncated");
    assert!(matches!(error, Error::Decode(_)));
}

#[test]
fn an_imported_or_exported_tag_is_refused() {
    let imported = unsupported(
        "tag-import",
        "(module (import \"env\" \"e\" (tag (param i32))))",
    );
    assert!(imported.contains("imported tag"), "{imported}");
    let exported = unsupported(
        "tag-export",
        "(module (tag $e (param i32)) (export \"e\" (tag $e)))",
    );
    assert!(exported.contains("exported tag"), "{exported}");
}

#[test]
fn a_struct_or_array_type_is_refused() {
    let message = unsupported("gc-type", "(module (type (struct (field i32))))");
    assert!(message.contains("non-function type"), "{message}");
}

#[test]
fn a_declared_element_segment_places_nothing() {
    // `elem declare` exists to make a function referencable by `ref.func`; it
    // puts nothing in the table, so it has no offset and contributes no slots.
    let module =
        parse("declared-only", "(module (func $f) (elem declare func $f))").expect("valid");
    assert_eq!(module.elems.len(), 1);
    assert_eq!(module.elems[0].offset, None);
}

#[test]
fn a_module_with_both_an_imported_and_a_declared_memory_is_refused() {
    // Legal only under the multi-memory proposal, and there is no single
    // `self.memory` for it to mean.
    let message = unsupported(
        "both-memories",
        "(module (import \"env\" \"memory\" (memory 1)) (memory 1))",
    );
    assert!(message.contains("multiple memories"), "{message}");
}

#[test]
fn an_imported_sixty_four_bit_memory_is_refused_like_a_declared_one() {
    let message = unsupported(
        "import-mem64",
        "(module (import \"env\" \"memory\" (memory i64 1)))",
    );
    assert!(message.contains("64-bit memory"), "{message}");
}
