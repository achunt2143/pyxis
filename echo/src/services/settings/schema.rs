//! The system's own settings: every `pyxis.*` key, its type, the values it may
//! take, and its default.
//!
//! `set` refuses a value outside what a key allows, naming the key, so no
//! `settings:write` holder can store `pyxis.theme = 42` and leave every reader
//! to cope with it. `get` answers an absent key with its default, so a fresh
//! install reads the right wallpaper with no special case anywhere. `pyxis.*`
//! is the system's namespace: a `pyxis.*` key that is not listed here is
//! refused, while any other key is an app's own and is stored as given.
//!
//! The SDK keeps its own copy of the keys and defaults (`SETTING_KEYS` and
//! `SETTING_DEFAULTS` in pyxis-experience's `packages/app-sdk`), and a Vitest
//! contract test (`src/hooks/__tests__/settingsContract.test.ts`) parses
//! `SCHEMA` out of this file to check them. Keep to the entry shape below:
//! `Spec { key: "...", kind: Kind::..., default: Default::... }`.

use anyhow::{anyhow, Result};
use serde_json::{Map, Value};

/// What a key may hold.
pub enum Kind {
    /// One of a fixed set of strings.
    OneOf(&'static [&'static str]),
    Bool,
    /// A finite number in `min..=max`. `integer` refuses fractions;
    /// `nullable` also allows null.
    Number { min: f64, max: f64, integer: bool, nullable: bool },
    /// null, or `{lat, lon, label, source}`: latitude -90..=90, longitude
    /// -180..=180, a label of at most `LABEL_MAX` characters, and the source
    /// `"zone"` (the time zone's coordinates) or `"manual"` (set by hand).
    Location,
}

/// A key's value when nothing is stored.
pub enum Default {
    Str(&'static str),
    Bool(bool),
    Int(i64),
    Null,
}

pub struct Spec {
    pub key: &'static str,
    pub kind: Kind,
    pub default: Default,
}

pub const NAMESPACE: &str = "pyxis.";
pub const LABEL_MAX: usize = 120;

/// The wallpapers the shell can draw (`WALLPAPERS` in the app-sdk's
/// `wallpapers.ts`). Daylight follows the sun; the others are fixed palettes.
pub const WALLPAPER_IDS: &[&str] = &["daylight", "aurora", "sunset", "forest", "mochi"];

pub const SCHEMA: &[Spec] = &[
    Spec { key: "pyxis.theme", kind: Kind::OneOf(&["light", "dark", "auto"]), default: Default::Str("light") },
    Spec { key: "pyxis.wallpaper", kind: Kind::OneOf(WALLPAPER_IDS), default: Default::Str("daylight") },
    Spec { key: "pyxis.wallpaper.motion", kind: Kind::OneOf(&["flow", "still"]), default: Default::Str("flow") },
    Spec { key: "pyxis.wallpaper.pinnedTime", kind: Kind::Number { min: 0.0, max: 1439.0, integer: true, nullable: true }, default: Default::Null },
    Spec { key: "pyxis.location", kind: Kind::Location, default: Default::Null },
    Spec { key: "pyxis.clock24h", kind: Kind::Bool, default: Default::Bool(false) },
    Spec { key: "pyxis.showBatteryPercent", kind: Kind::Bool, default: Default::Bool(true) },
    Spec { key: "pyxis.volume", kind: Kind::Number { min: 0.0, max: 100.0, integer: false, nullable: false }, default: Default::Int(50) },
    Spec { key: "pyxis.brightness", kind: Kind::Number { min: 0.0, max: 100.0, integer: false, nullable: false }, default: Default::Int(80) },
    Spec { key: "pyxis.dnd", kind: Kind::Bool, default: Default::Bool(false) },
    Spec { key: "pyxis.animations", kind: Kind::OneOf(&["full", "reduced", "off"]), default: Default::Str("full") },
    Spec { key: "pyxis.launchBar.position", kind: Kind::OneOf(&["bottom", "left"]), default: Default::Str("bottom") },
    Spec { key: "pyxis.lock.after", kind: Kind::Number { min: 1.0, max: 120.0, integer: true, nullable: true }, default: Default::Int(10) },
    Spec { key: "pyxis.lock.onSleep", kind: Kind::Bool, default: Default::Bool(true) },
    Spec { key: "pyxis.lock.notifications", kind: Kind::OneOf(&["hidden", "count", "full"]), default: Default::Str("count") },
    Spec { key: "pyxis.accessibility.textScale", kind: Kind::Number { min: 75.0, max: 150.0, integer: true, nullable: false }, default: Default::Int(100) },
    Spec { key: "pyxis.accessibility.highContrast", kind: Kind::Bool, default: Default::Bool(false) },
    Spec { key: "pyxis.accessibility.reduceTransparency", kind: Kind::Bool, default: Default::Bool(false) },
    Spec { key: "pyxis.accessibility.cursorSize", kind: Kind::OneOf(&["default", "large", "xlarge"]), default: Default::Str("default") },
];

pub fn spec(key: &str) -> Option<&'static Spec> {
    SCHEMA.iter().find(|s| s.key == key)
}

impl Default {
    pub fn value(&self) -> Value {
        match self {
            Default::Str(s) => Value::from(*s),
            Default::Bool(b) => Value::from(*b),
            Default::Int(n) => Value::from(*n),
            Default::Null => Value::Null,
        }
    }
}

/// What `get` answers for `key` when nothing is stored: its default for a
/// listed key, null for any other.
pub fn default_for(key: &str) -> Value {
    spec(key).map(|s| s.default.value()).unwrap_or(Value::Null)
}

/// Every listed key's default, for `getAll`.
pub fn defaults() -> Map<String, Value> {
    SCHEMA.iter().map(|s| (s.key.to_string(), s.default.value())).collect()
}

/// Refuse a value `key` can't hold, with an error naming the key.
pub fn validate(key: &str, value: &Value) -> Result<()> {
    if key.is_empty() {
        return Err(anyhow!("a setting needs a non-empty `key`"));
    }
    let Some(spec) = spec(key) else {
        if key.starts_with(NAMESPACE) {
            return Err(anyhow!(
                "unknown setting {key}: {NAMESPACE}* keys are the system's own, and this one is not one of them"
            ));
        }
        return Ok(());
    };
    match &spec.kind {
        Kind::OneOf(allowed) => match value.as_str() {
            Some(s) if allowed.contains(&s) => Ok(()),
            _ => Err(anyhow!("{key} must be one of {}, not {value}", allowed.join(", "))),
        },
        Kind::Bool => match value {
            Value::Bool(_) => Ok(()),
            _ => Err(anyhow!("{key} must be true or false, not {value}")),
        },
        Kind::Number { min, max, integer, nullable } => {
            if value.is_null() && *nullable {
                return Ok(());
            }
            let what = if *integer { "a whole number" } else { "a number" };
            let or_null = if *nullable { " (or null)" } else { "" };
            match value.as_f64() {
                Some(n) if n >= *min && n <= *max && (!integer || n.fract() == 0.0) => Ok(()),
                _ => Err(anyhow!("{key} must be {what} from {min} to {max}{or_null}, not {value}")),
            }
        }
        Kind::Location => validate_location(key, value),
    }
}

fn validate_location(key: &str, value: &Value) -> Result<()> {
    let obj = match value {
        Value::Null => return Ok(()),
        Value::Object(obj) => obj,
        _ => return Err(anyhow!("{key} must be null or {{lat, lon, label, source}}, not {value}")),
    };
    if let Some(extra) = obj.keys().find(|k| !matches!(k.as_str(), "lat" | "lon" | "label" | "source")) {
        return Err(anyhow!("{key} has an unknown field `{extra}`: it takes lat, lon, label and source"));
    }
    let coordinate = |field: &str, limit: f64| match obj.get(field).and_then(Value::as_f64) {
        Some(n) if (-limit..=limit).contains(&n) => Ok(()),
        _ => Err(anyhow!("{key}.{field} must be a number from -{limit} to {limit}")),
    };
    coordinate("lat", 90.0)?;
    coordinate("lon", 180.0)?;
    match obj.get("label").and_then(Value::as_str) {
        Some(label) if label.chars().count() <= LABEL_MAX => {}
        _ => return Err(anyhow!("{key}.label must be a string of at most {LABEL_MAX} characters")),
    }
    match obj.get("source").and_then(Value::as_str) {
        Some("zone" | "manual") => Ok(()),
        _ => Err(anyhow!("{key}.source must be \"zone\" or \"manual\"")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn every_key_is_in_the_namespace_once_and_its_default_is_valid() {
        let mut seen = std::collections::HashSet::new();
        for spec in SCHEMA {
            assert!(spec.key.starts_with(NAMESPACE), "{} is outside {NAMESPACE}", spec.key);
            assert!(seen.insert(spec.key), "{} is listed twice", spec.key);
            validate(spec.key, &spec.default.value())
                .unwrap_or_else(|e| panic!("{}'s default is not valid: {e}", spec.key));
        }
    }

    #[test]
    fn the_wallpapers_and_themes_the_shell_knows_are_accepted() {
        for id in ["daylight", "aurora", "sunset", "forest", "mochi"] {
            validate("pyxis.wallpaper", &json!(id)).unwrap();
        }
        for theme in ["light", "dark", "auto"] {
            validate("pyxis.theme", &json!(theme)).unwrap();
        }
        assert_eq!(default_for("pyxis.wallpaper"), json!("daylight"));
        assert_eq!(default_for("pyxis.theme"), json!("light"));
        // The launch bar sits at the bottom until someone picks the rail.
        assert_eq!(default_for("pyxis.launchBar.position"), json!("bottom"));
        // The screen locks after ten idle minutes, on sleep, and shows only a
        // count of notifications.
        assert_eq!(default_for("pyxis.lock.after"), json!(10));
        assert_eq!(default_for("pyxis.lock.onSleep"), json!(true));
        assert_eq!(default_for("pyxis.lock.notifications"), json!("count"));
    }

    #[test]
    fn bad_values_are_refused_with_the_key_named() {
        let cases = [
            ("pyxis.theme", json!(42)),
            ("pyxis.theme", json!("sepia")),
            ("pyxis.theme", Value::Null),
            ("pyxis.wallpaper", json!("vaporwave")),
            ("pyxis.wallpaper.motion", json!("fast")),
            ("pyxis.wallpaper.pinnedTime", json!(1440)),
            ("pyxis.wallpaper.pinnedTime", json!(-1)),
            ("pyxis.wallpaper.pinnedTime", json!(12.5)),
            ("pyxis.wallpaper.pinnedTime", json!("noon")),
            ("pyxis.clock24h", json!("true")),
            ("pyxis.dnd", json!(1)),
            ("pyxis.volume", json!(101)),
            ("pyxis.brightness", Value::Null),
            ("pyxis.animations", json!("fancy")),
            ("pyxis.launchBar.position", json!("right")),
            ("pyxis.launchBar.position", json!(true)),
            ("pyxis.launchBar.position", Value::Null),
            ("pyxis.lock.after", json!(0)),
            ("pyxis.lock.after", json!(121)),
            ("pyxis.lock.after", json!(7.5)),
            ("pyxis.lock.after", json!("10")),
            ("pyxis.lock.onSleep", json!("yes")),
            ("pyxis.lock.onSleep", Value::Null),
            ("pyxis.lock.notifications", json!("some")),
            ("pyxis.lock.notifications", Value::Null),
            ("pyxis.location", json!("Los Angeles")),
            ("pyxis.location", json!({"lat": 91, "lon": 0, "label": "x", "source": "manual"})),
            ("pyxis.location", json!({"lat": 0, "lon": 181, "label": "x", "source": "manual"})),
            ("pyxis.location", json!({"lat": 0, "lon": 0, "label": "x", "source": "gps"})),
            ("pyxis.location", json!({"lat": 0, "lon": 0, "source": "manual"})),
            ("pyxis.location", json!({"lat": 0, "lon": 0, "label": "x", "source": "zone", "alt": 3})),
            ("pyxis.location", json!({"lat": 0, "lon": 0, "label": "x".repeat(LABEL_MAX + 1), "source": "zone"})),
            ("pyxis.accessibility.textScale", json!(50)),
            ("pyxis.accessibility.textScale", json!(200)),
            ("pyxis.accessibility.textScale", json!(112.5)),
            ("pyxis.accessibility.highContrast", json!("yes")),
            ("pyxis.accessibility.reduceTransparency", json!(1)),
            ("pyxis.accessibility.cursorSize", json!("huge")),
            ("pyxis.nonsense", json!(true)),
        ];
        for (key, value) in cases {
            let err = validate(key, &value).expect_err(&format!("{key} = {value} was accepted")).to_string();
            assert!(err.contains(key), "error for {key} = {value} doesn't name the key: {err}");
        }
    }

    #[test]
    fn good_values_pass() {
        let cases = [
            ("pyxis.wallpaper.pinnedTime", Value::Null),
            ("pyxis.wallpaper.pinnedTime", json!(0)),
            ("pyxis.wallpaper.pinnedTime", json!(1439)),
            ("pyxis.wallpaper.pinnedTime", json!(720.0)),
            ("pyxis.brightness", json!(37.5)),
            ("pyxis.clock24h", json!(true)),
            ("pyxis.launchBar.position", json!("bottom")),
            ("pyxis.launchBar.position", json!("left")),
            ("pyxis.lock.after", Value::Null),
            ("pyxis.lock.after", json!(1)),
            ("pyxis.lock.after", json!(120)),
            ("pyxis.lock.onSleep", json!(false)),
            ("pyxis.lock.notifications", json!("hidden")),
            ("pyxis.lock.notifications", json!("count")),
            ("pyxis.lock.notifications", json!("full")),
            ("pyxis.accessibility.textScale", json!(75)),
            ("pyxis.accessibility.textScale", json!(125)),
            ("pyxis.accessibility.textScale", json!(150)),
            ("pyxis.accessibility.highContrast", json!(true)),
            ("pyxis.accessibility.reduceTransparency", json!(true)),
            ("pyxis.accessibility.cursorSize", json!("default")),
            ("pyxis.accessibility.cursorSize", json!("large")),
            ("pyxis.accessibility.cursorSize", json!("xlarge")),
            ("pyxis.location", Value::Null),
            ("pyxis.location", json!({"lat": 34.05, "lon": -118.24, "label": "Los Angeles", "source": "zone"})),
            ("pyxis.location", json!({"lat": -90, "lon": 180, "label": "", "source": "manual"})),
        ];
        for (key, value) in cases {
            validate(key, &value).unwrap_or_else(|e| panic!("{key} = {value} refused: {e}"));
        }
    }

    #[test]
    fn keys_outside_the_namespace_are_an_apps_own() {
        validate("com.example.notes.fontSize", &json!({"anything": [1, 2]})).unwrap();
        validate("pyxisish", &json!(42)).unwrap();
        assert_eq!(default_for("com.example.notes.fontSize"), Value::Null);
        assert!(validate("", &json!(1)).is_err());
    }
}
