//! UI side of the admin worker: starts it on first use, queues its events
//! and drains them on the Slint thread, and wires the System page.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};

use crate::AppWindow;
use crate::admin::{Admin, Cmd, Event};
use crate::system;

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

pub struct AdminPages {
    ui: slint::Weak<AppWindow>,
    admin: RefCell<Option<Admin>>,
    wake: Arc<Wake>,
    system: RefCell<SystemState>,
}

impl AdminPages {
    pub fn attach(ui: &AppWindow) -> Rc<Self> {
        let pages = Rc::new(AdminPages {
            ui: ui.as_weak(),
            admin: RefCell::new(None),
            wake: Arc::new(Wake::default()),
            system: RefCell::new(SystemState::default()),
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

        pages
    }

    pub fn section_shown(&self, section: i32) {
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
