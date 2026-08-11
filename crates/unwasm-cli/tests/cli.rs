//! The command line, driven the way a user drives it.

use std::path::PathBuf;
use std::process::Command;

fn scratch() -> PathBuf {
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.push("../../target/unwasm-tests/cli");
    std::fs::create_dir_all(&path).expect("creating the scratch directory");
    path
}

/// Assembles a wat fixture with wasm-tools, which the harness already requires.
fn fixture(name: &str, wat: &str) -> PathBuf {
    let source = scratch().join(format!("{name}.wat"));
    let binary = scratch().join(format!("{name}.wasm"));
    std::fs::write(&source, wat).expect("writing the fixture");
    let output = Command::new("wasm-tools")
        .arg("parse")
        .arg(&source)
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("`wasm-tools` is required by these tests and is not runnable");
    assert!(output.status.success(), "wasm-tools rejected the fixture");
    binary
}

fn run(arguments: &[&str]) -> (bool, String, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_unwasm"))
        .args(arguments)
        .output()
        .expect("running unwasm");
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
    )
}

const SAMPLE: &str = r#"(module
    (import "env" "host_fn" (func (param i32)))
    (memory (export "memory") 2 16)
    (global (mut i32) (i32.const 1))
    (data (i32.const 0) "hi")
    (func (export "add") (param i32 i32) (result i32)
        local.get 0 local.get 1 i32.add))"#;

#[test]
fn decompile_writes_rust_to_stdout() {
    let path = fixture("sample-stdout", SAMPLE);
    let (ok, stdout, stderr) = run(&["decompile", path.to_str().expect("utf-8 path")]);
    assert!(ok, "{stderr}");
    assert!(stdout.contains("pub struct Instance"), "{stdout}");
    assert!(stdout.contains("pub fn add(&mut self, p0: i32, p1: i32) -> i32"));
}

#[test]
fn decompile_writes_to_a_file_and_reports_what_it_wrote() {
    let path = fixture("sample-file", SAMPLE);
    let destination = scratch().join("out.rs");
    let (ok, stdout, stderr) = run(&[
        "decompile",
        path.to_str().expect("utf-8 path"),
        "-o",
        destination.to_str().expect("utf-8 path"),
    ]);
    assert!(ok, "{stderr}");
    assert!(stdout.contains("wrote"), "{stdout}");
    assert!(stdout.contains("1 functions"), "{stdout}");
    let written = std::fs::read_to_string(&destination).expect("the file exists");
    assert!(written.contains("pub mod rt"));
}

#[test]
fn inspect_reports_what_the_module_contains() {
    let path = fixture("sample-inspect", SAMPLE);
    let (ok, stdout, stderr) = run(&["inspect", path.to_str().expect("utf-8 path")]);
    assert!(ok, "{stderr}");
    assert!(stdout.contains("2 functions (1 imported)"), "{stdout}");
    assert!(stdout.contains("1 globals"), "{stdout}");
    assert!(stdout.contains("1 data segments"), "{stdout}");
    assert!(
        stdout.contains("memory: 2 pages initial, 16 maximum"),
        "{stdout}"
    );
    assert!(stdout.contains("env::host_fn"), "{stdout}");
    assert!(stdout.contains("Func add -> #1"), "{stdout}");
}

#[test]
fn inspect_says_so_when_there_is_no_memory() {
    let path = fixture("nomem", "(module (func (export \"f\")))");
    let (ok, stdout, _) = run(&["inspect", path.to_str().expect("utf-8 path")]);
    assert!(ok);
    assert!(stdout.contains("memory: none"), "{stdout}");
}

#[test]
fn no_arguments_prints_the_usage() {
    let (ok, stdout, _) = run(&[]);
    assert!(ok);
    assert!(stdout.contains("usage:"), "{stdout}");
    let (ok, stdout, _) = run(&["--help"]);
    assert!(ok);
    assert!(stdout.contains("usage:"), "{stdout}");
}

#[test]
fn an_unknown_command_fails_and_says_what_it_expected() {
    let (ok, _, stderr) = run(&["disassemble", "x.wasm"]);
    assert!(!ok);
    assert!(stderr.contains("unknown command `disassemble`"), "{stderr}");
    assert!(stderr.contains("usage:"));
}

#[test]
fn a_missing_path_fails_with_the_path_in_the_message() {
    let (ok, _, stderr) = run(&["decompile", "/nonexistent/module.wasm"]);
    assert!(!ok);
    assert!(stderr.contains("/nonexistent/module.wasm"), "{stderr}");
}

#[test]
fn a_missing_argument_is_reported_rather_than_ignored() {
    let (ok, _, stderr) = run(&["decompile"]);
    assert!(!ok);
    assert!(stderr.contains("needs a module path"), "{stderr}");

    let (ok, _, stderr) = run(&["inspect"]);
    assert!(!ok);
    assert!(stderr.contains("needs a module path"), "{stderr}");

    let path = fixture("sample-args", SAMPLE);
    let (ok, _, stderr) = run(&["decompile", path.to_str().expect("utf-8 path"), "-o"]);
    assert!(!ok);
    assert!(stderr.contains("-o needs a path"), "{stderr}");

    let (ok, _, stderr) = run(&["decompile", path.to_str().expect("utf-8 path"), "--wat"]);
    assert!(!ok);
    assert!(stderr.contains("unexpected argument `--wat`"), "{stderr}");
}

#[test]
fn an_unsupported_module_fails_with_the_construct_named() {
    let path = fixture(
        "simd-module",
        "(module (func (export \"f\") (result v128) v128.const i32x4 0 0 0 0))",
    );
    let (ok, _, stderr) = run(&["decompile", path.to_str().expect("utf-8 path")]);
    assert!(!ok);
    assert!(stderr.contains("v128"), "{stderr}");
    assert!(stderr.contains("rather than emitting a guess"), "{stderr}");
}

#[test]
fn writing_to_an_unwritable_path_reports_the_path() {
    let path = fixture("sample-unwritable", SAMPLE);
    let (ok, _, stderr) = run(&[
        "decompile",
        path.to_str().expect("utf-8 path"),
        "-o",
        "/nonexistent-directory/out.rs",
    ]);
    assert!(!ok);
    assert!(stderr.contains("/nonexistent-directory/out.rs"), "{stderr}");
}

#[test]
fn a_destination_that_is_not_a_rust_file_becomes_a_directory() {
    let path = fixture("sample-dir", SAMPLE);
    let destination = scratch().join("out-dir");
    let _ = std::fs::remove_dir_all(&destination);
    let (ok, stdout, stderr) = run(&[
        "decompile",
        path.to_str().expect("utf-8 path"),
        "-o",
        destination.to_str().expect("utf-8 path"),
        "--split",
        "1",
    ]);
    assert!(ok, "{stderr}");
    assert!(stdout.contains("2 Rust files plus names.json"), "{stdout}");
    let root = std::fs::read_to_string(destination.join("mod.rs")).expect("mod.rs exists");
    assert!(root.contains("mod part0;"), "{root}");
    let part = std::fs::read_to_string(destination.join("part0.rs")).expect("part0.rs exists");
    assert!(part.contains("use super::*;"), "{part}");
}

#[test]
fn a_small_module_written_to_a_directory_still_gets_one_file() {
    // The layout follows the module's size unless `--split` overrides it, and
    // one function does not need parts.
    let path = fixture("sample-autolayout", SAMPLE);
    let destination = scratch().join("out-auto");
    let _ = std::fs::remove_dir_all(&destination);
    let (ok, stdout, stderr) = run(&[
        "decompile",
        path.to_str().expect("utf-8 path"),
        "-o",
        destination.to_str().expect("utf-8 path"),
    ]);
    assert!(ok, "{stderr}");
    assert!(stdout.contains("1 Rust files plus names.json"), "{stdout}");
    assert!(destination.join("mod.rs").exists());
    assert!(!destination.join("part0.rs").exists());
}

#[test]
fn split_needs_a_number_and_a_destination() {
    let path = fixture("sample-splitargs", SAMPLE);
    let (ok, _, stderr) = run(&["decompile", path.to_str().expect("utf-8 path"), "--split"]);
    assert!(!ok);
    assert!(stderr.contains("--split needs a number"), "{stderr}");

    let (ok, _, stderr) = run(&[
        "decompile",
        path.to_str().expect("utf-8 path"),
        "--split",
        "many",
    ]);
    assert!(!ok);
    assert!(stderr.contains("not `many`"), "{stderr}");

    // Several files cannot go to stdout, and saying so beats writing the first
    // one and dropping the rest.
    let (ok, _, stderr) = run(&[
        "decompile",
        path.to_str().expect("utf-8 path"),
        "--split",
        "1",
    ]);
    assert!(!ok);
    assert!(stderr.contains("needs -o <directory>"), "{stderr}");
}

#[test]
fn an_unwritable_directory_reports_the_path() {
    let path = fixture("sample-baddir", SAMPLE);
    let (ok, _, stderr) = run(&[
        "decompile",
        path.to_str().expect("utf-8 path"),
        "-o",
        "/proc/unwasm-cannot-write-here",
    ]);
    assert!(!ok);
    assert!(
        stderr.contains("/proc/unwasm-cannot-write-here"),
        "{stderr}"
    );
}

#[test]
fn inspect_reports_what_the_analysis_could_make_of_the_module() {
    // A module with a stack pointer and a frame: the two things `inspect`
    // reports beyond the section counts.
    let path = fixture(
        "sample-analysis",
        r#"(module
            (memory (export "memory") 1)
            (global $sp (export "__stack_pointer") (mut i32) (i32.const 65536))
            (func (export "framed") (param i32) (result i32)
                (local i32)
                global.get $sp
                i32.const 16
                i32.sub
                local.tee 1
                global.set $sp
                local.get 1
                local.get 0
                i32.store offset=4
                local.get 1
                i32.load offset=4
                local.get 1
                i32.const 16
                i32.add
                global.set $sp))"#,
    );
    let (ok, stdout, stderr) = run(&["inspect", path.to_str().expect("utf-8 path")]);
    assert!(ok, "{stderr}");
    assert!(
        stdout.contains("stack pointer: global #0 (by its exported name)"),
        "{stdout}"
    );
    assert!(stdout.contains("frames: 1 functions"), "{stdout}");
    assert!(stdout.contains("1 whose address stays put"), "{stdout}");
    assert!(stdout.contains("largest 16 bytes"), "{stdout}");
}

#[test]
fn inspect_says_when_it_could_not_identify_a_stack_pointer() {
    let path = fixture(
        "sample-nosp",
        "(module (func (export \"f\") (result i32) i32.const 1))",
    );
    let (ok, stdout, _) = run(&["inspect", path.to_str().expect("utf-8 path")]);
    assert!(ok);
    assert!(stdout.contains("stack pointer: not identified"), "{stdout}");
    assert!(!stdout.contains("frames:"), "{stdout}");
}

#[test]
fn inspect_reports_an_imported_shared_memory_as_both() {
    // What the VoIP module looks like: the host owns the memory, and it is
    // shared because the module was built for threads.
    let path = fixture(
        "sample-shared",
        r#"(module
            (import "env" "memory" (memory 160 32768 shared))
            (func (export "f") (result i32) i32.const 1))"#,
    );
    let (ok, stdout, stderr) = run(&["inspect", path.to_str().expect("utf-8 path")]);
    assert!(ok, "{stderr}");
    assert!(
        stdout.contains(
            "memory: 160 pages initial, 32768 maximum, shared, imported from env::memory"
        ),
        "{stdout}"
    );
}

#[test]
fn inspect_reports_a_plain_declared_memory_plainly() {
    let path = fixture(
        "sample-plainmem",
        "(module (memory 2) (func (export \"f\") (result i32) i32.const 1))",
    );
    let (ok, stdout, _) = run(&["inspect", path.to_str().expect("utf-8 path")]);
    assert!(ok);
    assert!(stdout.contains("memory: 2 pages initial\n"), "{stdout}");
}

#[test]
fn host_writes_a_skeleton_for_the_imports_that_remain() {
    let path = fixture("sample-host", SAMPLE);
    let (ok, stdout, stderr) = run(&["host", path.to_str().expect("utf-8 path")]);
    assert!(ok, "{stderr}");
    assert!(stdout.contains("impl Imports for Host"), "{stdout}");
    assert!(stdout.contains(r#"todo!("env::host_fn")"#), "{stdout}");
    assert!(stdout.contains("1 methods"), "{stdout}");
}

#[test]
fn host_writes_to_a_file_and_says_how_much_is_left_to_do() {
    let path = fixture("sample-hostfile", SAMPLE);
    let destination = scratch().join("host.rs");
    let (ok, stdout, stderr) = run(&[
        "host",
        path.to_str().expect("utf-8 path"),
        "-o",
        destination.to_str().expect("utf-8 path"),
    ]);
    assert!(ok, "{stderr}");
    assert!(
        stdout.contains("1 methods, 1 of them still to implement"),
        "{stdout}"
    );
    let written = std::fs::read_to_string(&destination).expect("the file exists");
    assert!(written.contains("pub struct Host"));
}

#[test]
fn host_needs_a_module_and_rejects_what_it_does_not_understand() {
    let (ok, _, stderr) = run(&["host"]);
    assert!(!ok);
    assert!(stderr.contains("host needs a module path"), "{stderr}");

    let path = fixture("sample-hostargs", SAMPLE);
    let (ok, _, stderr) = run(&["host", path.to_str().expect("utf-8 path"), "--split", "4"]);
    assert!(!ok);
    assert!(stderr.contains("unexpected argument `--split`"), "{stderr}");

    let (ok, _, stderr) = run(&["host", path.to_str().expect("utf-8 path"), "-o"]);
    assert!(!ok);
    assert!(stderr.contains("-o needs a path"), "{stderr}");

    let (ok, _, stderr) = run(&[
        "host",
        path.to_str().expect("utf-8 path"),
        "-o",
        "/nonexistent-directory/host.rs",
    ]);
    assert!(!ok);
    assert!(
        stderr.contains("/nonexistent-directory/host.rs"),
        "{stderr}"
    );
}

#[test]
fn inspect_lists_what_the_module_registers_with_embind() {
    let path = fixture(
        "sample-embind",
        r#"(module
            (import "env" "_embind_register_integer"
                (func $reg_int (param i32 i32 i32 i32 i32)))
            (import "env" "_embind_register_function"
                (func $register (param i32 i32 i32 i32 i32 i32 i32)))
            (memory 1)
            (data (i32.const 64) "initVoipStack\00")
            (data (i32.const 100) "int\00")
            (data (i32.const 110) "typed_call\00")
            (data (i32.const 200) "\01\00\00\00\01\00\00\00")
            (func (export "register_it") (param i32)
                i32.const 64
                i32.const 0 i32.const 0 i32.const 0 i32.const 0 i32.const 0 i32.const 0
                call $register
                i32.const 1 i32.const 100 i32.const 4 i32.const 0 i32.const 0
                call $reg_int
                i32.const 110 i32.const 2 i32.const 200
                i32.const 0 i32.const 0 i32.const 0 i32.const 0
                call $register
                ;; a registration whose name is computed: neither name nor types
                local.get 0
                i32.const 0 i32.const 0 i32.const 0 i32.const 0 i32.const 0 i32.const 0
                call $register))"#,
    );
    let (ok, stdout, stderr) = run(&["inspect", path.to_str().expect("utf-8 path")]);
    assert!(ok, "{stderr}");
    assert!(
        stdout.contains("embind: 4 registrations, 3 of them named"),
        "{stdout}"
    );
    // A name with no types to resolve, a free function with its signature, and
    // a registration that yielded neither.
    assert!(stdout.contains("  function initVoipStack\n"), "{stdout}");
    assert!(
        stdout.contains("  function int typed_call(int)"),
        "{stdout}"
    );
}

#[test]
fn inspect_reports_a_stack_pointer_found_by_its_use() {
    // No exported name: the prologues are the evidence.
    let path = fixture(
        "sample-prologues",
        r#"(module
            (memory 1)
            (global $sp (mut i32) (i32.const 65536))
            (func (export "a") (result i32)
                (local i32)
                global.get $sp i32.const 16 i32.sub local.tee 0 global.set $sp
                local.get 0 i32.const 16 i32.add global.set $sp
                i32.const 1)
            (func (export "b") (result i32)
                (local i32)
                global.get $sp i32.const 32 i32.sub local.tee 0 global.set $sp
                local.get 0 i32.const 32 i32.add global.set $sp
                i32.const 2))"#,
    );
    let (ok, stdout, stderr) = run(&["inspect", path.to_str().expect("utf-8 path")]);
    assert!(ok, "{stderr}");
    assert!(
        stdout.contains("stack pointer: global #0 (by 2 prologues)"),
        "{stdout}"
    );
}

/// rustc parses nesting recursively and dies with `SIGSEGV` past a couple of
/// thousand blocks, which reads as a compiler bug. Saying so at the point the
/// file is written is the difference between a fixable run and a mystery.
#[test]
fn decompiling_a_deeply_nested_module_says_what_rustc_will_need() {
    const DEPTH: usize = 400;
    let mut wat = String::from("(module (func (export \"deep\") (param i32) (result i32)\n");
    for at in 0..DEPTH {
        wat.push_str(&format!(
            "  block\n  local.get 0\n  i32.const {at}\n  i32.eq\n  br_if 0\n"
        ));
    }
    for _ in 0..DEPTH {
        wat.push_str("  end\n");
    }
    wat.push_str("  local.get 0))\n");

    let path = fixture("sample-deep", &wat);
    let out = scratch().join("deep.rs");
    let (ok, stdout, stderr) = run(&[
        "decompile",
        path.to_str().expect("utf-8 path"),
        "-o",
        out.to_str().expect("utf-8 path"),
    ]);
    assert!(ok, "{stderr}");
    assert!(
        stdout.contains(&format!("nests {DEPTH} blocks")),
        "{stdout}"
    );
    assert!(stdout.contains("RUST_MIN_STACK"), "{stdout}");

    // And a module rustc can parse on the stack it has says nothing.
    let shallow = fixture(
        "sample-shallow",
        "(module (func (export \"f\") (result i32) (block (result i32) i32.const 1)))",
    );
    let (ok, stdout, stderr) = run(&[
        "decompile",
        shallow.to_str().expect("utf-8 path"),
        "-o",
        scratch().join("shallow.rs").to_str().expect("utf-8 path"),
    ]);
    assert!(ok, "{stderr}");
    assert!(!stdout.contains("RUST_MIN_STACK"), "{stdout}");
}

#[test]
fn stubbing_recognised_code_needs_a_catalogue_and_reports_what_it_left_out() {
    // Two functions with the same shape and different names, so one module can
    // be a catalogue for the other.
    let body = " local.get 0 i32.const 1 i32.add local.set 0".repeat(8);
    let named = fixture(
        "sample-catalogue",
        &format!(
            "(module (func $memcpy_like (export \"a\") (param i32) (result i32){body} local.get 0))"
        ),
    );
    let stripped = fixture(
        "sample-stubbable",
        &format!("(module (func (export \"a\") (param i32) (result i32){body} local.get 0))"),
    );
    let catalogue = scratch().join("sigs.txt");
    let (ok, _, stderr) = run(&[
        "signatures",
        named.to_str().expect("utf-8 path"),
        "-o",
        catalogue.to_str().expect("utf-8 path"),
    ]);
    assert!(ok, "{stderr}");

    // Without a catalogue it would leave nothing out, and say it had.
    let (ok, _, stderr) = run(&[
        "decompile",
        stripped.to_str().expect("utf-8 path"),
        "--stub-recognised",
        "-o",
        scratch().join("stubbed.rs").to_str().expect("utf-8 path"),
    ]);
    assert!(!ok);
    assert!(
        stderr.contains("--stub-recognised needs --signatures"),
        "{stderr}"
    );

    let out = scratch().join("stubbed.rs");
    let (ok, stdout, stderr) = run(&[
        "decompile",
        stripped.to_str().expect("utf-8 path"),
        "--signatures",
        catalogue.to_str().expect("utf-8 path"),
        "--stub-recognised",
        "-o",
        out.to_str().expect("utf-8 path"),
    ]);
    assert!(ok, "{stderr}");
    assert!(
        stdout.contains("left 1 of 1 functions out as recognised library code"),
        "{stdout}"
    );
    let written = std::fs::read_to_string(&out).expect("the file exists");
    assert!(written.contains("fn f0_memcpy_like"), "{written}");
    assert!(
        written.contains("left out by --stub-recognised"),
        "{written}"
    );
}

#[test]
fn inspect_reports_a_stack_pointer_the_name_section_names() {
    // No export and no prologue: the module names the global, which is the
    // module saying which one it is.
    let path = fixture(
        "sample-named-sp",
        r#"(module
            (memory 1)
            (global $__stack_pointer (mut i32) (i32.const 65536))
            (func (export "a") (result i32) global.get $__stack_pointer))"#,
    );
    let (ok, stdout, stderr) = run(&["inspect", path.to_str().expect("utf-8 path")]);
    assert!(ok, "{stderr}");
    assert!(
        stdout.contains("stack pointer: global #0 (by its name in the name section)"),
        "{stdout}"
    );
}

#[test]
fn table_lists_what_each_slot_holds_and_filters_by_signature() {
    let path = fixture(
        "sample-table",
        r#"(module
            (type $unary (func (param i32) (result i32)))
            (type $sink (func (param i32)))
            (func (export "double") (param i32) (result i32) local.get 0 i32.const 2 i32.mul)
            (func (export "sink") (param i32))
            (table 4 funcref)
            (elem (i32.const 1) func 0 1))"#,
    );
    let (ok, stdout, stderr) = run(&["table", path.to_str().expect("utf-8 path")]);
    assert!(ok, "{stderr}");
    assert!(stdout.contains("2 of 2 slots"), "{stdout}");
    assert!(stdout.contains("slot 1"), "{stdout}");
    assert!(stdout.contains("(i32) -> (i32)"), "{stdout}");

    // The question that is otherwise hard: which slot holds this signature?
    let (ok, stdout, _) = run(&[
        "table",
        path.to_str().expect("utf-8 path"),
        "--type",
        "(i32) -> ()",
    ]);
    assert!(ok);
    assert!(stdout.contains("1 of 2 slots matching"), "{stdout}");
    assert!(stdout.contains("slot 2"), "{stdout}");
    assert!(!stdout.contains("slot 1  "), "{stdout}");
}

#[test]
fn table_says_when_there_is_nothing_in_it() {
    let path = fixture(
        "sample-emptytable",
        "(module (table 4 funcref) (func (export \"f\")))",
    );
    let (ok, stdout, _) = run(&["table", path.to_str().expect("utf-8 path")]);
    assert!(ok);
    assert!(stdout.contains("the table is empty"), "{stdout}");
}

#[test]
fn table_rejects_arguments_it_does_not_understand() {
    let (ok, _, stderr) = run(&["table"]);
    assert!(!ok);
    assert!(stderr.contains("table needs a module path"), "{stderr}");

    let path = fixture("sample-tableargs", SAMPLE);
    let (ok, _, stderr) = run(&["table", path.to_str().expect("utf-8 path"), "--type"]);
    assert!(!ok);
    assert!(stderr.contains("--type needs a signature"), "{stderr}");

    let (ok, _, stderr) = run(&["table", path.to_str().expect("utf-8 path"), "--split", "4"]);
    assert!(!ok);
    assert!(stderr.contains("unexpected argument `--split`"), "{stderr}");
}

#[test]
fn only_decompiles_the_functions_named_and_writes_the_index() {
    let path = fixture(
        "sample-only",
        r#"(module
            (func (export "a") (result i32) i32.const 111)
            (func (export "b") (result i32) i32.const 222))"#,
    );
    let destination = scratch().join("only-out");
    let _ = std::fs::remove_dir_all(&destination);
    let (ok, stdout, stderr) = run(&[
        "decompile",
        path.to_str().expect("utf-8 path"),
        "-o",
        destination.to_str().expect("utf-8 path"),
        "--only",
        "1",
    ]);
    assert!(ok, "{stderr}");
    assert!(stdout.contains("names.json"), "{stdout}");

    let written = std::fs::read_to_string(destination.join("mod.rs")).expect("mod.rs");
    assert!(written.contains("222i32"), "the one asked for:\n{written}");
    assert!(written.contains("was not decompiled: --only"), "{written}");

    let index = std::fs::read_to_string(destination.join("names.json")).expect("names.json");
    assert!(index.contains(r#""index": 0"#), "{index}");
    assert!(index.contains(r#""index": 1"#), "{index}");
}

#[test]
fn only_needs_indices_it_can_parse() {
    let path = fixture("sample-onlyargs", SAMPLE);
    let (ok, _, stderr) = run(&["decompile", path.to_str().expect("utf-8 path"), "--only"]);
    assert!(!ok);
    assert!(
        stderr.contains("--only needs a list of indices"),
        "{stderr}"
    );

    let (ok, _, stderr) = run(&[
        "decompile",
        path.to_str().expect("utf-8 path"),
        "--only",
        "first",
    ]);
    assert!(!ok);
    assert!(
        stderr.contains("--only takes indices, not `first`"),
        "{stderr}"
    );

    let (ok, _, stderr) = run(&[
        "decompile",
        path.to_str().expect("utf-8 path"),
        "--only",
        ",",
    ]);
    assert!(!ok);
    assert!(
        stderr.contains("--only needs at least one index"),
        "{stderr}"
    );
}

#[test]
fn only_to_stdout_writes_just_the_rust() {
    let path = fixture("sample-onlystdout", SAMPLE);
    let (ok, stdout, stderr) = run(&[
        "decompile",
        path.to_str().expect("utf-8 path"),
        "--only",
        "1",
    ]);
    assert!(ok, "{stderr}");
    assert!(stdout.contains("pub struct Instance"), "{stdout}");
    assert!(
        !stdout.contains("\"functions\":"),
        "the index needs a directory"
    );
}

#[test]
fn calls_reports_both_directions_for_one_function() {
    let path = fixture(
        "sample-calls",
        r#"(module
            (func (export "entry") (result i32) call 1)
            (func (result i32) call 2)
            (func (result i32) i32.const 7)
            (table 1 funcref)
            (elem (i32.const 0) func 2))"#,
    );
    let (ok, stdout, stderr) = run(&["calls", path.to_str().expect("utf-8 path"), "1"]);
    assert!(ok, "{stderr}");
    assert!(
        stdout.contains("called by 1 from 1 call sites:"),
        "{stdout}"
    );
    assert!(stdout.contains("f0"), "{stdout}");
    assert!(stdout.contains("calls 1:"), "{stdout}");
    assert!(stdout.contains("f2"), "{stdout}");

    // The one in the table says so, and so does the exported one.
    let (ok, stdout, _) = run(&["calls", path.to_str().expect("utf-8 path"), "2"]);
    assert!(ok);
    assert!(stdout.contains("in table slots: 0"), "{stdout}");
    let (ok, stdout, _) = run(&["calls", path.to_str().expect("utf-8 path"), "0"]);
    assert!(ok);
    assert!(stdout.contains("exported as: entry"), "{stdout}");
    assert!(
        stdout.contains("called by 0 from 0 call sites:"),
        "{stdout}"
    );
}

#[test]
fn calls_rejects_an_index_the_module_does_not_have() {
    let path = fixture("sample-callsargs", SAMPLE);
    let (ok, _, stderr) = run(&["calls", path.to_str().expect("utf-8 path"), "9999"]);
    assert!(!ok);
    assert!(stderr.contains("#9999 is not one of them"), "{stderr}");

    let (ok, _, stderr) = run(&["calls", path.to_str().expect("utf-8 path")]);
    assert!(!ok);
    assert!(stderr.contains("calls needs a function index"), "{stderr}");

    let (ok, _, stderr) = run(&["calls", path.to_str().expect("utf-8 path"), "add"]);
    assert!(!ok);
    assert!(stderr.contains("calls takes a function index"), "{stderr}");

    let (ok, _, stderr) = run(&["calls"]);
    assert!(!ok);
    assert!(stderr.contains("calls needs a module path"), "{stderr}");

    let (ok, _, stderr) = run(&["calls", path.to_str().expect("utf-8 path"), "1", "extra"]);
    assert!(!ok);
    assert!(stderr.contains("unexpected argument `extra`"), "{stderr}");
}

#[test]
fn calls_says_what_an_indirect_call_could_reach() {
    let path = fixture(
        "sample-callsindirect",
        r#"(module
            (type $unary (func (param i32) (result i32)))
            (func (export "dispatch") (param i32) (result i32)
                local.get 0
                i32.const 0
                call_indirect (type $unary))
            (func (param i32) (result i32) local.get 0)
            (table 2 funcref)
            (elem (i32.const 0) func 1))"#,
    );
    let (ok, stdout, stderr) = run(&["calls", path.to_str().expect("utf-8 path"), "0"]);
    assert!(ok, "{stderr}");
    assert!(stdout.contains("and through the table"), "{stdout}");
    assert!(stdout.contains("(i32) -> (i32)"), "{stdout}");
    assert!(stdout.contains("1 slots hold one"), "{stdout}");
}

#[test]
fn calls_prints_a_placeholder_for_a_signature_the_module_does_not_have() {
    // A function whose type index points at nothing. wasm-tools will not
    // assemble that, so the bytes are written directly: one type, one function
    // declared with type 5, one empty body.
    const INCONSISTENT: &[u8] = &[
        0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, // header
        0x01, 0x04, 0x01, 0x60, 0x00, 0x00, // types: one, () -> ()
        0x03, 0x02, 0x01, 0x05, // functions: one, of type 5
        0x0a, 0x04, 0x01, 0x02, 0x00, 0x0b, // code: one empty body
    ];
    let path = scratch().join("inconsistent.wasm");
    std::fs::write(&path, INCONSISTENT).expect("writing the module");

    let (ok, stdout, stderr) = run(&["calls", path.to_str().expect("utf-8 path"), "0"]);
    assert!(ok, "{stderr}");
    assert!(
        stdout.contains('?'),
        "a signature that cannot be resolved is shown as unknown, not invented:\n{stdout}"
    );
}

#[test]
fn calls_names_an_indirect_type_it_cannot_resolve() {
    // `call_indirect (type 9)` in a module with fewer types. Again by hand.
    const INCONSISTENT: &[u8] = &[
        0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, // header
        0x01, 0x04, 0x01, 0x60, 0x00, 0x00, // types: one, () -> ()
        0x03, 0x02, 0x01, 0x00, // functions: one, of type 0
        0x04, 0x04, 0x01, 0x70, 0x00, 0x01, // table: one funcref, min 1
        0x0a, 0x09, 0x01, 0x07, 0x00, 0x41, 0x00, 0x11, 0x09, 0x00, 0x0b,
    ];
    let path = scratch().join("indirect-unknown.wasm");
    std::fs::write(&path, INCONSISTENT).expect("writing the module");

    let (ok, stdout, stderr) = run(&["calls", path.to_str().expect("utf-8 path"), "0"]);
    assert!(ok, "{stderr}");
    assert!(stdout.contains("type 9"), "{stdout}");
}

#[test]
fn inspect_prints_a_method_under_its_class_with_its_types() {
    let path = fixture(
        "sample-embind-method",
        r#"(module
            (import "env" "_embind_register_integer"
                (func $reg_int (param i32 i32 i32 i32 i32)))
            (import "env" "_embind_register_class"
                (func $reg_class
                    (param i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32)))
            (import "env" "_embind_register_class_function"
                (func $reg_method (param i32 i32 i32 i32 i32 i32 i32 i32 i32)))
            (memory 1)
            (data (i32.const 100) "int\00")
            (data (i32.const 110) "Uint8List\00")
            (data (i32.const 130) "push_back\00")
            (data (i32.const 200) "\01\00\00\00\01\00\00\00")
            (func (export "__wasm_call_ctors")
                i32.const 1 i32.const 100 i32.const 4 i32.const 0 i32.const 0
                call $reg_int
                i32.const 9 i32.const 10 i32.const 11 i32.const 0 i32.const 0
                i32.const 0 i32.const 0 i32.const 0 i32.const 0 i32.const 0
                i32.const 110 i32.const 0 i32.const 0
                call $reg_class
                i32.const 9 i32.const 130 i32.const 2 i32.const 200
                i32.const 0 i32.const 0 i32.const 0 i32.const 0 i32.const 0
                call $reg_method))"#,
    );
    let (ok, stdout, stderr) = run(&["inspect", path.to_str().expect("utf-8 path")]);
    assert!(ok, "{stderr}");
    assert!(
        stdout.contains("class_function Uint8List::int push_back(int)"),
        "{stdout}"
    );
    assert!(stdout.contains("class Uint8List"), "{stdout}");
}

// ---- signatures ----

/// A body long enough to clear the fingerprint floor.
fn long_body() -> String {
    let mut text = String::new();
    for _ in 0..8 {
        text.push_str(" local.get 0 i32.const 1 i32.add local.set 0");
    }
    text.push_str(" local.get 0");
    text
}

#[test]
fn signatures_writes_a_catalogue_and_decompile_reads_it_back() {
    let named = fixture(
        "signatures-named",
        &format!(
            "(module (func $strlen_like (export \"a\") (param i32) (result i32){}))",
            long_body()
        ),
    );
    let (ok, stdout, stderr) = run(&["signatures", named.to_str().expect("utf-8 path")]);
    assert!(ok, "{stderr}");
    assert!(stdout.trim().ends_with(" strlen_like"), "{stdout}");
    let (fingerprint, _) = stdout.trim().split_once(' ').expect("<hex> <name>");
    assert_eq!(fingerprint.len(), 16, "padded, so the file sorts sensibly");

    // Written to a file, that file names the same function in a module that
    // has no name section of its own.
    let catalogue = scratch().join("catalogue.txt");
    let (ok, stdout, stderr) = run(&[
        "signatures",
        named.to_str().expect("utf-8 path"),
        "-o",
        catalogue.to_str().expect("utf-8 path"),
    ]);
    assert!(ok, "{stderr}");
    assert!(stdout.contains("1 signatures"), "{stdout}");

    let anonymous = fixture(
        "signatures-anonymous",
        &format!(
            "(module (func (export \"a\") (param i32) (result i32){}))",
            long_body()
        ),
    );
    let (ok, stdout, stderr) = run(&[
        "decompile",
        anonymous.to_str().expect("utf-8 path"),
        "--signatures",
        catalogue.to_str().expect("utf-8 path"),
    ]);
    assert!(ok, "{stderr}");
    assert!(stdout.contains("fn f0_strlen_like"), "{stdout}");
}

#[test]
fn signatures_says_so_when_there_is_nothing_to_catalogue() {
    let path = fixture("signatures-nothing", SAMPLE);
    let (ok, _, stderr) = run(&["signatures", path.to_str().expect("utf-8 path")]);
    assert!(!ok, "a catalogue of nothing is a mistake, not a result");
    assert!(stderr.contains("names none of its functions"), "{stderr}");
    assert!(stderr.contains("-g2"), "and says how to fix it: {stderr}");
}

#[test]
fn signatures_rejects_arguments_it_does_not_know() {
    let path = fixture("signatures-args", SAMPLE);
    let (ok, _, stderr) = run(&["signatures", path.to_str().expect("utf-8 path"), "--wat"]);
    assert!(!ok);
    assert!(stderr.contains("unexpected argument `--wat`"), "{stderr}");

    let (ok, _, stderr) = run(&["signatures"]);
    assert!(!ok);
    assert!(stderr.contains("needs a module path"), "{stderr}");

    let (ok, _, stderr) = run(&["signatures", path.to_str().expect("utf-8 path"), "-o"]);
    assert!(!ok);
    assert!(stderr.contains("-o needs a path"), "{stderr}");
}

#[test]
fn a_catalogue_that_is_not_one_is_refused_with_the_line_that_is_wrong() {
    let path = fixture("signatures-bad-catalogue", SAMPLE);
    let broken = scratch().join("broken.txt");

    std::fs::write(&broken, "deadbeef strlen\nnotafingerprint x\n").expect("writing");
    let (ok, _, stderr) = run(&[
        "decompile",
        path.to_str().expect("utf-8 path"),
        "--signatures",
        broken.to_str().expect("utf-8 path"),
    ]);
    assert!(!ok);
    assert!(
        stderr.contains(":2: `notafingerprint` is not a fingerprint"),
        "{stderr}"
    );

    std::fs::write(&broken, "\nlonely\n").expect("writing");
    let (ok, _, stderr) = run(&[
        "decompile",
        path.to_str().expect("utf-8 path"),
        "--signatures",
        broken.to_str().expect("utf-8 path"),
    ]);
    assert!(!ok);
    assert!(
        stderr.contains(":2: expected `<fingerprint> <name>`"),
        "a blank line is skipped, and the count still points at line 2: {stderr}"
    );

    let (ok, _, stderr) = run(&[
        "decompile",
        path.to_str().expect("utf-8 path"),
        "--signatures",
        "/nonexistent/catalogue.txt",
    ]);
    assert!(!ok);
    assert!(
        stderr.contains("reading /nonexistent/catalogue.txt"),
        "{stderr}"
    );

    let (ok, _, stderr) = run(&[
        "decompile",
        path.to_str().expect("utf-8 path"),
        "--signatures",
    ]);
    assert!(!ok);
    assert!(stderr.contains("--signatures needs a path"), "{stderr}");
}

// ---- reading affordances ----

#[test]
fn only_can_bring_the_functions_it_calls_along() {
    // Reading one function and needing the next is the same minute; a second
    // invocation for it is a minute more.
    let path = fixture(
        "with-callees",
        r#"(module
            (func $leaf (param i32) (result i32) local.get 0)
            (func $middle (param i32) (result i32) local.get 0 call $leaf)
            (func (export "top") (param i32) (result i32) local.get 0 call $middle))"#,
    );
    let alone = run(&[
        "decompile",
        path.to_str().expect("utf-8 path"),
        "--only",
        "2",
    ]);
    assert!(alone.0, "{}", alone.2);
    assert_eq!(alone.1.matches("was not decompiled").count(), 2);

    let with = run(&[
        "decompile",
        path.to_str().expect("utf-8 path"),
        "--only",
        "2",
        "--with-callees",
    ]);
    assert!(with.0, "{}", with.2);
    // `middle` comes along; `leaf`, which is two levels down, does not.
    assert_eq!(
        with.1.matches("was not decompiled").count(),
        1,
        "one level, not the transitive closure"
    );

    let (ok, _, stderr) = run(&[
        "decompile",
        path.to_str().expect("utf-8 path"),
        "--with-callees",
    ]);
    assert!(!ok);
    assert!(stderr.contains("expands --only"), "{stderr}");
}

#[test]
fn frames_lists_the_functions_that_write_outside_themselves() {
    let path = fixture(
        "frames-outside",
        r#"(module
            (memory 1)
            (global $sp (export "__stack_pointer") (mut i32) (i32.const 65536))
            (func (export "tidy") (local i32)
                global.get $sp i32.const 32 i32.sub local.tee 0 global.set $sp
                local.get 0 i32.const 1 i32.store offset=4
                local.get 0 i32.const 32 i32.add global.set $sp)
            (func (export "overrun") (local i32)
                global.get $sp i32.const 32 i32.sub local.tee 0 global.set $sp
                local.get 0 i32.const 1 i32.store offset=64
                local.get 0 i32.const 32 i32.add global.set $sp)
            (func (export "indexed") (param i32) (local i32)
                global.get $sp i32.const 32 i32.sub local.tee 1 global.set $sp
                local.get 1 local.get 0 i32.add i32.const 1 i32.store
                local.get 1 i32.const 32 i32.add global.set $sp))"#,
    );
    let (ok, stdout, stderr) = run(&["frames", path.to_str().expect("utf-8 path")]);
    assert!(ok, "{stderr}");
    assert!(stdout.contains("3 of 3 frames"), "{stdout}");
    assert!(stdout.contains("(address escapes)"), "{stdout}");

    let (ok, stdout, stderr) = run(&["frames", path.to_str().expect("utf-8 path"), "--outside"]);
    assert!(ok, "{stderr}");
    assert!(
        stdout.contains("writes frame + 64 (4 bytes) — outside"),
        "{stdout}"
    );
    assert!(
        stdout.contains("1 writes through a computed frame address"),
        "an indexed array in the frame cannot be checked, and says so: {stdout}"
    );
    assert!(stdout.contains("2 of 3 frames"), "{stdout}");
    assert!(
        !stdout.contains("f0 "),
        "the tidy one is not a suspect: {stdout}"
    );

    let (ok, _, stderr) = run(&["frames"]);
    assert!(!ok);
    assert!(stderr.contains("needs a module path"), "{stderr}");
    let (ok, _, stderr) = run(&["frames", path.to_str().expect("utf-8 path"), "--nope"]);
    assert!(!ok);
    assert!(stderr.contains("unexpected argument `--nope`"), "{stderr}");
}

#[test]
fn instrument_stores_routes_writes_through_the_watchpoint_runtime() {
    let path = fixture(
        "instrumented",
        r#"(module
            (memory 1)
            (func (export "write") (param i32) i32.const 8 local.get 0 i32.store))"#,
    );
    let (ok, plain, stderr) = run(&["decompile", path.to_str().expect("utf-8 path")]);
    assert!(ok, "{stderr}");
    assert!(!plain.contains("self.memory.store32_at("), "{plain}");

    let (ok, instrumented, stderr) = run(&[
        "decompile",
        path.to_str().expect("utf-8 path"),
        "--instrument-stores",
    ]);
    assert!(ok, "{stderr}");
    assert!(
        instrumented.contains("self.memory.store32_at(0,"),
        "{instrumented}"
    );
}

#[test]
fn calls_reports_the_number_of_call_sites() {
    let path = fixture(
        "call-sites",
        r#"(module
            (func $shared (result i32) i32.const 1)
            (func (export "caller") (result i32)
                call $shared drop
                call $shared))"#,
    );
    let (ok, stdout, stderr) = run(&["calls", path.to_str().expect("utf-8 path"), "0"]);
    assert!(ok, "{stderr}");
    assert!(stdout.contains("called by 1 from 2 call sites"), "{stdout}");
}

#[test]
fn offsets_writes_a_sidecar_beside_the_output() {
    let path = fixture("offsets-cli", SAMPLE);
    let directory = scratch().join("offsets-out");
    let _ = std::fs::remove_dir_all(&directory);
    let (ok, stdout, stderr) = run(&[
        "decompile",
        path.to_str().expect("utf-8 path"),
        "-o",
        directory.to_str().expect("utf-8 path"),
        "--offsets",
    ]);
    assert!(ok, "{stderr}{stdout}");
    let sidecar = std::fs::read_to_string(directory.join("offsets.json")).expect("written");
    assert!(sidecar.contains("\"lines\":"), "{sidecar}");

    // It has nowhere to go without a directory, and says so rather than
    // silently dropping it.
    let (ok, _, stderr) = run(&["decompile", path.to_str().expect("utf-8 path"), "--offsets"]);
    assert!(!ok);
    assert!(stderr.contains("needs -o <directory>"), "{stderr}");
}

#[test]
fn bytes_prints_what_is_there_and_whether_it_is_unique() {
    let path = fixture("bytes-cli", SAMPLE);
    let raw = std::fs::read(&path).expect("read");
    // The magic number: unique, and at a known place.
    let (ok, stdout, stderr) = run(&["bytes", path.to_str().expect("utf-8 path"), "0", "4"]);
    assert!(ok, "{stderr}");
    assert!(stdout.contains("0 (0x0) + 4: 00 61 73 6d"), "{stdout}");
    assert!(stdout.contains("1 occurrence"), "{stdout}");
    assert!(
        stdout.contains("unique, so a pattern patch is safe"),
        "{stdout}"
    );

    // A single byte that occurs many times says the opposite.
    let common_byte = raw.iter().filter(|byte| **byte == 0x00).count();
    assert!(common_byte > 1, "the fixture has repeated zero bytes");
    let (ok, stdout, _) = run(&["bytes", path.to_str().expect("utf-8 path"), "0", "1"]);
    assert!(ok);
    assert!(stdout.contains("would hit all of them"), "{stdout}");

    // Nothing asked for is nothing found, rather than a claim of uniqueness.
    let (ok, stdout, _) = run(&["bytes", path.to_str().expect("utf-8 path"), "0", "0"]);
    assert!(ok);
    assert!(stdout.contains("0 occurrences"), "{stdout}");

    // Past the end, and the argument errors.
    let (ok, _, stderr) = run(&["bytes", path.to_str().expect("utf-8 path"), "999999", "4"]);
    assert!(!ok);
    assert!(stderr.contains("is past the end of"), "{stderr}");
    let (ok, _, stderr) = run(&["bytes", path.to_str().expect("utf-8 path"), "x", "4"]);
    assert!(!ok);
    assert!(stderr.contains("offset must be a number"), "{stderr}");
    let (ok, _, stderr) = run(&["bytes", path.to_str().expect("utf-8 path"), "0", "y"]);
    assert!(!ok);
    assert!(stderr.contains("length must be a number"), "{stderr}");
    let (ok, _, stderr) = run(&["bytes", path.to_str().expect("utf-8 path"), "0"]);
    assert!(!ok);
    assert!(stderr.contains("needs a length"), "{stderr}");
    let (ok, _, stderr) = run(&["bytes", path.to_str().expect("utf-8 path")]);
    assert!(!ok);
    assert!(stderr.contains("needs an offset"), "{stderr}");
    let (ok, _, stderr) = run(&["bytes"]);
    assert!(!ok);
    assert!(stderr.contains("needs a module path"), "{stderr}");
    let (ok, _, stderr) = run(&[
        "bytes",
        path.to_str().expect("utf-8 path"),
        "0",
        "4",
        "more",
    ]);
    assert!(!ok);
    assert!(stderr.contains("unexpected argument `more`"), "{stderr}");
    let (ok, _, stderr) = run(&["bytes", "/nonexistent.wasm", "0", "4"]);
    assert!(!ok);
    assert!(stderr.contains("reading /nonexistent.wasm"), "{stderr}");
}

#[test]
fn constants_finds_every_site_that_pushes_a_value() {
    // The measurement this exists for: give each site a distinct value, run,
    // and see which one comes back. A hand-counted subset answers the wrong
    // question, and an error code that is also a data constant makes counting
    // its bytes the wrong method too.
    let path = fixture(
        "constants-cli",
        r#"(module
            (memory 1)
            (data (i32.const 0) "\78\11\01\00")
            (func (export "one") (result i32) i32.const 70008)
            (func (export "two") (result i32)
                i32.const 70008 drop
                i32.const 70009)
            (func (export "wide") (result i64) i64.const 70008))"#,
    );
    let (ok, stdout, stderr) = run(&["constants", path.to_str().expect("utf-8 path"), "70008"]);
    assert!(ok, "{stderr}");
    assert_eq!(stdout.matches("i32.const at ").count(), 2, "{stdout}");
    assert!(stdout.contains("i64.const at "), "{stdout}");
    assert!(stdout.contains("3 sites push 70008"), "{stdout}");
    assert!(
        stdout.contains("four bytes appear 1 more time inside the data segments"),
        "counting bytes is not counting sites: {stdout}"
    );
    // Every site says where it is and how long it is, which is what a
    // same-length replacement needs.
    assert!(stdout.contains(" + 4 bytes"), "{stdout}");

    let (ok, stdout, _) = run(&["constants", path.to_str().expect("utf-8 path"), "12345"]);
    assert!(ok);
    assert!(stdout.contains("0 sites push 12345"), "{stdout}");
    assert!(!stdout.contains("data segments"), "{stdout}");

    let (ok, _, stderr) = run(&["constants", path.to_str().expect("utf-8 path")]);
    assert!(!ok);
    assert!(stderr.contains("needs a value"), "{stderr}");
    let (ok, _, stderr) = run(&["constants", path.to_str().expect("utf-8 path"), "x"]);
    assert!(!ok);
    assert!(stderr.contains("value must be a number"), "{stderr}");
    let (ok, _, stderr) = run(&["constants"]);
    assert!(!ok);
    assert!(stderr.contains("needs a module path"), "{stderr}");
    let (ok, _, stderr) = run(&["constants", path.to_str().expect("utf-8 path"), "1", "2"]);
    assert!(!ok);
    assert!(stderr.contains("unexpected argument `2`"), "{stderr}");
}

#[test]
fn reachable_from_compiles_a_path_and_stubs_the_rest() {
    let path = fixture(
        "reachable-cli",
        r#"(module
            (func $leaf (param i32) (result i32) local.get 0)
            (func (export "entry") (param i32) (result i32) local.get 0 call $leaf)
            (func (export "elsewhere") (result i32) i32.const 9))"#,
    );
    let (ok, stdout, stderr) = run(&[
        "decompile",
        path.to_str().expect("utf-8 path"),
        "--reachable-from",
        "1",
    ]);
    assert!(ok, "{stderr}");
    // `entry` and the leaf it calls are there; the unrelated export is not.
    assert_eq!(stdout.matches("was not decompiled").count(), 1, "{stdout}");

    // Several starting points, and the module's own `start` if it has one.
    let (ok, stdout, stderr) = run(&[
        "decompile",
        path.to_str().expect("utf-8 path"),
        "--reachable-from",
        "1,2",
    ]);
    assert!(ok, "{stderr}");
    assert_eq!(stdout.matches("was not decompiled").count(), 0, "{stdout}");

    let (ok, _, stderr) = run(&[
        "decompile",
        path.to_str().expect("utf-8 path"),
        "--reachable-from",
        "99",
    ]);
    assert!(!ok);
    assert!(stderr.contains("#99 is not one of them"), "{stderr}");

    let (ok, _, stderr) = run(&[
        "decompile",
        path.to_str().expect("utf-8 path"),
        "--reachable-from",
        "x",
    ]);
    assert!(!ok);
    assert!(stderr.contains("takes indices, not `x`"), "{stderr}");

    let (ok, _, stderr) = run(&[
        "decompile",
        path.to_str().expect("utf-8 path"),
        "--reachable-from",
    ]);
    assert!(!ok);
    assert!(stderr.contains("needs a function index"), "{stderr}");

    let (ok, _, stderr) = run(&[
        "decompile",
        path.to_str().expect("utf-8 path"),
        "--direct-only",
    ]);
    assert!(!ok);
    assert!(stderr.contains("modifies --reachable-from"), "{stderr}");

    let (ok, _, stderr) = run(&[
        "decompile",
        path.to_str().expect("utf-8 path"),
        "--reachable-from",
        ",",
    ]);
    assert!(!ok);
    assert!(stderr.contains("at least one index"), "{stderr}");
}

#[test]
fn a_reachable_set_always_includes_what_instantiation_runs() {
    // `start` runs before anything the caller asks for, so leaving it out
    // means the first thing that happens is a stub.
    let path = fixture(
        "reachable-start",
        r#"(module
            (global $g (mut i32) (i32.const 0))
            (func $init i32.const 7 global.set $g)
            (func (export "read") (result i32) global.get $g)
            (start $init))"#,
    );
    for arguments in [
        vec!["--reachable-from", "1", "--direct-only"],
        vec!["--reachable-from", "1"],
    ] {
        let mut all = vec!["decompile", path.to_str().expect("utf-8 path")];
        all.extend(arguments);
        let (ok, stdout, stderr) = run(&all);
        assert!(ok, "{stderr}");
        assert_eq!(
            stdout.matches("was not decompiled").count(),
            0,
            "the start function came along: {stdout}"
        );
    }
}

#[test]
fn direct_only_leaves_out_what_the_table_could_reach() {
    let path = fixture(
        "reachable-direct-cli",
        r#"(module
            (type $unary (func (param i32) (result i32)))
            (func (export "entry") (param i32) (result i32)
                local.get 0 i32.const 0 call_indirect (type $unary))
            (func (type $unary) local.get 0 i32.const 2 i32.mul)
            (table 1 funcref)
            (elem (i32.const 0) func 1))"#,
    );
    let (ok, whole, _) = run(&[
        "decompile",
        path.to_str().expect("utf-8 path"),
        "--reachable-from",
        "0",
    ]);
    assert!(ok);
    assert_eq!(whole.matches("was not decompiled").count(), 0, "{whole}");

    let (ok, direct, _) = run(&[
        "decompile",
        path.to_str().expect("utf-8 path"),
        "--reachable-from",
        "0",
        "--direct-only",
    ]);
    assert!(ok);
    assert_eq!(
        direct.matches("was not decompiled").count(),
        1,
        "the table's target is left out, and the stub names it: {direct}"
    );
}

#[test]
fn patch_rewrites_a_constant_without_moving_anything() {
    // The technique `constants` exists for: give a site a distinct value, run,
    // and see which one comes back. Doing it by hand means computing a signed
    // LEB128, which is where it goes wrong.
    let path = fixture(
        "patch-cli",
        r#"(module
            (func (export "code") (result i32) i32.const 70008)
            (func (export "other") (result i32) i32.const 70008))"#,
    );
    let (ok, stdout, stderr) = run(&["constants", path.to_str().expect("utf-8 path"), "70008"]);
    assert!(ok, "{stderr}");
    let offset: usize = stdout
        .lines()
        .find_map(|line| line.split("at ").nth(1))
        .and_then(|rest| rest.split(' ').next())
        .and_then(|number| number.parse().ok())
        .expect("constants gives an offset");

    let patched = scratch().join("patched.wasm");
    let (ok, stdout, stderr) = run(&[
        "patch",
        path.to_str().expect("utf-8 path"),
        &offset.to_string(),
        "200391",
        "-o",
        patched.to_str().expect("utf-8 path"),
    ]);
    assert!(ok, "{stderr}");
    assert!(stdout.contains("is now i32.const 200391"), "{stdout}");
    assert!(stdout.contains("unmoved"), "{stdout}");

    // Same size, and only the three bytes of the encoding differ.
    let before = std::fs::read(&path).expect("read");
    let after = std::fs::read(&patched).expect("read");
    assert_eq!(before.len(), after.len(), "nothing moved");
    let changed = before
        .iter()
        .zip(&after)
        .filter(|(one, two)| one != two)
        .count();
    assert_eq!(changed, 3, "the SLEB is three bytes wide either way");

    // And the result is a module, not a pile of bytes that used to be one.
    let (ok, stdout, _) = run(&["constants", patched.to_str().expect("utf-8 path"), "200391"]);
    assert!(ok);
    assert!(stdout.contains("1 site push 200391"), "{stdout}");
    let (ok, stdout, _) = run(&["constants", patched.to_str().expect("utf-8 path"), "70008"]);
    assert!(ok);
    assert!(
        stdout.contains("1 site push 70008"),
        "the other one is untouched: {stdout}"
    );
}

#[test]
fn patch_refuses_what_would_move_the_rest_of_the_module() {
    let path = fixture(
        "patch-refuse",
        r#"(module (func (export "code") (result i32) i32.const 70008))"#,
    );
    let (_, stdout, _) = run(&["constants", path.to_str().expect("utf-8 path"), "70008"]);
    let offset: usize = stdout
        .lines()
        .find_map(|line| line.split("at ").nth(1))
        .and_then(|rest| rest.split(' ').next())
        .and_then(|number| number.parse().ok())
        .expect("an offset");
    let out = scratch().join("refused.wasm");
    let out = out.to_str().expect("utf-8 path");

    // A value that encodes shorter would move every byte after it.
    let (ok, _, stderr) = run(&[
        "patch",
        path.to_str().expect("utf-8 path"),
        &offset.to_string(),
        "1",
        "-o",
        out,
    ]);
    assert!(!ok);
    assert!(stderr.contains("encodes to 1 bytes"), "{stderr}");
    assert!(stderr.contains("moves everything after it"), "{stderr}");

    // An offset that is not an i32.const at all.
    let (ok, _, stderr) = run(&[
        "patch",
        path.to_str().expect("utf-8 path"),
        "0",
        "5",
        "-o",
        out,
    ]);
    assert!(!ok);
    assert!(stderr.contains("not an i32.const"), "{stderr}");

    let (ok, _, stderr) = run(&[
        "patch",
        path.to_str().expect("utf-8 path"),
        "99999",
        "5",
        "-o",
        out,
    ]);
    assert!(!ok);
    assert!(stderr.contains("past the end"), "{stderr}");

    // And the arguments themselves.
    for (arguments, expected) in [
        (vec!["patch"], "needs a module path"),
        (vec!["patch", "m.wasm"], "needs the offset"),
        (vec!["patch", "m.wasm", "0"], "needs the value"),
        (vec!["patch", "m.wasm", "x", "1"], "offset must be a number"),
        (vec!["patch", "m.wasm", "0", "y"], "value must be a number"),
    ] {
        let (ok, _, stderr) = run(&arguments);
        assert!(!ok, "{arguments:?}");
        assert!(stderr.contains(expected), "{arguments:?}: {stderr}");
    }
    let (ok, _, stderr) = run(&["patch", path.to_str().expect("utf-8 path"), "0", "5"]);
    assert!(!ok);
    assert!(stderr.contains("needs -o <path>"), "{stderr}");
    let (ok, _, stderr) = run(&[
        "patch",
        path.to_str().expect("utf-8 path"),
        &offset.to_string(),
        "70009",
        "-o",
        out,
        "--twice",
    ]);
    assert!(!ok);
    assert!(stderr.contains("unexpected argument `--twice`"), "{stderr}");
}

#[test]
fn patch_handles_the_whole_range_of_a_signed_leb() {
    // The encoding is the thing that goes wrong by hand, so it is the thing to
    // pin: negative values, the sign-bit boundaries, and a five-byte constant.
    for value in [
        -1i64,
        63,
        64,
        -64,
        -65,
        8191,
        -8192,
        775_533,
        70_008,
        2_147_483_647,
        -2_147_483_648,
    ] {
        let path = fixture(
            "patch-range",
            &format!("(module (func (export \"code\") (result i32) i32.const {value}))"),
        );
        let (_, stdout, _) = run(&[
            "constants",
            path.to_str().expect("utf-8 path"),
            &value.to_string(),
        ]);
        let offset: usize = stdout
            .lines()
            .find_map(|line| line.split("at ").nth(1))
            .and_then(|rest| rest.split(' ').next())
            .and_then(|number| number.parse().ok())
            .unwrap_or_else(|| panic!("no offset for {value}: {stdout}"));
        // Rewriting it as itself must produce the same bytes: if the encoder
        // and the decoder disagree anywhere, this is where it shows.
        let out = scratch().join("range.wasm");
        let (ok, _, stderr) = run(&[
            "patch",
            path.to_str().expect("utf-8 path"),
            &offset.to_string(),
            &value.to_string(),
            "-o",
            out.to_str().expect("utf-8 path"),
        ]);
        assert!(ok, "{value}: {stderr}");
        assert_eq!(
            std::fs::read(&path).expect("read"),
            std::fs::read(&out).expect("read"),
            "re-encoding {value} changed the module"
        );
    }
}

#[test]
fn patch_refuses_a_constant_that_does_not_decode() {
    // Not a module: a byte that says i32.const followed by continuation bytes
    // that never end. `patch` reads raw bytes, so it has to answer for this
    // rather than assume a decoder already checked it.
    let path = scratch().join("broken.wasm");
    let mut bytes = vec![0x41u8];
    bytes.extend(std::iter::repeat_n(0x80u8, 12));
    std::fs::write(&path, &bytes).expect("writing");
    let (ok, _, stderr) = run(&[
        "patch",
        path.to_str().expect("utf-8 path"),
        "0",
        "1",
        "-o",
        scratch().join("out.wasm").to_str().expect("utf-8 path"),
    ]);
    assert!(!ok);
    assert!(stderr.contains("does not decode"), "{stderr}");

    // And one that runs off the end of the file.
    std::fs::write(&path, [0x41u8, 0x80]).expect("writing");
    let (ok, _, stderr) = run(&[
        "patch",
        path.to_str().expect("utf-8 path"),
        "0",
        "1",
        "-o",
        scratch().join("out.wasm").to_str().expect("utf-8 path"),
    ]);
    assert!(!ok);
    assert!(stderr.contains("does not decode"), "{stderr}");
}
