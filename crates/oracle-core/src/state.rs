//! Per-thread host state, and the guest memory behind it.
//!
//! One of these exists per `Store`, and there is one `Store` per guest thread.
//! Anything that must be coherent across threads lives in `shared.rs` instead;
//! that split is a correctness question, documented there.

use std::sync::Arc;

use anyhow::{Result, anyhow};
use wasmtime::{Caller, Extern, SharedMemory};

use crate::embind::EmbindRegistry;
use crate::shared::SharedHost;

/// One call into a stubbed host function, with the raw arguments the module
/// passed. The arguments are the interesting part: pointers into linear memory
/// can be dereferenced afterwards to recover strings and structs.
#[derive(Debug, Clone)]
pub struct HostCall {
    /// The import module, e.g. `env`.
    pub module: String,
    /// The import name.
    pub name: String,
    /// Its arguments, as scalars.
    pub args: Vec<i64>,
}

impl HostCall {
    /// `module::name`, which is how the trace is keyed.
    #[must_use]
    pub fn symbol(&self) -> String {
        format!("{}::{}", self.module, self.name)
    }
}

/// Per-thread host state.
///
/// One of these exists per `Store`, and there is one `Store` per guest thread.
/// Anything that must be coherent across threads lives in `shared` instead —
/// see `shared.rs` for why each item sits where it does.
pub struct HostState {
    /// Cross-thread state: trace, clock, PRNG, thread bookkeeping.
    pub shared: Arc<SharedHost>,
    /// 0 for the thread that instantiated the module.
    pub thread_id: u64,
    /// Set on threads that can start further threads.
    pub spawner: Option<Arc<crate::threads::Spawner>>,
    /// The imported *shared* memory, when the module has one. Shared memory can
    /// be read without a store context, so it is held directly.
    pub memory: Option<SharedMemory>,
    /// Base address and length of an ordinary exported memory.
    ///
    /// Non-shared memory cannot be reached without a store context, which host
    /// functions cannot hand to `HostState`. The window is therefore refreshed
    /// on entry to every host call — see `sync_memory`. It is never used across
    /// a call boundary, so `memory.grow` cannot leave it dangling.
    pub linear: Option<(usize, usize)>,
    /// Registered API. Only the instantiating thread runs the constructors, so
    /// this is not shared.
    pub embind: EmbindRegistry,
    /// The C++ exception currently unwinding. Per-thread by definition: two
    /// threads can be unwinding different exceptions at once.
    pub in_flight: crate::cxa::InFlight,
    /// Guest-visible arguments, environment and filesystem. See `wasi.rs`.
    pub wasi: crate::wasi::WasiState,
    /// Whether the instantiating thread registers itself as emscripten's *main
    /// runtime thread*.
    ///
    /// Off by default, and the reason is a real divergence rather than an
    /// oversight. Registering is what the browser does, and it is required for
    /// `emscripten_main_thread_process_queued_calls`, which asserts
    /// `emscripten_is_main_runtime_thread()` and traps otherwise. Turning it on
    /// is therefore the only way to drain work a worker queued for the main
    /// thread — which is how the VoIP engine sends its outbound signaling.
    ///
    /// But a browser's main thread never blocks: it returns to its event loop
    /// between calls, and the queue drains there. Here it blocks inside
    /// `call_embind` for the whole duration of a call, so guest code that waits
    /// on the main thread waits on a thread that cannot answer until it
    /// returns. With registration on, `handleIncomingSignalingOffer` stops
    /// completing — it spins instead of trapping, because the guest is waiting
    /// rather than failing.
    ///
    /// So: on when the caller wants the queue and controls the blocking, off
    /// when it wants calls to finish. Closing the gap properly means running
    /// the main thread's calls off the blocking path, which this does not yet
    /// do.
    pub register_main_thread: bool,
    /// How `pthread_create` is answered. See `ThreadPolicy`.
    pub threads: ThreadPolicy,
    /// Export name of the guest's memory.
    ///
    /// Not always `memory`: a minified module exports it under whatever letter
    /// the optimiser chose, and looking only for the conventional name leaves
    /// the host unable to read that module's memory at all — reads fail and the
    /// module looks broken rather than the host.
    pub memory_export: Option<String>,
    /// This thread's PRNG state. Per-thread on purpose — see
    /// `SharedHost::seed_for`.
    rng: std::cell::Cell<u64>,
    /// Whether the watched span was intact the last time this thread entered
    /// guest code — and `None` when that entry happened while threads were
    /// still running concurrently, which makes the reading worthless.
    ///
    /// See `host::install_memory_watch`. Invalidating rather than recording a
    /// stale `true` is the difference between naming one writer and naming
    /// three: a thread that entered wasm before the damage and returned after
    /// it reports "intact when I started, broken now" perfectly truthfully,
    /// while five other threads were running the whole time.
    pub watch_intact_entering_wasm: std::cell::Cell<Option<bool>>,
    /// Values handed to the guest as `emscripten::val` handles. See `emval.rs`.
    pub emval: crate::emval::EmvalTable,
}

/// What to tell a module that asks for a thread.
///
/// This host runs one thread, so neither answer is the truth. Which lie is more
/// useful depends on the module: the VoIP engine's media stack (PJSIP) treats a
/// refusal as a hard init failure, but a module that waits on the thread it
/// believes it started will hang instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ThreadPolicy {
    /// Return `EAGAIN`. Honest, and lets a caller take its error path.
    #[default]
    Refuse,
    /// Report success without running anything. Lets initialisation past a
    /// thread it does not immediately depend on, at the risk of a later wait
    /// that never completes.
    PretendSuccess,
    /// Actually start the thread: a fresh instance of the same module on its own
    /// `Store`, sharing this one's memory. What the module expects, at the cost
    /// of a reproducible interleaving — see `Runtime::quiesce` for how much of
    /// that is recoverable.
    Spawn,
}

impl Default for HostState {
    fn default() -> Self {
        Self::for_thread(Arc::new(SharedHost::default()), 0, ThreadPolicy::default())
    }
}

impl HostState {
    /// State for one guest thread, sharing `shared` with every other thread of
    /// the same module.
    pub fn for_thread(shared: Arc<SharedHost>, thread_id: u64, threads: ThreadPolicy) -> Self {
        Self {
            shared,
            thread_id,
            spawner: None,
            memory: None,
            linear: None,
            embind: EmbindRegistry::default(),
            in_flight: crate::cxa::InFlight::default(),
            wasi: crate::wasi::WasiState::default(),
            // Off even for thread 0, and that is a known, measured trade-off
            // rather than an oversight.
            //
            // The guest's `emscripten_main_thread_process_queued_calls` opens
            // by asserting `emscripten_is_main_runtime_thread()` — a
            // per-instance global set by
            // `_emscripten_thread_init(.., is_main=1, ..)` — and traps on
            // `unreachable` when it does not hold. With this off, *every* drain
            // of the main-thread proxy queue therefore fails, and outgoing VoIP
            // signaling is dispatched through exactly that queue. That is a
            // real bug.
            //
            // It is *not* the reason `sendSignalingXMPP_js_sync` looked
            // unreachable — that reading came from `all_calls_to`, which stops
            // recording at 8192 calls while startup makes tens of millions.
            // Measured through the counters, the engine reaches the sender with
            // or without main-thread registration. See
            // `examples/outbound_setup_matrix.rs`.
            //
            // But `thread_id == 0` here was measured and is worse: the drains
            // stop failing and `initVoipStack` starts trapping instead (round 3
            // of `startup_is_reliable_and_never_forces_a_turn`). In a browser
            // the main thread returns to an event loop between calls; here it
            // sits inside `call_embind` for the whole call, so a guest waiting
            // on the main thread waits on something that cannot answer.
            //
            // The fix is neither flag value: the queue has to be drained off
            // the blocking path. Until then this stays off, because a harness
            // that starts reliably is worth more than one that drains.
            register_main_thread: false,
            threads,
            memory_export: None,
            rng: std::cell::Cell::new(SharedHost::seed_for(thread_id)),
            watch_intact_entering_wasm: std::cell::Cell::new(None),
            emval: crate::emval::EmvalTable::default(),
        }
    }

    /// Appends a line to the shared transcript, tagged with this thread.
    pub fn log(&self, text: impl Into<String>) {
        self.shared.log(self.thread_id, text.into());
    }

    /// Advances and returns the shared virtual clock.
    pub fn tick_clock(&self) -> f64 {
        self.shared.tick_clock()
    }

    /// The monotonic clock, in milliseconds.
    #[must_use]
    pub fn clock_ms(&self) -> f64 {
        self.shared.clock()
    }

    /// Next byte of this thread's deterministic PRNG (SplitMix64).
    pub fn next_random_byte(&self) -> u8 {
        let mut state = self.rng.get().wrapping_add(0x9E37_79B9_7F4A_7C15);
        self.rng.set(state);
        state = (state ^ (state >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        state = (state ^ (state >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        ((state ^ (state >> 31)) & 0xff) as u8
    }

    /// Records that the guest asked to exit.
    pub fn set_exit_code(&self, code: i32) {
        self.shared.set_exit_code(code);
    }

    /// The exit code, if the guest asked to exit.
    #[must_use]
    pub fn exit_code(&self) -> Option<i32> {
        self.shared.exit_code()
    }

    /// Records one host call into the shared trace.
    pub fn record(&self, module: &str, name: &str, args: Vec<i64>) {
        self.shared.record(module, name, args);
    }
}

impl HostState {
    /// Reads `len` bytes of linear memory.
    ///
    /// What keeps this exclusive is `schedule.rs`, not the absence of threads:
    /// `ThreadPolicy::Spawn` runs real guest threads over one shared memory, and
    /// the scheduler is what holds all but one of them outside guest code.
    #[allow(unsafe_code)]
    pub fn read(&self, ptr: u32, len: u32) -> Result<Vec<u8>> {
        let start = ptr as usize;
        let end = start
            .checked_add(len as usize)
            .ok_or_else(|| anyhow!("read at {ptr}+{len} overflows"))?;

        if let Some(memory) = self.memory.as_ref() {
            let data = memory.data();
            if end > data.len() {
                return Err(anyhow!(
                    "read at {ptr}+{len} is out of bounds ({} bytes mapped)",
                    data.len()
                ));
            }
            // SAFETY: wasmtime exposes shared memory as UnsafeCell because
            // another thread could write to it. Bounds are checked above, and
            // the accesses are per-byte through the cell, so no reference to
            // the memory is formed and no typed invariant can be broken.
            //
            // Exclusion comes from the scheduler: a guest thread only runs
            // while it holds the turn, and it holds the turn across the host
            // call this read happens inside. That is bounded rather than
            // absolute — `schedule.rs` lets a thread take its turn after
            // TURN_TIMEOUT rather than deadlock, and `threads.rs` deliberately
            // watches a word another thread is writing. Past that point a read
            // can tear and return a mix of old and new bytes. `forced_turns()`
            // counts it, and the startup guard keeps it at zero.
            return Ok(unsafe { data[start..end].iter().map(|cell| *cell.get()).collect() });
        }

        let (base, size) = self
            .linear
            .ok_or_else(|| anyhow!("module memory is not available to the host"))?;
        if end > size {
            return Err(anyhow!(
                "read at {ptr}+{len} is out of bounds ({size} bytes mapped)"
            ));
        }
        // SAFETY: `base` was taken from the instance's memory on entry to this
        // host call and the guest is suspended for its duration, so the mapping
        // cannot move or shrink underneath it. Bounds are checked above.
        Ok(unsafe {
            std::slice::from_raw_parts((base + start) as *const u8, len as usize).to_vec()
        })
    }

    /// Whether the watched span still holds what it did.
    ///
    /// `None` means there is nothing to answer about — no watch is set, or this
    /// module's memory is not readable this way. Deliberately allocation-free:
    /// this runs on *both* directions of every host-code boundary, and a guest
    /// worker crosses millions of them, so a `Vec` per crossing would be the
    /// most expensive thing in the run.
    ///
    /// Keeps answering after the span has broken, rather than latching. The
    /// attribution in `install_memory_watch` needs the answer on the way into
    /// guest code as well as on the way out, and a latched check can only ever
    /// say that something happened, never which thread did it.
    #[allow(unsafe_code)]
    pub fn watch_intact(&self) -> Option<bool> {
        let watch = self.shared.watch.get()?;
        let memory = self.memory.as_ref()?;
        let start = watch.at as usize;
        let span = memory
            .data()
            .get(start..start.checked_add(watch.expected.len())?)?;
        // SAFETY: the same argument as `read` — per-byte through the cell, so
        // no reference into shared memory is formed, and the range came from
        // `get` rather than from arithmetic.
        Some(
            span.iter()
                .zip(&watch.expected)
                .all(|(cell, byte)| unsafe { *cell.get() } == *byte),
        )
    }

    /// Reads a NUL-terminated C string.
    pub fn read_cstr(&self, ptr: u32) -> Result<String> {
        const MAX: u32 = 4096;

        if ptr == 0 {
            return Ok(String::new());
        }
        let bytes = self.read(ptr, MAX).or_else(|_| {
            // Near the end of memory a full-length read fails; retry smaller.
            self.read(ptr, 256)
        })?;
        let end = bytes.iter().position(|&byte| byte == 0).unwrap_or(0);
        Ok(String::from_utf8_lossy(&bytes[..end]).into_owned())
    }

    /// Writes `bytes` into linear memory at `ptr`.
    #[allow(unsafe_code)]
    pub fn write(&self, ptr: u32, bytes: &[u8]) -> Result<()> {
        let start = ptr as usize;
        let end = start
            .checked_add(bytes.len())
            .ok_or_else(|| anyhow!("write at {ptr}+{} overflows", bytes.len()))?;

        if let Some(memory) = self.memory.as_ref() {
            let data = memory.data();
            if end > data.len() {
                return Err(anyhow!("write at {ptr}+{} is out of bounds", bytes.len()));
            }
            // SAFETY: same argument as `read` — per-byte through the cell,
            // range checked above, exclusion from the scheduler — and the guest
            // gave us this pointer to write through.
            unsafe {
                for (cell, byte) in data[start..end].iter().zip(bytes) {
                    *cell.get() = *byte;
                }
            }
            return Ok(());
        }

        let (base, size) = self
            .linear
            .ok_or_else(|| anyhow!("module memory is not available to the host"))?;
        if end > size {
            return Err(anyhow!("write at {ptr}+{} is out of bounds", bytes.len()));
        }
        // SAFETY: as in `read` — the window was refreshed on entry to this host
        // call, the guest is suspended, and the range is checked above.
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), (base + start) as *mut u8, bytes.len());
        }
        Ok(())
    }

    /// Reads a little-endian `u32`.
    pub fn read_u32(&self, ptr: u32) -> Result<u32> {
        let bytes = self.read(ptr, 4)?;
        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    /// Every recorded call to one `module::name`.
    #[must_use]
    pub fn calls_to(&self, symbol: &str) -> Vec<HostCall> {
        self.shared
            .calls()
            .into_iter()
            .filter(|call| call.symbol() == symbol)
            .collect()
    }

    /// Host symbols by call count, most-called first.
    pub fn hot_calls(&self) -> Vec<(String, u64)> {
        self.shared.hot_calls()
    }

    /// How many host calls were made in total.
    #[must_use]
    pub fn total_calls(&self) -> u64 {
        self.shared.total_calls()
    }

    /// The recorded host calls, with their arguments.
    #[must_use]
    pub fn calls(&self) -> Vec<HostCall> {
        self.shared.calls()
    }

    /// The host's own log lines.
    #[must_use]
    pub fn logs(&self) -> Vec<String> {
        self.shared.log_texts()
    }
}

/// Refreshes the host's view of an ordinary exported memory.
///
/// Must run at the start of every host function that may touch guest memory.
/// A module whose memory is exported rather than imported is invisible until
/// this runs, and a stale window would survive a `memory.grow`. Modules with a
/// shared memory need nothing here, since that handle stays valid on its own.
pub fn sync_memory(caller: &mut Caller<'_, HostState>) {
    if caller.data().memory.is_some() {
        return;
    }
    let name = caller
        .data()
        .memory_export
        .clone()
        .unwrap_or_else(|| "memory".to_owned());
    let Some(Extern::Memory(memory)) = caller.get_export(&name) else {
        return;
    };
    let window = (memory.data_ptr(&caller) as usize, memory.data_size(&caller));
    caller.data_mut().linear = Some(window);
}
