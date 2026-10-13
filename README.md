# YASDIG

**[yasdig.com](https://yasdig.com)** · [Download](https://github.com/Bastardly/yasdig/releases)

**Yet Another Stable Diffusion Image Generator** — a nod to the open-source habit of
calling a thing "yet another" one, and to the many generators before it. It was called
*rustimage* until October 2026.

YASDIG makes pictures on your own computer, with your own graphics card. It is one
file: no Python, no installer, nothing to set up. The drawing is done by
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

1. **Download** the file for your system from the
   [Releases page](https://github.com/Bastardly/yasdig/releases):
   `yasdig-windows-x86_64.exe` or `yasdig-linux-x86_64`.
2. **Put it in a folder of its own** that you can write to — `Documents/yasdig`, say,
   not Program Files. Everything it makes is kept in that folder.
3. **Start it.** On Windows, double-click it. On Linux, make it executable once
   (`chmod +x yasdig-linux-x86_64`, or tick "allow executing" in its properties), then
   run it.
4. **Your browser opens on the app.** Open the **Get models** tab and download a model
   — one click, with progress. The table below helps you choose.
5. **Back on the Image tab**, type a prompt and press **Generate**.

To stop it, close its console window or press **Close server** on the page. To update,
replace the file: your models, settings and pictures stay where they are. The folder
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

## What's supported

**Model families.** Each but SD 1.x/2.x has a one-click card on the Get models tab:

| family | good for | on the card |
|---|---|---|
| SD 1.x, SD 2.x, SDXL | the huge library of community checkpoints and LoRAs | SDXL: a 6.9 GB file |
| Z-Image Turbo | fast: 8 steps | about 11 GB |
| Flux.2 Klein 4B / 9B | fast (4 steps); takes reference images and can **Edit** | about 9 / 19 GB |
| Flux.1 schnell / dev | 4-step drafts / detailed 20-step renders | about 18.5 GB |
| Krea 2 Turbo | 8 steps | about 15 GB |
| Qwen-Image | the largest here (20B); strong at text in pictures | about 17 GB |
| FLUX.1 Fill | powers **Extend**; not used for generating | about 18.5 GB |

**Model files** can be `.safetensors`, `.gguf` or `.ckpt`, and are recognised by what
is inside them, so file names and folders don't matter. The Get models tab fetches the
parts a model needs beside it (text encoder, VAE); your own downloads can go anywhere
under `models/`.

**Also in the box:**
- **Image to image:** start from your own picture (upload, drop or paste) instead of
  from noise.
- **References:** Klein can look at up to four pictures while it draws ("the jacket
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
