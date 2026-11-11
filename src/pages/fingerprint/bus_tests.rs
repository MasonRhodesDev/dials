//! The worker and the fprintd calls against a stand-in fprintd on a private
//! bus: a scratch dbus-daemon, and a fake net.reactivated.Fprint that
//! records each write and sends the status signals a test asks for. Skipped
//! where dbus-daemon isn't installed.
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use zbus::blocking::Connection;
use zbus::blocking::connection::Builder;
use zbus::zvariant::OwnedObjectPath;

use super::fprintd::{self, DEVICE_IFACE, Probe, Reader, SERVICE};
use super::worker::{Cmd, Config, Event, Handle};

const MANAGER: &str = "/net/reactivated/Fprint/Manager";
const DEVICE: &str = "/net/reactivated/Fprint/Device/0";

struct Bus {
    daemon: Child,
    dir: PathBuf,
    address: String,
}

impl Bus {
    fn start(tag: &str) -> Option<Bus> {
        let dir = std::env::temp_dir().join(format!("dials-fp-bus-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).ok()?;
        let config = dir.join("bus.conf");
        let conf = format!(
            r#"<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN"
 "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig>
  <type>session</type>
  <listen>unix:path={}/bus</listen>
  <policy context="default">
    <allow send_destination="*" eavesdrop="true"/>
    <allow eavesdrop="true"/>
    <allow own="*"/>
  </policy>
</busconfig>
"#,
            dir.display()
        );
        fs::write(&config, conf).ok()?;
        let spawned = Command::new("dbus-daemon")
            .arg(format!("--config-file={}", config.display()))
            .args(["--nofork", "--print-address=1"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn();
        let Ok(mut daemon) = spawned else {
            eprintln!("dbus-daemon isn't installed; skipping");
            return None;
        };
        let mut line = String::new();
        BufReader::new(daemon.stdout.take()?)
            .read_line(&mut line)
            .ok()?;
        Some(Bus {
            daemon,
            dir,
            address: line.trim().to_string(),
        })
    }

    fn connect(&self) -> Connection {
        Builder::address(self.address.as_str())
            .unwrap()
            .build()
            .unwrap()
    }

    /// A worker on this bus, with its events on a channel.
    fn worker(&self) -> (Handle, Receiver<Event>) {
        let address = self.address.clone();
        let (tx, rx) = mpsc::channel();
        let config = Config {
            connect: Box::new(move || Builder::address(address.as_str())?.build()),
            pam_root: self.dir.clone(),
        };
        let handle = Handle::spawn(config, move |e| {
            let _ = tx.send(e);
        });
        (handle, rx)
    }
}

impl Drop for Bus {
    fn drop(&mut self) {
        let _ = self.daemon.kill();
        let _ = self.daemon.wait();
        let _ = fs::remove_dir_all(&self.dir);
    }
}

#[derive(Default)]
struct State {
    /// Every write, as "Method" or "Method arg".
    calls: Vec<String>,
    claimed: bool,
    enrolling: Option<String>,
    enrolled: Vec<String>,
    /// Claim answers AlreadyInUse, as when a lock screen holds the reader.
    busy: bool,
    /// fprintd before 1.92: no DeleteEnrolledFinger.
    old: bool,
}

type Shared = Arc<Mutex<State>>;

/// An error reply under fprintd's (or the bus's) own name. Hand-written:
/// the DBusError derive puts one prefix on every name, and replies to a
/// wrapped zbus::Error as org.freedesktop.zbus.Error.
#[derive(Debug)]
struct FpError {
    name: &'static str,
    message: String,
}

impl FpError {
    fn fprint(short: &str, message: &str) -> FpError {
        let name = match short {
            "AlreadyInUse" => "net.reactivated.Fprint.Error.AlreadyInUse",
            "ClaimDevice" => "net.reactivated.Fprint.Error.ClaimDevice",
            "NoEnrolledPrints" => "net.reactivated.Fprint.Error.NoEnrolledPrints",
            "NoSuchDevice" => "net.reactivated.Fprint.Error.NoSuchDevice",
            "UnknownMethod" => "org.freedesktop.DBus.Error.UnknownMethod",
            _ => unreachable!("{short}"),
        };
        FpError {
            name,
            message: message.into(),
        }
    }
}

impl From<zbus::Error> for FpError {
    fn from(e: zbus::Error) -> Self {
        FpError {
            name: "org.freedesktop.DBus.Error.Failed",
            message: e.to_string(),
        }
    }
}

impl zbus::DBusError for FpError {
    fn create_reply(
        &self,
        call: &zbus::message::Header<'_>,
    ) -> zbus::Result<zbus::message::Message> {
        zbus::message::Message::error(call, self.name())?.build(&(self.message.as_str(),))
    }

    fn name(&self) -> zbus::names::ErrorName<'_> {
        zbus::names::ErrorName::from_static_str_unchecked(self.name)
    }

    fn description(&self) -> Option<&str> {
        Some(&self.message)
    }
}

struct Manager {
    devices: Vec<OwnedObjectPath>,
}

#[zbus::interface(name = "net.reactivated.Fprint.Manager")]
impl Manager {
    fn get_devices(&self) -> Vec<OwnedObjectPath> {
        self.devices.clone()
    }

    fn get_default_device(&self) -> Result<OwnedObjectPath, FpError> {
        self.devices
            .first()
            .cloned()
            .ok_or_else(|| FpError::fprint("NoSuchDevice", "No devices available"))
    }
}

struct Device(Shared);

impl Device {
    /// Log a write; it needs the reader claimed.
    fn claimed(&self, call: String) -> Result<std::sync::MutexGuard<'_, State>, FpError> {
        let mut s = self.0.lock().unwrap();
        s.calls.push(call);
        if !s.claimed {
            return Err(FpError::fprint("ClaimDevice", "Device was not claimed"));
        }
        Ok(s)
    }
}

#[zbus::interface(name = "net.reactivated.Fprint.Device")]
impl Device {
    fn claim(&self, _username: &str) -> Result<(), FpError> {
        let mut s = self.0.lock().unwrap();
        s.calls.push("Claim".into());
        if s.busy || s.claimed {
            return Err(FpError::fprint(
                "AlreadyInUse",
                "Device was already claimed",
            ));
        }
        s.claimed = true;
        Ok(())
    }

    fn release(&self) -> Result<(), FpError> {
        self.claimed("Release".into())?.claimed = false;
        Ok(())
    }

    fn list_enrolled_fingers(&self, _username: &str) -> Result<Vec<String>, FpError> {
        let s = self.0.lock().unwrap();
        if s.enrolled.is_empty() {
            return Err(FpError::fprint(
                "NoEnrolledPrints",
                "Failed to discover prints",
            ));
        }
        Ok(s.enrolled.clone())
    }

    fn enroll_start(&self, finger: &str) -> Result<(), FpError> {
        self.claimed(format!("EnrollStart {finger}"))?.enrolling = Some(finger.into());
        Ok(())
    }

    fn enroll_stop(&self) -> Result<(), FpError> {
        self.claimed("EnrollStop".into())?.enrolling = None;
        Ok(())
    }

    fn verify_start(&self, finger: &str) -> Result<(), FpError> {
        self.claimed(format!("VerifyStart {finger}")).map(drop)
    }

    fn verify_stop(&self) -> Result<(), FpError> {
        self.claimed("VerifyStop".into()).map(drop)
    }

    fn delete_enrolled_finger(&self, finger: &str) -> Result<(), FpError> {
        let mut s = self.claimed(format!("DeleteEnrolledFinger {finger}"))?;
        if s.old {
            return Err(FpError::fprint(
                "UnknownMethod",
                "Unknown method DeleteEnrolledFinger",
            ));
        }
        s.enrolled.retain(|f| f != finger);
        Ok(())
    }

    #[zbus(name = "DeleteEnrolledFingers2")]
    fn delete_enrolled_fingers2(&self) -> Result<(), FpError> {
        self.claimed("DeleteEnrolledFingers2".into())?
            .enrolled
            .clear();
        Ok(())
    }

    #[zbus(property, name = "name")]
    fn reader_name(&self) -> String {
        "Virtual press reader".into()
    }

    #[zbus(property, name = "num-enroll-stages")]
    fn num_enroll_stages(&self) -> i32 {
        5
    }

    #[zbus(property, name = "scan-type")]
    fn scan_type(&self) -> String {
        "press".into()
    }
}

/// The stand-in fprintd: owns net.reactivated.Fprint while it lives.
struct Fake {
    conn: Connection,
    state: Shared,
}

impl Fake {
    fn serve(bus: &Bus, reader: bool, state: State) -> Fake {
        let state = Arc::new(Mutex::new(state));
        let devices = if reader {
            vec![OwnedObjectPath::try_from(DEVICE).unwrap()]
        } else {
            Vec::new()
        };
        let conn = Builder::address(bus.address.as_str())
            .unwrap()
            .serve_at(MANAGER, Manager { devices })
            .unwrap()
            .serve_at(DEVICE, Device(state.clone()))
            .unwrap()
            .name(SERVICE)
            .unwrap()
            .build()
            .unwrap();
        Fake { conn, state }
    }

    fn emit(&self, path: &str, member: &str, result: &str, done: bool) {
        self.conn
            .emit_signal(None::<&str>, path, DEVICE_IFACE, member, &(result, done))
            .unwrap();
    }

    /// The last scan of an enrollment: the print is stored, then reported.
    fn complete(&self) {
        {
            let mut s = self.state.lock().unwrap();
            let finger = s.enrolling.clone().expect("an enrollment is running");
            s.enrolled.push(finger);
        }
        self.emit(DEVICE, "EnrollStatus", "enroll-completed", true);
    }

    fn calls(&self) -> Vec<String> {
        std::mem::take(&mut self.state.lock().unwrap().calls)
    }

    fn claimed(&self) -> bool {
        self.state.lock().unwrap().claimed
    }
}

fn enrolled(names: &[&str]) -> Vec<String> {
    names.iter().map(|n| n.to_string()).collect()
}

fn next(rx: &Receiver<Event>) -> Event {
    rx.recv_timeout(Duration::from_secs(10))
        .expect("a worker event")
}

/// The next event is a snapshot; its enrolled fingers.
fn snapshot(rx: &Receiver<Event>) -> Vec<String> {
    match next(rx) {
        Event::Snapshot {
            probe: Probe::Ready { enrolled, .. },
            ..
        } => enrolled,
        other => panic!("expected a ready snapshot, got {other:?}"),
    }
}

fn quiet(rx: &Receiver<Event>) {
    if let Ok(e) = rx.recv_timeout(Duration::from_millis(300)) {
        panic!("unexpected event {e:?}");
    }
}

fn status(text: &str) -> Event {
    Event::Status(text.into())
}

fn enroll_status(result: &str, done: bool) -> Event {
    Event::EnrollStatus {
        result: result.into(),
        done,
    }
}

#[test]
fn probe_reads_the_reader_and_sorts_prints() {
    let Some(bus) = Bus::start("probe") else {
        return;
    };
    let conn = bus.connect();
    assert_eq!(
        fprintd::probe(&conn),
        Probe::Unreachable("fprintd isn't installed on the system bus.".into())
    );

    let fake = Fake::serve(
        &bus,
        true,
        State {
            enrolled: enrolled(&["right-index-finger", "left-thumb"]),
            ..Default::default()
        },
    );
    let reader = Reader {
        path: DEVICE.into(),
        name: "Virtual press reader".into(),
        swipe: false,
        stages: 5,
    };
    assert_eq!(
        fprintd::probe(&conn),
        Probe::Ready {
            reader: reader.clone(),
            enrolled: enrolled(&["left-thumb", "right-index-finger"]),
        }
    );
    fake.state.lock().unwrap().enrolled.clear();
    assert_eq!(
        fprintd::probe(&conn),
        Probe::Ready {
            reader,
            enrolled: Vec::new()
        }
    );
    // Reads never claim the reader.
    assert!(fake.calls().is_empty());
}

#[test]
fn probe_without_a_reader() {
    let Some(bus) = Bus::start("noreader") else {
        return;
    };
    let _fake = Fake::serve(&bus, false, State::default());
    assert_eq!(fprintd::probe(&bus.connect()), Probe::NoReader);
}

#[test]
fn enroll_relays_each_scan_then_releases() {
    let Some(bus) = Bus::start("enroll") else {
        return;
    };
    let fake = Fake::serve(&bus, true, State::default());
    let (worker, rx) = bus.worker();

    worker.send(Cmd::Refresh);
    assert!(snapshot(&rx).is_empty());

    worker.send(Cmd::Enroll("right-index-finger".into()));
    assert_eq!(
        next(&rx),
        Event::EnrollStarted {
            finger: "right-index-finger".into(),
            stages: 5
        }
    );
    // Another reader's signal and a verify result aren't this session's.
    fake.emit(
        "/net/reactivated/Fprint/Device/1",
        "EnrollStatus",
        "enroll-completed",
        true,
    );
    fake.emit(DEVICE, "VerifyStatus", "verify-match", true);
    for _ in 0..4 {
        fake.emit(DEVICE, "EnrollStatus", "enroll-stage-passed", false);
    }
    fake.emit(DEVICE, "EnrollStatus", "enroll-retry-scan", false);
    fake.complete();

    for _ in 0..4 {
        assert_eq!(next(&rx), enroll_status("enroll-stage-passed", false));
    }
    assert_eq!(next(&rx), enroll_status("enroll-retry-scan", false));
    assert_eq!(next(&rx), enroll_status("enroll-completed", true));
    assert_eq!(next(&rx), Event::Idle);
    assert_eq!(snapshot(&rx), enrolled(&["right-index-finger"]));
    quiet(&rx);

    assert_eq!(
        fake.calls(),
        [
            "Claim",
            "EnrollStart right-index-finger",
            "EnrollStop",
            "Release"
        ]
    );
    assert!(!fake.claimed());
}

#[test]
fn verify_matches_any_finger_then_releases() {
    let Some(bus) = Bus::start("verify") else {
        return;
    };
    let fake = Fake::serve(
        &bus,
        true,
        State {
            enrolled: enrolled(&["left-thumb"]),
            ..Default::default()
        },
    );
    let (worker, rx) = bus.worker();

    worker.send(Cmd::Verify);
    assert_eq!(next(&rx), Event::VerifyStarted);
    fake.emit(DEVICE, "VerifyStatus", "verify-retry-scan", false);
    fake.emit(DEVICE, "VerifyStatus", "verify-match", true);
    assert_eq!(
        next(&rx),
        Event::VerifyStatus {
            result: "verify-retry-scan".into(),
            done: false
        }
    );
    assert_eq!(
        next(&rx),
        Event::VerifyStatus {
            result: "verify-match".into(),
            done: true
        }
    );
    assert_eq!(next(&rx), Event::Idle);
    quiet(&rx);
    assert_eq!(
        fake.calls(),
        ["Claim", "VerifyStart any", "VerifyStop", "Release"]
    );
    assert!(!fake.claimed());
}

#[test]
fn cancel_stops_the_scan_and_ignores_late_signals() {
    let Some(bus) = Bus::start("cancel") else {
        return;
    };
    let fake = Fake::serve(&bus, true, State::default());
    let (worker, rx) = bus.worker();

    worker.send(Cmd::Enroll("left-thumb".into()));
    assert!(matches!(next(&rx), Event::EnrollStarted { .. }));
    assert!(fake.claimed());
    worker.send(Cmd::Cancel);
    assert_eq!(next(&rx), status("Cancelled."));
    assert_eq!(next(&rx), Event::Idle);
    fake.emit(DEVICE, "EnrollStatus", "enroll-stage-passed", false);
    // Nothing is running, so a second cancel says nothing.
    worker.send(Cmd::Cancel);
    quiet(&rx);
    assert_eq!(
        fake.calls(),
        ["Claim", "EnrollStart left-thumb", "EnrollStop", "Release"]
    );
    assert!(!fake.claimed());
}

#[test]
fn delete_one_then_all() {
    let Some(bus) = Bus::start("delete") else {
        return;
    };
    let fake = Fake::serve(
        &bus,
        true,
        State {
            enrolled: enrolled(&["left-thumb", "right-index-finger", "right-thumb"]),
            ..Default::default()
        },
    );
    let (worker, rx) = bus.worker();

    worker.send(Cmd::Delete("left-thumb".into()));
    assert_eq!(next(&rx), status("Deleted Left thumb."));
    assert_eq!(
        snapshot(&rx),
        enrolled(&["right-thumb", "right-index-finger"])
    );
    assert_eq!(next(&rx), Event::Idle);

    worker.send(Cmd::DeleteAll);
    assert_eq!(next(&rx), status("Deleted every enrolled finger."));
    assert!(snapshot(&rx).is_empty());
    assert_eq!(next(&rx), Event::Idle);

    assert_eq!(
        fake.calls(),
        [
            "Claim",
            "DeleteEnrolledFinger left-thumb",
            "Release",
            "Claim",
            "DeleteEnrolledFingers2",
            "Release"
        ]
    );
    assert!(!fake.claimed());
}

#[test]
fn old_fprintd_points_at_delete_all() {
    let Some(bus) = Bus::start("old") else {
        return;
    };
    let fake = Fake::serve(
        &bus,
        true,
        State {
            enrolled: enrolled(&["left-thumb"]),
            old: true,
            ..Default::default()
        },
    );
    let (worker, rx) = bus.worker();

    worker.send(Cmd::Delete("left-thumb".into()));
    match next(&rx) {
        Event::Status(s) => assert!(s.contains("needs 1.92"), "{s}"),
        other => panic!("expected a status, got {other:?}"),
    }
    assert_eq!(snapshot(&rx), enrolled(&["left-thumb"]));
    assert_eq!(next(&rx), Event::Idle);
    assert!(!fake.claimed());
}

#[test]
fn a_busy_reader_starts_nothing() {
    let Some(bus) = Bus::start("busy") else {
        return;
    };
    let fake = Fake::serve(
        &bus,
        true,
        State {
            enrolled: enrolled(&["left-thumb"]),
            busy: true,
            ..Default::default()
        },
    );
    let (worker, rx) = bus.worker();
    let busy = status(&fprintd::describe(&fprintd::Failure {
        name: "net.reactivated.Fprint.Error.AlreadyInUse".into(),
        message: String::new(),
    }));

    worker.send(Cmd::Enroll("right-thumb".into()));
    assert_eq!(next(&rx), busy);
    assert_eq!(next(&rx), Event::Idle);

    worker.send(Cmd::Delete("left-thumb".into()));
    assert_eq!(next(&rx), busy);
    assert_eq!(snapshot(&rx), enrolled(&["left-thumb"]));
    assert_eq!(next(&rx), Event::Idle);

    // The session never started, so a scan signal is dropped.
    fake.emit(DEVICE, "EnrollStatus", "enroll-stage-passed", false);
    quiet(&rx);
    assert_eq!(fake.calls(), ["Claim", "Claim"]);
}

#[test]
fn no_bus_reports_unreachable() {
    let (tx, rx) = mpsc::channel();
    let config = Config {
        connect: Box::new(|| Err(zbus::Error::Unsupported)),
        pam_root: std::env::temp_dir().join("dials-fp-no-such-root"),
    };
    let _worker = Handle::spawn(config, move |e| {
        let _ = tx.send(e);
    });
    assert!(matches!(
        next(&rx),
        Event::Snapshot {
            probe: Probe::Unreachable(_),
            ..
        }
    ));
}
