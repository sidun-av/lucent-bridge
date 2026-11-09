//! Make Windows programs look at home on the desktop they run on.
//!
//! Before each launch, while no `wineserver` has the prefix open, the
//! prefix's registry gets:
//!
//! - **colours** for the desktop's current light or dark mode, from Lucent's
//!   "Pebble Drift" design tokens (`docs/mockups/concepts-2026/pebble-drift/
//!   pebble-drift.css` in lucent-os), plus Windows' own "apps use light
//!   theme" switch that dark-mode-aware programs read. Light keeps Wine's
//!   Aero visual style (flat, light controls) with Lucent's colours; Wine
//!   has no dark visual style, so dark uses classic drawing, whose bevels
//!   the palette keeps soft;
//! - the desktop's **UI font** (Inter on Lucent) for dialogs ("MS Shell
//!   Dlg"), menus, message boxes and status bars, with Tahoma linked as the
//!   fallback for characters the UI font lacks (see [`font_values`]);
//! - **DPI** from the Plasma scale factor (what KWin gives X11 apps as
//!   `Xft.dpi`), so programs are neither tiny nor blurry;
//! - **smooth text** matching the desktop's anti-aliasing / sub-pixel
//!   setting.
//!
//! The mode is read at each launch, so a program started after switching
//! to dark comes up dark. `LUCENT_BRIDGE_APPEARANCE=off` leaves the prefix
//! alone; `light` / `dark` force a mode; `LUCENT_BRIDGE_DPI` forces a DPI.

use crate::regfile::{self, Value};
use serde::Serialize;
use std::path::{Path, PathBuf};

pub const ENV_MODE: &str = "LUCENT_BRIDGE_APPEARANCE";
pub const ENV_DPI: &str = "LUCENT_BRIDGE_DPI";
pub const ENV_FONT: &str = "LUCENT_BRIDGE_UI_FONT";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Light,
    Dark,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Smoothing {
    Off,
    Grayscale,
    /// ClearType, sub-pixels in RGB order.
    Rgb,
    /// ClearType, sub-pixels in BGR order.
    Bgr,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Settings {
    pub mode: Mode,
    pub dpi: u32,
    /// Where the DPI came from (for the log).
    #[serde(skip)]
    pub dpi_from: &'static str,
    pub smoothing: Smoothing,
    /// UI font family; `None` keeps Wine's (Tahoma).
    pub font: Option<String>,
    /// Its Medium weight as a family of its own ("Inter Medium"), if installed.
    pub font_medium: Option<String>,
}

/// What the desktop says, gathered by the caller (files and commands), so
/// detection itself is pure.
#[derive(Debug, Default, Clone)]
pub struct Desktop {
    /// `kdeglobals` files, the user's first, then the system-wide ones.
    pub kdeglobals: Vec<String>,
    /// `kwinrc` files, same order.
    pub kwinrc: Vec<String>,
    /// Output of `xrdb -query`, if it ran.
    pub xresources: Option<String>,
    /// Whether a font family is installed.
    pub has_font: Option<fn(&str) -> bool>,
}

/// A KConfig value: the first file (highest priority) that has it wins.
pub fn kconfig_get(files: &[String], group: &str, key: &str) -> Option<String> {
    for text in files {
        let mut in_group = false;
        for line in text.lines() {
            let line = line.trim();
            if line.starts_with('[') {
                in_group = line == format!("[{group}]");
                continue;
            }
            if !in_group || line.starts_with('#') {
                continue;
            }
            if let Some((k, v)) = line.split_once('=') {
                // `key[$i]` (immutable) and plain `key` both count.
                let k = k.trim();
                let k = k.split_once('[').map(|(a, _)| a).unwrap_or(k);
                if k == key {
                    return Some(v.trim().to_string());
                }
            }
        }
    }
    None
}

/// Light or dark, from the window background of the desktop's colour
/// scheme (as Plasma's own portal decides), else from the scheme's name.
pub fn detect_mode(kdeglobals: &[String]) -> Mode {
    if let Some(bg) = kconfig_get(kdeglobals, "Colors:Window", "BackgroundNormal") {
        let c: Vec<f64> = bg
            .split(',')
            .filter_map(|x| x.trim().parse::<f64>().ok())
            .collect();
        if c.len() >= 3 {
            let luma = (0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]) / 255.0;
            return if luma < 0.5 { Mode::Dark } else { Mode::Light };
        }
    }
    match kconfig_get(kdeglobals, "General", "ColorScheme") {
        Some(s) if s.to_ascii_lowercase().contains("dark") => Mode::Dark,
        _ => Mode::Light,
    }
}

/// The DPI Windows programs should use: `Xft.dpi` (what KWin hands X11
/// programs that scale themselves, 96 x the Plasma scale factor), else
/// the scale KWin recorded for X11 programs, else 96.
pub fn detect_dpi(xresources: Option<&str>, kwinrc: &[String]) -> u32 {
    dpi_and_source(xresources, kwinrc).0
}

/// [`detect_dpi`] and where the value came from (for the log).
pub fn dpi_and_source(xresources: Option<&str>, kwinrc: &[String]) -> (u32, &'static str) {
    let from_xft = xresources.and_then(|x| {
        x.lines().find_map(|l| {
            let (k, v) = l.split_once(':')?;
            (k.trim() == "Xft.dpi").then(|| v.trim().parse::<f64>().ok())?
        })
    });
    let from_scale = || {
        kconfig_get(kwinrc, "Xwayland", "Scale")
            .and_then(|s| s.parse::<f64>().ok())
            .map(|s| s * 96.0)
    };
    let (dpi, source) = match (from_xft, xresources) {
        (Some(d), _) => (d, "Xft.dpi"),
        (None, x) => match from_scale() {
            Some(d) => (d, "KWin Xwayland scale"),
            None if x.is_some() => (96.0, "no Xft.dpi"),
            None => (96.0, "no X resources"),
        },
    };
    ((dpi.round() as u32).clamp(72, 480), source)
}

/// Text smoothing that matches the desktop (Plasma's font settings).
///
/// Unset means Windows' own default, ClearType RGB: Wine's grayscale mode
/// blends through a fixed table (win32u/dibdrv/graphics.c, `ramp`) that
/// leaves dark-on-light stems light grey (50 % coverage of black on white
/// comes out 74 % white), while ClearType honours FontSmoothingGamma
/// ([`text_gamma`]). Plasma's explicit "none" (and the vertical orders
/// Windows has no ClearType for) get grayscale.
pub fn detect_smoothing(kdeglobals: &[String]) -> Smoothing {
    if kconfig_get(kdeglobals, "General", "XftAntialias").as_deref() == Some("false") {
        return Smoothing::Off;
    }
    match kconfig_get(kdeglobals, "General", "XftSubPixel").as_deref() {
        Some("bgr") => Smoothing::Bgr,
        Some("none" | "vrgb" | "vbgr") => Smoothing::Grayscale,
        _ => Smoothing::Rgb,
    }
}

/// The family of the desktop's general font (`Inter,11,-1,...` -> Inter).
pub fn desktop_font(kdeglobals: &[String]) -> Option<String> {
    kconfig_get(kdeglobals, "General", "font")
        .and_then(|f| f.split(',').next().map(|s| s.trim().to_string()))
        .filter(|s| !s.is_empty())
}

/// Everything at once. `None` when appearance management is off.
pub fn detect(env: &dyn Fn(&str) -> Option<String>, desktop: &Desktop) -> Option<Settings> {
    let forced = env(ENV_MODE).unwrap_or_default().to_ascii_lowercase();
    let mode = match forced.as_str() {
        "off" | "none" | "0" | "no" => return None,
        "light" => Mode::Light,
        "dark" => Mode::Dark,
        _ => detect_mode(&desktop.kdeglobals),
    };
    let (dpi, dpi_from) = env(ENV_DPI)
        .and_then(|d| d.trim().parse::<u32>().ok())
        .map(|d| (d.clamp(72, 480), ENV_DPI))
        .unwrap_or_else(|| dpi_and_source(desktop.xresources.as_deref(), &desktop.kwinrc));
    let has_font = desktop.has_font.unwrap_or(|_| false);
    let font = env(ENV_FONT)
        .filter(|f| !f.is_empty())
        .or_else(|| desktop_font(&desktop.kdeglobals))
        .or_else(|| Some("Inter".to_string()))
        .filter(|f| has_font(f));
    let font_medium = font
        .as_ref()
        .map(|f| format!("{f} Medium"))
        .filter(|m| has_font(m));
    Some(Settings {
        mode,
        dpi,
        dpi_from,
        smoothing: detect_smoothing(&desktop.kdeglobals),
        font,
        font_medium,
    })
}

type Rgb = (u8, u8, u8);

/// Windows system colours (`HKCU\Control Panel\Colors`) from the Pebble
/// Drift tokens. Translucent tokens are composited over
/// `--surface-window-solid`, since Windows colours are opaque.
pub fn palette(mode: Mode) -> Vec<(&'static str, Rgb)> {
    // name: token
    match mode {
        Mode::Light => vec![
            ("ActiveBorder", (242, 243, 250)),        // --surface-window-solid
            ("ActiveTitle", (66, 87, 232)), // --accent (programs reuse it, e.g. Notepad++ tab text)
            ("AppWorkSpace", (221, 223, 236)), // --surface-sunken-strong
            ("Background", (223, 229, 255)), // desktop behind Drift
            ("ButtonAlternateFace", (243, 244, 251)), // --surface-raised-2
            ("ButtonDkShadow", (181, 185, 208)), // --line-control
            ("ButtonFace", (242, 243, 250)), // --surface-window-solid
            ("ButtonHilight", (255, 255, 255)), // pillow top light
            ("ButtonLight", (243, 244, 251)), // --surface-raised-2
            ("ButtonShadow", (208, 210, 226)), // --line-strong
            ("ButtonText", (23, 26, 43)),   // --text-primary
            ("GradientActiveTitle", (66, 87, 232)),
            ("GradientInactiveTitle", (221, 223, 236)),
            ("GrayText", (144, 149, 168)),       // --text-disabled
            ("Hilight", (66, 87, 232)),          // --accent
            ("HilightText", (255, 255, 255)),    // --text-on-accent
            ("HotTrackingColor", (51, 70, 207)), // --accent-text
            ("InactiveBorder", (242, 243, 250)),
            ("InactiveTitle", (221, 223, 236)), // --surface-sunken-strong
            ("InactiveTitleText", (71, 77, 102)), // --text-secondary
            ("InfoText", (23, 26, 43)),
            ("InfoWindow", (248, 249, 253)), // --surface-popover
            ("Menu", (248, 249, 253)),       // --surface-popover
            ("MenuBar", (242, 243, 250)),
            ("MenuHilight", (66, 87, 232)),
            ("MenuText", (23, 26, 43)),
            ("Scrollbar", (229, 231, 242)),   // --surface-sunken
            ("TitleText", (255, 255, 255)),   // --text-on-accent
            ("Window", (253, 253, 255)),      // --surface-raised
            ("WindowFrame", (208, 210, 226)), // --line-strong
            ("WindowText", (23, 26, 43)),
        ],
        Mode::Dark => vec![
            ("ActiveBorder", (29, 31, 46)),   // --surface-window-solid
            ("ActiveTitle", (112, 136, 255)), // --accent
            ("AppWorkSpace", (20, 22, 35)),   // --surface-sunken-strong
            ("Background", (7, 10, 24)),
            ("ButtonAlternateFace", (44, 47, 68)), // --surface-raised-2
            ("ButtonDkShadow", (14, 15, 26)),      // soft pillow edge, below
            ("ButtonFace", (29, 31, 46)),          // --surface-window-solid
            ("ButtonHilight", (56, 59, 75)),       // --line-strong (top light)
            ("ButtonLight", (44, 47, 63)),         // --line
            ("ButtonShadow", (20, 22, 35)),        // --surface-sunken-strong
            ("ButtonText", (238, 240, 250)),       // --text-primary
            ("GradientActiveTitle", (112, 136, 255)),
            ("GradientInactiveTitle", (20, 22, 35)),
            ("GrayText", (101, 107, 132)),         // --text-disabled
            ("Hilight", (112, 136, 255)),          // --accent
            ("HilightText", (14, 18, 48)),         // --text-on-accent
            ("HotTrackingColor", (154, 171, 255)), // --accent-text
            ("InactiveBorder", (29, 31, 46)),
            ("InactiveTitle", (20, 22, 35)), // --surface-sunken-strong
            ("InactiveTitleText", (188, 193, 214)), // --text-secondary
            ("InfoText", (238, 240, 250)),
            ("InfoWindow", (36, 38, 56)), // --surface-popover
            ("Menu", (36, 38, 56)),       // --surface-popover
            ("MenuBar", (29, 31, 46)),
            ("MenuHilight", (112, 136, 255)),
            ("MenuText", (238, 240, 250)),
            ("Scrollbar", (23, 25, 39)),      // --surface-sunken
            ("TitleText", (14, 18, 48)),      // --text-on-accent
            ("Window", (44, 47, 68)),         // --surface-raised-2 (content cards)
            ("WindowFrame", (102, 106, 125)), // --line-control
            ("WindowText", (238, 240, 250)),
        ],
    }
}

const COLORS_KEY: &str = "Control Panel\\Colors";
const DESKTOP_KEY: &str = "Control Panel\\Desktop";
const METRICS_KEY: &str = "Control Panel\\Desktop\\WindowMetrics";
const PERSONALIZE_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize";
const THEME_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\ThemeManager";
const SUBST_KEY: &str = "Software\\Microsoft\\Windows NT\\CurrentVersion\\FontSubstitutes";
const LINK_KEY: &str = "Software\\Microsoft\\Windows NT\\CurrentVersion\\FontLink\\SystemLink";

/// A LOGFONTW as Wine stores it in `WindowMetrics` (height in 96-dpi
/// pixels; Wine scales it to the DPI in use).
pub fn logfont(face: &str, height_px: i32, weight: i32) -> Vec<u8> {
    let mut b = Vec::with_capacity(92);
    for v in [-height_px, 0, 0, 0, weight] {
        b.extend_from_slice(&v.to_le_bytes());
    }
    // italic, underline, strikeout, charset (DEFAULT_CHARSET), out precision,
    // clip precision, quality (DEFAULT_QUALITY: follows FontSmoothing),
    // pitch and family
    b.extend_from_slice(&[0, 0, 0, 1, 0, 0, 0, 0]);
    let mut name: Vec<u16> = face.encode_utf16().take(31).collect();
    name.resize(32, 0);
    for u in name {
        b.extend_from_slice(&u.to_le_bytes());
    }
    b
}

/// Registry values to write, key by key.
pub type KeyValues = Vec<(&'static str, Vec<(String, Value)>)>;

fn named(values: Vec<(&str, Value)>) -> Vec<(String, Value)> {
    values
        .into_iter()
        .map(|(n, v)| (n.to_string(), v))
        .collect()
}

/// Per-user values (`user.reg`), key by key.
pub fn user_values(s: &Settings) -> KeyValues {
    let colors = palette(s.mode)
        .into_iter()
        .map(|(n, (r, g, b))| (n.to_string(), Value::Str(format!("{r} {g} {b}"))))
        .collect();
    let (smooth, kind, orientation) = match s.smoothing {
        Smoothing::Off => ("0", 1, 1),
        Smoothing::Grayscale => ("2", 1, 1),
        Smoothing::Rgb => ("2", 2, 1),
        Smoothing::Bgr => ("2", 2, 0),
    };
    let light = u32::from(s.mode == Mode::Light);
    let mut out = vec![
        (COLORS_KEY, colors),
        (
            DESKTOP_KEY,
            named(vec![
                ("FontSmoothing", Value::Str(smooth.into())),
                ("FontSmoothingType", Value::Dword(kind)),
                ("FontSmoothingOrientation", Value::Dword(orientation)),
                ("FontSmoothingGamma", Value::Dword(text_gamma(s.mode))),
                ("LogPixels", Value::Dword(s.dpi)),
            ]),
        ),
        (
            PERSONALIZE_KEY,
            named(vec![
                ("AppsUseLightTheme", Value::Dword(light)),
                ("SystemUsesLightTheme", Value::Dword(light)),
            ]),
        ),
        (
            THEME_KEY,
            named(if light == 1 {
                vec![
                    ("ThemeActive", Value::Str("1".into())),
                    (
                        "DllName",
                        Value::Str("C:\\windows\\resources\\themes\\aero\\aero.msstyles".into()),
                    ),
                    ("ColorName", Value::Str("Blue".into())),
                    ("SizeName", Value::Str("NormalSize".into())),
                ]
            } else {
                vec![("ThemeActive", Value::Str("0".into()))]
            }),
        ),
    ];
    if let Some(face) = &s.font {
        // Windows 10/11 use 9 pt (12 px at 96 dpi) for these. Body text is
        // weight 500 in the design (--type-body-weight): the Medium face
        // when it is installed as its own family name, as Fedora's static
        // Inter is; Wine would otherwise round 500 to the Regular face.
        let (body, body_weight) = match &s.font_medium {
            Some(m) => (m.as_str(), 400),
            None => (face.as_str(), 500),
        };
        out.push((
            METRICS_KEY,
            named(vec![
                ("CaptionFont", Value::Binary(logfont(face, 12, 600))),
                ("SmCaptionFont", Value::Binary(logfont(face, 12, 600))),
                ("MenuFont", Value::Binary(logfont(body, 12, body_weight))),
                ("StatusFont", Value::Binary(logfont(body, 12, body_weight))),
                ("MessageFont", Value::Binary(logfont(body, 12, body_weight))),
                ("IconFont", Value::Binary(logfont(body, 12, body_weight))),
            ]),
        ));
    }
    out
}

/// Wine's `FontSmoothingGamma` (1000-2200; 1400 = blend as is). Seen in the
/// E2E: Inter's thin stems at 12 px came out grey on light backgrounds.
/// Wine blends ClearType text through this gamma (win32u/dibdrv, `blend_color_gamma`), so a
/// low value darkens dark-on-light edges and a high one brightens
/// light-on-dark ones: the same text weight in both modes, like the
/// stem darkening other platforms apply.
pub fn text_gamma(mode: Mode) -> u32 {
    match mode {
        Mode::Light => 1000,
        Mode::Dark => 1700,
    }
}

/// Machine-wide values (`system.reg`) for the UI font.
///
/// "MS Shell Dlg" / "MS Shell Dlg 2" (the face nearly every dialog asks
/// for) and "Segoe UI" (newer programs) map to the UI font. Two things need
/// care here, both seen in Wine's font code (win32u/font.c):
/// - a substitute only applies to names that are *not* installed fonts, so
///   "Tahoma" itself can't be redirected, and the UI font must exist (the
///   caller checks) or dialogs would fall back to an arbitrary face;
/// - Wine builds its fallback links for "MS Shell Dlg" only when it maps to
///   Tahoma (or another face it knows). The UI font therefore gets an
///   explicit link to Tahoma, so characters it lacks (Inter has Latin,
///   Cyrillic and Greek) still render instead of turning into boxes.
///
/// Dialog layouts are measured in dialog units derived from the font's
/// average character width and height, so a dialog grows with a wider font
/// instead of clipping it; Inter's line height (1.21 em) is Tahoma's.
pub fn system_values(s: &Settings) -> KeyValues {
    let Some(face) = &s.font else {
        return Vec::new();
    };
    vec![
        (
            SUBST_KEY,
            named(vec![
                ("MS Shell Dlg", Value::Str(face.clone())),
                ("MS Shell Dlg 2", Value::Str(face.clone())),
                ("Segoe UI", Value::Str(face.clone())),
            ]),
        ),
        (
            LINK_KEY,
            vec![(
                face.clone(),
                Value::MultiStr(vec!["tahoma.ttf,Tahoma".into()]),
            )],
        ),
    ]
}

/// Apply one file's values; returns the new text (equal when unchanged).
pub fn patch(text: &str, values: &KeyValues, now: u64) -> String {
    let mut out = text.to_string();
    for (key, vals) in values {
        let vals: Vec<(&str, Value)> = vals.iter().map(|(n, v)| (n.as_str(), v.clone())).collect();
        out = regfile::set_values(&out, key, &vals, now);
    }
    out
}

/// Where Wine's server for `prefix` listens (`/tmp/.wine-UID/server-DEV-INO`).
pub fn server_socket(prefix: &Path) -> Option<PathBuf> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let m = std::fs::metadata(prefix).ok()?;
        // SAFETY: getuid has no failure mode.
        let uid = unsafe { libc::getuid() };
        Some(PathBuf::from(format!(
            "/tmp/.wine-{uid}/server-{:x}-{:x}/socket",
            m.dev(),
            m.ino()
        )))
    }
    #[cfg(not(unix))]
    {
        let _ = prefix;
        None
    }
}

/// Whether a wineserver has this prefix open (its registry is then in the
/// server's memory, and editing the files would be lost).
pub fn server_running(prefix: &Path) -> bool {
    #[cfg(unix)]
    {
        server_socket(prefix).is_some_and(|s| std::os::unix::net::UnixStream::connect(s).is_ok())
    }
    #[cfg(not(unix))]
    {
        let _ = prefix;
        false
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Applied,
    Unchanged,
    /// A program of this prefix is running; the next launch applies it.
    Busy,
    NoPrefix,
}

/// Write the settings into a prefix (best effort, atomic per file).
pub fn apply(prefix: &Path, s: &Settings, now: u64) -> std::io::Result<Outcome> {
    let user = prefix.join("user.reg");
    let system = prefix.join("system.reg");
    if !user.is_file() || !system.is_file() {
        return Ok(Outcome::NoPrefix);
    }
    if server_running(prefix) {
        return Ok(Outcome::Busy);
    }
    let mut changed = false;
    for (path, values) in [(user, user_values(s)), (system, system_values(s))] {
        if values.is_empty() {
            continue;
        }
        let text = std::fs::read_to_string(&path)?;
        let new = patch(&text, &values, now);
        if new != text {
            let tmp = path.with_extension("reg.lucent-tmp");
            std::fs::write(&tmp, &new)?;
            std::fs::rename(&tmp, &path)?;
            changed = true;
        }
    }
    Ok(if changed {
        Outcome::Applied
    } else {
        Outcome::Unchanged
    })
}

/// Read the desktop's settings from this machine.
pub fn read_desktop(env: &dyn Fn(&str) -> Option<String>) -> Desktop {
    let home = env("HOME").map(PathBuf::from);
    let config_home = env("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| home.as_ref().map(|h| h.join(".config")));
    let config_dirs: Vec<PathBuf> = env("XDG_CONFIG_DIRS")
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "/etc/xdg".into())
        .split(':')
        .map(PathBuf::from)
        .collect();
    let read_all = |name: &str| -> Vec<String> {
        config_home
            .iter()
            .chain(config_dirs.iter())
            .filter_map(|d| std::fs::read_to_string(d.join(name)).ok())
            .collect()
    };
    let xresources = env("DISPLAY").filter(|d| !d.is_empty()).and_then(|_| {
        let out = std::process::Command::new("xrdb")
            .arg("-query")
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
            .ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
    });
    Desktop {
        kdeglobals: read_all("kdeglobals"),
        kwinrc: read_all("kwinrc"),
        xresources,
        has_font: Some(font_installed),
    }
}

/// Whether fontconfig knows the family (`fc-list` prints its files).
fn font_installed(family: &str) -> bool {
    std::process::Command::new("fc-list")
        .arg(format!("{family}:style=Regular"))
        .arg("family")
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .is_ok_and(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .any(|l| l.split(',').any(|f| f.trim().eq_ignore_ascii_case(family)))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIGHT: &str = "[General]\nColorScheme=LucentLight\nfont=Inter,11,-1,5,400,0,0,0,0,0,0,0,0,0,0,1\n\n[Colors:Window]\nBackgroundNormal=242,243,250\n";
    const DARK_USER: &str =
        "[Colors:Window]\nBackgroundNormal=29,31,46\n\n[General]\nXftSubPixel=rgb\n";

    #[test]
    fn mode_follows_the_window_background_user_file_first() {
        assert_eq!(detect_mode(&[LIGHT.into()]), Mode::Light);
        assert_eq!(detect_mode(&[DARK_USER.into(), LIGHT.into()]), Mode::Dark);
        // No colours, only a scheme name.
        assert_eq!(
            detect_mode(&["[General]\nColorScheme=LucentDark\n".into()]),
            Mode::Dark
        );
        assert_eq!(detect_mode(&[]), Mode::Light);
    }

    #[test]
    fn dpi_from_xft_then_kwin_scale() {
        assert_eq!(
            detect_dpi(Some("Xft.antialias:\t1\nXft.dpi:\t144\n"), &[]),
            144
        );
        let kwin = vec!["[Xwayland]\nScale=1.25\n".to_string()];
        assert_eq!(detect_dpi(None, &kwin), 120);
        assert_eq!(detect_dpi(Some("Xcursor.size: 24\n"), &kwin), 120);
        assert_eq!(detect_dpi(None, &[]), 96);
        assert_eq!(detect_dpi(Some("Xft.dpi: 1000\n"), &[]), 480);
    }

    #[test]
    fn smoothing_matches_plasma() {
        // Not set: Windows' own default, ClearType RGB.
        assert_eq!(detect_smoothing(&[LIGHT.into()]), Smoothing::Rgb);
        assert_eq!(detect_smoothing(&[DARK_USER.into()]), Smoothing::Rgb);
        assert_eq!(
            detect_smoothing(&["[General]\nXftSubPixel=none\n".into()]),
            Smoothing::Grayscale
        );
        assert_eq!(
            detect_smoothing(&["[General]\nXftSubPixel=bgr\n".into()]),
            Smoothing::Bgr
        );
        assert_eq!(
            detect_smoothing(&["[General]\nXftAntialias=false\n".into()]),
            Smoothing::Off
        );
        assert_eq!(
            detect_smoothing(&["[General]\nXftSubPixel=vbgr\n".into()]),
            Smoothing::Grayscale
        );
    }

    #[test]
    fn detect_honours_overrides_and_installed_fonts() {
        let desktop = Desktop {
            kdeglobals: vec![LIGHT.into()],
            kwinrc: vec![],
            xresources: Some("Xft.dpi: 120\n".into()),
            has_font: Some(|f| f == "Inter" || f == "Inter Medium"),
        };
        let none = |_: &str| None;
        let s = detect(&none, &desktop).unwrap();
        assert_eq!(
            s,
            Settings {
                mode: Mode::Light,
                dpi: 120,
                dpi_from: "Xft.dpi",
                smoothing: Smoothing::Rgb,
                font: Some("Inter".into()),
                font_medium: Some("Inter Medium".into()),
            }
        );
        let only_regular = Desktop {
            has_font: Some(|f| f == "Inter"),
            ..desktop.clone()
        };
        assert_eq!(detect(&none, &only_regular).unwrap().font_medium, None);
        let dark = |k: &str| match k {
            ENV_MODE => Some("dark".to_string()),
            ENV_DPI => Some("144".to_string()),
            _ => None,
        };
        let s = detect(&dark, &desktop).unwrap();
        assert_eq!((s.mode, s.dpi), (Mode::Dark, 144));
        let off = |k: &str| (k == ENV_MODE).then(|| "off".to_string());
        assert_eq!(detect(&off, &desktop), None);
        // A font that isn't installed is never substituted in.
        let missing = Desktop {
            has_font: Some(|_| false),
            ..desktop
        };
        assert_eq!(detect(&none, &missing).unwrap().font, None);
    }

    #[test]
    fn palettes_cover_every_windows_colour_name_once() {
        for mode in [Mode::Light, Mode::Dark] {
            let p = palette(mode);
            assert_eq!(p.len(), 31);
            let mut names: Vec<_> = p.iter().map(|(n, _)| *n).collect();
            names.sort();
            names.dedup();
            assert_eq!(names.len(), 31);
        }
        // Text on the window and on the selection stays readable (WCAG AA).
        for mode in [Mode::Light, Mode::Dark] {
            let get = |n: &str| palette(mode).into_iter().find(|(k, _)| *k == n).unwrap().1;
            assert!(contrast(get("WindowText"), get("Window")) >= 7.0);
            assert!(contrast(get("ButtonText"), get("ButtonFace")) >= 7.0);
            assert!(contrast(get("HilightText"), get("Hilight")) >= 4.5);
            assert!(contrast(get("MenuText"), get("Menu")) >= 7.0);
            assert!(contrast(get("TitleText"), get("ActiveTitle")) >= 4.5);
            assert!(contrast(get("InactiveTitleText"), get("InactiveTitle")) >= 4.5);
            // Programs draw text in the caption colour on window surfaces
            // (Notepad++ active tab): it must read there too.
            assert!(contrast(get("ActiveTitle"), get("ButtonFace")) >= 3.0);
        }
    }

    fn contrast(a: Rgb, b: Rgb) -> f64 {
        let lum = |(r, g, b): Rgb| {
            let ch = |c: u8| {
                let c = c as f64 / 255.0;
                if c <= 0.03928 {
                    c / 12.92
                } else {
                    ((c + 0.055) / 1.055).powf(2.4)
                }
            };
            0.2126 * ch(r) + 0.7152 * ch(g) + 0.0722 * ch(b)
        };
        let (x, y) = (lum(a), lum(b));
        (x.max(y) + 0.05) / (x.min(y) + 0.05)
    }

    #[test]
    fn logfont_is_wines_layout() {
        let f = logfont("Inter", 12, 600);
        assert_eq!(f.len(), 92);
        assert_eq!(&f[0..4], &(-12i32).to_le_bytes());
        assert_eq!(&f[16..20], &600i32.to_le_bytes());
        assert_eq!(f[23], 1); // DEFAULT_CHARSET
        assert_eq!(&f[28..38], &[b'I', 0, b'n', 0, b't', 0, b'e', 0, b'r', 0]);
        assert_eq!(&f[38..40], &[0, 0]);
    }

    fn settings(mode: Mode) -> Settings {
        Settings {
            mode,
            dpi: 144,
            dpi_from: "test",
            smoothing: Smoothing::Rgb,
            font: Some("Inter".into()),
            font_medium: Some("Inter Medium".into()),
        }
    }

    #[test]
    fn body_fonts_use_the_medium_face_and_text_gamma_follows_the_mode() {
        let metrics = |s: &Settings| {
            let text = patch("WINE REGISTRY Version 2\n", &user_values(s), 1);
            regfile::get_raw(&text, METRICS_KEY, "MenuFont").unwrap()
        };
        let hex = |f: Vec<u8>| Value::Binary(f).render();
        assert_eq!(
            metrics(&settings(Mode::Light)),
            hex(logfont("Inter Medium", 12, 400))
        );
        let no_medium = Settings {
            font_medium: None,
            ..settings(Mode::Light)
        };
        assert_eq!(metrics(&no_medium), hex(logfont("Inter", 12, 500)));
        let light = patch("", &user_values(&settings(Mode::Light)), 1);
        let dark = patch("", &user_values(&settings(Mode::Dark)), 1);
        assert_eq!(
            regfile::get_raw(&light, DESKTOP_KEY, "FontSmoothingGamma").as_deref(),
            Some("dword:000003e8")
        );
        assert_eq!(
            regfile::get_raw(&dark, DESKTOP_KEY, "FontSmoothingGamma").as_deref(),
            Some("dword:000006a4")
        );
    }

    #[test]
    fn user_reg_values_light_and_dark() {
        let user =
            "WINE REGISTRY Version 2\n\n[Control Panel\\\\Colors] 1\n\"Window\"=\"255 255 255\"\n";
        let light = patch(user, &user_values(&settings(Mode::Light)), 1);
        assert!(light.contains("\"Window\"=\"253 253 255\""), "{light}");
        assert!(light.contains("\"Hilight\"=\"66 87 232\""));
        assert_eq!(
            regfile::get_raw(&light, DESKTOP_KEY, "LogPixels").as_deref(),
            Some("dword:00000090")
        );
        assert_eq!(
            regfile::get_raw(&light, DESKTOP_KEY, "FontSmoothingType").as_deref(),
            Some("dword:00000002")
        );
        assert_eq!(
            regfile::get_raw(&light, THEME_KEY, "ThemeActive").as_deref(),
            Some("\"1\"")
        );
        assert_eq!(
            regfile::get_raw(&light, PERSONALIZE_KEY, "AppsUseLightTheme").as_deref(),
            Some("dword:00000001")
        );
        assert!(regfile::get_raw(&light, METRICS_KEY, "MenuFont").is_some());

        let dark = patch(&light, &user_values(&settings(Mode::Dark)), 2);
        assert!(dark.contains("\"Window\"=\"44 47 68\""), "{dark}");
        assert_eq!(
            regfile::get_raw(&dark, THEME_KEY, "ThemeActive").as_deref(),
            Some("\"0\"")
        );
        assert_eq!(
            regfile::get_raw(&dark, PERSONALIZE_KEY, "AppsUseLightTheme").as_deref(),
            Some("dword:00000000")
        );
        // Back to light: the same text as the first time.
        assert_eq!(patch(&dark, &user_values(&settings(Mode::Light)), 3), light);
    }

    #[test]
    fn system_reg_maps_dialog_fonts_and_links_a_fallback() {
        let sys = "WINE REGISTRY Version 2\n\n[Software\\\\Microsoft\\\\Windows NT\\\\CurrentVersion\\\\FontSubstitutes] 1\n\"Arial\"=\"Liberation Sans\"\n\"MS Shell Dlg\"=\"Tahoma\"\n";
        let out = patch(sys, &system_values(&settings(Mode::Light)), 1);
        assert_eq!(
            regfile::get_raw(&out, SUBST_KEY, "MS Shell Dlg").as_deref(),
            Some("\"Inter\"")
        );
        assert_eq!(
            regfile::get_raw(&out, SUBST_KEY, "MS Shell Dlg 2").as_deref(),
            Some("\"Inter\"")
        );
        assert_eq!(
            regfile::get_raw(&out, SUBST_KEY, "Arial").as_deref(),
            Some("\"Liberation Sans\"")
        );
        assert_eq!(
            regfile::get_raw(&out, LINK_KEY, "Inter").as_deref(),
            Some("str(7):\"tahoma.ttf,Tahoma\\0\"")
        );
        // Without a UI font nothing machine-wide changes.
        let s = Settings {
            font: None,
            ..settings(Mode::Light)
        };
        assert!(system_values(&s).is_empty());
        assert!(!user_values(&s).iter().any(|(k, _)| *k == METRICS_KEY));
    }

    #[cfg(unix)]
    #[test]
    fn apply_writes_only_without_a_running_server() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path();
        assert_eq!(
            apply(p, &settings(Mode::Dark), 1).unwrap(),
            Outcome::NoPrefix
        );
        std::fs::write(p.join("user.reg"), "WINE REGISTRY Version 2\n").unwrap();
        std::fs::write(p.join("system.reg"), "WINE REGISTRY Version 2\n").unwrap();
        assert_eq!(
            apply(p, &settings(Mode::Dark), 1).unwrap(),
            Outcome::Applied
        );
        assert_eq!(
            apply(p, &settings(Mode::Dark), 2).unwrap(),
            Outcome::Unchanged
        );
        let user = std::fs::read_to_string(p.join("user.reg")).unwrap();
        assert!(user.contains("\"AppsUseLightTheme\"=dword:00000000"));
        // A server listening on the prefix's socket: hands off.
        let sock = server_socket(p).unwrap();
        std::fs::create_dir_all(sock.parent().unwrap()).unwrap();
        let _ = std::fs::remove_file(&sock);
        let listener = std::os::unix::net::UnixListener::bind(&sock).unwrap();
        assert_eq!(apply(p, &settings(Mode::Light), 3).unwrap(), Outcome::Busy);
        drop(listener);
        let _ = std::fs::remove_file(&sock);
        let _ = std::fs::remove_dir(sock.parent().unwrap());
    }
}
