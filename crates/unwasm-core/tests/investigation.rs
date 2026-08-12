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

/// The line in the index is a line number in the file it names, and the only
/// way to check that is to go and look.
///
/// Worth its own test because the number is *counted forward* rather than
/// measured: recounting the whole output for each function was quadratic — 152
/// of the 154 seconds a 3.3 MiB capture took — and the arithmetic that replaced
/// it is exactly the kind that is off by one without anything noticing.
#[test]
fn the_line_the_index_gives_is_where_the_function_actually_starts() {
    let wasm = common::assemble("index-lines", SAMPLE);
    let module = Module::parse(&wasm).expect("valid");
    for layout in [
        codegen::Layout::Single,
        codegen::Layout::Split { lines_per_file: 1 },
    ] {
        let files = codegen::generate_files(&module, layout).expect("generates");
        let json = &files
            .iter()
            .find(|file| file.name == "names.json")
            .expect("present")
            .contents;

        let mut checked = 0;
        for entry in json.split("{\"index\":").skip(1) {
            let field = |name: &str| {
                entry
                    .split_once(&format!("\"{name}\": "))
                    .map(|(_, rest)| rest.split([',', '}']).next().unwrap_or_default().trim())
                    .expect("the field is in the entry")
                    .trim_matches('"')
                    .to_string()
            };
            let (name, file, line) = (field("name"), field("file"), field("line"));
            let line: usize = line.parse().expect("a number");
            let contents = &files
                .iter()
                .find(|candidate| candidate.name == file)
                .unwrap_or_else(|| panic!("{file} is named by the index but was not written"))
                .contents;
            // From the line it gives, the function's own declaration is the
            // next one — everything before it is that function's doc comment.
            let rest: Vec<&str> = contents.lines().skip(line - 1).collect();
            let declaration = rest
                .iter()
                .position(|text| text.contains(&format!("fn {name}(")))
                .unwrap_or_else(|| panic!("{name} is not at {file}:{line}:\n{}", rest.join("\n")));
            assert!(
                rest[..declaration]
                    .iter()
                    .all(|text| text.trim_start().starts_with("///")),
                "{file}:{line} starts inside {name} rather than at it:\n{}",
                rest[..=declaration].join("\n")
            );
            checked += 1;
        }
        assert_eq!(checked, module.funcs.len());
    }
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

// ---- what the doc comment warns about ----

#[test]
fn a_shared_function_says_how_many_sites_call_it_not_just_how_many_callers() {
    // The difference is the whole point: patching or instrumenting a shared
    // body measures whichever site happened to run, and a caller that calls in
    // a loop body is one caller and many sites.
    let wasm = common::assemble(
        "sites",
        r#"(module
            (func $shared (param i32) (result i32) local.get 0)
            (func (export "caller") (param i32) (result i32)
                local.get 0 call $shared
                call $shared
                call $shared))"#,
    );
    let module = Module::parse(&wasm).expect("valid");
    let graph = analysis::analyse(&module).call_graph;
    assert_eq!(graph.callers_of(0).len(), 1);
    assert_eq!(graph.sites_reaching(0), 3);
    assert_eq!(graph.sites_reaching(1), 0, "nothing calls the caller");

    let code = common::decompile(&wasm);
    assert!(code.contains("from 3 call sites"), "{code}");

    let files = codegen::generate_files(&module, codegen::Layout::Single).expect("generates");
    let json = &files
        .iter()
        .find(|file| file.name == "names.json")
        .expect("present")
        .contents;
    assert!(json.contains(r#""call_sites": 3"#), "{json}");
    common::assert_valid_json(json);
}

#[test]
fn one_caller_calling_once_does_not_say_the_same_thing_twice() {
    let wasm = common::assemble(
        "sites-once",
        r#"(module
            (func $once (result i32) i32.const 1)
            (func (export "caller") (result i32) call $once))"#,
    );
    let code = common::decompile(&wasm);
    assert!(code.contains("Called by 1: `f1`."), "{code}");
    assert!(!code.contains("call sites"), "{code}");
}

/// A function whose prologue reserves 32 bytes and that writes 32 bytes in.
const OVERRUN: &str = r#"(module
    (memory (export "memory") 1)
    (global $sp (export "__stack_pointer") (mut i32) (i32.const 65536))
    (func (export "overrun") (local i32)
        global.get $sp i32.const 32 i32.sub local.tee 0 global.set $sp
        ;; inside
        local.get 0 i32.const 1 i32.store offset=28
        ;; and one word past the end, which is the caller's frame
        local.get 0 i32.const 1 i32.store offset=32
        local.get 0 i32.const 32 i32.add global.set $sp))"#;

#[test]
fn a_write_past_the_end_of_the_frame_is_called_out_in_the_output() {
    let wasm = common::assemble("overrun", OVERRUN);
    let module = Module::parse(&wasm).expect("valid");
    let frame = &analysis::analyse(&module).frames[&0];
    assert_eq!(frame.writes_outside(), vec![(32, 4)]);

    let code = common::decompile(&wasm);
    assert!(code.contains("Writes outside its own frame"), "{code}");
    assert!(code.contains("`frame + 32` (4 bytes)"), "{code}");
    assert!(
        code.contains("lands in the caller's frame"),
        "and says why that matters:\n{code}"
    );
}

#[test]
fn a_write_through_a_computed_frame_address_says_it_cannot_be_checked() {
    let wasm = common::assemble(
        "computed",
        r#"(module
            (memory (export "memory") 1)
            (global $sp (export "__stack_pointer") (mut i32) (i32.const 65536))
            (func (export "indexed") (param i32) (local i32)
                global.get $sp i32.const 32 i32.sub local.tee 1 global.set $sp
                ;; frame + i: an array in the frame, indexed at run time
                local.get 1 local.get 0 i32.add
                i32.const 7 i32.store
                local.get 1 i32.const 32 i32.add global.set $sp))"#,
    );
    let module = Module::parse(&wasm).expect("valid");
    assert_eq!(analysis::analyse(&module).frames[&0].computed_writes, 1);

    let code = common::decompile(&wasm);
    assert!(code.contains("computed from the frame at run"), "{code}");
    assert!(
        code.contains("not\n    /// something this can say"),
        "{code}"
    );
}

// ---- which bytes made this line ----

/// A function whose every instruction is distinguishable in the binary.
const PATCHABLE: &str = r#"(module
    (memory (export "memory") 1)
    (func $helper (result i32) i32.const 775533)
    (func (export "work") (param i32) (result i32)
        call $helper
        drop
        local.get 0
        i32.const 4096
        i32.store
        local.get 0))"#;

#[test]
fn every_operator_carries_where_it_sits_in_the_file() {
    let wasm = common::assemble("offsets-decode", PATCHABLE);
    let module = Module::parse(&wasm).expect("valid");
    for func in &module.funcs {
        assert_eq!(
            func.offsets.len(),
            func.body.len(),
            "one span per operator, and the trailing End dropped from both"
        );
        for (offset, length) in &func.offsets {
            assert!(*length > 0, "an operator is at least its opcode byte");
            assert!(
                (*offset as usize) + (*length as usize) <= wasm.len(),
                "and inside the file"
            );
        }
    }
    // The first operator of the first function is `i32.const 775533`, whose
    // encoding is the opcode plus a four-byte SLEB.
    let (offset, length) = module.funcs[0].offsets[0];
    assert_eq!(length, 4, "0x41 then the three SLEB bytes ed aa 2f");
    assert_eq!(wasm[offset as usize], 0x41);
}

#[test]
fn the_sidecar_says_which_bytes_produced_a_line() {
    let wasm = common::assemble("offsets-sidecar", PATCHABLE);
    let module = Module::parse(&wasm).expect("valid");
    let files = codegen::generate_options(
        &module,
        &codegen::Options {
            map_offsets: true,
            ..codegen::Options::default()
        },
    )
    .expect("generates");

    let sidecar = files
        .iter()
        .find(|file| file.name == "offsets.json")
        .expect("written when asked for");
    common::assert_valid_json(&sidecar.contents);

    // Pick the line the store is on, look it up, and check the bytes behind it
    // really are that store.
    let code = &files[0].contents;
    let line_number = code
        .lines()
        .position(|line| line.contains("self.memory.store32("))
        .expect("the store is emitted")
        + 1;
    let needle = format!("[{line_number}, ");
    let at = sidecar
        .contents
        .find(&needle)
        .unwrap_or_else(|| panic!("line {line_number} is in the map:\n{}", sidecar.contents));
    let entry = &sidecar.contents[at + 1..];
    let entry = &entry[..entry.find(']').expect("closed")];
    let numbers: Vec<usize> = entry
        .split(", ")
        .map(|number| number.parse().expect("a number"))
        .collect();
    let (offset, length) = (numbers[1], numbers[2]);
    let span = &wasm[offset..offset + length];
    assert_eq!(
        &span[span.len() - 3..],
        &[0x36, 0x02, 0x00],
        "the span ends with the i32.store and its memarg: {span:02x?}"
    );
    assert!(
        span.contains(&0x41),
        "and reaches back over the constant folded into it: {span:02x?}"
    );
}

#[test]
fn a_function_left_out_is_not_in_the_map() {
    // `--only` and `--offsets` together: what was stubbed has no lines, and
    // must not claim any.
    let wasm = common::assemble("offsets-only", PATCHABLE);
    let module = Module::parse(&wasm).expect("valid");
    let files = codegen::generate_options(
        &module,
        &codegen::Options {
            only: Some([1u32].into_iter().collect()),
            map_offsets: true,
            ..codegen::Options::default()
        },
    )
    .expect("generates");
    let sidecar = &files
        .iter()
        .find(|file| file.name == "offsets.json")
        .expect("present")
        .contents;
    assert!(sidecar.contains(r#""index": 1"#), "{sidecar}");
    assert!(!sidecar.contains(r#""index": 0"#), "{sidecar}");
    common::assert_valid_json(sidecar);
}

#[test]
fn without_the_flag_there_is_no_sidecar() {
    let wasm = common::assemble("offsets-absent", PATCHABLE);
    let module = Module::parse(&wasm).expect("valid");
    let files = codegen::generate_files(&module, codegen::Layout::Single).expect("generates");
    assert!(!files.iter().any(|file| file.name == "offsets.json"));
}

#[test]
fn a_split_layout_records_the_file_each_line_is_in() {
    let wasm = common::assemble("offsets-split", PATCHABLE);
    let module = Module::parse(&wasm).expect("valid");
    let files = codegen::generate_options(
        &module,
        &codegen::Options {
            layout: codegen::Layout::Split { lines_per_file: 1 },
            map_offsets: true,
            ..codegen::Options::default()
        },
    )
    .expect("generates");
    let sidecar = &files
        .iter()
        .find(|file| file.name == "offsets.json")
        .expect("present")
        .contents;
    assert!(sidecar.contains(r#""file": "part0.rs""#), "{sidecar}");
    assert!(sidecar.contains(r#""file": "part1.rs""#), "{sidecar}");
    common::assert_valid_json(sidecar);
}

// ---- compiling only what a path can reach ----

/// A module where one export reaches half of it and the other half is dead
/// weight, plus an indirect call to show the difference the table makes.
const REACHABLE: &str = r#"(module
    (memory (export "memory") 1)
    (type $unary (func (param i32) (result i32)))
    (func (export "entry") (param i32) (result i32)
        local.get 0 call 1)
    (func (param i32) (result i32) local.get 0 call 2)
    (func (param i32) (result i32) local.get 0 i32.const 2 i32.mul)
    (func (export "indirect") (param i32) (result i32)
        local.get 0 i32.const 0 call_indirect (type $unary))
    (func (type $unary) local.get 0 i32.const 3 i32.mul)
    (func (export "elsewhere") (result i32) i32.const 9)
    (table 2 funcref)
    (elem (i32.const 0) func 4))"#;

#[test]
fn a_reachable_set_is_the_closure_over_calls() {
    let wasm = common::assemble("reachable", REACHABLE);
    let module = Module::parse(&wasm).expect("valid");
    let analysis = analysis::analyse(&module);

    let from_entry = analysis.reachable_from(&module, 0);
    assert_eq!(from_entry, [0, 1, 2].into_iter().collect());
    assert!(
        !from_entry.contains(&5),
        "an export nothing calls is not reachable from another one"
    );

    // A leaf reaches only itself.
    assert_eq!(
        analysis.reachable_from(&module, 2),
        [2].into_iter().collect()
    );
}

#[test]
fn an_indirect_call_reaches_everything_of_that_shape() {
    // `call_indirect` names a type, not a target, so the reachable set has to
    // include every slot the table holds with that type — which is the module's
    // own claim about what could run.
    let wasm = common::assemble("reachable-indirect", REACHABLE);
    let module = Module::parse(&wasm).expect("valid");
    let analysis = analysis::analyse(&module);
    assert!(analysis.reachable_from(&module, 3).contains(&4));
    // And the direct-only set does not, which is what makes it smaller and
    // incomplete.
    assert!(!analysis.directly_reachable_from(&module, 3).contains(&4));
}

#[test]
fn a_cycle_in_the_call_graph_terminates() {
    // Mutual recursion, and a diamond: reaching a function twice must not walk
    // it twice, or the closure never finishes.
    let wasm = common::assemble(
        "reachable-cycle",
        r#"(module
            (func (export "a") (param i32) (result i32)
                local.get 0 call 1)
            (func (param i32) (result i32)
                local.get 0 call 0 call 2)
            (func (param i32) (result i32) local.get 0))"#,
    );
    let module = Module::parse(&wasm).expect("valid");
    let analysis = analysis::analyse(&module);
    assert_eq!(
        analysis.reachable_from(&module, 0),
        [0, 1, 2].into_iter().collect()
    );
}

#[test]
fn an_imported_function_in_the_table_is_reachable_too() {
    // The table can hold an import, and its type comes from the import section
    // rather than from a body.
    let wasm = common::assemble(
        "reachable-import",
        r#"(module
            (import "env" "callback" (func (param i32) (result i32)))
            (type $unary (func (param i32) (result i32)))
            (func (export "entry") (param i32) (result i32)
                local.get 0 i32.const 0 call_indirect (type $unary))
            (table 1 funcref)
            (elem (i32.const 0) func 0))"#,
    );
    let module = Module::parse(&wasm).expect("valid");
    let analysis = analysis::analyse(&module);
    assert!(
        analysis.reachable_from(&module, 1).contains(&0),
        "the import the table holds is part of what this can reach"
    );
}

#[test]
fn what_is_left_out_of_a_reachable_set_still_compiles_and_says_so() {
    // The property the whole thing rests on: a stub keeps its name and its
    // signature, so the output builds, and a run that reaches one stops and
    // says which function it wanted rather than answering wrongly.
    let wasm = common::assemble("reachable-stub", REACHABLE);
    let module = Module::parse(&wasm).expect("valid");
    let analysis = analysis::analyse(&module);
    let only = analysis.reachable_from(&module, 0);
    let files = codegen::generate_options(
        &module,
        &codegen::Options {
            only: Some(only),
            ..codegen::Options::default()
        },
    )
    .expect("generates");

    const DRIVER: &str = r#"
mod generated;
fn main() {
    let mut instance = generated::Instance::new();
    println!("{}", instance.entry(21));
    let missing = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        generated::Instance::new().elsewhere()
    }));
    println!("{}", missing.is_err());
}
"#;
    let output = common::run_with_generated("reachable-stub", &files[0].contents, DRIVER);
    let lines: Vec<&str> = output.trim().lines().collect();
    assert_eq!(lines[0], "42", "what was asked for runs");
    assert_eq!(
        lines[1], "true",
        "and what was left out says so rather than lying"
    );
}

// ---- reaching what the module only named by index ----

#[test]
fn a_module_with_no_table_has_nothing_to_invoke() {
    let wasm = common::assemble(
        "invoke-no-table",
        r#"(module (func (export "f") (result i32) i32.const 1))"#,
    );
    let code = common::decompile(&wasm);
    assert!(!code.contains("TABLE_TYPE"), "{code}");
    assert!(!code.contains("pub fn invoke_slot"), "{code}");
}

#[test]
fn a_table_slot_can_be_called_from_outside() {
    // An embind registration is a table index and nothing else: the module
    // tells its host "call slot 4173 to reach `startVoipCall`". A host with
    // only `call_indirect_7` has to know that 4173 is of type 7, and this is
    // where that comes from.
    let wasm = common::assemble(
        "invoke-slot",
        r#"(module
            (memory (export "memory") 1)
            (type $unary (func (param i32) (result i32)))
            (type $void (func))
            (type $floats (func (param f64) (result f64)))
            (type $single (func (param f32) (result f32)))
            (type $wide (func (param i64) (result i64)))
            (func (type $unary) local.get 0 i32.const 2 i32.mul)
            (func (type $void) i32.const 0 i32.const 42 i32.store)
            (func (type $floats) local.get 0 f64.const 1.5 f64.add)
            (func (type $single) local.get 0 f32.const 0.5 f32.add)
            (func (type $wide) local.get 0 i64.const 1 i64.add)
            (func (export "read") (result i32) i32.const 0 i32.load)
            (table 6 funcref)
            (elem (i32.const 1) func 0 1 2 3 4))"#,
    );
    let code = common::decompile(&wasm);
    assert!(code.contains("const TABLE_TYPE: &[u8] = b\""), "{code}");

    const DRIVER: &str = r#"
mod generated;
fn main() {
    let mut instance = generated::Instance::new();
    println!("{:?}", instance.slot_type(0));
    println!("{:?}", instance.invoke_slot(1, &[21]));
    println!("{:?}", instance.invoke_slot(2, &[]));
    println!("read {}", instance.read());
    // A float travels as its bits, because a cast would round it.
    let answer = instance.invoke_slot(3, &[2.25f64.to_bits() as i64]).unwrap().unwrap();
    println!("{}", f64::from_bits(answer as u64));
    let single = instance.invoke_slot(4, &[i64::from(1.25f32.to_bits())]).unwrap().unwrap();
    println!("{}", f32::from_bits(single as u32));
    // An i64 travels as itself, which is the whole reason the wire is i64.
    println!("{:?}", instance.invoke_slot(5, &[i64::MAX - 1]));
    // The three ways it refuses, rather than guessing at any of them.
    println!("{:?}", instance.invoke_slot(0, &[]).unwrap_err());
    println!("{:?}", instance.invoke_slot(1, &[]).unwrap_err());
    println!("{:?}", instance.invoke_slot(99, &[]).unwrap_err());
}
"#;
    let output = common::run_with_driver("invoke-slot", &wasm, DRIVER);
    let lines: Vec<&str> = output.trim().lines().collect();
    assert_eq!(lines[0], "None", "slot 0 was never filled");
    assert_eq!(lines[1], "Ok(Some(42))", "twice 21");
    assert_eq!(
        lines[2], "Ok(None)",
        "a void call returns nothing, not zero"
    );
    assert_eq!(lines[3], "read 42", "and it really ran");
    assert_eq!(lines[4], "3.75");
    assert_eq!(lines[5], "1.75", "an f32 travels as its bits too");
    assert_eq!(
        lines[6],
        format!("Ok(Some({}))", i64::MAX),
        "and an i64 as itself"
    );
    // The three ways it refuses, rather than guessing at any of them.
    assert!(lines[7].contains("slot 0 is empty"), "{}", lines[7]);
    assert!(lines[8].contains("arity"), "{}", lines[8]);
    assert!(lines[9].contains("empty"), "{}", lines[9]);
}
