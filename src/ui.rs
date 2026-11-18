//! Design tokens, icons, font detection and the shared controls of the player shell.

use rune::{
    Color, El, Icon, Spring, SpringPair, TextStyle, Transition, el, icon, oklch, oklcha, row, text,
};
use std::process::Command;
use std::sync::OnceLock;

pub const TOP_REVEAL: f32 = 84.0;
pub const BOTTOM_REVEAL: f32 = 168.0;
pub const TOP_CAPSULE_H: f32 = 40.0;
pub const TRANSPORT_H: f32 = 86.0;
pub const TRANSPORT_INSET: f32 = 16.0;
pub const TRANSPORT_MAX_W: f32 = 720.0;
pub const EXPORT_MAX_W: f32 = 720.0;
pub const RADIUS_LG: f32 = 12.0;
pub const RADIUS_TOP: f32 = 12.0;
pub const RADIUS_TRANSPORT: f32 = 18.0;
pub const RADIUS_EXPORT: f32 = 16.0;
pub const SURFACE_BORDER: f32 = 1.0;
pub const SURFACE_SHADOW_Y: f32 = 18.0;
pub const SURFACE_SHADOW_BLUR: f32 = 50.0;
pub const TRAY_MOTION: SpringPair = SpringPair::new(Spring::new(0.20, 0.0), Spring::new(0.15, 0.0));
pub const CHROME_MOTION: SpringPair =
    SpringPair::new(Spring::new(0.28, 0.0), Spring::new(0.20, 0.0));

pub const TITLE: TextStyle = TextStyle::new(22.0, 600.0).tracking(-0.2);
pub const BODY: TextStyle = TextStyle::new(13.0, 400.0);
pub const BODY_STRONG: TextStyle = TextStyle::new(13.0, 600.0);
pub const LABEL: TextStyle = TextStyle::new(12.0, 500.0);
pub const META: TextStyle = TextStyle::new(11.0, 500.0);
pub const TINY: TextStyle = TextStyle::new(11.0, 400.0);
pub const TIME: TextStyle = TextStyle::new(12.0, 500.0).features("'tnum' 1");
pub const TIME_SMALL: TextStyle = TextStyle::new(11.0, 500.0).features("'tnum' 1");
pub const TOAST_TITLE: TextStyle = TextStyle {
    line_height: 20.0,
    ..BODY_STRONG
};
pub const TOAST_DETAIL: TextStyle = TextStyle {
    line_height: 16.0,
    ..TINY
};

#[derive(Clone, Copy, Debug)]
pub struct Palette {
    pub bg: Color,
    pub stage: Color,
    pub surface: Color,
    pub surface_raised: Color,
    pub border: Color,
    pub border_strong: Color,
    pub text: Color,
    pub text_muted: Color,
    pub text_faint: Color,
    pub accent: Color,
    pub accent_hover: Color,
    pub accent_ink: Color,
    pub accent_soft: Color,
    pub soft: Color,
    pub soft_hover: Color,
    pub soft_press: Color,
    pub play: Color,
    pub play_hover: Color,
    pub play_press: Color,
    pub danger: Color,
    pub danger_surface: Color,
    pub hover: Color,
    pub press: Color,
    pub track: Color,
    pub scrim: Color,
    pub focus: Color,
    pub shadow: Color,
}

pub fn palette() -> Palette {
    Palette {
        bg: oklch(0.055, 0.002, 255.0),
        stage: oklch(0.0, 0.0, 0.0),
        surface: oklcha(0.09, 0.005, 255.0, 0.94),
        surface_raised: oklcha(0.13, 0.006, 255.0, 0.97),
        border: oklcha(1.0, 0.0, 0.0, 0.12),
        border_strong: oklcha(1.0, 0.0, 0.0, 0.16),
        text: oklch(0.97, 0.002, 255.0),
        text_muted: oklch(0.73, 0.008, 255.0),
        text_faint: oklch(0.55, 0.008, 255.0),
        accent: oklch(0.79, 0.135, 68.0),
        accent_hover: oklch(0.85, 0.13, 68.0),
        accent_ink: oklch(0.17, 0.02, 68.0),
        accent_soft: oklcha(0.79, 0.135, 68.0, 0.16),
        soft: oklcha(1.0, 0.0, 0.0, 0.08),
        soft_hover: oklcha(1.0, 0.0, 0.0, 0.14),
        soft_press: oklcha(1.0, 0.0, 0.0, 0.16),
        play: oklcha(1.0, 0.0, 0.0, 0.12),
        play_hover: oklcha(1.0, 0.0, 0.0, 0.18),
        play_press: oklcha(1.0, 0.0, 0.0, 0.16),
        danger: oklch(0.70, 0.19, 25.0),
        danger_surface: oklcha(0.50, 0.17, 25.0, 0.24),
        hover: oklcha(1.0, 0.0, 0.0, 0.07),
        press: oklcha(1.0, 0.0, 0.0, 0.12),
        track: oklcha(1.0, 0.0, 0.0, 0.18),
        scrim: oklcha(0.0, 0.0, 0.0, 0.28),
        focus: oklcha(0.95, 0.0, 0.0, 0.80),
        shadow: oklcha(0.0, 0.0, 0.0, 0.45),
    }
}

pub mod icons {
    use rune::Icon;

    macro_rules! stroke_icon {
        ($name:ident, $label:literal, $body:literal) => {
            pub static $name: Icon = Icon {
                name: $label,
                view: 24.0,
                body: concat!(
                    r#"<g fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round">"#,
                    $body,
                    "</g>"
                ),
            };
        };
    }

    stroke_icon!(
        OPEN,
        "open",
        r#"<path d="M3 7a2 2 0 0 1 2-2h5l2 2h7a2 2 0 0 1 2 2v9a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z"/>"#
    );
    stroke_icon!(
        AUDIO,
        "audio",
        r#"<path d="M4 10v4M8 7v10M12 5v14M16 8v8M20 11v2"/>"#
    );
    stroke_icon!(
        SUBTITLES,
        "subtitles",
        r#"<rect x="3" y="5" width="18" height="14" rx="2"/><path d="M7 12h3M14 12h3M7 15h2M12 15h5"/>"#
    );
    stroke_icon!(
        EXPORT,
        "export",
        r#"<path d="M12 3v10"/><path d="m8 9 4 4 4-4"/><path d="M5 15v4h14v-4"/>"#
    );
    stroke_icon!(CLOSE, "close", r#"<path d="M6 6l12 12M18 6 6 18"/>"#);
    stroke_icon!(CHECK, "check", r#"<path d="m5 12 4 4L19 6"/>"#);
    stroke_icon!(
        FULLSCREEN,
        "fullscreen",
        r#"<path d="M8 3H3v5"/><path d="M16 3h5v5"/><path d="M21 16v5h-5"/><path d="M3 16v5h5"/>"#
    );
    stroke_icon!(CHEVRON, "chevron", r#"<path d="m6 9 6 6 6-6"/>"#);
    stroke_icon!(
        FILM,
        "film",
        r#"<rect x="3" y="4" width="18" height="16" rx="2"/><path d="M7 4v16M17 4v16M3 8h4M3 16h4M17 8h4M17 16h4"/>"#
    );
    stroke_icon!(
        ALERT,
        "alert",
        r#"<path d="M12 3 2.5 20h19z"/><path d="M12 9v5"/><path d="M12 17h.01"/>"#
    );
    stroke_icon!(PLUS, "plus", r#"<path d="M12 5v14M5 12h14"/>"#);
    stroke_icon!(MINUS, "minus", r#"<path d="M5 12h14"/>"#);
    stroke_icon!(
        VOLUME,
        "volume",
        r#"<path d="M11 5 6 9H3v6h3l5 4z"/><path d="M15.5 8.5a5 5 0 0 1 0 7"/>"#
    );
    stroke_icon!(
        MUTE,
        "mute",
        r#"<path d="M11 5 6 9H3v6h3l5 4z"/><path d="m16 9 5 6M21 9l-5 6"/>"#
    );
    stroke_icon!(
        BACK_10,
        "back-10",
        r#"<path d="M4 7v5h5"/><path d="M5.5 12a7 7 0 1 1 2 5"/><path d="M10.5 8.5l1.5-1v6"/><path d="M17 8.5a2.5 2.5 0 1 0 0 5 2.5 2.5 0 0 0 0-5z"/>"#
    );
    stroke_icon!(
        FORWARD_10,
        "forward-10",
        r#"<path d="M20 7v5h-5"/><path d="M18.5 12a7 7 0 1 0-2 5"/><path d="M10.5 8.5l1.5-1v6"/><path d="M17 8.5a2.5 2.5 0 1 0 0 5 2.5 2.5 0 0 0 0-5z"/>"#
    );

    pub static PLAY: Icon = Icon {
        name: "play",
        view: 24.0,
        body: r#"<g fill="currentColor" stroke="none"><path d="M8 5.5v13l10.5-6.5z"/></g>"#,
    };
    pub static PAUSE: Icon = Icon {
        name: "pause",
        view: 24.0,
        body: r#"<g fill="currentColor" stroke="none"><rect x="7" y="5" width="3.5" height="14" rx="1"/><rect x="13.5" y="5" width="3.5" height="14" rx="1"/></g>"#,
    };
}

fn sized_icon_button(
    glyph: &'static Icon,
    label: &str,
    theme: &Palette,
    size: f32,
    radius: f32,
    glyph_size: f32,
) -> El {
    el().size(size, size)
        .radius(radius)
        .center()
        .hover(theme.hover)
        .press(theme.press)
        .press_scale(0.96)
        .focusable(true)
        .focus_ring(2.0, 2.0, theme.focus)
        .role("button")
        .label(label)
        .child(
            icon(glyph, glyph_size)
                .color(theme.text_muted)
                .a11y_hidden(),
        )
}

pub fn icon_button(glyph: &'static Icon, label: &str, theme: &Palette) -> El {
    sized_icon_button(glyph, label, theme, 32.0, 8.0, 17.0)
}

pub fn small_icon_button(glyph: &'static Icon, label: &str, theme: &Palette) -> El {
    sized_icon_button(glyph, label, theme, 28.0, 6.0, 17.0)
}

pub fn large_icon_button(glyph: &'static Icon, label: &str, theme: &Palette) -> El {
    sized_icon_button(glyph, label, theme, 40.0, 8.0, 18.0)
}

pub fn soft_button(glyph: Option<&'static Icon>, label: impl Into<String>, theme: &Palette) -> El {
    let label = label.into();
    let mut button = row()
        .h(40.0)
        .px(14.0)
        .gap(6.0)
        .center_y()
        .radius(RADIUS_TOP)
        .fill(theme.soft)
        .border(1.0, theme.border)
        .hover(theme.soft_hover)
        .press(theme.soft_press)
        .press_scale(0.96)
        .focusable(true)
        .focus_ring(2.0, 2.0, theme.focus)
        .role("button")
        .label(label.clone());
    if let Some(glyph) = glyph {
        button = button.child(icon(glyph, 16.0).color(theme.text).a11y_hidden());
    }
    button.child(text(label).font(LABEL).color(theme.text))
}

pub fn top_transition() -> Transition {
    Transition::offset(0.0, -8.0)
        .combined(Transition::opacity())
        .springs(CHROME_MOTION)
}

pub fn bottom_transition() -> Transition {
    Transition::offset(0.0, 12.0)
        .combined(Transition::opacity())
        .springs(CHROME_MOTION)
}

pub fn primary_button(label: impl Into<String>, theme: &Palette) -> El {
    let label = label.into();
    row()
        .h(40.0)
        .px(16.0)
        .gap(8.0)
        .center_y()
        .radius(9.0)
        .fill(theme.accent)
        .hover(theme.accent_hover)
        .press(theme.accent_hover)
        .press_scale(0.96)
        .focusable(true)
        .focus_ring(2.0, 2.0, theme.focus)
        .role("button")
        .label(label.clone())
        .child(
            text(label)
                .font(TextStyle::new(13.0, 600.0))
                .color(theme.accent_ink),
        )
}

pub fn ghost_button(label: impl Into<String>, theme: &Palette) -> El {
    let label = label.into();
    row()
        .h(40.0)
        .px(10.0)
        .center_y()
        .radius(8.0)
        .hover(theme.hover)
        .press(theme.press)
        .press_scale(0.96)
        .focusable(true)
        .focus_ring(2.0, 2.0, theme.focus)
        .role("button")
        .label(label.clone())
        .child(text(label).font(LABEL).color(theme.text_muted))
}

pub fn menu_row(label: impl Into<String>, selected: bool, theme: &Palette) -> El {
    let label = label.into();
    let mut row = row()
        .h(34.0)
        .w_full()
        .px(8.0)
        .gap(8.0)
        .center_y()
        .radius(7.0)
        .hover(theme.hover)
        .press(theme.press)
        .focusable(true)
        .focus_ring(2.0, 2.0, theme.focus)
        .role("menuitemradio")
        .checked(selected)
        .label(label.clone());
    if selected {
        row = row.fill(theme.accent_soft);
    }
    row.child(el().size(16.0, 16.0).center().child(if selected {
        icon(&icons::CHECK, 16.0).color(theme.accent).a11y_hidden()
    } else {
        el()
    }))
    .child(
        text(label)
            .font(LABEL)
            .color(if selected {
                theme.text
            } else {
                theme.text_muted
            })
            .truncate()
            .grow(1.0),
    )
}

pub fn menu_empty(label: impl Into<String>, theme: &Palette) -> El {
    el().w_full()
        .padding(10.0)
        .child(text(label).font(LABEL).color(theme.text_faint).wrap())
}

pub fn segment(label: impl Into<String>, selected: bool, theme: &Palette) -> El {
    let label = label.into();
    row()
        .h(28.0)
        .px(10.0)
        .center_y()
        .radius(7.0)
        .fill(if selected {
            theme.accent
        } else {
            Color::TRANSPARENT
        })
        .hover(if selected {
            theme.accent_hover
        } else {
            theme.hover
        })
        .press(theme.press)
        .press_scale(0.96)
        .focusable(true)
        .focus_ring(2.0, 2.0, theme.focus)
        .role("button")
        .label(label.clone())
        .child(text(label).font(META).color(if selected {
            theme.accent_ink
        } else {
            theme.text_muted
        }))
}

pub fn sans_family() -> &'static str {
    static SANS: OnceLock<String> = OnceLock::new();
    SANS.get_or_init(|| {
        let family = gnome_font()
            .or_else(kde_font)
            .or_else(fontconfig_font)
            .unwrap_or_else(|| "Inter".into());
        format!("{family}, Inter")
    })
}

fn command_output(command: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(command).args(args).output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

fn gnome_font() -> Option<String> {
    parse_gnome_font(&command_output(
        "gsettings",
        &["get", "org.gnome.desktop.interface", "font-name"],
    )?)
}

fn parse_gnome_font(value: &str) -> Option<String> {
    let value = value.trim().trim_matches('\'').trim();
    if value.is_empty() {
        return None;
    }
    match value.rsplit_once(' ') {
        Some((family, size)) if size.parse::<f32>().is_ok() && !family.is_empty() => {
            Some(family.to_string())
        }
        Some(_) => None,
        None => Some(value.to_string()),
    }
}

fn kde_font() -> Option<String> {
    parse_kde_font(&command_output(
        "kreadconfig6",
        &[
            "--file",
            "kdeglobals",
            "--group",
            "General",
            "--key",
            "font",
        ],
    )?)
}

fn parse_kde_font(value: &str) -> Option<String> {
    let family = value.split(',').next()?.trim();
    (!family.is_empty()).then(|| family.to_string())
}

fn fontconfig_font() -> Option<String> {
    let family = command_output("fc-match", &["--format=%{family[0]}", "sans-serif"])?;
    let family = family.trim();
    (!family.is_empty()).then(|| family.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gnome_font_settings_lose_their_size() {
        assert_eq!(
            parse_gnome_font("'Adwaita Sans 11'\n").as_deref(),
            Some("Adwaita Sans")
        );
        assert_eq!(
            parse_gnome_font("Cantarell 10.5").as_deref(),
            Some("Cantarell")
        );
    }

    #[test]
    fn kde_font_settings_lose_the_trailing_metrics() {
        assert_eq!(
            parse_kde_font("Noto Sans,10,-1,0,400,0,0,0,0,0,0,0,0,0,0,1").as_deref(),
            Some("Noto Sans")
        );
    }

    #[test]
    fn missing_font_settings_are_ignored() {
        assert_eq!(parse_gnome_font(""), None);
        assert_eq!(parse_gnome_font("' '"), None);
        assert_eq!(parse_kde_font(""), None);
    }

    #[test]
    fn the_system_font_stack_keeps_a_fallback() {
        assert!(sans_family().ends_with(", Inter"));
    }
}
