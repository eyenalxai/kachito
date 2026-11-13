//! Finds libmpv with pkg-config so the link path follows a non-default prefix.

fn main() {
    println!("cargo:rerun-if-env-changed=PKG_CONFIG_PATH");
    println!("cargo:rerun-if-env-changed=PKG_CONFIG_LIBDIR");

    // `probe` emits the link search path and `-lmpv`. The mpv header is not
    // needed: the bindings in `src/engine.rs` are hand-written, so no C
    // compiler takes part in the build.
    pkg_config::Config::new()
        .probe("mpv")
        .unwrap_or_else(|error| {
            panic!(
                "libmpv was not found through pkg-config: {error}\n\
                 Install the libmpv development package so `pkg-config --modversion mpv` works, \
                 or set PKG_CONFIG_PATH to the prefix that holds mpv.pc."
            )
        });
}
