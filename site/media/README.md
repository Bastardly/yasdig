# yasdig.com's screenshots and clips

Pages name a file here with `<media src="…" alt="…" caption="…">`. Until it exists,
the preview (`node site/serve.mjs`) shows a labelled empty frame in its place and the
deployed page leaves it out; `node site/build.mjs` lists every missing one with what
it should show. This README isn't copied into the site.

- **Screenshots**: `.png` (`.webp` and `.jpg` work too if the page names them), at most 1600 px wide. The build reads the size
  from the file, so the page keeps its room while it loads.
- **Clips**: `.webm` (VP9), at most 1280 px wide, under 10 s and about 3 MB, no sound
  needed (they play muted). Put a still beside each with the same name — `hero.webm`
  and `hero.png` — and it is the poster shown before it plays, and to anyone who
  asked their system for less motion.
- **`og.png`**: 1200×630, the picture a shared link shows. Optional; with it, every
  page's preview carries it.

To make a webm from a screen recording:

```sh
ffmpeg -i in.mp4 -vf "scale=1280:-2,fps=30" -c:v libvpx-vp9 -b:v 0 -crf 36 -an -t 10 out.webm
```
