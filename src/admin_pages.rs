//! UI side of the admin worker: starts it on first use, queues its events
//! and drains them on the Slint thread, and wires the Users and System pages.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};

use crate::admin::{Admin, Cmd, Event};
use crate::{AppWindow, GroupCheck, UserRow, Users, system, users};

pub const SECTION_USERS: i32 = 4;
pub const SECTION_SYSTEM: i32 = 5;

/// Picker rows shown under a search box.
const MATCH_LIMIT: usize = 8;

/// Unlike telemetry (newest frame wins), every event matters: a status line
/// for each action, so this is a queue.
#[derive(Default)]
struct Wake {
    queue: Mutex<Vec<Event>>,
    pending: AtomicBool,
}

#[derive(Default)]
struct SystemState {
    snapshot: system::Snapshot,
    lists: system::Lists,
    lists_requested: bool,
}

#[derive(Default)]
struct UsersState {
    snapshot: users::Snapshot,
    /// Accounts in row order (all for an admin, just yours otherwise).
    shown: Vec<users::Account>,
    selected_uid: Option<u32>,
}

pub struct AdminPages {
    ui: slint::Weak<AppWindow>,
    admin: RefCell<Option<Admin>>,
    wake: Arc<Wake>,
    system: RefCell<SystemState>,
    users: RefCell<UsersState>,
    me_uid: u32,
}

impl AdminPages {
    pub fn attach(ui: &AppWindow) -> Rc<Self> {
        let pages = Rc::new(AdminPages {
            ui: ui.as_weak(),
            admin: RefCell::new(None),
            wake: Arc::new(Wake::default()),
            system: RefCell::new(SystemState::default()),
            users: RefCell::new(UsersState::default()),
            // SAFETY: getuid cannot fail.
            me_uid: unsafe { libc::getuid() },
        });

        let p = pages.clone();
        ui.on_admin_wake(move || p.drain());

        let p = pages.clone();
        ui.on_sys_apply_hostname(move |name| {
            let name = name.trim().to_string();
            match system::validate_static_hostname(&name) {
                Ok(()) => p.send_system(Cmd::SetStaticHostname(name)),
                Err(e) => p.set_status(e),
            }
        });
        let p = pages.clone();
        ui.on_sys_apply_pretty(move |name| {
            let name = name.trim().to_string();
            match system::validate_pretty_hostname(&name) {
                Ok(()) => p.send_system(Cmd::SetPrettyHostname(name)),
                Err(e) => p.set_status(e),
            }
        });
        let p = pages.clone();
        ui.on_sys_set_ntp(move |on| p.send_system(Cmd::SetNtp(on)));
        let p = pages.clone();
        ui.on_sys_query_changed(move |kind, query| p.update_matches(kind, &query));
        let p = pages.clone();
        ui.on_sys_picked(move |kind, idx| p.pick(kind, idx));

        Self::attach_users(&pages, ui);
        pages
    }

    pub fn section_shown(&self, section: i32) {
        if section == SECTION_USERS {
            self.send(Cmd::RefreshUsers);
            return;
        }
        if section != SECTION_SYSTEM {
            return;
        }
        self.send(Cmd::RefreshSystem);
        let mut st = self.system.borrow_mut();
        if !st.lists_requested {
            st.lists_requested = true;
            drop(st);
            self.send(Cmd::LoadSystemLists);
        }
    }

    fn send(&self, cmd: Cmd) {
        let mut admin = self.admin.borrow_mut();
        let admin = admin.get_or_insert_with(|| {
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
        admin.send(cmd);
    }

    /// A write: say so first, since the polkit prompt may take a while.
    fn send_system(&self, cmd: Cmd) {
        self.set_status("Waiting for authorization…".into());
        self.send(cmd);
    }

    fn set_status(&self, text: String) {
        if let Some(ui) = self.ui.upgrade() {
            ui.set_sys_status_text(text.into());
        }
    }

    fn drain(&self) {
        self.wake.pending.store(false, Ordering::Release);
        let events = std::mem::take(&mut *self.wake.queue.lock().expect("admin queue"));
        let Some(ui) = self.ui.upgrade() else {
            return;
        };
        for event in events {
            match event {
                Event::System(snap) => self.apply_snapshot(&ui, snap),
                Event::SystemLists(lists) => {
                    self.system.borrow_mut().lists = lists;
                    // Re-run any search typed before the lists arrived.
                    self.update_matches(0, &ui.get_sys_tz_query());
                    self.update_matches(1, &ui.get_sys_lang_query());
                    self.update_matches(2, &ui.get_sys_layout_query());
                }
                Event::SystemStatus(text) => ui.set_sys_status_text(text.into()),
                Event::Users(snap) => {
                    self.users.borrow_mut().snapshot = snap;
                    self.push_users(&ui);
                }
                Event::UsersStatus(text) => ui.global::<Users>().set_status_text(text.into()),
                Event::UserCreated(uid) => self.users.borrow_mut().selected_uid = Some(uid),
            }
        }
    }

    fn apply_snapshot(&self, ui: &AppWindow, snap: system::Snapshot) {
        let mut st = self.system.borrow_mut();
        // Follow the live value unless the user has an edit in progress.
        if ui.get_sys_hostname_draft() == st.snapshot.static_hostname.as_str() {
            ui.set_sys_hostname_draft(snap.static_hostname.as_str().into());
        }
        if ui.get_sys_pretty_draft() == st.snapshot.pretty_hostname.as_str() {
            ui.set_sys_pretty_draft(snap.pretty_hostname.as_str().into());
        }
        ui.set_sys_static_hostname(snap.static_hostname.as_str().into());
        ui.set_sys_pretty_hostname(snap.pretty_hostname.as_str().into());
        ui.set_sys_timezone(snap.timezone.as_str().into());
        ui.set_sys_can_ntp(snap.can_ntp);
        ui.set_sys_ntp(snap.ntp);
        ui.set_sys_ntp_synced(snap.ntp_synced);
        ui.set_sys_lang(system::lang_of(&snap.locale).into());
        let layout = match snap.x11_variant.as_str() {
            "" => snap.x11_layout.clone(),
            variant => format!("{} ({variant})", snap.x11_layout),
        };
        ui.set_sys_layout(layout.into());
        st.snapshot = snap;
    }

    fn update_matches(&self, kind: i32, query: &str) {
        let Some(ui) = self.ui.upgrade() else {
            return;
        };
        let st = self.system.borrow();
        let source = match kind {
            0 => &st.lists.timezones,
            1 => &st.lists.locales,
            _ => &st.lists.layouts,
        };
        let rows: Vec<SharedString> = system::matches(source, query, MATCH_LIMIT)
            .into_iter()
            .map(SharedString::from)
            .collect();
        let model = ModelRc::new(VecModel::from(rows));
        match kind {
            0 => ui.set_sys_tz_matches(model),
            1 => ui.set_sys_lang_matches(model),
            _ => ui.set_sys_layout_matches(model),
        }
    }

    fn pick(&self, kind: i32, idx: i32) {
        let Some(ui) = self.ui.upgrade() else {
            return;
        };
        let model = match kind {
            0 => ui.get_sys_tz_matches(),
            1 => ui.get_sys_lang_matches(),
            _ => ui.get_sys_layout_matches(),
        };
        let Some(value) = model.row_data(idx as usize).map(|v| v.to_string()) else {
            return;
        };
        let empty = ModelRc::new(VecModel::<SharedString>::default());
        match kind {
            0 => {
                ui.set_sys_tz_query("".into());
                ui.set_sys_tz_matches(empty);
                self.send_system(Cmd::SetTimezone(value));
            }
            1 => {
                ui.set_sys_lang_query("".into());
                ui.set_sys_lang_matches(empty);
                self.send_system(Cmd::SetLang(value));
            }
            _ => {
                ui.set_sys_layout_query("".into());
                ui.set_sys_layout_matches(empty);
                self.send_system(Cmd::SetLayout(value));
            }
        }
    }
}

impl AdminPages {
    fn attach_users(pages: &Rc<Self>, ui: &AppWindow) {
        let g = ui.global::<Users>();

        let p = pages.clone();
        g.on_select(move |idx| {
            let uid = p.users.borrow().shown.get(idx as usize).map(|a| a.uid);
            p.users.borrow_mut().selected_uid = uid;
            if let Some(ui) = p.ui.upgrade() {
                let g = ui.global::<Users>();
                g.set_confirm_delete(false);
                clear_passwords(&g);
                p.push_detail(&ui, true);
            }
        });
        let p = pages.clone();
        g.on_apply_real(move |name| {
            let name = name.trim().to_string();
            match (users::validate_real_name(&name), p.selected_uid()) {
                (Err(e), _) => p.set_users_status(e),
                (Ok(()), Some(uid)) => p.send_users(Cmd::SetRealName(uid, name)),
                (Ok(()), None) => {}
            }
        });
        let p = pages.clone();
        g.on_apply_icon(move |path| {
            if let Some(uid) = p.selected_uid() {
                p.send_users(Cmd::SetIcon(uid, expand_home(path.trim())));
            }
        });
        let p = pages.clone();
        g.on_set_admin(move |admin| {
            if let Some(uid) = p.selected_uid() {
                p.send_users(Cmd::SetAdmin(uid, admin));
            }
        });
        let p = pages.clone();
        g.on_set_locked(move |locked| {
            if let Some(uid) = p.selected_uid() {
                p.send_users(Cmd::SetLocked(uid, locked));
            }
        });
        let p = pages.clone();
        g.on_change_password(move || p.change_password());
        let p = pages.clone();
        g.on_delete_user(move || {
            let Some(ui) = p.ui.upgrade() else {
                return;
            };
            if let Some(uid) = p.selected_uid() {
                let remove_home = ui.global::<Users>().get_remove_home();
                p.send_users(Cmd::DeleteUser { uid, remove_home });
            }
        });
        let p = pages.clone();
        g.on_create_user(move || p.create_user());
        let p = pages.clone();
        g.on_toggle_group(move |group, member| {
            if let Some(uid) = p.selected_uid() {
                p.send_users(Cmd::SetGroupMember {
                    uid,
                    group: group.to_string(),
                    member,
                });
            }
        });
        let p = pages.clone();
        g.on_create_group(move || {
            let Some(ui) = p.ui.upgrade() else {
                return;
            };
            let g = ui.global::<Users>();
            let name = g.get_new_group().trim().to_string();
            let taken: Vec<String> = p
                .users
                .borrow()
                .snapshot
                .groups
                .iter()
                .map(|g| g.name.clone())
                .collect();
            match users::validate_username(&name, &taken) {
                Ok(()) => {
                    g.set_new_group("".into());
                    p.send_users(Cmd::CreateGroup(name));
                }
                Err(e) => p.set_users_status(
                    e.replace("Usernames", "Group names")
                        .replace("username", "group name"),
                ),
            }
        });
        let p = pages.clone();
        g.on_delete_group(move |name| p.send_users(Cmd::DeleteGroup(name.to_string())));
    }

    fn selected_uid(&self) -> Option<u32> {
        self.users.borrow().selected_uid
    }

    fn send_users(&self, cmd: Cmd) {
        self.set_users_status("Waiting for authorization…".into());
        self.send(cmd);
    }

    fn set_users_status(&self, text: String) {
        if let Some(ui) = self.ui.upgrade() {
            ui.global::<Users>().set_status_text(text.into());
        }
    }

    fn me_admin(&self) -> bool {
        let st = self.users.borrow();
        st.snapshot
            .accounts
            .iter()
            .any(|a| a.uid == self.me_uid && a.admin)
    }

    fn push_users(&self, ui: &AppWindow) {
        let me_admin = self.me_admin();
        let mut st = self.users.borrow_mut();
        st.shown = st
            .snapshot
            .accounts
            .iter()
            .filter(|a| me_admin || a.uid == self.me_uid)
            .cloned()
            .collect();
        if !st.shown.iter().any(|a| Some(a.uid) == st.selected_uid) {
            st.selected_uid = st
                .shown
                .iter()
                .find(|a| a.uid == self.me_uid)
                .or(st.shown.first())
                .map(|a| a.uid);
        }
        let rows: Vec<UserRow> = st
            .shown
            .iter()
            .map(|a| UserRow {
                uid: a.uid as i32,
                title: if a.real_name.is_empty() {
                    a.name.clone()
                } else {
                    a.real_name.clone()
                }
                .into(),
                meta: row_meta(a, self.me_uid).into(),
                initials: users::initials(&a.real_name, &a.name).into(),
                icon: load_icon(&a.icon),
            })
            .collect();
        let snap = &st.snapshot;
        let custom: Vec<SharedString> = snap
            .groups
            .iter()
            .filter(|g| users::is_custom_group(g, &snap.login_defs, &snap.primary_gids))
            .map(|g| g.name.as_str().into())
            .collect();
        let g = ui.global::<Users>();
        g.set_rows(ModelRc::new(VecModel::from(rows)));
        g.set_service(snap.service);
        g.set_me_admin(me_admin);
        g.set_custom_groups(ModelRc::new(VecModel::from(custom)));
        drop(st);
        self.push_detail(ui, false);
    }

    /// `reset_drafts`: a different account was picked, so edit fields follow
    /// it; otherwise an in-progress edit is kept.
    fn push_detail(&self, ui: &AppWindow, reset_drafts: bool) {
        let st = self.users.borrow();
        let g = ui.global::<Users>();
        let Some((idx, a)) = st
            .shown
            .iter()
            .enumerate()
            .find(|(_, a)| Some(a.uid) == st.selected_uid)
        else {
            g.set_selected(-1);
            return;
        };
        let me_admin = st
            .snapshot
            .accounts
            .iter()
            .any(|x| x.uid == self.me_uid && x.admin);
        let admins = st.snapshot.accounts.iter().filter(|x| x.admin).count();
        let allowed = users::allowed(a, self.me_uid, me_admin, admins, &st.snapshot.logged_in);
        let service = st.snapshot.service;

        if reset_drafts
            || g.get_real_draft() == g.get_real_name()
            || g.get_name() != a.name.as_str()
        {
            g.set_real_draft(a.real_name.as_str().into());
        }
        if reset_drafts || g.get_name() != a.name.as_str() {
            g.set_icon_draft("".into());
        }
        let snap = &st.snapshot;
        let checks: Vec<GroupCheck> =
            users::group_choices(&snap.groups, &snap.login_defs, &snap.primary_gids)
                .into_iter()
                .map(|grp| GroupCheck {
                    name: grp.name.as_str().into(),
                    member: grp.members.contains(&a.name),
                })
                .collect();
        g.set_groups(ModelRc::new(VecModel::from(checks)));
        g.set_selected(idx as i32);
        g.set_name(a.name.as_str().into());
        g.set_real_name(a.real_name.as_str().into());
        g.set_admin(a.admin);
        g.set_locked(a.locked);
        g.set_can_edit(service && allowed.edit_profile);
        g.set_can_admin(service && allowed.change_admin);
        g.set_can_lock(service && allowed.lock);
        // Self-change through passwd works without AccountsService.
        g.set_can_password(allowed.set_password && (service || allowed.needs_current_password));
        g.set_needs_current(allowed.needs_current_password);
        g.set_can_delete(service && allowed.delete);
        g.set_delete_reason(allowed.delete_reason.into());
    }

    fn change_password(&self) {
        let Some(ui) = self.ui.upgrade() else {
            return;
        };
        let Some(uid) = self.selected_uid() else {
            return;
        };
        let g = ui.global::<Users>();
        let new = g.get_pw_new().to_string();
        let confirm = g.get_pw_confirm().to_string();
        let current = g.get_pw_current().to_string();
        clear_passwords(&g);
        if let Err(e) = users::validate_new_password(&new, &confirm) {
            for s in [new, confirm, current] {
                users::wipe_string(s);
            }
            return self.set_users_status(e);
        }
        users::wipe_string(confirm);
        if g.get_needs_current() {
            self.send_users(Cmd::ChangeOwnPassword { current, new });
        } else {
            users::wipe_string(current);
            self.send_users(Cmd::SetPassword(uid, new));
        }
    }

    fn create_user(&self) {
        let Some(ui) = self.ui.upgrade() else {
            return;
        };
        let g = ui.global::<Users>();
        let name = g.get_new_name().trim().to_string();
        let real_name = g.get_new_real().trim().to_string();
        let password = g.get_new_pw().to_string();
        let confirm = g.get_new_pw_confirm().to_string();
        // Every passwd name counts, not just people: a new "ftp" must fail.
        let taken: Vec<String> = std::fs::read_to_string("/etc/passwd")
            .map(|t| {
                users::parse_passwd(&t)
                    .into_iter()
                    .map(|e| e.name)
                    .collect()
            })
            .unwrap_or_default();
        let check = users::validate_username(&name, &taken)
            .and_then(|()| users::validate_real_name(&real_name))
            .and_then(|()| users::validate_new_password(&password, &confirm));
        users::wipe_string(confirm);
        if let Err(e) = check {
            users::wipe_string(password);
            return self.set_users_status(e);
        }
        g.set_new_pw("".into());
        g.set_new_pw_confirm("".into());
        g.set_adding(false);
        self.send_users(Cmd::CreateUser {
            name,
            real_name,
            admin: g.get_new_admin(),
            password,
        });
    }
}

fn clear_passwords(g: &Users) {
    g.set_pw_current("".into());
    g.set_pw_new("".into());
    g.set_pw_confirm("".into());
}

fn row_meta(a: &users::Account, me_uid: u32) -> String {
    let mut parts = vec![if a.admin { "Administrator" } else { "Standard" }];
    if a.locked {
        parts.push("login disabled");
    }
    if a.uid == me_uid {
        parts.push("you");
    }
    parts.join(" · ")
}

fn load_icon(path: &str) -> slint::Image {
    if path.is_empty() {
        return slint::Image::default();
    }
    slint::Image::load_from_path(std::path::Path::new(path)).unwrap_or_default()
}

fn expand_home(path: &str) -> String {
    match (path.strip_prefix("~/"), std::env::var("HOME")) {
        (Some(rest), Ok(home)) => format!("{home}/{rest}"),
        _ => path.to_string(),
    }
}
