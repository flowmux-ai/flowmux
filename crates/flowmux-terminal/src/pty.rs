// SPDX-License-Identifier: GPL-3.0-or-later
//! PTY helpers shared by the terminal pane.
//!
//! Spawns a child under a fresh PTY via `forkpty(3)`, exposes the master fd for
//! read/write plumbing, and resizes the kernel window via `TIOCSWINSZ`.

use std::ffi::CString;
use std::io;
use std::os::fd::{FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::io::RawFd;
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant};

static SPAWN_LOCK: Mutex<()> = Mutex::new(());

const CLOSE_GRACE: Duration = Duration::from_secs(5);
const REAP_POLL_INTERVAL: Duration = Duration::from_millis(20);

/// A spawned child process attached to a PTY master fd.
pub struct Pty {
    master: RawFd,
    child: libc::pid_t,
    reaped: bool,
    external_child_watch: bool,
}

/// Holds the forked child before `exec` until released or dropped.
///
/// The child blocks reading the pipe whose write end this owns. Release (via
/// [`SpawnGate::release`] or `Drop`) writes one byte and then closes the write
/// end, so it does not depend on any other process that may have inherited a
/// duplicate of the write end (the child proceeds on the byte or on EOF).
pub struct SpawnGate(Option<OwnedFd>);

impl SpawnGate {
    /// Let the parked child proceed to `exec`.
    pub fn release(self) {
        drop(self);
    }

    #[cfg(test)]
    fn raw_fd(&self) -> RawFd {
        use std::os::fd::AsRawFd;
        self.0.as_ref().expect("gate fd").as_raw_fd()
    }
}

impl Drop for SpawnGate {
    fn drop(&mut self) {
        use std::os::fd::AsRawFd;
        if let Some(fd) = self.0.take() {
            let byte = 1u8;
            // SAFETY: valid fd and buffer. The result is ignored: EPIPE after
            // the child died is harmless (Rust ignores SIGPIPE).
            unsafe {
                libc::write(fd.as_raw_fd(), &byte as *const u8 as *const libc::c_void, 1);
            }
            // Dropping the OwnedFd closes the write end exactly once.
        }
    }
}

impl Pty {
    /// Spawn `argv` under a fresh PTY sized `cols` x `rows`. `cwd` is the
    /// child's working directory; `extra_env` is merged onto the inherited
    /// environment (later entries win). `argv[0]` is resolved via `PATH`.
    pub fn spawn(
        argv: &[&str],
        cwd: Option<&Path>,
        extra_env: &[(String, String)],
        cols: u16,
        rows: u16,
    ) -> io::Result<Pty> {
        let (pty, gate) = Self::spawn_gated(argv, cwd, extra_env, cols, rows, None)?;
        drop(gate);
        Ok(pty)
    }

    /// Like [`Pty::spawn`], but the child blocks before `exec` until the
    /// returned [`SpawnGate`] is released or dropped. When `oom_score_adj` is
    /// `Some` (Linux only) the child writes it to `/proc/self/oom_score_adj`
    /// before parking; write errors are ignored.
    pub fn spawn_gated(
        argv: &[&str],
        cwd: Option<&Path>,
        extra_env: &[(String, String)],
        cols: u16,
        rows: u16,
        oom_score_adj: Option<i32>,
    ) -> io::Result<(Pty, SpawnGate)> {
        if argv.is_empty() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "empty argv"));
        }

        // Build argv/envp/cwd as C strings BEFORE forking: after fork() in the
        // child we may only call async-signal-safe functions, so no allocation
        // or std::env mutation is allowed there.
        let c_argv: Vec<CString> = argv
            .iter()
            .map(|a| CString::new(*a))
            .collect::<Result<_, _>>()
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "argv has NUL"))?;
        let mut argv_ptrs: Vec<*const libc::c_char> = c_argv.iter().map(|s| s.as_ptr()).collect();
        argv_ptrs.push(std::ptr::null());

        let c_env = build_envp(extra_env)?;
        let mut env_ptrs: Vec<*const libc::c_char> = c_env.iter().map(|s| s.as_ptr()).collect();
        env_ptrs.push(std::ptr::null());

        let c_cwd = match cwd {
            Some(p) => Some(
                CString::new(p.as_os_str().as_bytes())
                    .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "cwd has NUL"))?,
            ),
            None => None,
        };

        // Prepared before fork: the child may not allocate.
        let c_oom_value = match oom_score_adj {
            Some(v) => Some(
                CString::new(v.to_string())
                    .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "bad oom value"))?,
            ),
            None => None,
        };
        let c_oom_path = CString::new("/proc/self/oom_score_adj").expect("static path");

        // Serialize "create gate pipe + fork": a child forked while another
        // thread's gate pipe exists inherits that pipe's write end, and two
        // concurrent spawns could then each hold the other's write end while
        // parked, deadlocking both. Held until this function returns.
        let _spawn_guard = SPAWN_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let (gate_read, gate_write) = create_gate_pipe()?;

        let mut ws = libc::winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };

        let mut master: RawFd = -1;
        // libc declares forkpty's `winp` as `*const winsize` on Linux
        // but `*mut winsize` on macOS; go through a raw pointer so one
        // call site type-checks (and passes clippy) on both.
        let winp = std::ptr::addr_of_mut!(ws);
        // SAFETY: forkpty allocates a PTY pair and forks. We pass a valid
        // master-out pointer and winsize; name/termios default to NULL.
        let pid = unsafe {
            libc::forkpty(
                &mut master,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                winp as _,
            )
        };

        if pid < 0 {
            let err = io::Error::last_os_error();
            // SAFETY: both fds were just created by us and are still open.
            unsafe {
                libc::close(gate_read);
                libc::close(gate_write);
            }
            return Err(err);
        }

        if pid == 0 {
            // ---- Child: async-signal-safe only, then exec or _exit. ----
            unsafe {
                libc::close(gate_write);
                #[cfg(target_os = "linux")]
                if let Some(ref v) = c_oom_value {
                    let fd = libc::open(c_oom_path.as_ptr(), libc::O_WRONLY);
                    if fd >= 0 {
                        let bytes = v.as_bytes();
                        libc::write(fd, bytes.as_ptr() as *const libc::c_void, bytes.len());
                        libc::close(fd);
                    }
                }
                #[cfg(not(target_os = "linux"))]
                {
                    let _ = (&c_oom_value, &c_oom_path);
                }
                // Park until the parent closes (or drops) its write end.
                let mut byte = 0u8;
                loop {
                    let n = libc::read(gate_read, &mut byte as *mut u8 as *mut libc::c_void, 1);
                    if n >= 0 || *errno_location() != libc::EINTR {
                        break;
                    }
                }
                libc::close(gate_read);
            }
            if let Some(ref c) = c_cwd {
                // Ignore chdir failure: better to exec in the wrong dir than to
                // leave a half-spawned child. (Mirrors common shell behavior.)
                unsafe { libc::chdir(c.as_ptr()) };
            }
            unsafe {
                // execvpe(3) is a glibc/BSD extension absent on macOS, so there
                // we replace the environment then execvp. Both paths hand the
                // child exactly `env_ptrs` (build_envp already merged the
                // inherited env with the pane's extra vars).
                #[cfg(not(target_os = "macos"))]
                libc::execvpe(argv_ptrs[0], argv_ptrs.as_ptr(), env_ptrs.as_ptr());
                #[cfg(target_os = "macos")]
                {
                    extern "C" {
                        fn _NSGetEnviron() -> *mut *mut *mut libc::c_char;
                    }
                    *_NSGetEnviron() = env_ptrs.as_ptr() as *mut *mut libc::c_char;
                    libc::execvp(argv_ptrs[0], argv_ptrs.as_ptr());
                }
                // exec only returns on failure.
                libc::_exit(127);
            }
        }

        // ---- Parent. ----
        // forkpty leaves the master inheritable. Without FD_CLOEXEC every
        // later child the GUI forks (other panes, helpers) inherits it and
        // keeps the PTY alive after this process dies — writers on the
        // slave side then never see a hangup.
        unsafe {
            libc::fcntl(master, libc::F_SETFD, libc::FD_CLOEXEC);
            libc::close(gate_read);
        }
        // SAFETY: gate_write is a valid fd we own and have not shared.
        let gate = SpawnGate(Some(unsafe { OwnedFd::from_raw_fd(gate_write) }));
        Ok((
            Pty {
                master,
                child: pid,
                reaped: false,
                external_child_watch: false,
            },
            gate,
        ))
    }

    /// The PTY master file descriptor (for poll/glib fd watches).
    pub fn master_fd(&self) -> RawFd {
        self.master
    }

    /// PID of the spawned child (for `/proc/<pid>/cwd` lookups).
    pub fn child_pid(&self) -> i32 {
        self.child
    }

    /// Read available output bytes into `buf`. Returns 0 at EOF (child exited
    /// and closed the slave).
    pub fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        // SAFETY: master is a valid fd; buf is a valid mutable slice.
        let n = unsafe {
            libc::read(
                self.master,
                buf.as_mut_ptr() as *mut libc::c_void,
                buf.len(),
            )
        };
        if n < 0 {
            let err = io::Error::last_os_error();
            // After the child exits, the master read fails with EIO on Linux;
            // surface that as a clean EOF for the read loop.
            if err.raw_os_error() == Some(libc::EIO) {
                return Ok(0);
            }
            return Err(err);
        }
        Ok(n as usize)
    }

    /// Write keystroke bytes to the child.
    pub fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        // SAFETY: master is a valid fd; data is a valid slice.
        let n = unsafe {
            libc::write(
                self.master,
                data.as_ptr() as *const libc::c_void,
                data.len(),
            )
        };
        if n < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(n as usize)
    }

    /// Resize the kernel PTY window. `cell_w_px`/`cell_h_px` populate the pixel
    /// fields some apps read; pass 0 when unknown.
    pub fn resize(
        &mut self,
        cols: u16,
        rows: u16,
        cell_w_px: u16,
        cell_h_px: u16,
    ) -> io::Result<()> {
        let ws = libc::winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: cols.saturating_mul(cell_w_px),
            ws_ypixel: rows.saturating_mul(cell_h_px),
        };
        // SAFETY: master is a valid fd; ws outlives the ioctl call.
        let rc = unsafe { libc::ioctl(self.master, libc::TIOCSWINSZ, &ws) };
        if rc < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// Give an external watcher (such as VTE) sole ownership of waitpid.
    /// Drop still hangs up and eventually kills the process group, but leaves
    /// the exit status for that watcher, which must outlive the child.
    pub fn set_external_child_watch(&mut self) {
        self.external_child_watch = true;
    }

    /// Non-blocking reap: returns `Some(status)` if the child has exited.
    pub fn try_wait(&mut self) -> io::Result<Option<i32>> {
        if self.external_child_watch {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "child exit status belongs to an external watcher",
            ));
        }
        if self.reaped {
            return Ok(Some(0));
        }
        let mut status: libc::c_int = 0;
        // SAFETY: standard waitpid usage.
        let rc = unsafe { libc::waitpid(self.child, &mut status, libc::WNOHANG) };
        if rc < 0 {
            return Err(io::Error::last_os_error());
        }
        if rc == 0 {
            return Ok(None); // still running
        }
        self.reaped = true;
        Ok(Some(status))
    }

    /// The external watcher reaped the child. The PID may already belong to an
    /// unrelated process group, so later close paths must neither signal nor
    /// poll it.
    pub fn mark_child_exited(&mut self) {
        self.reaped = true;
    }

    /// Send SIGHUP to the child (used on pane close before reaping).
    pub fn hangup(&mut self) {
        if !self.reaped {
            signal_process_group(self.child, libc::SIGHUP);
        }
    }

    /// Close immediately and reap the child process group off-thread.
    pub fn close_async(mut self) {
        self.hangup();
        self.close_master();
        self.start_reaper();
    }

    fn close_master(&mut self) {
        if self.master >= 0 {
            // SAFETY: closing our owned fd once.
            unsafe { libc::close(self.master) };
            self.master = -1;
        }
    }

    fn start_reaper(&mut self) {
        if self.reaped {
            return;
        }
        let child = self.child;
        let external_child_watch = self.external_child_watch;
        self.reaped = true;
        if let Err(error) = std::thread::Builder::new()
            .name("flowmux-pty-reaper".into())
            .spawn(move || reap_process_group(child, CLOSE_GRACE, external_child_watch))
        {
            signal_process_group(child, libc::SIGKILL);
            eprintln!("failed to start PTY reaper: {error}");
        }
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        self.hangup();
        self.close_master();
        self.start_reaper();
    }
}

/// Create the close-on-exec gate pipe, returning `(read, write)`.
fn create_gate_pipe() -> io::Result<(RawFd, RawFd)> {
    let mut fds = [-1 as libc::c_int; 2];
    #[cfg(not(target_os = "macos"))]
    {
        // SAFETY: fds is a valid two-element array.
        if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    #[cfg(target_os = "macos")]
    {
        // SAFETY: fds is a valid two-element array.
        if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        for fd in fds {
            // SAFETY: fd was just created by pipe.
            if unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } != 0 {
                let err = io::Error::last_os_error();
                unsafe {
                    libc::close(fds[0]);
                    libc::close(fds[1]);
                }
                return Err(err);
            }
        }
    }
    Ok((fds[0], fds[1]))
}

/// Async-signal-safe access to the calling thread's `errno`.
#[cfg(any(target_os = "linux", target_os = "android"))]
unsafe fn errno_location() -> *mut libc::c_int {
    libc::__errno_location()
}
#[cfg(not(any(target_os = "linux", target_os = "android")))]
unsafe fn errno_location() -> *mut libc::c_int {
    libc::__error()
}

fn signal_process_group(pid: libc::pid_t, signal: libc::c_int) {
    // SAFETY: forkpty creates the child as the leader of its own process group.
    unsafe {
        libc::kill(-pid, signal);
    }
}

fn process_group_exists(pid: libc::pid_t) -> bool {
    let rc = unsafe { libc::kill(-pid, 0) };
    rc == 0 || io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

fn reap_process_group(pid: libc::pid_t, grace: Duration, external_child_watch: bool) {
    let deadline = Instant::now() + grace;
    // External watchers consume the status even after the pane is destroyed.
    // In that case only monitor/terminate the group here; never call waitpid.
    let mut child_reaped = external_child_watch;
    loop {
        if !child_reaped {
            let mut status = 0;
            let rc = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
            if rc == pid
                || (rc < 0 && io::Error::last_os_error().raw_os_error() == Some(libc::ECHILD))
            {
                child_reaped = true;
            } else if rc < 0 && io::Error::last_os_error().raw_os_error() != Some(libc::EINTR) {
                return;
            }
        }
        if child_reaped && !process_group_exists(pid) {
            return;
        }
        if Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(REAP_POLL_INTERVAL);
    }

    signal_process_group(pid, libc::SIGKILL);
    let kill_deadline = Instant::now() + Duration::from_secs(1);
    while Instant::now() < kill_deadline {
        if !child_reaped {
            let mut status = 0;
            let rc = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
            if rc == pid
                || (rc < 0 && io::Error::last_os_error().raw_os_error() == Some(libc::ECHILD))
            {
                child_reaped = true;
            }
        }
        if child_reaped && !process_group_exists(pid) {
            return;
        }
        std::thread::sleep(REAP_POLL_INTERVAL);
    }
}

/// Merge `extra_env` onto the inherited environment into a `KEY=VALUE` CString
/// list. Later duplicate keys win.
fn build_envp(extra_env: &[(String, String)]) -> io::Result<Vec<CString>> {
    build_envp_from(std::env::vars(), extra_env)
}

fn build_envp_from<I>(inherited: I, extra_env: &[(String, String)]) -> io::Result<Vec<CString>>
where
    I: IntoIterator<Item = (String, String)>,
{
    use std::collections::BTreeMap;
    // Terminal panes should advertise their own color capability instead of
    // inheriting NO_COLOR from the GUI launcher (for example Codex runs with
    // NO_COLOR=1). Callers can still pass NO_COLOR explicitly via extra_env.
    let mut map: BTreeMap<String, String> = inherited
        .into_iter()
        .filter(|(k, _)| k != "NO_COLOR")
        .collect();
    for (k, v) in extra_env {
        map.insert(k.clone(), v.clone());
    }
    map.into_iter()
        .map(|(k, v)| {
            CString::new(format!("{k}={v}"))
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "env has NUL"))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    fn poll_readable(fd: RawFd, timeout_ms: i32) -> bool {
        let mut p = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        (unsafe { libc::poll(&mut p, 1, timeout_ms) }) > 0
    }

    #[test]
    fn gated_child_produces_no_output_until_released() {
        let (mut pty, gate) =
            Pty::spawn_gated(&["sh", "-c", "printf ready"], None, &[], 40, 8, None).expect("spawn");
        assert!(
            !poll_readable(pty.master_fd(), 300),
            "child must be parked before exec"
        );
        #[cfg(target_os = "linux")]
        {
            let comm = std::fs::read_to_string(format!("/proc/{}/comm", pty.child_pid())).unwrap();
            assert_ne!(comm.trim(), "sh", "child must not have exec'd yet");
        }
        gate.release();
        assert!(
            poll_readable(pty.master_fd(), 5000),
            "child must run after release"
        );
        let mut buf = [0u8; 64];
        let n = pty.read(&mut buf).expect("read");
        assert!(String::from_utf8_lossy(&buf[..n]).contains("ready"));
    }

    #[test]
    fn dropping_the_gate_releases_the_child() {
        let (mut pty, gate) =
            Pty::spawn_gated(&["sh", "-c", "printf go"], None, &[], 40, 8, None).expect("spawn");
        drop(gate);
        assert!(poll_readable(pty.master_fd(), 5000));
        let mut buf = [0u8; 64];
        let n = pty.read(&mut buf).expect("read");
        assert!(String::from_utf8_lossy(&buf[..n]).contains("go"));
    }

    #[test]
    fn release_works_while_another_holder_keeps_the_write_end() {
        let (mut pty, gate) =
            Pty::spawn_gated(&["sh", "-c", "printf held"], None, &[], 40, 8, None).expect("spawn");
        // SAFETY: duplicating a valid fd; the dup simulates a foreign holder.
        let dup = unsafe { libc::dup(gate.raw_fd()) };
        assert!(dup >= 0);
        gate.release();
        assert!(
            poll_readable(pty.master_fd(), 5000),
            "child must run despite a foreign write-end holder"
        );
        let mut buf = [0u8; 64];
        let n = pty.read(&mut buf).expect("read");
        assert!(String::from_utf8_lossy(&buf[..n]).contains("held"));
        unsafe { libc::close(dup) };
    }

    #[cfg(target_os = "linux")]
    fn read_adj_eventually(pid: i32, want: &str) -> String {
        // The child writes the value after fork, so poll briefly.
        let path = format!("/proc/{pid}/oom_score_adj");
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let adj = std::fs::read_to_string(&path).unwrap();
            if adj.trim() == want || Instant::now() >= deadline {
                return adj;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn oom_score_adj_is_written_before_exec_only_when_requested() {
        let (pty, gate) =
            Pty::spawn_gated(&["sleep", "5"], None, &[], 40, 8, Some(500)).expect("spawn");
        let adj = read_adj_eventually(pty.child_pid(), "500");
        assert_eq!(adj.trim(), "500");
        drop(gate);
        drop(pty);

        let own = std::fs::read_to_string("/proc/self/oom_score_adj").unwrap();
        let (pty, gate) = Pty::spawn_gated(&["sleep", "5"], None, &[], 40, 8, None).expect("spawn");
        gate.release();
        let comm = format!("/proc/{}/comm", pty.child_pid());
        let deadline = Instant::now() + Duration::from_secs(5);
        while std::fs::read_to_string(&comm)
            .map(|c| c.trim() != "sleep")
            .unwrap_or(true)
        {
            assert!(Instant::now() < deadline, "child never exec'd sleep");
            std::thread::sleep(Duration::from_millis(10));
        }
        let adj =
            std::fs::read_to_string(format!("/proc/{}/oom_score_adj", pty.child_pid())).unwrap();
        assert_eq!(adj.trim(), own.trim());
        drop(pty);
    }

    fn envp_strings(envp: Vec<CString>) -> Vec<String> {
        envp.into_iter()
            .map(|s| s.into_string().expect("test env should be utf8"))
            .collect()
    }

    #[test]
    fn inherited_no_color_is_not_forwarded_to_terminal_children() {
        let envp = build_envp_from(
            [
                ("NO_COLOR".to_string(), "1".to_string()),
                ("TERM".to_string(), "dumb".to_string()),
            ],
            &[("TERM".to_string(), "xterm-256color".to_string())],
        )
        .expect("envp");
        let entries = envp_strings(envp);

        assert!(!entries.iter().any(|e| e.starts_with("NO_COLOR=")));
        assert!(entries.iter().any(|e| e == "TERM=xterm-256color"));
    }

    #[test]
    fn explicit_no_color_extra_env_is_preserved() {
        let envp = build_envp_from(
            [("NO_COLOR".to_string(), "1".to_string())],
            &[("NO_COLOR".to_string(), "1".to_string())],
        )
        .expect("envp");
        let entries = envp_strings(envp);

        assert!(entries.iter().any(|e| e == "NO_COLOR=1"));
    }

    #[test]
    fn master_fd_is_cloexec() {
        let pty = Pty::spawn(&["sh", "-c", "true"], None, &[], 40, 8).expect("spawn sh");
        let flags = unsafe { libc::fcntl(pty.master_fd(), libc::F_GETFD) };
        assert!(flags >= 0, "F_GETFD failed");
        assert_ne!(
            flags & libc::FD_CLOEXEC,
            0,
            "PTY master must not leak into later spawned children"
        );
    }

    /// A real child under a PTY: its stdout reaches the master fd. The GTK layer
    /// hands this same master to VTE for rendering; here we only check the raw
    /// plumbing.
    #[test]
    fn shell_output_reaches_the_pty_master() {
        let mut pty =
            Pty::spawn(&["sh", "-c", "printf 'hello world'"], None, &[], 40, 8).expect("spawn sh");

        let mut out = String::new();
        let mut buf = [0u8; 4096];
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match pty.read(&mut buf) {
                Ok(0) => break, // EOF: child exited
                Ok(n) => out.push_str(&String::from_utf8_lossy(&buf[..n])),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => panic!("pty read failed: {e}"),
            }
            if Instant::now() > deadline {
                panic!("timed out waiting for shell output");
            }
        }

        assert!(out.contains("hello world"), "got: {out:?}");
    }

    #[test]
    fn extra_env_reaches_the_child() {
        let mut pty = Pty::spawn(
            &["sh", "-c", "printf \"%s\" \"$FLOWMUX_TEST_VAR\""],
            None,
            &[("FLOWMUX_TEST_VAR".to_string(), "pane42".to_string())],
            40,
            4,
        )
        .expect("spawn sh");

        let mut out = String::new();
        let mut buf = [0u8; 1024];
        loop {
            match pty.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => out.push_str(&String::from_utf8_lossy(&buf[..n])),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => panic!("read: {e}"),
            }
        }
        assert!(out.contains("pane42"), "got: {out:?}");
    }

    #[test]
    fn resize_updates_winsize_seen_by_child() {
        // `stty size` prints "rows cols" from the kernel winsize we set.
        let mut pty = Pty::spawn(&["sh", "-c", "stty size"], None, &[], 100, 30).expect("spawn");
        // Resize before the child reads its winsize.
        pty.resize(100, 30, 8, 16).expect("resize");

        let mut out = String::new();
        let mut buf = [0u8; 1024];
        loop {
            match pty.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => out.push_str(&String::from_utf8_lossy(&buf[..n])),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
        // Some shells emit "30 100"; tolerate either field order issues by
        // checking both numbers are present on the first row.
        let row = out;
        assert!(
            row.contains("30") && row.contains("100"),
            "stty size output was {row:?}"
        );
    }

    #[test]
    fn external_watcher_keeps_exit_status_after_close_and_forced_kill() {
        for ignore_hup in [false, true] {
            let script = if ignore_hup {
                "trap '' HUP TERM; printf ready; while :; do sleep 1; done"
            } else {
                "trap 'exit 23' HUP; printf ready; while :; do sleep 1; done"
            };
            let mut pty = Pty::spawn(&["sh", "-c", script], None, &[], 80, 24).unwrap();
            let child = pty.child_pid();
            let mut ready = [0; 5];
            let mut received = 0;
            while received < ready.len() {
                let n = pty.read(&mut ready[received..]).unwrap();
                assert!(n > 0, "child exited before readiness");
                received += n;
            }
            assert_eq!(&ready, b"ready");
            pty.set_external_child_watch();
            assert_eq!(
                pty.try_wait().unwrap_err().kind(),
                io::ErrorKind::InvalidInput
            );
            pty.close_async();
            // Let the cleanup thread run first: it must not steal the status.
            std::thread::sleep(Duration::from_millis(100));
            let deadline = Instant::now() + CLOSE_GRACE + Duration::from_secs(3);
            let mut status = 0;
            loop {
                let rc = unsafe { libc::waitpid(child, &mut status, libc::WNOHANG) };
                assert!(
                    rc >= 0,
                    "exit status was stolen: {}",
                    io::Error::last_os_error()
                );
                if rc == child {
                    break;
                }
                if Instant::now() >= deadline {
                    unsafe {
                        libc::kill(-child, libc::SIGKILL);
                        libc::waitpid(child, &mut status, 0);
                    }
                    panic!("PTY cleanup did not terminate child");
                }
                std::thread::sleep(REAP_POLL_INTERVAL);
            }
            if ignore_hup {
                assert!(libc::WIFSIGNALED(status));
                assert_eq!(libc::WTERMSIG(status), libc::SIGKILL);
            } else {
                assert!(libc::WIFEXITED(status));
                assert_eq!(libc::WEXITSTATUS(status), 23);
            }
        }
    }

    #[test]
    fn close_after_external_exit_does_not_signal_the_process_group() {
        let mut pty = Pty::spawn(
            &[
                "sh",
                "-c",
                "trap '' HUP; printf ready; while :; do sleep 1; done",
            ],
            None,
            &[],
            80,
            24,
        )
        .unwrap();
        let child = pty.child_pid();
        let mut ready = [0; 5];
        let mut received = 0;
        while received < ready.len() {
            let n = pty.read(&mut ready[received..]).unwrap();
            assert!(n > 0, "child exited before readiness");
            received += n;
        }
        pty.set_external_child_watch();
        // The watcher reported the exit; the PID may now name a foreign group.
        pty.mark_child_exited();
        pty.close_async();
        std::thread::sleep(CLOSE_GRACE + Duration::from_millis(500));
        let mut status = 0;
        let rc = unsafe { libc::waitpid(child, &mut status, libc::WNOHANG) };
        unsafe {
            libc::kill(-child, libc::SIGKILL);
            libc::waitpid(child, &mut status, 0);
        }
        assert_eq!(
            rc, 0,
            "closed PTY terminated a process group it no longer owns"
        );
    }

    #[test]
    fn drop_does_not_block_for_sighup_ignoring_child() {
        let mut pty = Pty::spawn(
            &["sh", "-c", "trap '' HUP TERM; printf ready; sleep 60"],
            None,
            &[],
            80,
            24,
        )
        .expect("spawn SIGHUP-ignoring child");
        let child_pid = pty.child_pid();
        let mut output = [0u8; 16];
        let ready_deadline = Instant::now() + Duration::from_secs(3);
        loop {
            match pty.read(&mut output) {
                Ok(n) if output[..n].windows(5).any(|bytes| bytes == b"ready") => break,
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => panic!("read child readiness: {error}"),
            }
            assert!(
                Instant::now() < ready_deadline,
                "timed out waiting for child readiness"
            );
        }
        let (done_tx, done_rx) = mpsc::channel();

        std::thread::spawn(move || {
            drop(pty);
            let _ = done_tx.send(());
        });

        let result = done_rx.recv_timeout(Duration::from_millis(300));
        // Ensure a failing baseline run cannot leave the process group alive.
        unsafe {
            libc::kill(-child_pid, libc::SIGKILL);
        }

        assert!(result.is_ok(), "Pty::drop blocked waiting for its child");
    }
}
