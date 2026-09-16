# Next steps

Written 2026-09-15. Everything below assumes `aa2b20e "Now with more refinement"`,
which is the last commit and has the whole of today's work in it: the RCAS
sharpness fix, refine moved to output resolution, sharpen-once-at-the-end, the
strength-scaled step floor, the 0.95 strength cap, and the clarity band-pass.



## 2. Recommended next build: a separate CFG scale for the img2img passes

**Why.** This is the other half of the "dirty" problem and the last untouched
lever on it. CFG 5 is right for a base render at 768²; at 2304² with few steps it
over-guides, and over-guidance shows up as high-frequency artifacts — the same
crunch the step floor attacks from the other side. Dropping to ~2–3 for the
img2img passes is the conventional cure. It was deliberately deferred yesterday
so the step floor could be judged on its own first.

**Design.** One optional field rather than a per-pass pair, to avoid knob sprawl:

```rust
// src/config.rs, on Template/Defaults/Profile
/// CFG scale for the refine and hi-res passes. `None` = use `cfg_scale`.
/// Lower is usually right: the base render's CFG over-guides at output
/// resolution, and over-guidance reads as high-frequency crunch.
#[serde(default)]
pub img2img_cfg_scale: Option<f32>,
```

`SdRunner::base_cmd` (`src/sd.rs`) currently reads `p.cfg_scale` directly. Give
it a `cfg_scale: f32` parameter; `txt2img` passes `p.cfg_scale`, `img2img` passes
`p.img2img_cfg_scale.unwrap_or(p.cfg_scale)`. Then the usual plumbing, following
the `sharpness` pattern exactly: `GenRequest` override, the `/generate` DTO,
`build_models_json`, a UI input beside CFG scale, and `models.toml` + `README.md`.

**Validation.** Range-check in `Profile::validate()` like the others — reject
negatives; `0.0` is meaningful (CFG off) so the floor is 0, not a minimum.

**Verification.** Same seed, same everything, refine at `0.30` with CFG 5 vs 2.5.
If the crunch drops without the image going flat or losing prompt adherence, it
ships with a lower default.

**Alternative considered:** separate `refine_cfg_scale` / `hires_cfg_scale`, for
symmetry with the existing `*_steps` / `*_strength` pairs. Rejected for now —
there's no reason yet to think the two passes want different values, and the
panel is already dense. Easy to split later if verification says otherwise.

---

## 3. Smaller loose ends

- **Orphaned intermediates on a failed pass.** If `sd-cli` fails mid-refine, the
  partially-written `{id}-{step}-raw.png` isn't tracked by any `Pending` and only
  gets cleared by `purge_pngs` on the next restart. Harmless, bounded, but untidy.
- **The `hires_max_side` name.** It now means "the largest side the *hi-res* pass
  may enlarge to"; refine deliberately isn't under it. The name is fine, the
  confusion was mine — noted only so it doesn't get "fixed" later by mistake.
- **Re-sharpen without re-diffusing.** `Pending` already retains the unfiltered
  `raw`, so changing sharpness or clarity on an existing image could be a
  millisecond GPU pass instead of a full regenerate. Not built — there's no
  endpoint for it — but the hard part is already done.

---

## 4. Reference: what was measured

Kept here so it doesn't have to be re-derived.

**Clarity band-pass**, 1536² synthetic image with four planted frequencies,
amplitude relative to the input:

| band | clarity 0.3 | 0.6 | 1.0 |
|---|---|---|---|
| whole-image tone | 0.99× | 0.98× | 0.97× |
| depth (400 px) | 1.48× | 1.97× | 2.61× |
| detail (40 px) | 1.13× | 1.26× | 1.43× |
| grit (3 px) | 0.99× | 0.98× | 0.96× |

`clarity 0.0` is byte-identical to its input (max channel diff 0). Cost at 3072²
is ~1 ms (143 ms off vs 144 ms on; the rest is PNG encode/decode).

An earlier version of this shader was a plain unsharp mask, and measured the
wrong way round — grit 1.89×, depth 1.04×. It is a *band-pass* (difference of two
Gaussians on a 1/8 luma plane) specifically so the fine band is excluded. Don't
simplify it back to `src - blur`.

**Step floor**, at `steps = 20`: strength 0.05 stays at 2 effective steps, 0.30
goes 2 → 6, 0.90 goes 2 → 18. The schedule handed to sd.cpp lands on 20 at every
strength, so the pass matches the base render's step density — the same semantics
sd-webui uses.

**RCAS sharpness**: 0.05 vs 0.95 at 2× differs by 128/255 at most. `0.0` is an
exact identity.

**Ground truth for the step arithmetic** is `src/pipeline/image.cpp:132-185` and
`make_hires_sigma_schedule` in `src/pipeline/request.cpp:427` in the
stable-diffusion.cpp checkout at `../stable-diffusion.cpp`.
