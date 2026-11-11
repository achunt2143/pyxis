//! Time and place: the device's time zone, the clock in it, and where the
//! device is for the sun.
//!
//! The UTC offset is computed here, never in a page. vela caches the zone it
//! started with, so a page's `Date` and `Intl` keep the old offset after the
//! zone changes; Bloom's clock and the Daylight wallpaper take Echo's epoch
//! and offset instead (the SDK's `useClock`).
//!
//! The zone is `/etc/localtime`, a symlink into `/usr/share/zoneinfo`, as
//! Debian keeps it (create-rootfs.sh links it to UTC). `setTimezone` relinks
//! it atomically and writes `/etc/timezone` beside it. The zones offered are
//! `zone1970.tab`'s (Debian's tzdata), plus `UTC`, which the image starts on.
//!
//! Offsets come from the image's own TZif files, read with jiff (already in
//! the lockfile through env_logger), rather than from a crate that bundles its
//! own copy of the database (chrono-tz): Echo then agrees with libc and with
//! whatever tzdata the image ships, and jiff follows a TZif file's POSIX
//! footer for transitions past the end of its table.
//!
//! `PYXIS_TIME_ROOT` puts the whole tree (`etc/`, `usr/share/zoneinfo/`)
//! under another directory, so tests and a dev echo-daemon never touch the
//! host's `/etc`.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{anyhow, Context, Result};
use jiff::tz::TimeZone;
use jiff::Timestamp;
use parking_lot::Mutex;
use serde::Serialize;
use serde_json::Value;

pub const TIME_IFACE: &str = "org.geckoOS.time";
pub const SIGNAL_TIMEZONE_CHANGED: &str = "TimezoneChanged";

/// The settings key holding a location the user set (lane S's schema).
pub const LOCATION_SETTING: &str = "pyxis.location";

/// The zone used when `/etc/localtime` says nothing usable, and the one zone
/// accepted that zone1970.tab doesn't list.
const UTC: &str = "UTC";
const ZONEINFO: &str = "usr/share/zoneinfo";
/// Where the link points on the device, whatever root it was written under.
const DEVICE_ZONEINFO: &str = "/usr/share/zoneinfo";

/// One `setTimezone` at a time, so two can't interleave their link and
/// /etc/timezone and leave the two disagreeing.
static SET_LOCK: Mutex<()> = Mutex::new(());
static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// `getClock`'s answer.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Clock {
    pub epoch_ms: i64,
    pub timezone: String,
    pub utc_offset_minutes: i32,
    /// "PST", "CEST", or a numeric one ("+0530") where the zone has none.
    pub abbreviation: String,
    pub dst: bool,
    /// The next change of offset (DST), or null for a zone without one.
    pub next_transition_ms: Option<i64>,
}

/// One row of zone1970.tab.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ZoneEntry {
    pub id: String,
    /// ISO 3166 codes, most populous first, as the file orders them.
    pub countries: Vec<String>,
    pub lat: f64,
    pub lon: f64,
}

/// `getLocation`'s answer. `source` is `manual` (the user set
/// `pyxis.location`), `zone` (the zone's principal city, from zone1970.tab),
/// or `none` (a zone with no place, UTC): then lat, lon and label are null,
/// and the wallpaper falls back to a fixed day.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Location {
    pub lat: Option<f64>,
    pub lon: Option<f64>,
    pub label: Option<String>,
    pub source: &'static str,
    pub timezone: String,
}

/// `setTimezone`'s answer, and the `TimezoneChanged` signal's body.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct TimezoneChange {
    pub timezone: String,
    pub utc_offset_minutes: i32,
}

pub struct TimeService {
    root: PathBuf,
}

impl TimeService {
    /// The device's own tree, or `PYXIS_TIME_ROOT`'s.
    pub fn system() -> Self {
        Self::with_root(std::env::var_os("PYXIS_TIME_ROOT").map(PathBuf::from).unwrap_or_else(|| "/".into()))
    }

    pub fn with_root(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    fn zoneinfo(&self) -> PathBuf {
        self.root.join(ZONEINFO)
    }

    fn etc(&self) -> PathBuf {
        self.root.join("etc")
    }

    /// The zone `/etc/localtime` names: the part of its link target after
    /// `zoneinfo/`. A localtime that is a plain file, or a link somewhere
    /// else, falls back to `/etc/timezone`, then to UTC.
    pub fn current_zone(&self) -> String {
        let from_link = fs::read_link(self.etc().join("localtime")).ok().and_then(|target| {
            let target = target.to_string_lossy().into_owned();
            target.rfind("zoneinfo/").map(|at| target[at + "zoneinfo/".len()..].to_string())
        });
        let from_file = || {
            fs::read_to_string(self.etc().join("timezone")).ok().map(|s| s.trim().to_string())
        };
        from_link
            .or_else(from_file)
            .filter(|zone| is_zone_name(zone))
            .unwrap_or_else(|| UTC.to_string())
    }

    /// The zone's rules, from its TZif file under the root.
    fn load(&self, zone: &str) -> Result<TimeZone> {
        if !is_zone_name(zone) {
            return Err(anyhow!("not a time zone name: {zone:?}"));
        }
        let path = self.zoneinfo().join(zone);
        let data = fs::read(&path).with_context(|| format!("no tz data for {zone} at {}", path.display()))?;
        TimeZone::tzif(zone, &data).with_context(|| format!("{} is not TZif data", path.display()))
    }

    /// The current zone's rules. If its file is unreadable the clock still
    /// answers, in UTC, rather than leaving the shell with no time at all.
    fn current(&self) -> (String, TimeZone) {
        let zone = self.current_zone();
        match self.load(&zone) {
            Ok(tz) => (zone, tz),
            Err(e) => {
                tracing::warn!("time: {e:#}; answering in UTC");
                (UTC.to_string(), TimeZone::UTC)
            }
        }
    }

    pub fn clock(&self) -> Clock {
        self.clock_at(Timestamp::now())
    }

    pub fn clock_at(&self, now: Timestamp) -> Clock {
        let (timezone, tz) = self.current();
        clock_in(&timezone, &tz, now)
    }

    /// zone1970.tab, sorted by id.
    pub fn list_timezones(&self) -> Result<Vec<ZoneEntry>> {
        let path = self.zoneinfo().join("zone1970.tab");
        let text = fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
        let mut zones = parse_zone_tab(&text);
        zones.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(zones)
    }

    /// Accept `zone` if zone1970.tab lists it (or it is UTC) and its TZif
    /// file loads. Returns its rules.
    pub fn validate(&self, zone: &str) -> Result<TimeZone> {
        if !is_zone_name(zone) {
            return Err(anyhow!("not a time zone name: {zone:?}"));
        }
        if zone != UTC && !self.list_timezones()?.iter().any(|z| z.id == zone) {
            return Err(anyhow!("unknown time zone {zone:?} (not in zone1970.tab)"));
        }
        self.load(zone)
    }

    /// Point `/etc/localtime` at `zone` and write `/etc/timezone`. Each is
    /// made under a temporary name and renamed over the old one, so a reader
    /// sees the old zone or the new, never a missing or half-written file.
    pub fn set_timezone(&self, zone: &str) -> Result<TimezoneChange> {
        let tz = self.validate(zone)?;
        let _guard = SET_LOCK.lock();
        let etc = self.etc();
        fs::create_dir_all(&etc).with_context(|| format!("creating {}", etc.display()))?;

        let link_tmp = temp_name(&etc, "localtime");
        let target = Path::new(DEVICE_ZONEINFO).join(zone);
        std::os::unix::fs::symlink(&target, &link_tmp)
            .with_context(|| format!("linking {} -> {}", link_tmp.display(), target.display()))?;
        if let Err(e) = fs::rename(&link_tmp, etc.join("localtime")) {
            let _ = fs::remove_file(&link_tmp);
            return Err(e).context("replacing /etc/localtime");
        }

        let name_tmp = temp_name(&etc, "timezone");
        let written = fs::write(&name_tmp, format!("{zone}\n")).and_then(|_| fs::rename(&name_tmp, etc.join("timezone")));
        if let Err(e) = written {
            let _ = fs::remove_file(&name_tmp);
            return Err(e).context("writing /etc/timezone");
        }

        Ok(TimezoneChange {
            timezone: zone.to_string(),
            utc_offset_minutes: offset_minutes(&tz, Timestamp::now()),
        })
    }

    /// The location for the sun: `pyxis.location` if the user set one, else
    /// the current zone's coordinates.
    pub fn location(&self) -> Location {
        let setting = crate::services::settings::SettingsService::get(LOCATION_SETTING)
            .ok()
            .and_then(|reply| reply.get("value").cloned());
        let zones = self.list_timezones().unwrap_or_else(|e| {
            tracing::warn!("time: {e:#}");
            Vec::new()
        });
        resolve_location(setting.as_ref(), &self.current_zone(), &zones)
    }
}

/// The clock in `tz` at `now`.
pub fn clock_in(timezone: &str, tz: &TimeZone, now: Timestamp) -> Clock {
    let info = tz.to_offset_info(now);
    Clock {
        epoch_ms: now.as_millisecond(),
        timezone: timezone.to_string(),
        utc_offset_minutes: info.offset().seconds() / 60,
        abbreviation: info.abbreviation().to_string(),
        dst: info.dst().is_dst(),
        next_transition_ms: tz.following(now).next().map(|t| t.timestamp().as_millisecond()),
    }
}

fn offset_minutes(tz: &TimeZone, at: Timestamp) -> i32 {
    tz.to_offset(at).seconds() / 60
}

/// A zone id is a relative path under zoneinfo, so it must not be able to
/// leave it: ASCII letters, digits, `_`, `+`, `-`, separated by single `/`,
/// no empty, `.` or `..` component, and nothing absolute.
pub fn is_zone_name(zone: &str) -> bool {
    !zone.is_empty()
        && zone.len() <= 64
        && zone.split('/').all(|part| {
            !part.is_empty()
                && !part.starts_with('.')
                && part.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '+' | '-'))
        })
}

fn temp_name(dir: &Path, base: &str) -> PathBuf {
    let n = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
    dir.join(format!(".{base}.pyxis-{}-{n}", std::process::id()))
}

/// zone1970.tab's rows: country codes, ISO 6709 coordinates, the zone id,
/// and an optional comment, tab-separated. Comment lines and rows that don't
/// parse are skipped.
pub fn parse_zone_tab(text: &str) -> Vec<ZoneEntry> {
    text.lines()
        .filter(|line| !line.starts_with('#') && !line.trim().is_empty())
        .filter_map(|line| {
            let mut cols = line.split('\t');
            let countries = cols.next()?.split(',').map(str::to_string).collect();
            let (lat, lon) = parse_iso6709(cols.next()?)?;
            let id = cols.next()?.trim().to_string();
            is_zone_name(&id).then_some(ZoneEntry { id, countries, lat, lon })
        })
        .collect()
}

/// ISO 6709 as zone1970.tab writes it: `±DDMM±DDDMM` or `±DDMMSS±DDDMMSS`.
/// Returns (latitude, longitude) in signed decimal degrees.
pub fn parse_iso6709(s: &str) -> Option<(f64, f64)> {
    let split = s.char_indices().skip(1).find(|(_, c)| matches!(c, '+' | '-'))?.0;
    let (lat, lon) = s.split_at(split);
    let lat = parse_angle(lat, 2)?;
    let lon = parse_angle(lon, 3)?;
    ((-90.0..=90.0).contains(&lat) && (-180.0..=180.0).contains(&lon)).then_some((lat, lon))
}

/// `±D…DMM[SS]` with `degree_digits` digits of degrees.
fn parse_angle(s: &str, degree_digits: usize) -> Option<f64> {
    let sign = match s.as_bytes().first()? {
        b'+' => 1.0,
        b'-' => -1.0,
        _ => return None,
    };
    let digits = &s[1..];
    if !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let field = |from: usize, len: usize| digits[from..from + len].parse::<f64>().ok();
    let (deg, min, sec) = match digits.len() - degree_digits {
        2 => (field(0, degree_digits)?, field(degree_digits, 2)?, 0.0),
        4 => (field(0, degree_digits)?, field(degree_digits, 2)?, field(degree_digits + 2, 2)?),
        _ => return None,
    };
    if min >= 60.0 || sec >= 60.0 {
        return None;
    }
    Some(sign * (deg + min / 60.0 + sec / 3600.0))
}

/// The city part of a zone id, as people write it:
/// `America/Argentina/Buenos_Aires` is "Buenos Aires".
pub fn zone_label(zone: &str) -> String {
    zone.rsplit('/').next().unwrap_or(zone).replace('_', " ")
}

/// The effective location. A `pyxis.location` setting is used when it is an
/// object with a finite `lat` in ±90 and `lon` in ±180; anything else (null,
/// absent, malformed) falls back to the zone.
pub fn resolve_location(setting: Option<&Value>, zone: &str, zones: &[ZoneEntry]) -> Location {
    if let Some(manual) = setting.and_then(manual_location) {
        return Location { timezone: zone.to_string(), ..manual };
    }
    match zones.iter().find(|z| z.id == zone) {
        Some(entry) => Location {
            lat: Some(entry.lat),
            lon: Some(entry.lon),
            label: Some(zone_label(zone)),
            source: "zone",
            timezone: zone.to_string(),
        },
        None => Location { lat: None, lon: None, label: None, source: "none", timezone: zone.to_string() },
    }
}

fn manual_location(value: &Value) -> Option<Location> {
    let lat = value.get("lat")?.as_f64().filter(|v| v.is_finite() && v.abs() <= 90.0)?;
    let lon = value.get("lon")?.as_f64().filter(|v| v.is_finite() && v.abs() <= 180.0)?;
    let label = value.get("label").and_then(Value::as_str).map(str::to_string);
    Some(Location { lat: Some(lat), lon: Some(lon), label, source: "manual", timezone: String::new() })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const HOST_ZONEINFO: &str = "/usr/share/zoneinfo";

    /// A root whose zoneinfo is the host's (read-only, through a link) and
    /// whose /etc is a fresh temp dir linked to UTC, as the image starts.
    fn temp_root() -> (tempfile::TempDir, TimeService) {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("usr/share")).unwrap();
        std::os::unix::fs::symlink(HOST_ZONEINFO, dir.path().join(ZONEINFO)).unwrap();
        fs::create_dir_all(dir.path().join("etc")).unwrap();
        std::os::unix::fs::symlink("/usr/share/zoneinfo/UTC", dir.path().join("etc/localtime")).unwrap();
        let service = TimeService::with_root(dir.path());
        (dir, service)
    }

    fn have_tzdata() -> bool {
        Path::new(HOST_ZONEINFO).join("zone1970.tab").is_file()
    }

    fn ts(s: &str) -> Timestamp {
        s.parse().unwrap()
    }

    #[test]
    fn zone_names_cannot_leave_zoneinfo() {
        for good in ["UTC", "America/Los_Angeles", "America/Argentina/Buenos_Aires", "Etc/GMT+5"] {
            assert!(is_zone_name(good), "{good}");
        }
        for bad in ["", "/etc/shadow", "../../etc/shadow", "America/../../../etc/passwd", "America//X", ".hidden", "Europe/Lon don", "a\0b"] {
            assert!(!is_zone_name(bad), "{bad:?}");
        }
    }

    #[test]
    fn validates_against_zone1970() {
        if !have_tzdata() {
            return;
        }
        let (_dir, service) = temp_root();
        assert!(service.validate("America/Los_Angeles").is_ok());
        assert!(service.validate("UTC").is_ok());
        let made_up = service.validate("Mars/Olympus_Mons").unwrap_err().to_string();
        assert!(made_up.contains("unknown time zone"), "{made_up}");
        assert!(service.validate("../../../etc/passwd").is_err());
        assert!(service.validate("/usr/share/zoneinfo/UTC").is_err());
        // Real TZif files that zone1970.tab doesn't offer (a backward link).
        assert!(service.validate("US/Pacific").is_err());
    }

    #[test]
    fn relinks_localtime_atomically_in_a_temp_root() {
        if !have_tzdata() {
            return;
        }
        let (dir, service) = temp_root();
        assert_eq!(service.current_zone(), "UTC");

        let change = service.set_timezone("America/Los_Angeles").unwrap();
        assert_eq!(change.timezone, "America/Los_Angeles");
        assert!(change.utc_offset_minutes == -480 || change.utc_offset_minutes == -420);

        let etc = dir.path().join("etc");
        let link = fs::read_link(etc.join("localtime")).unwrap();
        assert_eq!(link, Path::new("/usr/share/zoneinfo/America/Los_Angeles"));
        assert_eq!(fs::read_to_string(etc.join("timezone")).unwrap(), "America/Los_Angeles\n");
        assert_eq!(service.current_zone(), "America/Los_Angeles");
        // Nothing left behind under a temporary name.
        let stray: Vec<_> = fs::read_dir(&etc).unwrap().filter_map(|e| e.ok()).filter(|e| e.file_name().to_string_lossy().starts_with('.')).collect();
        assert!(stray.is_empty(), "{stray:?}");

        // A refused zone changes nothing.
        assert!(service.set_timezone("Mars/Olympus_Mons").is_err());
        assert_eq!(service.current_zone(), "America/Los_Angeles");

        service.set_timezone("UTC").unwrap();
        assert_eq!(service.current_zone(), "UTC");
    }

    #[test]
    fn localtime_as_a_plain_file_falls_back_to_etc_timezone() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("etc")).unwrap();
        fs::write(dir.path().join("etc/localtime"), b"TZif").unwrap();
        let service = TimeService::with_root(dir.path());
        assert_eq!(service.current_zone(), "UTC");
        fs::write(dir.path().join("etc/timezone"), "Europe/Berlin\n").unwrap();
        assert_eq!(service.current_zone(), "Europe/Berlin");
    }

    #[test]
    fn offset_on_both_sides_of_dst() {
        if !have_tzdata() {
            return;
        }
        let (_dir, service) = temp_root();
        let tz = service.load("America/Los_Angeles").unwrap();

        // 2026-03-08 02:00 PST is 10:00 UTC: clocks go forward.
        let before = clock_in("America/Los_Angeles", &tz, ts("2026-03-08T09:59:00Z"));
        assert_eq!(before.utc_offset_minutes, -480);
        assert!(!before.dst);
        assert_eq!(before.abbreviation, "PST");
        assert_eq!(before.next_transition_ms, Some(ts("2026-03-08T10:00:00Z").as_millisecond()));
        let after = clock_in("America/Los_Angeles", &tz, ts("2026-03-08T10:01:00Z"));
        assert_eq!(after.utc_offset_minutes, -420);
        assert!(after.dst);
        assert_eq!(after.abbreviation, "PDT");

        // 2026-11-01 02:00 PDT is 09:00 UTC: clocks go back.
        let before = clock_in("America/Los_Angeles", &tz, ts("2026-11-01T08:59:00Z"));
        assert_eq!(before.utc_offset_minutes, -420);
        assert_eq!(before.next_transition_ms, Some(ts("2026-11-01T09:00:00Z").as_millisecond()));
        let after = clock_in("America/Los_Angeles", &tz, ts("2026-11-01T09:01:00Z"));
        assert_eq!(after.utc_offset_minutes, -480);
        // The next one is March 2027, from the TZif footer's rule.
        assert_eq!(after.next_transition_ms, Some(ts("2027-03-14T10:00:00Z").as_millisecond()));
    }

    #[test]
    fn utc_has_no_transitions() {
        if !have_tzdata() {
            return;
        }
        let (_dir, service) = temp_root();
        let clock = service.clock_at(ts("2026-07-01T12:00:00Z"));
        assert_eq!(clock.timezone, "UTC");
        assert_eq!(clock.utc_offset_minutes, 0);
        assert_eq!(clock.next_transition_ms, None);
        assert_eq!(clock.epoch_ms, ts("2026-07-01T12:00:00Z").as_millisecond());
    }

    #[test]
    fn a_missing_zone_file_answers_in_utc() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("etc")).unwrap();
        std::os::unix::fs::symlink("/usr/share/zoneinfo/Europe/Paris", dir.path().join("etc/localtime")).unwrap();
        let clock = TimeService::with_root(dir.path()).clock();
        assert_eq!(clock.timezone, "UTC");
        assert_eq!(clock.utc_offset_minutes, 0);
    }

    #[test]
    fn parses_iso6709_both_forms() {
        // Minutes: Los Angeles, +340308-1181434 is the seconds form.
        assert_eq!(parse_iso6709("+3430-11815"), Some((34.5, -118.25)));
        let (lat, lon) = parse_iso6709("+340308-1181434").unwrap();
        assert!((lat - (34.0 + 3.0 / 60.0 + 8.0 / 3600.0)).abs() < 1e-9);
        assert!((lon + (118.0 + 14.0 / 60.0 + 34.0 / 3600.0)).abs() < 1e-9);
        // Southern and eastern: Sydney.
        let (lat, lon) = parse_iso6709("-3352+15113").unwrap();
        assert!(lat < 0.0 && lon > 0.0);
        for bad in ["", "3430-11815", "+3430", "+343-11815", "+3470-11815", "+3430-11815x", "+9500+00000"] {
            assert_eq!(parse_iso6709(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn parses_zone_tab_rows() {
        let tab = "# comment\n\
                   US\t+340308-1181434\tAmerica/Los_Angeles\tPacific\n\
                   CH,DE,LI\t+4723+00832\tEurope/Zurich\n\
                   XX\tgarbage\tBroken/Row\n";
        let zones = parse_zone_tab(tab);
        assert_eq!(zones.len(), 2);
        assert_eq!(zones[0].id, "America/Los_Angeles");
        assert_eq!(zones[0].countries, vec!["US"]);
        assert_eq!(zones[1].countries, vec!["CH", "DE", "LI"]);
        assert!((zones[1].lat - (47.0 + 23.0 / 60.0)).abs() < 1e-9);
    }

    #[test]
    fn reads_the_hosts_zone_tab() {
        if !have_tzdata() {
            return;
        }
        let (_dir, service) = temp_root();
        let zones = service.list_timezones().unwrap();
        assert!(zones.len() > 300, "{}", zones.len());
        let la = zones.iter().find(|z| z.id == "America/Los_Angeles").unwrap();
        assert!((la.lat - 34.05).abs() < 0.01 && (la.lon + 118.24).abs() < 0.01, "{la:?}");
        assert!(zones.windows(2).all(|w| w[0].id < w[1].id));
    }

    #[test]
    fn location_falls_back_to_the_zone() {
        let zones = parse_zone_tab("US\t+340308-1181434\tAmerica/Los_Angeles\nAR\t-3436-05827\tAmerica/Argentina/Buenos_Aires\n");

        let loc = resolve_location(None, "America/Los_Angeles", &zones);
        assert_eq!(loc.source, "zone");
        assert_eq!(loc.label.as_deref(), Some("Los Angeles"));
        assert!((loc.lat.unwrap() - 34.05).abs() < 0.01);

        let null = json!(null);
        assert_eq!(resolve_location(Some(&null), "America/Argentina/Buenos_Aires", &zones).label.as_deref(), Some("Buenos Aires"));

        // Malformed settings are ignored, not trusted.
        for bad in [json!({"lat": 120.0, "lon": 0.0}), json!({"lat": "34", "lon": 0}), json!("Paris")] {
            assert_eq!(resolve_location(Some(&bad), "America/Los_Angeles", &zones).source, "zone", "{bad}");
        }

        let manual = json!({"lat": 48.85, "lon": 2.35, "label": "Paris", "source": "manual"});
        let loc = resolve_location(Some(&manual), "America/Los_Angeles", &zones);
        assert_eq!(loc.source, "manual");
        assert_eq!(loc.label.as_deref(), Some("Paris"));
        assert_eq!(loc.timezone, "America/Los_Angeles");

        let utc = resolve_location(None, "UTC", &zones);
        assert_eq!((utc.source, utc.lat, utc.label), ("none", None, None));
    }
}
