//! Pyxis's visual identity, as far as Evo draws it.
//!
//! The waterdrop frame and the remochi spinner's three colours. Bloom keeps a
//! copy of these in its design tokens; lane T's contract test in
//! pyxis-experience reads this file by name, so keep the names and shapes.
//!
//! Also the apps' identities, which Bloom sends (`org.geckoOS.evo
//! SetAppIdentity {apps: [{app_id, name, accent, icon}], dark}`): the title
//! bar shows the app's icon and name, looked up by the window's *trusted* app
//! id (the security context's, never the client's claim), then the client's
//! own title, quieter, only when it says something the name doesn't. So an
//! app can no longer caption itself "Files" in the bar's primary label. The
//! bar is frosted glass (`shell/frost.rs`): the app's accent is its seam, not
//! its fill, and what the glass is veiled with is decided here, per theme.

use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
};

use serde_json::Value;

/// Frame corner radii in px: top-left, top-right, bottom-right, bottom-left.
/// The waterdrop: three round corners and one nearly square, the top right.
pub const CORNER_RADII: [f64; 4] = [16.0, 4.0, 16.0, 16.0];

/// The spinner's circle (close).
pub const SPINNER_RED: [u8; 3] = [0xff, 0x4a, 0x4a];
pub const SPINNER_RED_OUTLINE: [u8; 3] = [0xa1, 0x2c, 0x2c];
/// The spinner's square (maximize).
pub const SPINNER_BLUE: [u8; 3] = [0x69, 0xcd, 0xff];
pub const SPINNER_BLUE_OUTLINE: [u8; 3] = [0x33, 0x64, 0x7c];
/// The spinner's downward triangle (minimize).
pub const SPINNER_YELLOW: [u8; 3] = [0xff, 0xb8, 0x0d];
pub const SPINNER_YELLOW_OUTLINE: [u8; 3] = [0x5d, 0x43, 0x05];

/// Four corner radii, px, in `CORNER_RADII`'s order: top-left, top-right,
/// bottom-right, bottom-left. What every rounded thing Evo draws around a
/// window takes (the title bar, its border, the shadow, Bloom's clip, the
/// glint, Lumen's pictures), so the waterdrop is one shape everywhere.
///
/// The shaders take it as a `vec4` in the same order and pick a corner by
/// quadrant, with y pointing down.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Radii(pub [f64; 4]);

impl Radii {
    pub const SQUARE: Radii = Radii([0.0; 4]);
    /// An undecorated window's frame and the waterdrop at rest.
    pub const FRAME: Radii = Radii(CORNER_RADII);

    pub const fn uniform(r: f64) -> Self {
        Radii([r; 4])
    }

    /// A frame's corners: the waterdrop, or square when the window fills its
    /// space (maximized or fullscreen).
    pub fn frame(square: bool) -> Self {
        if square {
            Self::SQUARE
        } else {
            Self::FRAME
        }
    }

    pub fn top_left(self) -> f64 {
        self.0[0]
    }
    pub fn top_right(self) -> f64 {
        self.0[1]
    }
    pub fn bottom_right(self) -> f64 {
        self.0[2]
    }
    pub fn bottom_left(self) -> f64 {
        self.0[3]
    }

    pub fn max(self) -> f64 {
        self.0.iter().copied().fold(0.0, f64::max)
    }

    pub fn is_square(self) -> bool {
        self.0.iter().all(|r| *r <= 0.0)
    }

    pub fn scale(self, k: f64) -> Self {
        Radii(self.0.map(|r| r * k))
    }

    /// Each corner `t` of the way to `other`'s.
    pub fn mix(self, other: Radii, t: f64) -> Self {
        let mut out = self.0;
        for (o, b) in out.iter_mut().zip(other.0) {
            *o += (b - *o) * t;
        }
        Radii(out)
    }

    /// Each corner clamped to `0 ..= limit`.
    pub fn clamp_to(self, limit: f64) -> Self {
        let limit = limit.max(0.0);
        Radii(self.0.map(|r| r.clamp(0.0, limit)))
    }

    /// What a `w`×`h` rectangle can draw: no corner past half its short side
    /// (a circle, or a pill).
    pub fn fit(self, w: f64, h: f64) -> Self {
        self.clamp_to(w.min(h) / 2.0)
    }

    pub fn as_f32(self) -> (f32, f32, f32, f32) {
        (self.0[0] as f32, self.0[1] as f32, self.0[2] as f32, self.0[3] as f32)
    }
}

/// The GLSL every rounded-rectangle shader shares: the corner radius for a
/// point `d` from the centre (y down), and the signed distance to the
/// rectangle with per-corner radii (Inigo Quilez's rounded box, by quadrant),
/// positive outside. `radii` in `Radii`'s order.
#[macro_export]
macro_rules! radii_glsl {
    () => {
        r#"
float corner_of(vec2 d, vec4 radii) {
    return d.x < 0.0 ? (d.y < 0.0 ? radii.x : radii.w) : (d.y < 0.0 ? radii.y : radii.z);
}
float rbox4(vec2 d, vec2 half_size, vec4 radii) {
    float r = corner_of(d, radii);
    vec2 q = abs(d) - half_size + r;
    return min(max(q.x, q.y), 0.0) + length(max(q, 0.0)) - r;
}
"#
    };
}


// === App identities (SetAppIdentity) ===

/// The size an icon is drawn at in the title bar, logical px. Bloom sends
/// about twice that; it is scaled down once, when the table arrives.
pub const ICON_PX: u32 = 20;
/// No more apps than this in one table; the rest are ignored.
const MAX_APPS: usize = 256;
/// The longest name kept, in characters.
const MAX_NAME: usize = 64;
/// The largest icon data URL accepted, in bytes (a 40x40 PNG is a few KB).
const MAX_ICON_URL: usize = 256 * 1024;

/// An icon, straight-alpha RGBA, `ICON_PX` square.
#[derive(Clone, Debug, PartialEq)]
pub struct Icon {
    pub size: u32,
    pub rgba: Vec<u8>,
}

/// One app as the title bar knows it.
#[derive(Clone, Debug, PartialEq)]
pub struct AppIdentity {
    pub name: String,
    pub accent: Option<[u8; 3]>,
    pub icon: Option<Arc<Icon>>,
}

#[derive(Default)]
struct Table {
    apps: HashMap<String, AppIdentity>,
    dark: bool,
    material: Material,
}

/// Bloom's material for each theme (`SetAppIdentity`'s optional `material`):
/// the colour of the flat title bar when Evo has nothing to frost (F0).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Material {
    pub light: Option<[u8; 3]>,
    pub dark: Option<[u8; 3]>,
}

static TABLE: RwLock<Option<Table>> = RwLock::new(None);

/// What `SetAppIdentity` carried, parsed.
#[derive(Debug, Default, PartialEq)]
pub struct Update {
    pub apps: HashMap<String, AppIdentity>,
    pub dark: bool,
    pub material: Material,
}

/// Parse `SetAppIdentity`'s body. An entry needs an `app_id` and a `name`;
/// a bad `accent` or `icon` is dropped from it and never costs the entry. Only
/// a body with no `apps` array is an error. The same goes for `material`: a
/// colour that isn't `#rrggbb` is ignored, and that theme keeps Evo's own.
pub fn parse(body: &Value) -> Result<Update, String> {
    let entries = body
        .get("apps")
        .and_then(Value::as_array)
        .ok_or_else(|| "apps must be an array of {app_id, name, accent, icon}".to_string())?;
    let mut apps = HashMap::new();
    for entry in entries.iter().take(MAX_APPS) {
        let Some(id) = entry.get("app_id").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()) else {
            continue;
        };
        let Some(name) = entry.get("name").and_then(Value::as_str).map(clean_name).filter(|s| !s.is_empty()) else {
            continue;
        };
        let accent = entry.get("accent").and_then(Value::as_str).and_then(parse_hex);
        let icon = entry.get("icon").and_then(Value::as_str).and_then(decode_icon).map(Arc::new);
        apps.insert(id.to_string(), AppIdentity { name, accent, icon });
    }
    let material = body.get("material").map_or_else(Material::default, |m| {
        let colour = |key: &str| m.get(key).and_then(Value::as_str).and_then(parse_hex);
        Material { light: colour("light"), dark: colour("dark") }
    });
    Ok(Update { apps, dark: body.get("dark").and_then(Value::as_bool).unwrap_or(false), material })
}

/// A name as the bar draws it: one line, no control characters, bounded.
fn clean_name(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).take(MAX_NAME).collect::<String>().trim().to_string()
}

/// `#rrggbb` (the `#` optional).
pub fn parse_hex(s: &str) -> Option<[u8; 3]> {
    let h = s.trim().strip_prefix('#').unwrap_or(s.trim());
    if h.len() != 6 || !h.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let byte = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).ok();
    Some([byte(0)?, byte(2)?, byte(4)?])
}

/// Replace the table (the whole of it, as Bloom sends it).
pub fn set(update: Update) {
    let Ok(mut guard) = TABLE.write() else { return };
    *guard = Some(Table { apps: update.apps, dark: update.dark, material: update.material });
}

/// `app_id`'s identity, if Bloom sent one.
pub fn lookup(app_id: &str) -> Option<AppIdentity> {
    TABLE.read().ok()?.as_ref()?.apps.get(app_id).cloned()
}

/// The shell's theme, as Bloom last said.
pub fn dark() -> bool {
    TABLE.read().ok().and_then(|t| t.as_ref().map(|t| t.dark)).unwrap_or(false)
}

/// The flat title bar's base colour for a theme: Bloom's material when it sent
/// one, else Evo's own.
pub fn material(dark: bool) -> [u8; 3] {
    let sent = TABLE.read().ok().and_then(|t| t.as_ref().map(|t| t.material));
    let (sent, default) = if dark { (sent.and_then(|m| m.dark), MATERIAL_DARK) } else { (sent.and_then(|m| m.light), MATERIAL_LIGHT) };
    sent.unwrap_or(default)
}

/// Decode a `data:image/png;base64,` icon and scale it to `ICON_PX`. `None`
/// for anything else: a bad icon is ignored.
pub fn decode_icon(url: &str) -> Option<Icon> {
    if url.len() > MAX_ICON_URL {
        return None;
    }
    let (head, data) = url.split_once(',')?;
    let head = head.to_ascii_lowercase();
    if !head.starts_with("data:image/png") || !head.ends_with(";base64") {
        return None;
    }
    let bytes = base64_decode(data)?;
    decode_png(&bytes)
}

#[cfg(feature = "image")]
fn decode_png(bytes: &[u8]) -> Option<Icon> {
    let img = image::load_from_memory_with_format(bytes, image::ImageFormat::Png).ok()?;
    if img.width() == 0 || img.height() == 0 || img.width() > 512 || img.height() > 512 {
        return None;
    }
    let img = img.resize_exact(ICON_PX, ICON_PX, image::imageops::FilterType::Triangle).into_rgba8();
    Some(Icon { size: ICON_PX, rgba: img.into_raw() })
}

#[cfg(not(feature = "image"))]
fn decode_png(_: &[u8]) -> Option<Icon> {
    None
}

/// Standard base64, padding optional, whitespace ignored. `None` on any other
/// character.
pub fn base64_decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0u32);
    for c in s.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => break,
            b' ' | b'\n' | b'\r' | b'\t' => continue,
            _ => return None,
        };
        acc = (acc << 6) | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

// === What the title bar says ===

/// The title bar's text: the app's name and the document's own title, or,
/// with no identity, the client's title as it is.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TitleLabel {
    /// The app's name (bold), when Bloom gave the app an identity.
    pub name: Option<String>,
    /// The client's title, template stripped (quiet), when it adds anything.
    pub doc: Option<String>,
    /// With no name: the client's own title, drawn as it always was.
    pub plain: String,
}

impl TitleLabel {
    /// One line of text for `ListWindows`' `title_label`: what the bar shows.
    pub fn text(&self) -> String {
        match (&self.name, &self.doc) {
            (Some(name), Some(doc)) => format!("{name} · {doc}"),
            (Some(name), None) => name.clone(),
            _ => self.plain.clone(),
        }
    }
}

/// vela's title template taken off: a `pyxis://<id> - ` prefix and a
/// ` — <brand>` suffix (`pyxis://notes - Groceries — Pyxis Vela` → `Groceries`,
/// or ` — Vela` / legacy ` — Nightly`).
pub fn strip_template(title: &str) -> &str {
    let mut t = title.trim();
    if let Some(rest) = t.strip_prefix("pyxis://") {
        if let Some(i) = rest.find(" - ") {
            if !rest[..i].contains(char::is_whitespace) {
                t = rest[i + 3..].trim_start();
            }
        }
    }
    if let Some(i) = t.rfind(" \u{2014} ") {
        t = t[..i].trim_end();
    }
    t
}

/// What the bar shows for a window whose app is `name` (its identity, `None`
/// for none or for an X11 window) and whose client says `title`.
pub fn title_label(name: Option<&str>, title: &str) -> TitleLabel {
    let Some(name) = name else {
        return TitleLabel { name: None, doc: None, plain: title.trim().to_string() };
    };
    let doc = strip_template(title);
    // A page with no title of its own shows its address: nothing to add.
    let adds = !doc.is_empty() && !doc.eq_ignore_ascii_case(name) && !doc.starts_with("pyxis://");
    TitleLabel { name: Some(name.to_string()), doc: adds.then(|| doc.to_string()), plain: String::new() }
}

// === The bar's colours ===

/// The flat bar's base when Bloom sends no material: light and dark.
const MATERIAL_LIGHT: [u8; 3] = [0xfb, 0xfa, 0xf7];
const MATERIAL_DARK: [u8; 3] = [0x25, 0x26, 0x2e];
/// How opaque the flat bar (F0) is: opaque enough to read on any wallpaper.
const MATERIAL_ALPHA: u8 = 0xe6;
/// The ink is fixed per theme: the frost's lightness is held in a band
/// (`VeilParams`), so it clears 7:1 whatever is behind the bar.
pub(crate) const INK_DARK: [u8; 3] = [0x2a, 0x26, 0x20];
pub(crate) const INK_LIGHT: [u8; 3] = [0xec, 0xe9, 0xe3];
/// The quiet ink's alpha (the document title, inactive controls).
const QUIET_ALPHA: u8 = 0x8c;

/// How a title bar's backdrop is turned into glass, for one theme: mixed
/// `mix` of the way toward `target` in OKLab, its lightness clamped into
/// `l_min ..= l_max` (what makes the fixed ink safe), its chroma multiplied by
/// `chroma_gain` and capped at `chroma_cap` (so the wallpaper's hue carries
/// but never shouts). The maths is `frost::veil`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VeilParams {
    pub target: [u8; 3],
    pub mix: f64,
    pub l_min: f64,
    pub l_max: f64,
    pub chroma_gain: f64,
    pub chroma_cap: f64,
}

/// Light theme: toward `#fcfbf8`, OKLab L at least 0.86.
pub const VEIL_LIGHT: VeilParams =
    VeilParams { target: [0xfc, 0xfb, 0xf8], mix: 0.52, l_min: 0.86, l_max: 1.0, chroma_gain: 1.25, chroma_cap: 0.06 };
/// Dark theme: toward `#101228`, OKLab L at most 0.30.
pub const VEIL_DARK: VeilParams =
    VeilParams { target: [0x10, 0x12, 0x28], mix: 0.50, l_min: 0.0, l_max: 0.30, chroma_gain: 1.25, chroma_cap: 0.07 };
/// An inactive window's glass: the veil takes this much more of the way
/// toward its target, and the chroma that is left is multiplied by
/// `INACTIVE_CHROMA`. The window falls out of the light.
pub const INACTIVE_MIX: f64 = 0.20;
pub const INACTIVE_CHROMA: f64 = 0.35;

pub fn veil_params(dark: bool) -> VeilParams {
    if dark {
        VEIL_DARK
    } else {
        VEIL_LIGHT
    }
}

/// The title bar's colours, straight alpha.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BarColors {
    /// The flat bar (F0, nothing to frost): Bloom's material veiled for the
    /// theme, active and inactive.
    pub glass: [u8; 4],
    pub glass_inactive: [u8; 4],
    pub ink: [u8; 4],
    pub quiet: [u8; 4],
    /// The seam along the bar's bottom edge: the app's accent, or the ink for
    /// an app with none.
    pub accent: [u8; 3],
    pub dark: bool,
}

/// WCAG relative luminance.
#[cfg(test)]
pub fn luminance(c: [u8; 3]) -> f64 {
    let lin = |v: u8| {
        let s = v as f64 / 255.0;
        if s <= 0.03928 {
            s / 12.92
        } else {
            ((s + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * lin(c[0]) + 0.7152 * lin(c[1]) + 0.0722 * lin(c[2])
}

#[cfg(test)]
pub fn contrast(a: [u8; 3], b: [u8; 3]) -> f64 {
    let (x, y) = (luminance(a), luminance(b));
    (x.max(y) + 0.05) / (x.min(y) + 0.05)
}

/// The bar for an app with `accent` (none: a neutral seam) in the shell's
/// theme, over `material` when there is nothing to frost. The ink is the
/// theme's, always: dark on the light bar, light on the dark one.
pub fn bar_colors(accent: Option<[u8; 3]>, dark: bool, material: [u8; 3]) -> BarColors {
    let params = veil_params(dark);
    let flat = |inactive: bool| {
        let c = super::frost::veil(material, &params, inactive);
        [c[0], c[1], c[2], MATERIAL_ALPHA]
    };
    let ink = if dark { INK_LIGHT } else { INK_DARK };
    BarColors {
        glass: flat(false),
        glass_inactive: flat(true),
        ink: [ink[0], ink[1], ink[2], 0xff],
        quiet: [ink[0], ink[1], ink[2], QUIET_ALPHA],
        accent: accent.unwrap_or(ink),
        dark,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_waterdrop_has_one_sharp_corner_top_right() {
        let r = Radii::frame(false);
        assert_eq!((r.top_left(), r.top_right(), r.bottom_right(), r.bottom_left()), (16.0, 4.0, 16.0, 16.0));
        assert_eq!(r.max(), 16.0);
        assert!(Radii::frame(true).is_square());
    }

    #[test]
    fn fit_keeps_each_corner_inside_half_the_short_side() {
        assert_eq!(Radii::FRAME.fit(20.0, 100.0), Radii([10.0, 4.0, 10.0, 10.0]));
        assert_eq!(Radii::uniform(-2.0).fit(10.0, 10.0), Radii::SQUARE);
    }

    #[test]
    fn the_title_is_the_name_then_what_the_document_adds() {
        let notes = Some("Notes");
        let l = title_label(notes, "pyxis://notes - Notes \u{2014} Nightly");
        assert_eq!((l.name.as_deref(), l.doc.as_deref()), (Some("Notes"), None), "template stripped, nothing extra");
        assert_eq!(l.text(), "Notes");
        let l = title_label(notes, "pyxis://notes - Groceries \u{2014} Nightly");
        assert_eq!(l.doc.as_deref(), Some("Groceries"));
        assert_eq!(l.text(), "Notes · Groceries");
        let l = title_label(notes, "pyxis://notes - Notes \u{2014} Pyxis Vela");
        assert_eq!((l.name.as_deref(), l.doc.as_deref()), (Some("Notes"), None));
        assert_eq!(l.text(), "Notes");
        let l = title_label(notes, "pyxis://notes - Groceries \u{2014} Pyxis Vela");
        assert_eq!(l.doc.as_deref(), Some("Groceries"));
        assert_eq!(l.text(), "Notes · Groceries");
        let l = title_label(notes, "pyxis://notes - Groceries \u{2014} Vela");
        assert_eq!(l.doc.as_deref(), Some("Groceries"));
        assert_eq!(l.text(), "Notes · Groceries");
        assert_eq!(title_label(notes, "Groceries").doc.as_deref(), Some("Groceries"));
        assert_eq!(title_label(notes, "notes").doc, None, "the name again, in any case");
        assert_eq!(title_label(notes, "").doc, None);
        assert_eq!(title_label(notes, "pyxis://notes/index.html").doc, None, "a bare address adds nothing");
    }

    #[test]
    fn without_an_identity_the_title_is_the_clients_own() {
        // No entry, or an X11 window (the caller passes no name for one).
        let l = title_label(None, "pyxis://notes - Notes \u{2014} Nightly");
        assert_eq!(l.name, None);
        assert_eq!(l.text(), "pyxis://notes - Notes \u{2014} Nightly");
        // An app captioning itself as another can't reach the primary label.
        let l = title_label(Some("Weather"), "Files");
        assert_eq!(l.name.as_deref(), Some("Weather"));
        assert_eq!(l.doc.as_deref(), Some("Files"));
    }

    #[test]
    fn strip_template_leaves_other_titles_alone() {
        assert_eq!(strip_template("A - B"), "A - B");
        assert_eq!(strip_template("pyxis://my app - X"), "pyxis://my app - X");
        assert_eq!(strip_template("Report \u{2014} draft \u{2014} Nightly"), "Report \u{2014} draft");
        assert_eq!(strip_template("Report \u{2014} draft \u{2014} Pyxis Vela"), "Report \u{2014} draft");
        assert_eq!(strip_template("Report \u{2014} draft \u{2014} Vela"), "Report \u{2014} draft");
        assert_eq!(strip_template("pyxis://notes - Groceries \u{2014} Pyxis Vela"), "Groceries");
        assert_eq!(strip_template("pyxis://notes - Groceries \u{2014} Vela"), "Groceries");
    }

    const PNG_1X1_RED: &str = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg==";

    #[test]
    fn set_app_identity_parses_and_a_bad_icon_or_accent_costs_nothing_else() {
        let body = serde_json::json!({
            "apps": [
                {"app_id": "notes", "name": "Notes", "accent": "#eab308", "icon": PNG_1X1_RED},
                {"app_id": "files", "name": "Files", "accent": "orange", "icon": "data:image/png;base64,!!!"},
                {"app_id": "tasks", "name": "Tasks", "icon": "data:image/png;base64,aGVsbG8="},
                {"app_id": "", "name": "Nobody"},
                {"app_id": "noname"},
                {"app_id": "evil", "name": "Ev\u{7}il\n"}
            ],
            "dark": true
        });
        let u = parse(&body).unwrap();
        assert!(u.dark);
        assert_eq!(u.apps.len(), 4, "entries without an id or a name are skipped");
        let notes = &u.apps["notes"];
        assert_eq!(notes.accent, Some([0xea, 0xb3, 0x08]));
        #[cfg(feature = "image")]
        {
            let icon = notes.icon.as_ref().expect("a good PNG decodes");
            assert_eq!((icon.size, icon.rgba.len()), (ICON_PX, (ICON_PX * ICON_PX * 4) as usize));
            assert_eq!(&icon.rgba[..4], &[0xff, 0x00, 0x00, 0xff]);
        }
        let files = &u.apps["files"];
        assert_eq!((files.name.as_str(), files.accent, files.icon.is_none()), ("Files", None, true));
        assert!(u.apps["tasks"].icon.is_none(), "base64 that is not a PNG");
        assert_eq!(u.apps["evil"].name, "Evil");
        assert!(parse(&serde_json::json!({"dark": true})).is_err());
        assert!(!parse(&serde_json::json!({"apps": []})).unwrap().dark);
    }

    #[test]
    fn base64_round_trips_the_thumbnail_encoder() {
        let bytes: Vec<u8> = (0..=255).collect();
        assert_eq!(base64_decode(&crate::shell::thumbnails::base64(&bytes)).unwrap(), bytes);
        assert_eq!(base64_decode("aGk"), Some(b"hi".to_vec()));
        assert_eq!(base64_decode("a$b"), None);
    }

    #[test]
    fn the_ink_is_fixed_per_theme_and_the_accent_is_the_seam() {
        let light = bar_colors(Some([0xea, 0xb3, 0x08]), false, material(false));
        let dark = bar_colors(Some([0xea, 0xb3, 0x08]), true, material(true));
        assert_eq!(&light.ink[..3], &INK_DARK, "dark ink on the light bar");
        assert_eq!(&dark.ink[..3], &INK_LIGHT, "light ink on the dark bar");
        assert_eq!(light.accent, [0xea, 0xb3, 0x08]);
        // The accent is not in the fill: the same material gives the same glass.
        let other = bar_colors(Some([0x7c, 0x3a, 0xed]), false, material(false));
        assert_eq!(light.glass, other.glass);
        assert_ne!(light.accent, other.accent);
        // An app with no accent gets a seam in the ink's colour.
        assert_eq!(bar_colors(None, false, material(false)).accent, INK_DARK);
        // Both flats read under their ink, active or not, over Bloom's or Evo's material.
        for (c, ink) in [(light, INK_DARK), (dark, INK_LIGHT)] {
            for glass in [c.glass, c.glass_inactive] {
                assert!(contrast([glass[0], glass[1], glass[2]], ink) >= 7.0, "{glass:?}");
            }
        }
    }

    #[test]
    fn the_inactive_flat_bar_is_further_from_the_wallpaper_than_the_active_one() {
        let c = bar_colors(None, false, [0xe0, 0xb0, 0x70]);
        let chroma = |g: [u8; 4]| {
            let hi = g[0].max(g[1]).max(g[2]) as i32;
            let lo = g[0].min(g[1]).min(g[2]) as i32;
            hi - lo
        };
        assert!(chroma(c.glass_inactive) < chroma(c.glass));
    }

    #[test]
    fn material_is_bloom_s_when_it_sent_one_and_a_bad_colour_costs_nothing() {
        let body = serde_json::json!({
            "apps": [],
            "material": {"light": "#f0e8d8", "dark": "not a colour"}
        });
        let u = parse(&body).unwrap();
        assert_eq!(u.material, Material { light: Some([0xf0, 0xe8, 0xd8]), dark: None });
        let none = parse(&serde_json::json!({"apps": []})).unwrap();
        assert_eq!(none.material, Material::default());
        let junk = parse(&serde_json::json!({"apps": [], "material": 7})).unwrap();
        assert_eq!(junk.material, Material::default());
    }

    #[test]
    fn mix_goes_corner_by_corner() {
        let half = Radii::FRAME.mix(Radii::uniform(24.0), 0.5);
        assert_eq!(half, Radii([20.0, 14.0, 20.0, 20.0]));
        assert_eq!(Radii::FRAME.mix(Radii::SQUARE, 1.0), Radii::SQUARE);
        assert_eq!(Radii::FRAME.scale(2.0), Radii([32.0, 8.0, 32.0, 32.0]));
    }
}
