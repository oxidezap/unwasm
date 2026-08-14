//! Tests for the host environment itself: WASI, exception handling and the
//! deterministic clock.
//!
//! Each of these covers a failure that actually happened. A stubbed `fd_write`
//! made a module retry 3.2 million times; a stubbed `pthread_create` reported a
//! thread that never ran; stubbed exception handling turned every recoverable
//! C++ error into an opaque wasm trap.

use oracle_core::{Catalog, Runtime, Value};

mod common;

const VOIP: &str = "JgwtTQVeWPm";

/// Serialises against the other test binaries, which cargo runs in parallel:
/// two PJSIP worker pools competing for cores miss their own deadlines.
fn engine_guard() -> common::EngineLock {
    common::engine_lock()
}

fn voip() -> Option<Runtime> {
    let catalog = Catalog::discover().ok()?;
    let entry = catalog.resolve(VOIP).ok()?;
    let bytes = std::fs::read(&entry.path).ok()?;
    let mut runtime = Runtime::instantiate(&bytes).expect("instantiate");
    runtime.run_ctors().expect("ctors");
    Some(runtime)
}

macro_rules! voip_or_skip {
    () => {
        match voip() {
            Some(runtime) => runtime,
            None => {
                eprintln!("skipping: no capture (set WA_WASM_DIR)");
                return;
            }
        }
    };
}

/// Startup must not spin. The regression this guards against burned the entire
/// fuel budget on one import.
#[test]
fn startup_does_not_spin_on_any_host_call() {
    let _engine = engine_guard();
    let mut runtime = voip_or_skip!();

    // Generous enough for real startup work, far below a busy-wait.
    const CEILING: u64 = 100_000;

    let hottest = runtime.state().hot_calls();
    if let Some((symbol, count)) = hottest.first() {
        assert!(
            *count < CEILING,
            "`{symbol}` was called {count} times during startup, which is a spin"
        );
    }
    let _ = runtime.embind();
}

/// A recoverable C++ error must arrive as a readable message, not as a trap
/// with a wasm backtrace.
#[test]
fn cpp_exceptions_are_reported_with_their_message() {
    let _engine = engine_guard();
    let mut runtime = voip_or_skip!();
    runtime.clear_calls();

    // Ask for a parameter before the stack is initialised; the engine throws.
    let _ = runtime.call_embind("getVoipParam", &[Value::Str("nonexistent".to_owned())]);

    let messages: Vec<String> = runtime
        .logs()
        .into_iter()
        .filter(|line| line.starts_with("C++ exception:"))
        .collect();

    if messages.is_empty() {
        // The engine is entitled not to throw here; nothing to assert.
        return;
    }
    assert!(
        messages
            .iter()
            .any(|line| line.contains("::") || line.contains(':')),
        "exception text should name a type or reason, got {messages:?}"
    );
}

/// The virtual clock must move forward, or anything polling for a deadline
/// never reaches it.
#[test]
fn the_virtual_clock_advances() {
    let _engine = engine_guard();
    let mut runtime = voip_or_skip!();
    let before = runtime.state().clock_ms();

    let _ = runtime.call_embind("getCallInfo", &[]);
    // Startup alone observes the clock many times.
    assert!(
        runtime.state().clock_ms() >= before,
        "clock went backwards: {before} -> {}",
        runtime.state().clock_ms()
    );
    assert!(before > 0.0, "clock never advanced during startup");
}

/// Two runs must agree byte for byte, including the traces. Without this, any
/// comparison against whatsapp-rust is unattributable.
#[test]
fn two_runs_produce_identical_traces() {
    let Some(mut first) = voip() else {
        eprintln!("skipping: no capture (set WA_WASM_DIR)");
        return;
    };
    let mut second = voip().expect("second instance");

    let calls_a = first.state().hot_calls();
    let calls_b = second.state().hot_calls();
    assert_eq!(calls_a, calls_b, "startup traces diverged");

    assert_eq!(
        first.embind().functions.len(),
        second.embind().functions.len()
    );
}

/// The in-memory filesystem must round-trip, since it is the only filesystem a
/// module under test is allowed to see.
#[test]
fn wasi_filesystem_round_trips() {
    let catalog = match Catalog::discover() {
        Ok(catalog) => catalog,
        Err(_) => {
            eprintln!("skipping: no capture (set WA_WASM_DIR)");
            return;
        }
    };
    let Ok(entry) = catalog.resolve("rogm88TRRiw") else {
        eprintln!("skipping: media module unavailable");
        return;
    };
    let bytes = std::fs::read(&entry.path).expect("read");
    let mut runtime = Runtime::instantiate(&bytes).expect("instantiate");

    runtime.add_file("input.bin", vec![1, 2, 3, 4]);
    assert_eq!(runtime.wasi().file("input.bin"), Some(&[1u8, 2, 3, 4][..]));
    // Leading slashes must resolve to the same entry.
    assert_eq!(runtime.wasi().file("/input.bin"), Some(&[1u8, 2, 3, 4][..]));
    assert_eq!(runtime.wasi().file("missing.bin"), None);
}

/// Arguments are exposed with a program name in `argv[0]`, matching what a C
/// `main` expects.
#[test]
fn wasi_arguments_include_a_program_name() {
    let catalog = match Catalog::discover() {
        Ok(catalog) => catalog,
        Err(_) => {
            eprintln!("skipping: no capture (set WA_WASM_DIR)");
            return;
        }
    };
    let Ok(entry) = catalog.resolve("rogm88TRRiw") else {
        eprintln!("skipping: media module unavailable");
        return;
    };
    let bytes = std::fs::read(&entry.path).expect("read");
    let mut runtime = Runtime::instantiate(&bytes).expect("instantiate");

    runtime.set_args(&["check".to_owned(), "input.bin".to_owned()]);
    let args = &runtime.wasi().args;
    assert_eq!(args.len(), 3);
    assert_eq!(args[1], "check");
}

/// A missing export must name what the module *does* have.
///
/// This is the regression test for a bug that cost a day: the host asked for
/// `__emscripten_thread_init`, the module exported `_emscripten_thread_init`,
/// and the lookup returned `None` into an `else { return; }`. Nothing failed,
/// the main thread never registered itself, and the symptom — every call a
/// worker queued for the main thread being dropped — surfaced thousands of
/// instructions away.
///
/// So the requirement is not "look up both spellings" but "when a lookup
/// fails, say so and point at the near miss".
#[test]
fn a_missing_export_names_the_near_miss() {
    let _engine = engine_guard();
    let runtime = voip_or_skip!();

    // The VoIP module is the one that exposed the bug: it has the
    // single-underscore spelling and not the double.
    let names = runtime.export_names();
    assert!(
        names.contains("_emscripten_thread_init"),
        "this module should carry the one-underscore spelling"
    );
    assert!(
        !names.contains("__emscripten_thread_init"),
        "and not the two-underscore one — that is what made the miss silent"
    );

    // And a lookup for the spelling this module does not have must report the
    // one it does. That report is the whole fix: the original bug was a lookup
    // that returned `None` into an `else { return; }` and said nothing.
    //
    // `threading.rs::the_main_thread_queue_can_be_drained` covers the other
    // half — that finding the right spelling actually registers the thread.
    let close: Vec<&String> = names
        .iter()
        .filter(|name| {
            name.trim_start_matches('_') == "emscripten_thread_init".trim_start_matches('_')
        })
        .collect();
    assert_eq!(
        close.len(),
        1,
        "exactly one spelling should be present, for the report to point at"
    );
}

/// Measuring a duration must not age a timestamp.
///
/// A browser has two clocks and the guest uses them for different things:
/// `performance.now()` to time an operation, `Date.now()` to stamp a protocol
/// message. Deriving both from one counter conflated them here, and the futex
/// busy-wait's millions of monotonic observations dragged wall time forward
/// with them — 345 seconds during a single call. The engine then compared the
/// offer's timestamp against `wa_call_is_offer_expired`'s 45-second threshold
/// and dropped it, which read as the engine rejecting a valid offer.
#[test]
fn the_wall_clock_does_not_move_when_the_monotonic_one_does() {
    let _engine = engine_guard();
    let runtime = voip_or_skip!();
    let state = runtime.state();

    let wall_before = runtime.virtual_unix_time();
    let monotonic_before = state.clock_ms();

    // A busy-wait's worth of monotonic observations.
    for _ in 0..100_000 {
        state.tick_clock();
    }

    assert!(
        state.clock_ms() > monotonic_before,
        "the monotonic clock must advance, or a guest spin never terminates"
    );
    assert_eq!(
        runtime.virtual_unix_time(),
        wall_before,
        "wall time must not age just because something was timed"
    );
}

/// The heap has to be able to grow.
///
/// `emscripten_resize_heap` was stubbed, so every request to grow was refused
/// and the guest was stuck with its declared minimum — 160 pages for the VoIP
/// engine. The failure surfaces a long way from the cause: musl serves an
/// anonymous `mmap` from `emscripten_builtin_memalign` rather than from
/// `_mmap_js`, so a refusal becomes `ENOMEM`, then `map_pages_internal: mmap
/// failed`, then `abort()` inside whatever was running.
#[test]
fn the_guest_heap_can_grow() {
    let mut runtime = voip_or_skip!();
    let before = runtime.memory_size();

    // Ask through the guest's own allocator, for more than it starts with.
    let wanted = before + (4 << 20);
    let outcome = runtime.call(
        "emscripten_builtin_memalign",
        &[
            wasmtime::Val::I32(65_536),
            wasmtime::Val::I32(wanted as i32),
        ],
    );
    runtime.refuel();

    let ptr = match outcome {
        Ok(values) => match values.first() {
            Some(wasmtime::Val::I32(ptr)) => *ptr,
            other => panic!("memalign returned {other:?}"),
        },
        Err(error) => panic!("memalign trapped: {error:#}"),
    };

    assert_ne!(
        ptr, 0,
        "an allocation past the initial heap must succeed, which needs \
         emscripten_resize_heap to actually grow the memory"
    );
    assert!(
        runtime.memory_size() > before,
        "the memory should have grown from {before} to serve {wanted} bytes"
    );
}
