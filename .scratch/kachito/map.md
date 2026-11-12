# Kachito

Status: active
Scope: fog
Phase: implement
Goal: A Linux desktop player for local video with a timeline and clip export.
Exit: PRs for review and merge

## Problem

Local MKV and MP4 files cannot be watched in a Rune desktop app on Linux because Rune's built-in player is macOS-only and reads a narrow set of formats. The player also needs a timeline selection and a clip export that drops subtitle tracks.

## Flow

- [x] grill: the decision frontier was answered in conversation; the research note holds the evidence
- [x] map: the decision map is the `.scratch/kachito` record
- [x] decisions: engine, formats, platform, subtitle scope and clip behaviour
- [x] spec: `.scratch/kachito/spec.md`
- [x] tickets: `.scratch/kachito/issues/`
- [ ] implement: in progress, first ticket
- [ ] code-review: pending
- [ ] merge: pending

## Gates

| Scope | Gate | Method | Document | Status | Date |
| --- | --- | --- | --- | --- | --- |
| Fog | Decisions | wayfinder and research | `.scratch/kachito/spec.md` | approved | 2026-11-12 |
| Fog | Scope and spec | grill and spec draft | `.scratch/kachito/spec.md` | approved | 2026-11-12 |
| Fog | Tickets | decision | `.scratch/kachito/issues/` | approved | 2026-11-12 |
| Fog | PR | code-review | pending | pending | |

## Session plan

| Session | Work | Exit |
| --- | --- | --- |
| 1 | classify, grill, draft the spec and cut the tickets | the design gate decisions |
| 2 | implement the first frontier as a stack, review each PR | the stack's PRs are open; the owner merges |
| 3 | after merges unblock it, implement the next frontier | the next stack's PRs are open |

The concurrency cap is ten.

## References

| Reference | Why | Path or URL | Phase |
| --- | --- | --- | --- |
| Rune checkout | UI framework, `NativeSurface` and the pinned `dev` revision | `/home/ulezot/Projects/other/rune`, commit `f18ead891aba2437674cd8c54e238287cceb8b3a` | implement |
| mpv checkout | Render API, options and encoding behaviour | `/home/ulezot/Projects/other/mpv`, commit `bcde6b93cca4d128d84e1a60d1a6db2cd81c889e` | implement |
| Research note | Frame-path evidence and software-render measurements | `docs/research/video-player-feasibility.md` | implement |
| Benchmark source | Reproduces the render measurements | `docs/research/video-player-bench.c` | implement |
| FFmpeg documentation | Stream copy and seeking behaviour | https://ffmpeg.org/ffmpeg.html | implement |

## Decisions

- Playback engine is libmpv. Why: it covers the required containers, codecs, audio tracks, audio output and one subtitle track without a custom player core. Alternative: a direct libav* pipeline, rejected because it owns the clock, audio output, seek and subtitle compositing. Status: settled.
- Video frames reach Rune through `NativeSurface::push_bytes`. Why: Rune's built-in `Player` is macOS-only, and the byte surface is the only external-frame path on Linux. The `bgr0` fourth byte is set to 255 before the push. Alternative: Rune's `Player`, rejected because it is macOS-only and format-limited. Status: settled.
- The first release shows at most one subtitle track. Why: multi-track subtitle rendering is the largest custom piece and is not required now. Alternative: the libass overlay, deferred. Status: settled.
- Clip export uses ffmpeg, keeps video and audio, drops subtitles, and defaults to Matroska. Why: ffmpeg can stream-copy or re-encode, while mpv always re-encodes. Alternative: mpv encoding mode, rejected because it cannot stream-copy and is slower. Status: settled.
- The first release targets Linux, SDR, 1080p60 and 4K30. Why: the measured software-render path is comfortable in that range, and the machine used for development is Linux. Alternative: 4K60 and HDR, deferred because they need GPU interop and Rune's surface is SDR. Status: settled.
- The application source is MIT where the linked components allow it. A distributed build that links GPL components is GPL. Why: this keeps the source as permissive as the dependencies allow. Alternative: GPL from the start, kept only if an MIT-compatible distribution is not practical. Status: settled.
- The first release is a development build using the system libmpv and ffmpeg. Why: it gets a working player first and defers bundling. Alternative: an AppImage or Flatpak first, deferred. Status: settled.

## Rejected options

- A direct libav* pipeline: owns the player core, including the clock, audio output, seek and subtitle compositing. Do not re-offer for the first release.
- GStreamer: needs custom multi-subtitle compositing and heavier macOS and Windows packaging. Do not re-offer for the first release.
- Rune's built-in `Player`: macOS-only and format-limited. Do not re-offer for the first release.
- mpv `--wid` child-window embedding: no Wayland support and breaks Rune's compositing. Do not re-offer.
- Three or more simultaneous subtitles in the first release: deferred; the libass overlay is the later path.
- Subtitles in exported clips: out of scope; export keeps video and audio only.
- 4K60 and HDR in the first release: deferred; the SDR byte surface and software renderer are the current path.
- A bundled AppImage or Flatpak in the first release: deferred until the player works.

## Outputs

- spec: `.scratch/kachito/spec.md`
- tickets: `.scratch/kachito/issues/`
- PRs: pending

## Follow-ups

- Multi-subtitle overlay and bitmap subtitle compositing, if the player needs them.
- GPU interop for 4K60 and HDR, if the SDR byte path is not enough.
- Bundled distribution with LGPL libmpv and ffmpeg, or a GPL distribution.
- Remote playback from the Rails service, deferred.

## Next step

- Skill to load: `implement`
- Gate: PR
- Method: implement the first ticket and open a reviewable change
- Why: the ticket graph is approved and the first ticket has no blockers
- Unlocks: playback with picture and sound, the base for track selection, timeline and export
- Command: `/mission continue`
- Produce: the first ticket's implementation and a reviewable PR
