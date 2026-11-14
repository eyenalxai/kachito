# Exact, coalesced seek previews

Status: accepted

Scrubbing previews exact frames, not keyframes. A keyframe seek is fast but
lands on the previous keyframe, up to one GOP away, while the seek bar shows
the requested time; the exact seek on release then lands on a different frame.
Exact seeks show the frame at the requested time, so the drag preview uses
them. They cost decode time, and a 40 ms throttle would interrupt each seek
before it landed, so the session keeps one preview seek in flight and always
sends the latest pointer target. The handle stays at the requested time and
dims while the picture catches up. If no landing seek arrives within five
seconds, the handle returns to the engine position and one notice reports the
timeout. On release, one exact seek lands the final
frame; if the preview already landed within a frame, the exact seek is
skipped. The export range body drag uses the same path.

## Considered options

- Keyframe previews with the handle snapped to the keyframe: fast, but it
  cannot show frames between keyframes and stays jumpy.
- Hybrid with keyframes during motion and exact on settle: motion stays
  quantized, and the picture changes again when the exact frame lands.
- Thumbnail strip: smooth and truthful, but it adds an ffmpeg worker and
  cache.

## Consequences

On the attached 2 s GOP file, previews update at about 5 to 12 per second with
`hwdec=auto-safe` (82 to 192 ms per exact seek) and 12 to 25 per second with
software decoding (40 to 82 ms). On a 10 s GOP the rate falls to about 1 to
2.5 per second (387 to 1056 ms per seek), so a thumbnail or proxy path is the
follow-up if that latency is unacceptable.
