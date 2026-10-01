//! The single-owner claim on a data directory (Issue #2905).
//!
//! AletheiaDB is an embedded, **single-writer** store. When a daemon owns a data
//! directory, any other process that opens it becomes an unsupported second
//! writer — the exact configuration daemon mode exists to retire. This module is
//! the one place that claim is written and read, so the daemon, the CLI, the MCP
//! server, and the HTTP server cannot drift into disagreeing about who owns
//! what.
//!
//! # What the lock is, and is not
//!
//! [`DaemonLock::acquire`] creates `{data_dir}/daemon.lock` **exclusively**
//! (`O_CREAT|O_EXCL`) containing the owning process id. Exclusive creation is
//! what makes two daemons racing to claim the same directory resolve to one
//! winner rather than both believing they won, and it means the path is never
//! opened through a pre-existing symlink.
//!
//! A lock left behind by a **crashed** daemon is reclaimed: the recorded pid is
//! probed for liveness and, if dead, the stale file is removed and the claim
//! retried. Liveness — not mere file existence — is the test, so an unclean
//! shutdown never permanently bricks a data directory.
//!
//! It is *not* an OS-enforced mutual-exclusion primitive. A process that ignores
//! this module entirely can still open the directory. The lock defends against
//! accident (two daemons, a CLI command run while the daemon is up), not against
//! a hostile local process — which, having write access to the data directory,
//! could corrupt it directly anyway.

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

/// Name of the lock file inside a data directory.
pub const LOCK_FILE_NAME: &str = "daemon.lock";

/// How many times [`DaemonLock::acquire`] retries after reclaiming a stale lock.
///
/// One retry is the expected path (reclaim, then claim). The extra attempts
/// cover a second process reclaiming the same stale lock concurrently; a
/// persistent conflict means a live owner, which is reported rather than
/// retried forever.
const MAX_ACQUIRE_ATTEMPTS: usize = 3;

/// An exclusive claim on a data directory, released when dropped.
#[derive(Debug)]
pub struct DaemonLock {
    path: PathBuf,
    pid: u32,
}

impl DaemonLock {
    /// Claim `{data_dir}/daemon.lock` for the current process.
    ///
    /// # Errors
    ///
    /// Returns a human-readable message naming the owning pid when another live
    /// process holds the directory, or describing the I/O failure when the lock
    /// cannot be written.
    pub fn acquire(data_dir: &Path) -> Result<Self, String> {
        fs::create_dir_all(data_dir).map_err(|e| {
            format!(
                "failed to create data directory '{}': {e}",
                data_dir.display()
            )
        })?;
        let path = data_dir.join(LOCK_FILE_NAME);
        let pid = std::process::id();

        for _ in 0..MAX_ACQUIRE_ATTEMPTS {
            match create_exclusive(&path) {
                Ok(mut file) => {
                    write!(file, "{pid}").map_err(|e| {
                        format!("failed to write lock file '{}': {e}", path.display())
                    })?;
                    file.sync_all().map_err(|e| {
                        format!("failed to flush lock file '{}': {e}", path.display())
                    })?;
                    return Ok(Self { path, pid });
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    // Someone holds it. Live owner → refuse; dead owner (or an
                    // unreadable/garbage file) → reclaim and retry.
                    if let Some(owner) = owner_of(&path) {
                        return Err(already_owned_message(data_dir, owner));
                    }
                    if let Err(e) = fs::remove_file(&path)
                        && e.kind() != std::io::ErrorKind::NotFound
                    {
                        return Err(format!(
                            "failed to clear the stale lock file '{}': {e}",
                            path.display()
                        ));
                    }
                }
                Err(e) => {
                    return Err(format!(
                        "failed to write lock file '{}': {e}",
                        path.display()
                    ));
                }
            }
        }

        // Every attempt lost the race to another process claiming the same
        // stale lock. Whoever won is the owner.
        match owner_of(&path) {
            Some(owner) => Err(already_owned_message(data_dir, owner)),
            None => Err(format!(
                "could not claim the lock file '{}': it is being created and removed \
                 concurrently by another process",
                path.display()
            )),
        }
    }

    /// The lock file's path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for DaemonLock {
    /// Release the claim — but **only** if this process still owns it.
    ///
    /// Without the ownership check, a process whose lock had been reclaimed as
    /// stale (or replaced by an operator) would delete the *current* owner's
    /// lock on its way out, silently opening the directory to a second writer.
    fn drop(&mut self) {
        if owner_of(&self.path) == Some(self.pid) {
            let _ = fs::remove_file(&self.path);
        }
    }
}

/// Create the lock file, failing if it already exists.
///
/// `create_new` maps to `O_CREAT|O_EXCL`, which the kernel fails on a path whose
/// final component is a symlink — so this cannot be redirected into truncating
/// another file. On Unix the file is created `0600`: the pid is not a secret,
/// but a world-writable lock would let any local user forge ownership.
fn create_exclusive(path: &Path) -> std::io::Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    options.open(path)
}

/// The pid of the **live** process recorded in a lock file, if any.
///
/// `None` for an absent, unreadable, malformed, or stale lock — every one of
/// which means "no live owner", so a crashed daemon never blocks startup.
///
/// A lock recording *this* process's pid counts as held, deliberately: exempting
/// it would let one process acquire the same directory twice, ending with two
/// `DaemonLock` values whose `Drop`s each release a claim the other still
/// believes it holds.
#[must_use]
pub fn owner_of(lock_path: &Path) -> Option<u32> {
    let pid = fs::read_to_string(lock_path)
        .ok()?
        .trim()
        .parse::<u32>()
        .ok()?;
    pid_is_alive(pid).then_some(pid)
}

/// The pid of the live daemon owning `data_dir`, if any.
///
/// The check every non-daemon process runs before opening a data directory.
#[must_use]
pub fn live_owner(data_dir: &Path) -> Option<u32> {
    owner_of(&data_dir.join(LOCK_FILE_NAME))
}

/// The operator-facing refusal message, shared so every surface says the same
/// thing about the same situation.
#[must_use]
pub fn already_owned_message(data_dir: &Path, owner_pid: u32) -> String {
    format!(
        "data directory '{}' is owned by a running AletheiaDB daemon (pid {owner_pid}). \
         Opening it here would make this process a second writer, which AletheiaDB does \
         not support. Use the daemon's HTTP or MCP surface (`aletheia daemon status` \
         shows the URLs), or stop it first with `aletheia daemon stop`.",
        data_dir.display()
    )
}

/// Whether a process id currently exists.
///
/// Deliberately asymmetric: a false positive (pid reuse) costs a recoverable
/// startup refusal whose message says exactly how to proceed, while a false
/// negative costs data-directory corruption. Where the answer is uncertain,
/// this reports "alive".
#[cfg(unix)]
#[must_use]
pub fn pid_is_alive(pid: u32) -> bool {
    // `kill(pid, 0)` delivers no signal; it performs only the existence and
    // permission check, and unlike `/proc` it works on every Unix.
    //
    // SAFETY: both arguments are plain integers, no memory is accessed, and
    // signal 0 cannot affect the target process. A nonexistent pid returns -1
    // with ESRCH.
    if unsafe { libc::kill(pid as libc::pid_t, 0) } == 0 {
        return true;
    }
    // EPERM: the process exists but belongs to another user.
    std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// Windows counterpart of [`pid_is_alive`]: there is no `/proc` and no `kill`,
/// so the task list is the available existence check.
///
/// `tasklist.exe` is invoked by **absolute path** under `%SystemRoot%`: resolving
/// it through `PATH` would let a user-writable directory earlier in the search
/// order decide whether a daemon is considered alive — and a forged "not alive"
/// answer is what admits a second writer.
#[cfg(windows)]
#[must_use]
pub fn pid_is_alive(pid: u32) -> bool {
    let Ok(output) = std::process::Command::new(system32_tool("tasklist.exe"))
        .args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"])
        .output()
    else {
        // Cannot tell: fail closed (assume live) so the lock never admits a
        // second writer because a helper was unavailable.
        return true;
    };
    tasklist_reports_pid(&String::from_utf8_lossy(&output.stdout), pid)
}

#[cfg(not(any(unix, windows)))]
#[must_use]
pub fn pid_is_alive(_pid: u32) -> bool {
    // Unknown platform: fail closed.
    true
}

/// Absolute path to a System32 tool, so process resolution never consults a
/// user-writable `PATH` entry.
#[cfg(windows)]
#[must_use]
pub fn system32_tool(name: &str) -> std::path::PathBuf {
    let root = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".to_string());
    Path::new(&root).join("System32").join(name)
}

/// Whether `tasklist /FO CSV /NH` output reports `pid` as a running process.
///
/// Parses the **pid column** rather than substring-matching the whole output:
/// `tasklist` also prints session ids and memory figures, so a bare
/// `output.contains("512")` reports pid 512 as alive whenever any process uses
/// 512 K of memory. It also prints an informational "no tasks" line (not CSV)
/// when the filter matches nothing, which must read as "not alive".
///
/// CSV rows are `"Image Name","PID","Session Name","Session#","Mem Usage"`.
#[must_use]
pub fn tasklist_reports_pid(stdout: &str, pid: u32) -> bool {
    stdout.lines().any(|line| {
        line.split(',')
            .nth(1)
            .map(|field| field.trim().trim_matches('"'))
            .is_some_and(|field| field.parse::<u32>() == Ok(pid))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_directory_can_be_claimed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let lock = DaemonLock::acquire(dir.path()).expect("claim");
        assert!(lock.path().exists());
        assert_eq!(
            fs::read_to_string(lock.path()).expect("read"),
            std::process::id().to_string()
        );
    }

    #[test]
    fn the_claim_is_released_on_drop() {
        let dir = tempfile::tempdir().expect("tempdir");
        {
            let _lock = DaemonLock::acquire(dir.path()).expect("claim");
        }
        assert!(
            !dir.path().join(LOCK_FILE_NAME).exists(),
            "a clean exit releases the directory"
        );
    }

    #[test]
    fn a_stale_lock_is_reclaimed() {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::write(dir.path().join(LOCK_FILE_NAME), dead_pid().to_string()).expect("stale lock");
        let lock = DaemonLock::acquire(dir.path()).expect("a dead owner must not block startup");
        assert_eq!(
            fs::read_to_string(lock.path()).expect("read"),
            std::process::id().to_string(),
            "the reclaimed lock records the new owner"
        );
    }

    #[test]
    fn a_garbage_lock_is_reclaimed() {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::write(dir.path().join(LOCK_FILE_NAME), "not-a-pid\n").expect("garbage lock");
        assert!(
            DaemonLock::acquire(dir.path()).is_ok(),
            "an unparseable lock must not brick the directory"
        );
    }

    #[test]
    fn a_live_owner_blocks_the_claim() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut owner = spawn_live_process();
        fs::write(dir.path().join(LOCK_FILE_NAME), owner.id().to_string()).expect("live lock");

        let refused = DaemonLock::acquire(dir.path());
        let _ = owner.kill();
        let _ = owner.wait();

        let message = refused.expect_err("a live owner must block the claim");
        assert!(
            message.contains("owned by a running AletheiaDB daemon"),
            "the refusal names the conflict: {message}"
        );
        assert!(
            message.contains("aletheia daemon stop"),
            "the refusal says how to proceed: {message}"
        );
    }

    #[test]
    fn dropping_a_reclaimed_lock_does_not_delete_the_new_owners_claim() {
        let dir = tempfile::tempdir().expect("tempdir");
        let lock = DaemonLock::acquire(dir.path()).expect("claim");
        // Simulate: this lock was reclaimed and a different process now owns
        // the directory. Dropping ours must not remove theirs.
        fs::write(lock.path(), "424242").expect("overwrite with another owner");
        drop(lock);
        assert!(
            dir.path().join(LOCK_FILE_NAME).exists(),
            "a stale handle must never release the current owner's claim"
        );
    }

    #[test]
    fn live_owner_reports_none_for_an_unlocked_directory() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert_eq!(live_owner(dir.path()), None);
    }

    #[test]
    fn a_second_claim_from_the_same_process_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let first = DaemonLock::acquire(dir.path()).expect("first claim");
        let second = DaemonLock::acquire(dir.path());
        assert!(
            second.is_err(),
            "acquiring twice would leave two handles racing to release one claim"
        );
        drop(first);
        assert!(
            !dir.path().join(LOCK_FILE_NAME).exists(),
            "the surviving claim is still released normally"
        );
    }

    #[test]
    fn tasklist_output_is_parsed_by_pid_column_not_substring() {
        // A real `tasklist /FO CSV /NH` row. The memory column contains "512",
        // which a substring match would report as pid 512 being alive.
        let row = "\"aletheia-daemon.exe\",\"4321\",\"Console\",\"1\",\"512 K\"\n";
        assert!(tasklist_reports_pid(row, 4321), "the pid column matches");
        assert!(
            !tasklist_reports_pid(row, 512),
            "a memory figure is not a pid"
        );
        assert!(
            !tasklist_reports_pid(row, 1),
            "a session number is not a pid"
        );
    }

    #[test]
    fn tasklist_no_tasks_output_reads_as_not_alive() {
        let output = "INFO: No tasks are running which match the specified criteria.\n";
        assert!(!tasklist_reports_pid(output, 4321));
        assert!(!tasklist_reports_pid("", 4321), "empty output is not alive");
    }

    #[test]
    fn a_reaped_pid_is_not_alive_and_a_live_one_is() {
        let mut child = spawn_live_process();
        let pid = child.id();
        assert!(pid_is_alive(pid), "a running child is alive");
        let _ = child.kill();
        let _ = child.wait();
        assert!(
            !pid_is_alive(pid),
            "a reaped child is not alive (its pid is free again)"
        );
    }

    /// A pid that is definitely not running: spawn, then reap it.
    fn dead_pid() -> u32 {
        let mut child = spawn_live_process();
        let pid = child.id();
        let _ = child.kill();
        let _ = child.wait();
        pid
    }

    /// A process that stays alive long enough to act as a lock owner.
    ///
    /// Windows uses `ping`, not `timeout`: `timeout` refuses to run when stdin
    /// is not a console ("ERROR: Input redirection is not supported") and exits
    /// immediately, so under a test harness it yields an already-dead pid — which
    /// makes every liveness assertion here fail while pointing the blame at the
    /// lock. `ping -n` sleeps between echoes and needs no console.
    fn spawn_live_process() -> std::process::Child {
        let (program, args): (&str, Vec<&str>) = if cfg!(windows) {
            // 31 echoes, ~1s apart => ~30s of life.
            ("ping", vec!["-n", "31", "127.0.0.1"])
        } else {
            ("sleep", vec!["30"])
        };
        let mut child = std::process::Command::new(program)
            .args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap_or_else(|e| panic!("spawn `{program}` as a live lock owner: {e}"));

        // Confirm the helper is actually running before any test depends on it.
        // Checked with `try_wait` rather than `pid_is_alive` so this stays
        // independent of the function under test: if a future platform quirk
        // kills the helper on startup, the failure says so instead of being
        // misread as a broken lock.
        std::thread::sleep(std::time::Duration::from_millis(100));
        match child.try_wait() {
            Ok(None) => child,
            Ok(Some(status)) => panic!(
                "the `{program}` test helper exited immediately with {status}; it cannot \
                 stand in for a live lock owner on this platform"
            ),
            Err(e) => panic!("cannot determine whether the `{program}` helper is running: {e}"),
        }
    }
}
