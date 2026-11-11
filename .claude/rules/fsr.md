---
paths:
  - "src/fsr/**/*"
---

# The FSR post-process

## The four GPU passes

`Fsr::process()` (`src/fsr/mod.rs`) is the single entry point, taking one `Filters`
(`src/config.rs`). A no-op pass is skipped and its textures never allocated. The
`.wgsl` headers are thorough — read them.

- `upscale.wgsl` — **FSR1 EASU**. Skipped at scale 1.0.
- `sharpen.wgsl` — **FSR1 RCAS**. Identity at 0. `FSR_RCAS_DENOISE` deliberately off
  (header has the measurement). **Sharpness 1.0 is the ceiling**: past it `4*lobe + 1`
  goes negative and the filter inverts. More sharpening comes from `rcas_radius`.
- `contrast.wgsl` — **not FSR**: DoG band-pass on luma ("clarity") on a 1/8 plane.
  Identity at 0. **Signed** -1..1 (negative softens).
- `grade.wgsl` — nine grading knobs in **one** pass, in **OKLab** (temperature/tint
  are exactly its chroma axes; header says what they are not). Vibrance is a chroma
  scale faded out by `VIBRANCE_KNEE`. A tenth knob belongs here. Neutral is skipped, since the sRGB/OKLab round trip loses LSBs and
  `fsr-selftest` asserts byte equality. `check_grade` fails a knob that moves nothing
  (the `GradeParams`/shader `Params` copy-paste trap).

**Order is EASU → grade → clarity → RCAS**, and load-bearing: grading after a sharpen
amplifies and clips its halos.
