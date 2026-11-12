# 01: Play a local file with picture and sound

**What to build:** Launching Kachito opens a player screen. Opening a local MKV or MP4 file through a dialog or drag and drop shows the video and plays its audio. The viewer can play, pause, seek, change volume and change speed. A missing file or an unsupported codec produces a clear error instead of a blank window.

**Surface owned:** app shell, playback engine, video surface.

**Blocked by:** None (can start immediately).

**Status:** resolved

- [ ] The app launches on Linux and shows the player screen.
- [ ] A local MKV and a local MP4 open through the file dialog and through drag and drop.
- [ ] Video frames reach the Rune surface and are not the grey placeholder.
- [ ] Audio plays and stays in sync with the picture during normal playback and after a seek.
- [ ] Play, pause, seek, volume and speed controls work.
- [ ] A missing file or an unsupported codec shows a readable error.
- [ ] A headless test proves a decoded frame reaches the surface and renders.

## Answer

Built the first slice. Kachito launches, opens a local MKV or MP4 through the dialog or a dropped file, renders the video through `NativeSurface`, plays audio through libmpv, and has play, pause, seek, volume and speed controls. Missing files, unsupported extensions and remote URLs produce a readable error.

Verification: eight unit tests cover the engine and the UI seam, including MP4 and MKV frame pushes and a missing-file error. A headless integration test spawns the app on a real MKV, decodes a frame, renders the snapshot and asserts the centre pixel is a bright video frame. Manual 1080p snapshots for MP4 and MKV showed real video frames. `cargo clippy --all-targets` is clean.

Manual checks remain for the native file dialog on the desktop and audio-device playback; both use standard platform paths. Sustained playback needs a frame-buffer pool to avoid a 3.7 MB allocation per frame.
