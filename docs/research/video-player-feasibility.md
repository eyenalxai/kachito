# Can a Rune app play local video with full track, subtitle and clip control?

Date: 2026-11-12
Status: answered

## Answer

Yes. The practical architecture is Rune for the interface, libmpv for playback, libass for a subtitle overlay when more than two tracks must be visible at once, and the ffmpeg executable for clip export. The app can stay a normal Rune application; it does not need a Rune fork.

The important constraint is Rune's own media API. `Player` (alias `Video`) is implemented with AVPlayer on macOS only. On every other platform it reports `Failed("video playback isn't supported on this platform yet")` and draws a grey placeholder (`crates/rune/src/video/mod.rs`, lines 26 and 38-41; `crates/rune/src/video/unsupported.rs`, lines 1-30). Even on macOS it covers mp4/mov with H.264 or HEVC, and rejects video in Ogg, WebM or Matroska (`crates/rune/README.md`, lines 1957-1996). It is not a base for "all formats".

Rune does expose a portable frame slot. `NativeSurface::push_bytes` accepts packed BGRA or NV12 planes and draws them as a normal Rune element (`crates/rune/src/native_surface/mod.rs`, lines 34-43 and 229-259; `examples/video-tiles/README.md`, lines 26-31). On Linux that byte path is the only external-frame path; macOS also has zero-copy IOSurface import, and Windows has D3D shared textures (`crates/rune/src/native_surface/mod.rs`, lines 1-8). An external player can therefore decode frames and hand them to Rune without changing Rune's renderer.

libmpv is the shortest route to the requested feature set. It keeps libavformat, libavcodec, the audio output, libass, track selection and the playback clock inside mpv, and exposes a software render API that writes a complete frame to caller-owned memory (`include/mpv/render.h`, lines 126-140 and 362-426). A frame can be handed to `NativeSurface::push_bytes` with no extra decode step. The alternative, using libav* directly, means implementing the clock, audio output, seek and flush behaviour, track switching and subtitle compositing. That is a player core, not an integration.

Measured on the test machine (AMD Ryzen 7 5700X, 16 threads, mpv 0.41.0, ffmpeg 9.0.2), libmpv's software render averaged 2.2-3.5 ms per 1920x1080 frame, including one subtitle track, and 6.0-6.4 ms per 3840x2160 frame without subtitles. The render is single-threaded, and the measurement excludes the Rune upload. At 1080p the BGRA frame is 8.3 MB, so the upload should cost roughly 1-2 ms on PCIe; at 4K it is 33 MB, roughly 3-6 ms. That leaves 1080p60 comfortable and 4K30 comfortable; 4K60 is plausible but needs an upload benchmark before it is promised.

Two details need a small amount of code. mpv's software renderer writes `bgr0` or `rgb0`, whose fourth byte is documented as uninitialised garbage (`include/mpv/render.h`, lines 364-382). Rune's BGRA shader multiplies RGB by alpha (`crates/rune/src/native_surface/video.wgsl`, lines 22-23), so those frames can become transparent. Either fill the fourth byte with 255 on the decode thread, or add an opaque BGRx format to Rune. Adding the format is cleaner. Also, Rune's surface is SDR only: wide-gamut conversion, HDR, interlacing and rotation metadata are not implemented (`examples/video-tiles/README.md`, lines 70-78).

Subtitles have a hard limit in every general-purpose engine. mpv can display two tracks at once with `--sid` and `--secondary-sid`; the secondary is rendered as a top title, its styling and formatting tags are stripped, and bitmap subtitles selected as secondary overlap the primary (`DOCS/man/options.rst`, lines 2540-2575). GStreamer's `subtitleoverlay` has one subtitle sink pad, so multiple tracks require chaining overlays or custom compositing (GStreamer `subtitleoverlay` documentation). Three or more simultaneous tracks therefore need custom rendering. The workable path is to disable mpv's subtitle drawing, render each text track with libass at the mpv clock into one transparent BGRA buffer, push that buffer to a second `NativeSurface`, and draw it above the video. That gives control over stacking, styling and per-track delay. Bitmap subtitle tracks (PGS, DVD) need their own decoder and are a separate phase.

Clip export is an ffmpeg job, not an mpv job. mpv's encoding mode always re-encodes; it has no stream-copy mode (`DOCS/man/encode.rst`, lines 1-40; `common/encode_lavc.c`, line 724 selects only encoders with `avcodec_find_encoder`). ffmpeg can copy packets without decoding, which is nearly instant, but the cut lands on the nearest keyframe before the requested point. In the measurement, a 3-second request with `-ss 2 -t 3 -c copy` finished in 56 ms and produced a 5.03-second file. Re-encoding with libx264 veryfast took 757 ms and produced exactly 3.000 seconds. The product should offer both: fast copy for long selections, precise re-encode for short ones, and a later smart-cut pass if exact lossless trimming matters.

The recommended first release is Linux-first, local files only, with libmpv playback, mpv's native two-subtitle support, and ffmpeg fast and precise clip export. The libass overlay for three or more subtitles, GPU interop for 4K60/HDR, and macOS/Windows frame paths are follow-on work. ffmpeg-only and GStreamer are viable alternatives, but each costs more integration work for the same first-release feature set.

## Evidence

- Rune's built-in `Player` fails on non-macOS: `crates/rune/src/video/mod.rs`, lines 26 and 38-41; `crates/rune/src/video/unsupported.rs`, lines 1-30; Rune commit `f18ead891aba2437674cd8c54e238287cceb8b3a`, 2026-11-10.
- Rune's built-in video formats are AVFoundation-limited: `crates/rune/README.md`, lines 1957-1996.
- Rune's portable byte-frame path accepts BGRA and NV12 and is available off macOS: `crates/rune/src/native_surface/mod.rs`, lines 34-43 and 229-259; `examples/video-tiles/README.md`, lines 26-31.
- Rune's native zero-copy paths are macOS IOSurface and Windows D3D shared textures, not Linux: `crates/rune/src/native_surface/mod.rs`, lines 1-8.
- Rune's BGRA shader premultiplies by alpha: `crates/rune/src/native_surface/video.wgsl`, lines 22-23.
- Rune's surface is SDR-only and lacks HDR, wide-gamut and rotation handling: `examples/video-tiles/README.md`, lines 70-78.
- libmpv's software render API writes RGB to caller memory: `include/mpv/render.h`, lines 126-140 and 362-426; mpv commit `bcde6b93cca4d128d84e1a60d1a6db2cd81c889e`, 2026-11-09.
- mpv's render API exposes only OpenGL, D3D11 and software backends: `include/mpv/render.h`, lines 481-485.
- mpv shows two subtitle tracks with `--sid` and `--secondary-sid`, with styling and bitmap caveats: `DOCS/man/options.rst`, lines 2540-2575.
- mpv's encoding mode re-encodes; it has no stream copy: `DOCS/man/encode.rst`, lines 1-40.
- mpv is GPLv2+ by default and LGPLv2.1+ with `-Dgpl=false`: `README.md`, line 192; `meson.options`, line 2.
- ffmpeg's input `-ss` seeks to the closest seek point and preserves the extra segment during stream copy: FFmpeg documentation, "Main options", `-ss position`, https://ffmpeg.org/ffmpeg.html#Main-options (fetched 2026-11-12).
- GStreamer's `subtitleoverlay` has one `subtitle_sink` pad: GStreamer documentation, https://gstreamer.freedesktop.org/documentation/playback/subtitleoverlay.html (fetched 2026-11-12).
- Software-render benchmark: local test, `research/video-player-bench.c`, run 2026-11-12 on AMD Ryzen 7 5700X, 16 threads, mpv 0.41.0, ffmpeg 9.0.2. 1080p60 baseline 2.21-2.72 ms/frame; 1080p60 with one SRT track 3.49 ms/frame; 4K30 5.98-6.39 ms/frame. `hwdec=no` and `hwdec=auto-safe` were within noise of each other because the software render still downloads frames.
- Clip benchmark: local test, same machine. `ffmpeg -ss 2 -t 3 -i 1080p60.mp4 -map 0 -c copy` produced a 5.034 s file in 56 ms. `ffmpeg -ss 2 -t 3 -i 1080p60.mp4 -map 0:v:0 -map 0:a:0 -c:v libx264 -preset veryfast -crf 20 -c:a aac` produced a 3.000 s file in 757 ms.

## Conflicts

- mpv's render header describes the software renderer as "extremely simple (but slow)" and warns that OSD or subtitles at large sizes can make it too slow for realtime (`include/mpv/render.h`, lines 126-140). The measurement finds 2.2-3.5 ms per 1080p frame and 6.0-6.4 ms per 4K frame on this machine without large burned-in OSD. The two are compatible: the warning is a worst-case statement, and the measured scope is a local file with a small subtitle overlay. The product still needs its own benchmark with the real subtitle stack before promising 4K60.
- Rune's README says video playback is macOS-only, while `NativeSurface` accepts portable byte frames. This is not a contradiction: the first statement is about the `Player` API, the second about the lower-level surface API. The app uses the surface API.
- GStreamer's overlay-composition design supports multiple rectangles internally, but the `subtitleoverlay` element exposes one subtitle sink. The design document describes the composition data structure, not out-of-the-box N-track selection. The practical conclusion stands: N simultaneous tracks need custom work.

## Not found

- No official Rune Linux or Windows video backend, and no Rune API for an external `wgpu::Texture`. The only off-macOS route is byte upload.
- No measured `NativeSurface::push_bytes` upload cost. The numbers above are estimates from frame size and PCIe bandwidth, not a Rune benchmark.
- No libmpv or GStreamer support for more than two simultaneous subtitle tracks through a standard property or element. Checked mpv's options manual and render API, and GStreamer's `subtitleoverlay` documentation.
- No first-party libmpv Rust binding from the mpv project. Crates such as `rsmpv`, `libmpv` and `libmpv-sys` exist; their maintenance and API coverage need a separate check before choosing one.
- No macOS libmpv zero-copy path into Rune. The software renderer works there too, but it gives up Rune's IOSurface zero-copy advantage.

## References

- Rune repository: `/home/ulezot/Projects/other/rune`, commit `f18ead891aba2437674cd8c54e238287cceb8b3a`, 2026-11-10. The video API, `NativeSurface` and shader live here.
- mpv repository: `/home/ulezot/Projects/other/mpv`, commit `bcde6b93cca4d128d84e1a60d1a6db2cd81c889e`, 2026-11-09. The render API, subtitle options and encoding mode live here.
- FFmpeg documentation: https://ffmpeg.org/ffmpeg.html. The `-ss`, `-t` and stream-copy behaviour are documented under "Main options" and "Streamcopy".
- GStreamer `subtitleoverlay` documentation: https://gstreamer.freedesktop.org/documentation/playback/subtitleoverlay.html. Used for the alternative comparison.
- `mpv-wgpu-player` crate: https://crates.io/crates/mpv-wgpu-player. Prior art for libmpv software rendering into a caller-owned `wgpu` texture; useful as an integration reference, not as a dependency.
- `rsmpv` crate: https://crates.io/crates/rsmpv. Rust bindings over the mpv client headers; one candidate for the FFI layer.
- `libmpv-sys` crate: https://crates.io/crates/libmpv-sys. Generated bindings; another candidate for the FFI layer.
- Benchmark source: `research/video-player-bench.c`. Local, not part of the product.
