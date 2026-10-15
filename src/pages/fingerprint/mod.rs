//! Fingerprint: enroll, test and delete your own fingers through fprintd,
//! and show where pam_fprintd is wired into PAM (read-only). A section of
//! the Users detail for the account dials runs as, entered through
//! `FingerprintSlot`; it stays hidden until fprintd reports a reader, so a
//! machine without one (or without fprintd) never sees it. fprintd does the
//! work: dials claims the reader only for the length of one action.
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use slint::{ComponentHandle, ModelRc, VecModel};

use crate::{AppState, AppWindow, FingerSlot, Fingerprint, PamRow};
use fprintd::{FINGERS, Probe, Reader, Tone};
use worker::{Cmd, Event};

pub mod fprintd;
pub mod pam;
mod worker;

#[cfg(test)]
mod bus_tests;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum Mode {
    #[default]
    Idle,
    /// A call is in flight (it may be waiting on a polkit prompt).
    Busy,
    Enroll,
    Verify,
}

impl Mode {
    fn as_str(self) -> &'static str {
        match self {
            Mode::Idle => "",
            Mode::Busy => "busy",
            Mode::Enroll => "enroll",
            Mode::Verify => "verify",
        }
    }
}

/// Worker events, queued and drained on the Slint thread.
#[derive(Default)]
struct Wake {
    queue: Mutex<Vec<Event>>,
    pending: AtomicBool,
}

#[derive(Default)]
pub struct State {
    worker: Option<worker::Handle>,
    wake: Arc<Wake>,
    reader: Option<Reader>,
    enrolled: Vec<String>,
    selected: Option<usize>,
    mode: Mode,
    /// Finger of the running enrollment, and scans counted so far.
    finger: String,
    stage: i32,
}

pub fn attach(ui: &AppWindow, state: &Rc<RefCell<AppState>>) {
    let g = ui.global::<Fingerprint>();
    g.set_available(fprintd::installed());
    g.set_user(current_user().unwrap_or_default().into());
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        g.on_wake(move || {
            let Some(ui) = ui_weak.upgrade() else {
                return;
            };
            let events = {
                let st = state.borrow();
                let w = &st.fingerprint.wake;
                w.pending.store(false, Ordering::Release);
                std::mem::take(&mut *w.queue.lock().expect("fingerprint queue"))
            };
            for event in events {
                on_event(&ui, &state, event);
            }
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        g.on_select(move |idx| {
            let ui = ui_weak.unwrap();
            state.borrow_mut().fingerprint.selected =
                usize::try_from(idx).ok().filter(|i| *i < FINGERS.len());
            let g = ui.global::<Fingerprint>();
            g.set_confirm_delete(false);
            push(&ui, &state);
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        g.on_enroll(move || {
            let ui = ui_weak.unwrap();
            if let Some(name) = selected_finger(&state) {
                begin(
                    &ui,
                    &state,
                    Cmd::Enroll(name.into()),
                    "Waiting for the reader…",
                );
            }
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        g.on_verify(move || {
            let ui = ui_weak.unwrap();
            begin(&ui, &state, Cmd::Verify, "Waiting for the reader…");
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        g.on_delete(move || {
            let ui = ui_weak.unwrap();
            if let Some(name) = selected_finger(&state) {
                begin(&ui, &state, Cmd::Delete(name.into()), "Deleting…");
            }
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        g.on_delete_all(move || {
            let ui = ui_weak.unwrap();
            begin(&ui, &state, Cmd::DeleteAll, "Deleting…");
        });
    }
    {
        let state = state.clone();
        g.on_cancel(move || cancel(&state));
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        g.on_account_changed(move |name| {
            let ui = ui_weak.unwrap();
            if name != ui.global::<Fingerprint>().get_user() {
                cancel(&state);
            }
        });
    }
}

/// The Users page was opened: look for the reader (starts fprintd, which
/// exits again when idle) and re-read PAM.
pub fn enter(ui: &AppWindow, state: &Rc<RefCell<AppState>>) {
    if ui.global::<Fingerprint>().get_available() {
        send(ui, state, Cmd::Refresh);
    }
}

/// The Users page was left: stop any scan so the reader is released.
pub fn leave(_ui: &AppWindow, state: &Rc<RefCell<AppState>>) {
    cancel(state);
}

fn cancel(state: &Rc<RefCell<AppState>>) {
    let st = state.borrow();
    if st.fingerprint.mode != Mode::Idle
        && let Some(w) = &st.fingerprint.worker
    {
        w.send(Cmd::Cancel);
    }
}

fn selected_finger(state: &Rc<RefCell<AppState>>) -> Option<&'static str> {
    let st = state.borrow();
    st.fingerprint.selected.map(|i| FINGERS[i].0)
}

fn begin(ui: &AppWindow, state: &Rc<RefCell<AppState>>, cmd: Cmd, status: &str) {
    {
        let mut st = state.borrow_mut();
        if st.fingerprint.mode != Mode::Idle {
            return;
        }
        st.fingerprint.mode = Mode::Busy;
    }
    let g = ui.global::<Fingerprint>();
    g.set_feedback("".into());
    g.set_status_text(status.into());
    push(ui, state);
    send(ui, state, cmd);
}

/// Send to the worker, starting it on first use.
fn send(ui: &AppWindow, state: &Rc<RefCell<AppState>>, cmd: Cmd) {
    let mut st = state.borrow_mut();
    let fp = &mut st.fingerprint;
    let wake = Arc::downgrade(&fp.wake);
    let ui_weak = ui.as_weak();
    let worker = fp.worker.get_or_insert_with(|| {
        worker::Handle::spawn(worker::Config::system(), move |event| {
            let Some(wake) = wake.upgrade() else {
                return;
            };
            wake.queue.lock().expect("fingerprint queue").push(event);
            if !wake.pending.swap(true, Ordering::AcqRel)
                && ui_weak
                    .upgrade_in_event_loop(|ui| ui.global::<Fingerprint>().invoke_wake())
                    .is_err()
            {
                wake.pending.store(false, Ordering::Release);
            }
        })
    });
    worker.send(cmd);
}

fn on_event(ui: &AppWindow, state: &Rc<RefCell<AppState>>, event: Event) {
    let g = ui.global::<Fingerprint>();
    match event {
        Event::Snapshot { probe, pam } => {
            {
                let mut st = state.borrow_mut();
                let fp = &mut st.fingerprint;
                match probe {
                    Probe::Ready { reader, enrolled } => {
                        fp.reader = Some(reader);
                        fp.enrolled = enrolled;
                    }
                    Probe::NoReader | Probe::Unreachable(_) => {
                        fp.reader = None;
                        fp.enrolled.clear();
                    }
                }
            }
            push_pam(ui, &pam);
        }
        Event::EnrollStarted { finger, stages } => {
            {
                let mut st = state.borrow_mut();
                let fp = &mut st.fingerprint;
                fp.mode = Mode::Enroll;
                fp.finger = finger;
                fp.stage = 0;
            }
            g.set_stages(stages.max(0));
            g.set_feedback("".into());
            g.set_status_text("".into());
        }
        Event::EnrollStatus { result, done } => {
            let (swipe, label) = {
                let st = state.borrow();
                let fp = &st.fingerprint;
                (
                    fp.reader.as_ref().is_some_and(|r| r.swipe),
                    fprintd::finger_label(&fp.finger).to_string(),
                )
            };
            let (tone, text) = fprintd::enroll_feedback(&result, swipe);
            {
                let mut st = state.borrow_mut();
                let fp = &mut st.fingerprint;
                match tone {
                    Tone::Progress => fp.stage += 1,
                    Tone::Done => fp.stage = g.get_stages().max(fp.stage + 1),
                    Tone::Retry | Tone::Failed => {}
                }
            }
            show_feedback(&g, tone, text);
            if done {
                g.set_status_text(format!("{label}: {text}").into());
            }
        }
        Event::VerifyStarted => {
            state.borrow_mut().fingerprint.mode = Mode::Verify;
            g.set_feedback("".into());
            g.set_status_text("".into());
        }
        Event::VerifyStatus { result, done } => {
            let swipe = state
                .borrow()
                .fingerprint
                .reader
                .as_ref()
                .is_some_and(|r| r.swipe);
            let (tone, text) = fprintd::verify_feedback(&result, swipe);
            show_feedback(&g, tone, text);
            if done {
                g.set_status_text(format!("Test: {text}").into());
            }
        }
        Event::Idle => state.borrow_mut().fingerprint.mode = Mode::Idle,
        Event::Status(text) => g.set_status_text(text.into()),
    }
    push(ui, state);
}

fn show_feedback(g: &Fingerprint<'_>, tone: Tone, text: &str) {
    g.set_feedback(text.into());
    g.set_tone(
        match tone {
            Tone::Progress => "progress",
            Tone::Retry => "retry",
            Tone::Done => "done",
            Tone::Failed => "failed",
        }
        .into(),
    );
}

/// Reader, fingers and mode into the global.
fn push(ui: &AppWindow, state: &Rc<RefCell<AppState>>) {
    let st = state.borrow();
    let fp = &st.fingerprint;
    let g = ui.global::<Fingerprint>();
    g.set_ready(fp.reader.is_some());
    if let Some(r) = &fp.reader {
        g.set_reader(r.name.as_str().into());
        g.set_swipe(r.swipe);
    }
    let slots: Vec<FingerSlot> = FINGERS
        .iter()
        .enumerate()
        .map(|(i, (name, label))| FingerSlot {
            index: i as i32,
            label: (*label).into(),
            enrolled: fp.enrolled.iter().any(|e| e == name),
        })
        .collect();
    let selected = fp.selected.and_then(|i| slots.get(i));
    g.set_selected(fp.selected.map_or(-1, |i| i as i32));
    g.set_selected_label(selected.map(|s| s.label.clone()).unwrap_or_default());
    g.set_selected_enrolled(selected.is_some_and(|s| s.enrolled));
    let (left, right) = slots.split_at(5);
    g.set_left(ModelRc::new(VecModel::from(left.to_vec())));
    g.set_right(ModelRc::new(VecModel::from(right.to_vec())));
    g.set_enrolled_count(fp.enrolled.len() as i32);
    g.set_mode(fp.mode.as_str().into());
    g.set_finger(fprintd::finger_label(&fp.finger).into());
    g.set_stage(fp.stage);
}

fn push_pam(ui: &AppWindow, pam: &pam::Status) {
    let g = ui.global::<Fingerprint>();
    g.set_pam_module(pam.module.is_some());
    let rows: Vec<PamRow> = pam
        .stacks
        .iter()
        .map(|s| {
            let (badge, detail) = pam::describe(&s.wiring);
            PamRow {
                label: s.label.as_str().into(),
                on: s.wiring != pam::Wiring::Absent,
                badge: badge.into(),
                detail: detail.into(),
            }
        })
        .collect();
    g.set_pam_rows(ModelRc::new(VecModel::from(rows)));
    g.set_pam_note(pam_note(pam).into());
}

fn pam_note(pam: &pam::Status) -> String {
    match &pam.authselect {
        Some(a) if a.fingerprint => format!(
            "authselect profile \"{}\" has with-fingerprint on.",
            a.profile
        ),
        Some(a) => format!(
            "authselect profile \"{}\" without with-fingerprint: `authselect enable-feature with-fingerprint` turns it on.",
            a.profile
        ),
        None => String::new(),
    }
}

/// The login name dials runs as.
fn current_user() -> Option<String> {
    // SAFETY: getpwuid returns a pointer into static storage, read at once
    // on the UI thread; nothing else in dials calls it concurrently.
    unsafe {
        let pw = libc::getpwuid(libc::getuid());
        if pw.is_null() || (*pw).pw_name.is_null() {
            return None;
        }
        Some(
            std::ffi::CStr::from_ptr((*pw).pw_name)
                .to_string_lossy()
                .into_owned(),
        )
    }
}

/// `dials --fingerprint`: what the section would show, and why it is hidden
/// when it is.
pub fn print_status() -> Result<(), Box<dyn std::error::Error>> {
    println!(
        "fprintd activation file: {}",
        if fprintd::installed() {
            "present"
        } else {
            "missing"
        }
    );
    match zbus::blocking::Connection::system() {
        Ok(conn) => match fprintd::probe(&conn) {
            Probe::Ready { reader, enrolled } => {
                println!(
                    "reader: {} ({}, {} enroll stages)",
                    reader.name,
                    if reader.swipe { "swipe" } else { "press" },
                    reader.stages
                );
                let names: Vec<&str> = enrolled.iter().map(|e| fprintd::finger_label(e)).collect();
                println!(
                    "enrolled: {}",
                    if names.is_empty() {
                        "none".to_string()
                    } else {
                        names.join(", ")
                    }
                );
            }
            Probe::NoReader => println!("reader: none detected (section hidden)"),
            Probe::Unreachable(why) => println!("fprintd: {why} (section hidden)"),
        },
        Err(e) => println!("system bus: {e} (section hidden)"),
    }
    let pam = pam::read(std::path::Path::new("/"));
    match &pam.module {
        Some(p) => println!("pam_fprintd: {}", p.display()),
        None => println!("pam_fprintd: not installed"),
    }
    for s in &pam.stacks {
        let (badge, detail) = pam::describe(&s.wiring);
        println!("  {:<20} {badge:<4} {detail}", s.name);
    }
    let note = pam_note(&pam);
    if !note.is_empty() {
        println!("{note}");
    }
    Ok(())
}
