//! fprintd on the system bus (net.reactivated.Fprint): find the reader, list,
//! enroll, verify and delete the caller's own prints. Every call names the
//! empty username, which fprintd reads as the caller; enrolling for anyone
//! else needs the admin-only `setusername` action, which dials never uses.
//! Writes allow interactive authorization so polkit can prompt
//! (`net.reactivated.fprint.device.enroll` is `auth_self_keep`).
use std::path::Path;

use zbus::blocking::{Connection, Proxy};
use zbus::proxy::{CacheProperties, MethodFlags};
use zbus::zvariant::OwnedObjectPath;

pub const SERVICE: &str = "net.reactivated.Fprint";
pub const DEVICE_IFACE: &str = "net.reactivated.Fprint.Device";
const MANAGER_PATH: &str = "/net/reactivated/Fprint/Manager";
const MANAGER_IFACE: &str = "net.reactivated.Fprint.Manager";

/// fprintd's finger names and their labels, left hand then right.
pub const FINGERS: [(&str, &str); 10] = [
    ("left-thumb", "Left thumb"),
    ("left-index-finger", "Left index"),
    ("left-middle-finger", "Left middle"),
    ("left-ring-finger", "Left ring"),
    ("left-little-finger", "Left little"),
    ("right-thumb", "Right thumb"),
    ("right-index-finger", "Right index"),
    ("right-middle-finger", "Right middle"),
    ("right-ring-finger", "Right ring"),
    ("right-little-finger", "Right little"),
];

pub fn finger_label(name: &str) -> &str {
    FINGERS
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, l)| *l)
        .unwrap_or(name)
}

/// fprintd is installed: its D-Bus activation file exists. A file check, so
/// the toolbar can decide without bus traffic or starting the daemon.
pub fn installed() -> bool {
    installed_under(Path::new("/"))
}

fn installed_under(root: &Path) -> bool {
    [
        "usr/share/dbus-1/system-services",
        "usr/local/share/dbus-1/system-services",
        "etc/dbus-1/system-services",
    ]
    .iter()
    .any(|d| root.join(d).join(format!("{SERVICE}.service")).exists())
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Reader {
    pub path: String,
    pub name: String,
    /// Swipe readers want a swipe, press readers a touch.
    pub swipe: bool,
    /// Scans per enrollment; fprintd reports -1 when the driver can't say.
    pub stages: i32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Probe {
    /// fprintd isn't on the bus (not installed, or the bus refused).
    Unreachable(String),
    NoReader,
    Ready {
        reader: Reader,
        enrolled: Vec<String>,
    },
}

/// One failed call: the D-Bus error name (empty for transport errors) and
/// fprintd's message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    pub name: String,
    pub message: String,
}

impl Failure {
    pub fn is(&self, short: &str) -> bool {
        self.name.rsplit('.').next() == Some(short)
    }
}

impl From<zbus::Error> for Failure {
    fn from(e: zbus::Error) -> Self {
        match e {
            zbus::Error::MethodError(name, msg, _) => Failure {
                name: name.to_string(),
                message: msg.unwrap_or_default(),
            },
            zbus::Error::FDO(fdo) => {
                use zbus::DBusError;
                Failure {
                    name: fdo.name().to_string(),
                    message: fdo.description().unwrap_or_default().to_string(),
                }
            }
            other => Failure {
                name: String::new(),
                message: other.to_string(),
            },
        }
    }
}

impl From<zbus::fdo::Error> for Failure {
    fn from(e: zbus::fdo::Error) -> Self {
        zbus::Error::from(e).into()
    }
}

impl From<zbus::names::Error> for Failure {
    fn from(e: zbus::names::Error) -> Self {
        zbus::Error::from(e).into()
    }
}

/// One status line for a failed call.
pub fn describe(f: &Failure) -> String {
    let short = f.name.rsplit('.').next().unwrap_or("");
    match short {
        "PermissionDenied" | "AccessDenied" | "NotAuthorized" => {
            "Not authorized: the password prompt was cancelled or denied, or no polkit agent is running.".into()
        }
        "InteractiveAuthorizationRequired" => {
            "Needs your password, but no polkit agent is running.".into()
        }
        "AlreadyInUse" => {
            "The reader is busy (a lock screen or another prompt is using it). Try again in a moment.".into()
        }
        "ClaimDevice" => "Couldn't open the reader.".into(),
        "NoSuchDevice" => "The reader is gone.".into(),
        "NoEnrolledPrints" => "No fingers are enrolled yet.".into(),
        "PrintsNotDeleted" => "fprintd couldn't delete the print.".into(),
        "PrintsNotDeletedFromDevice" => {
            "Deleted, but the reader's own storage still holds a copy.".into()
        }
        "InvalidFingername" => "fprintd doesn't know that finger name.".into(),
        "ServiceUnknown" | "NameHasNoOwner" => "fprintd isn't running on the system bus.".into(),
        _ if !f.message.is_empty() => f.message.clone(),
        _ if !f.name.is_empty() => f.name.clone(),
        _ => "fprintd call failed.".into(),
    }
}

fn proxy<'a>(conn: &'a Connection, path: &'a str, iface: &'a str) -> zbus::Result<Proxy<'a>> {
    zbus::blocking::proxy::Builder::new(conn)
        .destination(SERVICE)?
        .path(path)?
        .interface(iface)?
        .cache_properties(CacheProperties::No)
        .build()
}

fn device<'a>(conn: &'a Connection, path: &'a str) -> zbus::Result<Proxy<'a>> {
    proxy(conn, path, DEVICE_IFACE)
}

/// fprintd owns its name or the bus can start it.
fn on_bus(conn: &Connection) -> Result<bool, Failure> {
    let bus = zbus::blocking::fdo::DBusProxy::new(conn)?;
    let name = zbus::names::BusName::try_from(SERVICE)?;
    if bus.name_has_owner(name)? {
        return Ok(true);
    }
    Ok(bus
        .list_activatable_names()?
        .iter()
        .any(|n| n.as_str() == SERVICE))
}

/// Find the reader and the caller's prints. Starts fprintd if it isn't
/// running (it exits again when idle).
pub fn probe(conn: &Connection) -> Probe {
    match on_bus(conn) {
        Ok(true) => {}
        Ok(false) => {
            return Probe::Unreachable("fprintd isn't installed on the system bus.".into());
        }
        Err(f) => return Probe::Unreachable(describe(&f)),
    }
    let devices: Vec<OwnedObjectPath> =
        match proxy(conn, MANAGER_PATH, MANAGER_IFACE).and_then(|m| m.call("GetDevices", &())) {
            Ok(d) => d,
            Err(e) => return Probe::Unreachable(describe(&e.into())),
        };
    if devices.is_empty() {
        return Probe::NoReader;
    }
    let path = match default_device(conn) {
        Ok(p) => p,
        Err(f) if f.is("NoSuchDevice") => return Probe::NoReader,
        Err(f) => return Probe::Unreachable(describe(&f)),
    };
    let reader = match read_reader(conn, &path) {
        Ok(r) => r,
        Err(f) => return Probe::Unreachable(describe(&f)),
    };
    match list_enrolled(conn, &path) {
        Ok(enrolled) => Probe::Ready { reader, enrolled },
        Err(f) => Probe::Unreachable(describe(&f)),
    }
}

pub fn default_device(conn: &Connection) -> Result<String, Failure> {
    let path: OwnedObjectPath =
        proxy(conn, MANAGER_PATH, MANAGER_IFACE)?.call("GetDefaultDevice", &())?;
    Ok(path.as_str().to_string())
}

fn read_reader(conn: &Connection, path: &str) -> Result<Reader, Failure> {
    let d = device(conn, path)?;
    Ok(Reader {
        path: path.to_string(),
        name: d.get_property::<String>("name")?,
        swipe: d.get_property::<String>("scan-type")? == "swipe",
        stages: d.get_property::<i32>("num-enroll-stages")?,
    })
}

/// Scans per enrollment, as the reader reports it.
pub fn stages(conn: &Connection, path: &str) -> Option<i32> {
    device(conn, path)
        .ok()?
        .get_property::<i32>("num-enroll-stages")
        .ok()
}

/// The caller's enrolled fingers, in `FINGERS` order. fprintd answers
/// "none" with the NoEnrolledPrints error.
pub fn list_enrolled(conn: &Connection, path: &str) -> Result<Vec<String>, Failure> {
    match device(conn, path)?.call::<_, _, Vec<String>>("ListEnrolledFingers", &("",)) {
        Ok(mut names) => {
            names.sort_by_key(|n| FINGERS.iter().position(|(f, _)| f == n).unwrap_or(99));
            Ok(names)
        }
        Err(e) => {
            let f = Failure::from(e);
            if f.is("NoEnrolledPrints") {
                Ok(Vec::new())
            } else {
                Err(f)
            }
        }
    }
}

fn write<B>(conn: &Connection, path: &str, method: &str, body: &B) -> Result<(), Failure>
where
    B: serde::Serialize + zbus::zvariant::DynamicType,
{
    device(conn, path)?
        .call_with_flags::<_, _, ()>(method, MethodFlags::AllowInteractiveAuth.into(), body)
        .map(drop)
        .map_err(Failure::from)
}

/// Claim the reader for the caller. Exclusive: while dials holds it,
/// pam_fprintd can't use it, so every claim is released when its action ends.
pub fn claim(conn: &Connection, path: &str) -> Result<(), Failure> {
    write(conn, path, "Claim", &("",))
}

pub fn release(conn: &Connection, path: &str) -> Result<(), Failure> {
    write(conn, path, "Release", &())
}

pub fn enroll_start(conn: &Connection, path: &str, finger: &str) -> Result<(), Failure> {
    write(conn, path, "EnrollStart", &(finger,))
}

pub fn enroll_stop(conn: &Connection, path: &str) -> Result<(), Failure> {
    write(conn, path, "EnrollStop", &())
}

/// Match against any enrolled finger.
pub fn verify_start(conn: &Connection, path: &str) -> Result<(), Failure> {
    write(conn, path, "VerifyStart", &("any",))
}

pub fn verify_stop(conn: &Connection, path: &str) -> Result<(), Failure> {
    write(conn, path, "VerifyStop", &())
}

/// Delete one print. `DeleteEnrolledFinger` arrived in fprintd 1.92; older
/// daemons answer UnknownMethod, which the caller reports.
pub fn delete_finger(conn: &Connection, path: &str, finger: &str) -> Result<(), Failure> {
    write(conn, path, "DeleteEnrolledFinger", &(finger,))
}

/// Delete every print of the caller on this reader.
pub fn delete_all(conn: &Connection, path: &str) -> Result<(), Failure> {
    write(conn, path, "DeleteEnrolledFingers2", &())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    /// A scan counted.
    Progress,
    /// Didn't count; try again.
    Retry,
    Done,
    Failed,
}

/// What to tell the user for an `EnrollStatus` result.
pub fn enroll_feedback(result: &str, swipe: bool) -> (Tone, &'static str) {
    match result {
        "enroll-stage-passed" => (
            Tone::Progress,
            if swipe {
                "Got it. Swipe again."
            } else {
                "Got it. Lift your finger and touch the reader again."
            },
        ),
        "enroll-completed" => (Tone::Done, "Enrolled."),
        "enroll-retry-scan" => (Tone::Retry, "That scan didn't read. Try again."),
        "enroll-swipe-too-short" => (Tone::Retry, "Swipe was too short. Try again."),
        "enroll-too-fast" => (Tone::Retry, "Too fast. Slow down and try again."),
        "enroll-finger-not-centered" => (Tone::Retry, "Center your finger on the reader."),
        "enroll-remove-and-retry" => (Tone::Retry, "Lift your finger, then try again."),
        "enroll-duplicate" => (
            Tone::Failed,
            "That finger is already enrolled (under another name). Delete it first.",
        ),
        "enroll-data-full" => (
            Tone::Failed,
            "The reader's storage is full. Delete a print, then try again.",
        ),
        "enroll-disconnected" => (Tone::Failed, "The reader was disconnected."),
        "enroll-failed" => (Tone::Failed, "Enrollment failed. Try again."),
        _ => (Tone::Failed, "Enrollment failed with an unknown error."),
    }
}

/// What to tell the user for a `VerifyStatus` result.
pub fn verify_feedback(result: &str, swipe: bool) -> (Tone, &'static str) {
    match result {
        "verify-match" => (Tone::Done, "Matched. This finger will work."),
        "verify-no-match" => (Tone::Failed, "No match. That finger isn't enrolled."),
        "verify-retry-scan" => (Tone::Retry, "That scan didn't read. Try again."),
        "verify-swipe-too-short" => (Tone::Retry, "Swipe was too short. Try again."),
        "verify-too-fast" => (Tone::Retry, "Too fast. Slow down and try again."),
        "verify-finger-not-centered" => (Tone::Retry, "Center your finger on the reader."),
        "verify-remove-and-retry" => (Tone::Retry, "Lift your finger, then try again."),
        "verify-disconnected" => (Tone::Failed, "The reader was disconnected."),
        _ if swipe => (Tone::Failed, "Couldn't check the swipe."),
        _ => (Tone::Failed, "Couldn't check the finger."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_documented_result_has_feedback() {
        // From fprintd's net.reactivated.Fprint.Device.xml.
        for r in [
            "enroll-completed",
            "enroll-failed",
            "enroll-stage-passed",
            "enroll-retry-scan",
            "enroll-swipe-too-short",
            "enroll-finger-not-centered",
            "enroll-remove-and-retry",
            "enroll-data-full",
            "enroll-duplicate",
            "enroll-disconnected",
            "enroll-too-fast",
        ] {
            assert_ne!(
                enroll_feedback(r, false).1,
                "Enrollment failed with an unknown error.",
                "{r}"
            );
        }
        assert_eq!(
            enroll_feedback("enroll-unknown-error", false).0,
            Tone::Failed
        );
        assert_eq!(
            enroll_feedback("enroll-stage-passed", true).1,
            "Got it. Swipe again."
        );
        assert_eq!(verify_feedback("verify-match", false).0, Tone::Done);
        assert_eq!(verify_feedback("verify-retry-scan", false).0, Tone::Retry);
    }

    #[test]
    fn failures_read_as_sentences() {
        let f = |name: &str| Failure {
            name: name.into(),
            message: "raw".into(),
        };
        assert!(describe(&f("net.reactivated.Fprint.Error.AlreadyInUse")).contains("busy"));
        assert!(
            describe(&f("net.reactivated.Fprint.Error.PermissionDenied"))
                .starts_with("Not authorized")
        );
        assert_eq!(describe(&f("org.example.Other")), "raw");
        assert!(f("net.reactivated.Fprint.Error.NoEnrolledPrints").is("NoEnrolledPrints"));
    }

    #[test]
    fn finger_labels() {
        assert_eq!(finger_label("right-index-finger"), "Right index");
        assert_eq!(finger_label("unknown"), "unknown");
    }

    #[test]
    fn installed_checks_the_activation_file() {
        let root = std::env::temp_dir().join(format!("dials-fp-installed-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        assert!(!installed_under(&root));
        let dir = root.join("usr/share/dbus-1/system-services");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("net.reactivated.Fprint.service"), "").unwrap();
        assert!(installed_under(&root));
        let _ = std::fs::remove_dir_all(&root);
    }
}
