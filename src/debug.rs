//! Temporary verbose logging for tracking down the crash when going back from the
//! review page to the PR list (the app dies with "Terminated", i.e. SIGTERM).
//!
//! Everything goes to stderr with a `[debug +<ms>]` prefix, so
//! `cargo run 2>&1 | tee debug.log` captures it all. Remove this module once the
//! cause is found.

use std::sync::OnceLock;
use std::time::Instant;

static START: OnceLock<Instant> = OnceLock::new();

/// Milliseconds since the app started.
pub fn elapsed_ms() -> u128 {
    START.get_or_init(Instant::now).elapsed().as_millis()
}

macro_rules! dlog {
    ($($arg:tt)*) => {
        eprintln!("[debug +{}ms] {}", $crate::debug::elapsed_ms(), format!($($arg)*))
    };
}
pub(crate) use dlog;

/// This process's pid, process group and session, for comparing against children.
pub fn self_ids() -> String {
    // SAFETY: these calls have no preconditions.
    unsafe { format!("pid={} pgid={} sid={}", libc::getpid(), libc::getpgrp(), libc::getsid(0)) }
}

/// The process group of `pid`, or the error if it can't be read (e.g. it has exited).
pub fn pgid_of(pid: u32) -> String {
    // SAFETY: getpgid has no preconditions; a bad pid just returns -1.
    let g = unsafe { libc::getpgid(pid as libc::pid_t) };
    if g < 0 { format!("? ({})", std::io::Error::last_os_error()) } else { g.to_string() }
}

/// Logs a message when dropped: shows when a subscription's stream is torn down.
pub struct DropLog(pub String);

impl Drop for DropLog {
    fn drop(&mut self) {
        dlog!("dropped: {}", self.0);
    }
}

/// Installs handlers for SIGTERM, SIGINT and SIGHUP that print the signal, the pid
/// and uid that sent it, and that pid's command line and parent pid. The handler
/// then restores the default action and re-raises the signal, so the app still
/// dies exactly as before.
pub fn install_signal_logging() {
    START.get_or_init(Instant::now);
    dlog!("app started: {}", self_ids());
    for sig in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
        // SAFETY: the handler only calls async-signal-safe functions (open, read,
        // write, close, raise) and works on stack buffers.
        unsafe {
            let mut sa: libc::sigaction = std::mem::zeroed();
            sa.sa_sigaction = on_signal as extern "C" fn(libc::c_int, *mut libc::siginfo_t, *mut libc::c_void) as usize;
            sa.sa_flags = libc::SA_SIGINFO | libc::SA_RESETHAND;
            libc::sigemptyset(&mut sa.sa_mask);
            libc::sigaction(sig, &sa, std::ptr::null_mut());
        }
    }
}

extern "C" fn on_signal(sig: libc::c_int, info: *mut libc::siginfo_t, _ctx: *mut libc::c_void) {
    // SAFETY: the kernel passes a valid siginfo_t because of SA_SIGINFO.
    let (pid, uid) = unsafe { ((*info).si_pid(), (*info).si_uid()) };
    let mut buf = Buf::new();
    buf.push(b"\n[debug] !!! received signal ");
    buf.num(sig as i64);
    buf.push(b" from pid ");
    buf.num(pid as i64);
    buf.push(b" (uid ");
    buf.num(uid as i64);
    buf.push(b"); our pid ");
    // SAFETY: getpid/getpgrp are async-signal-safe.
    buf.num(unsafe { libc::getpid() } as i64);
    buf.push(b", our pgid ");
    buf.num(unsafe { libc::getpgrp() } as i64);
    buf.push(b"\n[debug] !!! sender cmdline: ");
    buf.proc_file(pid, b"cmdline");
    buf.push(b"\n[debug] !!! sender stat: ");
    buf.proc_file(pid, b"stat");
    buf.push(b"\n");
    buf.write_stderr();
    // SA_RESETHAND has restored the default action; the signal is blocked while
    // the handler runs, so it is delivered (and kills us) right after we return.
    // SAFETY: raise is async-signal-safe.
    unsafe { libc::raise(sig) };
}

/// A fixed-size stack buffer, since the signal handler can't allocate.
struct Buf {
    data: [u8; 2048],
    len: usize,
}

impl Buf {
    fn new() -> Self {
        Buf { data: [0; 2048], len: 0 }
    }

    fn push(&mut self, s: &[u8]) {
        for &b in s {
            if self.len < self.data.len() {
                self.data[self.len] = b;
                self.len += 1;
            }
        }
    }

    fn num(&mut self, mut n: i64) {
        if n < 0 {
            self.push(b"-");
            n = -n;
        }
        let mut digits = [0u8; 20];
        let mut i = digits.len();
        loop {
            i -= 1;
            digits[i] = b'0' + (n % 10) as u8;
            n /= 10;
            if n == 0 {
                break;
            }
        }
        self.push(&digits[i..]);
    }

    /// Appends /proc/<pid>/<name>, with NULs shown as spaces.
    fn proc_file(&mut self, pid: libc::pid_t, name: &[u8]) {
        let mut path = Buf::new();
        path.push(b"/proc/");
        path.num(pid as i64);
        path.push(b"/");
        path.push(name);
        path.push(b"\0");
        let mut tmp = [0u8; 512];
        // SAFETY: path is NUL-terminated; open/read/close are async-signal-safe.
        let n = unsafe {
            let fd = libc::open(path.data.as_ptr() as *const libc::c_char, libc::O_RDONLY);
            if fd < 0 {
                self.push(b"<gone>");
                return;
            }
            let n = libc::read(fd, tmp.as_mut_ptr() as *mut libc::c_void, tmp.len());
            libc::close(fd);
            n
        };
        for &b in &tmp[..n.max(0) as usize] {
            self.push(if b == 0 { b" " } else { std::slice::from_ref(&b) });
        }
    }

    fn write_stderr(&self) {
        // SAFETY: write is async-signal-safe.
        unsafe { libc::write(2, self.data.as_ptr() as *const libc::c_void, self.len) };
    }
}
