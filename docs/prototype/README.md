# Float

Throwaway prototype. Float is the design: the chrome is a title capsule, a standalone Open button
and a transport capsule over an image-only window. Layout only; materials are out of scope here
and will be built in Rune.

The capsules:

- **Title capsule**: top-left, 360 pt maximum width, 40 pt high, 12 pt corners, 16 pt from the
  top and 24 pt from the left. Film icon, filename at 13 pt. The long filename tests truncation.
- **Open button**: top-right, 40 pt high, 12 pt corners, 16 pt from the top and 24 pt from the
  right. It is a standalone soft-filled text button. There is no action capsule around it; a
  container holding one control adds a layer with no meaning.
- **Transport capsule**: bottom-center, 720 pt maximum width, 86 pt high, 18 pt corners, 16 pt
  from the bottom. The timeline row sits above the control row: 11 pt timecodes at the ends, a
  3 pt seek line, then a 44 pt play, 32 pt skip and utility buttons, a 72 pt volume slider, and
  Export at the far right of the row, separated from the utilities by a 1 pt divider. Export is
  a quiet 32 pt text button; it belongs with the controls, not beside Open.

The right side of the transport row is volume, audio, subtitles, speed and fullscreen. Volume is
a speaker. Audio is a waveform. Subtitles is the caption rectangle. Speed is a direct 1× button
with a small speed menu. It has a fixed 44 pt width and tabular figures, so 1× to 1.25× does not
move the row. Fullscreen is a direct icon. There is no three-dot overflow menu.

The timecodes are fixed 44 pt boxes at the capsule edges: current time left-aligned, total time
right-aligned, both on a 16 pt line box. They line up with the play button and Export below.

The bottom capsule is the hero. The title capsule and Open button are secondary and deliberately
smaller. The objects are separate; there is no full-width bar, no side rail and no centered HUD.

Rejected:

- The full-width bottom bar. It turned the chrome into a toolbar.
- The Stage centered HUD. It covered the lower third and used a short seek line.
- The Deck side rail. It separated play from the timeline and put the primary controls where the
  eye does not go.

Export is always precise. There is no Fast/Precise choice and no explanatory text in the panel.
The panel has the range and one Export clip button. The implementation uses the libx264 CRF 20
and AAC 160k path.

Open `immersive-ui.html` in a browser. It is self-contained: the picture and timeline are
simulated, and there are no external assets.

- Hover the top edge for the title and action capsules.
- Hover the bottom edge for the transport capsule.
- Move into the middle to let them go.
- Paused keeps the transport. The full set also shows for three seconds on load so the hover
  affordance is discoverable.
- `G` toggles guides. `Space` pauses. `Escape` closes a menu.

The capsule surface is neutral on purpose: a flat dark fill, a 1 pt boundary and a simple shadow.
No blur, gradients or material effects.

## Implementation

The shell is implemented in the Rune app. The title capsule and Open button reveal at the top
edge, the transport reveals at the bottom edge, and the transport stays while paused. The panels
sit above the transport. Export always uses the precise path.

Materials remain deferred. The Rune findings are in `docs/research/rune-materials.md` for a
separate pass; the shipped surfaces are flat.
