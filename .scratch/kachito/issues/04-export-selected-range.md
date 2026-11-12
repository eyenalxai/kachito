# 04: Export the selected range

**What to build:** An export button writes the selected range to a Matroska file with the video and the selected audio track, and no subtitle tracks. The viewer can choose the fast keyframe-aligned copy or the precise re-encode. The export shows progress and can be cancelled.

**Surface owned:** export module and export UI.

**Blocked by:** 02, 03.

**Status:** ready-for-agent

- [ ] Export writes a playable Matroska file for the selected range.
- [ ] The output keeps the video and the selected audio track and contains no subtitle streams.
- [ ] The output duration matches the selection in precise mode.
- [ ] Fast copy is available and the UI shows when the cut is keyframe-aligned.
- [ ] Progress is visible while the export runs.
- [ ] Cancelling stops the ffmpeg child process and leaves no partial file.
- [ ] An export failure shows the ffmpeg error in a readable form.
- [ ] The export command builder is covered by unit tests for both modes and with and without a selected audio track.
