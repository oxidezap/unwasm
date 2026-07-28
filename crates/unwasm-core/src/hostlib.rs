//! The mechanical half of a host, for modules an Emscripten toolchain built.
//!
//! Like [`crate::rt`] this file is compiled twice: once as part of this crate,
//! where its tests run, and once as text, embedded in what `unwasm host` writes.
//! It has no dependencies for the same reason the runtime has none.
//!
//! What is here is the part that is the *same* for every module: WASI over an
//! in-memory filesystem, the C++ runtime's exception entry points, Emscripten's
//! own runtime, and embind's registration calls, which are recorded rather than
//! acted on. What is not here is the application's own imports — nothing but
//! the application can say what `on_call_event_js_sync` should do, and a stub
//! that returns zero would be a lie the module cannot detect.
//!
//! Two decisions worth knowing before reading:
//!
//! - **The filesystem is a `BTreeMap<String, Vec<u8>>` and nothing escapes to
//!   the real one.** A decompiled module is something you are running to find
//!   out what it does; letting it open `/etc/passwd` because that is what the
//!   real syscall would do is not faithfulness, it is a hole. Paths that are
//!   not in the map do not exist, and that is the honest answer.
//! - **Randomness and the clock are supplied, not taken.** `random_get` reads
//!   from a seeded generator and the clock from a counter the caller sets, so
//!   two runs of the same module produce the same bytes. A host that wants the
//!   real ones assigns them; a *test* that wants a repeatable run gets it by
//!   default.

use super::rt;

/// WASI's `errno`, for the handful this answers with.
///
/// The numbering is preview1's, which is not errno.h's: `ENOENT` is 44 here and
/// 2 there, and a module built against WASI compares against these.
pub mod errno {
    /// No error.
    pub const SUCCESS: i32 = 0;
    /// Bad file descriptor.
    pub const BADF: i32 = 8;
    /// File exists.
    pub const EXIST: i32 = 20;
    /// Invalid argument.
    pub const INVAL: i32 = 28;
    /// Is a directory.
    pub const ISDIR: i32 = 31;
    /// No such file or directory.
    pub const NOENT: i32 = 44;
    /// Function not implemented.
    pub const NOSYS: i32 = 52;
    /// Not a directory.
    pub const NOTDIR: i32 = 54;
    /// Not a tty.
    pub const NOTTY: i32 = 59;
    /// Invalid seek.
    pub const SPIPE: i32 = 70;
}

/// Where a `fd_seek` measures its offset from.
const WHENCE_SET: i32 = 0;
const WHENCE_CUR: i32 = 1;
const WHENCE_END: i32 = 2;

/// A guest exit, as `proc_exit` and `exit` raise it.
///
/// A panic rather than a return, because there is nothing to return to: the
/// module has said it is finished. A driver that wants to keep going catches it
/// with [`std::panic::catch_unwind`], the same way a trampoline catches a guest
/// exception.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Exit {
    /// The status the guest asked for.
    pub code: i32,
}

/// Ends the run with `code`.
///
/// # Panics
///
/// Always. That is what an exit is here.
pub fn exit(code: i32) -> ! {
    std::panic::panic_any(Exit { code });
}

/// An open file descriptor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Open {
    /// The path it was opened under, and the key into [`Wasi::files`].
    pub path: String,
    /// Where the next read or write lands.
    pub position: u64,
    /// Whether writes go to the end regardless of the position.
    pub append: bool,
}

/// A stream that is not a file: the three the C runtime assumes exist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Standard {
    /// `stdin`, read from [`Wasi::stdin`].
    In,
    /// `stdout`, collected into [`Wasi::stdout`].
    Out,
    /// `stderr`, collected into [`Wasi::stderr`].
    Error,
}

/// What a descriptor number refers to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Descriptor {
    /// One of the three standard streams.
    Standard(Standard),
    /// A file in [`Wasi::files`].
    File(Open),
    /// A directory, which `openat` resolves paths against.
    Directory(String),
}

/// A host for the WASI imports, over a filesystem that lives in this struct.
///
/// Descriptors 0, 1 and 2 are open before the module starts, because a C
/// runtime assumes they are; 3 is the preopened root directory, which is what
/// `__syscall_openat` resolves a relative path against.
#[derive(Debug, Clone, PartialEq)]
pub struct Wasi {
    /// The filesystem: a path to its contents. Nothing outside it exists.
    pub files: std::collections::BTreeMap<String, Vec<u8>>,
    /// Open descriptors by number.
    pub open: std::collections::BTreeMap<i32, Descriptor>,
    /// What the next `openat` will be given.
    pub next_fd: i32,
    /// What the guest has written to `stdout`.
    pub stdout: Vec<u8>,
    /// What the guest has written to `stderr`.
    pub stderr: Vec<u8>,
    /// What a read of `stdin` will produce, and how far it has got.
    pub stdin: Vec<u8>,
    /// The read position in `stdin`.
    pub stdin_position: usize,
    /// The environment, as `NAME=value`.
    pub environment: Vec<String>,
    /// `argv`.
    pub arguments: Vec<String>,
    /// The seed the next `random_get` continues from.
    pub random: u64,
    /// Milliseconds since the epoch, as the clock imports report it. It does
    /// not advance by itself: a host that wants a moving clock moves it.
    pub now_milliseconds: f64,
}

impl Default for Wasi {
    fn default() -> Self {
        let mut open = std::collections::BTreeMap::new();
        open.insert(0, Descriptor::Standard(Standard::In));
        open.insert(1, Descriptor::Standard(Standard::Out));
        open.insert(2, Descriptor::Standard(Standard::Error));
        open.insert(3, Descriptor::Directory("/".to_string()));
        Self {
            files: std::collections::BTreeMap::new(),
            open,
            next_fd: 4,
            stdout: Vec::new(),
            stderr: Vec::new(),
            stdin: Vec::new(),
            stdin_position: 0,
            environment: Vec::new(),
            arguments: vec!["module.wasm".to_string()],
            // Chosen so the first `random_get` is not zero; any non-zero seed
            // does, since the generator is xorshift and zero is its fixed point.
            random: 0x2545_F491_4F6C_DD1D,
            now_milliseconds: 0.0,
        }
    }
}

impl Wasi {
    /// Puts a file in the filesystem, replacing whatever was there.
    pub fn add_file(&mut self, path: &str, contents: &[u8]) {
        self.files.insert(path.to_string(), contents.to_vec());
    }

    /// What the guest wrote to `stdout`, as text, with anything that is not
    /// UTF-8 replaced rather than refused.
    #[must_use]
    pub fn stdout_text(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }

    /// What the guest wrote to `stderr`.
    #[must_use]
    pub fn stderr_text(&self) -> String {
        String::from_utf8_lossy(&self.stderr).into_owned()
    }

    /// The next pseudo-random `u64`. Deterministic on purpose: see the module
    /// comment.
    fn next_random(&mut self) -> u64 {
        // xorshift64*, which is four lines and has no dependency.
        let mut state = self.random;
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        self.random = state;
        state.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Reads an iovec array: `count` pairs of (address, length).
    fn iovecs(caller: &rt::Caller<'_>, iovs: i32, count: i32) -> Vec<(i32, i32)> {
        (0..count)
            .map(|at| {
                let entry = iovs.wrapping_add(at.wrapping_mul(8));
                (
                    caller.memory.load32(entry, 0),
                    caller.memory.load32(entry, 4),
                )
            })
            .collect()
    }

    /// `fd_write`: gathers the iovecs and appends them where the descriptor
    /// points.
    pub fn fd_write(
        &mut self,
        caller: &mut rt::Caller<'_>,
        fd: i32,
        iovs: i32,
        count: i32,
        written: i32,
    ) -> i32 {
        let mut bytes = Vec::new();
        for (address, length) in Self::iovecs(caller, iovs, count) {
            bytes.extend_from_slice(caller.bytes(address, length));
        }
        let total = bytes.len() as i64;
        match self.open.get_mut(&fd) {
            Some(Descriptor::Standard(Standard::Out)) => self.stdout.extend_from_slice(&bytes),
            Some(Descriptor::Standard(Standard::Error)) => self.stderr.extend_from_slice(&bytes),
            Some(Descriptor::Standard(Standard::In)) | Some(Descriptor::Directory(_)) => {
                return errno::INVAL;
            }
            Some(Descriptor::File(open)) => {
                let Some(contents) = self.files.get_mut(&open.path) else {
                    return errno::NOENT;
                };
                let at = if open.append {
                    contents.len()
                } else {
                    open.position as usize
                };
                if at > contents.len() {
                    contents.resize(at, 0);
                }
                let end = at + bytes.len();
                if end > contents.len() {
                    contents.resize(end, 0);
                }
                contents[at..end].copy_from_slice(&bytes);
                open.position = end as u64;
            }
            None => return errno::BADF,
        }
        caller.memory.store32(written, 0, total);
        errno::SUCCESS
    }

    /// `fd_read`: scatters into the iovecs, stopping at the end of the source.
    pub fn fd_read(
        &mut self,
        caller: &mut rt::Caller<'_>,
        fd: i32,
        iovs: i32,
        count: i32,
        read: i32,
    ) -> i32 {
        let vectors = Self::iovecs(caller, iovs, count);
        let wanted: usize = vectors
            .iter()
            .map(|(_, length)| *length as u32 as usize)
            .sum();

        let (source, start) = match self.open.get(&fd) {
            Some(Descriptor::Standard(Standard::In)) => (&self.stdin, self.stdin_position),
            Some(Descriptor::File(open)) => {
                let Some(contents) = self.files.get(&open.path) else {
                    return errno::NOENT;
                };
                (contents, open.position as usize)
            }
            Some(_) => return errno::INVAL,
            None => return errno::BADF,
        };

        let start = start.min(source.len());
        let taken = source[start..].len().min(wanted);
        let bytes = source[start..start + taken].to_vec();

        let mut at = 0usize;
        for (address, length) in vectors {
            let length = length as u32 as usize;
            let piece = bytes[at..(at + length).min(bytes.len())].to_vec();
            if piece.is_empty() {
                break;
            }
            caller.write(address, &piece);
            at += piece.len();
        }

        // Only the two kinds that got this far can be here: everything else
        // returned above.
        if let Some(Descriptor::File(open)) = self.open.get_mut(&fd) {
            open.position = (start + taken) as u64;
        } else {
            self.stdin_position = start + taken;
        }
        caller.memory.store32(read, 0, taken as i64);
        errno::SUCCESS
    }

    /// `fd_pread`: a read at an explicit offset, which does not move the
    /// position.
    pub fn fd_pread(
        &mut self,
        caller: &mut rt::Caller<'_>,
        fd: i32,
        iovs: i32,
        count: i32,
        offset: i64,
        read: i32,
    ) -> i32 {
        let Some(Descriptor::File(open)) = self.open.get(&fd) else {
            return match self.open.get(&fd) {
                Some(_) => errno::SPIPE,
                None => errno::BADF,
            };
        };
        let mut moved = open.clone();
        moved.position = offset as u64;
        let saved = std::mem::replace(
            self.open.get_mut(&fd).expect("just matched"),
            Descriptor::File(moved),
        );
        let result = self.fd_read(caller, fd, iovs, count, read);
        self.open.insert(fd, saved);
        result
    }

    /// `fd_seek`. Reports where it landed through `out`.
    pub fn fd_seek(
        &mut self,
        caller: &mut rt::Caller<'_>,
        fd: i32,
        offset: i64,
        whence: i32,
        out: i32,
    ) -> i32 {
        let (current, end) = match self.open.get(&fd) {
            Some(Descriptor::File(open)) => match self.files.get(&open.path) {
                Some(contents) => (open.position as i64, contents.len() as i64),
                None => return errno::NOENT,
            },
            // Seeking a pipe is not an error a caller can recover from by
            // trying a different offset, so it gets its own errno.
            Some(_) => return errno::SPIPE,
            None => return errno::BADF,
        };
        let base = match whence {
            WHENCE_SET => 0,
            WHENCE_CUR => current,
            WHENCE_END => end,
            _ => return errno::INVAL,
        };
        let Some(position) = base.checked_add(offset) else {
            return errno::INVAL;
        };
        if position < 0 {
            return errno::INVAL;
        }
        if let Some(Descriptor::File(open)) = self.open.get_mut(&fd) {
            open.position = position as u64;
        }
        caller.memory.store64(out, 0, position);
        errno::SUCCESS
    }

    /// `fd_close`. Closing a standard stream is allowed and keeps what was
    /// written: the run is over before anyone reads it.
    pub fn fd_close(&mut self, fd: i32) -> i32 {
        match self.open.remove(&fd) {
            Some(_) => errno::SUCCESS,
            None => errno::BADF,
        }
    }

    /// `environ_sizes_get`: how many entries, and how many bytes they need.
    pub fn environ_sizes_get(&self, caller: &mut rt::Caller<'_>, count: i32, size: i32) -> i32 {
        let bytes: usize = self
            .environment
            .iter()
            .map(|entry| entry.len() + 1)
            .sum::<usize>();
        caller
            .memory
            .store32(count, 0, self.environment.len() as i64);
        caller.memory.store32(size, 0, bytes as i64);
        errno::SUCCESS
    }

    /// `environ_get`: the pointers, then the strings they point at.
    pub fn environ_get(&self, caller: &mut rt::Caller<'_>, pointers: i32, buffer: i32) -> i32 {
        let mut at = buffer;
        for (index, entry) in self.environment.iter().enumerate() {
            caller
                .memory
                .store32(pointers + (index as i32) * 4, 0, i64::from(at));
            let mut bytes = entry.clone().into_bytes();
            bytes.push(0);
            caller.write(at, &bytes);
            at += bytes.len() as i32;
        }
        errno::SUCCESS
    }

    /// `args_sizes_get`.
    pub fn args_sizes_get(&self, caller: &mut rt::Caller<'_>, count: i32, size: i32) -> i32 {
        let bytes: usize = self
            .arguments
            .iter()
            .map(|entry| entry.len() + 1)
            .sum::<usize>();
        caller.memory.store32(count, 0, self.arguments.len() as i64);
        caller.memory.store32(size, 0, bytes as i64);
        errno::SUCCESS
    }

    /// `args_get`.
    pub fn args_get(&self, caller: &mut rt::Caller<'_>, pointers: i32, buffer: i32) -> i32 {
        let mut at = buffer;
        for (index, entry) in self.arguments.iter().enumerate() {
            caller
                .memory
                .store32(pointers + (index as i32) * 4, 0, i64::from(at));
            let mut bytes = entry.clone().into_bytes();
            bytes.push(0);
            caller.write(at, &bytes);
            at += bytes.len() as i32;
        }
        errno::SUCCESS
    }

    /// `random_get`, from the seeded generator described at the top.
    pub fn random_get(&mut self, caller: &mut rt::Caller<'_>, buffer: i32, length: i32) -> i32 {
        let length = length as u32 as usize;
        let mut bytes = Vec::with_capacity(length);
        while bytes.len() < length {
            bytes.extend_from_slice(&self.next_random().to_le_bytes());
        }
        bytes.truncate(length);
        caller.write(buffer, &bytes);
        errno::SUCCESS
    }

    /// `clock_time_get`, in nanoseconds, from [`Self::now_milliseconds`].
    pub fn clock_time_get(&self, caller: &mut rt::Caller<'_>, out: i32) -> i32 {
        #[allow(clippy::cast_possible_truncation)]
        let nanoseconds = (self.now_milliseconds * 1_000_000.0) as i64;
        caller.memory.store64(out, 0, nanoseconds);
        errno::SUCCESS
    }

    /// `__syscall_openat`. Only the path matters here: the directory
    /// descriptor is checked for existence and otherwise ignored, since every
    /// path in this filesystem is absolute.
    ///
    /// Returns a descriptor, or the negative errno a Linux syscall returns —
    /// which is what Emscripten's `__syscall_*` shims expect, and is not what
    /// WASI returns.
    pub fn openat(&mut self, path: &str, create: bool, append: bool) -> i32 {
        if !self.files.contains_key(path) {
            if !create {
                return -errno::NOENT;
            }
            self.files.insert(path.to_string(), Vec::new());
        }
        let fd = self.next_fd;
        self.next_fd += 1;
        self.open.insert(
            fd,
            Descriptor::File(Open {
                path: path.to_string(),
                position: 0,
                append,
            }),
        );
        fd
    }

    /// The size of what a descriptor refers to, for `fstat`.
    #[must_use]
    pub fn size_of(&self, fd: i32) -> Option<usize> {
        match self.open.get(&fd)? {
            Descriptor::File(open) => self.files.get(&open.path).map(Vec::len),
            _ => Some(0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A memory with the iovec array at 0 and room to write into.
    fn memory() -> rt::Memory {
        rt::Memory::new(1, None)
    }

    /// Lays out `count` iovecs at address 0 pointing at consecutive buffers.
    fn iovecs(memory: &mut rt::Memory, pieces: &[&[u8]]) -> i32 {
        let mut at = 1024;
        for (index, piece) in pieces.iter().enumerate() {
            memory.store32(index as i32 * 8, 0, i64::from(at));
            memory.store32(index as i32 * 8 + 4, 0, piece.len() as i64);
            for (offset, byte) in piece.iter().enumerate() {
                memory.store8(at + offset as i32, 0, i64::from(*byte));
            }
            at += piece.len() as i32;
        }
        0
    }

    #[test]
    fn the_three_standard_streams_are_open_before_the_module_starts() {
        let wasi = Wasi::default();
        assert!(matches!(wasi.open[&0], Descriptor::Standard(Standard::In)));
        assert!(matches!(wasi.open[&1], Descriptor::Standard(Standard::Out)));
        assert!(matches!(
            wasi.open[&2],
            Descriptor::Standard(Standard::Error)
        ));
        assert!(matches!(wasi.open[&3], Descriptor::Directory(_)));
    }

    #[test]
    fn writing_to_stdout_and_stderr_collects_what_was_written() {
        let mut wasi = Wasi::default();
        let mut memory = memory();
        let iovs = iovecs(&mut memory, &[b"hel", b"lo\n"]);
        let mut caller = rt::Caller {
            memory: &mut memory,
        };
        assert_eq!(wasi.fd_write(&mut caller, 1, iovs, 2, 512), errno::SUCCESS);
        assert_eq!(wasi.fd_write(&mut caller, 2, iovs, 1, 512), errno::SUCCESS);
        assert_eq!(wasi.stdout_text(), "hello\n");
        assert_eq!(wasi.stderr_text(), "hel");
        // The count goes back where the guest asked for it.
        assert_eq!(caller.memory.load32(512, 0), 3);
    }

    #[test]
    fn writing_to_a_descriptor_that_is_not_open_is_ebadf() {
        let mut wasi = Wasi::default();
        let mut memory = memory();
        let iovs = iovecs(&mut memory, &[b"x"]);
        let mut caller = rt::Caller {
            memory: &mut memory,
        };
        assert_eq!(wasi.fd_write(&mut caller, 99, iovs, 1, 512), errno::BADF);
        // Nothing was written, so the count must not have been touched either.
        assert_eq!(caller.memory.load32(512, 0), 0);
        assert_eq!(wasi.fd_write(&mut caller, 0, iovs, 1, 512), errno::INVAL);
        assert_eq!(wasi.fd_write(&mut caller, 3, iovs, 1, 512), errno::INVAL);
    }

    #[test]
    fn a_file_round_trips_through_write_seek_and_read() {
        let mut wasi = Wasi::default();
        let fd = wasi.openat("/tmp/notes", true, false);
        assert_eq!(fd, 4, "the first descriptor after the preopened ones");

        let mut memory = memory();
        let iovs = iovecs(&mut memory, &[b"abcdef"]);
        let mut caller = rt::Caller {
            memory: &mut memory,
        };
        assert_eq!(wasi.fd_write(&mut caller, fd, iovs, 1, 512), errno::SUCCESS);
        assert_eq!(wasi.files["/tmp/notes"], b"abcdef");

        assert_eq!(
            wasi.fd_seek(&mut caller, fd, 2, WHENCE_SET, 520),
            errno::SUCCESS
        );
        assert_eq!(caller.memory.load64(520, 0), 2);

        // One iovec of three bytes, to read back what was just written.
        caller.memory.store32(0, 0, 2048);
        caller.memory.store32(4, 0, 3);
        assert_eq!(wasi.fd_read(&mut caller, fd, 0, 1, 528), errno::SUCCESS);
        assert_eq!(caller.memory.load32(528, 0), 3);
        assert_eq!(caller.bytes(2048, 3), b"cde");
    }

    #[test]
    fn seeking_measures_from_where_it_was_told_to() {
        let mut wasi = Wasi::default();
        wasi.add_file("/f", b"0123456789");
        let fd = wasi.openat("/f", false, false);
        let mut memory = memory();
        let mut caller = rt::Caller {
            memory: &mut memory,
        };
        assert_eq!(
            wasi.fd_seek(&mut caller, fd, 4, WHENCE_SET, 512),
            errno::SUCCESS
        );
        assert_eq!(caller.memory.load64(512, 0), 4);
        assert_eq!(
            wasi.fd_seek(&mut caller, fd, 3, WHENCE_CUR, 512),
            errno::SUCCESS
        );
        assert_eq!(caller.memory.load64(512, 0), 7);
        assert_eq!(
            wasi.fd_seek(&mut caller, fd, -2, WHENCE_END, 512),
            errno::SUCCESS
        );
        assert_eq!(caller.memory.load64(512, 0), 8);
        // Before the start, an unknown whence, and an offset that overflows.
        assert_eq!(
            wasi.fd_seek(&mut caller, fd, -1, WHENCE_SET, 512),
            errno::INVAL
        );
        assert_eq!(wasi.fd_seek(&mut caller, fd, 0, 9, 512), errno::INVAL);
        assert_eq!(
            wasi.fd_seek(&mut caller, fd, i64::MAX, WHENCE_END, 512),
            errno::INVAL
        );
        // A stream has no position, and a closed descriptor has no stream.
        assert_eq!(
            wasi.fd_seek(&mut caller, 1, 0, WHENCE_SET, 512),
            errno::SPIPE
        );
        assert_eq!(
            wasi.fd_seek(&mut caller, 99, 0, WHENCE_SET, 512),
            errno::BADF
        );
    }

    #[test]
    fn seeking_or_reading_a_file_that_was_deleted_underneath_is_enoent() {
        let mut wasi = Wasi::default();
        wasi.add_file("/f", b"xyz");
        let fd = wasi.openat("/f", false, false);
        wasi.files.remove("/f");
        let mut memory = memory();
        let mut caller = rt::Caller {
            memory: &mut memory,
        };
        assert_eq!(
            wasi.fd_seek(&mut caller, fd, 0, WHENCE_SET, 512),
            errno::NOENT
        );
        caller.memory.store32(0, 0, 2048);
        caller.memory.store32(4, 0, 1);
        assert_eq!(wasi.fd_read(&mut caller, fd, 0, 1, 512), errno::NOENT);
        let iovs = iovecs(caller.memory, &[b"x"]);
        assert_eq!(wasi.fd_write(&mut caller, fd, iovs, 1, 512), errno::NOENT);
    }

    #[test]
    fn appending_ignores_the_position() {
        let mut wasi = Wasi::default();
        wasi.add_file("/log", b"first ");
        let fd = wasi.openat("/log", false, true);
        let mut memory = memory();
        let iovs = iovecs(&mut memory, &[b"second"]);
        let mut caller = rt::Caller {
            memory: &mut memory,
        };
        assert_eq!(wasi.fd_write(&mut caller, fd, iovs, 1, 512), errno::SUCCESS);
        assert_eq!(wasi.files["/log"], b"first second");
    }

    #[test]
    fn writing_past_the_end_of_a_file_zero_fills_the_gap() {
        let mut wasi = Wasi::default();
        wasi.add_file("/sparse", b"ab");
        let fd = wasi.openat("/sparse", false, false);
        let mut memory = memory();
        let iovs = iovecs(&mut memory, &[b"z"]);
        let mut caller = rt::Caller {
            memory: &mut memory,
        };
        assert_eq!(
            wasi.fd_seek(&mut caller, fd, 5, WHENCE_SET, 512),
            errno::SUCCESS
        );
        assert_eq!(wasi.fd_write(&mut caller, fd, iovs, 1, 512), errno::SUCCESS);
        assert_eq!(wasi.files["/sparse"], b"ab\0\0\0z");
    }

    #[test]
    fn reading_stdin_stops_where_it_ran_out() {
        let mut wasi = Wasi {
            stdin: b"line".to_vec(),
            ..Wasi::default()
        };
        let mut memory = memory();
        let mut caller = rt::Caller {
            memory: &mut memory,
        };
        // Two iovecs of three bytes each, and only four bytes to give.
        caller.memory.store32(0, 0, 2048);
        caller.memory.store32(4, 0, 3);
        caller.memory.store32(8, 0, 3072);
        caller.memory.store32(12, 0, 3);
        assert_eq!(wasi.fd_read(&mut caller, 0, 0, 2, 512), errno::SUCCESS);
        assert_eq!(caller.memory.load32(512, 0), 4);
        assert_eq!(caller.bytes(2048, 3), b"lin");
        assert_eq!(caller.bytes(3072, 1), b"e");
        // A second read is at the end and returns nothing, not an error.
        assert_eq!(wasi.fd_read(&mut caller, 0, 0, 2, 512), errno::SUCCESS);
        assert_eq!(caller.memory.load32(512, 0), 0);
    }

    #[test]
    fn reading_something_that_cannot_be_read_says_which_way_it_cannot() {
        let mut wasi = Wasi::default();
        let mut memory = memory();
        let mut caller = rt::Caller {
            memory: &mut memory,
        };
        caller.memory.store32(0, 0, 2048);
        caller.memory.store32(4, 0, 1);
        assert_eq!(wasi.fd_read(&mut caller, 1, 0, 1, 512), errno::INVAL);
        assert_eq!(wasi.fd_read(&mut caller, 99, 0, 1, 512), errno::BADF);
    }

    #[test]
    fn a_positional_read_does_not_move_the_position() {
        let mut wasi = Wasi::default();
        wasi.add_file("/f", b"0123456789");
        let fd = wasi.openat("/f", false, false);
        let mut memory = memory();
        let mut caller = rt::Caller {
            memory: &mut memory,
        };
        caller.memory.store32(0, 0, 2048);
        caller.memory.store32(4, 0, 2);
        assert_eq!(wasi.fd_pread(&mut caller, fd, 0, 1, 6, 512), errno::SUCCESS);
        assert_eq!(caller.bytes(2048, 2), b"67");
        // The ordinary read still starts at the beginning.
        assert_eq!(wasi.fd_read(&mut caller, fd, 0, 1, 512), errno::SUCCESS);
        assert_eq!(caller.bytes(2048, 2), b"01");
        // And a stream cannot be read positionally at all.
        assert_eq!(wasi.fd_pread(&mut caller, 1, 0, 1, 0, 512), errno::SPIPE);
        assert_eq!(wasi.fd_pread(&mut caller, 99, 0, 1, 0, 512), errno::BADF);
    }

    #[test]
    fn closing_takes_the_descriptor_out_and_says_so_twice() {
        let mut wasi = Wasi::default();
        let fd = wasi.openat("/f", true, false);
        assert_eq!(wasi.fd_close(fd), errno::SUCCESS);
        assert_eq!(wasi.fd_close(fd), errno::BADF);
    }

    #[test]
    fn opening_something_that_is_not_there_is_enoent_unless_asked_to_create() {
        let mut wasi = Wasi::default();
        assert_eq!(wasi.openat("/missing", false, false), -errno::NOENT);
        assert!(wasi.openat("/missing", true, false) > 0);
        assert_eq!(wasi.files["/missing"], b"");
    }

    #[test]
    fn the_environment_and_the_arguments_go_across_as_pointers_then_bytes() {
        let wasi = Wasi {
            environment: vec!["A=1".to_string(), "LONGER=two".to_string()],
            arguments: vec!["prog".to_string()],
            ..Wasi::default()
        };
        let mut memory = memory();
        let mut caller = rt::Caller {
            memory: &mut memory,
        };

        assert_eq!(
            wasi.environ_sizes_get(&mut caller, 512, 516),
            errno::SUCCESS
        );
        assert_eq!(caller.memory.load32(512, 0), 2);
        assert_eq!(caller.memory.load32(516, 0), 4 + 11);

        assert_eq!(wasi.environ_get(&mut caller, 1024, 2048), errno::SUCCESS);
        let first = caller.memory.load32(1024, 0);
        let second = caller.memory.load32(1028, 0);
        assert_eq!(caller.cstring(first), b"A=1");
        assert_eq!(caller.cstring(second), b"LONGER=two");
        assert_eq!(second - first, 4, "the strings are laid out end to end");

        assert_eq!(wasi.args_sizes_get(&mut caller, 512, 516), errno::SUCCESS);
        assert_eq!(caller.memory.load32(512, 0), 1);
        assert_eq!(caller.memory.load32(516, 0), 5);
        assert_eq!(wasi.args_get(&mut caller, 1024, 2048), errno::SUCCESS);
        assert_eq!(caller.cstring(caller.memory.load32(1024, 0)), b"prog");
    }

    #[test]
    fn random_bytes_are_the_same_two_runs_running() {
        let mut first = Wasi::default();
        let mut second = Wasi::default();
        let mut memory = memory();
        let mut caller = rt::Caller {
            memory: &mut memory,
        };
        assert_eq!(first.random_get(&mut caller, 512, 20), errno::SUCCESS);
        let one = caller.bytes(512, 20).to_vec();
        assert_eq!(second.random_get(&mut caller, 1024, 20), errno::SUCCESS);
        assert_eq!(caller.bytes(1024, 20), one.as_slice());
        assert!(one.iter().any(|byte| *byte != 0), "and not all zero");
        // A different seed gives different bytes, so the seed is what decides.
        let mut third = Wasi {
            random: 7,
            ..Wasi::default()
        };
        assert_eq!(third.random_get(&mut caller, 2048, 20), errno::SUCCESS);
        assert_ne!(caller.bytes(2048, 20), one.as_slice());
    }

    #[test]
    fn the_clock_reports_what_it_was_set_to() {
        let wasi = Wasi {
            now_milliseconds: 1_500.5,
            ..Wasi::default()
        };
        let mut memory = memory();
        let mut caller = rt::Caller {
            memory: &mut memory,
        };
        assert_eq!(wasi.clock_time_get(&mut caller, 512), errno::SUCCESS);
        assert_eq!(caller.memory.load64(512, 0), 1_500_500_000);
    }

    #[test]
    fn the_size_of_a_descriptor_is_the_size_of_what_it_holds() {
        let mut wasi = Wasi::default();
        wasi.add_file("/f", b"abcd");
        let fd = wasi.openat("/f", false, false);
        assert_eq!(wasi.size_of(fd), Some(4));
        assert_eq!(wasi.size_of(1), Some(0), "a stream has no size");
        assert_eq!(wasi.size_of(99), None);
        wasi.files.remove("/f");
        assert_eq!(wasi.size_of(fd), None, "and neither has a file that went");
    }

    #[test]
    fn exiting_carries_the_status() {
        let result = std::panic::catch_unwind(|| exit(3));
        let payload = result.expect_err("exit does not return");
        assert_eq!(payload.downcast_ref::<Exit>(), Some(&Exit { code: 3 }));
    }
}
