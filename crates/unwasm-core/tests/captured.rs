//! The real modules.
//!
//! Fixtures are what a decompiler is developed against; shipped modules are what
//! it is judged by. These run over the WhatsApp Web captures — minified,
//! Emscripten-built, up to 9 MiB — and they found the two bugs that fixtures did
//! not: a value spilled inside a block and consumed after it, and an `Imports`
//! trait emitted without return types.
//!
//! They are `#[ignore]`d because they are slow, not because they are optional:
//!
//! ```sh
//! cargo test --test captured -- --ignored --nocapture
//! ```
//!
//! The captures live in the whatsapp-rust checkout and are not copied here — a
//! second copy drifts from the one the protocol notes refer to. `WA_WASM_DIR`
//! points somewhere else.

mod common;

use unwasm_core::{Module, codegen};

/// Every capture, with what is known about it.
const CAPTURES: &[(&str, &str)] = &[
    ("COs9e0Kj0ic", "VOPRF / crypto, 236 KiB"),
    ("php8T1oSIZM", "mozjpeg, 376 KiB"),
    ("9Nbh3eMuVjD", "2.9 MiB"),
    ("ayqr5HQtlkb", "2.0 MiB"),
    ("rogm88TRRiw", "2.1 MiB"),
    (
        "D5pLH9sfOOl",
        "VoIP / PJSIP, 9.4 MiB — a shared imported memory and 1070 atomics",
    ),
];

#[test]
#[ignore = "reads the capture directory and decompiles several megabytes"]
fn every_capture_either_decompiles_or_says_why_not() {
    let mut seen = 0;
    for (id, description) in CAPTURES {
        let Some(bytes) = common::captured(id) else {
            eprintln!("skipping: {id} unavailable (set WA_WASM_DIR)");
            continue;
        };
        seen += 1;
        match Module::parse(&bytes) {
            Ok(module) => {
                let code = codegen::generate(&module).unwrap_or_else(|error| {
                    panic!("{id} ({description}) parsed but did not generate: {error}")
                });
                assert!(code.contains("pub struct Instance"));
                eprintln!(
                    "{id}: {} functions, {} lines of Rust — {description}",
                    module.funcs.len(),
                    code.lines().count()
                );
            }
            Err(error) => {
                // A refusal is a result, as long as it names the construct. The
                // VoIP module imports its memory, which this version does not
                // model; what must never happen is a module that decompiles
                // into something that does not mean the same thing.
                let message = error.to_string();
                assert!(message.contains("unsupported"), "{id}: {message}");
                eprintln!("{id}: refused — {message}");
            }
        }
    }
    assert!(
        seen > 0,
        "no captures were found; this test compared nothing. Set WA_WASM_DIR."
    );
}

/// The smallest capture, taken all the way: decompiled, compiled by rustc, and
/// instantiated. Instantiation runs the module's own `__wasm_call_ctors` and
/// every static initialiser with it, which is a far broader exercise of the
/// generated code than any fixture.
#[test]
#[ignore = "compiles 70k lines of generated Rust"]
fn the_smallest_capture_compiles_and_instantiates() {
    let Some(bytes) = common::captured("COs9e0Kj0ic") else {
        panic!("COs9e0Kj0ic is not available; set WA_WASM_DIR");
    };
    const DRIVER: &str = r#"
mod generated;
fn main() {
    let instance = generated::Instance::new();
    println!("{}", instance.memory.data.len());
}
"#;
    let output = common::run_with_driver("capture-crypto", &bytes, DRIVER);
    let pages: usize = output.trim().parse().expect("a memory size");
    assert!(pages > 0, "the module instantiated with an empty memory");
    assert_eq!(pages % 65536, 0, "memory is a whole number of pages");
}

/// A real module in the split layout, compiled and instantiated.
///
/// The layout is forced rather than taken from the module's size: 478
/// functions compile quickly enough whole that the default leaves them in one
/// file, and the claim under test is that the *parts* hold together on
/// something nobody wrote to be easy.
#[test]
#[ignore = "compiles 70k lines of generated Rust"]
fn a_capture_compiles_in_the_split_layout() {
    let Some(bytes) = common::captured("COs9e0Kj0ic") else {
        panic!("COs9e0Kj0ic is not available; set WA_WASM_DIR");
    };
    let module = Module::parse(&bytes).expect("it parses");
    let layout = codegen::Layout::Split {
        lines_per_file: 400,
    };
    let files = codegen::generate_files(&module, layout).expect("it generates");
    assert!(
        files.len() > 5,
        "expected several parts, got {}",
        files.len()
    );

    const DRIVER: &str = r#"
mod generated;
fn main() {
    let instance = generated::Instance::new();
    println!("{}", instance.memory.data.len());
}
"#;
    let output = common::run_with_driver_in_layout("capture-split", &bytes, DRIVER, layout);
    let bytes_of_memory: usize = output.trim().parse().expect("a memory size");
    assert_eq!(bytes_of_memory % 65536, 0);
    eprintln!(
        "COs9e0Kj0ic: {} files, {} bytes of memory after instantiation",
        files.len(),
        bytes_of_memory
    );
}

#[test]
#[ignore = "needs emcc and the capture directory"]
fn a_catalogue_from_our_own_emscripten_names_musl_in_a_capture() {
    // End to end, at the scale it is meant for: build a reference with names,
    // catalogue it, and apply it to a stripped 236 KiB capture that was built
    // by a different emscripten version. The numbers here are the honest ones —
    // a handful out of hundreds — and the point of the test is that the
    // pipeline works and that a match is a *plausible* one, not that recall is
    // good. See the fingerprint doc comment for the measurements.
    let Some(capture) = common::captured("COs9e0Kj0ic") else {
        eprintln!("skipping: capture unavailable (set WA_WASM_DIR)");
        return;
    };
    let reference = common::compile_emscripten(
        "signature-reference",
        r#"#include <stdio.h>
           #include <stdlib.h>
           #include <string.h>
           #include <math.h>
           // Exported rather than `main`, because the harness passes
           // `--no-entry` and an entry point nothing exports is dropped whole.
           __attribute__((export_name("go")))
           int go(int n) {
               char *p = malloc(64);
               strcpy(p, "x");
               printf("%s %f %d\n", p, sqrt((double)n), (int)strlen(p));
               free(p);
               return n;
           }"#,
        "c",
        // `-g2` keeps the name section; without it there is nothing to
        // catalogue, and `signatures` says so rather than writing an empty file.
        &["-O2", "-g2"],
    );

    let reference = Module::parse(&reference).expect("the reference decodes");
    let catalogue = codegen::extract_signatures(&reference);
    assert!(
        catalogue.len() > 5,
        "a libc of one printf is still a few functions: {catalogue:?}"
    );

    let capture = Module::parse(&capture).expect("the capture decodes");
    let named = codegen::generate_with_signatures(&capture, codegen::Layout::Single, &catalogue)
        .expect("generates");
    let plain = codegen::generate_files(&capture, codegen::Layout::Single).expect("generates");

    let recognised: Vec<&str> = named[0]
        .contents
        .lines()
        .filter_map(|line| line.split_once("recognised as `"))
        .filter_map(|(_, rest)| rest.split_once('`'))
        .map(|(name, _)| name)
        .collect();
    eprintln!("recognised {}: {recognised:?}", recognised.len());
    assert!(
        !recognised.is_empty(),
        "across emscripten versions this is a handful, but it is not zero"
    );
    assert!(
        recognised
            .iter()
            .all(|name| catalogue.values().any(|v| v == name)),
        "every name claimed came from the catalogue"
    );

    // A name is a name: with the doc comments dropped and the recognised names
    // spelled back out of the identifiers, the two are the same file. This is
    // the same property the differential tests hold over the annotations —
    // recognising a function must not change what it does.
    let strip = |text: &str| -> String {
        let mut text: String = text
            .lines()
            .filter(|line| !line.trim_start().starts_with("///"))
            .map(|line| format!("{line}\n"))
            .collect();
        for name in &recognised {
            text = text.replace(&format!("_{name}"), "");
        }
        text
    };
    assert_eq!(strip(&named[0].contents), strip(&plain[0].contents));
}
