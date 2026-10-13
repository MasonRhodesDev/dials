//! Machine-wide settings through systemd's hostnamed, timedated and localed on
//! the system bus. Writes pass `interactive = true` so polkit can prompt; that
//! blocks until the user answers, so only the admin worker calls into here.

use std::process::Command;

use zbus::blocking::{Connection, Proxy, proxy::Builder};
use zbus::proxy::{CacheProperties, MethodFlags};

struct Svc {
    dest: &'static str,
    path: &'static str,
    iface: &'static str,
}

const HOSTNAME: Svc = Svc {
    dest: "org.freedesktop.hostname1",
    path: "/org/freedesktop/hostname1",
    iface: "org.freedesktop.hostname1",
};
const TIMEDATE: Svc = Svc {
    dest: "org.freedesktop.timedate1",
    path: "/org/freedesktop/timedate1",
    iface: "org.freedesktop.timedate1",
};
const LOCALE: Svc = Svc {
    dest: "org.freedesktop.locale1",
    path: "/org/freedesktop/locale1",
    iface: "org.freedesktop.locale1",
};

/// Object paths whose `PropertiesChanged` should trigger a re-read.
pub const WATCHED_PATHS: [&str; 3] = [HOSTNAME.path, TIMEDATE.path, LOCALE.path];

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Snapshot {
    pub static_hostname: String,
    pub pretty_hostname: String,
    pub timezone: String,
    pub can_ntp: bool,
    pub ntp: bool,
    pub ntp_synced: bool,
    /// `LANG=…`-style entries, as localed reports them.
    pub locale: Vec<String>,
    pub x11_layout: String,
    pub x11_variant: String,
}

/// Candidate values for the pickers. Read once per worker; they only change
/// when packages do.
#[derive(Clone, Debug, Default)]
pub struct Lists {
    pub timezones: Vec<String>,
    pub locales: Vec<String>,
    pub layouts: Vec<String>,
}

fn proxy<'a>(conn: &'a Connection, svc: &Svc) -> zbus::Result<Proxy<'a>> {
    Builder::new(conn)
        .destination(svc.dest)?
        .path(svc.path)?
        .interface(svc.iface)?
        .cache_properties(CacheProperties::No)
        .build()
}

/// Read everything the System page shows. One service failing (e.g. no
/// localed in a container) leaves its fields empty and is reported.
pub fn read(conn: &Connection) -> (Snapshot, Vec<String>) {
    let mut snap = Snapshot::default();
    let mut errors = Vec::new();

    match proxy(conn, &HOSTNAME) {
        Ok(p) => {
            snap.static_hostname = p.get_property("StaticHostname").unwrap_or_default();
            snap.pretty_hostname = p.get_property("PrettyHostname").unwrap_or_default();
        }
        Err(e) => errors.push(format!("hostnamed: {}", describe(&e))),
    }
    match proxy(conn, &TIMEDATE) {
        Ok(p) => {
            snap.timezone = p.get_property("Timezone").unwrap_or_default();
            snap.can_ntp = p.get_property("CanNTP").unwrap_or(false);
            snap.ntp = p.get_property("NTP").unwrap_or(false);
            snap.ntp_synced = p.get_property("NTPSynchronized").unwrap_or(false);
        }
        Err(e) => errors.push(format!("timedated: {}", describe(&e))),
    }
    match proxy(conn, &LOCALE) {
        Ok(p) => {
            snap.locale = p.get_property("Locale").unwrap_or_default();
            snap.x11_layout = p.get_property("X11Layout").unwrap_or_default();
            snap.x11_variant = p.get_property("X11Variant").unwrap_or_default();
        }
        Err(e) => errors.push(format!("localed: {}", describe(&e))),
    }
    (snap, errors)
}

pub fn read_lists(conn: &Connection) -> Lists {
    let timezones = proxy(conn, &TIMEDATE)
        .and_then(|p| p.call::<_, _, Vec<String>>("ListTimezones", &()))
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| command_lines("timedatectl", &["list-timezones"]));
    Lists {
        timezones,
        locales: command_lines("localectl", &["list-locales"]),
        layouts: command_lines("localectl", &["list-x11-keymap-layouts"]),
    }
}

fn command_lines(cmd: &str, args: &[&str]) -> Vec<String> {
    Command::new(cmd)
        .args(args)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(String::from)
                .collect()
        })
        .unwrap_or_default()
}

/// A write that may need polkit. Both the `interactive` argument (systemd's
/// API) and the message flag are needed: systemd also checks the flag, and
/// without it answers InteractiveAuthorizationRequired instead of prompting.
fn write<B>(p: &Proxy<'_>, method: &str, body: &B) -> zbus::Result<()>
where
    B: serde::Serialize + zbus::zvariant::DynamicType,
{
    p.call_with_flags::<_, _, ()>(method, MethodFlags::AllowInteractiveAuth.into(), body)
        .map(drop)
}

pub fn set_static_hostname(conn: &Connection, name: &str) -> Result<(), String> {
    validate_static_hostname(name)?;
    proxy(conn, &HOSTNAME)
        .and_then(|p| write(&p, "SetStaticHostname", &(name, true)))
        .map_err(|e| describe(&e))
}

pub fn set_pretty_hostname(conn: &Connection, name: &str) -> Result<(), String> {
    validate_pretty_hostname(name)?;
    proxy(conn, &HOSTNAME)
        .and_then(|p| write(&p, "SetPrettyHostname", &(name, true)))
        .map_err(|e| describe(&e))
}

pub fn set_timezone(conn: &Connection, tz: &str) -> Result<(), String> {
    proxy(conn, &TIMEDATE)
        .and_then(|p| write(&p, "SetTimezone", &(tz, true)))
        .map_err(|e| describe(&e))
}

pub fn set_ntp(conn: &Connection, on: bool) -> Result<(), String> {
    proxy(conn, &TIMEDATE)
        .and_then(|p| write(&p, "SetNTP", &(on, true)))
        .map_err(|e| describe(&e))
}

/// Replace `LANG` and keep any other `LC_*` overrides the user already has.
pub fn set_lang(conn: &Connection, current: &[String], lang: &str) -> Result<(), String> {
    let locale = with_lang(current, lang);
    proxy(conn, &LOCALE)
        .and_then(|p| write(&p, "SetLocale", &(locale, true)))
        .map_err(|e| describe(&e))
}

/// X11 layout with no variant; localed converts it to the console keymap too
/// (`convert = true`), which is what a single keyboard choice means here.
pub fn set_layout(conn: &Connection, layout: &str) -> Result<(), String> {
    proxy(conn, &LOCALE)
        .and_then(|p| write(&p, "SetX11Keyboard", &(layout, "", "", "", true, true)))
        .map_err(|e| describe(&e))
}

pub fn describe(e: &zbus::Error) -> String {
    match e {
        zbus::Error::MethodError(name, msg, _) => {
            describe_dbus_error(name.as_str(), msg.as_deref())
        }
        zbus::Error::FDO(fdo) => {
            use zbus::DBusError;
            describe_dbus_error(fdo.name().as_str(), fdo.description())
        }
        other => other.to_string(),
    }
}

/// Turn the D-Bus/polkit error vocabulary into one status line.
pub fn describe_dbus_error(name: &str, msg: Option<&str>) -> String {
    let short = name.rsplit('.').next().unwrap_or(name);
    match short {
        "AccessDenied" | "NotAuthorized" | "Cancelled" => {
            "Not authorized — the password prompt was cancelled or denied.".into()
        }
        "InteractiveAuthorizationRequired" => {
            "Needs an administrator password, but no polkit agent is running.".into()
        }
        "ServiceUnknown" | "NameHasNoOwner" => {
            format!(
                "Service unavailable{}",
                msg.map(|m| format!(": {m}")).unwrap_or_default()
            )
        }
        _ => msg
            .filter(|m| !m.is_empty())
            .map(String::from)
            .unwrap_or_else(|| name.to_string()),
    }
}

pub fn lang_of(locale: &[String]) -> &str {
    locale
        .iter()
        .find_map(|l| l.strip_prefix("LANG="))
        .unwrap_or("")
}

pub fn with_lang(locale: &[String], lang: &str) -> Vec<String> {
    let mut out: Vec<String> = locale
        .iter()
        .filter(|l| !l.starts_with("LANG="))
        .cloned()
        .collect();
    out.insert(0, format!("LANG={lang}"));
    out
}

/// hostnamed's static-hostname rules: dot-separated labels of ASCII letters,
/// digits and hyphens, no label starting or ending with a hyphen, 64 bytes max.
pub fn validate_static_hostname(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("Hostname can't be empty.".into());
    }
    if name.len() > 64 {
        return Err("Hostname is longer than 64 characters.".into());
    }
    for label in name.split('.') {
        if label.is_empty() {
            return Err("Hostname has an empty part between dots.".into());
        }
        if label.len() > 63 {
            return Err("Each dot-separated part must be 63 characters or fewer.".into());
        }
        if !label
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return Err("Use only letters, digits, hyphens and dots.".into());
        }
        if label.starts_with('-') || label.ends_with('-') {
            return Err("A part can't start or end with a hyphen.".into());
        }
    }
    Ok(())
}

pub fn validate_pretty_hostname(name: &str) -> Result<(), String> {
    if name.chars().any(char::is_control) {
        return Err("Device name can't contain control characters.".into());
    }
    Ok(())
}

/// Picker search: exact match, then prefix (of the whole value or of any
/// `/`-separated part, so "chi" finds America/Chicago), then substring.
/// Case-insensitive; keeps list order within each rank.
pub fn matches(items: &[String], query: &str, limit: usize) -> Vec<String> {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return Vec::new();
    }
    let mut ranked: Vec<(u8, &String)> = items
        .iter()
        .filter_map(|item| {
            let low = item.to_lowercase();
            if low == q {
                Some((0, item))
            } else if low.starts_with(&q) || low.split('/').any(|part| part.starts_with(&q)) {
                Some((1, item))
            } else if low.contains(&q) {
                Some((2, item))
            } else {
                None
            }
        })
        .collect();
    ranked.sort_by_key(|(rank, _)| *rank);
    ranked
        .into_iter()
        .take(limit)
        .map(|(_, item)| item.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn static_hostname_rules() {
        assert!(validate_static_hostname("dev").is_ok());
        assert!(validate_static_hostname("mason-desktop.lan").is_ok());
        assert!(validate_static_hostname("").is_err());
        assert!(validate_static_hostname("-dev").is_err());
        assert!(validate_static_hostname("dev-").is_err());
        assert!(validate_static_hostname("a..b").is_err());
        assert!(validate_static_hostname("my_box").is_err());
        assert!(validate_static_hostname("bad name").is_err());
        assert!(validate_static_hostname(&"a".repeat(64)).is_err());
        assert!(validate_static_hostname(&format!("{}.b", "a".repeat(62))).is_ok());
        assert!(validate_static_hostname(&format!("{}.b", "a".repeat(63))).is_err());
    }

    #[test]
    fn pretty_hostname_allows_unicode_not_control() {
        assert!(validate_pretty_hostname("Mason’s Desktop").is_ok());
        assert!(validate_pretty_hostname("").is_ok());
        assert!(validate_pretty_hostname("a\nb").is_err());
    }

    #[test]
    fn lang_round_trip_keeps_overrides() {
        let cur = v(&["LANG=en_US.UTF-8", "LC_TIME=en_GB.UTF-8"]);
        assert_eq!(lang_of(&cur), "en_US.UTF-8");
        assert_eq!(
            with_lang(&cur, "de_DE.UTF-8"),
            v(&["LANG=de_DE.UTF-8", "LC_TIME=en_GB.UTF-8"])
        );
        assert_eq!(lang_of(&[]), "");
        assert_eq!(with_lang(&[], "C.UTF-8"), v(&["LANG=C.UTF-8"]));
    }

    #[test]
    fn matches_ranks_exact_prefix_substring() {
        let tz = v(&[
            "America/Chicago",
            "Asia/Chita",
            "Europe/Zurich",
            "UTC",
            "Etc/UTC",
        ]);
        assert_eq!(
            matches(&tz, "chi", 10),
            v(&["America/Chicago", "Asia/Chita"])
        );
        assert_eq!(matches(&tz, "utc", 10), v(&["UTC", "Etc/UTC"]));
        assert_eq!(matches(&tz, "rich", 10), v(&["Europe/Zurich"]));
        assert_eq!(matches(&tz, "  ", 10), Vec::<String>::new());
        assert_eq!(matches(&tz, "a", 2).len(), 2);
    }

    #[test]
    fn dbus_errors_read_as_sentences() {
        assert!(
            describe_dbus_error("org.freedesktop.DBus.Error.AccessDenied", None)
                .starts_with("Not authorized")
        );
        assert!(
            describe_dbus_error(
                "org.freedesktop.DBus.Error.InteractiveAuthorizationRequired",
                None
            )
            .contains("no polkit agent")
        );
        assert_eq!(
            describe_dbus_error("org.freedesktop.timedate1.Error", Some("Invalid time zone")),
            "Invalid time zone"
        );
    }
}
