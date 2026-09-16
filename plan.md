# Next steps

Written 2026-09-15, last revised 2026-09-16. §1 and §2 are built and struck; what
remains below is reference rather than work.

A note on reading it: measurements move when constants move, and this file can't know
that it has gone stale. So the numbers that *can* be re-derived on demand live in
`cargo run --release -- fsr-selftest` now, and what's kept here is the reasoning behind
the design, which doesn't rot the same way.




## 3. Smaller loose ends

- ~~**Orphaned intermediates on a failed pass.**~~ **Built 2026-09-16.** `Scratch`
  (`src/pipeline.rs`) is a drop guard: each pass registers every file it is about to
  write, and promotes only `raw` and `display` out of the set once it has succeeded.
  An early `?` — an `sd-cli` that ran out of VRAM, most often — now cleans up after
  itself, and the genuine intermediates (`lowres`, `upscaled`, `hires`) went into the
  same mechanism, so one rule covers both paths instead of a `remove_file` for success
  and nothing for failure. `adjust` is deliberately *not* under it: it writes over a
  file a live `Pending` owns, so a failed drag would delete the image on screen.
- ~~**The `hires_max_side` name.**~~ Now `CLAUDE.md` invariant 7, which is where a
  "don't "fix" this later" note belongs. Removed from here rather than kept in two
  places.
- ~~**Re-sharpen without re-diffusing.**~~ **Built 2026-09-16.** `POST /adjust`
  (`Engine::adjust`) re-runs `publish()` on the retained `raw`, and the two controls are
  now live sliders under the image rather than generation settings.

  The new values are retained on the **`Pending`**, not on the profile — an earlier
  draft of this line said otherwise and was wrong. The profile's values are where the
  sliders *start*, and every new image (generate, refine, hi-res) starts there again,
  because an escalation replaces the pixels and so the amount of sharpening that looked
  right is stale by definition. Putting the live value back on the profile compiles
  fine and silently restores the old inherit-forever behaviour; see
  `Pending::sharpness` and the warning in `CLAUDE.md`.

  If a drag feels sluggish at 3072², the two things to try, in order, are caching the
  decoded `raw` pixels (one entry — 3072²×4 is 37 MB) and replacing the base64 data URL
  with a binary `GET`; both target PNG/transfer cost, not GPU cost, which is ~1 ms.

---

## 4. Reference: the arguments, and where the numbers live

**The numbers are a command now.** `cargo run --release -- fsr-selftest` plants four
gratings whose wavelengths are fractions of the image, runs the filters, and reports how
much each band moved — at 1536² and 3072², so the claim that a setting means the same
thing at both sizes (`CLAUDE.md` invariant 6) is a comparison rather than an assertion.
It also *asserts* that scale 1.0 with both filters at 0 is byte-exact, and prints the
per-pass cost. Run it after touching a filter constant; a table pasted into a file here
would only go stale again, which is exactly what happened to the one this section used
to hold.

What it can't tell you is which value looks good. That was what the old table was for,
and the sliders answer it better by being dragged.

The rest of this section is argument rather than measurement, so it stands on its own.

**Clarity is a band-pass, not a sharpener.** An earlier version of the shader was a
plain unsharp mask (`src - blur`) and measured the wrong way round — it lifted the 3 px
grit band 1.89× while moving the 400 px depth band only 1.04×, i.e. it amplified exactly
the diffusion noise it was meant to see past. It is a difference of two Gaussians on a
1/8 luma plane specifically so the fine band is excluded. Don't simplify it back.

**RCAS 1.0 is a hard ceiling, not a soft one** — it is `FsrRcasF` at 0 stops. Anything
that wants "more sharpening than maximum" has to either widen the tap distance (done —
`rcas_radius`) or let the lobe exceed the ring limiter, which means accepting overshoot.
The second is a real option and the conventional one outside FSR — it is what makes an
unsharp mask read as punchy — but it trades away the "ringing excluded by construction"
property the shader header advertises, so it should be a separate, measured decision
rather than a constant someone nudges.

**The two filters hold invariant 6 differently, and RCAS holds it in steps.**
Measured 2026-09-16 on a 7900 XTX. Clarity is smoothly proportional and comes back
within a few percent across a 2× size change (depth 1.402× → 1.406×, grit 0.997× →
0.996×). RCAS can't be: `rcas_radius` is `round(W/1536).clamp(1,3)`, so the tap distance
moves in whole pixels while the image moves continuously. It is therefore exact at size
ratios that happen to match a step, and drifts inside a plateau. Measured on the 3 px
band at full sharpness: **3.245× at 1536², 2.481× at 2048², 3.245× again at 3072²**. The
first and last are both exact because doubling the image doubled the radius — identical
numbers by arithmetic, not by the invariant holding, which is why that pair alone proved
nothing and 2048 was added. 2048 is the honest reading: same radius, bigger picture, 24%
of the sharpening gone. And 2048 is only a third of the way into the radius-1 plateau,
which runs 768..2303, so the full swing is larger than that.

Still far better than the fixed footprint it replaced — that was a 3× drift over the same
range — but it is a sawtooth, not a line. Removing the teeth means a fractional tap
distance, which costs the limiter its "ringing excluded by construction" guarantee. Per
`sharpen.wgsl`'s own header, that is a separate and measured decision, not a constant to
nudge. Left as found, and `check_bands` deliberately doesn't assert it away.

**Clarity stops scaling at 3200 px**, separately. `clarity_sigmas` clamps its wide lobe
at 16 low-res pixels, which 0.04 × side / 8 reaches at 3200. Past that the clarity band
is fixed in pixels again. Not a bug — the lobe has to stop somewhere — but it is
untested above 3072, and `fsr-selftest` says so in its output when the clamp is active.

**Clarity's band is broad and peaks nearer the fine end than the name suggests** — see
`clarity_sigmas`, which now carries the measured shape. The old table in this section
had it inverted, claiming the 400 px band moved more than the 40 px one; the current
numbers match what the sigmas predict to within the box filter and the midtone
protection, so the old one was wrong rather than merely out of date. Which is the
argument for the command over the table, made by the table itself.

**Step floor**, at `steps = 20`: strength 0.05 stays at 2 effective steps, 0.30 goes
2 → 6, 0.90 goes 2 → 18. The schedule handed to sd.cpp lands on 20 at every strength, so
the pass matches the base render's step density — the same semantics sd-webui uses. This
one is arithmetic rather than a filter measurement, so it doesn't move when a shader
constant does.

**Ground truth for the step arithmetic** is `src/pipeline/image.cpp:132-185` and
`make_hires_sigma_schedule` in `src/pipeline/request.cpp:427` in the
stable-diffusion.cpp checkout at `../stable-diffusion.cpp`. Both re-checked 2026-09-16
and still exact.

---

## Open

Whether `img2img_cfg_scale` should ship with a default rather than unset. It is a
measurement, not a judgement call: same seed, sharpness at 0, refine at strength 0.30
with the box blank versus 2.5, and see whether the crunch drops without the image going
flat. If refine and hi-res want different values, that is the evidence for splitting the
knob per pass.
