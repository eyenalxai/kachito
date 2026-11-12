# Kachito spec

Date: 2026-11-12
Status: approved

## Problem Statement

Local MKV and MP4 files cannot be watched in a Rune desktop app on Linux because Rune's built-in player is macOS-only and reads only a narrow set of formats. The app also needs to mark a range on a timeline and export that range as a shareable video clip without subtitle tracks.

## Solution

Kachito is a Linux desktop app built with Rune. libmpv handles playback: containers, codecs, audio tracks, audio output, seeking, speed and one subtitle track. Video frames come from libmpv's software renderer into a Rune `NativeSurface`. A timeline shows in and out handles with zoom and scrubbing. An export button runs ffmpeg to write the selected range as a Matroska file with the video and its audio, and no subtitle tracks.

## User Stories

1. As a viewer, I want to open a local MKV or MP4 file so that I can watch downloaded video.
2. As a viewer, I want play, pause, seek, volume and speed controls so that I can control playback.
3. As a viewer, I want to switch audio tracks so that I can choose the language or commentary.
4. As a viewer, I want to turn one subtitle track on or off so that I can follow dialogue when needed.
5. As a clipper, I want a timeline with in and out handles, zoom and scrubbing so that I can select a precise range.
6. As a clipper, I want an export button that writes the selected range as a video file with audio and no subtitles so that I can share the snippet.
7. As a viewer, I want a clear error when a file or codec cannot be opened so that the app does not appear broken.

## Implementation Decisions

Kachito is a new standalone Rust application using Rune. The UI is Rune views and state, not a web view.

Playback uses libmpv through a small FFI layer. The engine opens a local path, exposes the track list, and reports position, duration and state. The render path uses `MPV_RENDER_API_TYPE_SW` at the video element's pixel size, writes `bgr0`, sets the fourth byte to 255, and pushes the frame to `NativeSurface`. Audio goes through mpv's audio output. Hardware decode is requested with `hwdec=auto-safe` and falls back to software.

The UI has one screen. It contains the video surface, a transport bar, an audio track menu, a subtitle track menu with an off option, and the timeline. The timeline shows the full duration, the current position, in and out handles, and a pixels-per-second zoom. Dragging the playhead seeks. Dragging a handle moves the selection. The selection is clamped to the file and cannot be shorter than one frame.

Export runs the system ffmpeg as a child process. The selected range is written as Matroska. The video and the selected audio track are kept; subtitle tracks are dropped. Two modes exist: a fast keyframe-aligned stream copy and a precise re-encode. Precise is the default for selections under 60 seconds; fast copy is the default for longer selections. The export shows progress from ffmpeg's progress pipe and can be cancelled.

One subtitle track is supported at a time. mpv renders it into the video frame. There is no libass overlay and no stacking. Bitmap subtitle tracks work if mpv can decode them; they are not exported.

Opening a file uses a native file dialog and drag and drop. The app remembers the last directory for the session. Remote URLs are rejected with a clear message in this release.

The first release targets Linux, SDR, 1080p60 and 4K30. 4K60 and HDR are not promised.

The application source is MIT where the linked components allow it. A distributed build that links GPL libmpv or GPL ffmpeg is GPL. The development build uses the system libraries, so the source can stay MIT. Before a bundled release, use LGPL builds of libmpv and ffmpeg to keep MIT, or accept GPL.

## Testing Decisions

The highest seam is a `PlayerEngine` interface between the UI and the playback backend. It covers open, play, pause, seek, volume, speed, track listing, audio selection, subtitle selection, position, duration and state. The UI talks to this interface. A fake engine drives UI tests. A real libmpv engine drives one integration test that opens a fixture, decodes a frame, pushes it to `NativeSurface`, renders a headless snapshot, and checks that the frame is not the placeholder.

The timeline is a separate model with tests for clamping, selection duration, zoom limits and handle ordering. The export command builder is pure and tested for the exact argv in fast and precise modes, with and without a selected audio track. An export integration test runs ffmpeg on a short fixture and probes the output for duration and streams: video and audio present, subtitles absent.

The Rune UI gets a headless test that finds the timeline handles, drags them, and checks the resulting selection. Prior art is in Rune's `video-player` example and its headless runtime tests.

## Out of Scope

Three or more simultaneous subtitles, the libass overlay, per-track styling and offsets. Subtitle tracks in exported clips. Remote streaming from the Rails service. macOS and Windows. 4K60, HDR, 10-bit and wide-gamut output. Playlists, a media library, thumbnails, waveform display and audio-only mode. A bundled AppImage or Flatpak in the first slice. Multiple audio tracks playing at once. Smart-cut lossless trimming.

## Further Notes

Rune is pre-release; pin a `dev` revision and treat upgrades as deliberate. The research note holds the source lines and the frame-time measurements. The first implementation slice should prove playback with a real MKV or MP4 before the timeline and export are built, because the frame path is the only technical risk left.

The repository lives at `/home/ulezot/Projects/first-party/kachito`. The application name is Kachito.
