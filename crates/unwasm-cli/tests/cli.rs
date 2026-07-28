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
    assert!(stdout.contains("2 files"), "{stdout}");
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
    assert!(stdout.contains("1 files"), "{stdout}");
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
    assert!(stdout.contains("1 methods to implement"), "{stdout}");
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
