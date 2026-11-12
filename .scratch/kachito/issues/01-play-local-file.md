# 01: Play a local file with picture and sound

**What to build:** Launching Kachito opens a player screen. Opening a local MKV or MP4 file through a dialog or drag and drop shows the video and plays its audio. The viewer can play, pause, seek, change volume and change speed. A missing file or an unsupported codec produces a clear error instead of a blank window.

**Surface owned:** app shell, playback engine, video surface.

**Blocked by:** None (can start immediately).

**Status:** claimed

- [ ] The app launches on Linux and shows the player screen.
- [ ] A local MKV and a local MP4 open through the file dialog and through drag and drop.
- [ ] Video frames reach the Rune surface and are not the grey placeholder.
- [ ] Audio plays and stays in sync with the picture during normal playback and after a seek.
- [ ] Play, pause, seek, volume and speed controls work.
- [ ] A missing file or an unsupported codec shows a readable error.
- [ ] A headless test proves a decoded frame reaches the surface and renders.
