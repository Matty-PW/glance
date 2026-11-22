# Images for the README

The top-level README has commented-out placeholders for these. Add the files
here, then uncomment the matching lines in `README.md`.

| File | What | How |
|---|---|---|
| `logo.png` | The glance logo, square, transparent background, at least 256×256 | TODO (maintainer): design |
| `demo.gif` (or `demo.mp4`) | One question and its spoken answer, 10 to 20 seconds, about 720 px wide | A screen recording of a real turn, or of `--overlay-demo` (see CONTRIBUTING.md, "Recording a demo") |
| `overlay-listening.png`, `overlay-speaking.png` | The overlay in two states, light and dark | `glance ui-snapshots` writes all states to `eval-local/ui/`; copy the ones you want |
| `settings.png` | The settings window (Voice or Models section) | `glance ui-snapshots`, or a real screenshot (`screencapture -W`) |

Rules for anything added here:

- No real screen content: use the mock screens in `eval/scenes/` or the
  demo text from `--overlay-demo`, and check for names, emails, file paths
  and notifications before committing.
- Keep each file under 2 MB (the pre-commit hook refuses larger files);
  optimise GIFs (for example with `gifsicle -O3 --lossy=80`).
- `*.png` is ignored everywhere else in the repository; only this folder
  is allowed, in `.gitignore` and in the pre-commit hook.
