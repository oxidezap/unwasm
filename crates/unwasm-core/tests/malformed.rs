//! Modules that decode but do not add up.
//!
//! wasmparser decodes without validating, so a module can be well-formed bytes
//! and still say impossible things: a function whose type index does not exist,
//! a `call` to function 99 in a module with three, an `i32.add` with nothing on
//! the stack. A real engine rejects those at validation; this crate meets them
//! after decoding, and each one has to come back as a named [`Error::Invalid`]
//! rather than as a panic or, worse, as emitted code.
//!
//! No assembler will produce these, so they are built here byte by byte. The
//! encoder is about eighty lines and covers exactly the sections these tests
//! need — a fixture, not a library.

use unwasm_core::{Error, Module, codegen};

// ---- a very small wasm encoder ----

fn leb_u32(mut value: u32, out: &mut Vec<u8>) {
    loop {
        let mut byte = (value & 0x7F) as u8;
        value >>= 7;
        if value != 0 {
            byte |= 0x80;
        }
        out.push(byte);
        if value == 0 {
            return;
        }
    }
}

fn leb_i32(mut value: i32, out: &mut Vec<u8>) {
    loop {
        let byte = (value & 0x7F) as u8;
        value >>= 7;
        let sign_bit_set = byte & 0x40 != 0;
        if (value == 0 && !sign_bit_set) || (value == -1 && sign_bit_set) {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

fn vector(items: &[Vec<u8>]) -> Vec<u8> {
    let mut out = Vec::new();
    leb_u32(items.len() as u32, &mut out);
    for item in items {
        out.extend_from_slice(item);
    }
    out
}

fn section(id: u8, contents: &[u8]) -> Vec<u8> {
    let mut out = vec![id];
    leb_u32(contents.len() as u32, &mut out);
    out.extend_from_slice(contents);
    out
}

/// Assembles a module from already-encoded sections.
fn module(sections: &[Vec<u8>]) -> Vec<u8> {
    let mut out = vec![0x00, 0x61, 0x73, 0x6D, 0x01, 0x00, 0x00, 0x00];
    for part in sections {
        out.extend_from_slice(part);
    }
    out
}

const I32: u8 = 0x7F;
const END: u8 = 0x0B;

/// `(func (param ..) (result ..))`, encoded.
fn func_type(params: &[u8], results: &[u8]) -> Vec<u8> {
    let mut out = vec![0x60];
    leb_u32(params.len() as u32, &mut out);
    out.extend_from_slice(params);
    leb_u32(results.len() as u32, &mut out);
    out.extend_from_slice(results);
    out
}

fn type_section(types: &[Vec<u8>]) -> Vec<u8> {
    section(1, &vector(types))
}

fn function_section(type_indices: &[u32]) -> Vec<u8> {
    let encoded: Vec<Vec<u8>> = type_indices
        .iter()
        .map(|index| {
            let mut out = Vec::new();
            leb_u32(*index, &mut out);
            out
        })
        .collect();
    section(3, &vector(&encoded))
}

/// A function body with no locals and the given instruction bytes.
fn body(code: &[u8]) -> Vec<u8> {
    let mut inner = vec![0x00]; // no local declarations
    inner.extend_from_slice(code);
    inner.push(END);
    let mut out = Vec::new();
    leb_u32(inner.len() as u32, &mut out);
    out.extend_from_slice(&inner);
    out
}

fn code_section(bodies: &[Vec<u8>]) -> Vec<u8> {
    section(10, &vector(bodies))
}

fn export_section(name: &str, kind: u8, index: u32) -> Vec<u8> {
    let mut entry = Vec::new();
    leb_u32(name.len() as u32, &mut entry);
    entry.extend_from_slice(name.as_bytes());
    entry.push(kind);
    leb_u32(index, &mut entry);
    section(7, &vector(&[entry]))
}

fn i32_const(value: i32) -> Vec<u8> {
    let mut out = vec![0x41];
    leb_i32(value, &mut out);
    out
}

// ---- the tests ----

/// A module that is rejected, whoever rejects it.
///
/// Some of these never reach this crate's own checks: wasmparser catches an
/// unbalanced `end` or a function/code length mismatch while decoding. The
/// property under test is that the module is refused and named — not which
/// layer refuses it — so both error kinds count, and the check in this crate
/// stays as the backstop it is.
fn rejected(bytes: &[u8]) -> String {
    let module = match Module::parse(bytes) {
        Ok(module) => module,
        Err(error) => return error.to_string(),
    };
    match codegen::generate(&module) {
        Err(error) => error.to_string(),
        Ok(_) => panic!("expected the module to be refused, but it generated"),
    }
}

fn invalid(bytes: &[u8]) -> String {
    let module = match Module::parse(bytes) {
        Ok(module) => module,
        Err(error @ Error::Invalid { .. }) => return error.to_string(),
        Err(other) => panic!("expected an invalid-module error, got: {other}"),
    };
    match codegen::generate(&module) {
        Err(error @ Error::Invalid { .. }) => error.to_string(),
        Err(other) => panic!("expected an invalid-module error, got: {other}"),
        Ok(_) => panic!("expected the module to be refused, but it generated"),
    }
}

#[test]
fn fewer_bodies_than_the_function_section_declared() {
    let bytes = module(&[
        type_section(&[func_type(&[], &[])]),
        function_section(&[0, 0]),
        code_section(&[body(&[])]),
    ]);
    // wasmparser notices this while decoding; the check in `Module::parse` is
    // the backstop for a decoder that one day does not.
    let message = rejected(&bytes);
    assert!(message.contains("inconsistent lengths"), "{message}");
}

#[test]
fn more_bodies_than_the_function_section_declared() {
    let bytes = module(&[
        type_section(&[func_type(&[], &[])]),
        function_section(&[0]),
        code_section(&[body(&[]), body(&[])]),
    ]);
    let message = rejected(&bytes);
    assert!(message.contains("inconsistent lengths"), "{message}");
}

#[test]
fn a_function_whose_type_index_does_not_exist() {
    let bytes = module(&[
        type_section(&[func_type(&[], &[])]),
        function_section(&[7]),
        code_section(&[body(&[])]),
    ]);
    let message = invalid(&bytes);
    assert!(message.contains("missing type"), "{message}");
}

#[test]
fn a_call_to_a_function_that_is_not_there() {
    let bytes = module(&[
        type_section(&[func_type(&[], &[])]),
        function_section(&[0]),
        code_section(&[body(&[0x10, 99])]), // call 99
    ]);
    let message = invalid(&bytes);
    assert!(
        message.contains("call to unknown function #99"),
        "{message}"
    );
}

#[test]
fn a_read_of_a_global_that_is_not_there() {
    let bytes = module(&[
        type_section(&[func_type(&[], &[I32])]),
        function_section(&[0]),
        code_section(&[body(&[0x23, 4])]), // global.get 4
    ]);
    let message = invalid(&bytes);
    assert!(message.contains("global #4 does not exist"), "{message}");
}

#[test]
fn a_read_of_a_local_that_is_not_there() {
    let bytes = module(&[
        type_section(&[func_type(&[], &[I32])]),
        function_section(&[0]),
        code_section(&[body(&[0x20, 3])]), // local.get 3
    ]);
    let message = invalid(&bytes);
    assert!(message.contains("local #3 does not exist"), "{message}");
}

#[test]
fn an_instruction_with_nothing_to_operate_on() {
    let bytes = module(&[
        type_section(&[func_type(&[], &[I32])]),
        function_section(&[0]),
        code_section(&[body(&[0x6A])]), // i32.add, on an empty stack
    ]);
    let message = invalid(&bytes);
    assert!(message.contains("operand stack underflow"), "{message}");
}

#[test]
fn a_branch_past_the_outermost_block() {
    let bytes = module(&[
        type_section(&[func_type(&[], &[])]),
        function_section(&[0]),
        code_section(&[body(&[0x0C, 5])]), // br 5, with no blocks open
    ]);
    let message = invalid(&bytes);
    assert!(
        message.contains("branch past the outermost block"),
        "{message}"
    );
}

#[test]
fn an_end_with_no_block_open() {
    // Two `end`s: one closes the function body, the second closes nothing.
    let bytes = module(&[
        type_section(&[func_type(&[], &[])]),
        function_section(&[0]),
        code_section(&[body(&[END])]),
    ]);
    let message = rejected(&bytes);
    assert!(message.contains("end of function body"), "{message}");
}

#[test]
fn an_else_outside_a_block() {
    let bytes = module(&[
        type_section(&[func_type(&[], &[])]),
        function_section(&[0]),
        code_section(&[body(&[0x05])]), // else, with no if
    ]);
    let message = rejected(&bytes);
    assert!(message.contains("`else` found outside"), "{message}");
}

#[test]
fn an_else_closing_something_that_is_not_an_if() {
    let bytes = module(&[
        type_section(&[func_type(&[], &[])]),
        function_section(&[0]),
        code_section(&[body(&[0x02, 0x40, 0x05])]), // block, else
    ]);
    let message = rejected(&bytes);
    assert!(message.contains("`else` found outside"), "{message}");
}

#[test]
fn a_block_whose_type_index_does_not_exist() {
    let bytes = module(&[
        type_section(&[func_type(&[], &[])]),
        function_section(&[0]),
        code_section(&[body(&[0x02, 9, END])]), // block (type 9)
    ]);
    let message = invalid(&bytes);
    assert!(message.contains("block names a missing type"), "{message}");
}

#[test]
fn a_call_indirect_whose_type_index_does_not_exist() {
    let mut code = i32_const(0);
    code.extend_from_slice(&[0x11, 9, 0x00]); // call_indirect (type 9) (table 0)
    let bytes = module(&[
        type_section(&[func_type(&[], &[])]),
        function_section(&[0]),
        code_section(&[body(&code)]),
    ]);
    let message = invalid(&bytes);
    assert!(
        message.contains("call_indirect names a missing type"),
        "{message}"
    );
}

#[test]
fn a_branch_that_should_carry_a_value_and_has_none() {
    // `block (result i32)` whose body branches out with an empty stack.
    let bytes = module(&[
        type_section(&[func_type(&[], &[I32])]),
        function_section(&[0]),
        code_section(&[body(&[0x02, I32, 0x0C, 0x00, END])]),
    ]);
    let message = invalid(&bytes);
    assert!(message.contains("branch with no result value"), "{message}");
}

#[test]
fn an_export_of_a_function_that_is_not_there_is_left_out_rather_than_faked() {
    // A wrapper for a function with no signature cannot be written, and
    // inventing one would publish an entry point that calls nothing.
    let bytes = module(&[
        type_section(&[func_type(&[], &[])]),
        function_section(&[0]),
        export_section("ghost", 0x00, 42),
        code_section(&[body(&[])]),
    ]);
    let parsed = Module::parse(&bytes).expect("it decodes");
    let code = codegen::generate(&parsed).expect("and generates");
    assert!(!code.contains("pub fn ghost"), "{code}");
}

#[test]
fn a_data_segment_placed_by_a_global_is_reported_rather_than_placed_at_zero() {
    // `(data (global.get 0) "..")`, which only an imported global can mean —
    // and imported globals are refused, so this can only arrive hand-built.
    let mut segment = vec![0x00]; // active, memory 0
    segment.extend_from_slice(&[0x23, 0x00, END]); // global.get 0, end
    segment.push(2);
    segment.extend_from_slice(b"hi");
    let bytes = module(&[
        section(5, &vector(&[vec![0x00, 0x01]])), // memory: min 1
        section(11, &vector(&[segment])),
    ]);
    let parsed = Module::parse(&bytes).expect("it decodes");
    let code = codegen::generate(&parsed).expect("and generates");
    // The offset is unknown, so the segment is not written anywhere and the
    // output says why rather than quietly placing it at address 0.
    assert!(code.contains("global.get 0"), "{code}");
}

#[test]
fn a_memory_whose_minimum_does_not_fit_in_a_u32() {
    // Encodable, since the field is a LEB with no width limit, and meaningless
    // for a 32-bit memory. It is reported rather than truncated: truncating
    // would silently produce a memory of a completely different size.
    let mut memory = vec![0x00]; // no maximum
    leb_u32_wide(&mut memory);
    let bytes = module(&[section(5, &vector(&[memory]))]);
    let message = invalid(&bytes);
    assert!(
        message.contains("memory minimum overflows u32"),
        "{message}"
    );
}

/// Writes a LEB128 value larger than `u32::MAX` (2^33).
fn leb_u32_wide(out: &mut Vec<u8>) {
    out.extend_from_slice(&[0x80, 0x80, 0x80, 0x80, 0x20]);
}

#[test]
fn a_data_segment_aimed_at_a_second_memory_is_refused() {
    // Only reachable by hand: an assembler would need two memories, which are
    // refused earlier.
    let mut segment = vec![0x02]; // active, explicit memory index
    segment.push(0x01); // memory 1
    segment.extend_from_slice(&i32_const(0));
    segment.push(END);
    segment.push(1);
    segment.push(b'x');
    let bytes = module(&[
        section(5, &vector(&[vec![0x00, 0x01]])),
        section(11, &vector(&[segment])),
    ]);
    let message = match Module::parse(&bytes) {
        Err(error) => error.to_string(),
        Ok(_) => panic!("expected the module to be refused"),
    };
    assert!(message.contains("multiple memories"), "{message}");
}

#[test]
fn an_access_to_a_second_memory_is_refused() {
    // `i32.load` with a non-zero memory index in its immediate.
    let mut code = i32_const(0);
    code.extend_from_slice(&[0x28, 0x41, 0x01, 0x00]); // i32.load (memory 1) align=1 offset=0
    let bytes = module(&[
        type_section(&[func_type(&[], &[I32])]),
        function_section(&[0]),
        section(5, &vector(&[vec![0x00, 0x01]])),
        code_section(&[body(&code)]),
    ]);
    let message = match Module::parse(&bytes) {
        Err(error) => error.to_string(),
        Ok(_) => panic!("expected the module to be refused"),
    };
    assert!(
        message.contains("memory other than 0") || message.contains("multiple memories"),
        "{message}"
    );
}

#[test]
fn an_initialiser_that_is_not_a_constant_is_refused() {
    // `(global i32 (i32.add (i32.const 1) (i32.const 2)))` — not a constant
    // expression, and not something an assembler will emit.
    let mut global = vec![I32, 0x00]; // i32, immutable
    global.extend_from_slice(&i32_const(1));
    global.extend_from_slice(&i32_const(2));
    global.push(0x6A); // i32.add
    global.push(END);
    let bytes = module(&[section(6, &vector(&[global]))]);
    let message = rejected(&bytes);
    assert!(message.contains("constant expression"), "{message}");
}

#[test]
fn an_element_segment_holding_something_other_than_a_function_is_refused() {
    // An expression-form segment whose entry is `ref.null func` rather than
    // `ref.func`: the table slot would hold nothing callable.
    let mut segment = vec![0x05]; // expression form, with an element type
    segment.push(0x70); // funcref
    segment.push(0x01); // one entry
    segment.extend_from_slice(&[0xD0, 0x70, END]); // ref.null func
    let bytes = module(&[section(9, &vector(&[segment]))]);
    let message = rejected(&bytes);
    assert!(message.contains("element segment"), "{message}");
}

#[test]
fn a_call_indirect_through_a_second_table_is_refused() {
    let mut code = i32_const(0);
    code.extend_from_slice(&[0x11, 0x00, 0x01]); // call_indirect (type 0) (table 1)
    let bytes = module(&[
        type_section(&[func_type(&[], &[])]),
        function_section(&[0]),
        code_section(&[body(&code)]),
    ]);
    let message = rejected(&bytes);
    assert!(message.contains("table other than 0"), "{message}");
}

#[test]
fn an_initialiser_that_starts_with_a_non_constant_is_refused() {
    // `(global i32 (i32.add ..))` where the very first operator is the
    // arithmetic, rather than a constant followed by one.
    let mut global = vec![I32, 0x00];
    global.push(0x6A); // i32.add, with nothing before it
    global.push(END);
    let bytes = module(&[section(6, &vector(&[global]))]);
    let message = rejected(&bytes);
    assert!(message.contains("constant expression"), "{message}");
}

#[test]
fn a_passive_element_segment_places_nothing_in_the_table() {
    // A passive segment has no offset, so it contributes no slots at
    // instantiation — `table.init` would be needed, and that is not modelled.
    let mut segment = vec![0x01]; // passive, element kind follows
    segment.push(0x00); // elemkind: funcref
    segment.push(0x01); // one function
    segment.push(0x00); // function 0
    let bytes = module(&[
        type_section(&[func_type(&[], &[])]),
        function_section(&[0]),
        section(4, &vector(&[vec![0x70, 0x00, 0x02]])), // table: funcref, min 2
        section(9, &vector(&[segment])),
        code_section(&[body(&[])]),
    ]);
    let parsed = Module::parse(&bytes).expect("it decodes");
    assert_eq!(parsed.elems[0].offset, None);
    let code = codegen::generate(&parsed).expect("and generates");
    // Nothing was written into the table, so no slot assignment appears.
    assert!(!code.contains("instance.table["), "{code}");
}

#[test]
fn an_atomic_on_a_second_memory_is_refused() {
    // The check that all sixty-six atomics share. Only reachable by hand: an
    // assembler needs two memories to write it, and those are refused earlier.
    let mut code = i32_const(0);
    // i32.atomic.load. The alignment carries a flag saying a memory index
    // follows, and the index comes before the offset.
    code.extend_from_slice(&[0xFE, 0x10, 0x42, 0x01, 0x00]);
    let bytes = module(&[
        type_section(&[func_type(&[], &[I32])]),
        function_section(&[0]),
        section(5, &vector(&[vec![0x00, 0x01]])),
        code_section(&[body(&code)]),
    ]);
    let message = rejected(&bytes);
    assert!(
        message.contains("memory other than 0") || message.contains("multiple memories"),
        "{message}"
    );
}

#[test]
fn two_imported_memories_are_refused_at_the_second() {
    let mut first = Vec::new();
    leb_u32(3, &mut first);
    first.extend_from_slice(b"env");
    leb_u32(3, &mut first);
    first.extend_from_slice(b"one");
    first.extend_from_slice(&[0x02, 0x00, 0x01]); // memory, no maximum, min 1

    let mut second = Vec::new();
    leb_u32(3, &mut second);
    second.extend_from_slice(b"env");
    leb_u32(3, &mut second);
    second.extend_from_slice(b"two");
    second.extend_from_slice(&[0x02, 0x00, 0x01]);

    let bytes = module(&[section(2, &vector(&[first, second]))]);
    let message = rejected(&bytes);
    assert!(message.contains("multiple memories"), "{message}");
}

#[test]
fn a_code_section_with_no_function_section_has_bodies_belonging_to_nothing() {
    // The function section is what says which type each body has. Without it,
    // the first body has no declaration to pair with.
    let bytes = module(&[
        type_section(&[func_type(&[], &[])]),
        code_section(&[body(&[])]),
    ]);
    // A third route by which the decoder guarantees the two sections agree —
    // which is why the backstop in `Module::parse` cannot be reached from any
    // input, and is kept anyway.
    let message = rejected(&bytes);
    assert!(message.contains("function section is absent"), "{message}");
}
