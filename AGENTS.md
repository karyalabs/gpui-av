# Agent guide

- The plan of record is `docs/spec.md`. Update it in the same pull request as
  any work that changes the plan.
- Experiments live in `experiments/<id>-<slug>/` with a README: question,
  setup, result, numbers (CPU, memory, dropped frames, machine). An
  experiment is done when its answer is folded into the spec.
- Never commit video files. Generate test clips with a script.
- Never bundle codecs (ffmpeg, GStreamer plugins). Use the platform's own.
- Keep `unsafe` and platform types out of the public API.
- This is a Karya Labs repo: CI, releases and process follow
  `karyalabs/matty` `docs/engineering-process.md`.
