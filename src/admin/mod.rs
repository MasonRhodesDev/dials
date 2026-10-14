//! The admin worker shared by the Users and System pages: one thread owns the
//! system-bus connection and makes every privileged call, because a polkit
//! prompt blocks the caller until the user answers. Pages send `Cmd`s through
//! `Client`; results come back as `Event`s, queued and drained on the Slint
//! thread (`attach`). Watcher threads turn the services' signals into
//! refreshes, so nothing polls.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;

use slint::ComponentHandle;
use zbus::MatchRule;
use zbus::blocking::{Connection, MessageIterator};

#[cfg(feature = "system")]
use crate::pages::system::systemd as system;
#[cfg(feature = "users")]
use crate::pages::users::{accounts as users, passwd_pty};
use crate::{AppState, AppWindow};

pub enum Cmd {
    #[cfg(feature = "system")]
    System(SystemCmd),
    #[cfg(feature = "users")]
    Users(UsersCmd),
}

#[cfg(feature = "system")]
pub enum SystemCmd {
    Refresh,
    LoadLists,
    SetStaticHostname(String),
    SetPrettyHostname(String),
    SetTimezone(String),
    SetNtp(bool),
    SetLang(String),
    SetLayout(String),
}

#[cfg(feature = "users")]
pub enum UsersCmd {
    Refresh,
    CreateUser {
        name: String,
        real_name: String,
        admin: bool,
        password: String,
    },
    SetRealName(u32, String),
    SetAdmin(u32, bool),
    SetLocked(u32, bool),
    SetIcon(u32, String),
    SetPassword(u32, String),
    ChangeOwnPassword {
        current: String,
        new: String,
    },
    DeleteUser {
        uid: u32,
        remove_home: bool,
    },
    SetGroupMember {
        uid: u32,
        group: String,
        member: bool,
    },
    CreateGroup(String),
    DeleteGroup(String),
}

impl Cmd {
    /// Refreshes of the same page collapse into one read.
    fn refresh_key(&self) -> Option<u8> {
        match self {
            #[cfg(feature = "system")]
            Cmd::System(SystemCmd::Refresh) => Some(0),
            #[cfg(feature = "users")]
            Cmd::Users(UsersCmd::Refresh) => Some(1),
            #[allow(unreachable_patterns)]
            _ => None,
        }
    }
}

pub enum Event {
    #[cfg(feature = "system")]
    System(SystemEvent),
    #[cfg(feature = "users")]
    Users(UsersEvent),
}

#[cfg(feature = "system")]
pub enum SystemEvent {
    Snapshot(system::Snapshot),
    Lists(system::Lists),
    Status(String),
}

#[cfg(feature = "users")]
pub enum UsersEvent {
    Snapshot(users::Snapshot),
    Status(String),
    /// Select this uid once it shows up in the list.
    Created(u32),
}

/// Every page's status line, for failures that aren't any one page's.
fn status_all(text: &str) -> Vec<Event> {
    vec![
        #[cfg(feature = "system")]
        Event::System(SystemEvent::Status(text.to_string())),
        #[cfg(feature = "users")]
        Event::Users(UsersEvent::Status(text.to_string())),
    ]
}

/// UI-side handle, kept in `AppState`: starts the worker on first use.
pub struct Client {
    ui: slint::Weak<AppWindow>,
    worker: Option<Admin>,
    wake: Arc<Wake>,
    pub me_uid: u32,
}

/// Unlike telemetry (newest frame wins), every event matters: a status line
/// for each action, so this is a queue.
#[derive(Default)]
struct Wake {
    queue: Mutex<Vec<Event>>,
    pending: AtomicBool,
}

impl Client {
    pub fn new(ui: &AppWindow) -> Self {
        Self {
            ui: ui.as_weak(),
            worker: None,
            wake: Arc::new(Wake::default()),
            // SAFETY: getuid cannot fail.
            me_uid: unsafe { libc::getuid() },
        }
    }

    pub fn send(&mut self, cmd: Cmd) {
        let worker = self.worker.get_or_insert_with(|| {
            let wake = Arc::downgrade(&self.wake);
            let ui = self.ui.clone();
            Admin::spawn(move |event| {
                let Some(wake) = wake.upgrade() else {
                    return;
                };
                wake.queue.lock().expect("admin queue").push(event);
                if !wake.pending.swap(true, Ordering::AcqRel)
                    && ui
                        .upgrade_in_event_loop(|ui| ui.invoke_admin_wake())
                        .is_err()
                {
                    wake.pending.store(false, Ordering::Release);
                }
            })
        });
        worker.send(cmd);
    }

    fn take_events(&self) -> Vec<Event> {
        self.wake.pending.store(false, Ordering::Release);
        std::mem::take(&mut *self.wake.queue.lock().expect("admin queue"))
    }
}

/// Drain the worker's events on the Slint thread and hand each to its page.
pub fn attach(ui: &AppWindow, state: &Rc<RefCell<AppState>>) {
    let ui_weak = ui.as_weak();
    let state = state.clone();
    ui.on_admin_wake(move || {
        let Some(ui) = ui_weak.upgrade() else {
            return;
        };
        // Pages borrow state while handling; don't hold it across them.
        let events = state.borrow().admin.take_events();
        for event in events {
            crate::pages::admin_event(&ui, &state, event);
        }
    });
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

struct Worker<E: Fn(Event)> {
    #[cfg_attr(not(any(feature = "system", feature = "users")), allow(dead_code))]
    conn: Connection,
    emit: E,
    #[cfg(feature = "system")]
    system: system::Snapshot,
    #[cfg(feature = "users")]
    users: users::Snapshot,
    #[cfg(feature = "users")]
    me_uid: u32,
}

fn run(rx: Receiver<Cmd>, signal_tx: Sender<Cmd>, emit: impl Fn(Event)) {
    let conn = match Connection::system() {
        Ok(conn) => conn,
        Err(e) => {
            let msg = format!("System bus unavailable: {e}");
            status_all(&msg).into_iter().for_each(&emit);
            // Keep answering so every action reports why it did nothing.
            for _ in rx {
                status_all(&msg).into_iter().for_each(&emit);
            }
            return;
        }
    };
    #[cfg(feature = "system")]
    watch_system(conn.clone(), signal_tx.clone());
    #[cfg(feature = "users")]
    watch_accounts(conn.clone(), signal_tx.clone());
    drop(signal_tx);

    let mut w = Worker {
        conn,
        emit,
        #[cfg(feature = "system")]
        system: system::Snapshot::default(),
        #[cfg(feature = "users")]
        users: users::Snapshot::default(),
        // SAFETY: getuid cannot fail.
        #[cfg(feature = "users")]
        me_uid: unsafe { libc::getuid() },
    };
    // Signals arrive in bursts (one Changed per property); collapse queued
    // refreshes of the same kind into one read.
    let mut backlog: VecDeque<Cmd> = VecDeque::new();
    loop {
        let cmd = match backlog.pop_front() {
            Some(cmd) => cmd,
            None => match rx.recv() {
                Ok(cmd) => cmd,
                Err(_) => return,
            },
        };
        if let Some(key) = cmd.refresh_key() {
            backlog.extend(rx.try_iter());
            backlog.retain(|c| c.refresh_key() != Some(key));
        }
        w.handle(cmd);
    }
}

impl<E: Fn(Event)> Worker<E> {
    fn handle(&mut self, cmd: Cmd) {
        match cmd {
            #[cfg(feature = "system")]
            Cmd::System(cmd) => self.handle_system(cmd),
            #[cfg(feature = "users")]
            Cmd::Users(cmd) => self.handle_user(cmd),
        }
    }
}

#[cfg(feature = "system")]
impl<E: Fn(Event)> Worker<E> {
    fn system_status(&self, text: String) {
        (self.emit)(Event::System(SystemEvent::Status(text)));
    }

    fn handle_system(&mut self, cmd: SystemCmd) {
        let conn = &self.conn;
        let result = match cmd {
            SystemCmd::Refresh => return self.refresh_system(false),
            SystemCmd::LoadLists => {
                let lists = system::read_lists(conn);
                return (self.emit)(Event::System(SystemEvent::Lists(lists)));
            }
            SystemCmd::SetStaticHostname(name) => system::set_static_hostname(conn, &name)
                .map(|()| format!("Hostname set to {name}.")),
            SystemCmd::SetPrettyHostname(name) => system::set_pretty_hostname(conn, &name)
                .map(|()| "Device name updated.".to_string()),
            SystemCmd::SetTimezone(tz) => {
                system::set_timezone(conn, &tz).map(|()| format!("Time zone set to {tz}."))
            }
            SystemCmd::SetNtp(on) => system::set_ntp(conn, on).map(|()| {
                if on {
                    "Automatic time on."
                } else {
                    "Automatic time off."
                }
                .to_string()
            }),
            SystemCmd::SetLang(lang) => system::set_lang(conn, &self.system.locale, &lang)
                .map(|()| format!("Language set to {lang}. Applies at next login.")),
            SystemCmd::SetLayout(layout) => system::set_layout(conn, &self.system, &layout)
                .map(|()| format!("Keyboard layout set to {layout}.")),
        };
        self.system_status(result.unwrap_or_else(|e| e));
        // Signals cover most changes, but not every property emits one
        // (NTPSynchronized doesn't); re-read after any write.
        self.refresh_system(true);
    }

    fn refresh_system(&mut self, quiet: bool) {
        let (snap, errors) = system::read(&self.conn);
        self.system = snap.clone();
        (self.emit)(Event::System(SystemEvent::Snapshot(snap)));
        if !errors.is_empty() && !quiet {
            self.system_status(errors.join("; "));
        }
    }
}

#[cfg(feature = "users")]
impl<E: Fn(Event)> Worker<E> {
    fn users_status(&self, text: String) {
        (self.emit)(Event::Users(UsersEvent::Status(text)));
    }

    fn refresh_users(&mut self) {
        self.users = users::read(&self.conn);
        (self.emit)(Event::Users(UsersEvent::Snapshot(self.users.clone())));
    }

    /// Re-check the UI's guards against the worker's own view before a write.
    fn allowed(&self, uid: u32) -> Option<users::Allowed> {
        let target = self.users.accounts.iter().find(|a| a.uid == uid)?;
        let admins = self.users.accounts.iter().filter(|a| a.admin).count();
        Some(users::allowed(
            target,
            self.me_uid,
            self.me_admin(),
            admins,
            &self.users.logged_in,
        ))
    }

    fn me_admin(&self) -> bool {
        self.users
            .accounts
            .iter()
            .any(|a| a.uid == self.me_uid && a.admin)
    }

    fn handle_user(&mut self, cmd: UsersCmd) {
        if matches!(cmd, UsersCmd::Refresh) {
            return self.refresh_users();
        }
        let group_cmd = matches!(
            cmd,
            UsersCmd::SetGroupMember { .. } | UsersCmd::CreateGroup(_) | UsersCmd::DeleteGroup(_)
        );
        if group_cmd && !self.me_admin() {
            return self.users_status("Only an administrator can change groups.".into());
        }
        // Groups go through pkexec and self-change through passwd; the rest
        // needs AccountsService.
        if !self.users.service && !group_cmd && !matches!(cmd, UsersCmd::ChangeOwnPassword { .. }) {
            return self.users_status(
                "AccountsService isn't installed, so accounts are read-only.".into(),
            );
        }
        let conn = &self.conn;
        let denied = |why: &str| Err::<String, String>(why.to_string());
        let result = match cmd {
            UsersCmd::Refresh => unreachable!("handled above"),
            UsersCmd::CreateUser {
                name,
                real_name,
                admin,
                password,
            } => {
                let r = users::create(conn, &name, &real_name, admin).and_then(|uid| {
                    (self.emit)(Event::Users(UsersEvent::Created(uid)));
                    // AccountsService creates the account locked with no
                    // password; setting one unlocks it.
                    users::set_password(conn, uid, &password)
                        .map(|()| format!("Created {name}."))
                        .map_err(|e| {
                            format!("Created {name}, but setting the password failed: {e}")
                        })
                });
                users::wipe_string(password);
                r
            }
            UsersCmd::SetRealName(uid, name) => match self.allowed(uid) {
                Some(a) if a.edit_profile => {
                    users::set_real_name(conn, uid, &name).map(|()| "Name updated.".into())
                }
                _ => denied("You can't change this account's name."),
            },
            UsersCmd::SetIcon(uid, file) => match self.allowed(uid) {
                Some(a) if a.edit_profile => {
                    users::set_icon(conn, uid, &file).map(|()| "Picture updated.".into())
                }
                _ => denied("You can't change this account's picture."),
            },
            UsersCmd::SetAdmin(uid, admin) => match self.allowed(uid) {
                Some(a) if a.change_admin => users::set_admin(conn, uid, admin).map(|()| {
                    if admin {
                        "Now an administrator."
                    } else {
                        "Now a standard user."
                    }
                    .to_string()
                }),
                _ => denied("At least one administrator has to remain."),
            },
            UsersCmd::SetLocked(uid, locked) => match self.allowed(uid) {
                Some(a) if a.lock => users::set_locked(conn, uid, locked).map(|()| {
                    if locked {
                        "Login disabled."
                    } else {
                        "Login enabled."
                    }
                    .to_string()
                }),
                _ => denied("You can't disable your own login."),
            },
            UsersCmd::SetPassword(uid, password) => {
                let r = match self.allowed(uid) {
                    Some(a) if a.set_password && !a.needs_current_password => {
                        users::set_password(conn, uid, &password)
                            .map(|()| "Password changed.".into())
                    }
                    _ => denied("You can't set this account's password."),
                };
                users::wipe_string(password);
                r
            }
            UsersCmd::ChangeOwnPassword { current, new } => {
                let r = passwd_pty::change_own(&current, &new).map(|()| "Password changed.".into());
                users::wipe_string(current);
                users::wipe_string(new);
                r
            }
            UsersCmd::DeleteUser { uid, remove_home } => match self.allowed(uid) {
                Some(a) if a.delete => {
                    let name = self
                        .users
                        .accounts
                        .iter()
                        .find(|a| a.uid == uid)
                        .map(|a| a.name.clone());
                    users::delete(conn, uid, remove_home)
                        .map(|()| format!("Deleted {}.", name.unwrap_or_else(|| uid.to_string())))
                }
                Some(a) => denied(a.delete_reason),
                None => denied("No such account."),
            },
            UsersCmd::SetGroupMember { uid, group, member } => {
                match self.users.accounts.iter().find(|a| a.uid == uid) {
                    Some(a) => users::set_group_member(&a.name, &group, member).map(|()| {
                        if member {
                            format!("Added {} to {group}. Applies at their next login.", a.name)
                        } else {
                            format!(
                                "Removed {} from {group}. Applies at their next login.",
                                a.name
                            )
                        }
                    }),
                    None => denied("No such account."),
                }
            }
            UsersCmd::CreateGroup(name) => {
                let taken: Vec<String> = self.users.groups.iter().map(|g| g.name.clone()).collect();
                users::validate_username(&name, &taken)
                    .and_then(|()| users::create_group(&name))
                    .map(|()| format!("Created group {name}."))
            }
            UsersCmd::DeleteGroup(name) => {
                match self.users.groups.iter().find(|g| g.name == name) {
                    Some(g) => {
                        users::can_delete_group(g, &self.users.login_defs, &self.users.primary_gids)
                            .and_then(|()| users::delete_group(&name))
                            .map(|()| format!("Deleted group {name}."))
                    }
                    None => denied("No such group."),
                }
            }
        };
        self.users_status(result.unwrap_or_else(|e| e));
        self.refresh_users();
    }
}

fn watch(
    conn: Connection,
    name: &str,
    rule: zbus::Result<MatchRule<'static>>,
    mut on: impl FnMut(&zbus::Message) -> bool + Send + 'static,
) {
    let Ok(rule) = rule else {
        return;
    };
    let _ = thread::Builder::new().name(name.into()).spawn(move || {
        let Ok(iter) = MessageIterator::for_match_rule(rule, &conn, None) else {
            return;
        };
        for msg in iter.flatten() {
            if !on(&msg) {
                return;
            }
        }
    });
}

#[cfg(feature = "system")]
fn watch_system(conn: Connection, tx: Sender<Cmd>) {
    let rule = MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .interface("org.freedesktop.DBus.Properties")
        .and_then(|b| b.member("PropertiesChanged"))
        .and_then(|b| b.path_namespace("/org/freedesktop"))
        .map(|b| b.build());
    watch(conn, "dials-admin-system-signals", rule, move |msg| {
        let watched = msg
            .header()
            .path()
            .is_some_and(|p| system::WATCHED_PATHS.contains(&p.as_str()));
        !watched || tx.send(Cmd::System(SystemCmd::Refresh)).is_ok()
    });
}

/// UserAdded / UserDeleted on the manager and Changed on each user.
#[cfg(feature = "users")]
fn watch_accounts(conn: Connection, tx: Sender<Cmd>) {
    let rule = MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .sender("org.freedesktop.Accounts")
        .map(|b| b.build());
    watch(conn, "dials-admin-accounts-signals", rule, move |_| {
        tx.send(Cmd::Users(UsersCmd::Refresh)).is_ok()
    });
}

/// One status line for a failed D-Bus call.
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

#[cfg(test)]
mod tests {
    use super::*;

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
