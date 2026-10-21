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
//! Linux on x86_64 and aarch64 only; elsewhere `confine` does nothing and
//! `doctor` says so. `YASDIG_NO_SANDBOX=1` turns it off, for a kernel without
//! seccomp.
use std::process::Command;

/// The environment variable that turns the filter off.
pub const OPT_OUT: &str = "YASDIG_NO_SANDBOX";

/// True when the user set [`OPT_OUT`] (to anything but empty or `0`).
pub fn opted_out() -> bool {
    std::env::var_os(OPT_OUT).is_some_and(|v| !v.is_empty() && v != "0")
}

/// True when `confine` installs a filter on this build and this run.
pub fn enabled() -> bool {
    imp::SUPPORTED && !opted_out()
}

/// Whether this kernel can run the filter, read off what it advertises
/// (`actions_avail`, Linux 4.14+) without spawning anything. `None` off Linux.
pub fn kernel_ready() -> Option<bool> {
    imp::kernel_ready()
}

/// Make `cmd`'s child unable to reach the network. A child the filter can't be
/// installed in fails to spawn, with the OS error — never runs unconfined.
pub fn confine(cmd: &mut Command) {
    if enabled() {
        imp::apply(cmd);
    }
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

#[cfg(not(all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64"))))]
mod imp {
    pub const SUPPORTED: bool = false;
    pub fn apply(_: &mut std::process::Command) {}
    pub fn kernel_ready() -> Option<bool> {
        None
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
