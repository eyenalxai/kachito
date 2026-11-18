# Rune blur and materials on Linux

Question: can the Kachito shell have real blur and a rich material treatment in Rune on Linux?

The HTML prototype now focuses on layout only. This note is the material research for the Rune
implementation.

Yes, with one important distinction. Rune has two kinds of material:

1. `El::backdrop_blur(sigma)` blurs the window's own content. It is cross-platform, including
   Linux and headless renders. This is the one Kachito should use.
2. `El::system_material(SystemMaterial::Glass | Popover)` asks the operating system for a
   native material behind the window. It is macOS-only. On Linux and in headless renders it
   falls back to the element's fill, or a neutral dark tint when there is no fill.

The rest of this note records what the pinned Rune revision can do, the limits that matter for a
player, and a concrete recipe for the Veil shell.

## The cross-platform primitive: `backdrop_blur`

Source: `crates/rune/src/el/backdrop.rs`, `crates/rune/src/render/backdrop_blur.rs`,
`examples/frosted-bar/src/main.rs`.

```rust
el()
    .backdrop_blur(18.0)
    .fill(oklcha(0.08, 0.004, 255.0, 0.42))
```

What it does:

- Blurs what is already drawn behind the element in the same window, inside the element's
  rounded or continuous corners and its clip.
- Draws the element's fill over the blur as a tint, then its border, children and gradients on
  top. `el().backdrop_blur(8.0).fill(tint)` is the frosted-bar recipe.
- Follows scrolling, layout, springs and opacity. The element's opacity fades the blur with it,
  so the chrome's enter/exit transition also fades the material.
- Renders the same in headless snapshots, which makes the visual review cheap.
- Blurs a copy of the window target at half resolution. The pyramid is cut to the element's rect
  plus three sigma, not the whole window, so a 40 pt top capsule and an 86 pt bottom capsule are
  cheap.

Limits:

- At most `MAX_BACKDROP_BLURS` (4) elements blur per frame. Later elements draw only their fill.
  A four-edge shell (top, bottom, left, right) exactly fills the budget. Menus, popovers and the
  export tray should reuse a chrome blur, or use an opaque panel, or temporarily replace one of
  the four.
- Sigma clamps around 78 points at 2x. Use 12-24 for chrome. A larger value stops adding detail.
- The first four blurred elements in paint order win. Do not put a blurred element inside an
  ancestor that is already blurred, cached or grouped with `group_opacity` unless that is the
  intended CSS-like behavior: inside such a layer the backdrop is only what the layer has drawn
  so far.
- A target without `COPY_SRC`, or a caller's own `wgpu::RenderPass` through
  `Renderer::draw_range`, draws the fill without the blur. Kachito's normal `rune::run` path is
  fine.

Video: native surface draws are interleaved into the same color target by `before_instance`
(`crates/rune/src/render.rs`). The chrome is drawn after the video, so the backdrop copy should
include the video frame. This is the one item to verify on the real libmpv surface before
committing to the final material. The fallback is a tinted gradient, which still reads as a
surface but loses the frost.

Performance: the `frosted-bar` example uses the same path and has a 120 fps scripted performance
run. A 720 pt capsule over a 1280x720 surface is a small region. Measure on the target machine,
but this is not a full-screen blur.

## The macOS-only path: `system_material`

Source: `crates/rune/src/system_material/mod.rs`, `examples/glass-card`, `examples/glass-panel`.

- `SystemMaterial::Glass` is Liquid Glass (`NSGlassEffectView`) on macOS 26 and later, the
  popover material before that. `SystemMaterial::Popover` is `NSVisualEffectView` popover
  vibrancy.
- At most `MAX_NATIVE_MATERIALS` (4) views per window.
- On Linux, in opaque windows and in headless renders, the element's fill stands in. With no
  fill, Rune uses a neutral dark tint. There is no blur behind the window.

So the native Liquid Glass API exists, but it is not a Linux feature. Do not make the Linux
design depend on it. It can be a macOS enhancement later, with `backdrop_blur` as the baseline.

## Supporting surface tools

- `El::blur(sigma)` blurs the element and its subtree. This is a layer blur, not a backdrop
  blur. It is useful for defocusing content during a swap or for a soft glow, not for glass.
  `Transition::blur(sigma)` is the content-swap version.
- `El::gradient(top, bottom)` is a two-stop vertical gradient. `El::linear_gradient(angle,
  stops)` and `El::radial_gradient(center, radius, stops)` take up to 16 stops. That is enough
  for eased tints, scrims and a specular sheen. Rune has no separate noise or grain shader; a
  tiny `Pixels` texture could add grain if it is ever wanted.
- `El::border`, `El::border_sides`, `El::outline` draw the hairline. `El::box_shadow` takes
  offset, blur, spread, color and `inset`, so contact, ambient and inner-highlight layers are
  all available. The first four shadows spring.
- `El::continuous_corners()` gives the squircle corners `NSGlassEffectView` uses. Use it on
  floating panels and menus, not on an edge-to-edge bar.
- `El::cache` and `El::group_opacity` exist for compositing, but a bar does not need them.

There is no saturation or color-matrix filter in the element API. The web prototype's
`backdrop-filter: blur(18px) saturate(1.18)` cannot be copied literally. Use the tint and
gradients to set the color cast; the blur itself is exact.

## Where the material would go

The chosen model is Float. The material belongs to the title capsule, the standalone Open button
and the transport capsule. They are real surfaces, so `backdrop_blur` fits them directly. The
timeline line inside the transport capsule stays crisp and unblurred.

Capsule measurements:

- Title capsule: 360 pt maximum width, 40 pt high, 12 pt corners.
- Open button: 40 pt high, 12 pt corners, standalone. There is no wrapper.
- Transport capsule: 720 pt maximum width, 86 pt high, 18 pt corners.

The material layers, in paint order:

1. `backdrop_blur(18.0)` for a light frost, `24.0` for a darker one.
2. Tint fill at roughly `oklcha(0.08, 0.004, 255.0, 0.42)` or darker.
3. A vertical linear gradient for the light falloff, using `linear_gradient(PI, stops)`.
4. A radial sheen near the top-left, or a low-alpha amber glow near the bottom center.
5. A 1 pt border at white 0.12-0.15, plus an inset top highlight at white 0.06-0.08.
6. A contact shadow and an ambient shadow. `continuous_corners` suits the capsule shape.
7. The transport capsule keeps the seek line and handle above the blur. The line is the spine.

The title capsule and Open button can use the same material at a smaller scale. They are
secondary, so a lighter tint and a single shadow are enough. Do not blur the timeline line
itself.

## Recommendation

Use Float with Frost on the title capsule, the Open button and the transport capsule. The
transport capsule is the main material object; the title capsule and Open button are quieter.
Build them from `backdrop_blur` plus tint, gradients, a border, an inset highlight and shadows.
Leave `system_material` out of the Linux path. Verify the blur against the real libmpv surface
before the full implementation, because that is the only unproven link.
