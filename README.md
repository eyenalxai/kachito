# Kachito

Kachito is a Linux desktop video player built with Rune. It plays local MKV and MP4 files with libmpv, shows the video through a Rune `NativeSurface`, and will add a timeline selection and clip export.

The project is pre-alpha. The first slice is playback with sound.

## Requirements

- Linux with a working Vulkan driver
- libmpv (`pkg-config --modversion mpv`)
- ffmpeg on `PATH`
- Rust 1.99.1

## Build

The Rune dependency is pinned to a `dev` revision. Rune is pre-release, so upgrades are deliberate.

```sh
cargo build
```

## Run

```sh
cargo run -- /path/to/video.mkv
```

## Documents

- Spec: `.scratch/kachito/spec.md`
- Mission record: `.scratch/kachito/map.md`
- Research: `docs/research/video-player-feasibility.md`

## License

MIT for the application source. A distributed build that links GPL libmpv or GPL ffmpeg is GPL. The development build uses the system libraries, so the source stays MIT.
