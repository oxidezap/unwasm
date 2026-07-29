//! The runtime the generated code stands on.
//!
//! This file is compiled twice: once as part of this crate, where its tests run,
//! and once as text — [`crate::codegen`] embeds it verbatim into every module it
//! emits, so generated code has no dependency at all. Anything wasm defines and
//! Rust spells differently lives here rather than in the emitter, which keeps
//! the semantics in one place and under test.
//!
//! The differences are not cosmetic. `i32.div_s` traps where Rust panics only in
//! debug, `f32.min` propagates NaN where Rust's `f32::min` swallows it, and
//! `i32.trunc_f32_s` traps on NaN where `as` saturates. Each one is a wrong
//! answer waiting to happen, and each one is a test below.

/// A wasm trap. The generated code panics, which is the observable behaviour a
/// trap has for a caller that is not catching it: execution stops, and nothing
/// downstream sees an invented value.
///
/// The message matches the text wasm engines use, so a differential run against
/// a real engine can compare failures and not just successes.
#[cold]
#[inline(never)]
pub fn trap(message: &str) -> ! {
    panic!("wasm trap: {message}");
}

/// Bytes in a wasm page.
pub const PAGE_SIZE: usize = 65536;

/// The trap an access outside the memory raises, with the numbers that say
/// which kind of mistake it was.
///
/// The message an engine gives is just "out of bounds memory access", and it
/// is the same whether the address was one byte past the end or a pointer read
/// out of uninitialised memory. Those are different bugs, and the address tells
/// them apart at a glance — a decompilation is something you are running to
/// find out what it does, and this is the moment it has most to say.
///
/// # Panics
///
/// Always. It is a trap.
#[cold]
#[inline(never)]
pub fn out_of_bounds(at: u64, size: u64, memory: usize) -> ! {
    panic!(
        "wasm trap: out of bounds memory access: {size} bytes at {at:#x}, \
         and the memory is {memory} bytes ({:.1} MiB)",
        memory as f64 / (1024.0 * 1024.0)
    );
}

/// One write to a watched address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hit {
    /// The function that did it, by index.
    pub site: u32,
    /// Where the instruction that did it sits in the wasm file.
    ///
    /// The function alone leaves the next question open — a function with
    /// fifty stores in it wrote the address from one of them — and this is the
    /// one. `unwasm bytes <module> <at> <len>` prints it.
    pub at: u32,
    /// The address written, after the static offset was added.
    pub address: i32,
    /// How many bytes it wrote.
    pub bytes: u32,
    /// Which write it was: `memory.fill` and a plain store look alike in a
    /// hit list otherwise, and a `memset` is the usual answer to "who zeroed
    /// this".
    pub kind: WriteKind,
}

/// What kind of write a [`Hit`] records.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteKind {
    /// An `i32.store`, `f64.store`, and so on.
    Store,
    /// `memory.fill` — what a `memset` compiles to.
    Fill,
    /// `memory.copy` — a `memcpy` or `memmove`.
    Copy,
    /// `memory.init`, placing a data segment.
    Init,
    /// An atomic store or read-modify-write.
    Atomic,
}

/// Addresses to report writes to.
///
/// A decompiled module *runs*, which makes "who writes to 0x24bed0?" a question
/// the machine can answer instead of one answered by patching bytecode and
/// re-running. Set a range, run, and every write to it is recorded with the
/// function that did it; set `stop` and the first one panics, so the backtrace
/// is the guest's own call chain.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Watchpoints {
    /// Half-open `[from, to)` ranges, compared as unsigned addresses.
    pub ranges: Vec<(u32, u32)>,
    /// Every write that landed in one, in the order they happened.
    pub hits: Vec<Hit>,
    /// Panic on the first hit rather than recording and carrying on.
    pub stop: bool,
}

/// A wasm linear memory: a byte vector, bounds-checked on every access.
///
/// No `unsafe`, no raw pointers. A decompiler that needs them to model a
/// sandbox has lost the property that made the sandbox worth reading.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Memory {
    /// The raw bytes. Public because generated code reads and writes it, and
    /// because a differential harness compares it against an engine's.
    pub data: Vec<u8>,
    /// Page limit from the module's memory type, if it declared one.
    pub max_pages: Option<u32>,
    /// Where writes are being watched. Empty unless a host asks, and every
    /// `*_at` method is a no-op then, which is why instrumented output can be
    /// run without paying for it.
    pub watch: Watchpoints,
}

impl Memory {
    /// A memory of `min_pages`, zero-filled, as instantiation leaves it.
    #[must_use]
    pub fn new(min_pages: u32, max_pages: Option<u32>) -> Self {
        Self {
            data: vec![0; min_pages as usize * PAGE_SIZE],
            max_pages,
            watch: Watchpoints::default(),
        }
    }

    /// Watches `bytes` bytes from `address`.
    ///
    /// Only output generated with store instrumentation reports anything: the
    /// plain output calls `store32` rather than `store32_at`, and there is
    /// nowhere for a hit to come from.
    pub fn watch(&mut self, address: i32, bytes: i32) {
        let from = address as u32;
        self.watch
            .ranges
            .push((from, from.wrapping_add(bytes as u32)));
    }

    /// The writes to watched addresses so far.
    #[must_use]
    pub fn hits(&self) -> &[Hit] {
        &self.watch.hits
    }

    /// Records a write that has already happened, if anyone is watching it.
    ///
    /// After rather than before, so a store that trapped is not reported as a
    /// write that never landed.
    ///
    /// # Panics
    ///
    /// If `watch.stop` is set and the write is watched. That is the point: the
    /// panic carries the site and the address, and the backtrace carries the
    /// call chain that reached it.
    fn note(&mut self, site: u32, at: u32, addr: i32, offset: u64, bytes: u64, kind: WriteKind) {
        if self.watch.ranges.is_empty() {
            return;
        }
        let start = u64::from(addr as u32) + offset;
        let end = start + bytes;
        let watched = self
            .watch
            .ranges
            .iter()
            .any(|(from, to)| start < u64::from(*to) && end > u64::from(*from));
        if !watched {
            return;
        }
        let hit = Hit {
            site,
            at,
            address: start as u32 as i32,
            bytes: bytes as u32,
            kind,
        };
        self.watch.hits.push(hit);
        if self.watch.stop {
            panic!(
                "watchpoint: function #{site} wrote {bytes} bytes at {start:#x} ({kind:?}), \
                 from the instruction at file offset {at}"
            );
        }
    }

    /// `i32.store8` / `i64.store8`, reporting to any watchpoint.
    pub fn store8_at(&mut self, site: u32, at: u32, addr: i32, offset: u64, value: i64) {
        self.store8(addr, offset, value);
        self.note(site, at, addr, offset, 1, WriteKind::Store);
    }
    /// `i32.store16` / `i64.store16`, reporting to any watchpoint.
    pub fn store16_at(&mut self, site: u32, at: u32, addr: i32, offset: u64, value: i64) {
        self.store16(addr, offset, value);
        self.note(site, at, addr, offset, 2, WriteKind::Store);
    }
    /// `i32.store` / `i64.store32`, reporting to any watchpoint.
    pub fn store32_at(&mut self, site: u32, at: u32, addr: i32, offset: u64, value: i64) {
        self.store32(addr, offset, value);
        self.note(site, at, addr, offset, 4, WriteKind::Store);
    }
    /// `i64.store`, reporting to any watchpoint.
    pub fn store64_at(&mut self, site: u32, at: u32, addr: i32, offset: u64, value: i64) {
        self.store64(addr, offset, value);
        self.note(site, at, addr, offset, 8, WriteKind::Store);
    }
    /// `f32.store`, reporting to any watchpoint.
    pub fn store_f32_at(&mut self, site: u32, at: u32, addr: i32, offset: u64, value: f32) {
        self.store_f32(addr, offset, value);
        self.note(site, at, addr, offset, 4, WriteKind::Store);
    }
    /// `f64.store`, reporting to any watchpoint.
    pub fn store_f64_at(&mut self, site: u32, at: u32, addr: i32, offset: u64, value: f64) {
        self.store_f64(addr, offset, value);
        self.note(site, at, addr, offset, 8, WriteKind::Store);
    }
    /// `memory.fill`, reporting to any watchpoint. This is the one that
    /// answers "who zeroed it": a `memset` is a fill, not a store.
    pub fn fill_at(&mut self, site: u32, at: u32, addr: i32, value: i32, len: i32) {
        self.fill(addr, value, len);
        self.note(site, at, addr, 0, u64::from(len as u32), WriteKind::Fill);
    }
    /// `memory.copy`, reporting to any watchpoint on the destination.
    pub fn copy_at(&mut self, site: u32, at: u32, dst: i32, src: i32, len: i32) {
        self.copy(dst, src, len);
        self.note(site, at, dst, 0, u64::from(len as u32), WriteKind::Copy);
    }
    /// `memory.init`, reporting to any watchpoint on the destination.
    pub fn init_at(&mut self, site: u32, at: u32, dst: i32, segment: &[u8], src: i32, len: i32) {
        self.init(dst, segment, src, len);
        self.note(site, at, dst, 0, u64::from(len as u32), WriteKind::Init);
    }
    /// An atomic store, reporting to any watchpoint.
    pub fn atomic_store_at(
        &mut self,
        site: u32,
        at: u32,
        addr: i32,
        offset: u64,
        bytes: u32,
        value: i64,
    ) {
        self.atomic_store(addr, offset, bytes, value);
        self.note(site, at, addr, offset, u64::from(bytes), WriteKind::Atomic);
    }
    /// An atomic read-modify-write, reporting to any watchpoint.
    // Eight arguments, and every one of them is the instruction's: the site
    // that identifies it, and the six operands `i32.atomic.rmw.add` has.
    // Bundling them into a struct would put a literal at every call site in a
    // two-million-line file to satisfy a count.
    #[allow(clippy::too_many_arguments)]
    #[allow(clippy::too_many_arguments)]
    pub fn atomic_rmw_at(
        &mut self,
        site: u32,
        at: u32,
        addr: i32,
        offset: u64,
        bytes: u32,
        op: Rmw,
        value: i64,
    ) -> i64 {
        let old = self.atomic_rmw(addr, offset, bytes, op, value);
        self.note(site, at, addr, offset, u64::from(bytes), WriteKind::Atomic);
        old
    }
    /// An atomic compare-and-exchange, reporting to any watchpoint.
    ///
    /// Only when the exchange happened. A comparison that failed wrote
    /// nothing, and reporting it would answer "who wrote this address" with a
    /// function that did not — and with `stop` set, would stop the run before
    /// the write being hunted ever happens.
    #[allow(clippy::too_many_arguments)]
    pub fn atomic_cmpxchg_at(
        &mut self,
        site: u32,
        at: u32,
        addr: i32,
        offset: u64,
        bytes: u32,
        expected: i64,
        replacement: i64,
    ) -> i64 {
        let old = self.atomic_cmpxchg(addr, offset, bytes, expected, replacement);
        if exchanged(old, expected, bytes) {
            self.note(site, at, addr, offset, u64::from(bytes), WriteKind::Atomic);
        }
        old
    }

    /// The bytes, copied out.
    ///
    /// The same method a [`SharedMemory`] has, so a driver that dumps a memory
    /// does not have to know which kind it got.
    #[must_use]
    pub fn snapshot(&self) -> Vec<u8> {
        self.data.clone()
    }

    /// `memory.size`: the current size in pages.
    #[must_use]
    pub fn size(&self) -> i32 {
        (self.data.len() / PAGE_SIZE) as i32
    }

    /// `memory.grow`: returns the previous size in pages, or `-1` if refused.
    pub fn grow(&mut self, delta_pages: u32) -> i32 {
        let old_pages = self.data.len() / PAGE_SIZE;
        let new_pages = old_pages as u64 + u64::from(delta_pages);
        // 65536 pages is the 4 GiB ceiling of a 32-bit memory.
        if new_pages > u64::from(self.max_pages.unwrap_or(65536)) || new_pages > 65536 {
            return -1;
        }
        self.data.resize(new_pages as usize * PAGE_SIZE, 0);
        old_pages as i32
    }

    /// Resolves `addr + offset` and checks `size` bytes fit, in `u64` so the
    /// sum itself cannot wrap into a valid-looking address.
    fn range(&self, addr: i32, offset: u64, size: u64) -> usize {
        let at = u64::from(addr as u32) + offset;
        if at + size > self.data.len() as u64 {
            out_of_bounds(at, size, self.data.len());
        }
        at as usize
    }

    /// Reads `N` bytes. The width is the type's, not the address's.
    fn read<const N: usize>(&self, addr: i32, offset: u64) -> [u8; N] {
        let at = self.range(addr, offset, N as u64);
        let mut bytes = [0u8; N];
        bytes.copy_from_slice(&self.data[at..at + N]);
        bytes
    }

    fn write<const N: usize>(&mut self, addr: i32, offset: u64, bytes: [u8; N]) {
        let at = self.range(addr, offset, N as u64);
        self.data[at..at + N].copy_from_slice(&bytes);
    }

    /// `i32.load8_u` / the unsigned byte read every wider load is built from.
    pub fn load8_u(&self, addr: i32, offset: u64) -> i32 {
        i32::from(self.read::<1>(addr, offset)[0])
    }
    /// `i32.load8_s`
    pub fn load8_s(&self, addr: i32, offset: u64) -> i32 {
        i32::from(self.read::<1>(addr, offset)[0] as i8)
    }
    /// `i32.load16_u`
    pub fn load16_u(&self, addr: i32, offset: u64) -> i32 {
        i32::from(u16::from_le_bytes(self.read(addr, offset)))
    }
    /// `i32.load16_s`
    pub fn load16_s(&self, addr: i32, offset: u64) -> i32 {
        i32::from(i16::from_le_bytes(self.read(addr, offset)))
    }
    /// `i32.load`
    pub fn load32(&self, addr: i32, offset: u64) -> i32 {
        i32::from_le_bytes(self.read(addr, offset))
    }
    /// `i64.load`
    pub fn load64(&self, addr: i32, offset: u64) -> i64 {
        i64::from_le_bytes(self.read(addr, offset))
    }
    /// `f32.load`
    pub fn load_f32(&self, addr: i32, offset: u64) -> f32 {
        f32::from_le_bytes(self.read(addr, offset))
    }
    /// `f64.load`
    pub fn load_f64(&self, addr: i32, offset: u64) -> f64 {
        f64::from_le_bytes(self.read(addr, offset))
    }

    /// `i64.load8_s`. The narrow i64 loads read the same bytes as their i32
    /// counterparts and sign- or zero-extend to 64 bits.
    pub fn load8_s_i64(&self, addr: i32, offset: u64) -> i64 {
        i64::from(self.read::<1>(addr, offset)[0] as i8)
    }
    /// `i64.load8_u`
    pub fn load8_u_i64(&self, addr: i32, offset: u64) -> i64 {
        i64::from(self.read::<1>(addr, offset)[0])
    }
    /// `i64.load16_s`
    pub fn load16_s_i64(&self, addr: i32, offset: u64) -> i64 {
        i64::from(i16::from_le_bytes(self.read(addr, offset)))
    }
    /// `i64.load16_u`
    pub fn load16_u_i64(&self, addr: i32, offset: u64) -> i64 {
        i64::from(u16::from_le_bytes(self.read(addr, offset)))
    }
    /// `i64.load32_s`
    pub fn load32_s_i64(&self, addr: i32, offset: u64) -> i64 {
        i64::from(i32::from_le_bytes(self.read(addr, offset)))
    }
    /// `i64.load32_u`
    pub fn load32_u_i64(&self, addr: i32, offset: u64) -> i64 {
        i64::from(u32::from_le_bytes(self.read(addr, offset)))
    }

    /// `i32.store8` / `i64.store8`
    pub fn store8(&mut self, addr: i32, offset: u64, value: i64) {
        self.write(addr, offset, [value as u8]);
    }
    /// `i32.store16` / `i64.store16`
    pub fn store16(&mut self, addr: i32, offset: u64, value: i64) {
        self.write(addr, offset, (value as u16).to_le_bytes());
    }
    /// `i32.store` / `i64.store32`
    pub fn store32(&mut self, addr: i32, offset: u64, value: i64) {
        self.write(addr, offset, (value as u32).to_le_bytes());
    }
    /// `i64.store`
    pub fn store64(&mut self, addr: i32, offset: u64, value: i64) {
        self.write(addr, offset, value.to_le_bytes());
    }
    /// `f32.store`
    pub fn store_f32(&mut self, addr: i32, offset: u64, value: f32) {
        self.write(addr, offset, value.to_le_bytes());
    }
    /// `f64.store`
    pub fn store_f64(&mut self, addr: i32, offset: u64, value: f64) {
        self.write(addr, offset, value.to_le_bytes());
    }

    /// `memory.fill`. A zero-length fill at the end of memory is legal; one
    /// past it is not, which is why the check runs even when `len` is zero.
    pub fn fill(&mut self, addr: i32, value: i32, len: i32) {
        let at = self.range(addr, 0, u64::from(len as u32));
        let end = at + len as u32 as usize;
        self.data[at..end].fill(value as u8);
    }

    /// `memory.copy`. Overlapping ranges move as `memmove` does.
    pub fn copy(&mut self, dst: i32, src: i32, len: i32) {
        let len = len as u32 as usize;
        let to = self.range(dst, 0, len as u64);
        let from = self.range(src, 0, len as u64);
        self.data.copy_within(from..from + len, to);
    }

    /// `memory.init`, and how a passive data segment reaches memory.
    pub fn init(&mut self, dst: i32, segment: &[u8], src: i32, len: i32) {
        let len = len as u32 as usize;
        let src = src as u32 as usize;
        if src + len > segment.len() {
            trap("out of bounds memory access");
        }
        let to = self.range(dst, 0, len as u64);
        self.data[to..to + len].copy_from_slice(&segment[src..src + len]);
    }

    // ---- atomics ----
    //
    // A decompilation runs one thread, and under one thread an atomic
    // load, store or read-modify-write does exactly what the plain one does:
    // there is nobody to interleave with. What is *not* the same is the
    // alignment rule — an atomic access must be naturally aligned or it traps,
    // where a plain one is happy anywhere — and the two blocking instructions,
    // which are the only place the thread count is actually observable.

    /// Bounds-checks and alignment-checks an atomic access.
    ///
    /// The alignment trap is the one thing an atomic does that its plain
    /// counterpart does not, so it is the one thing a decompilation could
    /// quietly get wrong.
    fn atomic_at(&self, addr: i32, offset: u64, bytes: u32) -> usize {
        let at = self.range(addr, offset, u64::from(bytes));
        if !at.is_multiple_of(bytes as usize) {
            trap("unaligned atomic operation");
        }
        at
    }

    /// Reads `bytes` bytes, zero-extended. Covers every `*.atomic.load*`.
    pub fn atomic_load(&self, addr: i32, offset: u64, bytes: u32) -> i64 {
        let at = self.atomic_at(addr, offset, bytes);
        let mut value = 0u64;
        for (step, byte) in self.data[at..at + bytes as usize].iter().enumerate() {
            value |= u64::from(*byte) << (8 * step);
        }
        value as i64
    }

    /// Writes the low `bytes` bytes. Covers every `*.atomic.store*`.
    pub fn atomic_store(&mut self, addr: i32, offset: u64, bytes: u32, value: i64) {
        let at = self.atomic_at(addr, offset, bytes);
        let value = value as u64;
        for step in 0..bytes as usize {
            self.data[at + step] = (value >> (8 * step)) as u8;
        }
    }

    /// The read-modify-write operations, which differ only in the arithmetic.
    pub fn atomic_rmw(&mut self, addr: i32, offset: u64, bytes: u32, op: Rmw, value: i64) -> i64 {
        let old = self.atomic_load(addr, offset, bytes);
        // The arithmetic happens at the access width, not at the value's: an
        // `i32.atomic.rmw8.add_u` wraps at 256.
        let (old_u, value_u) = (old as u64, value as u64);
        let result = match op {
            Rmw::Add => old_u.wrapping_add(value_u),
            Rmw::Sub => old_u.wrapping_sub(value_u),
            Rmw::And => old_u & value_u,
            Rmw::Or => old_u | value_u,
            Rmw::Xor => old_u ^ value_u,
            Rmw::Xchg => value_u,
        };
        self.atomic_store(addr, offset, bytes, result as i64);
        old
    }

    /// `*.atomic.rmw*.cmpxchg*`. Returns the old value either way, which is how
    /// the caller learns whether the exchange happened.
    pub fn atomic_cmpxchg(
        &mut self,
        addr: i32,
        offset: u64,
        bytes: u32,
        expected: i64,
        replacement: i64,
    ) -> i64 {
        let old = self.atomic_load(addr, offset, bytes);
        // `expected` arrives at the value's width; the comparison is at the
        // access width, so it is truncated the same way the load was.
        let mask = if bytes >= 8 {
            u64::MAX
        } else {
            (1u64 << (8 * bytes)) - 1
        };
        if old as u64 == expected as u64 & mask {
            self.atomic_store(addr, offset, bytes, replacement);
        }
        old
    }

    /// `memory.atomic.notify`. Nobody is waiting, because nobody else is
    /// running — so this wakes zero threads, which is the honest answer rather
    /// than a convenient one.
    pub fn atomic_notify(&self, addr: i32, offset: u64) -> i32 {
        self.atomic_at(addr, offset, 4);
        0
    }

    /// `memory.atomic.wait32` / `wait64`.
    ///
    /// Two of the three outcomes are exactly right with one thread:
    ///
    /// - the value has already changed → `1` (not-equal), which is the common
    ///   case in an uncontended lock and needs no thread at all;
    /// - a zero timeout → `2` (timed-out), because a zero timeout expires
    ///   immediately whoever else is running.
    ///
    /// The third would block until another thread notifies, and there is no
    /// other thread. Returning "timed-out" there would be inventing an event
    /// that did not happen, so it traps and says why.
    pub fn atomic_wait(
        &self,
        addr: i32,
        offset: u64,
        bytes: u32,
        expected: i64,
        timeout: i64,
    ) -> i32 {
        if self.atomic_load(addr, offset, bytes) != expected {
            return 1;
        }
        if timeout == 0 {
            return 2;
        }
        trap(
            "memory.atomic.wait would block forever: this decompilation runs a single thread, \
             and no other thread can notify it",
        );
    }
}

/// A memory several instances share, which is what a threaded module has.
///
/// Emscripten's pthreads are wasm instances over one `shared` memory: the
/// module is instantiated once per thread, each with its own globals — its own
/// `__stack_pointer` above all — and all of them addressing the same bytes.
/// This is that, in safe Rust: an `Arc<[AtomicU8]>` every instance holds a
/// handle to.
///
/// Three consequences worth knowing before using it:
///
/// - **Every byte is an `AtomicU8`, read and written `Relaxed`.** A wasm plain
///   load or store on a shared memory is exactly a relaxed atomic — the spec
///   has no data races, only unsynchronised accesses — so this is the faithful
///   model rather than a conservative one. It does cost: the optimiser cannot
///   vectorise it.
/// - **An atomic access takes a lock for its duration.** Byte-wise atomics
///   cannot make a four-byte read-modify-write atomic on their own. The lock is
///   stronger than the spec asks for and never weaker, and atomics are rare
///   next to plain accesses.
/// - **The size is reserved up front and `grow` only publishes more of it.**
///   A shared memory cannot be reallocated while other threads hold handles to
///   it, so the reservation is allocated at construction and growth moves a
///   counter. Past the reservation `grow` returns -1, which is an answer the
///   spec allows and which says exactly what happened.
#[derive(Debug, Clone)]
pub struct SharedMemory {
    /// The bytes, all of the reservation.
    cells: std::sync::Arc<Vec<std::sync::atomic::AtomicU8>>,
    /// How many pages are currently addressable.
    pages: std::sync::Arc<std::sync::atomic::AtomicU32>,
    /// The maximum the memory type declared.
    max_pages: Option<u32>,
    /// Held for the duration of an atomic access, so a four-byte one is not
    /// four separate ones.
    atomics: std::sync::Arc<std::sync::Mutex<()>>,
    /// Threads parked in `memory.atomic.wait`.
    waiters: std::sync::Arc<Waiters>,
    /// How many grows were refused by the reservation rather than by the
    /// module's own declared maximum.
    refused: std::sync::Arc<std::sync::atomic::AtomicU32>,
    /// Watchpoints, shared so a hit from any thread lands in one list.
    watch: std::sync::Arc<std::sync::Mutex<Watchpoints>>,
}

/// Threads parked on addresses, each holding a ticket.
///
/// A ticket per waiter rather than a generation per address, because
/// `memory.atomic.notify` takes a *count*: waking everybody and reporting how
/// many were parked is a different program. The tickets are woken in the order
/// they parked, which the spec does not require and which makes a test able to
/// say what happened.
#[derive(Debug, Default)]
struct Waiters {
    /// The tickets parked on each address, oldest first.
    parked: std::sync::Mutex<Parked>,
    /// Signalled by every `notify`.
    wake: std::sync::Condvar,
}

/// The parking lot's contents.
#[derive(Debug, Default)]
struct Parked {
    /// Address to the tickets waiting on it, oldest first.
    queues: std::collections::BTreeMap<usize, std::collections::VecDeque<u64>>,
    /// Tickets that have been notified and have not yet noticed.
    woken: std::collections::BTreeSet<u64>,
    /// The next ticket to hand out.
    next_ticket: u64,
}

impl SharedMemory {
    /// A shared memory of `min_pages`, able to grow to `reserved_pages`.
    ///
    /// # Panics
    ///
    /// If the reservation is smaller than the initial size, which is a host
    /// mistake rather than a guest one.
    #[must_use]
    pub fn new(min_pages: u32, max_pages: Option<u32>, reserved_pages: u32) -> Self {
        assert!(
            reserved_pages >= min_pages,
            "the reservation must cover the memory the module declares"
        );
        let mut cells = Vec::with_capacity(reserved_pages as usize * PAGE_SIZE);
        cells.resize_with(reserved_pages as usize * PAGE_SIZE, || {
            std::sync::atomic::AtomicU8::new(0)
        });
        Self {
            cells: std::sync::Arc::new(cells),
            pages: std::sync::Arc::new(std::sync::atomic::AtomicU32::new(min_pages)),
            max_pages,
            atomics: std::sync::Arc::default(),
            waiters: std::sync::Arc::default(),
            refused: std::sync::Arc::default(),
            watch: std::sync::Arc::default(),
        }
    }

    /// How many pages were reserved, which is the ceiling `grow` can reach.
    #[must_use]
    pub fn reserved_pages(&self) -> u32 {
        (self.cells.len() / PAGE_SIZE) as u32
    }

    /// How many grows this host refused that the module's own maximum allowed.
    ///
    /// A guest sees one `-1` and cannot tell "the module said this is as big
    /// as it gets" from "the host reserved less than that". The first is the
    /// program; the second is a setting, and a `malloc` that fails for it is a
    /// run to throw away — so ask this before believing what such a run did.
    /// `Instance::with_host_and_reservation` is where to change it.
    #[must_use]
    pub fn grows_refused_by_the_reservation(&self) -> u32 {
        self.refused.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// `memory.size`.
    #[must_use]
    pub fn size(&self) -> i32 {
        self.pages.load(std::sync::atomic::Ordering::Acquire) as i32
    }

    /// `memory.grow`, within the reservation.
    ///
    /// A compare-exchange loop rather than load-then-store: two threads growing
    /// one page each must publish two pages and report different old sizes.
    /// Reading and then storing lets both read the same size, both report it,
    /// and one page appear — which a guest allocator turns into two allocations
    /// at the same address.
    pub fn grow(&mut self, delta_pages: u32) -> i32 {
        let declared = u64::from(self.max_pages.unwrap_or(65536));
        let reserved = u64::from(self.reserved_pages());
        let mut old = self.pages.load(std::sync::atomic::Ordering::Acquire);
        loop {
            let new = u64::from(old) + u64::from(delta_pages);
            if new > declared.min(reserved) {
                // Two refusals that look identical to the guest and are not
                // the same thing at all. Past what the module declared is the
                // module's own limit; past the reservation is this host's, and
                // a guest whose `malloc` fails for that reason would otherwise
                // have no way to say so.
                if new <= declared {
                    self.refused
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
                return -1;
            }
            match self.pages.compare_exchange_weak(
                old,
                new as u32,
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
            ) {
                Ok(_) => return old as i32,
                Err(current) => old = current,
            }
        }
    }

    /// The committed bytes, copied out. For comparing one run against another.
    #[must_use]
    pub fn snapshot(&self) -> Vec<u8> {
        (0..self.byte_len()).map(|at| self.byte_at(at)).collect()
    }

    fn range(&self, addr: i32, offset: u64, size: u64) -> usize {
        let at = u64::from(addr as u32) + offset;
        if at + size > self.byte_len() as u64 {
            out_of_bounds(at, size, self.byte_len());
        }
        at as usize
    }

    fn get(&self, at: usize) -> u8 {
        self.cells[at].load(std::sync::atomic::Ordering::Relaxed)
    }

    fn set(&self, at: usize, value: u8) {
        self.cells[at].store(value, std::sync::atomic::Ordering::Relaxed);
    }

    fn read<const N: usize>(&self, addr: i32, offset: u64) -> [u8; N] {
        let at = self.range(addr, offset, N as u64);
        let mut bytes = [0u8; N];
        for (step, byte) in bytes.iter_mut().enumerate() {
            *byte = self.get(at + step);
        }
        bytes
    }

    fn write_bytes<const N: usize>(&self, addr: i32, offset: u64, bytes: [u8; N]) {
        let at = self.range(addr, offset, N as u64);
        for (step, byte) in bytes.iter().enumerate() {
            self.set(at + step, *byte);
        }
    }

    /// `i32.load8_u`
    pub fn load8_u(&self, addr: i32, offset: u64) -> i32 {
        i32::from(self.read::<1>(addr, offset)[0])
    }
    /// `i32.load8_s`
    pub fn load8_s(&self, addr: i32, offset: u64) -> i32 {
        i32::from(self.read::<1>(addr, offset)[0] as i8)
    }
    /// `i32.load16_u`
    pub fn load16_u(&self, addr: i32, offset: u64) -> i32 {
        i32::from(u16::from_le_bytes(self.read(addr, offset)))
    }
    /// `i32.load16_s`
    pub fn load16_s(&self, addr: i32, offset: u64) -> i32 {
        i32::from(i16::from_le_bytes(self.read(addr, offset)))
    }
    /// `i32.load`
    pub fn load32(&self, addr: i32, offset: u64) -> i32 {
        i32::from_le_bytes(self.read(addr, offset))
    }
    /// `i64.load`
    pub fn load64(&self, addr: i32, offset: u64) -> i64 {
        i64::from_le_bytes(self.read(addr, offset))
    }
    /// `f32.load`
    pub fn load_f32(&self, addr: i32, offset: u64) -> f32 {
        f32::from_le_bytes(self.read(addr, offset))
    }
    /// `f64.load`
    pub fn load_f64(&self, addr: i32, offset: u64) -> f64 {
        f64::from_le_bytes(self.read(addr, offset))
    }
    /// `i64.load8_s`
    pub fn load8_s_i64(&self, addr: i32, offset: u64) -> i64 {
        i64::from(self.read::<1>(addr, offset)[0] as i8)
    }
    /// `i64.load8_u`
    pub fn load8_u_i64(&self, addr: i32, offset: u64) -> i64 {
        i64::from(self.read::<1>(addr, offset)[0])
    }
    /// `i64.load16_s`
    pub fn load16_s_i64(&self, addr: i32, offset: u64) -> i64 {
        i64::from(i16::from_le_bytes(self.read(addr, offset)))
    }
    /// `i64.load16_u`
    pub fn load16_u_i64(&self, addr: i32, offset: u64) -> i64 {
        i64::from(u16::from_le_bytes(self.read(addr, offset)))
    }
    /// `i64.load32_s`
    pub fn load32_s_i64(&self, addr: i32, offset: u64) -> i64 {
        i64::from(i32::from_le_bytes(self.read(addr, offset)))
    }
    /// `i64.load32_u`
    pub fn load32_u_i64(&self, addr: i32, offset: u64) -> i64 {
        i64::from(u32::from_le_bytes(self.read(addr, offset)))
    }

    /// `i32.store8` / `i64.store8`
    pub fn store8(&mut self, addr: i32, offset: u64, value: i64) {
        self.write_bytes(addr, offset, [value as u8]);
    }
    /// `i32.store16` / `i64.store16`
    pub fn store16(&mut self, addr: i32, offset: u64, value: i64) {
        self.write_bytes(addr, offset, (value as u16).to_le_bytes());
    }
    /// `i32.store` / `i64.store32`
    pub fn store32(&mut self, addr: i32, offset: u64, value: i64) {
        self.write_bytes(addr, offset, (value as u32).to_le_bytes());
    }
    /// `i64.store`
    pub fn store64(&mut self, addr: i32, offset: u64, value: i64) {
        self.write_bytes(addr, offset, value.to_le_bytes());
    }
    /// `f32.store`
    pub fn store_f32(&mut self, addr: i32, offset: u64, value: f32) {
        self.write_bytes(addr, offset, value.to_le_bytes());
    }
    /// `f64.store`
    pub fn store_f64(&mut self, addr: i32, offset: u64, value: f64) {
        self.write_bytes(addr, offset, value.to_le_bytes());
    }

    /// `memory.fill`
    pub fn fill(&mut self, addr: i32, value: i32, len: i32) {
        let at = self.range(addr, 0, u64::from(len as u32));
        for step in 0..len as u32 as usize {
            self.set(at + step, value as u8);
        }
    }

    /// `memory.copy`, which moves as `memmove` does.
    pub fn copy(&mut self, dst: i32, src: i32, len: i32) {
        let len = len as u32 as usize;
        let to = self.range(dst, 0, len as u64);
        let from = self.range(src, 0, len as u64);
        // Through a buffer: the ranges may overlap, and another thread reading
        // the middle of a byte-by-byte move would see it half done either way.
        let moving: Vec<u8> = (0..len).map(|step| self.get(from + step)).collect();
        for (step, byte) in moving.iter().enumerate() {
            self.set(to + step, *byte);
        }
    }

    /// `memory.init`
    pub fn init(&mut self, dst: i32, segment: &[u8], src: i32, len: i32) {
        let len = len as u32 as usize;
        let src = src as u32 as usize;
        if src + len > segment.len() {
            trap("out of bounds memory access");
        }
        let to = self.range(dst, 0, len as u64);
        for (step, byte) in segment[src..src + len].iter().enumerate() {
            self.set(to + step, *byte);
        }
    }

    fn atomic_at(&self, addr: i32, offset: u64, bytes: u32) -> usize {
        let at = self.range(addr, offset, u64::from(bytes));
        if !at.is_multiple_of(bytes as usize) {
            trap("unaligned atomic operation");
        }
        at
    }

    /// Reads `bytes` bytes as one atomic access.
    pub fn atomic_load(&self, addr: i32, offset: u64, bytes: u32) -> i64 {
        let at = self.atomic_at(addr, offset, bytes);
        let _held = self.atomics.lock();
        let mut value = 0u64;
        for step in 0..bytes as usize {
            value |= u64::from(self.get(at + step)) << (8 * step);
        }
        value as i64
    }

    /// Writes the low `bytes` bytes as one atomic access.
    pub fn atomic_store(&mut self, addr: i32, offset: u64, bytes: u32, value: i64) {
        let at = self.atomic_at(addr, offset, bytes);
        let _held = self.atomics.lock();
        let value = value as u64;
        for step in 0..bytes as usize {
            self.set(at + step, (value >> (8 * step)) as u8);
        }
    }

    /// A read-modify-write, atomic against every other thread.
    pub fn atomic_rmw(&mut self, addr: i32, offset: u64, bytes: u32, op: Rmw, value: i64) -> i64 {
        let at = self.atomic_at(addr, offset, bytes);
        let _held = self.atomics.lock();
        let mut old = 0u64;
        for step in 0..bytes as usize {
            old |= u64::from(self.get(at + step)) << (8 * step);
        }
        let value_u = value as u64;
        let result = match op {
            Rmw::Add => old.wrapping_add(value_u),
            Rmw::Sub => old.wrapping_sub(value_u),
            Rmw::And => old & value_u,
            Rmw::Or => old | value_u,
            Rmw::Xor => old ^ value_u,
            Rmw::Xchg => value_u,
        };
        for step in 0..bytes as usize {
            self.set(at + step, (result >> (8 * step)) as u8);
        }
        old as i64
    }

    /// Compare-and-exchange, atomic against every other thread.
    pub fn atomic_cmpxchg(
        &mut self,
        addr: i32,
        offset: u64,
        bytes: u32,
        expected: i64,
        replacement: i64,
    ) -> i64 {
        let at = self.atomic_at(addr, offset, bytes);
        let _held = self.atomics.lock();
        let mut old = 0u64;
        for step in 0..bytes as usize {
            old |= u64::from(self.get(at + step)) << (8 * step);
        }
        let mask = if bytes >= 8 {
            u64::MAX
        } else {
            (1u64 << (8 * bytes)) - 1
        };
        if old == expected as u64 & mask {
            let replacement = replacement as u64;
            for step in 0..bytes as usize {
                self.set(at + step, (replacement >> (8 * step)) as u8);
            }
        }
        old as i64
    }

    /// `memory.atomic.notify`: wakes up to `count` threads parked here, and
    /// returns how many it woke.
    ///
    /// The count is the instruction's, and it decides the program: three
    /// threads parked on a lock and `notify(addr, 1)` means one of them
    /// proceeds. Waking all three and reporting three is a different program.
    pub fn atomic_notify_count(&self, addr: i32, offset: u64, count: i32) -> i32 {
        let at = self.atomic_at(addr, offset, 4);
        let wanted = if count < 0 {
            usize::MAX
        } else {
            count as usize
        };
        let mut parked = self
            .waiters
            .parked
            .lock()
            .unwrap_or_else(|held| held.into_inner());
        let mut woken = 0;
        while woken < wanted {
            let Some(queue) = parked.queues.get_mut(&at) else {
                break;
            };
            let Some(ticket) = queue.pop_front() else {
                parked.queues.remove(&at);
                break;
            };
            parked.woken.insert(ticket);
            woken += 1;
        }
        drop(parked);
        // Every waiter shares one condvar, so each wakes and checks its own
        // ticket. Waking a thread that goes back to sleep costs a context
        // switch; waking the wrong number of them costs a different program.
        self.waiters.wake.notify_all();
        woken as i32
    }

    /// `memory.atomic.notify` with no count, which wakes everybody.
    pub fn atomic_notify(&self, addr: i32, offset: u64) -> i32 {
        self.atomic_notify_count(addr, offset, i32::MAX)
    }

    /// `memory.atomic.wait32` / `wait64`, for real.
    ///
    /// Returns 0 if it was woken, 1 if the value had already changed, and 2 on
    /// timeout. The value is compared while holding the parking lock, so a
    /// notify that lands between the comparison and the parking cannot be lost.
    pub fn atomic_wait(
        &self,
        addr: i32,
        offset: u64,
        bytes: u32,
        expected: i64,
        timeout: i64,
    ) -> i32 {
        let at = self.atomic_at(addr, offset, bytes);
        let mut parked = self
            .waiters
            .parked
            .lock()
            .unwrap_or_else(|held| held.into_inner());
        if self.atomic_load(addr, offset, bytes) != expected {
            return 1;
        }
        if timeout == 0 {
            return 2;
        }
        // Nobody else holds this memory, so nobody can ever notify: the wait is
        // not slow, it is unsatisfiable. The single-threaded runtime traps here
        // for the same reason, on an assumption; this decides it on a fact, and
        // it is what lets a threaded module be run on one thread without
        // hanging.
        if timeout < 0 && std::sync::Arc::strong_count(&self.cells) == 1 {
            trap(
                "memory.atomic.wait would block forever: no other instance shares this \
                 memory, so no other thread can notify it",
            );
        }
        let ticket = {
            parked.next_ticket += 1;
            let ticket = parked.next_ticket;
            parked.queues.entry(at).or_default().push_back(ticket);
            ticket
        };
        // An absolute deadline, computed once. Passing the whole timeout again
        // after every wake-up lets unrelated notifications — every one of them
        // wakes every waiter — restart it, and a bounded wait becomes an
        // unbounded one.
        let deadline = (timeout >= 0)
            .then(|| std::time::Instant::now() + std::time::Duration::from_nanos(timeout as u64));
        let mut answer = 0;
        loop {
            if parked.woken.remove(&ticket) {
                break;
            }
            match deadline {
                None => {
                    parked = self
                        .waiters
                        .wake
                        .wait(parked)
                        .unwrap_or_else(|held| held.into_inner());
                }
                Some(deadline) => {
                    let now = std::time::Instant::now();
                    if now >= deadline {
                        answer = 2;
                        break;
                    }
                    let (next, _) = self
                        .waiters
                        .wake
                        .wait_timeout(parked, deadline - now)
                        .unwrap_or_else(|held| held.into_inner());
                    parked = next;
                }
            }
        }
        // Leaving without being notified means the ticket is still queued.
        if answer != 0
            && let Some(queue) = parked.queues.get_mut(&at)
        {
            queue.retain(|queued| *queued != ticket);
        }
        answer
    }

    /// Watches `bytes` bytes from `address`, in every thread.
    pub fn watch(&mut self, address: i32, bytes: i32) {
        let from = address as u32;
        self.watchpoints()
            .ranges
            .push((from, from.wrapping_add(bytes as u32)));
    }

    /// Panic at the first watched write rather than recording it.
    pub fn stop_on_hit(&mut self, stop: bool) {
        self.watchpoints().stop = stop;
    }

    /// The writes to watched addresses so far, from every thread.
    #[must_use]
    pub fn hits(&self) -> Vec<Hit> {
        self.watch
            .lock()
            .unwrap_or_else(|held| held.into_inner())
            .hits
            .clone()
    }

    fn watchpoints(&self) -> std::sync::MutexGuard<'_, Watchpoints> {
        self.watch.lock().unwrap_or_else(|held| held.into_inner())
    }

    /// Records a write that has already happened, if anyone is watching it.
    fn note(&self, site: u32, at: u32, addr: i32, offset: u64, bytes: u64, kind: WriteKind) {
        let mut watch = self.watchpoints();
        if watch.ranges.is_empty() {
            return;
        }
        let start = u64::from(addr as u32) + offset;
        let end = start + bytes;
        let watched = watch
            .ranges
            .iter()
            .any(|(from, to)| start < u64::from(*to) && end > u64::from(*from));
        if !watched {
            return;
        }
        watch.hits.push(Hit {
            site,
            at,
            address: start as u32 as i32,
            bytes: bytes as u32,
            kind,
        });
        let stop = watch.stop;
        drop(watch);
        if stop {
            panic!(
                "watchpoint: function #{site} wrote {bytes} bytes at {start:#x} ({kind:?}), \
                 from the instruction at file offset {at}, on thread {:?}",
                std::thread::current().id()
            );
        }
    }

    /// `i32.store8` / `i64.store8`, reporting to any watchpoint.
    pub fn store8_at(&mut self, site: u32, at: u32, addr: i32, offset: u64, value: i64) {
        self.store8(addr, offset, value);
        self.note(site, at, addr, offset, 1, WriteKind::Store);
    }
    /// `i32.store16` / `i64.store16`, reporting to any watchpoint.
    pub fn store16_at(&mut self, site: u32, at: u32, addr: i32, offset: u64, value: i64) {
        self.store16(addr, offset, value);
        self.note(site, at, addr, offset, 2, WriteKind::Store);
    }
    /// `i32.store` / `i64.store32`, reporting to any watchpoint.
    pub fn store32_at(&mut self, site: u32, at: u32, addr: i32, offset: u64, value: i64) {
        self.store32(addr, offset, value);
        self.note(site, at, addr, offset, 4, WriteKind::Store);
    }
    /// `i64.store`, reporting to any watchpoint.
    pub fn store64_at(&mut self, site: u32, at: u32, addr: i32, offset: u64, value: i64) {
        self.store64(addr, offset, value);
        self.note(site, at, addr, offset, 8, WriteKind::Store);
    }
    /// `f32.store`, reporting to any watchpoint.
    pub fn store_f32_at(&mut self, site: u32, at: u32, addr: i32, offset: u64, value: f32) {
        self.store_f32(addr, offset, value);
        self.note(site, at, addr, offset, 4, WriteKind::Store);
    }
    /// `f64.store`, reporting to any watchpoint.
    pub fn store_f64_at(&mut self, site: u32, at: u32, addr: i32, offset: u64, value: f64) {
        self.store_f64(addr, offset, value);
        self.note(site, at, addr, offset, 8, WriteKind::Store);
    }
    /// `memory.fill`, reporting to any watchpoint.
    pub fn fill_at(&mut self, site: u32, at: u32, addr: i32, value: i32, len: i32) {
        self.fill(addr, value, len);
        self.note(site, at, addr, 0, u64::from(len as u32), WriteKind::Fill);
    }
    /// `memory.copy`, reporting to any watchpoint on the destination.
    pub fn copy_at(&mut self, site: u32, at: u32, dst: i32, src: i32, len: i32) {
        self.copy(dst, src, len);
        self.note(site, at, dst, 0, u64::from(len as u32), WriteKind::Copy);
    }
    /// `memory.init`, reporting to any watchpoint on the destination.
    pub fn init_at(&mut self, site: u32, at: u32, dst: i32, segment: &[u8], src: i32, len: i32) {
        self.init(dst, segment, src, len);
        self.note(site, at, dst, 0, u64::from(len as u32), WriteKind::Init);
    }
    /// An atomic store, reporting to any watchpoint.
    pub fn atomic_store_at(
        &mut self,
        site: u32,
        at: u32,
        addr: i32,
        offset: u64,
        bytes: u32,
        value: i64,
    ) {
        self.atomic_store(addr, offset, bytes, value);
        self.note(site, at, addr, offset, u64::from(bytes), WriteKind::Atomic);
    }
    /// An atomic read-modify-write, reporting to any watchpoint.
    #[allow(clippy::too_many_arguments)]
    pub fn atomic_rmw_at(
        &mut self,
        site: u32,
        at: u32,
        addr: i32,
        offset: u64,
        bytes: u32,
        op: Rmw,
        value: i64,
    ) -> i64 {
        let old = self.atomic_rmw(addr, offset, bytes, op, value);
        self.note(site, at, addr, offset, u64::from(bytes), WriteKind::Atomic);
        old
    }
    /// An atomic compare-and-exchange, reporting to any watchpoint.
    #[allow(clippy::too_many_arguments)]
    pub fn atomic_cmpxchg_at(
        &mut self,
        site: u32,
        at: u32,
        addr: i32,
        offset: u64,
        bytes: u32,
        expected: i64,
        replacement: i64,
    ) -> i64 {
        let old = self.atomic_cmpxchg(addr, offset, bytes, expected, replacement);
        self.note(site, at, addr, offset, u64::from(bytes), WriteKind::Atomic);
        old
    }
}

impl Access for SharedMemory {
    fn byte_at(&self, at: usize) -> u8 {
        if at >= self.byte_len() {
            trap("out of bounds memory access");
        }
        self.get(at)
    }
    fn set_byte_at(&mut self, at: usize, value: u8) {
        if at >= self.byte_len() {
            trap("out of bounds memory access");
        }
        self.set(at, value);
    }
    fn byte_len(&self) -> usize {
        self.pages.load(std::sync::atomic::Ordering::Acquire) as usize * PAGE_SIZE
    }
    fn grow_pages(&mut self, delta_pages: u32) -> i32 {
        self.grow(delta_pages)
    }
    fn declared_max_pages(&self) -> Option<u32> {
        // The reservation is a ceiling as real as the declared maximum, and
        // reporting the larger of the two would promise memory that cannot be
        // allocated once other threads hold this one.
        Some(self.max_pages.unwrap_or(65536).min(self.reserved_pages()))
    }
}

impl PartialEq for SharedMemory {
    fn eq(&self, other: &Self) -> bool {
        self.snapshot() == other.snapshot()
    }
}

/// An exception thrown by the guest, on its way out through Rust's unwinder.
///
/// Emscripten compiles a C++ `throw` into a call to the imported
/// `__cxa_throw`, and the JavaScript glue turns that into a JS exception which
/// the `invoke_*` trampolines catch. A host here does the same thing by calling
/// [`throw`], and the generated trampolines catch exactly this type.
///
/// The distinction matters as much as the mechanism. The glue checks
/// `if (e !== e+0) throw e` — anything that is not one of its own exceptions is
/// re-thrown rather than swallowed. A trap is not a C++ exception, and a
/// trampoline that caught traps too would turn a crash into a silently handled
/// error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GuestException {
    /// The exception object, as the guest's `__cxa_throw` was given it.
    pub exception: i32,
    /// Its `std::type_info`, for a host that wants to match on the type.
    pub info: i32,
}

/// Whether a compare-and-exchange took: the old value, at the access width,
/// against what the guest expected.
fn exchanged(old: i64, expected: i64, bytes: u32) -> bool {
    let mask = if bytes >= 8 {
        u64::MAX
    } else {
        (1u64 << (8 * bytes)) - 1
    };
    old as u64 == expected as u64 & mask
}

/// `atomic.fence`, which orders this thread's accesses against the others.
///
/// Under one thread there is nothing to order and this is free. Under several
/// it is the instruction's whole point: the plain accesses either side of it
/// are relaxed atomics, and without the fence a reader can see them in an
/// order the guest excluded.
pub fn fence() {
    std::sync::atomic::fence(std::sync::atomic::Ordering::SeqCst);
}

/// Throws a guest exception. What a host's `__cxa_throw` calls.
///
/// # Panics
///
/// Always: that is what a throw is here. A generated `invoke_*` trampoline
/// catches it; anywhere else it propagates like any other panic.
pub fn throw(exception: i32, info: i32) -> ! {
    std::panic::panic_any(GuestException { exception, info });
}

/// The byte-addressable part of a memory, so a host can read one without
/// caring whether the module declared it shared.
///
/// Both memories implement it, and [`Caller`] is generic over it. A host
/// written against `Caller` therefore compiles unchanged for a threaded module
/// — which matters, because whether a memory is shared is the module's choice
/// and not the host author's.
pub trait Access {
    /// The byte at `at`. Traps if it is outside the memory.
    fn byte_at(&self, at: usize) -> u8;
    /// Writes the byte at `at`. Traps if it is outside the memory.
    fn set_byte_at(&mut self, at: usize, value: u8);
    /// How many bytes there are.
    fn byte_len(&self) -> usize;
    /// `memory.grow`, so a host can answer `emscripten_resize_heap` without
    /// knowing which memory it is holding.
    fn grow_pages(&mut self, delta_pages: u32) -> i32;
    /// The maximum the memory type declared, if it declared one.
    fn declared_max_pages(&self) -> Option<u32>;
}

impl Access for Memory {
    fn byte_at(&self, at: usize) -> u8 {
        match self.data.get(at) {
            Some(byte) => *byte,
            None => trap("out of bounds memory access"),
        }
    }
    fn set_byte_at(&mut self, at: usize, value: u8) {
        match self.data.get_mut(at) {
            Some(byte) => *byte = value,
            None => trap("out of bounds memory access"),
        }
    }
    fn byte_len(&self) -> usize {
        self.data.len()
    }
    fn grow_pages(&mut self, delta_pages: u32) -> i32 {
        self.grow(delta_pages)
    }
    fn declared_max_pages(&self) -> Option<u32> {
        self.max_pages
    }
}

/// What an import is given besides its arguments.
///
/// A wasm import receives numbers, and almost every interesting one is a
/// number *into* linear memory: `fd_write` is handed the address of an iovec
/// array, `__assert_fail` the address of a string. Without the memory a host
/// cannot answer any of them, so every import method takes one of these.
///
/// It is a struct rather than a bare `&mut Memory` because it is the place the
/// next capability goes — the table, the globals, a way back into an export —
/// and adding a field costs nothing, while adding a parameter changes every
/// host ever written.
#[derive(Debug)]
pub struct Caller<'a, M: Access = Memory> {
    /// The instance's linear memory. `Memory` unless the module declared a
    /// shared one, in which case [`SharedMemory`].
    pub memory: &'a mut M,
}

impl<M: Access> Caller<'_, M> {
    /// The `len` bytes at `addr`.
    ///
    /// A copy rather than a slice: a shared memory is not a `&[u8]` anybody
    /// can borrow, since another thread may be writing it.
    ///
    /// # Panics
    ///
    /// Traps if the range leaves the memory, exactly as a guest load would:
    /// a host reading past the end is the same mistake with the same answer.
    #[must_use]
    pub fn bytes(&self, addr: i32, len: i32) -> Vec<u8> {
        let start = u64::from(addr as u32);
        let end = start + u64::from(len as u32);
        if end > self.memory.byte_len() as u64 {
            trap("out of bounds memory access");
        }
        (start..end)
            .map(|at| self.memory.byte_at(at as usize))
            .collect()
    }

    /// Writes `bytes` at `addr`.
    ///
    /// # Panics
    ///
    /// Traps if the range leaves the memory.
    pub fn write(&mut self, addr: i32, bytes: &[u8]) {
        let start = u64::from(addr as u32);
        let end = start + bytes.len() as u64;
        if end > self.memory.byte_len() as u64 {
            trap("out of bounds memory access");
        }
        for (step, byte) in bytes.iter().enumerate() {
            self.memory.set_byte_at(start as usize + step, *byte);
        }
    }

    /// The NUL-terminated string at `addr`, without its terminator.
    ///
    /// # Panics
    ///
    /// Traps if there is no NUL before the end of memory. Returning what was
    /// found instead would hand the host a string the guest never wrote.
    #[must_use]
    pub fn cstring(&self, addr: i32) -> Vec<u8> {
        let start = u64::from(addr as u32) as usize;
        if start > self.memory.byte_len() {
            trap("out of bounds memory access");
        }
        let mut text = Vec::new();
        for at in start..self.memory.byte_len() {
            let byte = self.memory.byte_at(at);
            if byte == 0 {
                return text;
            }
            text.push(byte);
        }
        trap("unterminated string")
    }

    /// The four bytes at `addr`, as the little-endian i32 the guest wrote.
    #[must_use]
    pub fn read_i32(&self, addr: i32) -> i32 {
        let bytes = self.bytes(addr, 4);
        i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
    }

    /// Writes an i32 at `addr`.
    pub fn write_i32(&mut self, addr: i32, value: i32) {
        self.write(addr, &value.to_le_bytes());
    }

    /// Writes an i64 at `addr`.
    pub fn write_i64(&mut self, addr: i32, value: i64) {
        self.write(addr, &value.to_le_bytes());
    }
}

/// Which read-modify-write an atomic performs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rmw {
    /// `add`
    Add,
    /// `sub`
    Sub,
    /// `and`
    And,
    /// `or`
    Or,
    /// `xor`
    Xor,
    /// `xchg`: the old value out, the new one in.
    Xchg,
}

/// `i32.div_s`. Traps on zero and on `i32::MIN / -1`, which has no result.
pub fn i32_div_s(lhs: i32, rhs: i32) -> i32 {
    if rhs == 0 {
        trap("integer divide by zero");
    }
    match lhs.checked_div(rhs) {
        Some(result) => result,
        None => trap("integer overflow"),
    }
}

/// `i32.div_u`
pub fn i32_div_u(lhs: i32, rhs: i32) -> i32 {
    if rhs == 0 {
        trap("integer divide by zero");
    }
    ((lhs as u32) / (rhs as u32)) as i32
}

/// `i32.rem_s`. `i32::MIN % -1` is 0 in wasm and a panic in Rust, hence
/// `wrapping_rem`.
pub fn i32_rem_s(lhs: i32, rhs: i32) -> i32 {
    if rhs == 0 {
        trap("integer divide by zero");
    }
    lhs.wrapping_rem(rhs)
}

/// `i32.rem_u`
pub fn i32_rem_u(lhs: i32, rhs: i32) -> i32 {
    if rhs == 0 {
        trap("integer divide by zero");
    }
    ((lhs as u32) % (rhs as u32)) as i32
}

/// `i64.div_s`
pub fn i64_div_s(lhs: i64, rhs: i64) -> i64 {
    if rhs == 0 {
        trap("integer divide by zero");
    }
    match lhs.checked_div(rhs) {
        Some(result) => result,
        None => trap("integer overflow"),
    }
}

/// `i64.div_u`
pub fn i64_div_u(lhs: i64, rhs: i64) -> i64 {
    if rhs == 0 {
        trap("integer divide by zero");
    }
    ((lhs as u64) / (rhs as u64)) as i64
}

/// `i64.rem_s`
pub fn i64_rem_s(lhs: i64, rhs: i64) -> i64 {
    if rhs == 0 {
        trap("integer divide by zero");
    }
    lhs.wrapping_rem(rhs)
}

/// `i64.rem_u`
pub fn i64_rem_u(lhs: i64, rhs: i64) -> i64 {
    if rhs == 0 {
        trap("integer divide by zero");
    }
    ((lhs as u64) % (rhs as u64)) as i64
}

/// `f32.min`. Wasm propagates NaN; `f32::min` returns the other operand
/// instead, and `min(-0.0, 0.0)` must be `-0.0` rather than either.
pub fn f32_min(lhs: f32, rhs: f32) -> f32 {
    if lhs.is_nan() || rhs.is_nan() {
        return f32::NAN;
    }
    if lhs == 0.0 && rhs == 0.0 {
        return if lhs.is_sign_negative() { lhs } else { rhs };
    }
    if lhs < rhs { lhs } else { rhs }
}

/// `f32.max`
pub fn f32_max(lhs: f32, rhs: f32) -> f32 {
    if lhs.is_nan() || rhs.is_nan() {
        return f32::NAN;
    }
    if lhs == 0.0 && rhs == 0.0 {
        return if lhs.is_sign_positive() { lhs } else { rhs };
    }
    if lhs > rhs { lhs } else { rhs }
}

/// `f64.min`
pub fn f64_min(lhs: f64, rhs: f64) -> f64 {
    if lhs.is_nan() || rhs.is_nan() {
        return f64::NAN;
    }
    if lhs == 0.0 && rhs == 0.0 {
        return if lhs.is_sign_negative() { lhs } else { rhs };
    }
    if lhs < rhs { lhs } else { rhs }
}

/// `f64.max`
pub fn f64_max(lhs: f64, rhs: f64) -> f64 {
    if lhs.is_nan() || rhs.is_nan() {
        return f64::NAN;
    }
    if lhs == 0.0 && rhs == 0.0 {
        return if lhs.is_sign_positive() { lhs } else { rhs };
    }
    if lhs > rhs { lhs } else { rhs }
}

/// Traps when a float cannot be truncated into the target integer.
///
/// Shared by every `trunc` below: NaN has no integer value, and a value outside
/// the range would otherwise saturate silently under Rust's `as`.
fn check_trunc(is_nan: bool, in_range: bool) {
    if is_nan {
        trap("invalid conversion to integer");
    }
    if !in_range {
        trap("integer overflow");
    }
}

/// `i32.trunc_f32_s`
pub fn i32_trunc_f32_s(value: f32) -> i32 {
    check_trunc(
        value.is_nan(),
        value > -2147483904.0 && value < 2147483648.0,
    );
    value as i32
}
/// `i32.trunc_f32_u`
pub fn i32_trunc_f32_u(value: f32) -> i32 {
    check_trunc(value.is_nan(), value > -1.0 && value < 4294967296.0);
    value as u32 as i32
}
/// `i32.trunc_f64_s`
pub fn i32_trunc_f64_s(value: f64) -> i32 {
    check_trunc(
        value.is_nan(),
        value > -2147483649.0 && value < 2147483648.0,
    );
    value as i32
}
/// `i32.trunc_f64_u`
pub fn i32_trunc_f64_u(value: f64) -> i32 {
    check_trunc(value.is_nan(), value > -1.0 && value < 4294967296.0);
    value as u32 as i32
}
/// `i64.trunc_f32_s`
pub fn i64_trunc_f32_s(value: f32) -> i64 {
    check_trunc(
        value.is_nan(),
        (-9223372036854775808.0..9223372036854775808.0).contains(&value),
    );
    value as i64
}
/// `i64.trunc_f32_u`
pub fn i64_trunc_f32_u(value: f32) -> i64 {
    check_trunc(
        value.is_nan(),
        value > -1.0 && value < 18446744073709551616.0,
    );
    value as u64 as i64
}
/// `i64.trunc_f64_s`
pub fn i64_trunc_f64_s(value: f64) -> i64 {
    check_trunc(
        value.is_nan(),
        (-9223372036854775808.0..9223372036854775808.0).contains(&value),
    );
    value as i64
}
/// `i64.trunc_f64_u`
pub fn i64_trunc_f64_u(value: f64) -> i64 {
    check_trunc(
        value.is_nan(),
        value > -1.0 && value < 18446744073709551616.0,
    );
    value as u64 as i64
}

/// `f32.nearest` / `f64.nearest`: round half to even, not away from zero.
pub fn f32_nearest(value: f32) -> f32 {
    value.round_ties_even()
}
/// `f64.nearest`
pub fn f64_nearest(value: f64) -> f64 {
    value.round_ties_even()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_caller_reads_and_writes_what_the_guest_can() {
        let mut memory = Memory::new(1, None);
        let mut caller = Caller {
            memory: &mut memory,
        };
        caller.write(16, b"hello\0world");
        assert_eq!(caller.bytes(16, 5), b"hello");
        assert_eq!(caller.cstring(16), b"hello");
        // An empty string is a NUL at the address, not an absent one.
        caller.write(16, b"\0");
        assert_eq!(caller.cstring(16), b"");
        assert_eq!(caller.bytes(0, 0), b"");
    }

    #[test]
    fn an_out_of_bounds_trap_says_which_kind_of_mistake_it_was() {
        // One byte past the end and a pointer read out of uninitialised memory
        // are different bugs with the same message in every engine. The
        // address tells them apart, and this is the moment a decompilation has
        // most to say.
        let memory = Memory::new(1, None);
        let near = std::panic::catch_unwind(|| memory.load32((PAGE_SIZE - 2) as i32, 0));
        let message = near
            .expect_err("it traps")
            .downcast_ref::<String>()
            .cloned()
            .unwrap_or_default();
        assert!(message.contains("out of bounds memory access"), "{message}");
        assert!(message.contains("4 bytes at 0xfffe"), "{message}");
        assert!(message.contains("the memory is 65536 bytes"), "{message}");
        assert!(message.contains("0.1 MiB"), "{message}");

        let wild = std::panic::catch_unwind(|| memory.load32(0x24b_ed0, 0));
        let message = wild
            .expect_err("it traps")
            .downcast_ref::<String>()
            .cloned()
            .unwrap_or_default();
        assert!(message.contains("at 0x24bed0"), "{message}");
    }

    #[test]
    #[should_panic(expected = "out of bounds memory access")]
    fn a_caller_reading_past_the_end_traps() {
        let mut memory = Memory::new(1, None);
        let caller = Caller {
            memory: &mut memory,
        };
        let _ = caller.bytes((PAGE_SIZE - 4) as i32, 8);
    }

    #[test]
    #[should_panic(expected = "out of bounds memory access")]
    fn a_caller_writing_past_the_end_traps() {
        let mut memory = Memory::new(1, None);
        let mut caller = Caller {
            memory: &mut memory,
        };
        caller.write((PAGE_SIZE - 2) as i32, b"four");
    }

    #[test]
    #[should_panic(expected = "out of bounds memory access")]
    fn a_string_outside_memory_traps_rather_than_slicing() {
        let mut memory = Memory::new(1, None);
        let caller = Caller {
            memory: &mut memory,
        };
        let _ = caller.cstring(PAGE_SIZE as i32 + 1);
    }

    #[test]
    #[should_panic(expected = "unterminated string")]
    fn a_string_with_no_terminator_traps() {
        let mut memory = Memory::new(1, None);
        memory.data.iter_mut().for_each(|byte| *byte = b'x');
        let caller = Caller {
            memory: &mut memory,
        };
        let _ = caller.cstring(0);
    }

    // ---- a memory several threads share ----

    #[test]
    fn a_shared_memory_starts_at_its_declared_size_inside_its_reservation() {
        let memory = SharedMemory::new(2, Some(8), 4);
        assert_eq!(memory.size(), 2);
        assert_eq!(memory.reserved_pages(), 4);
        assert_eq!(memory.snapshot().len(), 2 * PAGE_SIZE);
        assert!(memory.snapshot().iter().all(|byte| *byte == 0));
    }

    #[test]
    #[should_panic(expected = "the reservation must cover")]
    fn reserving_less_than_the_module_asks_for_is_a_host_mistake() {
        let _ = SharedMemory::new(4, None, 2);
    }

    #[test]
    fn growing_publishes_reserved_pages_and_stops_at_the_reservation() {
        let mut memory = SharedMemory::new(1, Some(8), 3);
        assert_eq!(memory.grow(1), 1);
        assert_eq!(memory.size(), 2);
        // Past the reservation is a refusal, not a reallocation: another
        // thread is holding this memory and it cannot move.
        assert_eq!(memory.grow(2), -1);
        assert_eq!(memory.size(), 2);
        // And past the declared maximum, even inside the reservation.
        let mut small = SharedMemory::new(1, Some(2), 8);
        assert_eq!(small.grow(1), 1);
        assert_eq!(small.grow(1), -1);
    }

    #[test]
    fn a_shared_memory_reads_and_writes_what_a_plain_one_does() {
        let mut memory = SharedMemory::new(1, None, 1);
        memory.store8(0, 0, 0xFF);
        memory.store16(0, 8, 0xBEEF);
        memory.store32(0, 16, 0x1234_5678);
        memory.store64(0, 24, 0x0102_0304_0506_0708);
        memory.store_f32(0, 40, 1.5);
        memory.store_f64(0, 48, -2.25);
        assert_eq!(memory.load8_u(0, 0), 255);
        assert_eq!(memory.load8_s(0, 0), -1);
        assert_eq!(memory.load16_u(0, 8), 0xBEEF);
        assert_eq!(memory.load16_s(0, 8), 0xBEEF_u16 as i16 as i32);
        assert_eq!(memory.load32(0, 16), 0x1234_5678);
        assert_eq!(memory.load64(0, 24), 0x0102_0304_0506_0708);
        assert_eq!(memory.load_f32(0, 40), 1.5);
        assert_eq!(memory.load_f64(0, 48), -2.25);
        assert_eq!(memory.load8_s_i64(0, 0), -1);
        assert_eq!(memory.load8_u_i64(0, 0), 255);
        assert_eq!(memory.load16_s_i64(0, 8), 0xBEEF_u16 as i16 as i64);
        assert_eq!(memory.load16_u_i64(0, 8), 0xBEEF);
        assert_eq!(memory.load32_s_i64(0, 16), 0x1234_5678);
        assert_eq!(memory.load32_u_i64(0, 16), 0x1234_5678);

        memory.fill(64, 7, 4);
        memory.copy(72, 64, 4);
        memory.init(80, b"abcd", 1, 2);
        assert_eq!(memory.load32(64, 0), 0x0707_0707);
        assert_eq!(memory.load32(72, 0), 0x0707_0707);
        assert_eq!(
            memory.load16_u(80, 0),
            u32::from(u16::from_le_bytes(*b"bc")) as i32
        );
    }

    #[test]
    fn a_shared_copy_that_overlaps_moves_the_bytes_it_started_with() {
        let mut memory = SharedMemory::new(1, None, 1);
        memory.init(0, b"12345678", 0, 8);
        memory.copy(2, 0, 6);
        assert_eq!(&memory.snapshot()[0..8], b"12123456");
    }

    #[test]
    #[should_panic(expected = "out of bounds memory access")]
    fn a_shared_access_past_the_committed_size_traps_even_inside_the_reservation() {
        // The reservation is not the memory: bytes beyond `memory.size()` are
        // not addressable, and a host that could reach them would be reading
        // pages the module never grew into.
        let memory = SharedMemory::new(1, None, 4);
        let _ = memory.load32(PAGE_SIZE as i32, 0);
    }

    #[test]
    fn two_threads_see_each_other_writes() {
        let memory = SharedMemory::new(1, None, 1);
        let mut writer = memory.clone();
        let handle = std::thread::spawn(move || {
            writer.store32(64, 0, 0x2A);
            writer.atomic_notify(64, 0)
        });
        handle.join().expect("the writer finishes");
        assert_eq!(memory.load32(64, 0), 0x2A, "one memory, two handles");
    }

    #[test]
    fn an_atomic_read_modify_write_from_two_threads_loses_nothing() {
        // Byte-wise atomics cannot make a four-byte add atomic on their own,
        // which is what the lock inside `atomic_rmw` is for. Without it this
        // test loses increments.
        let memory = SharedMemory::new(1, None, 1);
        let threads: Vec<_> = (0..4)
            .map(|_| {
                let mut counter = memory.clone();
                std::thread::spawn(move || {
                    for _ in 0..500 {
                        counter.atomic_rmw(0, 0, 4, Rmw::Add, 1);
                    }
                })
            })
            .collect();
        for thread in threads {
            thread.join().expect("each thread finishes");
        }
        assert_eq!(memory.atomic_load(0, 0, 4), 2000);
    }

    #[test]
    fn waiting_is_woken_by_a_notify_from_another_thread() {
        let memory = SharedMemory::new(1, None, 1);
        let waiting = memory.clone();
        let waiter = std::thread::spawn(move || waiting.atomic_wait(0, 0, 4, 0, -1));
        // Wake it until it is actually parked: a notify that arrives before
        // the wait is not lost — the value is compared under the parking lock —
        // but it is not a wake-up either.
        let mut woken = 0;
        while woken == 0 {
            std::thread::yield_now();
            woken = memory.atomic_notify(0, 0);
        }
        assert_eq!(waiter.join().expect("the waiter returns"), 0);
    }

    #[test]
    fn waiting_for_a_value_that_already_changed_returns_immediately() {
        let mut memory = SharedMemory::new(1, None, 1);
        memory.atomic_store(0, 0, 4, 7);
        assert_eq!(memory.atomic_wait(0, 0, 4, 0, -1), 1);
        // A zero timeout expires immediately whoever else is running.
        assert_eq!(memory.atomic_wait(0, 0, 4, 7, 0), 2);
        // And a real timeout expires when nobody notifies.
        assert_eq!(memory.atomic_wait(0, 0, 4, 7, 1_000_000), 2);
    }

    #[test]
    fn notify_wakes_the_number_it_was_asked_for_and_no_more() {
        // The count is the instruction's, and it decides the program: three
        // threads on a lock and `notify(addr, 1)` means one proceeds.
        let memory = SharedMemory::new(1, None, 1);
        let waiters: Vec<_> = (0..3)
            .map(|_| {
                let waiting = memory.clone();
                std::thread::spawn(move || waiting.atomic_wait(0, 0, 4, 0, -1))
            })
            .collect();

        // Park all three before waking any: `notify` reports what it woke, so
        // this loop is also the proof they are parked.
        let mut parked = 0;
        let mut woken_first = 0;
        while parked < 3 {
            std::thread::yield_now();
            let woke = memory.atomic_notify_count(0, 0, 1);
            parked += woke;
            woken_first += woke;
            if woken_first > 0 && parked < 3 {
                // One is awake; the others are still arriving. Keep going with
                // a count of one so nothing extra is woken by accident.
                continue;
            }
        }
        assert_eq!(woken_first, 3, "one at a time, three times");
        for waiter in waiters {
            assert_eq!(waiter.join().expect("returns"), 0);
        }
        // Nobody is left, so a further notify wakes nothing.
        assert_eq!(memory.atomic_notify_count(0, 0, 10), 0);
    }

    #[test]
    fn notifying_zero_threads_wakes_none_of_them() {
        let memory = SharedMemory::new(1, None, 1);
        let waiting = memory.clone();
        let waiter = std::thread::spawn(move || waiting.atomic_wait(0, 0, 4, 0, -1));
        // A count of zero must not wake anybody, however many are parked.
        for _ in 0..50 {
            assert_eq!(memory.atomic_notify_count(0, 0, 0), 0);
            std::thread::yield_now();
        }
        let mut woken = 0;
        while woken == 0 {
            std::thread::yield_now();
            woken = memory.atomic_notify_count(0, 0, 1);
        }
        assert_eq!(waiter.join().expect("returns"), 0);
    }

    #[test]
    fn a_finite_wait_expires_however_much_unrelated_traffic_there_is() {
        // Every notify wakes every waiter, whatever address it was for. A
        // timeout re-armed on each wake-up would never expire.
        let memory = SharedMemory::new(1, None, 1);
        let waiting = memory.clone();
        let waiter = std::thread::spawn(move || {
            let started = std::time::Instant::now();
            let answer = waiting.atomic_wait(0, 0, 4, 0, 200_000_000);
            (answer, started.elapsed())
        });
        let noise = memory.clone();
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stopping = stop.clone();
        let noisy = std::thread::spawn(move || {
            while !stopping.load(std::sync::atomic::Ordering::Relaxed) {
                // A different address, so it never wakes the waiter's ticket.
                noise.atomic_notify(64, 0);
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        });
        let (answer, elapsed) = waiter.join().expect("the waiter returns");
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        noisy.join().expect("the noise stops");
        assert_eq!(answer, 2, "timed out");
        assert!(
            elapsed < std::time::Duration::from_secs(5),
            "and within its own bound rather than being restarted: {elapsed:?}"
        );
    }

    #[test]
    fn a_grow_the_host_refused_is_not_a_grow_the_module_refused() {
        // The guest sees one -1 either way. A host that reserved too little
        // has to be able to tell, or a run whose `malloc` failed for that
        // reason reads as a run that found something out.
        let mut memory = SharedMemory::new(1, Some(64), 4);
        assert_eq!(memory.grow(2), 1);
        assert_eq!(memory.grows_refused_by_the_reservation(), 0);

        // Past the reservation, inside what the module declared: the host's
        // limit, and counted.
        assert_eq!(memory.grow(8), -1);
        assert_eq!(memory.grows_refused_by_the_reservation(), 1);

        // Past what the module declared: the module's own limit, and not.
        let mut small = SharedMemory::new(1, Some(2), 64);
        assert_eq!(small.grow(8), -1);
        assert_eq!(small.grows_refused_by_the_reservation(), 0);

        // The count is shared, like everything else about this memory: a
        // thread that hit the ceiling tells the thread that reads it.
        let mut other = memory.clone();
        assert_eq!(other.grow(8), -1);
        assert_eq!(memory.grows_refused_by_the_reservation(), 2);
    }

    #[test]
    fn two_threads_growing_at_once_both_get_their_pages() {
        // Load-then-store lets both read the same size, both report it, and
        // one page appear — which a guest allocator turns into two allocations
        // at one address.
        const THREADS: usize = 8;
        const EACH: usize = 16;
        let memory = SharedMemory::new(1, None, (THREADS * EACH) as u32 + 1);
        let start = std::sync::Arc::new(std::sync::Barrier::new(THREADS));
        let growers: Vec<_> = (0..THREADS)
            .map(|_| {
                let mut growing = memory.clone();
                let start = start.clone();
                std::thread::spawn(move || {
                    start.wait();
                    // Sixteen each, in a tight loop, so the compare-exchange
                    // actually contends rather than happening to be serialised.
                    (0..EACH).map(|_| growing.grow(1)).collect::<Vec<i32>>()
                })
            })
            .collect();
        let mut reported: Vec<i32> = growers
            .into_iter()
            .flat_map(|grower| grower.join().expect("returns"))
            .collect();
        reported.sort_unstable();
        let expected: Vec<i32> = (1..=(THREADS * EACH) as i32).collect();
        assert_eq!(reported, expected, "each grow got its own old size");
        assert_eq!(
            memory.size(),
            (THREADS * EACH) as i32 + 1,
            "and every page was published"
        );
    }

    #[test]
    fn a_fence_is_emitted_rather_than_assumed_away() {
        // Nothing to assert about its effect from one thread; what matters is
        // that it exists and is callable, since the emitter now lowers a guest
        // `atomic.fence` to it.
        fence();
    }

    #[test]
    fn notifying_an_address_nobody_waits_on_wakes_nothing() {
        let memory = SharedMemory::new(1, None, 1);
        assert_eq!(memory.atomic_notify(0, 0), 0);
        assert_eq!(memory.atomic_notify_count(0, 0, 0), 0);
        // A negative count is "everybody", which is how `-1` reaches here from
        // a guest that spelled the count as `i32.const -1`.
        assert_eq!(memory.atomic_notify_count(0, 0, -1), 0);
    }

    #[test]
    fn a_negative_count_wakes_everybody() {
        let memory = SharedMemory::new(1, None, 1);
        let waiters: Vec<_> = (0..3)
            .map(|_| {
                let waiting = memory.clone();
                std::thread::spawn(move || waiting.atomic_wait(0, 0, 4, 0, -1))
            })
            .collect();
        let mut woken = 0;
        while woken < 3 {
            std::thread::yield_now();
            woken += memory.atomic_notify_count(0, 0, -1);
        }
        for waiter in waiters {
            assert_eq!(waiter.join().expect("returns"), 0);
        }
    }

    #[test]
    fn an_eight_byte_exchange_that_takes_is_reported() {
        let mut memory = Memory::new(1, None);
        memory.watch(0x100, 8);
        memory.atomic_store(0x100, 0, 8, -1);
        assert_eq!(memory.atomic_cmpxchg_at(1, 10, 0x100, 0, 8, -1, 7), -1);
        assert_eq!(memory.load64(0x100, 0), 7);
        assert_eq!(memory.hits().len(), 1, "the whole 64-bit value matched");
    }

    #[test]
    fn a_shared_atomic_access_that_is_not_aligned_traps() {
        let memory = SharedMemory::new(1, None, 1);
        let unaligned = std::panic::catch_unwind(|| memory.atomic_load(1, 0, 4));
        assert!(unaligned.is_err());
    }

    #[test]
    fn a_shared_cmpxchg_exchanges_only_on_a_match() {
        let mut memory = SharedMemory::new(1, None, 1);
        memory.atomic_store(0, 0, 4, 5);
        assert_eq!(memory.atomic_cmpxchg(0, 0, 4, 4, 9), 5);
        assert_eq!(memory.atomic_load(0, 0, 4), 5, "no match, no write");
        assert_eq!(memory.atomic_cmpxchg(0, 0, 4, 5, 9), 5);
        assert_eq!(memory.atomic_load(0, 0, 4), 9);
        // The comparison is at the access width, so the high bits of what the
        // module passes do not decide it.
        memory.atomic_store(0, 0, 1, 3);
        assert_eq!(memory.atomic_cmpxchg(0, 0, 1, 0x103, 4), 3);
        assert_eq!(memory.atomic_load(0, 0, 1), 4);
    }

    #[test]
    fn a_shared_rmw_covers_every_operation() {
        let mut memory = SharedMemory::new(1, None, 1);
        memory.atomic_store(0, 0, 4, 0b1100);
        assert_eq!(memory.atomic_rmw(0, 0, 4, Rmw::Or, 0b0011), 0b1100);
        assert_eq!(memory.atomic_rmw(0, 0, 4, Rmw::And, 0b1010), 0b1111);
        assert_eq!(memory.atomic_rmw(0, 0, 4, Rmw::Xor, 0b0110), 0b1010);
        assert_eq!(memory.atomic_rmw(0, 0, 4, Rmw::Sub, 1), 0b1100);
        assert_eq!(memory.atomic_rmw(0, 0, 4, Rmw::Xchg, 42), 0b1011);
        assert_eq!(memory.atomic_load(0, 0, 4), 42);
    }

    #[test]
    fn a_watchpoint_on_a_shared_memory_collects_from_every_thread() {
        let mut memory = SharedMemory::new(1, None, 1);
        memory.watch(256, 4);
        memory.store32_at(1, 14, 256, 0, 1);
        memory.store8_at(2, 24, 256, 0, 1);
        memory.store16_at(3, 34, 256, 0, 1);
        memory.store64_at(4, 44, 256, 0, 1);
        memory.store_f32_at(5, 54, 256, 0, 1.0);
        memory.store_f64_at(6, 64, 256, 0, 1.0);
        memory.fill_at(7, 74, 256, 0, 4);
        memory.copy_at(8, 84, 256, 0, 4);
        memory.init_at(9, 94, 256, b"abcd", 0, 4);
        memory.atomic_store_at(10, 104, 256, 0, 4, 1);
        memory.atomic_rmw_at(11, 114, 256, 0, 4, Rmw::Add, 1);
        memory.atomic_cmpxchg_at(12, 124, 256, 0, 4, 0, 1);
        memory.store32_at(13, 134, 1024, 0, 1);

        let mut other = memory.clone();
        std::thread::spawn(move || other.store32_at(99, 994, 256, 0, 5))
            .join()
            .expect("the other thread finishes");

        let sites: Vec<u32> = memory.hits().iter().map(|hit| hit.site).collect();
        assert_eq!(sites, vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 99]);
    }

    #[test]
    #[should_panic(expected = "watchpoint: function #3 wrote 4 bytes at 0x100")]
    fn stopping_on_a_shared_hit_names_the_thread() {
        let mut memory = SharedMemory::new(1, None, 1);
        memory.watch(0x100, 4);
        memory.stop_on_hit(true);
        memory.store32_at(3, 34, 0x100, 0, 1);
    }

    #[test]
    fn two_shared_memories_holding_the_same_bytes_compare_equal() {
        let mut one = SharedMemory::new(1, None, 1);
        let mut two = SharedMemory::new(1, None, 2);
        assert_eq!(one, two, "the reservation is not the memory");
        one.store32(0, 0, 1);
        assert_ne!(one, two);
        two.store32(0, 0, 1);
        assert_eq!(one, two);
    }

    #[test]
    #[should_panic(expected = "out of bounds memory access")]
    fn a_shared_init_past_the_end_of_the_segment_traps() {
        let mut memory = SharedMemory::new(1, None, 1);
        memory.init(0, b"ab", 1, 4);
    }

    #[test]
    fn a_timed_wait_that_is_notified_reports_being_woken() {
        // The other side of the timeout loop: woken before the deadline, which
        // returns 0 rather than 2 and is what a lock handoff looks like.
        let memory = SharedMemory::new(1, None, 1);
        let waiting = memory.clone();
        let waiter = std::thread::spawn(move || waiting.atomic_wait(0, 0, 4, 0, 30_000_000_000));
        let mut woken = 0;
        while woken == 0 {
            std::thread::yield_now();
            woken = memory.atomic_notify(0, 0);
        }
        assert_eq!(waiter.join().expect("the waiter returns"), 0);
    }

    #[test]
    #[should_panic(expected = "out of bounds memory access")]
    fn a_shared_byte_read_past_the_end_traps() {
        let memory = SharedMemory::new(1, None, 4);
        let _ = memory.byte_at(PAGE_SIZE);
    }

    #[test]
    #[should_panic(expected = "out of bounds memory access")]
    fn a_shared_byte_write_past_the_end_traps() {
        let mut memory = SharedMemory::new(1, None, 4);
        memory.set_byte_at(PAGE_SIZE, 1);
    }

    #[test]
    #[should_panic(expected = "out of bounds memory access")]
    fn a_plain_byte_read_past_the_end_traps() {
        let memory = Memory::new(1, None);
        let _ = memory.byte_at(PAGE_SIZE);
    }

    #[test]
    #[should_panic(expected = "out of bounds memory access")]
    fn a_plain_byte_write_past_the_end_traps() {
        let mut memory = Memory::new(1, None);
        memory.set_byte_at(PAGE_SIZE, 1);
    }

    #[test]
    fn growing_through_the_trait_answers_for_either_memory() {
        // What a host calls to answer `emscripten_resize_heap`, without
        // knowing which memory the module declared.
        let mut plain = Memory::new(1, Some(4));
        assert_eq!(plain.grow_pages(1), 1);
        assert_eq!(plain.declared_max_pages(), Some(4));

        let mut shared = SharedMemory::new(1, Some(8), 3);
        assert_eq!(shared.grow_pages(1), 1);
        // The reservation is a ceiling as real as the declared maximum, so the
        // smaller of the two is what a host is told.
        assert_eq!(shared.declared_max_pages(), Some(3));
        let undeclared = SharedMemory::new(1, None, 2);
        assert_eq!(undeclared.declared_max_pages(), Some(2));
    }

    #[test]
    fn a_snapshot_is_the_bytes_either_kind_of_memory_holds() {
        let mut plain = Memory::new(1, None);
        plain.store32(0, 0, 7);
        assert_eq!(plain.snapshot().len(), PAGE_SIZE);
        assert_eq!(plain.snapshot()[0], 7);

        let mut shared = SharedMemory::new(1, None, 2);
        shared.store32(0, 0, 7);
        assert_eq!(shared.snapshot(), plain.snapshot());
    }

    #[test]
    fn a_caller_over_a_shared_memory_reads_and_writes_the_same_way() {
        // The host code is generic over which memory it got, and this is the
        // implementation that makes the shared one usable from one.
        let mut memory = SharedMemory::new(1, None, 1);
        let mut caller = Caller {
            memory: &mut memory,
        };
        caller.write(32, b"shared\0");
        assert_eq!(caller.cstring(32), b"shared");
        assert_eq!(caller.bytes(32, 6), b"shared");
        caller.write_i32(64, -2);
        assert_eq!(caller.read_i32(64), -2);
    }

    #[test]
    #[should_panic(expected = "out of bounds memory access")]
    fn a_caller_over_a_shared_memory_still_traps_past_the_end() {
        let mut memory = SharedMemory::new(1, None, 4);
        let mut caller = Caller {
            memory: &mut memory,
        };
        // Inside the reservation, past what the module grew into.
        caller.write(PAGE_SIZE as i32, b"x");
    }

    #[test]
    #[should_panic(expected = "out of bounds memory access")]
    fn reading_a_shared_byte_past_the_end_traps() {
        let mut memory = SharedMemory::new(1, None, 4);
        let caller = Caller {
            memory: &mut memory,
        };
        let _ = caller.bytes(PAGE_SIZE as i32, 1);
    }

    #[test]
    #[should_panic(expected = "out of bounds memory access")]
    fn a_caller_over_a_plain_memory_traps_on_a_byte_past_the_end() {
        let mut memory = Memory::new(1, None);
        let caller = Caller {
            memory: &mut memory,
        };
        let _ = caller.bytes(PAGE_SIZE as i32, 1);
    }

    #[test]
    #[should_panic(expected = "out of bounds memory access")]
    fn writing_a_byte_past_the_end_of_a_plain_memory_traps() {
        let mut memory = Memory::new(1, None);
        let mut caller = Caller {
            memory: &mut memory,
        };
        caller.write(PAGE_SIZE as i32, b"x");
    }

    #[test]
    #[should_panic(expected = "no other instance shares this memory")]
    fn a_wait_nobody_can_answer_traps_rather_than_hanging() {
        // One handle means no other thread can ever notify, so this wait is
        // not slow — it is unsatisfiable, and saying so is the only honest
        // answer. It is also what lets a threaded module run on one thread.
        let memory = SharedMemory::new(1, None, 1);
        let _ = memory.atomic_wait(0, 0, 4, 0, -1);
    }

    #[test]
    fn a_shared_cmpxchg_at_eight_bytes_compares_the_whole_value() {
        let mut memory = SharedMemory::new(1, None, 1);
        memory.atomic_store(0, 0, 8, -1);
        assert_eq!(memory.atomic_cmpxchg(0, 0, 8, -1, 5), -1);
        assert_eq!(memory.atomic_load(0, 0, 8), 5);
    }

    #[test]
    fn a_shared_write_reports_nothing_when_nothing_is_watched() {
        let mut memory = SharedMemory::new(1, None, 1);
        memory.store32_at(1, 14, 0, 0, 1);
        assert!(memory.hits().is_empty());
    }

    #[test]
    fn a_shared_watchpoint_ignores_a_write_beside_the_range() {
        let mut memory = SharedMemory::new(1, None, 1);
        memory.watch(0x100, 4);
        memory.store32_at(1, 14, 0x0FC, 0, 1);
        memory.store32_at(2, 24, 0x104, 0, 1);
        assert!(memory.hits().is_empty());
    }

    #[test]
    fn nothing_is_watched_until_a_host_asks() {
        let mut memory = Memory::new(1, None);
        memory.store32_at(7, 74, 0, 0, 1);
        memory.fill_at(7, 74, 0, 0, 4);
        assert!(memory.hits().is_empty());
    }

    #[test]
    fn a_watched_write_records_who_did_it_and_what_kind_it_was() {
        let mut memory = Memory::new(1, None);
        memory.watch(0x100, 4);
        memory.store32_at(11, 114, 0x100, 0, 42);
        memory.store8_at(12, 124, 0x102, 0, 1);
        memory.store16_at(13, 134, 0x102, 0, 1);
        memory.store64_at(14, 144, 0x0FC, 0, 1);
        memory.store_f32_at(15, 154, 0x100, 0, 1.0);
        memory.store_f64_at(16, 164, 0x100, 0, 1.0);
        memory.fill_at(17, 174, 0x100, 0, 4);
        memory.copy_at(18, 184, 0x100, 0, 4);
        memory.init_at(19, 194, 0x100, b"abcd", 0, 4);
        assert_eq!(
            memory
                .hits()
                .iter()
                .map(|hit| (hit.site, hit.kind))
                .collect::<Vec<_>>(),
            vec![
                (11, WriteKind::Store),
                (12, WriteKind::Store),
                (13, WriteKind::Store),
                (14, WriteKind::Store),
                (15, WriteKind::Store),
                (16, WriteKind::Store),
                (17, WriteKind::Fill),
                (18, WriteKind::Copy),
                (19, WriteKind::Init),
            ]
        );
        assert_eq!(memory.hits()[0].address, 0x100);
        assert_eq!(memory.hits()[0].bytes, 4);
        // The i64 store starts below the range and runs into it, which is the
        // case a check comparing only the start address misses. A store that
        // *ends* exactly at 0x100 is not a hit, which is why this one is at
        // 0xFC rather than 0xF8.
        assert_eq!(memory.hits()[3].address, 0x0FC);
    }

    #[test]
    fn a_write_beside_the_range_is_not_a_hit() {
        let mut memory = Memory::new(1, None);
        memory.watch(0x100, 4);
        memory.store32_at(1, 14, 0x0FC, 0, 1);
        memory.store32_at(2, 24, 0x104, 0, 1);
        // The static offset counts towards the address, or an instrumented
        // `store offset=4` would be attributed to the wrong place.
        memory.store32_at(3, 34, 0x0FC, 4, 1);
        assert_eq!(
            memory.hits().iter().map(|hit| hit.site).collect::<Vec<_>>(),
            vec![3]
        );
        assert_eq!(memory.hits()[0].address, 0x100);
    }

    #[test]
    fn an_atomic_write_is_reported_and_says_it_was_atomic() {
        let mut memory = Memory::new(1, None);
        memory.watch(0x200, 4);
        memory.atomic_store_at(1, 14, 0x200, 0, 4, 7);
        assert_eq!(memory.atomic_rmw_at(2, 24, 0x200, 0, 4, Rmw::Add, 1), 7);
        // A comparison that fails wrote nothing, and reporting it would answer
        // "who wrote this address" with a function that did not.
        assert_eq!(memory.atomic_cmpxchg_at(3, 34, 0x200, 0, 4, 99, 1), 8);
        assert_eq!(memory.load32(0x200, 0), 8);
        assert_eq!(memory.hits().len(), 2);
        // One that succeeds is a write, and is reported.
        assert_eq!(memory.atomic_cmpxchg_at(4, 44, 0x200, 0, 4, 8, 12), 8);
        assert_eq!(memory.load32(0x200, 0), 12);
        assert_eq!(memory.hits().len(), 3);
        assert_eq!(memory.hits()[2].site, 4);
        assert!(
            memory
                .hits()
                .iter()
                .all(|hit| hit.kind == WriteKind::Atomic)
        );
    }

    #[test]
    #[should_panic(
        expected = "watchpoint: function #5 wrote 4 bytes at 0x100 (Store), from the instruction at file offset 54"
    )]
    fn stopping_on_a_hit_panics_where_the_write_happened() {
        let mut memory = Memory::new(1, None);
        memory.watch(0x100, 4);
        memory.watch.stop = true;
        memory.store32_at(5, 54, 0x100, 0, 1);
    }

    #[test]
    #[should_panic(expected = "out of bounds memory access")]
    fn a_store_that_traps_is_not_reported_as_a_write() {
        // Noted after the write, not before, so nothing that did not happen is
        // ever in the list.
        let mut memory = Memory::new(1, None);
        memory.watch(0, 4);
        memory.store32_at(1, 14, (PAGE_SIZE - 2) as i32, 0, 1);
    }

    #[test]
    fn memory_starts_zeroed_and_sized_in_pages() {
        let memory = Memory::new(2, None);
        assert_eq!(memory.size(), 2);
        assert_eq!(memory.data.len(), 2 * PAGE_SIZE);
        assert!(memory.data.iter().all(|&byte| byte == 0));
    }

    #[test]
    fn grow_reports_old_size_and_refuses_past_the_maximum() {
        let mut memory = Memory::new(1, Some(2));
        assert_eq!(memory.grow(1), 1);
        assert_eq!(memory.size(), 2);
        assert_eq!(memory.grow(1), -1);
        assert_eq!(memory.size(), 2);
        // Refusal must not have resized anything.
        assert_eq!(memory.data.len(), 2 * PAGE_SIZE);
    }

    #[test]
    fn grow_refuses_beyond_the_four_gigabyte_ceiling() {
        let mut memory = Memory::new(1, None);
        assert_eq!(memory.grow(65536), -1);
    }

    #[test]
    fn loads_and_stores_round_trip_at_every_width() {
        let mut memory = Memory::new(1, None);
        memory.store8(0, 0, 0xFF);
        memory.store16(0, 8, 0xBEEF);
        memory.store32(0, 16, 0x1234_5678);
        memory.store64(0, 24, 0x0102_0304_0506_0708);
        memory.store_f32(0, 40, 1.5);
        memory.store_f64(0, 48, -2.25);

        assert_eq!(memory.load8_u(0, 0), 255);
        assert_eq!(memory.load8_s(0, 0), -1);
        assert_eq!(memory.load16_u(0, 8), 0xBEEF);
        assert_eq!(memory.load16_s(0, 8), 0xBEEF_u16 as i16 as i32);
        assert_eq!(memory.load32(0, 16), 0x1234_5678);
        assert_eq!(memory.load64(0, 24), 0x0102_0304_0506_0708);
        assert_eq!(memory.load_f32(0, 40), 1.5);
        assert_eq!(memory.load_f64(0, 48), -2.25);
    }

    #[test]
    fn the_narrow_i64_loads_extend_by_their_sign() {
        let mut memory = Memory::new(1, None);
        memory.store8(0, 0, -1);
        memory.store16(0, 2, -2);
        memory.store32(0, 4, -3);
        assert_eq!(memory.load8_s_i64(0, 0), -1);
        assert_eq!(memory.load8_u_i64(0, 0), 255);
        assert_eq!(memory.load16_s_i64(0, 2), -2);
        assert_eq!(memory.load16_u_i64(0, 2), 65534);
        assert_eq!(memory.load32_s_i64(0, 4), -3);
        assert_eq!(memory.load32_u_i64(0, 4), 4294967293);
    }

    #[test]
    #[should_panic(expected = "out of bounds memory access")]
    fn the_address_is_unsigned_even_though_it_is_typed_i32() {
        // wasm addresses are u32, so -4 is 4294967292 — far past a one-page
        // memory, and a trap. Treating the i32 as signed would instead index
        // four bytes *before* the buffer, which is the bug this guards.
        Memory::new(1, None).load32(-4, 0);
    }

    #[test]
    #[should_panic(expected = "out of bounds memory access")]
    fn a_load_past_the_end_traps() {
        Memory::new(1, None).load32((PAGE_SIZE - 2) as i32, 0);
    }

    #[test]
    #[should_panic(expected = "out of bounds memory access")]
    fn the_offset_cannot_wrap_into_a_valid_address() {
        // addr + offset is computed in u64 precisely so this traps instead of
        // wrapping to a small, in-bounds address.
        Memory::new(1, None).load32(-1, u64::from(u32::MAX));
    }

    #[test]
    #[should_panic(expected = "out of bounds memory access")]
    fn a_store_past_the_end_traps() {
        Memory::new(1, None).store8(PAGE_SIZE as i32, 0, 1);
    }

    #[test]
    fn fill_and_copy_move_bytes_the_way_memmove_does() {
        let mut memory = Memory::new(1, None);
        memory.fill(0, 0xAB, 4);
        assert_eq!(memory.data[..5], [0xAB, 0xAB, 0xAB, 0xAB, 0x00]);
        // Overlapping forward copy: a naive byte loop would smear the source.
        memory.fill(0, 0, 16);
        memory.store32(0, 0, 0x0403_0201);
        memory.copy(2, 0, 4);
        assert_eq!(memory.data[..8], [1, 2, 1, 2, 3, 4, 0, 0]);
    }

    #[test]
    fn a_zero_length_fill_at_the_very_end_is_legal() {
        let mut memory = Memory::new(1, None);
        memory.fill(PAGE_SIZE as i32, 0, 0);
    }

    #[test]
    #[should_panic(expected = "out of bounds memory access")]
    fn a_zero_length_fill_past_the_end_is_not() {
        Memory::new(1, None).fill(PAGE_SIZE as i32 + 1, 0, 0);
    }

    #[test]
    #[should_panic(expected = "out of bounds memory access")]
    fn copy_checks_the_source_as_well_as_the_destination() {
        Memory::new(1, None).copy(0, (PAGE_SIZE - 2) as i32, 4);
    }

    #[test]
    fn init_copies_a_slice_of_a_segment() {
        let mut memory = Memory::new(1, None);
        memory.init(4, b"hello", 1, 3);
        assert_eq!(&memory.data[4..7], b"ell");
    }

    #[test]
    #[should_panic(expected = "out of bounds memory access")]
    fn init_checks_the_segment_bounds() {
        Memory::new(1, None).init(0, b"hi", 1, 4);
    }

    #[test]
    fn signed_division_traps_where_wasm_says_it_must() {
        assert_eq!(i32_div_s(7, 2), 3);
        assert_eq!(i32_div_s(-7, 2), -3);
        assert_eq!(i32_div_u(-1, 2), 0x7FFF_FFFF);
        assert_eq!(i64_div_s(-7, 2), -3);
        assert_eq!(i64_div_u(-1, 2), 0x7FFF_FFFF_FFFF_FFFF);
    }

    #[test]
    fn remainder_of_the_minimum_by_minus_one_is_zero_not_a_panic() {
        // The one case where Rust's `%` panics and wasm defines a result.
        assert_eq!(i32_rem_s(i32::MIN, -1), 0);
        assert_eq!(i64_rem_s(i64::MIN, -1), 0);
        assert_eq!(i32_rem_s(-7, 2), -1);
        assert_eq!(i32_rem_u(-1, 3), 0);
        assert_eq!(i64_rem_s(-7, 2), -1);
        assert_eq!(i64_rem_u(-1, 3), 0);
    }

    #[test]
    #[should_panic(expected = "integer divide by zero")]
    fn i32_division_by_zero_traps() {
        i32_div_s(1, 0);
    }

    #[test]
    #[should_panic(expected = "integer divide by zero")]
    fn i32_unsigned_division_by_zero_traps() {
        i32_div_u(1, 0);
    }

    #[test]
    #[should_panic(expected = "integer divide by zero")]
    fn i32_remainder_by_zero_traps() {
        i32_rem_s(1, 0);
    }

    #[test]
    #[should_panic(expected = "integer divide by zero")]
    fn i32_unsigned_remainder_by_zero_traps() {
        i32_rem_u(1, 0);
    }

    #[test]
    #[should_panic(expected = "integer divide by zero")]
    fn i64_division_by_zero_traps() {
        i64_div_s(1, 0);
    }

    #[test]
    #[should_panic(expected = "integer divide by zero")]
    fn i64_unsigned_division_by_zero_traps() {
        i64_div_u(1, 0);
    }

    #[test]
    #[should_panic(expected = "integer divide by zero")]
    fn i64_remainder_by_zero_traps() {
        i64_rem_s(1, 0);
    }

    #[test]
    #[should_panic(expected = "integer divide by zero")]
    fn i64_unsigned_remainder_by_zero_traps() {
        i64_rem_u(1, 0);
    }

    #[test]
    #[should_panic(expected = "integer overflow")]
    fn i32_minimum_divided_by_minus_one_traps() {
        i32_div_s(i32::MIN, -1);
    }

    #[test]
    #[should_panic(expected = "integer overflow")]
    fn i64_minimum_divided_by_minus_one_traps() {
        i64_div_s(i64::MIN, -1);
    }

    #[test]
    fn float_min_and_max_propagate_nan_and_keep_the_sign_of_zero() {
        assert!(f32_min(f32::NAN, 1.0).is_nan());
        assert!(f32_max(1.0, f32::NAN).is_nan());
        assert!(f64_min(f64::NAN, 1.0).is_nan());
        assert!(f64_max(1.0, f64::NAN).is_nan());
        assert!(f32_min(0.0, -0.0).is_sign_negative());
        assert!(f32_min(-0.0, 0.0).is_sign_negative());
        assert!(f32_max(0.0, -0.0).is_sign_positive());
        assert!(f32_max(-0.0, 0.0).is_sign_positive());
        assert!(f64_min(0.0, -0.0).is_sign_negative());
        assert!(f64_min(-0.0, 0.0).is_sign_negative());
        assert!(f64_max(0.0, -0.0).is_sign_positive());
        assert!(f64_max(-0.0, 0.0).is_sign_positive());
        assert_eq!(f32_min(1.0, 2.0), 1.0);
        assert_eq!(f32_max(1.0, 2.0), 2.0);
        assert_eq!(f64_min(1.0, 2.0), 1.0);
        assert_eq!(f64_max(1.0, 2.0), 2.0);
    }

    #[test]
    fn truncation_matches_wasm_at_the_boundaries() {
        assert_eq!(i32_trunc_f32_s(-1.9), -1);
        assert_eq!(i32_trunc_f32_u(4294967040.0), -256);
        assert_eq!(i32_trunc_f64_s(-2147483648.0), i32::MIN);
        assert_eq!(i32_trunc_f64_u(4294967295.0), -1);
        assert_eq!(i64_trunc_f32_s(-1.9), -1);
        assert_eq!(i64_trunc_f32_u(1.5), 1);
        assert_eq!(i64_trunc_f64_s(9223372036854774784.0), 9223372036854774784);
        assert_eq!(i64_trunc_f64_u(1.5), 1);
    }

    #[test]
    #[should_panic(expected = "invalid conversion to integer")]
    fn truncating_a_nan_traps_rather_than_saturating() {
        // `f32 as i32` would quietly give 0 here.
        i32_trunc_f32_s(f32::NAN);
    }

    #[test]
    #[should_panic(expected = "integer overflow")]
    fn truncating_out_of_range_traps() {
        i32_trunc_f32_s(3.0e9);
    }

    #[test]
    #[should_panic(expected = "invalid conversion to integer")]
    fn unsigned_f32_truncation_rejects_nan() {
        i32_trunc_f32_u(f32::NAN);
    }

    #[test]
    #[should_panic(expected = "integer overflow")]
    fn unsigned_f32_truncation_rejects_negatives() {
        i32_trunc_f32_u(-1.0);
    }

    #[test]
    #[should_panic(expected = "invalid conversion to integer")]
    fn signed_f64_truncation_rejects_nan() {
        i32_trunc_f64_s(f64::NAN);
    }

    #[test]
    #[should_panic(expected = "integer overflow")]
    fn signed_f64_truncation_rejects_out_of_range() {
        i32_trunc_f64_s(-2147483649.0);
    }

    #[test]
    #[should_panic(expected = "invalid conversion to integer")]
    fn unsigned_f64_truncation_rejects_nan() {
        i32_trunc_f64_u(f64::NAN);
    }

    #[test]
    #[should_panic(expected = "integer overflow")]
    fn unsigned_f64_truncation_rejects_out_of_range() {
        i32_trunc_f64_u(4294967296.0);
    }

    #[test]
    #[should_panic(expected = "invalid conversion to integer")]
    fn i64_from_f32_rejects_nan() {
        i64_trunc_f32_s(f32::NAN);
    }

    #[test]
    #[should_panic(expected = "integer overflow")]
    fn i64_from_f32_rejects_out_of_range() {
        i64_trunc_f32_s(1.0e19);
    }

    #[test]
    #[should_panic(expected = "invalid conversion to integer")]
    fn i64_unsigned_from_f32_rejects_nan() {
        i64_trunc_f32_u(f32::NAN);
    }

    #[test]
    #[should_panic(expected = "integer overflow")]
    fn i64_unsigned_from_f32_rejects_negatives() {
        i64_trunc_f32_u(-1.0);
    }

    #[test]
    #[should_panic(expected = "invalid conversion to integer")]
    fn i64_from_f64_rejects_nan() {
        i64_trunc_f64_s(f64::NAN);
    }

    #[test]
    #[should_panic(expected = "integer overflow")]
    fn i64_from_f64_rejects_out_of_range() {
        i64_trunc_f64_s(1.0e19);
    }

    #[test]
    #[should_panic(expected = "invalid conversion to integer")]
    fn i64_unsigned_from_f64_rejects_nan() {
        i64_trunc_f64_u(f64::NAN);
    }

    #[test]
    #[should_panic(expected = "integer overflow")]
    fn i64_unsigned_from_f64_rejects_out_of_range() {
        i64_trunc_f64_u(-1.0);
    }

    #[test]
    fn nearest_rounds_halves_to_even() {
        // 2.5 rounds to 2, not 3: `f32::round` would give 3.
        assert_eq!(f32_nearest(2.5), 2.0);
        assert_eq!(f32_nearest(3.5), 4.0);
        assert_eq!(f32_nearest(-2.5), -2.0);
        assert_eq!(f64_nearest(2.5), 2.0);
        assert_eq!(f64_nearest(-3.5), -4.0);
    }

    #[test]
    fn atomic_loads_and_stores_move_the_same_bytes_as_plain_ones() {
        let mut memory = Memory::new(1, None);
        memory.atomic_store(0, 0, 4, 0x1234_5678);
        assert_eq!(memory.load32(0, 0), 0x1234_5678);
        assert_eq!(memory.atomic_load(0, 0, 4), 0x1234_5678);
        // Narrow accesses are zero-extended, never sign-extended: there is no
        // signed atomic load in wasm.
        memory.atomic_store(8, 0, 1, -1);
        assert_eq!(memory.atomic_load(8, 0, 1), 255);
        memory.atomic_store(16, 0, 2, -1);
        assert_eq!(memory.atomic_load(16, 0, 2), 65535);
        memory.atomic_store(24, 0, 8, -1);
        assert_eq!(memory.atomic_load(24, 0, 8), -1);
    }

    #[test]
    #[should_panic(expected = "unaligned atomic operation")]
    fn an_unaligned_atomic_access_traps_where_a_plain_one_would_not() {
        // The one thing an atomic does that its plain counterpart does not.
        let mut memory = Memory::new(1, None);
        memory.store32(1, 0, 7); // fine
        memory.atomic_load(1, 0, 4); // not
    }

    #[test]
    #[should_panic(expected = "unaligned atomic operation")]
    fn the_static_offset_counts_towards_alignment() {
        Memory::new(1, None).atomic_load(0, 2, 4);
    }

    #[test]
    fn a_single_byte_atomic_is_aligned_anywhere() {
        let mut memory = Memory::new(1, None);
        memory.atomic_store(7, 0, 1, 3);
        assert_eq!(memory.atomic_load(7, 0, 1), 3);
    }

    #[test]
    #[should_panic(expected = "out of bounds memory access")]
    fn an_atomic_past_the_end_traps_like_any_other_access() {
        Memory::new(1, None).atomic_load(PAGE_SIZE as i32, 0, 4);
    }

    #[test]
    fn read_modify_write_returns_the_old_value_and_stores_the_new_one() {
        let mut memory = Memory::new(1, None);
        memory.atomic_store(0, 0, 4, 10);
        assert_eq!(memory.atomic_rmw(0, 0, 4, Rmw::Add, 5), 10);
        assert_eq!(memory.atomic_load(0, 0, 4), 15);
        assert_eq!(memory.atomic_rmw(0, 0, 4, Rmw::Sub, 20), 15);
        // The result is read back zero-extended, as every atomic load is; the
        // generated code casts it to i32, where it is -5.
        assert_eq!(memory.atomic_load(0, 0, 4), 0xFFFF_FFFB);
        assert_eq!(memory.atomic_load(0, 0, 4) as i32, -5);
        assert_eq!(memory.atomic_rmw(0, 0, 4, Rmw::And, 0xFF), 0xFFFF_FFFB);
        assert_eq!(memory.atomic_load(0, 0, 4), 0xFB);
        assert_eq!(memory.atomic_rmw(0, 0, 4, Rmw::Or, 0x100), 0xFB);
        assert_eq!(memory.atomic_load(0, 0, 4), 0x1FB);
        assert_eq!(memory.atomic_rmw(0, 0, 4, Rmw::Xor, 0xFF), 0x1FB);
        assert_eq!(memory.atomic_load(0, 0, 4), 0x104);
        assert_eq!(memory.atomic_rmw(0, 0, 4, Rmw::Xchg, 42), 0x104);
        assert_eq!(memory.atomic_load(0, 0, 4), 42);
    }

    #[test]
    fn a_narrow_read_modify_write_wraps_at_its_own_width() {
        // `i32.atomic.rmw8.add_u` on 250 + 10 is 4, not 260: the arithmetic
        // happens at the access width, not at the value's.
        let mut memory = Memory::new(1, None);
        memory.atomic_store(0, 0, 1, 250);
        assert_eq!(memory.atomic_rmw(0, 0, 1, Rmw::Add, 10), 250);
        assert_eq!(memory.atomic_load(0, 0, 1), 4);
        // And the neighbouring byte is untouched.
        assert_eq!(memory.data[1], 0);
    }

    #[test]
    fn compare_and_exchange_swaps_only_on_a_match() {
        let mut memory = Memory::new(1, None);
        memory.atomic_store(0, 0, 4, 7);
        assert_eq!(
            memory.atomic_cmpxchg(0, 0, 4, 7, 9),
            7,
            "returns the old value"
        );
        assert_eq!(memory.atomic_load(0, 0, 4), 9, "and swapped");
        assert_eq!(
            memory.atomic_cmpxchg(0, 0, 4, 7, 11),
            9,
            "still the old value"
        );
        assert_eq!(memory.atomic_load(0, 0, 4), 9, "and did not swap");
    }

    #[test]
    fn a_full_width_compare_and_exchange_compares_every_bit() {
        // Eight bytes: there is no mask, because there is nothing to mask off.
        let mut memory = Memory::new(1, None);
        memory.atomic_store(0, 0, 8, -1);
        assert_eq!(
            memory.atomic_cmpxchg(0, 0, 8, 0, 5),
            -1,
            "no match, no swap"
        );
        assert_eq!(memory.atomic_load(0, 0, 8), -1);
        assert_eq!(memory.atomic_cmpxchg(0, 0, 8, -1, 5), -1);
        assert_eq!(memory.atomic_load(0, 0, 8), 5);
    }

    #[test]
    fn a_narrow_compare_and_exchange_compares_at_its_own_width() {
        // The expected value arrives at the operand's width; only the low byte
        // can possibly match a one-byte access.
        let mut memory = Memory::new(1, None);
        memory.atomic_store(0, 0, 1, 0x42);
        assert_eq!(memory.atomic_cmpxchg(0, 0, 1, 0xFF42, 0x99), 0x42);
        assert_eq!(
            memory.atomic_load(0, 0, 1),
            0x99,
            "the high bits are not part of it"
        );
    }

    #[test]
    fn notify_wakes_nobody_because_nobody_is_there() {
        assert_eq!(Memory::new(1, None).atomic_notify(0, 0), 0);
    }

    #[test]
    fn waiting_on_a_value_that_already_changed_returns_not_equal() {
        let mut memory = Memory::new(1, None);
        memory.atomic_store(0, 0, 4, 5);
        // The uncontended case, and the common one: no thread is needed to
        // answer it.
        assert_eq!(memory.atomic_wait(0, 0, 4, 99, -1), 1);
    }

    #[test]
    fn waiting_with_a_zero_timeout_times_out_immediately() {
        let mut memory = Memory::new(1, None);
        memory.atomic_store(0, 0, 4, 5);
        assert_eq!(memory.atomic_wait(0, 0, 4, 5, 0), 2);
    }

    #[test]
    #[should_panic(expected = "would block forever")]
    fn waiting_for_a_notification_that_can_never_come_traps() {
        // Returning "timed out" here would be inventing an event.
        let mut memory = Memory::new(1, None);
        memory.atomic_store(0, 0, 4, 5);
        memory.atomic_wait(0, 0, 4, 5, 1000);
    }

    #[test]
    #[should_panic(expected = "unaligned atomic operation")]
    fn notify_checks_alignment_too() {
        Memory::new(1, None).atomic_notify(2, 0);
    }

    #[test]
    fn a_guest_exception_carries_what_the_guest_threw() {
        let outcome = std::panic::catch_unwind(|| throw(0x1234, 0x5678));
        let payload = outcome.expect_err("a throw is a panic");
        let exception = payload
            .downcast_ref::<GuestException>()
            .expect("and it carries the exception");
        assert_eq!(exception.exception, 0x1234);
        assert_eq!(exception.info, 0x5678);
    }

    #[test]
    fn a_trap_is_not_a_guest_exception() {
        // What the generated trampolines rely on to tell one from the other:
        // a trap panics with a string, not with this type.
        let outcome = std::panic::catch_unwind(|| trap("out of bounds memory access"));
        let payload = outcome.expect_err("a trap is a panic too");
        assert!(
            payload.downcast_ref::<GuestException>().is_none(),
            "a trampoline would swallow this if it were"
        );
        assert!(payload.downcast_ref::<String>().is_some());
    }

    #[test]
    #[should_panic(expected = "wasm trap: nothing here")]
    fn trap_names_itself_as_a_trap() {
        trap("nothing here");
    }
}
