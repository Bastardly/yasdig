# Building yasdig from source

For developers, and for anyone who wants a build for their own machine. Most people want the [release download](../README.md#install) instead.


Verified target: AMD Radeon RX 7900 XTX (RADV), Vulkan 1.4, Fedora. Any Vulkan-capable
GPU should work. You need Rust (stable), a Vulkan driver, and what stable-diffusion.cpp's
Vulkan backend needs to compile its shaders:

```sh
sudo dnf install cmake gcc-c++ glslc vulkan-loader-devel \
                 spirv-headers-devel spirv-tools-devel glslang-devel
```

**On Windows**, without Visual Studio: MSYS2's UCRT64 toolchain (GCC, CMake, Ninja,
the Vulkan headers and `glslc`) and Rust's GNU toolchain. In PowerShell:

```powershell
winget install --id MSYS2.MSYS2 -e
$env:MSYSTEM = 'UCRT64'
C:\msys64\usr\bin\bash.exe -lc "pacman -Syu --noconfirm"   # closes itself; run it again
C:\msys64\usr\bin\bash.exe -lc "pacman -Syu --noconfirm && pacman -S --needed --noconfirm mingw-w64-ucrt-x86_64-{gcc,cmake,ninja,vulkan-devel,shaderc,spirv-headers,spirv-tools,glslang}"
rustup toolchain install stable-x86_64-pc-windows-gnu --profile minimal
rustup override set stable-x86_64-pc-windows-gnu           # for this checkout only
```

Then build from any terminal; `build.rs` finds MSYS2 at `C:\msys64\ucrt64` by itself
(set `MSYS2_UCRT64` if it is elsewhere), picks Ninja and links sd-cli statically, so it needs no MSYS2 DLLs at
run time (the sandbox couldn't reach them). Releases are still built with MSVC
(`release.yml`); this route is for a build on your own machine.

Then, once, put stable-diffusion.cpp **inside** this repo, where `build.rs` looks for
it (the folder is gitignored):

```sh
git clone --recursive https://github.com/leejet/stable-diffusion.cpp
```

For the Assistant, put llama.cpp beside it at the tag release builds use (also
gitignored; without it the build works and has no agent):

```sh
git clone --branch v0.7.0 https://github.com/ggml-org/llama.cpp
```

After that it is only:

```sh
cargo build --release      # target/release/yasdig, sd-cli inside — the file to hand out
cargo run --release        # or run it straight from the checkout
```

`build.rs` builds sd.cpp's `sd-cli` into `stable-diffusion.cpp/build-yasdig/` and a
release build carries it. The first build takes a few minutes, most of it ggml's Vulkan
shaders; after that an unchanged sd.cpp costs nothing. A debug build (`cargo build`,
`cargo test`) builds sd-cli too but doesn't embed it, so tests stay quick.

To update sd.cpp, `git -C stable-diffusion.cpp pull && git -C stable-diffusion.cpp
submodule update --recursive`, then build as usual — the next build notices the new
commit. `YASDIG_SD_CLI=/path/to/sd-cli` embeds a binary built elsewhere instead.

The agent's driver is our own small program, `llm/yasdig-llm.cpp`, built against
llama.cpp as a library: `build.rs` configures `llm/` (not llama.cpp's own project) into
`llama.cpp/build-yasdig/`, so only the driver and what it links are compiled — no server,
no TLS. A release carries it as it carries sd-cli. `YASDIG_LLM=/path/to/yasdig-llm`
embeds one built elsewhere. Moving llama.cpp to another commit means checking the
driver against it: it calls llama.cpp's chat-template and multimodal helpers, whose
signatures change between releases (CLAUDE.md, *The agent*, has the list).

A checkout runs the sd-cli named by `[sd].binary` in the repo's `models.toml` — the one
above — and serves the page straight from `web/`, so an edit shows on reload.

**What you build here runs on machines like yours.** sd.cpp is built portable
(`GGML_NATIVE=OFF`, OpenMP off), but the binary still needs your distribution's glibc and
libstdc++ or newer. `.github/workflows/release.yml` makes the files to publish: the same
`cargo build --release`, on Ubuntu 22.04 with libstdc++ linked in, and on Windows.

> If configure fails with *"Could not find a package configuration file provided by
> SPIRV-Headers"*, install `spirv-headers-devel` / `spirv-tools-devel` and build again —
> a failed configure is retried. `yasdig` itself does **not** need `glslc`; its own
> shaders are WGSL, compiled in-process by `wgpu`/naga.
