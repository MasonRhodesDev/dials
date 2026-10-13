//! The admin worker: one thread owns the system-bus connection and makes every
//! privileged call, because a polkit prompt blocks the caller until the user
//! answers. The UI sends `Cmd`s; results come back as `Event`s through the
//! callback (which wakes the Slint loop). Watcher threads turn the services'
//! signals into refreshes, so nothing polls.

use std::collections::VecDeque;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use zbus::MatchRule;
use zbus::blocking::{Connection, MessageIterator};

use crate::{passwd_pty, system, users};

pub enum Cmd {
    RefreshSystem,
    LoadSystemLists,
    SetStaticHostname(String),
    SetPrettyHostname(String),
    SetTimezone(String),
    SetNtp(bool),
    SetLang(String),
    SetLayout(String),

    RefreshUsers,
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
    fn is_refresh(&self) -> bool {
        matches!(self, Cmd::RefreshSystem | Cmd::RefreshUsers)
    }
}

pub enum Event {
    System(system::Snapshot),
    SystemLists(system::Lists),
    SystemStatus(String),
    Users(users::Snapshot),
    UsersStatus(String),
    /// Select this uid once it shows up in the list.
    UserCreated(u32),
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
    conn: Connection,
    emit: E,
    system: system::Snapshot,
    users: users::Snapshot,
    me_uid: u32,
}

fn run(rx: Receiver<Cmd>, signal_tx: Sender<Cmd>, emit: impl Fn(Event)) {
    let conn = match Connection::system() {
        Ok(conn) => conn,
        Err(e) => {
            let msg = format!("System bus unavailable: {e}");
            emit(Event::SystemStatus(msg.clone()));
            emit(Event::UsersStatus(msg.clone()));
            // Keep answering so every action reports why it did nothing.
            for _ in rx {
                emit(Event::SystemStatus(msg.clone()));
                emit(Event::UsersStatus(msg.clone()));
            }
            return;
        }
    };
    watch_system(conn.clone(), signal_tx.clone());
    watch_accounts(conn.clone(), signal_tx);

    let mut w = Worker {
        conn,
        emit,
        system: system::Snapshot::default(),
        users: users::Snapshot::default(),
        // SAFETY: getuid cannot fail.
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
        if cmd.is_refresh() {
            backlog.extend(rx.try_iter());
            let kind = std::mem::discriminant(&cmd);
            backlog.retain(|c| std::mem::discriminant(c) != kind);
        }
        w.handle(cmd);
    }
}

impl<E: Fn(Event)> Worker<E> {
    fn handle(&mut self, cmd: Cmd) {
        let conn = &self.conn;
        let system_result = match cmd {
            Cmd::RefreshSystem => return self.refresh_system(false),
            Cmd::LoadSystemLists => {
                return (self.emit)(Event::SystemLists(system::read_lists(conn)));
            }
            Cmd::SetStaticHostname(name) => system::set_static_hostname(conn, &name)
                .map(|()| format!("Hostname set to {name}.")),
            Cmd::SetPrettyHostname(name) => system::set_pretty_hostname(conn, &name)
                .map(|()| "Device name updated.".to_string()),
            Cmd::SetTimezone(tz) => {
                system::set_timezone(conn, &tz).map(|()| format!("Time zone set to {tz}."))
            }
            Cmd::SetNtp(on) => system::set_ntp(conn, on).map(|()| {
                if on {
                    "Automatic time on."
                } else {
                    "Automatic time off."
                }
                .to_string()
            }),
            Cmd::SetLang(lang) => system::set_lang(conn, &self.system.locale, &lang)
                .map(|()| format!("Language set to {lang}. Applies at next login.")),
            Cmd::SetLayout(layout) => system::set_layout(conn, &self.system, &layout)
                .map(|()| format!("Keyboard layout set to {layout}.")),
            user_cmd => return self.handle_user(user_cmd),
        };
        (self.emit)(Event::SystemStatus(system_result.unwrap_or_else(|e| e)));
        // Signals cover most changes, but not every property emits one
        // (NTPSynchronized doesn't); re-read after any write.
        self.refresh_system(true);
    }

    fn refresh_system(&mut self, quiet: bool) {
        let (snap, errors) = system::read(&self.conn);
        self.system = snap.clone();
        (self.emit)(Event::System(snap));
        if !errors.is_empty() && !quiet {
            (self.emit)(Event::SystemStatus(errors.join("; ")));
        }
    }

    fn refresh_users(&mut self) {
        self.users = users::read(&self.conn);
        (self.emit)(Event::Users(self.users.clone()));
    }

    /// Re-check the UI's guards against the worker's own view before a write.
    fn allowed(&self, uid: u32) -> Option<users::Allowed> {
        let target = self.users.accounts.iter().find(|a| a.uid == uid)?;
        let me_admin = self
            .users
            .accounts
            .iter()
            .any(|a| a.uid == self.me_uid && a.admin);
        let admins = self.users.accounts.iter().filter(|a| a.admin).count();
        Some(users::allowed(
            target,
            self.me_uid,
            me_admin,
            admins,
            &self.users.logged_in,
        ))
    }

    fn handle_user(&mut self, cmd: Cmd) {
        if matches!(cmd, Cmd::RefreshUsers) {
            return self.refresh_users();
        }
        let group_cmd = matches!(
            cmd,
            Cmd::SetGroupMember { .. } | Cmd::CreateGroup(_) | Cmd::DeleteGroup(_)
        );
        let me_admin = self
            .users
            .accounts
            .iter()
            .any(|a| a.uid == self.me_uid && a.admin);
        if group_cmd && !me_admin {
            return (self.emit)(Event::UsersStatus(
                "Only an administrator can change groups.".into(),
            ));
        }
        // Groups go through pkexec and self-change through passwd; the rest
        // needs AccountsService.
        if !self.users.service && !group_cmd && !matches!(cmd, Cmd::ChangeOwnPassword { .. }) {
            return (self.emit)(Event::UsersStatus(
                "AccountsService isn't installed, so accounts are read-only.".into(),
            ));
        }
        let conn = &self.conn;
        let denied = |why: &str| Err::<String, String>(why.to_string());
        let result = match cmd {
            Cmd::CreateUser {
                name,
                real_name,
                admin,
                password,
            } => {
                let r = users::create(conn, &name, &real_name, admin).and_then(|uid| {
                    (self.emit)(Event::UserCreated(uid));
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
            Cmd::SetRealName(uid, name) => match self.allowed(uid) {
                Some(a) if a.edit_profile => {
                    users::set_real_name(conn, uid, &name).map(|()| "Name updated.".into())
                }
                _ => denied("You can't change this account's name."),
            },
            Cmd::SetIcon(uid, file) => match self.allowed(uid) {
                Some(a) if a.edit_profile => {
                    users::set_icon(conn, uid, &file).map(|()| "Picture updated.".into())
                }
                _ => denied("You can't change this account's picture."),
            },
            Cmd::SetAdmin(uid, admin) => match self.allowed(uid) {
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
            Cmd::SetLocked(uid, locked) => match self.allowed(uid) {
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
            Cmd::SetPassword(uid, password) => {
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
            Cmd::ChangeOwnPassword { current, new } => {
                let r = passwd_pty::change_own(&current, &new).map(|()| "Password changed.".into());
                users::wipe_string(current);
                users::wipe_string(new);
                r
            }
            Cmd::DeleteUser { uid, remove_home } => match self.allowed(uid) {
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
            Cmd::SetGroupMember { uid, group, member } => {
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
            Cmd::CreateGroup(name) => {
                let taken: Vec<String> = self.users.groups.iter().map(|g| g.name.clone()).collect();
                users::validate_username(&name, &taken)
                    .and_then(|()| users::create_group(&name))
                    .map(|()| format!("Created group {name}."))
            }
            Cmd::DeleteGroup(name) => match self.users.groups.iter().find(|g| g.name == name) {
                Some(g) => {
                    users::can_delete_group(g, &self.users.login_defs, &self.users.primary_gids)
                        .and_then(|()| users::delete_group(&name))
                        .map(|()| format!("Deleted group {name}."))
                }
                None => denied("No such group."),
            },
            _ => unreachable!("system commands are handled in handle()"),
        };
        (self.emit)(Event::UsersStatus(result.unwrap_or_else(|e| e)));
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
        !watched || tx.send(Cmd::RefreshSystem).is_ok()
    });
}

/// UserAdded / UserDeleted on the manager and Changed on each user.
fn watch_accounts(conn: Connection, tx: Sender<Cmd>) {
    let rule = MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .sender("org.freedesktop.Accounts")
        .map(|b| b.build());
    watch(conn, "dials-admin-accounts-signals", rule, move |_| {
        tx.send(Cmd::RefreshUsers).is_ok()
    });
}
