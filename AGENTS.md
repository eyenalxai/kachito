# Agent notes

## Project

Kachito is a Linux desktop video player built with Rune. The architecture is Rune for the UI, libmpv for playback, and ffmpeg for clip export. The approved spec is `.scratch/kachito/spec.md`; the mission record is `.scratch/kachito/map.md`.

## Tracker

Issues and specs are local markdown under `.scratch/kachito/`. One feature directory, a spec, and one ticket per file under `issues/`. Status lines use `open`, `claimed`, `ready-for-agent`, or `resolved`. The tracker conventions are in the `issue-tracker-local` skill.

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
