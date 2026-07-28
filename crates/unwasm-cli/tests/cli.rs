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
        "imported-memory",
        "(module (import \"env\" \"memory\" (memory 1)))",
    );
    let (ok, _, stderr) = run(&["decompile", path.to_str().expect("utf-8 path")]);
    assert!(!ok);
    assert!(stderr.contains("imported memory"), "{stderr}");
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
