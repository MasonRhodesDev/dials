//! The fingerprint worker: one thread owns a system-bus connection and runs
//! every fprintd call, since `Claim` and `EnrollStart` may wait on a polkit
//! prompt. An enrollment or verification is a session: the worker claims
//! the reader, starts it, relays each status signal, and stops and releases
//! the reader when fprintd reports `done` or the page cancels. A watcher
//! thread turns fprintd's signals into messages on the same channel, so the
//! worker blocks on one queue and nothing polls.
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use zbus::MatchRule;
use zbus::blocking::{Connection, MessageIterator};

use super::fprintd::{self, Probe};
use super::pam;

pub enum Cmd {
    Refresh,
    Enroll(String),
    Verify,
    Delete(String),
    DeleteAll,
    /// Stop a running enrollment or verification and release the reader.
    Cancel,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Snapshot {
        probe: Probe,
        pam: pam::Status,
    },
    /// An enrollment started on `finger`; `stages` as the reader reports it.
    EnrollStarted {
        finger: String,
        stages: i32,
    },
    EnrollStatus {
        result: String,
        done: bool,
    },
    VerifyStarted,
    VerifyStatus {
        result: String,
        done: bool,
    },
    /// The session (if any) is over and the reader is released.
    Idle,
    Status(String),
}

enum Msg {
    Cmd(Cmd),
    Signal {
        path: String,
        member: String,
        result: String,
        done: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Enroll,
    Verify,
}

struct Session {
    path: String,
    kind: Kind,
}

/// Where the worker connects and reads PAM from; tests point both at
/// scratch locations.
pub struct Config {
    pub connect: Box<dyn FnOnce() -> zbus::Result<Connection> + Send>,
    pub pam_root: PathBuf,
}

impl Config {
    pub fn system() -> Self {
        Config {
            connect: Box::new(Connection::system),
            pam_root: PathBuf::from("/"),
        }
    }
}

pub struct Handle {
    tx: Sender<Msg>,
}

impl Handle {
    pub fn spawn(config: Config, emit: impl Fn(Event) + Send + 'static) -> Self {
        let (tx, rx) = mpsc::channel();
        let signals = tx.clone();
        thread::Builder::new()
            .name("dials-fingerprint".into())
            .spawn(move || run(config, rx, signals, emit))
            .expect("spawn fingerprint worker");
        Handle { tx }
    }

    pub fn send(&self, cmd: Cmd) {
        let _ = self.tx.send(Msg::Cmd(cmd));
    }
}

struct Worker<E> {
    conn: Connection,
    pam_root: PathBuf,
    emit: E,
    session: Option<Session>,
}

fn run(config: Config, rx: Receiver<Msg>, signals: Sender<Msg>, emit: impl Fn(Event)) {
    let pam_root = config.pam_root;
    let conn = match (config.connect)() {
        Ok(c) => c,
        Err(e) => {
            emit(Event::Snapshot {
                probe: Probe::Unreachable(fprintd::describe(&e.into())),
                pam: pam::read(&pam_root),
            });
            return;
        }
    };
    // Subscribe before any EnrollStart so no status is missed.
    watch(&conn, signals);
    let mut w = Worker {
        conn,
        pam_root,
        emit,
        session: None,
    };
    while let Ok(msg) = rx.recv() {
        match msg {
            Msg::Cmd(cmd) => w.handle(cmd),
            Msg::Signal {
                path,
                member,
                result,
                done,
            } => w.signal(&path, &member, result, done),
        }
    }
    w.end_session();
}

/// Forward EnrollStatus/VerifyStatus from any fprintd device. Returns once
/// the match is registered.
fn watch(conn: &Connection, tx: Sender<Msg>) {
    let rule = MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .interface(fprintd::DEVICE_IFACE)
        .map(|b| b.build());
    let Ok(rule) = rule else {
        return;
    };
    let conn = conn.clone();
    let (ready_tx, ready_rx) = mpsc::channel();
    let spawned = thread::Builder::new()
        .name("dials-fingerprint-signals".into())
        .spawn(move || {
            let iter = MessageIterator::for_match_rule(rule, &conn, None);
            let _ = ready_tx.send(());
            let Ok(iter) = iter else {
                return;
            };
            for msg in iter.flatten() {
                let header = msg.header();
                let (Some(path), Some(member)) = (header.path(), header.member()) else {
                    continue;
                };
                if member.as_str() != "EnrollStatus" && member.as_str() != "VerifyStatus" {
                    continue;
                }
                let Ok((result, done)) = msg.body().deserialize::<(String, bool)>() else {
                    continue;
                };
                let sent = tx.send(Msg::Signal {
                    path: path.to_string(),
                    member: member.to_string(),
                    result,
                    done,
                });
                if sent.is_err() {
                    return;
                }
            }
        });
    if spawned.is_ok() {
        let _ = ready_rx.recv();
    }
}

impl<E: Fn(Event)> Worker<E> {
    fn status(&self, text: impl Into<String>) {
        (self.emit)(Event::Status(text.into()));
    }

    fn refresh(&self) {
        (self.emit)(Event::Snapshot {
            probe: fprintd::probe(&self.conn),
            pam: pam::read(&self.pam_root),
        });
    }

    fn handle(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::Refresh => self.refresh(),
            Cmd::Cancel => {
                if self.session.is_some() {
                    self.end_session();
                    self.status("Cancelled.");
                    (self.emit)(Event::Idle);
                }
            }
            Cmd::Enroll(finger) => self.start(Kind::Enroll, Some(finger)),
            Cmd::Verify => self.start(Kind::Verify, None),
            Cmd::Delete(finger) => {
                let label = fprintd::finger_label(&finger).to_string();
                let r = self.with_claim(|conn, path| {
                    fprintd::delete_finger(conn, path, &finger).map_err(|f| {
                        if f.is("UnknownMethod") {
                            fprintd::Failure {
                                name: String::new(),
                                message: "This fprintd is too old to delete one finger (needs 1.92). Use Delete all instead.".into(),
                            }
                        } else {
                            f
                        }
                    })
                });
                self.finish(r, &format!("Deleted {label}."));
            }
            Cmd::DeleteAll => {
                let r = self.with_claim(fprintd::delete_all);
                self.finish(r, "Deleted every enrolled finger.");
            }
        }
    }

    /// Claim, run one call, release.
    fn with_claim(
        &mut self,
        f: impl FnOnce(&Connection, &str) -> Result<(), fprintd::Failure>,
    ) -> Result<(), fprintd::Failure> {
        if self.session.is_some() {
            return Err(fprintd::Failure {
                name: String::new(),
                message: "Finish or cancel the current scan first.".into(),
            });
        }
        let path = fprintd::default_device(&self.conn)?;
        fprintd::claim(&self.conn, &path)?;
        let r = f(&self.conn, &path);
        let _ = fprintd::release(&self.conn, &path);
        r
    }

    fn finish(&self, r: Result<(), fprintd::Failure>, ok: &str) {
        match r {
            Ok(()) => self.status(ok),
            Err(f) => self.status(fprintd::describe(&f)),
        }
        self.refresh();
        (self.emit)(Event::Idle);
    }

    fn start(&mut self, kind: Kind, finger: Option<String>) {
        if self.session.is_some() {
            return self.status("Finish or cancel the current scan first.");
        }
        let conn = &self.conn;
        let begin = || -> Result<(String, i32), fprintd::Failure> {
            let path = fprintd::default_device(conn)?;
            fprintd::claim(conn, &path)?;
            let started = match (&kind, &finger) {
                (Kind::Enroll, Some(f)) => fprintd::enroll_start(conn, &path, f),
                _ => fprintd::verify_start(conn, &path),
            };
            if let Err(f) = started {
                let _ = fprintd::release(conn, &path);
                return Err(f);
            }
            let stages = fprintd::stages(conn, &path).unwrap_or(-1);
            Ok((path, stages))
        };
        match begin() {
            Ok((path, stages)) => {
                self.session = Some(Session { path, kind });
                (self.emit)(match (kind, finger) {
                    (Kind::Enroll, Some(finger)) => Event::EnrollStarted { finger, stages },
                    _ => Event::VerifyStarted,
                });
            }
            Err(f) => {
                self.status(fprintd::describe(&f));
                (self.emit)(Event::Idle);
            }
        }
    }

    fn signal(&mut self, path: &str, member: &str, result: String, done: bool) {
        let Some(s) = &self.session else {
            return;
        };
        let kind = match member {
            "EnrollStatus" => Kind::Enroll,
            "VerifyStatus" => Kind::Verify,
            _ => return,
        };
        if s.path != path || s.kind != kind {
            return;
        }
        (self.emit)(match kind {
            Kind::Enroll => Event::EnrollStatus { result, done },
            Kind::Verify => Event::VerifyStatus { result, done },
        });
        if done {
            self.end_session();
            (self.emit)(Event::Idle);
            if kind == Kind::Enroll {
                self.refresh();
            }
        }
    }

    /// Stop the running scan, if any, and release the reader.
    fn end_session(&mut self) {
        let Some(s) = self.session.take() else {
            return;
        };
        let _ = match s.kind {
            Kind::Enroll => fprintd::enroll_stop(&self.conn, &s.path),
            Kind::Verify => fprintd::verify_stop(&self.conn, &s.path),
        };
        let _ = fprintd::release(&self.conn, &s.path);
    }
}
