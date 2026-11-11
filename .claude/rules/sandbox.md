---
paths:
  - "src/sandbox.rs"
---

# The sd-cli sandbox

- **Every sd-cli spawn is `SdRunner::run_tail`'s, which confines it**
(`sandbox::confine`, after the arguments are read; the builders stay plain, so
their `argv` tests hold on every OS). `curl` for downloads is deliberately outside.
Off by `startup.toml`'s `[sandbox] enabled = false` (`startup::sandbox_off`, read
without creating the file and set in `Config::load`, since `doctor` and `upscale`
run sd-cli too) or `YASDIG_NO_SANDBOX=1`; messages name the file
(`sandbox::TURN_OFF`).
- **Linux**: seccomp in `pre_exec` (namespaces fail on Ubuntu's AppArmor). The
  filter's jump offsets are checked by a BPF interpreter in its tests — renumber
  them with it. **And Landlock, writes only** (`imp::landlock`): the ruleset is made
  in the parent per spawn (it allocates) and entered in `pre_exec` before the
  filter. Writable: the `write` roots, symlinked folders under them (`linked_dirs`,
  three levels), `/dev` (open only) and `/dev/shm`. Reads are deliberately open (a
  driver's ICDs and configs differ per distro). **A new place a child writes must
  be under a root** — as on Windows; `XDG_CACHE_HOME`/`MESA_SHADER_CACHE_DIR` are
  pointed at `work/gpu-cache/` for that. Best effort: no Landlock in the kernel
  means the network block alone, and `doctor` says which. **The filter refuses
  every `connect`** (the session D-Bus is a path-named `AF_UNIX` socket, and
  systemd behind it starts programs outside); `socket`/`bind` on `AF_UNIX` and
  `AF_NETLINK` stay for libudev. A driver that needs to connect loses the card:
  `sd::NO_GPU` is watched on Linux too and stops the run, naming
  `Settings::sandbox_block_local` (page-level, Settings → General → Advanced, read
  per spawn and passed to `confine`): off swaps in `FILTER_LOCAL`, the same filter
  with instruction 7's jumps falling through, so offsets are shared.
- **Windows**: an AppContainer (`yasdig.sd-cli`, zero capabilities). `confine`
  rewrites the command into `yasdig.exe sandbox-run <sd-cli> <args…>`, since std
  can't pass process attributes on stable; **the `sandbox::HELPER` dispatch must
  stay first in `main()`**, before `build_id`. The launcher re-quotes the
  arguments (`quote_into`, round-tripped by a test-only MSVC splitter), passes
  only its std handles (stdin is NUL — a console handle can't go on the handle
  list), and holds sd-cli in a kill-on-close job, so Stop's kill of the launcher
  takes sd-cli too.
- **The container opens only what is granted**: `Config::sandbox_roots` (`work/`,
  every model folder — writable, Convert writes beside its source — and sd-cli's
  folder), as inheritable ACEs added once. **A path sd-cli is handed must be under
  one**: the `upscale` CLI stages an ESRGAN's files in scratch.
- **Enforcement is the firewall's filtering engine**, which a third-party firewall
  can replace, so the first spawn of a run checks it (`sandbox::probe`: `curl.exe`
  inside against our loopback port; a connection that *arrives* refuses every
  run). `NO_GPU` (`ggml_vulkan: No devices found.`) under the container stops the
  run, or sd.cpp would quietly fall back to the CPU.
