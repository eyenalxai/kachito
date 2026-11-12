# Player shell UI: scaling, overlays, menus and tokens

Date: 2026-11-12
Status: answered

## Answer

At Rune `f18ead891aba2437674cd8c54e238287cceb8b3a`, the player shell should:

1. Let the video stage take the window's remaining height with
   `el().grow(1.0).min_h(0.0)`, then give the surface the largest box that
   fits with `el().aspect_fit(width / height)`. A fixed `1280x720` surface
   node is what clips the lower chrome today; a fitted node leaves the
   siblings their space. `NativeSurface` already centres the image in its node
   and `SurfaceFit::Fit` letterboxes it, so the stage can also be a full-size
   node with `native_surface(..., SurfaceFit::Fit)`. The fitted wrapper is the
   better seam for controls that should sit on the picture rather than on the
   letterbox.
2. Float the transport and menus over the stage with `El::pin` or
   `El::absolute`, not with fixed `x`/`y` values. A full-edge scrim with
   `El::modal` gives an in-app menu focus trapping, background inertness,
   Escape dismissal and click-outside dismissal.
3. Draw its own menus on Linux. Rune's native `Tx::popup_menu` is available on
   macOS and Windows only; elsewhere the callback receives `None` at once.
4. Use one static `rune::Icon` set. An icon is an SVG body on a square
   viewBox, tinted with `.color()`; there is no built-in icon catalog to
   import from the framework.
5. Define app colours and type in the app. `rune::tok::theme` is a semantic
   dark/light palette, and `TextStyle` carries size, weight, line height,
   tracking and OpenType features. Timer digits should use `"'tnum' 1"`.
6. Fullscreen is `Tx::toggle_full_screen`; the initial window bounds come from
   `AppSpec` and its `min_size`.

The video frame is fixed at 1280x720 in this revision, so the fitted aspect is
16:9 until the engine reads the media's own dimensions.

## Evidence

- The fitted box helper: `crates/rune/src/el/layout.rs`, `El::aspect_fit`,
  lines 62-67, commit `f18ead8`. Its doc says it takes the largest box at the
  ratio that fits the room the element is given and that the parent's
  alignment places it.
- The grow contract: `crates/rune/src/el/layout.rs`, `El::grow`, lines
  113-123, commit `f18ead8`. It warns that a grown scroll viewport inside an
  unbounded parent collapses, and that a grown column needs `min_h(0.0)`.
- Surface placement: `crates/rune/src/el.rs`, `El::native_surface`,
  lines 963-966, and `El::surface_anchor`, lines 975-983, commit `f18ead8`.
  `crates/rune/src/native_surface/place.rs`, `image_rect`, lines 34-55, shows
  the default anchor is centre on both axes and `SurfaceFit::Fit` picks the
  smaller scale.
- Out-of-flow placement: `crates/rune/src/el/layout.rs`, `El::absolute`,
  lines 562-565, and `El::pin`, lines 575-583, commit `f18ead8`.
- Modal menu behaviour: `crates/rune/src/el.rs`, `El::modal`, lines 1175-1180,
  commit `f18ead8`: traps focus, makes the background inert, and Escape
  requests dismissal. `El::modal_click_outside` follows at lines 1187-1190.
- Linux has no native pop-up menus: `crates/rune/src/popup_menu/mod.rs`, the
  module comment lines 10-11 and `popup_menu_supported` at line 51, commit
  `f18ead8`. The host stub for non-macOS/non-Windows returns `None`.
- Icons: `crates/rune/src/el.rs`, `icon`, lines 652-658, and
  `crates/rune/src/text/icon.rs`, `Icon`, lines 12-17, commit `f18ead8`.
  `Icon.body` is a subset of SVG; `El::color` tints it.
- Theme and type: `crates/rune/src/tok.rs`, `Theme`, `theme(dark)` and
  `TextStyle`, lines 83-190, commit `f18ead8`. `TextStyle::features` accepts
  CSS OpenType settings, including `"'tnum' 1"`.
- Fullscreen: `crates/rune/src/el/tx.rs`, `Tx::toggle_full_screen`, lines
  284-288, commit `f18ead8`.
- Window bounds: `crates/rune/src/tooling.rs`, `AppSpec::min_size`, line 21,
  commit `f18ead8`.
- The app pins Rune to `f18ead891aba2437674cd8c54e238287cceb8b3a` in
  `Cargo.toml` and renders a fixed 1280x720 frame in `src/engine.rs` lines
  15-16 and 700-708.

## Visual reference

The ReUI MCP is not exposed in this session: `list_mcp_resources` returned no
resources or templates, and no `reui` tool is in the catalog. The local cache
at `/home/ulezot/.local/share/reui/templates` is available and was read for
the visual language only. The templates are React and shadcn/ui, so they are
not a code source for a Rune view.

- `atlas-admin/app/globals.css`: dark background `oklch(0.145 0 0)`, card and
  popover `oklch(0.205 0 0)`, border `oklch(1 0 0 / 10%)`, radius `0.625rem`
  (10 pt), Inter.
- `signal-analytics/src/styles.css`: dark background
  `oklch(0.148 0.004 228.8)`, popover `oklch(0.218 0.008 223.9)`, border
  `oklch(1 0 0 / 10%)`, Inter.

The useful common ground is the near-black canvas, one raised surface step,
a 1 px light-alpha border in dark mode, a 10 pt radius family, and one accent
colour. The templates are dashboards, so their density and navigation do not
transfer; the surface and type treatment does.

Two shell patterns recur across the templates:

- A persistent app header, 48 px tall, with a bottom hairline, a truncating
  title or breadcrumb on the left and page actions on the right. See
  `strata-observability/components/app-shell/app-header.tsx`, the `<header>`
  around line 100. The header's controls use 32 px buttons and a `gap-2` row;
  the title is the only part that shrinks.
- A floating control island over a canvas, made from a small `Card` with
  `role="group"`, icon controls and a dropdown menu. See
  `signal-analytics/src/features/north-star/components/canvas-toolbar.tsx`
  and `flowly-app/features/workflow-editor/components/canvas-toolbar.tsx`.
  The bar sits over the content and keeps its own compact padding and 1 px
  border.

The templates have no media-chrome example, so auto-hiding top and bottom
chrome is a video-player convention rather than a template pattern.

## System font

Rune does not resolve the CSS `system-ui` generic to a system family. The
`sans` slot's family list is filtered to named families before the font query,
so a generic alone leaves no family to load (`crates/rune/src/text.rs`,
`primary_font`, around lines 400-425, commit `f18ead8`). Installed families
are loaded by name through `load_installed` in
`crates/rune/src/text/system_fonts.rs` around lines 170-185.

A native system font is reachable by naming the desktop's UI family at launch
and passing it through `AppSpec::families` (`crates/rune/src/tooling.rs`,
lines 28-30, commit `f18ead8`). On this machine:

- `gsettings get org.gnome.desktop.interface font-name` returns
  `'Adwaita Sans 11'`; stripping the trailing size gives `Adwaita Sans`, and
  `fc-match 'Adwaita Sans'` resolves to
  `/usr/share/fonts/Adwaita/AdwaitaSans-Regular.ttf`.
- `fc-match --format='%{family[0]}' sans-serif` returns `Liberation Sans` as a
  fallback when the desktop's own setting is unavailable.
- Keeping `Inter` at the end of the family list gives Rune a font when no
  system family resolves.

`ui-monospace` is the one generic Rune treats specially: the mono slot expands
it to the platform's installed monospace families
(`crates/rune/src/text/system_fonts.rs`, `names_system_mono` and
`family_list`, lines 75-94, commit `f18ead8`).

## Conflicts

None between primary sources. The two fitting mechanisms overlap: `aspect_fit`
sizes the node while `SurfaceFit::Fit` sizes the image inside the node. Doing
both with the same ratio is harmless, but only one is needed. This note
recommends `aspect_fit` on the surface node because it gives the controls a
picture-shaped parent.

## Not found

- The media's native width and height are not exposed by `PlayerEngine` or
  `Core` at this revision. `src/engine.rs` renders every file into a fixed
  1280x720 buffer, so the shell can only assume 16:9. Reading
  `width`/`height` (or `video-params/aspect`) from libmpv is a follow-up, not a
  UI-only change.
- Rune exposes no built-in icon catalog; an app must declare its own `Icon`
  statics.
