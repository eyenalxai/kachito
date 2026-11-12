# Agent notes

## Project

Kachito is a Linux desktop video player built with Rune. The architecture is Rune for the UI, libmpv for playback, and ffmpeg for clip export. The approved spec is KACHITO-1; the mission record is KACHITO-6.

## Agent skills

### Issue tracker
Issues and specs live in Linear on the KACHITO team. See `docs/agents/issue-tracker.md`.

### Triage labels
The default role labels are used. See `docs/agents/triage-labels.md`.

### Domain docs
Single-context: `GLOSSARY.md` and `docs/adr/`. See `docs/agents/domain.md`.

## Commands

- `cargo build`
- `cargo test`
- `cargo run -- /path/to/video.mkv`

## Constraints

- Linux first. The frame path is `NativeSurface::push_bytes` with BGRA.
- One subtitle track at most in the first release. No libass overlay.
- Clip export drops subtitle tracks.
- Rune is pinned to a `dev` revision; do not float it.
- The application source is MIT. A distributed build that links GPL libmpv or GPL ffmpeg is GPL.
