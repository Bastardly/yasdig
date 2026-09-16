# Next steps

Written 2026-09-15. Everything below assumes `aa2b20e "Now with more refinement"`,
which is the last commit and has the whole of today's work in it: the RCAS
sharpness fix, refine moved to output resolution, sharpen-once-at-the-end, the
strength-scaled step floor, the 0.95 strength cap, and the clarity band-pass.




## 3. Smaller loose ends

- **Orphaned intermediates on a failed pass.** If `sd-cli` fails mid-refine, the
  partially-written `{id}-{step}-raw.png` isn't tracked by any `Pending` and only
  gets cleared by `purge_pngs` on the next restart. Harmless, bounded, but untidy.
- **The `hires_max_side` name.** It now means "the largest side the *hi-res* pass
  may enlarge to"; refine deliberately isn't under it. The name is fine, the
  confusion was mine — noted only so it doesn't get "fixed" later by mistake.
- ~~**Re-sharpen without re-diffusing.**~~ **Built 2026-09-16.** `POST /adjust`
  (`Engine::adjust`) re-runs `publish()` on the retained `raw`, and the two
  controls are now live sliders under the image rather than generation settings.
  The new values are retained on the profile, so a later refine or hi-res keeps
  them. If a drag feels sluggish at 3072², the two things to try, in order, are
  caching the decoded `raw` pixels (one entry — 3072²×4 is 37 MB) and replacing
  the base64 data URL with a binary `GET`; both target PNG/transfer cost, not GPU
  cost, which is ~1 ms.

---

## 4. Reference: what was measured

Kept here so it doesn't have to be re-derived.

**Clarity band-pass**, 1536² synthetic image with four planted frequencies,
amplitude relative to the input.

> **Stale as of 2026-09-16, in a known way.** The gain multiplier went 3.0 → 2.0
> (the top of the range read as halo, not depth), so every row below now needs
> two-thirds of its boost: a number on the slider today does what `value / 1.5`
> did when this was measured. The *shape* is unchanged — it is the same band-pass
> with the same sigmas — so the point the table makes still stands. Re-measure
> before quoting the figures themselves. The knob also runs to -1 now; the
> negative half has not been measured at all.

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
exact identity. Measured at tap radius 1, which is still what images up to ~2300
px get; larger ones now sample the cross further out (`rcas_radius`), and that
has not been measured.

`1.0` is a hard ceiling, not a soft one — it is `FsrRcasF` at 0 stops. Anything
that wants "more sharpening than maximum" has to either widen the tap distance
(done) or let the lobe exceed the ring limiter, which means accepting overshoot.
The second is a real option and the conventional one outside FSR — it is what
makes an unsharp mask read as punchy — but it trades away the "ringing excluded
by construction" property the shader header advertises, so it should be a
separate, measured decision rather than a constant someone nudges.

**Ground truth for the step arithmetic** is `src/pipeline/image.cpp:132-185` and
`make_hires_sigma_schedule` in `src/pipeline/request.cpp:427` in the
stable-diffusion.cpp checkout at `../stable-diffusion.cpp`.
