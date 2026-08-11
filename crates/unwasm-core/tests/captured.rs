//! The real modules.
//!
//! Fixtures are what a decompiler is developed against; shipped modules are what
//! it is judged by. These run over the WhatsApp Web captures — minified,
//! Emscripten-built, up to 10 MiB — and they found the two bugs that fixtures
//! did not: a value spilled inside a block and consumed after it, and an
//! `Imports` trait emitted without return types.
//!
//! They are `#[ignore]`d because they are slow, not because they are optional:
//!
//! ```sh
//! ./scripts/fetch-captures.sh
//! cargo test --test captured -- --ignored --nocapture
//! ```
//!
//! The captures are megabytes of somebody else's build output, so they are not
//! committed: `scripts/fetch-captures.sh` downloads them into `fixtures/wasm`
//! from the public `oxidezap/whatspec` archive and checks each against a pinned
//! sha256. `WA_WASM_DIR` points somewhere else.
//!
//! WhatsApp rolls these payloads. An id that stops being served is replaced
//! here by the build that succeeded it, and every number a test pins to it is
//! re-read against the new one rather than carried over.

mod common;

use unwasm_core::{Module, codegen};

/// Every capture, with what is known about it.
const CAPTURES: &[(&str, &str)] = &[
    (common::captures::SMALLEST, "VOPRF / crypto, 236 KiB"),
    ("php8T1oSIZM", "mozjpeg, 376 KiB"),
    ("a19OxQ3jkd2", "3.3 MiB, and the only WASI one"),
    ("ayqr5HQtlkb", "2.0 MiB"),
    ("rogm88TRRiw", "2.1 MiB"),
    (
        common::captures::VOIP,
        "VoIP / PJSIP, 10.2 MiB — a shared imported memory and 134 trampolines",
    ),
];

#[test]
#[ignore = "reads the capture directory and decompiles several megabytes"]
fn every_capture_either_decompiles_or_says_why_not() {
    let mut missing = Vec::new();
    for (id, description) in CAPTURES {
        let Some(bytes) = common::captured(id) else {
            missing.push(*id);
            continue;
        };
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
                // A refusal is a result, as long as it names the construct.
                // What must never happen is a module that decompiles into
                // something that does not mean the same thing.
                let message = error.to_string();
                assert!(message.contains("unsupported"), "{id}: {message}");
                eprintln!("{id}: refused — {message}");
            }
        }
    }
    // The whole corpus, not whatever happened to be on the disk. A run that
    // covered four of six modules and reported the same green as one that
    // covered all six is the failure this tier exists to make visible — and
    // now that the captures are fetchable, a partial corpus is a fixable state
    // rather than a fact of the machine.
    assert!(
        missing.is_empty(),
        "these captures were not found: {}.\n\
         Run `scripts/fetch-captures.sh`, or set WA_WASM_DIR to a directory \
         that already holds them.",
        missing.join(", ")
    );
}

/// The smallest capture, taken all the way: decompiled, compiled by rustc, and
/// instantiated. Instantiation runs the module's own `__wasm_call_ctors` and
/// every static initialiser with it, which is a far broader exercise of the
/// generated code than any fixture.
#[test]
#[ignore = "compiles 70k lines of generated Rust"]
fn the_smallest_capture_compiles_and_instantiates() {
    let id = common::captures::SMALLEST;
    let Some(bytes) = common::captured(id) else {
        panic!("{}", common::missing_capture(id));
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

/// How much of a real module level 1 can actually place — measured, on the
/// corpus, rather than estimated.
///
/// The number is the argument for the level being opt-in. Promotion needs a
/// frame nothing else in the function can reach, and compiled C reaches its
/// frames constantly: it passes `&point` to something, indexes an array in it,
/// or packs two variables into one word. What survives all of that is a
/// minority, and the size of the minority is what this prints.
#[test]
#[ignore = "reads the capture directory"]
fn how_much_of_each_capture_level_1_can_place() {
    let mut missing = Vec::new();
    for (id, description) in CAPTURES {
        let Some(bytes) = common::captured(id) else {
            missing.push(*id);
            continue;
        };
        let module = Module::parse(&bytes).expect("parses");
        let analysis = unwasm_core::analysis::analyse(&module);
        let import_count = module.func_imports.len() as u32;
        let (mut promotable, mut slots, mut promoted) = (0usize, 0usize, 0usize);
        for (index, frame) in &analysis.frames {
            slots += frame.slots.len();
            let body = &module.funcs[(index - import_count) as usize].body;
            let placed = unwasm_core::analysis::promotable_slots(frame, body);
            if !placed.is_empty() {
                promotable += 1;
            }
            promoted += placed.len();
        }
        let frames = analysis.frames.len();
        eprintln!(
            "{id}: {promotable} of {frames} frames have something to promote, \
             {promoted} of {slots} slots — {description}"
        );
        // The claim is only that the analysis answers, not that it answers
        // generously: a module where nothing can be placed is a real result.
        assert!(promoted <= slots);
    }
    assert!(
        missing.is_empty(),
        "these captures were not found: {missing:?}"
    );
}

/// The VoIP module's own allocator, run.
///
/// Everything else in this tier reads the big capture or compiles a small one.
/// This one *runs* the big one: Emscripten's dlmalloc as it was shipped, sliced
/// out of 14733 functions, compiled by rustc, and asked the same questions the
/// engine is asked — with the whole of linear memory compared afterwards, which
/// is where an allocator's real answer lives. A boundary tag written one byte
/// out returns the same pointer and a different heap.
///
/// `free` is not called with an address, because the harness's calls are
/// literals and the address is whichever one the run produces. What it is
/// called with is 0, which the standard defines as doing nothing — and which a
/// wrong translation of the null check would not.
#[test]
#[ignore = "reads the capture directory and compiles a slice of a 10.2 MiB module"]
fn the_voip_modules_own_allocator_agrees_with_the_engine() {
    let id = common::captures::VOIP;
    let Some(bytes) = common::captured(id) else {
        panic!("{}", common::missing_capture(id));
    };
    let calls = vec![
        common::call("malloc", &[common::Arg::I32(64)]),
        common::call("malloc", &[common::Arg::I32(64)]),
        common::call("malloc", &[common::Arg::I32(1024)]),
        // Zero bytes is still an allocation, and a distinct one.
        common::call("malloc", &[common::Arg::I32(0)]),
        // More than the address space holds: a null, not a wrap to something
        // small, and not a growth of the memory.
        common::call("malloc", &[common::Arg::I32(-1)]),
        common::call("free", &[common::Arg::I32(0)]),
        common::call(
            "emscripten_builtin_memalign",
            &[common::Arg::I32(64), common::Arg::I32(128)],
        ),
        common::call("__errno_location", &[]),
    ];
    common::assert_agrees_over_reachable("capture-voip-malloc", &bytes, &calls);
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
    let id = common::captures::SMALLEST;
    let Some(bytes) = common::captured(id) else {
        panic!("{}", common::missing_capture(id));
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
        "{id}: {} files, {} bytes of memory after instantiation",
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
    let id = common::captures::SMALLEST;
    let Some(capture) = common::captured(id) else {
        panic!("{}", common::missing_capture(id));
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
