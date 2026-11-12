# 03: Select a range on the timeline

**What to build:** A timeline under the video shows the full duration, the current playhead, and in and out handles. The viewer can drag the playhead to seek, drag either handle to change the selection, and zoom the timeline in and out. The selection stays inside the file and cannot be shorter than one frame.

**Surface owned:** timeline model and timeline UI.

**Blocked by:** 01.

**Status:** ready-for-agent

- [ ] The timeline shows the full duration and follows the playhead during playback.
- [ ] Dragging the playhead seeks the video.
- [ ] In and out handles can be dragged and the selected range is visible.
- [ ] Zooming changes the visible time span without losing the selection.
- [ ] The selection clamps to the file and enforces a one-frame minimum.
- [ ] The timeline model is covered by unit tests for clamping, zoom and handle ordering.
