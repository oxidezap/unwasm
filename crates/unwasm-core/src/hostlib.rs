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
    fn iovecs<M: rt::Access>(caller: &rt::Caller<'_, M>, iovs: i32, count: i32) -> Vec<(i32, i32)> {
        (0..count)
            .map(|at| {
                let entry = iovs.wrapping_add(at.wrapping_mul(8));
                (caller.read_i32(entry), caller.read_i32(entry + 4))
            })
            .collect()
    }

    /// `fd_write`: gathers the iovecs and appends them where the descriptor
    /// points.
    pub fn fd_write<M: rt::Access>(
        &mut self,
        caller: &mut rt::Caller<'_, M>,
        fd: i32,
        iovs: i32,
        count: i32,
        written: i32,
    ) -> i32 {
        let mut bytes = Vec::new();
        for (address, length) in Self::iovecs(caller, iovs, count) {
            bytes.extend_from_slice(&caller.bytes(address, length));
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
        caller.write_i32(written, total as i32);
        errno::SUCCESS
    }

    /// `fd_read`: scatters into the iovecs, stopping at the end of the source.
    pub fn fd_read<M: rt::Access>(
        &mut self,
        caller: &mut rt::Caller<'_, M>,
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
        caller.write_i32(read, taken as i32);
        errno::SUCCESS
    }

    /// `fd_pread`: a read at an explicit offset, which does not move the
    /// position.
    pub fn fd_pread<M: rt::Access>(
        &mut self,
        caller: &mut rt::Caller<'_, M>,
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
    pub fn fd_seek<M: rt::Access>(
        &mut self,
        caller: &mut rt::Caller<'_, M>,
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
        caller.write_i64(out, position);
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
    pub fn environ_sizes_get<M: rt::Access>(
        &self,
        caller: &mut rt::Caller<'_, M>,
        count: i32,
        size: i32,
    ) -> i32 {
        let bytes: usize = self
            .environment
            .iter()
            .map(|entry| entry.len() + 1)
            .sum::<usize>();
        caller.write_i32(count, self.environment.len() as i32);
        caller.write_i32(size, bytes as i32);
        errno::SUCCESS
    }

    /// `environ_get`: the pointers, then the strings they point at.
    pub fn environ_get<M: rt::Access>(
        &self,
        caller: &mut rt::Caller<'_, M>,
        pointers: i32,
        buffer: i32,
    ) -> i32 {
        let mut at = buffer;
        for (index, entry) in self.environment.iter().enumerate() {
            caller.write_i32(pointers + (index as i32) * 4, at);
            let mut bytes = entry.clone().into_bytes();
            bytes.push(0);
            caller.write(at, &bytes);
            at += bytes.len() as i32;
        }
        errno::SUCCESS
    }

    /// `args_sizes_get`.
    pub fn args_sizes_get<M: rt::Access>(
        &self,
        caller: &mut rt::Caller<'_, M>,
        count: i32,
        size: i32,
    ) -> i32 {
        let bytes: usize = self
            .arguments
            .iter()
            .map(|entry| entry.len() + 1)
            .sum::<usize>();
        caller.write_i32(count, self.arguments.len() as i32);
        caller.write_i32(size, bytes as i32);
        errno::SUCCESS
    }

    /// `args_get`.
    pub fn args_get<M: rt::Access>(
        &self,
        caller: &mut rt::Caller<'_, M>,
        pointers: i32,
        buffer: i32,
    ) -> i32 {
        let mut at = buffer;
        for (index, entry) in self.arguments.iter().enumerate() {
            caller.write_i32(pointers + (index as i32) * 4, at);
            let mut bytes = entry.clone().into_bytes();
            bytes.push(0);
            caller.write(at, &bytes);
            at += bytes.len() as i32;
        }
        errno::SUCCESS
    }

    /// `random_get`, from the seeded generator described at the top.
    pub fn random_get<M: rt::Access>(
        &mut self,
        caller: &mut rt::Caller<'_, M>,
        buffer: i32,
        length: i32,
    ) -> i32 {
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
    pub fn clock_time_get<M: rt::Access>(&self, caller: &mut rt::Caller<'_, M>, out: i32) -> i32 {
        #[allow(clippy::cast_possible_truncation)]
        let nanoseconds = (self.now_milliseconds * 1_000_000.0) as i64;
        caller.write_i64(out, nanoseconds);
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

    /// `__syscall_openat(dirfd, path, flags, mode)`, which is how Emscripten
    /// spells `open`.
    ///
    /// The directory descriptor is ignored: every path in this filesystem is
    /// absolute, and resolving a relative one against a directory that does
    /// not exist would be inventing a tree.
    pub fn openat_at<M: rt::Access>(
        &mut self,
        caller: &rt::Caller<'_, M>,
        path: i32,
        flags: i32,
    ) -> i32 {
        let path = String::from_utf8_lossy(&caller.cstring(path)).into_owned();
        // musl's values, which is what an Emscripten module was built against:
        // O_CREAT is 0o100 and O_APPEND 0o2000.
        self.openat(&path, flags & 0o100 != 0, flags & 0o2000 != 0)
    }

    /// `__syscall_unlinkat`. Returns 0, or the negative errno a syscall does.
    pub fn unlinkat<M: rt::Access>(&mut self, caller: &rt::Caller<'_, M>, path: i32) -> i32 {
        let path = String::from_utf8_lossy(&caller.cstring(path)).into_owned();
        if self.files.remove(&path).is_some() {
            0
        } else {
            -errno::NOENT
        }
    }

    /// Writes a `struct stat` at `buf` for something of `size` bytes.
    ///
    /// The offsets are not guessed. They are Emscripten's own, from the
    /// `struct_info_generated.json` its JavaScript glue is built against — the
    /// 96-byte layout with `st_size` at 24 as an i64 and `st_ino` at 88. This
    /// is the one place in this file that depends on a *layout* rather than on
    /// something the wasm itself says, and it is toolchain-specific: a build
    /// whose libc laid the struct out differently would be read wrong here and
    /// nowhere else.
    ///
    /// Times are all zero. A filesystem that lives in this struct has no
    /// history, and inventing one would be inventing evidence.
    pub fn write_stat<M: rt::Access>(
        &self,
        caller: &mut rt::Caller<'_, M>,
        buf: i32,
        size: usize,
        directory: bool,
        inode: i64,
    ) -> i32 {
        // 96 zero bytes first, so every field this does not set reads as zero
        // rather than as whatever the guest left there.
        caller.write(buf, &[0u8; 96]);
        let mode = if directory {
            0o040_755 // S_IFDIR | rwxr-xr-x
        } else {
            0o100_644 // S_IFREG | rw-r--r--
        };
        caller.write_i32(buf, 1); // st_dev
        caller.write_i32(buf + 4, mode);
        caller.write_i32(buf + 8, 1); // st_nlink
        caller.write_i64(buf + 24, size as i64);
        caller.write_i32(buf + 32, 4096); // st_blksize
        caller.write_i32(buf + 36, size.div_ceil(512) as i32);
        caller.write_i64(buf + 88, inode);
        errno::SUCCESS
    }

    /// `__syscall_stat64` and `__syscall_lstat64`: no symbolic links live in
    /// this filesystem, so the two are the same answer.
    ///
    /// Returns 0, or the negative errno a Linux syscall returns.
    pub fn stat_path<M: rt::Access>(
        &self,
        caller: &mut rt::Caller<'_, M>,
        path: i32,
        buf: i32,
    ) -> i32 {
        let path = String::from_utf8_lossy(&caller.cstring(path)).into_owned();
        match self.files.get(&path) {
            Some(contents) => {
                let size = contents.len();
                let inode = self.inode_of(&path);
                self.write_stat(caller, buf, size, false, inode)
            }
            // A directory exists if anything lives under it: this filesystem
            // has no directory entries of its own, and saying "not found" for
            // a prefix every path shares would be the wrong answer.
            None if self.is_directory(&path) => self.write_stat(caller, buf, 0, true, 1),
            None => -errno::NOENT,
        }
    }

    /// `__syscall_fstat64`, for something already open.
    pub fn stat_fd<M: rt::Access>(&self, caller: &mut rt::Caller<'_, M>, fd: i32, buf: i32) -> i32 {
        match self.open.get(&fd) {
            Some(Descriptor::File(open)) => match self.files.get(&open.path) {
                Some(contents) => {
                    let (size, inode) = (contents.len(), self.inode_of(&open.path));
                    self.write_stat(caller, buf, size, false, inode)
                }
                None => -errno::NOENT,
            },
            Some(Descriptor::Directory(_)) => self.write_stat(caller, buf, 0, true, 1),
            // A standard stream is a character device, and reporting it as a
            // regular file of length zero is what makes a `isatty` answer
            // wrong. Zero size, and the mode says what it is.
            Some(Descriptor::Standard(_)) => {
                let written = self.write_stat(caller, buf, 0, false, 0);
                caller.write_i32(buf + 4, 0o020_620); // S_IFCHR | rw--w----
                written
            }
            None => -errno::BADF,
        }
    }

    /// Whether anything in the filesystem lives under `path`.
    #[must_use]
    pub fn is_directory(&self, path: &str) -> bool {
        let prefix = if path.ends_with('/') {
            path.to_string()
        } else {
            format!("{path}/")
        };
        path == "/" || self.files.keys().any(|name| name.starts_with(&prefix))
    }

    /// A stable inode for a path: its position in the map.
    ///
    /// Stable within a run, which is what a program comparing two `stat`
    /// results needs, and not stable across runs, which nothing needs.
    fn inode_of(&self, path: &str) -> i64 {
        self.files
            .keys()
            .position(|name| name == path)
            .map_or(0, |at| at as i64 + 2)
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

// ---- the C++ runtime ----

/// The exception entry points a module built with `-fexceptions` imports.
///
/// Emscripten's JavaScript glue keeps a stack of thrown objects and hands the
/// current one back to `__cxa_begin_catch`; this is the same, in a `Vec`. What
/// it deliberately does not do is invent a type match: `__cxa_find_matching_catch`
/// returns the exception and lets the personality code in the module decide,
/// which is what the glue does too.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Cxx {
    /// Exceptions thrown and not yet caught, innermost last.
    pub thrown: Vec<rt::GuestException>,
    /// How many are currently being caught, for `__cxa_uncaught_exceptions`.
    pub caught: Vec<rt::GuestException>,
}

impl Cxx {
    /// `__cxa_throw(exception, info, destructor)`.
    ///
    /// # Panics
    ///
    /// Always: a throw is a panic carrying the exception, which is what a
    /// generated `invoke_*` trampoline catches.
    pub fn throw(&mut self, exception: i32, info: i32) -> ! {
        self.thrown.push(rt::GuestException { exception, info });
        rt::throw(exception, info);
    }

    /// `__cxa_begin_catch(pointer)`. Returns the pointer it was given, and
    /// moves the exception to the caught list.
    pub fn begin_catch(&mut self, pointer: i32) -> i32 {
        if let Some(at) = self
            .thrown
            .iter()
            .rposition(|exception| exception.exception == pointer)
        {
            let exception = self.thrown.remove(at);
            self.caught.push(exception);
        }
        pointer
    }

    /// `__cxa_end_catch`.
    pub fn end_catch(&mut self) {
        self.caught.pop();
    }

    /// `__cxa_rethrow`, and `__resumeException`.
    ///
    /// # Panics
    ///
    /// Always, unless there is nothing to rethrow — which is a bug in the
    /// module rather than in the host, and traps rather than returning.
    pub fn rethrow(&mut self) -> ! {
        match self.caught.pop().or_else(|| self.thrown.pop()) {
            Some(exception) => rt::throw(exception.exception, exception.info),
            None => rt::trap("__cxa_rethrow with nothing in flight"),
        }
    }

    /// `__cxa_uncaught_exceptions`.
    #[must_use]
    pub fn uncaught(&self) -> i32 {
        self.thrown.len() as i32
    }

    /// The exception currently in flight, or 0.
    ///
    /// Not enough to answer `__cxa_find_matching_catch_N`, which is why the
    /// generated host leaves that one to you: matching a throw against a catch
    /// clause means comparing the thrown type against each requested one —
    /// through the module's own `__cxa_can_catch` — and adjusting the pointer
    /// for a base class. This is the exception, not the decision.
    #[must_use]
    pub fn in_flight(&self) -> i32 {
        self.caught
            .last()
            .or_else(|| self.thrown.last())
            .map_or(0, |exception| exception.exception)
    }
}

/// What `__assert_fail` was told, in a form a person can read.
///
/// # Panics
///
/// Always. A failed assertion in the guest is not something a host can carry
/// on from, and the four strings are exactly what makes it diagnosable.
pub fn assert_fail<M: rt::Access>(
    caller: &rt::Caller<'_, M>,
    expression: i32,
    file: i32,
    line: i32,
    function: i32,
) -> ! {
    let text = |address: i32| String::from_utf8_lossy(&caller.cstring(address)).into_owned();
    panic!(
        "assertion failed: {} at {}:{line} in {}",
        text(expression),
        text(file),
        text(function)
    );
}

// ---- Emscripten's own runtime ----

/// The parts of Emscripten's JavaScript runtime that are not JavaScript.
///
/// `emscripten_asm_const_*` is not here and cannot be: it runs a string of
/// JavaScript the module carries, and there is no honest mechanical answer.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Emscripten {
    /// How many times the runtime was asked to stay alive.
    pub keepalive: i32,
    /// Callbacks `emscripten_async_call` was given: `(function, argument,
    /// milliseconds)`. Recorded rather than run — running them would need a
    /// way back into the instance, and inventing a schedule is worse than
    /// saying what was asked for.
    pub scheduled: Vec<(i32, i32, i32)>,
    /// Whether the module asked to exit while keeping the runtime alive.
    pub exited_with_live_runtime: bool,
    /// How many logical cores to report.
    pub cores: i32,
}

impl Emscripten {
    /// `emscripten_resize_heap(bytes)`: grow linear memory to at least that.
    ///
    /// Returns 1 on success and 0 on refusal, which is the opposite of
    /// `memory.grow`'s convention and is what the module checks.
    pub fn resize_heap<M: rt::Access>(caller: &mut rt::Caller<'_, M>, bytes: i32) -> i32 {
        let wanted = u64::from(bytes as u32);
        let have = caller.memory.byte_len() as u64;
        if wanted <= have {
            return 1;
        }
        let pages = wanted.div_ceil(rt::PAGE_SIZE as u64) - have / rt::PAGE_SIZE as u64;
        i32::from(caller.memory.grow_pages(pages as u32) >= 0)
    }

    /// `emscripten_get_heap_max`: the ceiling the memory type declared, or the
    /// one a 32-bit memory has regardless.
    ///
    /// A memory with no declared maximum can reach 4 GiB, which does not fit
    /// in the i32 this returns. The answer is a page short of it rather than
    /// `u32::MAX`, because the module divides it by the page size and a value
    /// that is not a multiple of one is a number no memory can ever have.
    #[must_use]
    pub fn heap_max<M: rt::Access>(caller: &rt::Caller<'_, M>) -> i32 {
        let pages = u64::from(caller.memory.declared_max_pages().unwrap_or(65536));
        let bytes = pages * rt::PAGE_SIZE as u64;
        bytes.min(0xFFFF_0000) as u32 as i32
    }

    /// `emscripten_console_error`: a string, straight to `stderr`.
    pub fn console_error<M: rt::Access>(
        &self,
        caller: &rt::Caller<'_, M>,
        wasi: &mut Wasi,
        text: i32,
    ) {
        wasi.stderr.extend_from_slice(&caller.cstring(text));
        wasi.stderr.push(b'\n');
    }
}

/// The imports a run answered with a default rather than with an answer.
///
/// A stub that returns zero is a hypothesis — the module cannot tell it from a
/// real answer, and neither can a reader. This does not make it safe; it makes
/// it *visible*. Every default is counted and the arguments of the first call
/// are kept, so a run that produced something interesting comes with the list
/// of assumptions it rests on, and the reader can decide whether the
/// interesting thing survives them.
///
/// The rule stays what it was: `unwasm host` writes `todo!()` unless asked for
/// this. What this is for is the case where a path has to be *reached* before
/// it can be studied, and the imports in the way are ones nobody outside the
/// application can answer.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Unanswered {
    /// Each import that was answered with a default: how many times, and the
    /// arguments it was given the first time.
    pub calls: std::collections::BTreeMap<String, (usize, Vec<i64>)>,
    /// The order they were first reached in, which is usually the story.
    pub order: Vec<String>,
}

impl Unanswered {
    /// Records one call answered with a default.
    pub fn record(&mut self, import: &str, arguments: &[i64]) {
        match self.calls.get_mut(import) {
            Some((count, _)) => *count += 1,
            None => {
                self.calls
                    .insert(import.to_string(), (1, arguments.to_vec()));
                self.order.push(import.to_string());
            }
        }
    }

    /// Whether anything was.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.calls.is_empty()
    }

    /// What a run rested on, in the order it reached them.
    ///
    /// Print this beside any result from such a run. A conclusion drawn under
    /// forty invented answers is a conclusion about forty invented answers.
    #[must_use]
    pub fn report(&self) -> String {
        if self.calls.is_empty() {
            return "nothing was answered with a default\n".to_string();
        }
        let mut out = format!(
            "{} imports were answered with a default rather than an answer:\n",
            self.calls.len()
        );
        for import in &self.order {
            if let Some((count, arguments)) = self.calls.get(import) {
                let shown: Vec<String> = arguments.iter().map(i64::to_string).collect();
                out.push_str(&format!(
                    "  {import}  {count}x, first ({})\n",
                    shown.join(", ")
                ));
            }
        }
        out
    }
}

// ---- the clock, as C sees it ----

/// `struct tm`, at the offsets Emscripten's own `C_STRUCTS.tm` gives.
///
/// 44 bytes: seconds at 0, then minute, hour, day, month, year, weekday, day
/// of the year, `isdst` at 32 and `gmtoff` at 36. Like `struct stat`, this is
/// a layout rather than something the wasm says, and it is the only kind of
/// thing in this file that a different toolchain could move.
///
/// Everything is UTC. The host supplies the clock and nothing here reads the
/// machine's — a run whose result depends on which timezone it happened to be
/// in is a run nobody can repeat.
pub fn write_tm<M: rt::Access>(caller: &mut rt::Caller<'_, M>, at: i32, seconds: i64) -> i32 {
    let days = seconds.div_euclid(86_400);
    let rest = seconds.rem_euclid(86_400);
    let (year, month, day, yday) = civil_from_days(days);

    caller.write(at, &[0u8; 44]);
    caller.write_i32(at, (rest % 60) as i32); // tm_sec
    caller.write_i32(at + 4, ((rest / 60) % 60) as i32); // tm_min
    caller.write_i32(at + 8, (rest / 3600) as i32); // tm_hour
    caller.write_i32(at + 12, day); // tm_mday
    caller.write_i32(at + 16, month - 1); // tm_mon, zero-based
    caller.write_i32(at + 20, year - 1900); // tm_year
    // 1970-01-01 was a Thursday, which is what anchors the weekday.
    caller.write_i32(at + 24, (days + 4).rem_euclid(7) as i32); // tm_wday
    caller.write_i32(at + 28, yday); // tm_yday
    caller.write_i32(at + 32, 0); // tm_isdst: UTC has none
    caller.write_i32(at + 36, 0); // tm_gmtoff
    0
}

/// The civil date of a day count from 1970-01-01, and the day of its year.
///
/// Howard Hinnant's algorithm, which is the one every C library uses and which
/// is exact for every year a `time_t` can hold — no month tables, no leap-year
/// special cases to get wrong.
fn civil_from_days(days: i64) -> (i32, i32, i32, i32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { year + 1 } else { year };
    // The day of the year, counted from the first of January of that year.
    let january_first = days_from_civil(year, 1, 1);
    (
        year as i32,
        month as i32,
        day as i32,
        (days - january_first) as i32,
    )
}

/// The inverse, for the one thing the day-of-year needs.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let yoe = year - era * 400;
    let mp = if month > 2 { month - 3 } else { month + 9 };
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// `_tzset_js(timezone, daylight, std_name, dst_name)`.
///
/// UTC, for the same reason `write_tm` is: the host supplies the clock, and a
/// timezone read from the machine makes a run depend on where it ran.
pub fn tzset<M: rt::Access>(
    caller: &mut rt::Caller<'_, M>,
    timezone: i32,
    daylight: i32,
    std_name: i32,
    dst_name: i32,
) {
    caller.write_i32(timezone, 0);
    caller.write_i32(daylight, 0);
    for name in [std_name, dst_name] {
        if name != 0 {
            caller.write(name, b"UTC\0");
        }
    }
}

// ---- embind, and emval ----

/// One `_embind_register_*` call, as it happened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Registration {
    /// Which registration it was, without the `_embind_register_` prefix.
    pub kind: String,
    /// The name it registered, when the call carries one.
    pub name: String,
    /// The arguments, in order, for anything the name does not cover.
    pub arguments: Vec<i32>,
}

/// What embind registered and what emval is holding.
///
/// Registration is recorded rather than acted on. Acting on it would mean
/// building a JavaScript type system, and what a reader wants from a run is
/// the list: which classes, which methods, in which order — the same list
/// `unwasm inspect` recovers statically, confirmed by the module actually
/// running.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Embind {
    /// Every registration, in the order the module made it.
    pub registrations: Vec<Registration>,
    /// emval handles by number, with their reference counts. Handle 0 is
    /// never given out: the module treats it as null.
    pub handles: std::collections::BTreeMap<i32, i32>,
    /// The next handle to hand out.
    pub next_handle: i32,
}

impl Registration {
    /// The invoker and the context of a registration that has them.
    ///
    /// An embind registration is a pair of function pointers and a name: the
    /// invoker unpacks the arguments and calls the second, so reaching the
    /// registered function from outside is `invoke_slot(invoker, [context,
    /// …args])`. Which arguments hold which is embind's own C signature:
    ///
    /// ```c
    /// _embind_register_function(name, argCount, argTypes, signature,
    ///                           invoker, function, isAsync, isNonnullReturn);
    /// _embind_register_class_function(classType, methodName, argCount, argTypes,
    ///                                 signature, invoker, context, …);
    /// ```
    ///
    /// The name is not in [`Self::arguments`] — it is [`Self::name`] — so the
    /// positions here are the rest, in order.
    #[must_use]
    pub fn call(&self) -> Option<Call> {
        let (invoker, context) = match self.kind.as_str() {
            // argCount, argTypes, signature, invoker, function, …
            "function" => (3, 4),
            // classType, argCount, argTypes, signature, invoker, context, …
            "class_function" | "class_class_function" => (4, 5),
            // classType, argCount, argTypes, signature, invoker, …
            "class_constructor" => (4, 5),
            _ => return None,
        };
        Some(Call {
            invoker: *self.arguments.get(invoker)?,
            context: *self.arguments.get(context)?,
            arity: *self.arguments.first()?,
        })
    }
}

/// What it takes to call something the module registered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Call {
    /// The table slot of the invoker, which unpacks the arguments.
    pub invoker: i32,
    /// The function the invoker calls, as its first argument.
    pub context: i32,
    /// How many arguments the registration says it takes, counting the return
    /// type — embind's `argCount` includes it.
    pub arity: i32,
}

impl Embind {
    /// Records a registration whose second argument is its name.
    pub fn register<M: rt::Access>(
        &mut self,
        caller: &rt::Caller<'_, M>,
        kind: &str,
        name: i32,
        arguments: &[i32],
    ) {
        self.registrations.push(Registration {
            kind: kind.to_string(),
            name: String::from_utf8_lossy(&caller.cstring(name)).into_owned(),
            arguments: arguments.to_vec(),
        });
    }

    /// Records a registration that carries no name.
    pub fn register_unnamed(&mut self, kind: &str, arguments: &[i32]) {
        self.registrations.push(Registration {
            kind: kind.to_string(),
            name: String::new(),
            arguments: arguments.to_vec(),
        });
    }

    /// The registration of a free function by that name.
    #[must_use]
    pub fn function(&self, name: &str) -> Option<&Registration> {
        self.registrations
            .iter()
            .find(|entry| entry.kind == "function" && entry.name == name)
    }

    /// The registration of a method by that name, whatever class it is on.
    ///
    /// A method's registration does not carry its class's *name*, only the
    /// type id — so this is by method name, and a module with two classes
    /// sharing one is ambiguous. It returns the first, and there is no way to
    /// do better without resolving type ids.
    #[must_use]
    pub fn method(&self, name: &str) -> Option<&Registration> {
        self.registrations
            .iter()
            .find(|entry| entry.kind.ends_with("class_function") && entry.name == name)
    }

    /// `_emval_take_value`: a new handle, with one reference.
    pub fn take_value(&mut self) -> i32 {
        if self.next_handle < 1 {
            self.next_handle = 1;
        }
        let handle = self.next_handle;
        self.next_handle += 1;
        self.handles.insert(handle, 1);
        handle
    }

    /// `_emval_incref`.
    pub fn incref(&mut self, handle: i32) {
        if handle != 0 {
            *self.handles.entry(handle).or_insert(0) += 1;
        }
    }

    /// `_emval_decref`. A handle whose count reaches zero is released.
    pub fn decref(&mut self, handle: i32) {
        if handle == 0 {
            return;
        }
        if let Some(count) = self.handles.get_mut(&handle) {
            *count -= 1;
            if *count <= 0 {
                self.handles.remove(&handle);
            }
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

    // ---- the C++ runtime ----

    #[test]
    fn an_exception_is_thrown_caught_and_ended() {
        let mut cxx = Cxx::default();
        let thrown = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut throwing = Cxx::default();
            throwing.throw(0x1000, 0x2000)
        }));
        let payload = thrown.expect_err("a throw does not return");
        assert_eq!(
            payload.downcast_ref::<rt::GuestException>(),
            Some(&rt::GuestException {
                exception: 0x1000,
                info: 0x2000
            })
        );

        // The trampoline caught it; the module then asks for it by pointer.
        cxx.thrown.push(rt::GuestException {
            exception: 0x1000,
            info: 0x2000,
        });
        assert_eq!(cxx.uncaught(), 1);
        assert_eq!(cxx.begin_catch(0x1000), 0x1000);
        assert_eq!(cxx.uncaught(), 0, "it is being caught, not in flight");
        assert_eq!(cxx.in_flight(), 0x1000);
        cxx.end_catch();
        assert_eq!(cxx.in_flight(), 0, "and nothing is left");
    }

    #[test]
    fn beginning_a_catch_for_something_that_was_not_thrown_still_answers() {
        // The module hands back a pointer it got from the personality routine,
        // and a host that lost track must not lose the pointer with it.
        let mut cxx = Cxx::default();
        assert_eq!(cxx.begin_catch(0x40), 0x40);
        assert!(cxx.caught.is_empty());
    }

    #[test]
    #[should_panic(expected = "wasm trap: __cxa_rethrow with nothing in flight")]
    fn rethrowing_nothing_traps_rather_than_inventing_an_exception() {
        Cxx::default().rethrow();
    }

    #[test]
    fn rethrowing_takes_the_one_being_caught_first() {
        let mut cxx = Cxx::default();
        cxx.thrown.push(rt::GuestException {
            exception: 1,
            info: 0,
        });
        cxx.caught.push(rt::GuestException {
            exception: 2,
            info: 0,
        });
        let again = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| cxx.rethrow()));
        let payload = again.expect_err("a rethrow does not return");
        assert_eq!(
            payload
                .downcast_ref::<rt::GuestException>()
                .map(|exception| exception.exception),
            Some(2)
        );
    }

    #[test]
    fn a_rethrow_with_only_a_thrown_one_takes_that() {
        let mut cxx = Cxx::default();
        cxx.thrown.push(rt::GuestException {
            exception: 9,
            info: 0,
        });
        let again = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| cxx.rethrow()));
        assert!(again.is_err());
    }

    #[test]
    #[should_panic(expected = "assertion failed: x != 0 at check.c:42 in verify")]
    fn a_failed_assertion_reads_all_four_of_its_strings() {
        let mut memory = memory();
        let mut caller = rt::Caller {
            memory: &mut memory,
        };
        caller.write(100, b"x != 0\0");
        caller.write(200, b"check.c\0");
        caller.write(300, b"verify\0");
        assert_fail(&caller, 100, 200, 42, 300);
    }

    // ---- Emscripten's runtime ----

    #[test]
    fn resizing_the_heap_grows_it_and_reports_the_way_the_module_expects() {
        let mut memory = rt::Memory::new(1, Some(4));
        let mut caller = rt::Caller {
            memory: &mut memory,
        };
        // 1 on success, which is the opposite of `memory.grow`'s convention.
        assert_eq!(Emscripten::resize_heap(&mut caller, 3 * 65536), 1);
        assert_eq!(caller.memory.size(), 3);
        // Already big enough is success without growing.
        assert_eq!(Emscripten::resize_heap(&mut caller, 65536), 1);
        assert_eq!(caller.memory.size(), 3);
        // Past the declared maximum is a refusal, and nothing moves.
        assert_eq!(Emscripten::resize_heap(&mut caller, 9 * 65536), 0);
        assert_eq!(caller.memory.size(), 3);
    }

    #[test]
    fn the_heap_maximum_is_what_the_memory_type_declared() {
        let mut memory = rt::Memory::new(1, Some(4));
        let caller = rt::Caller {
            memory: &mut memory,
        };
        assert_eq!(Emscripten::heap_max(&caller), 4 * 65536);

        // Undeclared means the ceiling a 32-bit memory has anyway, which does
        // not fit in an i32 and comes back as -65536 rather than as a wrap to
        // something small.
        let mut memory = rt::Memory::new(1, None);
        let caller = rt::Caller {
            memory: &mut memory,
        };
        assert_eq!(Emscripten::heap_max(&caller) as u32, 0xFFFF_0000);
    }

    #[test]
    fn console_error_goes_to_stderr_with_a_newline() {
        let mut memory = memory();
        let mut wasi = Wasi::default();
        let emscripten = Emscripten::default();
        let mut caller = rt::Caller {
            memory: &mut memory,
        };
        caller.write(64, b"something went wrong\0");
        emscripten.console_error(&caller, &mut wasi, 64);
        assert_eq!(wasi.stderr_text(), "something went wrong\n");
    }

    // ---- embind ----

    #[test]
    fn a_registration_is_recorded_with_the_name_it_carried() {
        let mut memory = memory();
        let mut embind = Embind::default();
        let mut caller = rt::Caller {
            memory: &mut memory,
        };
        caller.write(64, b"Uint8List\0");
        embind.register(&caller, "class", 64, &[1, 2, 3]);
        embind.register_unnamed("void", &[9]);
        assert_eq!(
            embind.registrations,
            vec![
                Registration {
                    kind: "class".to_string(),
                    name: "Uint8List".to_string(),
                    arguments: vec![1, 2, 3],
                },
                Registration {
                    kind: "void".to_string(),
                    name: String::new(),
                    arguments: vec![9],
                },
            ]
        );
    }

    #[test]
    fn a_time_becomes_the_struct_c_expects() {
        let mut memory = memory();
        let mut caller = rt::Caller {
            memory: &mut memory,
        };
        // 2026-07-28 00:28:16 UTC, a Tuesday and the 209th day of the year.
        let seconds = 1_785_198_496;
        assert_eq!(write_tm(&mut caller, 256, seconds), 0);
        assert_eq!(caller.read_i32(256), 16, "tm_sec");
        assert_eq!(caller.read_i32(256 + 4), 28, "tm_min");
        assert_eq!(caller.read_i32(256 + 8), 0, "tm_hour");
        assert_eq!(caller.read_i32(256 + 12), 28, "tm_mday");
        assert_eq!(caller.read_i32(256 + 16), 6, "tm_mon is zero-based");
        assert_eq!(caller.read_i32(256 + 20), 126, "tm_year is since 1900");
        assert_eq!(caller.read_i32(256 + 24), 2, "tm_wday: Tuesday");
        assert_eq!(caller.read_i32(256 + 28), 208, "tm_yday, from zero");
        assert_eq!(caller.read_i32(256 + 32), 0, "UTC has no daylight saving");
        assert_eq!(caller.read_i32(256 + 36), 0, "and no offset");
    }

    #[test]
    fn the_epoch_and_the_dates_around_it_are_right() {
        let mut memory = memory();
        let mut caller = rt::Caller {
            memory: &mut memory,
        };
        // The epoch itself: a Thursday, which is what anchors every weekday.
        write_tm(&mut caller, 0, 0);
        assert_eq!(caller.read_i32(20), 70, "1970");
        assert_eq!(caller.read_i32(12), 1, "the first");
        assert_eq!(caller.read_i32(24), 4, "a Thursday");
        assert_eq!(caller.read_i32(28), 0, "day zero of the year");

        // A second before it, which is where a naive division goes wrong.
        write_tm(&mut caller, 64, -1);
        assert_eq!(caller.read_i32(64 + 20), 69, "1969");
        assert_eq!(caller.read_i32(64 + 16), 11, "December");
        assert_eq!(caller.read_i32(64 + 12), 31);
        assert_eq!(caller.read_i32(64 + 8), 23, "23:59:59");
        assert_eq!(caller.read_i32(64 + 4), 59);
        assert_eq!(caller.read_i32(64), 59);

        // The 29th of February in a leap year, and the day after.
        write_tm(&mut caller, 128, 1_709_164_800);
        assert_eq!(caller.read_i32(128 + 12), 29);
        assert_eq!(caller.read_i32(128 + 16), 1, "February");
        assert_eq!(caller.read_i32(128 + 28), 59, "the 60th day");
        write_tm(&mut caller, 192, 1_709_164_800 + 86_400);
        assert_eq!(caller.read_i32(192 + 12), 1);
        assert_eq!(caller.read_i32(192 + 16), 2, "March");

        // And a century year that is not a leap year, which is the case a
        // simpler algorithm gets wrong: 2100-03-01.
        write_tm(&mut caller, 256, 4_107_542_400);
        assert_eq!(caller.read_i32(256 + 20), 200, "2100");
        assert_eq!(caller.read_i32(256 + 16), 2, "March");
        assert_eq!(caller.read_i32(256 + 12), 1);
    }

    #[test]
    fn tzset_says_utc_rather_than_wherever_this_ran() {
        let mut memory = memory();
        let mut caller = rt::Caller {
            memory: &mut memory,
        };
        caller.write(64, b"xxxx");
        caller.write(96, b"xxxx");
        tzset(&mut caller, 0, 4, 64, 96);
        assert_eq!(caller.read_i32(0), 0, "no offset from UTC");
        assert_eq!(caller.read_i32(4), 0, "and no daylight saving");
        assert_eq!(caller.cstring(64), b"UTC");
        assert_eq!(caller.cstring(96), b"UTC");
        // A build that passes no second name gets nothing written at 0.
        tzset(&mut caller, 0, 4, 64, 0);
        assert_eq!(caller.cstring(64), b"UTC");
    }

    #[test]
    fn a_registration_says_what_it_takes_to_call_it() {
        // embind's own C signature decides which argument is which, and
        // getting it wrong means calling a table slot that is not an invoker.
        let mut memory = memory();
        let mut embind = Embind::default();
        let mut caller = rt::Caller {
            memory: &mut memory,
        };
        caller.write(64, b"initVoipStack\0");
        // `_embind_register_function(name, argCount, argTypes, signature,
        //                            invoker, function, isAsync, isNonnull)`
        embind.register(&caller, "function", 64, &[1, 200, 300, 4173, 8000, 0, 0]);
        let registered = embind.function("initVoipStack").expect("registered");
        let call = registered.call().expect("a function is callable");
        assert_eq!(call.invoker, 4173);
        assert_eq!(call.context, 8000);
        assert_eq!(call.arity, 1, "embind counts the return type");

        // A method's invoker and context sit one further along, because the
        // class type comes first.
        caller.write(128, b"push_back\0");
        embind.register(
            &caller,
            "class_function",
            128,
            &[9, 2, 200, 300, 5000, 9000, 0, 0, 0],
        );
        let method = embind.method("push_back").expect("registered");
        assert_eq!(
            method.call(),
            Some(Call {
                invoker: 5000,
                context: 9000,
                arity: 9
            })
        );

        // A registration that is a *type* rather than something to call says
        // so rather than pointing at whatever happens to be in that position.
        embind.register(&caller, "integer", 64, &[1, 2, 3, 4]);
        assert!(
            embind
                .registrations
                .last()
                .expect("present")
                .call()
                .is_none()
        );
        assert!(embind.function("nothing").is_none());
        assert!(embind.method("nothing").is_none());
    }

    #[test]
    fn an_emval_handle_is_counted_and_released() {
        let mut embind = Embind::default();
        let handle = embind.take_value();
        assert_eq!(handle, 1, "zero is null and is never handed out");
        assert_eq!(embind.handles[&handle], 1);
        embind.incref(handle);
        assert_eq!(embind.handles[&handle], 2);
        embind.decref(handle);
        embind.decref(handle);
        assert!(!embind.handles.contains_key(&handle), "released at zero");

        // Null is not a handle, in either direction.
        embind.incref(0);
        embind.decref(0);
        assert!(embind.handles.is_empty());
        // And decrementing something that is not held is not a panic.
        embind.decref(77);
        assert!(embind.handles.is_empty());
    }

    #[test]
    fn handles_keep_going_up() {
        let mut embind = Embind::default();
        assert_eq!(embind.take_value(), 1);
        assert_eq!(embind.take_value(), 2);
        embind.decref(1);
        assert_eq!(embind.take_value(), 3, "a released number is not reused");
    }

    // ---- syscalls ----

    #[test]
    fn openat_reads_its_path_out_of_memory_and_honours_the_flags() {
        let mut memory = memory();
        let mut wasi = Wasi::default();
        let mut caller = rt::Caller {
            memory: &mut memory,
        };
        caller.write(64, b"/notes.txt\0");
        // Without O_CREAT a missing file is a negative errno, not a descriptor.
        assert_eq!(wasi.openat_at(&caller, 64, 0), -errno::NOENT);
        let fd = wasi.openat_at(&caller, 64, 0o100);
        assert!(fd > 0, "O_CREAT makes it");
        assert!(wasi.files.contains_key("/notes.txt"));

        // O_APPEND reaches the descriptor.
        let appending = wasi.openat_at(&caller, 64, 0o2000);
        assert!(matches!(
            &wasi.open[&appending],
            Descriptor::File(open) if open.append
        ));

        assert_eq!(wasi.unlinkat(&caller, 64), 0);
        assert_eq!(wasi.unlinkat(&caller, 64), -errno::NOENT);
    }

    #[test]
    fn a_stat_writes_the_layout_emscriptens_own_glue_writes() {
        let mut wasi = Wasi::default();
        wasi.add_file("/data/file", b"0123456789");
        let mut memory = memory();
        let mut caller = rt::Caller {
            memory: &mut memory,
        };
        caller.write(64, b"/data/file\0");
        // Something recognisable in the buffer first, so a field left unset is
        // visible as a field left unset.
        caller.write(256, &[0xEE; 96]);

        assert_eq!(wasi.stat_path(&mut caller, 64, 256), errno::SUCCESS);
        assert_eq!(caller.read_i32(256 + 4), 0o100_644, "S_IFREG and its mode");
        assert_eq!(caller.read_i32(256 + 8), 1, "st_nlink");
        assert_eq!(caller.read_i32(256 + 24), 10, "st_size, low half");
        assert_eq!(caller.read_i32(256 + 28), 0, "and its high half");
        assert_eq!(caller.read_i32(256 + 32), 4096, "st_blksize");
        assert_eq!(caller.read_i32(256 + 36), 1, "st_blocks, rounded up");
        assert!(caller.read_i32(256 + 88) > 0, "st_ino");
        assert_eq!(caller.read_i32(256 + 40), 0, "and no invented timestamps");
        // Every one of the 96 bytes was written, so nothing of the guest's
        // remains: 0xEE anywhere would be a field this does not know about.
        let written = caller.bytes(256, 96);
        assert!(written.iter().all(|byte| *byte != 0xEE), "{written:02x?}");
    }

    #[test]
    fn a_directory_is_anything_with_something_under_it() {
        let mut wasi = Wasi::default();
        wasi.add_file("/data/file", b"x");
        let mut memory = memory();
        let mut caller = rt::Caller {
            memory: &mut memory,
        };
        caller.write(64, b"/data\0");
        assert_eq!(wasi.stat_path(&mut caller, 64, 256), errno::SUCCESS);
        assert_eq!(caller.read_i32(256 + 4), 0o040_755, "S_IFDIR");
        assert!(wasi.is_directory("/"));
        assert!(wasi.is_directory("/data/"));
        assert!(!wasi.is_directory("/dat"));

        caller.write(64, b"/missing\0");
        assert_eq!(wasi.stat_path(&mut caller, 64, 256), -errno::NOENT);
    }

    #[test]
    fn stat_by_descriptor_answers_for_each_kind() {
        let mut wasi = Wasi::default();
        wasi.add_file("/f", b"abc");
        let fd = wasi.openat("/f", false, false);
        let mut memory = memory();
        let mut caller = rt::Caller {
            memory: &mut memory,
        };

        assert_eq!(wasi.stat_fd(&mut caller, fd, 256), errno::SUCCESS);
        assert_eq!(caller.read_i32(256 + 24), 3);

        // A standard stream is a character device, and calling it a regular
        // file is what makes an `isatty` come back wrong.
        assert_eq!(wasi.stat_fd(&mut caller, 1, 256), errno::SUCCESS);
        assert_eq!(caller.read_i32(256 + 4), 0o020_620);

        assert_eq!(wasi.stat_fd(&mut caller, 3, 256), errno::SUCCESS);
        assert_eq!(caller.read_i32(256 + 4), 0o040_755, "the preopened root");

        assert_eq!(wasi.stat_fd(&mut caller, 99, 256), -errno::BADF);

        // A file that went away underneath an open descriptor.
        wasi.files.remove("/f");
        assert_eq!(wasi.stat_fd(&mut caller, fd, 256), -errno::NOENT);
    }

    #[test]
    fn two_files_have_two_inodes_and_the_same_file_has_one() {
        let mut wasi = Wasi::default();
        wasi.add_file("/a", b"x");
        wasi.add_file("/b", b"y");
        let mut memory = memory();
        let mut caller = rt::Caller {
            memory: &mut memory,
        };
        caller.write(64, b"/a\0");
        caller.write(96, b"/b\0");
        wasi.stat_path(&mut caller, 64, 256);
        let first = caller.read_i32(256 + 88);
        wasi.stat_path(&mut caller, 96, 256);
        let second = caller.read_i32(256 + 88);
        assert_ne!(first, second, "two files are not one file");
        wasi.stat_path(&mut caller, 64, 256);
        assert_eq!(caller.read_i32(256 + 88), first, "and one file is one file");
    }

    #[test]
    fn a_default_is_counted_the_first_time_and_every_time() {
        let mut unanswered = Unanswered::default();
        assert!(unanswered.is_empty());
        assert_eq!(unanswered.report(), "nothing was answered with a default\n");

        unanswered.record("env::tell", &[7, 8]);
        unanswered.record("env::ask", &[3]);
        // The arguments kept are the *first* call's: the tenth is rarely the
        // one that explains anything.
        unanswered.record("env::tell", &[99, 99]);

        assert!(!unanswered.is_empty());
        assert_eq!(unanswered.calls["env::tell"], (2, vec![7, 8]));
        let report = unanswered.report();
        assert!(
            report.starts_with("2 imports were answered with a default"),
            "{report}"
        );
        // In the order they were reached, which is usually the story.
        let tell = report.find("env::tell").expect("present");
        let ask = report.find("env::ask").expect("present");
        assert!(tell < ask, "{report}");
        assert!(report.contains("env::tell  2x, first (7, 8)"), "{report}");
        assert!(report.contains("env::ask  1x, first (3)"), "{report}");
    }

    #[test]
    fn exiting_carries_the_status() {
        let result = std::panic::catch_unwind(|| exit(3));
        let payload = result.expect_err("exit does not return");
        assert_eq!(payload.downcast_ref::<Exit>(), Some(&Exit { code: 3 }));
    }
}
