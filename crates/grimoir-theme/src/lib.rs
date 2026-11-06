//! Theme schema, loading, and CSS generation for Grimoir.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Theme {
    pub version: u32,
    pub name: String,
    pub mode: String,
    pub colors: ThemeColors,
    pub wallpaper: Option<String>,
    pub fonts: ThemeFonts,
    pub radius: u32,
    pub blur: BlurConfig,
    pub opacity: OpacityConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ThemeColors {
    pub bg: String,
    pub surface: String,
    pub surface_alt: String,
    pub text: String,
    pub text_muted: String,
    pub accent: String,
    pub border: String,
    pub shadow: String,
    pub danger: String,
    pub success: String,
    pub warning: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ThemeFonts {
    pub ui: String,
    pub ui_size: f32,
    pub reading: String,
    pub reading_size: f32,
    pub mono: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct BlurConfig {
    pub enabled: bool,
    pub size: u32,
    pub passes: u32,
    pub vibrancy: f32,
    pub radius_px: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct OpacityConfig {
    pub chrome: f32,
    pub sidebar: f32,
    pub letterbox: f32,
    pub paper: f32,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            version: 1,
            name: "Grimoir Fallback".to_owned(),
            mode: "dark".to_owned(),
            colors: ThemeColors::default(),
            wallpaper: None,
            fonts: ThemeFonts::default(),
            radius: 4,
            blur: BlurConfig::default(),
            opacity: OpacityConfig::default(),
        }
    }
}

impl Default for ThemeColors {
    fn default() -> Self {
        Self {
            bg: "#080d0b".to_owned(),
            surface: "#121a15".to_owned(),
            surface_alt: "#1a241d".to_owned(),
            text: "#dce9dd".to_owned(),
            text_muted: "#9aa89c".to_owned(),
            accent: "#b7c9b9".to_owned(),
            border: "#263329".to_owned(),
            shadow: "#040706".to_owned(),
            danger: "#c08a72".to_owned(),
            success: "#9bb88c".to_owned(),
            warning: "#d4c79a".to_owned(),
        }
    }
}

impl Default for ThemeFonts {
    fn default() -> Self {
        Self {
            ui: "IBM Plex Mono".to_owned(),
            ui_size: 10.0,
            reading: "Noto Serif".to_owned(),
            reading_size: 11.0,
            mono: "Hack".to_owned(),
        }
    }
}

impl Default for BlurConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            size: 2,
            passes: 2,
            vibrancy: 0.4,
            radius_px: 24,
        }
    }
}

impl Default for OpacityConfig {
    fn default() -> Self {
        Self {
            chrome: 0.65,
            sidebar: 0.55,
            letterbox: 0.0,
            paper: 0.82,
        }
    }
}

impl Theme {
    pub fn load(path: impl AsRef<std::path::Path>) -> Result<Self> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read theme {}", path.display()))?;
        Self::from_json_str(&text)
            .with_context(|| format!("failed to parse theme {}", path.display()))
    }

    /// Parses either Grimoir's own theme schema or a Hyprland `theme-apply`
    /// theme file. The latter is a development fallback and is also what the
    /// personal layer will translate into the public schema.
    pub fn from_json_str(text: &str) -> Result<Self> {
        let value: Value = serde_json::from_str(text)?;
        let colors = value.get("colors").and_then(Value::as_object);

        let is_app_theme = colors.is_some_and(|c| c.contains_key("bg") || c.contains_key("text"));
        let is_hypr_theme = colors.is_some_and(|c| {
            c.contains_key("background") || c.contains_key("color0") || c.contains_key("foreground")
        });

        if is_app_theme {
            let theme: Theme = serde_json::from_value(value)?;
            Ok(theme)
        } else if is_hypr_theme {
            Ok(Self::from_hypr_value(&value))
        } else {
            bail!("theme has no recognizable colors (expected bg/text or background/color0)")
        }
    }

    fn from_hypr_value(value: &Value) -> Self {
        let colors = value.get("colors").and_then(Value::as_object);

        let get = |key: &str| {
            colors
                .and_then(|c| c.get(key))
                .and_then(Value::as_str)
                .map(str::to_owned)
        };

        let bg = get("background")
            .or_else(|| get("color0"))
            .unwrap_or_else(|| "#080d0b".to_owned());
        let text = get("foreground")
            .or_else(|| get("color7"))
            .unwrap_or_else(|| "#dce9dd".to_owned());
        let surface = get("surface").unwrap_or_else(|| mix_hex(&bg, &text, 0.10));
        let border = get("border")
            .or_else(|| get("color8"))
            .unwrap_or_else(|| mix_hex(&bg, &text, 0.18));
        let accent = get("active_border")
            .or_else(|| get("color5"))
            .or_else(|| get("cursor"))
            .unwrap_or_else(|| "#b7c9b9".to_owned());

        let surface_alt = mix_hex(&surface, &text, 0.08);
        let text_muted = mix_hex(&text, &bg, 0.35);
        let shadow = get("shadow").unwrap_or_else(|| bg.clone());
        let danger = get("color9")
            .or_else(|| get("color1"))
            .unwrap_or_else(|| "#c08a72".to_owned());
        let success = get("color10")
            .or_else(|| get("color2"))
            .unwrap_or_else(|| "#9bb88c".to_owned());
        let warning = get("color11")
            .or_else(|| get("color3"))
            .unwrap_or_else(|| "#d4c79a".to_owned());

        Theme {
            name: value
                .get("name")
                .or_else(|| value.get("theme"))
                .and_then(Value::as_str)
                .unwrap_or("Hyprland")
                .to_owned(),
            mode: value
                .get("mode")
                .and_then(Value::as_str)
                .unwrap_or("dark")
                .to_owned(),
            wallpaper: value
                .get("wallpaper")
                .and_then(Value::as_str)
                .map(str::to_owned),
            colors: ThemeColors {
                bg,
                surface,
                surface_alt,
                text,
                text_muted,
                accent,
                border,
                shadow,
                danger,
                success,
                warning,
            },
            ..Theme::default()
        }
    }

    /// Produces the concrete CSS applied to the reader window.
    ///
    /// `backdrop` is only enabled when the running GTK understands
    /// `backdrop-filter` (GTK 4.24+). GTK ignores the declaration on older
    /// versions, but avoiding it keeps the CSS provider quiet.
    pub fn css(&self, backdrop: bool) -> String {
        let c = &self.colors;
        let ui_font = css_font(&self.fonts.ui);
        let radius = self.radius.max(1);
        let bar_radius = radius.saturating_sub(1).max(1);
        let blur = if self.blur.enabled {
            self.blur.radius_px.max(1)
        } else {
            0
        };

        let backdrop_window = if backdrop && self.blur.enabled {
            format!("  backdrop-filter: blur({blur}px);\n")
        } else {
            String::new()
        };
        let backdrop_bar = if backdrop && self.blur.enabled {
            format!("  backdrop-filter: blur({}px);\n", blur / 2 + 2)
        } else {
            String::new()
        };

        format!(
            r#"window.grimoir-reader {{
  background-color: {window_bg};
  color: {text};
  font-family: {ui_font};
  font-size: {ui_size}pt;
{backdrop_window}}}

.grimoir-canvas {{
  background-color: transparent;
}}

.epub-scroll,
.epub-scroll viewport,
.epub-text,
.epub-text text {{
  background-color: transparent;
  background-image: none;
  color: {text};
}}

.epub-page {{
  background-color: {bar_bg};
  border: 1px solid {bar_border};
  border-radius: 14px;
  box-shadow: 0 12px 34px {bar_shadow};
}}

.epub-text {{
  font-size: 15pt;
}}

.epub-text text {{
  color: {text};
  caret-color: {accent};
}}

.reader-bar {{
  background-color: {bar_bg};
  border: 1px solid {bar_border};
  border-radius: {radius}px;
  padding: 6px 10px;
  box-shadow: 0 4px 18px {bar_shadow};
  color: {text};
{backdrop_bar}}}

.reader-bar label {{
  color: {text};
}}

.reader-bar .reader-muted {{
  color: {text_muted};
}}

.reader-bar .reader-title {{
  font-weight: bold;
}}

.reader-bar button,
.reader-bar togglebutton {{
  background-image: none;
  background-color: {button_bg};
  color: {text};
  border: 1px solid {button_border};
  border-radius: {bar_radius}px;
  min-height: 24px;
  min-width: 24px;
  padding: 2px 8px;
  box-shadow: none;
}}

.reader-bar button:hover,
.reader-bar togglebutton:hover {{
  background-color: {button_hover};
  border-color: {accent};
}}

.reader-bar button:checked,
.reader-bar togglebutton:checked {{
  background-image: none;
  background-color: {button_checked};
  border-color: {accent};
  color: {on_accent};
}}

.reader-bar separator {{
  background-color: {border};
  min-width: 1px;
}}

.reader-bar scale {{
  min-width: 84px;
}}

.reader-bar scale trough {{
  background-color: {trough};
  border-radius: 2px;
}}

.reader-bar scale highlight {{
  background-color: {accent};
  border-radius: 2px;
}}

.reader-bar scale slider {{
  background-color: {text};
  border-radius: 6px;
}}

.reader-page-label {{
  color: {text_muted};
  min-width: 84px;
}}

.reader-bar progressbar trough {{
  background-color: {trough};
  border-radius: 2px;
}}

.reader-bar progressbar progress {{
  background-color: {accent};
  border-radius: 2px;
}}

.reader-bar dropdown {{
  background-image: none;
  background-color: {button_bg};
  color: {text};
  border: 1px solid {button_border};
  border-radius: {bar_radius}px;
  min-height: 24px;
}}

popover,
popover contents {{
  background-color: {bar_bg};
  background-image: none;
  color: {text};
}}

popover listview,
popover row {{
  background-color: transparent;
  background-image: none;
  color: {text};
}}

popover row:hover {{
  background-color: {button_hover};
}}

.card-menu,
.card-menu contents {{
  background-color: {bar_bg};
  background-image: none;
  border: 1px solid {bar_border};
  border-radius: 10px;
  box-shadow: 0 10px 28px {bar_shadow};
}}

.card-menu .library-menu-item {{
  background-image: none;
  background-color: transparent;
  color: {text};
  border-radius: 6px;
  padding: 6px 10px;
}}

.card-menu .library-menu-item:hover {{
  background-color: {button_hover};
}}

.reader-popover,
.reader-popover contents {{
  background-color: {bar_bg};
  background-image: none;
  color: {text};
  border: 1px solid {bar_border};
  border-radius: {radius}px;
}}

.reader-popover scrolledwindow,
.reader-popover list,
.reader-popover row {{
  background-color: transparent;
  background-image: none;
  color: {text};
}}

.reader-popover label {{
  color: {text};
}}

.reader-popover entry,
.reader-popover searchbar,
.reader-popover .search {{
  background-color: {button_bg};
  background-image: none;
  border: 1px solid {button_border};
  border-radius: {bar_radius}px;
  color: {text};
}}

.reader-popover entry text,
.reader-popover searchbar text {{
  background-color: transparent;
  background-image: none;
  color: {text};
}}

.reader-popover entry:focus-within,
.reader-popover searchbar:focus-within {{
  border-color: {accent};
  outline: none;
  box-shadow: none;
}}

.reader-popover row {{
  border-radius: {bar_radius}px;
}}

.reader-popover row:hover,
.reader-popover row:selected {{
  background-color: {button_hover};
}}

.reader-popover row:selected label {{
  color: {text};
}}

.reader-popover row:disabled label {{
  color: {text_muted};
}}

window.library-window {{
  background-color: transparent;
  background-image: none;
  color: {text};
  font-family: {ui_font};
  font-size: {ui_size}pt;
}}

.library-scroll,
.library-scroll viewport {{
  background-color: transparent;
  background-image: none;
}}

.library-topbar {{
  background-color: {bar_bg};
  border: 1px solid {bar_border};
  border-radius: 9px;
  padding: 5px 8px;
  box-shadow: 0 8px 24px {bar_shadow};
  color: {text};
{backdrop_bar}}}

.library-brand {{
  color: {text_muted};
  font-size: 9pt;
  font-weight: bold;
  letter-spacing: 4px;
  margin-right: 10px;
}}

.library-topbar .library-search,
.library-topbar entry,
.library-topbar searchbar {{
  background-color: {button_bg};
  background-image: none;
  border: 1px solid {button_border};
  border-radius: 6px;
  color: {text};
  min-height: 28px;
}}

.library-topbar .library-search text,
.library-topbar entry text,
.library-topbar searchbar text {{
  background-color: transparent;
  background-image: none;
  color: {text};
}}

.library-topbar .library-search:focus,
.library-topbar .library-search:focus-visible,
.library-topbar .library-search:focus-within,
.library-topbar entry:focus,
.library-topbar entry:focus-visible,
.library-topbar entry:focus-within,
window.library-window .library-topbar entry.library-search:focus,
window.library-window .library-topbar entry.library-search:focus-visible,
window.library-window .library-topbar entry.library-search:focus-within {{
  border-color: {accent};
  outline-style: none;
  outline-width: 0;
  outline-color: transparent;
  box-shadow: none;
}}

.library-topbar .library-search placeholder,
.library-topbar entry placeholder {{
  color: {text_muted};
}}

.library-topbar menubutton {{
  background-image: none;
  background-color: transparent;
  border: none;
  box-shadow: none;
  padding: 0;
}}

.library-topbar menubutton > button {{
  background-image: none;
  background-color: transparent;
  border: 1px solid transparent;
  border-radius: 6px;
  color: {text_muted};
  box-shadow: none;
  min-height: 26px;
  padding: 3px 8px;
}}

.library-topbar menubutton > button:hover {{
  background-color: {button_hover};
  color: {text};
}}

.library-topbar menubutton > button:checked,
.library-topbar menubutton > button:active {{
  background-color: {accent_soft};
  color: {accent};
  border-color: transparent;
}}

.library-topbar menubutton > button:focus {{
  background-color: transparent;
  border-color: transparent;
  color: {text};
  outline: none;
  box-shadow: none;
}}

.library-chip {{
  background-image: none;
  background-color: transparent;
  border: 1px solid transparent;
  border-radius: 5px;
  color: {text_muted};
  font-size: 9pt;
  min-height: 24px;
  padding: 3px 8px;
}}

.library-chip:hover {{
  background-color: {button_hover};
  color: {text};
}}

.library-chip.active,
.library-chip:checked {{
  background-color: {accent_soft};
  border-color: transparent;
  color: {accent};
}}

.library-icon {{
  background-image: none;
  background-color: transparent;
  border: 1px solid transparent;
  border-radius: 6px;
  color: {text_muted};
  min-height: 26px;
  min-width: 26px;
  padding: 3px;
}}

.library-icon:hover {{
  background-color: {button_hover};
}}

.library-menu {{
  background-color: {bar_bg};
}}

.library-menu-heading {{
  color: {text_muted};
  font-size: 8pt;
  font-weight: bold;
  letter-spacing: 2px;
  padding: 4px 8px;
}}

.library-menu-item {{
  background-image: none;
  background-color: transparent;
  border: 1px solid transparent;
  border-radius: 4px;
  color: {text};
  padding: 6px 10px;
}}

.library-menu-item:hover {{
  background-color: {button_hover};
}}

.library-menu-item.active {{
  background-color: {button_checked};
  color: {on_accent};
}}

.library-bottombar {{
  background-color: {bar_bg};
  border: 1px solid {bar_border};
  border-radius: 8px;
  padding: 5px 12px;
  box-shadow: 0 6px 18px {bar_shadow};
  color: {text_muted};
{backdrop_bar}}}

.library-status,
.library-count {{
  color: {text_muted};
  font-size: 9pt;
}}

.library-grid {{
  background-color: transparent;
  outline: none;
}}

.library-grid:focus,
.library-grid:focus-within {{
  outline: none;
  box-shadow: none;
}}

.library-grid flowboxchild {{
  background-color: transparent;
  padding: 8px;
}}

.showcase-card {{
  background-color: transparent;
  background-image: none;
  border: none;
  box-shadow: none;
  outline: none;
  padding: 4px;
  color: {text};
}}

.showcase-card:hover,
.showcase-card:focus,
.showcase-card:active {{
  background-color: transparent;
  background-image: none;
  border: none;
  outline: none;
  box-shadow: none;
}}

.card-actions {{
  background-color: transparent;
}}

.card-action {{
  background-image: none;
  background-color: {bar_bg};
  border: 1px solid {bar_border};
  border-radius: 999px;
  color: {text};
  min-width: 25px;
  min-height: 25px;
  padding: 2px;
  opacity: 0.86;
  box-shadow: 0 3px 10px {cover_shadow};
}}

.card-action:hover {{
  background-color: {button_hover};
  border-color: {accent};
  opacity: 1;
}}

.card-action:checked,
.favorite-star:checked {{
  background-color: {button_checked};
  border-color: {accent};
  color: {on_accent};
  opacity: 1;
}}

window.book-dialog {{
  background-color: {bg};
  border: 1px solid {bar_border};
  border-radius: 16px;
}}

.book-dialog entry,
.book-dialog spinbutton,
.book-dialog spinbutton entry,
.book-dialog combobox button {{
  background-image: none;
  background-color: {button_bg};
  border: 1px solid {button_border};
  border-radius: 8px;
  color: {text};
  min-height: 28px;
}}

.book-dialog entry text,
.book-dialog spinbutton text {{
  background-color: transparent;
  background-image: none;
  color: {text};
}}

.book-dialog entry:focus-within,
.book-dialog spinbutton:focus-within,
.book-dialog combobox button:focus {{
  border-color: {accent};
  outline: none;
  box-shadow: none;
}}

.book-dialog checkbutton {{
  color: {text};
}}

.cover-stage {{
  min-width: 172px;
  min-height: 258px;
  background: transparent;
  border-radius: 2px 8px 8px 2px;
  box-shadow: none;
}}

.showcase-cover {{
  min-width: 172px;
  min-height: 258px;
  border-left: 3px solid {cover_edge};
  border-radius: 2px 8px 8px 2px;
  background-color: rgba(0,0,0,0.001);
  background-image: none;
  box-shadow: -8px 10px 22px {cover_shadow};
  transform: perspective(720px) rotateY(-17deg) rotateX(3deg);
  transform-origin: center center;
}}

.showcase-cover.tilt-right {{
  transform: perspective(720px) rotateY(17deg) rotateX(3deg);
}}

.showcase-card:hover .showcase-cover {{
  transform: perspective(720px) rotateY(-17deg) rotateX(3deg) translateY(-6px);
  box-shadow: -12px 6px 18px {cover_shadow}, -24px 34px 48px {cover_shadow},
    0 0 0 1px {cover_edge} inset;
}}

.showcase-card:hover .showcase-cover.tilt-right {{
  transform: perspective(720px) rotateY(17deg) rotateX(3deg) translateY(-6px);
}}

.cover-shelf {{
  background-color: {shelf_shadow};
  border-radius: 50%;
  margin-top: 2px;
}}

.showcase-title {{
  color: {text};
  font-weight: bold;
  text-shadow: 0 2px 8px {cover_shadow};
}}

.showcase-muted {{
  color: {text_muted};
  font-size: 9pt;
  text-shadow: 0 1px 5px {cover_shadow};
}}

.showcase-progress {{
  background-color: {trough};
  border-radius: 1px;
  margin-top: 3px;
}}

.showcase-progress progress {{
  background-color: {accent};
  border-radius: 1px;
}}

.library-empty-state {{
  background-color: {bar_bg};
  border: 1px solid {bar_border};
  border-radius: 16px;
  padding: 22px 28px;
  box-shadow: 0 12px 32px {bar_shadow};
{backdrop_bar}}}

.library-empty {{
  color: {text};
  font-size: 16pt;
  font-weight: bold;
  text-shadow: 0 2px 10px {cover_shadow};
}}

.library-empty-muted {{
  color: {text_muted};
  font-size: 10pt;
  text-shadow: 0 1px 6px {cover_shadow};
}}

.library-empty-action {{
  background-image: none;
  background-color: {button_checked};
  border: 1px solid {accent};
  border-radius: 12px;
  color: {on_accent};
  padding: 6px 16px;
}}

.library-empty-action:hover {{
  background-color: {accent};
}}
"#,
            bg = c.bg,
            window_bg = css_rgba(&c.surface, self.opacity.letterbox),
            bar_bg = css_rgba(&c.surface, self.opacity.chrome),
            bar_border = css_rgba(&c.border, 0.9),
            bar_shadow = css_rgba(&c.shadow, 0.55),
            button_bg = css_rgba(&c.surface_alt, 0.82),
            button_border = css_rgba(&c.border, 0.75),
            button_hover = css_rgba(&c.accent, 0.22),
            button_checked = css_rgba(&c.accent, 0.55),
            on_accent = c.surface,
            trough = css_rgba(&c.border, 0.65),
            cover_shadow = css_rgba(&c.shadow, 0.78),
            cover_edge = css_rgba(&c.text, 0.08),
            shelf_shadow = css_rgba(&c.shadow, 0.45),
            text = c.text,
            text_muted = c.text_muted,
            accent = c.accent,
            accent_soft = css_rgba(&c.accent, 0.16),
            border = css_rgba(&c.border, 0.8),
            radius = radius,
            bar_radius = bar_radius,
            ui_font = ui_font,
            ui_size = self.fonts.ui_size,
        )
    }
}

fn css_font(name: &str) -> String {
    if name.contains(' ') {
        format!("\"{}\"", name.replace('"', "\\\""))
    } else {
        name.to_owned()
    }
}

fn parse_hex(value: &str) -> Option<(u8, u8, u8)> {
    let value = value.trim().trim_start_matches('#');
    if value.len() != 6 && value.len() != 8 {
        return None;
    }
    let r = u8::from_str_radix(&value[0..2], 16).ok()?;
    let g = u8::from_str_radix(&value[2..4], 16).ok()?;
    let b = u8::from_str_radix(&value[4..6], 16).ok()?;
    Some((r, g, b))
}

fn css_rgba(value: &str, alpha: f32) -> String {
    let (r, g, b) = parse_hex(value).unwrap_or((0, 0, 0));
    format!("rgba({r},{g},{b},{:.3})", alpha.clamp(0.0, 1.0))
}

fn mix_hex(a: &str, b: &str, t: f32) -> String {
    let a = parse_hex(a).unwrap_or((0, 0, 0));
    let b = parse_hex(b).unwrap_or((255, 255, 255));
    let t = t.clamp(0.0, 1.0);
    let mix = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    format!(
        "#{:02x}{:02x}{:02x}",
        mix(a.0, b.0),
        mix(a.1, b.1),
        mix(a.2, b.2)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_app_theme() {
        let theme = Theme::from_json_str(
            r##"{
                "version": 1,
                "name": "Test",
                "colors": { "bg": "#000000", "text": "#ffffff" },
                "opacity": { "paper": 0.5 }
            }"##,
        )
        .unwrap();
        assert_eq!(theme.name, "Test");
        assert_eq!(theme.colors.bg, "#000000");
        assert_eq!(theme.opacity.paper, 0.5);
    }

    #[test]
    fn parses_hypr_theme() {
        let theme = Theme::from_json_str(
            r##"{
                "name": "Forest_Dove",
                "mode": "dark",
                "colors": {
                    "background": "#080d0b",
                    "foreground": "#dce9dd",
                    "surface": "#121a15",
                    "border": "#263329",
                    "active_border": "#b7c9b9"
                }
            }"##,
        )
        .unwrap();
        assert_eq!(theme.name, "Forest_Dove");
        assert_eq!(theme.colors.bg, "#080d0b");
        assert_eq!(theme.colors.accent, "#b7c9b9");
    }

    #[test]
    fn css_uses_theme_tokens() {
        let theme = Theme::default();
        let css = theme.css(true);
        assert!(css.contains("window.grimoir-reader"));
        assert!(css.contains("#dce9dd"));
        assert!(css.contains("backdrop-filter"));
    }
}
