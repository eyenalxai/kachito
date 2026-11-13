# Can seeking be instant?

Date: 2026-11-12
Updated: 2026-11-13
Status: answered

## Answer

Not for arbitrary compressed video, if "instant" means the exact requested
frame. A precise seek has to decode every frame between the previous keyframe
and the target; that is the cost the player cannot skip. On the attached
15-second test file (H.264 High, 2560x1440, 60 fps, 2-second GOP), an exact
seek took **74 ms** (74-76 ms across three runs) with software decoding and
**137 ms** with the app's current `hwdec=auto-safe`, while a keyframe seek
took **about 15 ms**. A re-encode with a 10-second GOP took **387 ms** /
**1056 ms** at a 9.0 s target; a 0.5-second GOP took **16-18 ms**. The cost
scales with the distance from the previous keyframe, not with the file size or
the cache.

The app makes the preview truthful and as fast as the decoder allows:

1. **Preview exact frames while the user drags, but coalesce the seeks.** A
   keyframe preview is fast but lands on the previous keyframe, up to one GOP
   away, while the seek bar shows the requested time. The release exact seek
   then lands on a different frame. The app now sends an exact seek for the
   drag preview, keeps one seek in flight, and always sends the latest pointer
   target. On the attached file that gives about 5 to 12 preview updates per
   second (82 to 192 ms per exact seek with `hwdec=auto-safe`; 40 to 82 ms
   with software decoding). On a 10-second GOP it gives about 1 to 2.5 updates
   per second.
2. **Keep the handle at the requested time and show that the picture is
   catching up.** The session holds the timeline position during a scrub and
   does not sync it from `time-pos` until the final seek lands. The seek bar
   dims its handle while an exact seek is in flight.
3. **Keep the exact seek for the final position and for keyboard skips.** On
   release the app sends one exact seek for the release position. If the
   preview already landed within one frame, it skips the redundant seek. The
   export range body drag uses the same preview and final paths.
4. **Consider software decoding for seek latency.** In this measurement,
   `hwdec=auto-safe` made precise seeks about 1.8 times slower than
   `hwdec=no` on the 2-second-GOP file, and about 2.7 times slower on the
   10-second-GOP file, with the same software render path. That is a
   measurement, not a universal rule; it should be checked with the real
   playback audio path and a 4K file before changing the default
   (`src/engine.rs`, lines 255-257).

Truly instant exact seeking on an arbitrary file would require an all-intra
file, a very short GOP, or a proxy/transcode. All of those change the file or
add a preparation step; none is a property a player can add on demand to a
normal H.264/HEVC recording.

The attached file is 15 seconds long. A longer source was not available. The
numbers above are for the attached 15-second file and synthetic re-encodes of
it. A longer GOP on the real test
video would make exact seeking slower than these numbers.

## Evidence

- Exact seek is defined as decoding from the previous keyframe to the target:
  mpv manual, `DOCS/man/options.rst`, `--hr-seek`, lines 395-411:
  "Such seeks require decoding video from the previous keyframe up to the
  target position and so can take some time depending on decoding
  performance."
- Seek flags: `keyframes` is documented as "Always restart playback at
  keyframe boundaries (fast)" and `exact` as "Always do exact/hr/precise seeks
  (slow)"; absolute seeks default to exact: `DOCS/man/input.rst`, lines
  300-326, mpv commit `bcde6b93cca4d128d84e1a60d1a6db2cd81c889e`.
- Dropping pre-target frames during an exact seek is on by default:
  `--hr-seek-framedrop`, default `yes`, `DOCS/man/options.rst`,
  lines 426-433. This helps, but does not remove the decode dependency.
- The demuxer cache only helps when caching is enabled; local files are not
  cached by default: `--demuxer-seekable-cache`, `DOCS/man/options.rst`,
  lines 4389-4405, and `--cache=auto`, lines 5586-5604.
- Demuxer readahead is one second by default for local playback:
  `--demuxer-readahead-secs`, `DOCS/man/options.rst`, lines 4429-4437.
  Increasing it avoids I/O stalls but does not skip decode work.
- mpv's frame queue is off by default and is documented to make seeking
  slower: `--vd-queue-enable`, default `no`, `DOCS/man/options.rst`, lines
  5713-5735. The app does not enable it.
- App seek path: `PlayerEngine::seek` and `PlayerEngine::seek_preview` both
  send `absolute+exact` (`src/engine.rs`, lines 358-400); the engine exposes
  the `seeking` property through `PlayerEngine::is_seeking`
  (`src/engine.rs`, lines 398-400).
- Drag path: `src/app.rs`, lines 801-854, calls `begin_scrub` on pointer down,
  `preview_fraction` on movement past 2 points, and `seek_fraction` on pointer
  up. The session coalesces previews in `src/session.rs`, lines 168-200, and
  issues the next target from `poll` when the engine is idle
  (`src/session.rs`, lines 318-323).
- While scrubbing, the session stops syncing the timeline from `time-pos` and
  resumes after the exact seek lands: `src/session.rs`, lines 310-331.
- The export range body drag uses the same preview path and exacts on release:
  `src/app.rs`, lines 1291-1413.
- Regression tests: `src/engine.rs`, lines 1045-1065, asserts a preview seek
  to 0.25 s lands within one frame; `src/engine.rs`, lines 1067-1092, asserts
  the release after a preview keeps that frame; `src/app.rs`, lines 2322-2341,
  asserts the session keeps the latest target while a seek is in flight;
  `src/app.rs`, lines 2344-2387, assert the release skip and the busy-engine
  guard.
- The app selects `hwdec=auto-safe` for non-headless playback and `no` for
  headless tests: `src/engine.rs`, lines 255-257. On this machine, through the
  app's own `vo=libmpv` software-render path, mpv resolved `auto-safe` to
  `hwdec-current=vulkan-copy` for the attached H.264 file, so decoding is
  hardware-accelerated with copy-back. `hwdec=no` forces software decoding.
- Attached file properties: H.264 High, 2560x1440, 60 fps, AAC stereo,
  15.000 s, 8,088,435 bytes. Keyframe packets are at -0.1, 1.9, 3.9, 5.9,
  7.9, 9.9, 11.9, 13.9 s, i.e. a 2.0 s GOP, measured with
  `ffprobe -select_streams v:0 -show_packets -show_entries
  packet=pts_time,flags`.
- Measurement table, libmpv 0.41.0, ffmpeg 9.0.2, AMD Ryzen 7 5700X,
  16 threads, software render to 1280x720 `bgr0`, `vo=libmpv`,
  `ao=null`, `pause=yes`, `video-sync=audio`, seek from 0 to target.
  `restart` is the time from the seek command to
  `MPV_EVENT_PLAYBACK_RESTART`, i.e. when the seek is complete.
  Benchmark source: `docs/research/seek-bench.c`.

  | Source | GOP | Mode | Target | hwdec | Time to restart |
  |---|---:|---|---:|---|---:|
  | attached 15 s | 2 s | exact | 7.0 s | no | 73.8, 76.1, 74.3 ms |
  | attached 15 s | 2 s | exact | 7.0 s | auto-safe | 137.2, 137.4, 137.9 ms |
  | attached 15 s | 2 s | keyframes | 7.0 s | auto-safe | 14.3, 17.2, 15.1 ms |
  | synthetic 0.5 s GOP | 0.5 s | exact | 9.0 s | no / auto-safe | 17.9 / 15.7 ms |
  | synthetic 10 s GOP | 10 s | exact | 9.0 s | no / auto-safe | 387.0 / 1056.0 ms |

  The synthetic files are re-encodes of the attached content with
  `-g 30` and `-g 600`, `-keyint_min` equal to `-g`, and
  `-sc_threshold 0`, so keyframes occur only at the requested interval.

  The preview runs on the attached file took 82.0, 139.6, 192.1 and 158.7 ms
  at 2.5, 3.0, 5.5 and 9.2 s with `hwdec=auto-safe`, and 40.5 and 82.3 ms at
  2.5 and 5.5 s with `hwdec=no`. The keyframe runs at 2.5, 3.0, 3.5, 4.2, 5.0,
  5.5, 6.4, 7.0 and 9.2 s landed at 1.9, 1.9, 1.9, 3.9, 3.9, 3.9, 5.9, 5.9
  and 7.9 s. The exact runs landed on the requested times.

## Conflicts

- The mpv manual says exact seeks "can take some time depending on decoding
  performance" but does not quantify it. The measurement above supplies the
  numbers for this file and machine; it does not contradict the manual.
- The earlier feasibility note found `hwdec=no` and `hwdec=auto-safe` within
  noise for *software-render cost*. That is a different metric. The present
  measurement is *seek completion*: hardware decode adds decoder/surface work
  on a seek that the render-cost benchmark did not exercise. Both can be true;
  use the metric that matches the user-visible problem.
- Keyframe seek is fast, but it lands on a keyframe, not on the requested
  time. The keyframe can be up to one GOP early (or, for some demuxers, the
  next keyframe). For a 2 s GOP that is at most 2 s of error; for a 10 s GOP
  it is not acceptable as the final position. The app does not use it for the
  drag preview: the frame would not match the handle or the release frame.

## Not found

- The source video used in the test, if it was longer than the attached 15 s
  file, was not found on disk. Its keyframe interval and decode cost are
  unknown. The attached file is the only test asset measured.
- No measurement through the app's real audio output. The benchmark uses
  `ao=null`; audio device buffering can add to the time before playback
  resumes, but the exact frame becomes available on the video path measured
  here.
- No 1080p, 4K, HEVC or AV1 measurement. The cost model predicts a roughly
  linear increase with the number of frames between keyframes and with
  resolution; it was not measured.

## References

- Attached test video: `screenrecording-2026-11-12_14-07-36.mp4`, 15.000 s,
  2560x1440, 60 fps, H.264 High. Saved by T3 at
  `/home/ulezot/.t3/userdata/attachments/d260e77e-1018-4ecf-b390-c028dba3b41a-9425c3b0-e4e9-4bb6-970a-ff2694925c5a-mp4.mp4`.
- mpv repository: `/home/ulezot/Projects/other/mpv`, commit
  `bcde6b93cca4d128d84e1a60d1a6db2cd81c889e`, 2026-11-09. The seek command
  and `--hr-seek` semantics live in `DOCS/man/input.rst` and
  `DOCS/man/options.rst`.
- Kachito seek path: `src/engine.rs`, lines 358-400; `src/session.rs`, lines
  141-200 and 310-331; `src/app.rs`, lines 127-160, 801-854 and 1291-1413.
- Decision record: `docs/adr/0001-exact-coalesced-preview.md`.
- Benchmark: `docs/research/seek-bench.c`. Local, not part of the product.
