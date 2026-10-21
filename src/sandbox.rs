//! sd-cli runs with no network.
//!
//! A model file is data: weights only change the pixels, sd.cpp has no template
//! engine, and its pickle reader keeps REDUCE inert. What a downloaded file *can*
//! do is hit a memory-safety bug in sd.cpp's C++ loaders (GGUF, safetensors,
//! torch zip/pickle) and run code as the user. This is the defence in depth
//! against that: the child can't open an internet socket, and neither can
//! anything it `exec`s (`curl` included) — a seccomp filter is inherited and
//! can't be removed.
//!
//! **Seccomp, not a network namespace.** Ubuntu 23.10+ restricts unprivileged
//! user namespaces through AppArmor, so `unshare(CLONE_NEWUSER|CLONE_NEWNET)`
//! fails on a common distro; a seccomp filter only needs `no_new_privs`, which
//! any process may set on itself.
//!
//! **Allowlist by address family**: `AF_UNIX` and `AF_NETLINK` stay (local only;
//! libudev and a Vulkan loader may use them), every other `socket`/`socketpair`
//! is `EACCES`. `io_uring_setup` is refused too, since `IORING_OP_SOCKET`
//! creates sockets without a `socket` syscall. sd-cli uses none of these.
//!
//! **Windows: an AppContainer with no capabilities** — Windows' own app
//! sandbox, and the only one that needs no admin rights (a WFP filter or a
//! firewall rule do; a job object can't touch the network). With no
//! `internetClient`/`privateNetworkClientServer` it has no internet, no LAN and
//! no loopback, every process it starts is in it too, and it opens only files
//! whose ACL names its SID — so [`confine`] first grants it yasdig's own
//! folders ([`Root`]), and the person's documents stay out of its reach.
//!
//! std's `Command` can't pass a process attribute on stable, and the runner
//! needs a `std::process::Child` with piped output, so on Windows `confine`
//! rewrites the command into `yasdig.exe sandbox-run <sd-cli> <args…>`:
//! [`helper_main`] starts sd-cli in the container with the three std handles it
//! was given, inside a kill-on-close job (Stop's `TerminateProcess` on the
//! launcher takes sd-cli with it), and exits with its exit code. A step that
//! fails prints why and exits [`SANDBOX_FAILED`] — sd-cli never starts outside.
//!
//! The network block is enforced by the Windows firewall's filtering engine,
//! which a third-party firewall can replace, so the first confined spawn of a
//! run checks it holds ([`probe`]): `curl.exe`, inside, against a loopback port
//! of ours. A connection that arrives refuses every sd-cli run.
//!
//! Linux on x86_64 and aarch64, and Windows; elsewhere `confine` does nothing
//! and `doctor` says so. `[sandbox] enabled = false` in `startup.toml` (or
//! `YASDIG_NO_SANDBOX=1`) turns it off, for a kernel without seccomp or a
//! Windows where the GPU isn't visible from inside.
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};

/// The environment variable that turns the sandbox off.
pub const OPT_OUT: &str = "YASDIG_NO_SANDBOX";

/// The hidden first argument that makes yasdig the Windows launcher.
/// Dispatched before anything else in `main`.
pub const HELPER: &str = "sandbox-run";

/// The launcher's exit code when it couldn't start sd-cli confined.
pub const SANDBOX_FAILED: i32 = 125;

/// How to turn it off, for every message that offers it.
pub const TURN_OFF: &str = "`enabled = false` under [sandbox] in startup.toml";

/// Set from `startup.toml` by `Config::load`, before any sd-cli runs.
static OFF_BY_FILE: AtomicBool = AtomicBool::new(false);

pub fn set_off_by_file(off: bool) {
    OFF_BY_FILE.store(off, Ordering::Relaxed);
}

/// True when `startup.toml` says `enabled = false` under `[sandbox]`.
pub fn off_by_file() -> bool {
    OFF_BY_FILE.load(Ordering::Relaxed)
}

/// True when the user set [`OPT_OUT`] (to anything but empty or `0`).
pub fn off_by_env() -> bool {
    std::env::var_os(OPT_OUT).is_some_and(|v| !v.is_empty() && v != "0")
}

/// Turned off, by the file or the environment.
pub fn opted_out() -> bool {
    off_by_file() || off_by_env()
}

/// True when `confine` sandboxes on this build and this run.
pub fn enabled() -> bool {
    imp::SUPPORTED && !opted_out()
}

/// Whether this system can run the sandbox, without spawning anything:
/// the seccomp actions the kernel advertises (`actions_avail`, Linux 4.14+),
/// or whether the AppContainer profile can be made. `None` elsewhere.
pub fn kernel_ready() -> Option<bool> {
    imp::kernel_ready()
}

/// A folder sd-cli must reach from inside: Windows grants it to the container
/// (with everything below it); Linux has no file side.
#[derive(Debug, Clone, PartialEq)]
pub struct Root {
    pub path: PathBuf,
    /// Write and delete as well as read: `work/`, and the model folders, which
    /// Convert writes into.
    pub write: bool,
}

/// What [`probe`] found.
#[cfg_attr(not(windows), allow(dead_code))]
#[derive(Debug, Clone, PartialEq)]
pub enum Probe {
    /// A connection from inside was refused.
    Held,
    /// Couldn't tell, and why (no `curl.exe`, or it failed some other way).
    Unchecked(String),
    /// A connection from inside got through: the block isn't enforced here.
    Leaks,
}

/// Whether the network block holds, checked once per run (Windows; `None`
/// elsewhere, where the filter is the kernel's and needs no check).
pub fn probe() -> Option<Result<Probe, String>> {
    imp::probe()
}

/// Make `cmd`'s child unable to reach the network, as the command to spawn in
/// its place. A child that can't be confined fails — to spawn, or at once with
/// the reason on stderr — and never runs unconfined. Called on every sd-cli
/// spawn (`SdRunner::run_tail`), after the arguments are read off `cmd`.
pub fn confine(cmd: Command, roots: &[Root]) -> anyhow::Result<Command> {
    if enabled() {
        imp::confine(cmd, roots)
    } else {
        Ok(cmd)
    }
}

/// `yasdig sandbox-run <program> <args…>`: start `program` in the container,
/// wait, exit with its code.
pub fn helper_main(args: Vec<std::ffi::OsString>) -> ! {
    imp::helper_main(args)
}

/// Append `arg` to a Windows command line the way the MSVC runtime (and
/// Rust's std) split it back: quoted when it holds a space, tab, newline or
/// quote, or is empty; a backslash run doubled before a quote or the closing
/// quote, left alone anywhere else.
#[cfg_attr(not(windows), allow(dead_code))]
fn quote_into(line: &mut Vec<u16>, arg: &[u16]) {
    let (q, bs) = (b'"' as u16, b'\\' as u16);
    let plain = !arg.is_empty() && !arg.iter().any(|&c| c == b' ' as u16 || c == b'\t' as u16 || c == b'\n' as u16 || c == q);
    if plain {
        line.extend_from_slice(arg);
        return;
    }
    line.push(q);
    let mut run = 0;
    for &c in arg {
        if c == bs {
            run += 1;
            continue;
        }
        let n = if c == q { run * 2 + 1 } else { run };
        line.extend(std::iter::repeat_n(bs, n));
        line.push(c);
        run = 0;
    }
    line.extend(std::iter::repeat_n(bs, run * 2));
    line.push(q);
}

/// The folders granted to the container, minus any inside another.
#[cfg_attr(not(windows), allow(dead_code))]
fn outermost(roots: &[Root]) -> Vec<&Root> {
    roots
        .iter()
        .filter(|r| !roots.iter().any(|o| o.path != r.path && r.path.starts_with(&o.path) && (o.write || !r.write)))
        .collect()
}

#[cfg(all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64")))]
mod imp {
    use std::os::unix::process::CommandExt;
    use std::process::Command;

    pub const SUPPORTED: bool = true;

    // linux/audit.h; not in the libc crate.
    #[cfg(target_arch = "x86_64")]
    const ARCH: u32 = 0xc000_003e;
    #[cfg(target_arch = "aarch64")]
    const ARCH: u32 = 0xc000_00b7;
    /// x32 syscalls set this bit on x86_64; they would reach `socket` under
    /// another number. No aarch64 syscall number is this large, so the same
    /// check is harmless there.
    const X32_BIT: u32 = 0x4000_0000;

    const LD: u16 = (libc::BPF_LD | libc::BPF_W | libc::BPF_ABS) as u16;
    const JEQ: u16 = (libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K) as u16;
    const JGE: u16 = (libc::BPF_JMP | libc::BPF_JGE | libc::BPF_K) as u16;
    const RET: u16 = (libc::BPF_RET | libc::BPF_K) as u16;

    const fn op(code: u16, jt: u8, jf: u8, k: u32) -> libc::sock_filter {
        libc::sock_filter { code, jt, jf, k }
    }

    // `seccomp_data`: nr at 0, arch at 4, args[0]'s low half at 16
    // (little-endian; the kernel reads `domain` as an int).
    const DENY: u32 = libc::SECCOMP_RET_ERRNO | libc::EACCES as u32;
    pub(super) static FILTER: [libc::sock_filter; 14] = [
        /* 0 */ op(LD, 0, 0, 4),
        /* 1 */ op(JEQ, 0, 11, ARCH), // foreign arch → 13
        /* 2 */ op(LD, 0, 0, 0),
        /* 3 */ op(JGE, 7, 0, X32_BIT), // → 11
        /* 4 */ op(JEQ, 3, 0, libc::SYS_socket as u32), // → 8
        /* 5 */ op(JEQ, 2, 0, libc::SYS_socketpair as u32), // → 8
        /* 6 */ op(JEQ, 4, 0, libc::SYS_io_uring_setup as u32), // → 11
        /* 7 */ op(RET, 0, 0, libc::SECCOMP_RET_ALLOW),
        /* 8 */ op(LD, 0, 0, 16),
        /* 9 */ op(JEQ, 2, 0, libc::AF_UNIX as u32), // → 12
        /* 10 */ op(JEQ, 1, 0, libc::AF_NETLINK as u32), // → 12
        /* 11 */ op(RET, 0, 0, DENY),
        /* 12 */ op(RET, 0, 0, libc::SECCOMP_RET_ALLOW),
        /* 13 */ op(RET, 0, 0, libc::SECCOMP_RET_KILL_PROCESS),
    ];

    pub fn confine(mut cmd: Command, _: &[super::Root]) -> anyhow::Result<Command> {
        apply(&mut cmd);
        Ok(cmd)
    }

    pub fn probe() -> Option<Result<super::Probe, String>> {
        None
    }

    pub fn helper_main(_: Vec<std::ffi::OsString>) -> ! {
        eprintln!("[yasdig sandbox] {} is the Windows launcher; this build confines with seccomp", super::HELPER);
        std::process::exit(super::SANDBOX_FAILED)
    }

    pub fn apply(cmd: &mut Command) {
        // SAFETY: runs between fork and exec, so it may only make
        // async-signal-safe calls: two `prctl`s on a static, no allocation
        // (`last_os_error` is the `Os` variant, a plain integer).
        unsafe {
            cmd.pre_exec(|| {
                if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                let prog = libc::sock_fprog {
                    len: FILTER.len() as u16,
                    filter: FILTER.as_ptr() as *mut libc::sock_filter,
                };
                if libc::prctl(libc::PR_SET_SECCOMP, libc::SECCOMP_MODE_FILTER, &prog) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }

    pub fn kernel_ready() -> Option<bool> {
        let avail = std::fs::read_to_string("/proc/sys/kernel/seccomp/actions_avail").unwrap_or_default();
        let has = |a: &str| avail.split_whitespace().any(|w| w == a);
        Some(has("errno") && has("kill_process") && has("allow"))
    }
}

#[cfg(not(any(windows, all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64")))))]
mod imp {
    use std::process::Command;
    pub const SUPPORTED: bool = false;
    pub fn confine(cmd: Command, _: &[super::Root]) -> anyhow::Result<Command> {
        Ok(cmd)
    }
    pub fn kernel_ready() -> Option<bool> {
        None
    }
    pub fn probe() -> Option<Result<super::Probe, String>> {
        None
    }
    pub fn helper_main(_: Vec<std::ffi::OsString>) -> ! {
        eprintln!("[yasdig sandbox] no sandbox on this OS");
        std::process::exit(super::SANDBOX_FAILED)
    }
}

#[cfg(windows)]
mod imp {
    use super::{quote_into, Probe, Root, HELPER, SANDBOX_FAILED};
    use std::ffi::{OsStr, OsString};
    use std::os::windows::ffi::OsStrExt;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};
    use std::ptr::{null, null_mut};
    use std::sync::{Mutex, OnceLock};
    use windows_sys::Win32::Foundation::{
        CloseHandle, LocalFree, SetHandleInformation, ERROR_SUCCESS, HANDLE, HANDLE_FLAG_INHERIT,
        INVALID_HANDLE_VALUE, WAIT_OBJECT_0,
    };
    use windows_sys::Win32::Security::Authorization::{
        ConvertSidToStringSidW, GetExplicitEntriesFromAclW, GetNamedSecurityInfoW, SetEntriesInAclW,
        SetNamedSecurityInfoW, EXPLICIT_ACCESS_W, GRANT_ACCESS, NO_MULTIPLE_TRUSTEE, SET_ACCESS, SE_FILE_OBJECT,
        TRUSTEE_IS_SID, TRUSTEE_IS_WELL_KNOWN_GROUP, TRUSTEE_W,
    };
    use windows_sys::Win32::Security::Isolation::{
        CreateAppContainerProfile, DeriveAppContainerSidFromAppContainerName, GetAppContainerFolderPath,
    };
    use windows_sys::Win32::Security::{
        EqualSid, FreeSid, ACL, DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID, SECURITY_CAPABILITIES,
        SUB_CONTAINERS_AND_OBJECTS_INHERIT,
    };
    use windows_sys::Win32::Storage::FileSystem::{DELETE, FILE_GENERIC_EXECUTE, FILE_GENERIC_READ, FILE_GENERIC_WRITE};
    use windows_sys::Win32::System::Com::CoTaskMemFree;
    use windows_sys::Win32::System::Console::{GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation, SetInformationJobObject,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };
    use windows_sys::Win32::System::Threading::{
        CreateProcessW, DeleteProcThreadAttributeList, GetExitCodeProcess, InitializeProcThreadAttributeList,
        ResumeThread, TerminateProcess, UpdateProcThreadAttribute, WaitForSingleObject, CREATE_SUSPENDED,
        EXTENDED_STARTUPINFO_PRESENT, INFINITE, PROCESS_INFORMATION, PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
        PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES, STARTF_USESTDHANDLES, STARTUPINFOEXW, STARTUPINFOW,
    };

    pub const SUPPORTED: bool = true;

    /// The container's name: one profile for every sd-cli run, so the folder
    /// grants are made once. Windows keeps it under
    /// `%LOCALAPPDATA%\Packages\yasdig.sd-cli`.
    const NAME: &str = "yasdig.sd-cli";

    const WRITE: u32 = FILE_GENERIC_READ | FILE_GENERIC_WRITE | FILE_GENERIC_EXECUTE | DELETE;
    const READ: u32 = FILE_GENERIC_READ | FILE_GENERIC_EXECUTE;

    fn wide(s: impl AsRef<OsStr>) -> Vec<u16> {
        s.as_ref().encode_wide().chain(std::iter::once(0)).collect()
    }

    fn os_error(what: &str, code: u32) -> String {
        format!("{what}: {}", std::io::Error::from_raw_os_error(code as i32))
    }

    fn last_error(what: &str) -> String {
        format!("{what}: {}", std::io::Error::last_os_error())
    }

    /// The container's SID, freed on drop.
    struct Sid(PSID);

    impl Drop for Sid {
        fn drop(&mut self) {
            // SAFETY: both profile calls hand back a SID for `FreeSid`.
            unsafe { FreeSid(self.0) };
        }
    }

    /// The profile, made the first time and derived after: creating one that
    /// exists fails with "already exists", and deriving is the documented way
    /// back to its SID. Neither needs admin rights.
    fn container() -> Result<Sid, String> {
        let name = wide(NAME);
        let display = wide("yasdig sd-cli sandbox");
        let mut sid: PSID = null_mut();
        // SAFETY: valid NUL-terminated strings, no capabilities, an out pointer.
        let made = unsafe {
            CreateAppContainerProfile(name.as_ptr(), display.as_ptr(), display.as_ptr(), null(), 0, &mut sid)
        };
        if made >= 0 {
            return Ok(Sid(sid));
        }
        // SAFETY: as above.
        let derived = unsafe { DeriveAppContainerSidFromAppContainerName(name.as_ptr(), &mut sid) };
        if derived >= 0 {
            Ok(Sid(sid))
        } else {
            Err(format!("creating the AppContainer profile failed (HRESULT {made:#010x}, then {derived:#010x})"))
        }
    }

    pub fn kernel_ready() -> Option<bool> {
        Some(container().is_ok())
    }

    /// Paths already granted this run; a grant is checked once per path.
    static GRANTED: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

    /// Give the container `mask` on `path` and everything below it, unless an
    /// inheritable grant of at least that is already there — adding one
    /// rewrites every file's ACL below, which is quick for a models folder
    /// and a second or two for a full `work/`, so only the first time.
    fn grant(sid: &Sid, path: &Path, mask: u32) -> Result<(), String> {
        let name = wide(path);
        let mut dacl: *mut ACL = null_mut();
        let mut sd: PSECURITY_DESCRIPTOR = null_mut();
        // SAFETY: out pointers; `sd` owns `dacl` and is freed below.
        let got = unsafe {
            GetNamedSecurityInfoW(
                name.as_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                null_mut(),
                null_mut(),
                &mut dacl,
                null_mut(),
                &mut sd,
            )
        };
        if got != ERROR_SUCCESS {
            return Err(os_error(&format!("reading the permissions of {}", path.display()), got));
        }
        let result = (|| {
            // No DACL (FAT, exFAT) is no access check at all.
            if dacl.is_null() || has_grant(sid, dacl, mask) {
                return Ok(());
            }
            let entry = EXPLICIT_ACCESS_W {
                grfAccessPermissions: mask,
                grfAccessMode: GRANT_ACCESS,
                grfInheritance: SUB_CONTAINERS_AND_OBJECTS_INHERIT,
                Trustee: TRUSTEE_W {
                    pMultipleTrustee: null_mut(),
                    MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE,
                    TrusteeForm: TRUSTEE_IS_SID,
                    TrusteeType: TRUSTEE_IS_WELL_KNOWN_GROUP,
                    ptstrName: sid.0 as *mut u16,
                },
            };
            let mut new: *mut ACL = null_mut();
            // SAFETY: one entry, the old DACL, an out pointer freed below.
            let set = unsafe { SetEntriesInAclW(1, &entry, dacl, &mut new) };
            if set != ERROR_SUCCESS {
                return Err(os_error("building the sandbox's permission", set));
            }
            // SAFETY: a NUL-terminated path and the ACL just built.
            let put = unsafe {
                SetNamedSecurityInfoW(
                    name.as_ptr(),
                    SE_FILE_OBJECT,
                    DACL_SECURITY_INFORMATION,
                    null_mut(),
                    null_mut(),
                    new,
                    null(),
                )
            };
            // SAFETY: `SetEntriesInAclW` allocates with `LocalAlloc`.
            unsafe { LocalFree(new as _) };
            if put != ERROR_SUCCESS {
                return Err(os_error(
                    &format!(
                        "letting sd-cli's sandbox into {} (move the yasdig folder somewhere you own, \
                         or set {})",
                        path.display(),
                        super::TURN_OFF
                    ),
                    put,
                ));
            }
            Ok(())
        })();
        // SAFETY: allocated by `GetNamedSecurityInfoW` with `LocalAlloc`.
        unsafe { LocalFree(sd as _) };
        result
    }

    /// An explicit, inheritable allow entry for the container with every bit
    /// of `mask`.
    fn has_grant(sid: &Sid, dacl: *mut ACL, mask: u32) -> bool {
        let mut n = 0u32;
        let mut list: *mut EXPLICIT_ACCESS_W = null_mut();
        // SAFETY: a valid DACL and out pointers; `list` is freed below.
        if unsafe { GetExplicitEntriesFromAclW(dacl, &mut n, &mut list) } != ERROR_SUCCESS || list.is_null() {
            return false;
        }
        // SAFETY: `n` entries at `list`.
        let entries = unsafe { std::slice::from_raw_parts(list, n as usize) };
        let found = entries.iter().any(|e| {
            e.Trustee.TrusteeForm == TRUSTEE_IS_SID
                && (e.grfAccessMode == GRANT_ACCESS || e.grfAccessMode == SET_ACCESS)
                && e.grfAccessPermissions & mask == mask
                && e.grfInheritance & SUB_CONTAINERS_AND_OBJECTS_INHERIT == SUB_CONTAINERS_AND_OBJECTS_INHERIT
                // SAFETY: a SID trustee's name is its SID.
                && unsafe { EqualSid(e.Trustee.ptstrName as PSID, sid.0) } != 0
        });
        // SAFETY: allocated with `LocalAlloc`.
        unsafe { LocalFree(list as _) };
        found
    }

    fn grant_all(roots: &[Root]) -> Result<(), String> {
        let sid = container()?;
        let mut done = GRANTED.lock().unwrap_or_else(|e| e.into_inner());
        for r in super::outermost(roots) {
            if done.contains(&r.path) || !r.path.exists() {
                continue;
            }
            grant(&sid, &r.path, if r.write { WRITE } else { READ })?;
            done.push(r.path.clone());
        }
        Ok(())
    }

    pub fn confine(cmd: Command, roots: &[Root]) -> anyhow::Result<Command> {
        grant_all(roots).map_err(anyhow::Error::msg)?;
        match probe().expect("Windows probes") {
            Ok(Probe::Leaks) => anyhow::bail!(
                "Windows isn't enforcing the sd-cli sandbox's network block on this machine (a test \
                 connection from inside it got through — a third-party firewall can replace the one \
                 that does), so sd-cli won't run. {} runs it without the sandbox.",
                super::TURN_OFF
            ),
            Ok(_) => {}
            Err(e) => anyhow::bail!("the sd-cli sandbox can't start: {e}. {} runs sd-cli without it.", super::TURN_OFF),
        }
        let mut wrapped = Command::new(std::env::current_exe()?);
        wrapped.arg(HELPER).arg(cmd.get_program()).args(cmd.get_args());
        if let Some(dir) = cmd.get_current_dir() {
            wrapped.current_dir(dir);
        }
        for (k, v) in cmd.get_envs() {
            match v {
                Some(v) => wrapped.env(k, v),
                None => wrapped.env_remove(k),
            };
        }
        // A real handle, so it can go on the child's handle list (a console
        // one can't); sd-cli reads nothing.
        wrapped.stdin(Stdio::null());
        Ok(wrapped)
    }

    pub fn helper_main(args: Vec<OsString>) -> ! {
        let Some((program, rest)) = args.split_first() else {
            eprintln!("[yasdig sandbox] usage: yasdig {HELPER} <program> <args…>");
            std::process::exit(SANDBOX_FAILED);
        };
        // The user's temp folder is outside the container; its own isn't.
        if let Ok(temp) = container_temp() {
            std::env::set_var("TEMP", &temp);
            std::env::set_var("TMP", &temp);
        }
        // SAFETY: plain handle queries.
        let own = unsafe {
            [GetStdHandle(STD_INPUT_HANDLE), GetStdHandle(STD_OUTPUT_HANDLE), GetStdHandle(STD_ERROR_HANDLE)]
        };
        match launch(Path::new(program), rest, own) {
            Ok(code) => std::process::exit(code as i32),
            Err(e) => {
                eprintln!("[yasdig sandbox] {e} — sd-cli was not started. {} runs it without the sandbox.", super::TURN_OFF);
                std::process::exit(SANDBOX_FAILED);
            }
        }
    }

    /// `%LOCALAPPDATA%\Packages\yasdig.sd-cli\AC\Temp`.
    fn container_temp() -> Result<PathBuf, String> {
        let sid = container()?;
        let mut text: *mut u16 = null_mut();
        // SAFETY: a valid SID and an out pointer freed with `LocalFree`.
        if unsafe { ConvertSidToStringSidW(sid.0, &mut text) } == 0 {
            return Err(last_error("naming the container"));
        }
        let mut folder: *mut u16 = null_mut();
        // SAFETY: the SID string, an out pointer freed with `CoTaskMemFree`.
        let hr = unsafe { GetAppContainerFolderPath(text, &mut folder) };
        // SAFETY: as above.
        unsafe { LocalFree(text as _) };
        if hr < 0 || folder.is_null() {
            return Err(format!("finding the container's folder (HRESULT {hr:#010x})"));
        }
        // SAFETY: a NUL-terminated string from the call above.
        let path = unsafe {
            let len = (0..).take_while(|&i| *folder.add(i) != 0).count();
            let s = String::from_utf16_lossy(std::slice::from_raw_parts(folder, len));
            CoTaskMemFree(folder as _);
            PathBuf::from(s).join("Temp")
        };
        std::fs::create_dir_all(&path).map_err(|e| format!("creating {}: {e}", path.display()))?;
        Ok(path)
    }

    /// Start `program` in the container with `stdio` as its stdin, stdout and
    /// stderr (the only handles it inherits), wait for it and everything it
    /// starts, and hand back its exit code.
    pub(super) fn launch(program: &Path, args: &[OsString], stdio: [HANDLE; 3]) -> Result<u32, String> {
        let sid = container()?;
        let mut caps = SECURITY_CAPABILITIES {
            AppContainerSid: sid.0,
            Capabilities: null_mut(),
            CapabilityCount: 0,
            Reserved: 0,
        };
        let mut inherit: Vec<HANDLE> = Vec::new();
        for &h in &stdio {
            if !h.is_null() && h != INVALID_HANDLE_VALUE && !inherit.contains(&h) {
                // SAFETY: a handle of ours. The list below is what limits
                // inheritance to these three, so the flag must be on.
                unsafe { SetHandleInformation(h, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT) };
                inherit.push(h);
            }
        }

        let mut size = 0usize;
        // SAFETY: the documented size query; it fails with the size set.
        unsafe { InitializeProcThreadAttributeList(null_mut(), 2, 0, &mut size) };
        let mut buf = vec![0usize; size.div_ceil(std::mem::size_of::<usize>())];
        let list = buf.as_mut_ptr() as *mut core::ffi::c_void;
        // SAFETY: `buf` is at least `size` bytes and outlives the list.
        if unsafe { InitializeProcThreadAttributeList(list, 2, 0, &mut size) } == 0 {
            return Err(last_error("preparing the process attributes"));
        }
        struct List(*mut core::ffi::c_void);
        impl Drop for List {
            fn drop(&mut self) {
                // SAFETY: initialised above.
                unsafe { DeleteProcThreadAttributeList(self.0) };
            }
        }
        let _list = List(list);
        // SAFETY: `caps` and `inherit` outlive `CreateProcessW` below.
        unsafe {
            if UpdateProcThreadAttribute(
                list,
                0,
                PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES as usize,
                &mut caps as *mut _ as *const _,
                std::mem::size_of::<SECURITY_CAPABILITIES>(),
                null_mut(),
                null(),
            ) == 0
            {
                return Err(last_error("asking for the AppContainer"));
            }
            if !inherit.is_empty()
                && UpdateProcThreadAttribute(
                    list,
                    0,
                    PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
                    inherit.as_ptr() as *const _,
                    inherit.len() * std::mem::size_of::<HANDLE>(),
                    null_mut(),
                    null(),
                ) == 0
            {
                return Err(last_error("limiting the inherited handles"));
            }
        }

        // SAFETY: all-zero is a valid STARTUPINFOEXW.
        let mut si: STARTUPINFOEXW = unsafe { std::mem::zeroed() };
        si.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
        si.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        si.StartupInfo.hStdInput = stdio[0];
        si.StartupInfo.hStdOutput = stdio[1];
        si.StartupInfo.hStdError = stdio[2];
        si.lpAttributeList = list;

        let app = wide(program);
        let mut line = Vec::new();
        quote_into(&mut line, &program.as_os_str().encode_wide().collect::<Vec<_>>());
        for a in args {
            line.push(b' ' as u16);
            quote_into(&mut line, &a.encode_wide().collect::<Vec<_>>());
        }
        line.push(0);

        // SAFETY: all-zero is a valid PROCESS_INFORMATION out value.
        let mut pi: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
        // SAFETY: NUL-terminated strings, a mutable command line as the API
        // wants, the extended startup info built above.
        let made = unsafe {
            CreateProcessW(
                app.as_ptr(),
                line.as_mut_ptr(),
                null(),
                null(),
                1,
                EXTENDED_STARTUPINFO_PRESENT | CREATE_SUSPENDED,
                null(),
                null(),
                &si as *const STARTUPINFOEXW as *const STARTUPINFOW,
                &mut pi,
            )
        };
        if made == 0 {
            return Err(last_error(&format!("starting {} in the AppContainer", program.display())));
        }
        // SAFETY: handles from `CreateProcessW` and a fresh job, all closed
        // below. The job closes last: with kill-on-close, that ends whatever
        // the child left running — and the launcher being killed closes it too.
        unsafe {
            let job = CreateJobObjectW(null(), null());
            let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let held = !job.is_null()
                && SetInformationJobObject(
                    job,
                    JobObjectExtendedLimitInformation,
                    &limits as *const _ as *const _,
                    std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                ) != 0
                && AssignProcessToJobObject(job, pi.hProcess) != 0;
            let result = if !held {
                let e = last_error("putting it in a job object");
                TerminateProcess(pi.hProcess, SANDBOX_FAILED as u32);
                Err(e)
            } else if ResumeThread(pi.hThread) == u32::MAX {
                let e = last_error("resuming it");
                TerminateProcess(pi.hProcess, SANDBOX_FAILED as u32);
                Err(e)
            } else if WaitForSingleObject(pi.hProcess, INFINITE) != WAIT_OBJECT_0 {
                Err(last_error("waiting for it"))
            } else {
                let mut code = 0u32;
                if GetExitCodeProcess(pi.hProcess, &mut code) == 0 {
                    Err(last_error("reading its exit code"))
                } else {
                    Ok(code)
                }
            };
            CloseHandle(pi.hThread);
            CloseHandle(pi.hProcess);
            if !job.is_null() {
                CloseHandle(job);
            }
            result
        }
    }

    static PROBE: OnceLock<Result<Probe, String>> = OnceLock::new();

    pub fn probe() -> Option<Result<Probe, String>> {
        Some(PROBE.get_or_init(check).clone())
    }

    /// `curl.exe` from inside against a loopback port of ours. Loopback is
    /// blocked for a container by the same filtering engine as the internet,
    /// and a port here needs no network to test with. What decides is whether
    /// a connection *arrived*, not curl's exit code, so a curl that fails some
    /// other way can't read as a block.
    fn check() -> Result<Probe, String> {
        let root = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
        let curl = PathBuf::from(root).join("System32").join("curl.exe");
        if !curl.is_file() {
            return Ok(Probe::Unchecked("no curl.exe to try a connection with".into()));
        }
        let listener = std::net::TcpListener::bind("127.0.0.1:0").map_err(|e| format!("opening a test port: {e}"))?;
        let port = listener.local_addr().map_err(|e| e.to_string())?.port();
        let args: Vec<OsString> =
            ["-s", "-o", "NUL", "--connect-timeout", "3", "--max-time", "4", &format!("http://127.0.0.1:{port}/")]
                .iter()
                .map(OsString::from)
                .collect();
        let code = launch(&curl, &args, null_stdio()?.handles())?;
        listener.set_nonblocking(true).map_err(|e| e.to_string())?;
        if listener.accept().is_ok() {
            return Ok(Probe::Leaks);
        }
        // Nothing arrived, and curl says it couldn't connect (7) or gave up
        // waiting (28, a dropped SYN): blocked. Anything else, it may never
        // have tried.
        Ok(if code == 7 || code == 28 { Probe::Held } else { Probe::Unchecked(format!("curl exited {code}")) })
    }

    /// NUL as stdin, stdout and stderr, for a child of the server's own (the
    /// probe, the tests): never the server's console handles, which the
    /// handle list would have to mark inheritable for every later spawn.
    pub(super) struct NullStdio(std::fs::File);

    impl NullStdio {
        pub(super) fn handles(&self) -> [HANDLE; 3] {
            use std::os::windows::io::AsRawHandle;
            let h = self.0.as_raw_handle() as HANDLE;
            [h, h, h]
        }
    }

    pub(super) fn null_stdio() -> Result<NullStdio, String> {
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open("NUL")
            .map(NullStdio)
            .map_err(|e| format!("opening NUL: {e}"))
    }
}

#[cfg(all(test, target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64")))]
mod tests {
    use super::imp::{apply, FILTER};
    use std::process::Command;

    /// Runs the filter the way the kernel does, over a `seccomp_data` laid out
    /// as nr, arch, ip, args — so a wrong jump offset fails here, not in a
    /// child that dies or, worse, runs unconfined.
    fn verdict(nr: u32, arch: u32, arg0: u32) -> u32 {
        let mut data = [0u8; 64];
        data[0..4].copy_from_slice(&nr.to_le_bytes());
        data[4..8].copy_from_slice(&arch.to_le_bytes());
        data[16..20].copy_from_slice(&arg0.to_le_bytes());
        let (mut pc, mut acc) = (0usize, 0u32);
        loop {
            let f = &FILTER[pc];
            let class = (f.code & 0x07) as u32;
            match class {
                c if c == libc::BPF_LD => {
                    let k = f.k as usize;
                    acc = u32::from_le_bytes(data[k..k + 4].try_into().unwrap());
                    pc += 1;
                }
                c if c == libc::BPF_JMP => {
                    let taken = if (f.code as u32 & 0xf0) == libc::BPF_JEQ { acc == f.k } else { acc >= f.k };
                    pc += 1 + if taken { f.jt } else { f.jf } as usize;
                }
                _ => return f.k,
            }
        }
    }

    #[test]
    fn the_filter_refuses_internet_sockets_and_nothing_else() {
        let arch = verdict(0, 0, 0); // any foreign arch
        assert_eq!(arch, libc::SECCOMP_RET_KILL_PROCESS);
        let a = super::imp::FILTER[1].k;
        let deny = libc::SECCOMP_RET_ERRNO | libc::EACCES as u32;
        let socket = libc::SYS_socket as u32;
        let pair = libc::SYS_socketpair as u32;
        for fam in [libc::AF_INET, libc::AF_INET6, libc::AF_PACKET, libc::AF_BLUETOOTH] {
            assert_eq!(verdict(socket, a, fam as u32), deny, "socket({fam})");
            assert_eq!(verdict(pair, a, fam as u32), deny, "socketpair({fam})");
        }
        for fam in [libc::AF_UNIX, libc::AF_NETLINK] {
            assert_eq!(verdict(socket, a, fam as u32), libc::SECCOMP_RET_ALLOW);
        }
        assert_eq!(verdict(libc::SYS_io_uring_setup as u32, a, 0), deny);
        assert_eq!(verdict(0x4000_0000 + socket, a, libc::AF_INET as u32), deny);
        for nr in [libc::SYS_openat, libc::SYS_read, libc::SYS_mmap, libc::SYS_ioctl] {
            assert_eq!(verdict(nr as u32, a, libc::AF_INET as u32), libc::SECCOMP_RET_ALLOW);
        }
    }

    /// Against the real kernel: bash's `/dev/tcp` to a listener on loopback
    /// connects unconfined and is refused confined.
    #[test]
    fn a_confined_child_cannot_connect() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let script = format!("exec 3<>/dev/tcp/127.0.0.1/{port}");
        let run = |confined: bool| {
            let mut cmd = Command::new("bash");
            cmd.args(["-c", &script]).stderr(std::process::Stdio::null());
            if confined {
                apply(&mut cmd);
            }
            cmd.status()
        };
        let Ok(open) = run(false) else {
            eprintln!("skipped: no bash to try a connection with");
            return;
        };
        assert!(open.success(), "the unconfined connect should work");
        assert!(!run(true).unwrap().success(), "the confined connect must fail");

        let mut ok = Command::new("sh");
        ok.args(["-c", "true"]);
        apply(&mut ok);
        assert!(ok.status().unwrap().success(), "a confined child otherwise runs");
    }
}

#[cfg(test)]
mod quoting {
    use super::{outermost, quote_into, Root};
    use std::path::PathBuf;

    /// The MSVC runtime's split (2008 and later), the one sd-cli's `main`
    /// gets its `argv` from, written out from Microsoft's rules so the quoting
    /// is checked on every OS rather than only on a Windows runner.
    fn split(line: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut chars = line.chars().peekable();
        loop {
            while chars.peek().is_some_and(|c| *c == ' ' || *c == '\t') {
                chars.next();
            }
            if chars.peek().is_none() {
                return out;
            }
            let (mut arg, mut quoted) = (String::new(), false);
            while let Some(&c) = chars.peek() {
                if !quoted && (c == ' ' || c == '\t') {
                    break;
                }
                chars.next();
                match c {
                    '\\' => {
                        let mut n = 1;
                        while chars.peek() == Some(&'\\') {
                            chars.next();
                            n += 1;
                        }
                        if chars.peek() == Some(&'"') {
                            arg.extend(std::iter::repeat_n('\\', n / 2));
                            if n % 2 == 1 {
                                chars.next();
                                arg.push('"');
                            }
                        } else {
                            arg.extend(std::iter::repeat_n('\\', n));
                        }
                    }
                    '"' if quoted && chars.peek() == Some(&'"') => {
                        chars.next();
                        arg.push('"');
                    }
                    '"' => quoted = !quoted,
                    c => arg.push(c),
                }
            }
            out.push(arg);
        }
    }

    #[test]
    fn every_argument_comes_back_as_it_went_in() {
        let args = [
            "-p",
            "a cat, (sitting:1.2) on \"the\" mat",
            "",
            r"C:\models\sdxl\x.safetensors",
            r"C:\path with space\",
            r"ends in two\\",
            r#"a\"b"#,
            r#"\\"quoted"\\"#,
            "tab\there",
            "line\nbreak",
            "naïve — 日本",
        ];
        let mut line = Vec::new();
        quote_into(&mut line, &"sd-cli.exe".encode_utf16().collect::<Vec<_>>());
        for a in args {
            line.push(b' ' as u16);
            quote_into(&mut line, &a.encode_utf16().collect::<Vec<_>>());
        }
        let back = split(&String::from_utf16(&line).unwrap());
        assert_eq!(back[0], "sd-cli.exe");
        assert_eq!(&back[1..], &args.map(String::from));
    }

    #[test]
    fn a_folder_inside_a_granted_one_is_left_to_inherit() {
        let r = |p: &str, write| Root { path: PathBuf::from(p), write };
        let roots = [r("/y/work", true), r("/y/models", true), r("/y/models/loras", true), r("/y/models/bin", false), r("/elsewhere/video", true)];
        let kept: Vec<_> = outermost(&roots).into_iter().map(|r| r.path.to_str().unwrap()).collect();
        assert_eq!(kept, ["/y/work", "/y/models", "/elsewhere/video"]);
        // A read grant above doesn't cover a folder that needs writing.
        let roots = [r("/y/bin", false), r("/y/bin/out", true)];
        assert_eq!(outermost(&roots).len(), 2);
    }
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::imp::{launch, null_stdio};
    use super::Probe;
    use std::ffi::OsString;
    use std::path::PathBuf;

    fn system32(exe: &str) -> PathBuf {
        PathBuf::from(std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into()))
            .join("System32")
            .join(exe)
    }

    fn args(a: &[&str]) -> Vec<OsString> {
        a.iter().map(OsString::from).collect()
    }

    /// The Linux test's twin: curl to a listener on loopback connects
    /// unconfined and gets nowhere from inside the container.
    #[test]
    fn a_confined_child_cannot_connect() {
        let curl = system32("curl.exe");
        if !curl.is_file() {
            eprintln!("skipped: no curl.exe to try a connection with");
            return;
        }
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://127.0.0.1:{}/", listener.local_addr().unwrap().port());
        let a = args(&["-s", "-o", "NUL", "--connect-timeout", "2", "--max-time", "3", &url]);
        listener.set_nonblocking(true).unwrap();

        let _ = std::process::Command::new(&curl).args(&a).status().unwrap();
        assert!(listener.accept().is_ok(), "the unconfined connect should arrive");
        launch(&curl, &a, null_stdio().unwrap().handles()).unwrap();
        assert!(listener.accept().is_err(), "the confined connect must not arrive");
        assert_eq!(super::probe(), Some(Ok(Probe::Held)));
    }

    /// A confined child still runs, and its exit code comes back.
    #[test]
    fn a_confined_child_runs_and_reports_its_exit_code() {
        let nul = null_stdio().unwrap();
        assert_eq!(launch(&system32("cmd.exe"), &args(&["/c", "exit 3"]), nul.handles()), Ok(3));
        assert_eq!(launch(&system32("cmd.exe"), &args(&["/c", "echo from inside"]), nul.handles()), Ok(0));
    }
}
