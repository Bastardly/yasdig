# YASDIG

[![Latest release](https://img.shields.io/github/v/release/Bastardly/yasdig?include_prereleases)](https://github.com/Bastardly/yasdig/releases)
[![VirusTotal](https://img.shields.io/endpoint?url=https%3A%2F%2Fyasdig.com%2Fbadge.json)](https://github.com/Bastardly/yasdig/releases/latest)
[![Release build](https://img.shields.io/github/actions/workflow/status/Bastardly/yasdig/release.yml?label=release%20build)](https://github.com/Bastardly/yasdig/actions/workflows/release.yml)
[![Downloads](https://img.shields.io/github/downloads/Bastardly/yasdig/total)](https://github.com/Bastardly/yasdig/releases)
[![Licence](https://img.shields.io/github/license/Bastardly/yasdig)](LICENSE)
![Windows | Linux](https://img.shields.io/badge/platform-Windows%20%7C%20Linux-informational)
![Vulkan](https://img.shields.io/badge/GPU-Vulkan-red)

**[yasdig.com](https://yasdig.com)** · [Download](https://github.com/Bastardly/yasdig/releases) · [YouTube](https://youtube.com/@yasdig-sd)

**Yet Another Stable Diffusion Image Generator** — a nod to the open-source habit of
calling a thing "yet another" one, and to the many generators before it. 

YASDIG makes pictures on your own computer, with your own graphics card. It is one
file: no Python, no installer, nothing to set up. It's private: There is no account, no telemetry and no update checks. The drawing is done by
[stable-diffusion.cpp](https://github.com/leejet/stable-diffusion.cpp) on Vulkan, so it
runs on AMD, NVIDIA and Intel GPUs alike.

## Why YASDIG: draft cheap, finish what's worth it

Most generators make you pick the expensive settings *before* you know whether the
picture is any good. YASDIG turns that round:

```
  generate small and fast ──▶ keep the good ones ──▶ escalate only those
  (a batch, or a loop)        (every image is        Hi-res · Refine · Refine an area
                               archived with its      Edit · Extend · Upscale
                               settings)              ── each one a step you can undo
```

- **Generate small and fast.** Run a batch, or let it loop while you watch. Every
  image lands in the work-item grid with everything that made it, so any one can be
  reopened and taken further later.
- **Escalate only the keepers**, from the buttons beside the result and the Refine
  dialog:
  - **Hi-res** makes it bigger and adds the detail a plain upscale can't invent.
  - **Refine** goes over the whole picture lightly, or **paint an area** to redraw
    just that part: a hand, a face, a sign.
  - **Edit** changes it by instruction ("make it night", "remove the hat"), with
    Flux.2 Klein.
  - **Extend** grows the canvas and draws what lies beyond the edges, with FLUX.1 Fill.
  - **Upscale** enlarges it with AMD FSR or an upscaler model, drawing nothing new.
- **Any pass can use a different model.** Draft on a fast Turbo model, finish on a
  detailed one.
- **Every pass is a step in the picture's history.** Go back with Ctrl+Z, or compare
  two steps side by side with a split you drag.
- **Colour and sharpness are live and lossless.** Ten sliders (sharpness, clarity and
  eight colour controls) work on the untouched original, so you can change your mind
  as often as you like, and nothing is ever redrawn over pixels that were already
  sharpened.

## Install

You need a 64-bit **Windows 10 or 11** or **Linux** PC with a graphics card and an
up-to-date driver; current AMD, NVIDIA and Intel drivers all include Vulkan. There is
no macOS version.

1. **Download** the archive for your system from [yasdig.com](https://yasdig.com) or the
   [Releases page](https://github.com/Bastardly/yasdig/releases):
   `yasdig-windows-x86_64.zip` or `yasdig-linux-x86_64.tar.gz`.
2. **Extract it** somewhere you can write to — `Documents`, say, not Program Files. On
   Windows, right-click the zip → *Extract All*. You get a `yasdig` folder: keep it,
   everything the app makes goes in there.
3. **Start `yasdig`** in that folder. On Windows, the first start may show *Windows
   protected your PC*: yasdig is open source but not yet signed with a Microsoft
   certificate, so choose *More info → Run anyway*. (Started from inside the zip
   without extracting, it stops and says so: it would be running from a temporary
   folder Windows deletes.)
4. **Your browser opens on the app.** Open the **Get models** tab and download a model
   — one click, with progress. The table below helps you choose.
5. **Back on the Image tab**, type a prompt and press **Generate**.

To stop it, close its console window or press **Close server** on the page. To update,
replace `yasdig.exe` (or `yasdig`) with the one from the new archive: your models,
settings and pictures stay where they are. The folder
ends up holding:

```
yasdig(.exe)       the program
models/            the models you download (each subfolder has a README.txt)
user/              your settings and favourites — never cleared
work/              every generation, and work/output/ for the pictures you save
bin/               stable-diffusion.cpp, unpacked from the program
models.toml, startup.toml   start-up settings; rarely needed
```

**How much graphics memory you need** depends on the model: the table below gives
what each takes on the card. With less than that, the **Memory** setting keeps the
model in system RAM or streams it from the SSD, and when a render runs out of memory
anyway, the error offers the ways out.

YASDIG is developed and tested on Linux with an AMD Radeon RX 7900 XTX (24 GB). Other
Vulkan GPUs and Windows should work; reports are welcome.

## Privacy and security

**Everything stays on your computer.** Your prompts, pictures and settings never
leave it: there is no account, no telemetry and no update check. The only time YASDIG
goes online is when you press download on the **Get models** tab, and then it fetches
that model from Hugging Face, from addresses built into the program.

**The page is for this computer only.** The app is a small web server that listens on
`127.0.0.1`, so other machines on your network can't reach it. It also refuses
requests that come from other websites open in your browser, and requests sent to it
under another name (DNS rebinding). There is no login, so if you change `host` in
`startup.toml` to open the page to your network, anyone who can reach it can use
it — including deleting pictures and stopping the server. Only do that on a network
you trust.

**Model files are read, never run.** There is no Python inside, so a `.ckpt` or
`.pth` file is parsed for its weights and nothing in it is executed. A file it can't
make sense of is refused, not guessed at.

**Saved pictures carry their settings.** *Save to output* and *Download* write the
prompt, model names and settings into the PNG, so others can recreate your picture
and A1111, Forge and Civitai can read it. No file paths are written. If you'd rather
share pictures without that information, turn off *Settings inside saved PNGs* in
Settings → **General**.

**Checking a download.** YASDIG isn't signed with a paid certificate yet, so every
release can be checked three other ways, and the download dialog on
[yasdig.com](https://yasdig.com) shows all three:
- **VirusTotal** scans each archive and the program inside it when a release is
  published; the release page links the reports. Antivirus engines sometimes flag
  new, unsigned open-source programs that are perfectly safe (a false positive). If
  that happens to a release, the report stays public, and we report it to the vendor
  as a false positive and say so on the release page.
- **The SHA-256** of each archive is on the release page and in `SHA256SUMS`: compare
  it with `sha256sum <file>` (Linux) or `certutil -hashfile <file> SHA256` (Windows).
- **GitHub vouches for the build**: `gh attestation verify <file> --repo
  Bastardly/yasdig` checks that the archive was built by this repository's release
  workflow from its public source.

**Found a security problem?** Please report it privately through the repository's
[Security tab](https://github.com/Bastardly/yasdig/security) (*Report a
vulnerability*) rather than in a public issue.

## What's supported

**Model families.** Each but SD 1.x/2.x has a one-click card on the Get models tab:

| family | good for | on the card |
|---|---|---|
| SD 1.x, SD 2.x, SDXL | the huge library of community checkpoints and LoRAs | SDXL: a 6.9 GB file |
| Z-Image Turbo | fast: 8 steps | about 11 GB |
| Flux.2 Klein 4B / 9B | fast (4 steps); takes reference images and can **Edit** | about 9 / 19 GB |
| FLUX.3 Klein / Klein Base | 16 steps / 20-step base; takes reference images and can **Edit** | about 11 GB |
| Flux.1 schnell / dev | 4-step drafts / detailed 20-step renders | about 18.5 GB |
| Krea 2 Turbo | 8 steps | about 15 GB |
| Qwen-Image | the largest here (20B); strong at text in pictures | about 17 GB |
| MOSS-Image | layout and lettering; 50 steps at real CFG, so not fast | about 16 GB |
| FLUX.1 Fill | powers **Extend**; not used for generating | about 18.5 GB |

**Model files** can be `.safetensors`, `.gguf` or `.ckpt`, and are recognised by what
is inside them, so file names and folders don't matter. The Get models tab fetches the
parts a model needs beside it (text encoder, VAE); your own downloads can go anywhere
under `models/`.

**Also in the box:**
- **Image to image:** start from your own picture (upload, drop or paste) instead of
  from noise.
- **References:** Klein (Flux.2 and FLUX.3) can look at up to four pictures while it draws ("the jacket
  from image 2").
- **LoRAs**, checked against the model, so one made for another family is refused
  rather than silently doing nothing.
- **Upscaler models:** ESRGAN, Compact, SPAN and SwinIR, beside the built-in FSR.
- **Prompt tools:** Ctrl+↑/↓ to weight a word, and `{variables}` that roll a different
  word for each image of a batch.
- **Outputs and favourites:** a gallery of what you saved, with the same sliders, and
  favourites that *Delete all* can't touch.
- **Convert:** shrink a model to GGUF (q8, q4 and others) so it fits your card.
- **Settings and per-model defaults:** steps, CFG and the rest, saved once per model.

The app's **Help** tab explains all of it, and every control has an ⓘ.

## More

- [User guide](docs/guide.md): every feature in detail.
- [Configuration](docs/configuration.md): folders, model files and every setting.
- [Command line and HTTP API](docs/api.md).
- [Building from source](docs/building.md), and
  [architecture notes](docs/architecture.md) for contributors.

## Licence

YASDIG is licensed under the [Apache License 2.0](LICENSE). It is powered by
[stable-diffusion.cpp](https://github.com/leejet/stable-diffusion.cpp) (MIT), which is
built into the program; its notices and those of everything else inside are in
[THIRD_PARTY_LICENSES.md](THIRD_PARTY_LICENSES.md) and in the app under Help → *About &
licences*. **Models are not part of YASDIG.** Every model has its own licence, set by
the people who made it, and some forbid commercial use: check the page each Get models
card links before using what you make.

## Support

YASDIG is free and always will be. If it's useful to you and you'd like to help keep
it going, you can [support it on Ko-fi](https://ko-fi.com/yasdig). Thank you!
