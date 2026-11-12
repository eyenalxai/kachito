# 02: Choose audio and subtitle tracks

**What to build:** The player lists the audio and subtitle tracks in the open file. The viewer can switch the audio track, and can turn the single subtitle track on, off, or switch between available subtitle tracks. Only one subtitle track is shown at a time.

**Surface owned:** track menus and the engine track API.

**Blocked by:** 01.

**Status:** ready-for-agent

- [ ] The audio menu lists the available tracks with language and title when present.
- [ ] Switching the audio track changes the sound without stopping playback.
- [ ] The subtitle menu lists the available tracks and an off option.
- [ ] Selecting a subtitle track shows it; selecting off hides it.
- [ ] Only one subtitle track is active at a time.
- [ ] The menus show an empty state when the file has no matching tracks.
