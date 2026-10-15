//! Users: local accounts through AccountsService (add, delete, full name,
//! picture, administrator, disable login, passwords) and group membership
//! through pkexec. An administrator sees every account; a standard user sees
//! only their own. Writes go through the admin worker, since each one may
//! wait on a polkit prompt.
use std::cell::RefCell;
use std::rc::Rc;

use slint::{ComponentHandle, ModelRc, SharedString, VecModel};

use crate::admin::{Cmd, UsersCmd, UsersEvent};
use crate::{AppState, AppWindow, GroupCheck, UserRow, Users};

pub mod accounts;
pub mod passwd_pty;

#[derive(Default)]
pub struct State {
    snapshot: accounts::Snapshot,
    /// Accounts in row order (all for an admin, just yours otherwise).
    shown: Vec<accounts::Account>,
    selected_uid: Option<u32>,
}

/// Bind the page's callbacks. Nothing is read until the page is opened.
pub fn attach(ui: &AppWindow, state: &Rc<RefCell<AppState>>) {
    let g = ui.global::<Users>();
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        g.on_select(move |idx| {
            let ui = ui_weak.unwrap();
            {
                let mut st = state.borrow_mut();
                st.users.selected_uid = st.users.shown.get(idx as usize).map(|a| a.uid);
            }
            let g = ui.global::<Users>();
            g.set_confirm_delete(false);
            clear_passwords(&g);
            push_detail(&ui, &state, true);
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        g.on_apply_real(move |name| {
            let ui = ui_weak.unwrap();
            let name = name.trim().to_string();
            match (accounts::validate_real_name(&name), selected_uid(&state)) {
                (Err(e), _) => set_status(&ui, e),
                (Ok(()), Some(uid)) => write(&ui, &state, UsersCmd::SetRealName(uid, name)),
                (Ok(()), None) => {}
            }
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        g.on_apply_icon(move |path| {
            if let Some(uid) = selected_uid(&state) {
                let cmd = UsersCmd::SetIcon(uid, expand_home(path.trim()));
                write(&ui_weak.unwrap(), &state, cmd);
            }
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        g.on_set_admin(move |admin| {
            if let Some(uid) = selected_uid(&state) {
                write(&ui_weak.unwrap(), &state, UsersCmd::SetAdmin(uid, admin));
            }
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        g.on_set_locked(move |locked| {
            if let Some(uid) = selected_uid(&state) {
                write(&ui_weak.unwrap(), &state, UsersCmd::SetLocked(uid, locked));
            }
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        g.on_change_password(move || change_password(&ui_weak.unwrap(), &state));
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        g.on_delete_user(move || {
            let ui = ui_weak.unwrap();
            if let Some(uid) = selected_uid(&state) {
                let remove_home = ui.global::<Users>().get_remove_home();
                write(&ui, &state, UsersCmd::DeleteUser { uid, remove_home });
            }
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        g.on_create_user(move || create_user(&ui_weak.unwrap(), &state));
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        g.on_toggle_group(move |group, member| {
            if let Some(uid) = selected_uid(&state) {
                let cmd = UsersCmd::SetGroupMember {
                    uid,
                    group: group.to_string(),
                    member,
                };
                write(&ui_weak.unwrap(), &state, cmd);
            }
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        g.on_create_group(move || {
            let ui = ui_weak.unwrap();
            let g = ui.global::<Users>();
            let name = g.get_new_group().trim().to_string();
            let taken: Vec<String> = state
                .borrow()
                .users
                .snapshot
                .groups
                .iter()
                .map(|g| g.name.clone())
                .collect();
            match accounts::validate_username(&name, &taken) {
                Ok(()) => {
                    g.set_new_group("".into());
                    write(&ui, &state, UsersCmd::CreateGroup(name));
                }
                Err(e) => set_status(
                    &ui,
                    e.replace("Usernames", "Group names")
                        .replace("username", "group name"),
                ),
            }
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        g.on_delete_group(move |name| {
            write(
                &ui_weak.unwrap(),
                &state,
                UsersCmd::DeleteGroup(name.to_string()),
            );
        });
    }
}

/// The page was opened: re-read the accounts.
pub fn enter(_ui: &AppWindow, state: &Rc<RefCell<AppState>>) {
    send(state, UsersCmd::Refresh);
}

pub fn on_event(ui: &AppWindow, state: &Rc<RefCell<AppState>>, event: UsersEvent) {
    match event {
        UsersEvent::Snapshot(snap) => {
            state.borrow_mut().users.snapshot = snap;
            push_users(ui, state);
        }
        UsersEvent::Status(text) => set_status(ui, text),
        UsersEvent::Created(uid) => state.borrow_mut().users.selected_uid = Some(uid),
    }
}

fn send(state: &Rc<RefCell<AppState>>, cmd: UsersCmd) {
    state.borrow_mut().admin.send(Cmd::Users(cmd));
}

/// A write: say so first, since the polkit prompt may take a while.
fn write(ui: &AppWindow, state: &Rc<RefCell<AppState>>, cmd: UsersCmd) {
    set_status(ui, "Waiting for authorization…".into());
    send(state, cmd);
}

fn set_status(ui: &AppWindow, text: String) {
    ui.global::<Users>().set_status_text(text.into());
}

fn selected_uid(state: &Rc<RefCell<AppState>>) -> Option<u32> {
    state.borrow().users.selected_uid
}

fn me_admin(snap: &accounts::Snapshot, me_uid: u32) -> bool {
    snap.accounts.iter().any(|a| a.uid == me_uid && a.admin)
}

fn push_users(ui: &AppWindow, state: &Rc<RefCell<AppState>>) {
    {
        let mut app = state.borrow_mut();
        let me_uid = app.admin.me_uid;
        let st = &mut app.users;
        let me_admin = me_admin(&st.snapshot, me_uid);
        st.shown = st
            .snapshot
            .accounts
            .iter()
            .filter(|a| me_admin || a.uid == me_uid)
            .cloned()
            .collect();
        if !st.shown.iter().any(|a| Some(a.uid) == st.selected_uid) {
            st.selected_uid = st
                .shown
                .iter()
                .find(|a| a.uid == me_uid)
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
                meta: row_meta(a, me_uid).into(),
                initials: accounts::initials(&a.real_name, &a.name).into(),
                icon: load_icon(&a.icon),
            })
            .collect();
        let snap = &st.snapshot;
        let custom: Vec<SharedString> = snap
            .groups
            .iter()
            .filter(|g| accounts::is_custom_group(g, &snap.login_defs, &snap.primary_gids))
            .map(|g| g.name.as_str().into())
            .collect();
        let g = ui.global::<Users>();
        g.set_rows(ModelRc::new(VecModel::from(rows)));
        g.set_service(snap.service);
        g.set_me_admin(me_admin);
        g.set_custom_groups(ModelRc::new(VecModel::from(custom)));
    }
    push_detail(ui, state, false);
}

/// `reset_drafts`: a different account was picked, so edit fields follow
/// it; otherwise an in-progress edit is kept.
fn push_detail(ui: &AppWindow, state: &Rc<RefCell<AppState>>, reset_drafts: bool) {
    let app = state.borrow();
    let me_uid = app.admin.me_uid;
    let st = &app.users;
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
    let snap = &st.snapshot;
    let admins = snap.accounts.iter().filter(|x| x.admin).count();
    let allowed = accounts::allowed(a, me_uid, me_admin(snap, me_uid), admins, &snap.logged_in);
    let service = snap.service;

    if reset_drafts || g.get_real_draft() == g.get_real_name() || g.get_name() != a.name.as_str() {
        g.set_real_draft(a.real_name.as_str().into());
    }
    if reset_drafts || g.get_name() != a.name.as_str() {
        g.set_icon_draft("".into());
    }
    let checks: Vec<GroupCheck> =
        accounts::group_choices(&snap.groups, &snap.login_defs, &snap.primary_gids)
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

fn change_password(ui: &AppWindow, state: &Rc<RefCell<AppState>>) {
    let Some(uid) = selected_uid(state) else {
        return;
    };
    let g = ui.global::<Users>();
    let new = g.get_pw_new().to_string();
    let confirm = g.get_pw_confirm().to_string();
    let current = g.get_pw_current().to_string();
    clear_passwords(&g);
    if let Err(e) = accounts::validate_new_password(&new, &confirm) {
        for s in [new, confirm, current] {
            accounts::wipe_string(s);
        }
        return set_status(ui, e);
    }
    accounts::wipe_string(confirm);
    if g.get_needs_current() {
        write(ui, state, UsersCmd::ChangeOwnPassword { current, new });
    } else {
        accounts::wipe_string(current);
        write(ui, state, UsersCmd::SetPassword(uid, new));
    }
}

fn create_user(ui: &AppWindow, state: &Rc<RefCell<AppState>>) {
    let g = ui.global::<Users>();
    let name = g.get_new_name().trim().to_string();
    let real_name = g.get_new_real().trim().to_string();
    let password = g.get_new_pw().to_string();
    let confirm = g.get_new_pw_confirm().to_string();
    // Every passwd name counts, not just people: a new "ftp" must fail.
    let taken: Vec<String> = std::fs::read_to_string("/etc/passwd")
        .map(|t| {
            accounts::parse_passwd(&t)
                .into_iter()
                .map(|e| e.name)
                .collect()
        })
        .unwrap_or_default();
    let check = accounts::validate_username(&name, &taken)
        .and_then(|()| accounts::validate_real_name(&real_name))
        .and_then(|()| accounts::validate_new_password(&password, &confirm));
    accounts::wipe_string(confirm);
    if let Err(e) = check {
        accounts::wipe_string(password);
        return set_status(ui, e);
    }
    g.set_new_pw("".into());
    g.set_new_pw_confirm("".into());
    g.set_adding(false);
    let admin = g.get_new_admin();
    write(
        ui,
        state,
        UsersCmd::CreateUser {
            name,
            real_name,
            admin,
            password,
        },
    );
}

fn clear_passwords(g: &Users) {
    g.set_pw_current("".into());
    g.set_pw_new("".into());
    g.set_pw_confirm("".into());
}

fn row_meta(a: &accounts::Account, me_uid: u32) -> String {
    let mut parts = vec![if a.admin { "Admin" } else { "Standard" }];
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
