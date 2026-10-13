//! The admin worker: one thread owns the system-bus connection and makes every
//! privileged call, because a polkit prompt blocks the caller until the user
//! answers. The UI sends `Cmd`s; results come back as `Event`s through the
//! callback (which wakes the Slint loop). A second thread turns the services'
//! `PropertiesChanged` signals into refreshes, so nothing polls.

use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use zbus::MatchRule;
use zbus::blocking::{Connection, MessageIterator};

use crate::system;

pub enum Cmd {
    RefreshSystem,
    LoadSystemLists,
    SetStaticHostname(String),
    SetPrettyHostname(String),
    SetTimezone(String),
    SetNtp(bool),
    SetLang(String),
    SetLayout(String),
}

pub enum Event {
    System(system::Snapshot),
    SystemLists(system::Lists),
    SystemStatus(String),
}

pub struct Admin {
    tx: Sender<Cmd>,
}

impl Admin {
    pub fn spawn(emit: impl Fn(Event) + Send + 'static) -> Self {
        let (tx, rx) = mpsc::channel();
        let signal_tx = tx.clone();
        thread::Builder::new()
            .name("dials-admin".into())
            .spawn(move || run(rx, signal_tx, emit))
            .expect("spawn admin worker");
        Admin { tx }
    }

    pub fn send(&self, cmd: Cmd) {
        // The worker only exits with the process; a send error means it
        // panicked, which already printed.
        let _ = self.tx.send(cmd);
    }
}

fn run(rx: Receiver<Cmd>, signal_tx: Sender<Cmd>, emit: impl Fn(Event)) {
    let conn = match Connection::system() {
        Ok(conn) => conn,
        Err(e) => {
            let msg = format!("System bus unavailable: {e}");
            emit(Event::SystemStatus(msg.clone()));
            // Keep answering so every action reports why it did nothing.
            for _ in rx {
                emit(Event::SystemStatus(msg.clone()));
            }
            return;
        }
    };
    spawn_signal_watch(conn.clone(), signal_tx);

    let mut snapshot = system::Snapshot::default();
    for cmd in rx {
        let result = match cmd {
            Cmd::RefreshSystem => {
                refresh(&conn, &mut snapshot, &emit, false);
                continue;
            }
            Cmd::LoadSystemLists => {
                emit(Event::SystemLists(system::read_lists(&conn)));
                continue;
            }
            Cmd::SetStaticHostname(name) => system::set_static_hostname(&conn, &name)
                .map(|()| format!("Hostname set to {name}.")),
            Cmd::SetPrettyHostname(name) => system::set_pretty_hostname(&conn, &name)
                .map(|()| "Device name updated.".to_string()),
            Cmd::SetTimezone(tz) => {
                system::set_timezone(&conn, &tz).map(|()| format!("Time zone set to {tz}."))
            }
            Cmd::SetNtp(on) => system::set_ntp(&conn, on).map(|()| {
                if on {
                    "Automatic time on."
                } else {
                    "Automatic time off."
                }
                .to_string()
            }),
            Cmd::SetLang(lang) => system::set_lang(&conn, &snapshot.locale, &lang)
                .map(|()| format!("Language set to {lang}. Applies at next login.")),
            Cmd::SetLayout(layout) => system::set_layout(&conn, &layout)
                .map(|()| format!("Keyboard layout set to {layout}.")),
        };
        emit(Event::SystemStatus(result.unwrap_or_else(|e| e)));
        // Signals cover most changes, but not every property emits one
        // (NTPSynchronized doesn't); re-read after any write.
        refresh(&conn, &mut snapshot, &emit, true);
    }
}

fn refresh(conn: &Connection, snapshot: &mut system::Snapshot, emit: &impl Fn(Event), quiet: bool) {
    let (snap, errors) = system::read(conn);
    *snapshot = snap.clone();
    emit(Event::System(snap));
    if !errors.is_empty() && !quiet {
        emit(Event::SystemStatus(errors.join("; ")));
    }
}

fn spawn_signal_watch(conn: Connection, tx: Sender<Cmd>) {
    let _ = thread::Builder::new()
        .name("dials-admin-signals".into())
        .spawn(move || {
            let rule = match MatchRule::builder()
                .msg_type(zbus::message::Type::Signal)
                .interface("org.freedesktop.DBus.Properties")
                .and_then(|b| b.member("PropertiesChanged"))
                .and_then(|b| b.path_namespace("/org/freedesktop"))
            {
                Ok(b) => b.build(),
                Err(_) => return,
            };
            let Ok(iter) = MessageIterator::for_match_rule(rule, &conn, None) else {
                return;
            };
            for msg in iter.flatten() {
                let watched = msg
                    .header()
                    .path()
                    .is_some_and(|p| system::WATCHED_PATHS.contains(&p.as_str()));
                if watched && tx.send(Cmd::RefreshSystem).is_err() {
                    return;
                }
            }
        });
}
