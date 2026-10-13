//! hyprstate daemon surface shared by the Power and Help pages: the
//! telemetry client and its edge-triggered wake into the Slint loop.
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{
    Arc, Mutex, Weak as SyncWeak,
    atomic::{AtomicBool, Ordering},
};

use slint::ComponentHandle;

use crate::{AppState, AppWindow};

pub mod telemetry;

#[derive(Default)]
struct Wake {
    latest: Mutex<Option<telemetry::HelpLive>>,
    pending: AtomicBool,
}

/// Start the telemetry client. Edge-triggered: the worker stores only the
/// newest frame and wakes Slint once. With no frame, input, animation, or
/// one-shot deadline, winit can block indefinitely.
pub fn attach(ui: &AppWindow, state: &Rc<RefCell<AppState>>) {
    let wake = Arc::new(Wake::default());
    {
        let wake = wake.clone();
        let state = state.clone();
        let ui_weak = ui.as_weak();
        ui.on_telemetry_wake(move || {
            let ui = ui_weak.unwrap();
            wake.pending.store(false, Ordering::Release);
            let Some(frame) = wake.latest.lock().expect("telemetry slot").take() else {
                return;
            };
            state.borrow_mut().hyprstate = frame;
            crate::pages::telemetry_frame(&ui, &state);
        });
    }
    let weak = Arc::downgrade(&wake);
    let ui_weak = ui.as_weak();
    telemetry::spawn(move |frame| queue_frame(&weak, &ui_weak, frame));
}

fn queue_frame(
    slot: &SyncWeak<Wake>,
    ui: &slint::Weak<AppWindow>,
    frame: telemetry::HelpLive,
) -> bool {
    let Some(slot) = slot.upgrade() else {
        return false;
    };
    *slot.latest.lock().expect("telemetry slot") = Some(frame);
    if !slot.pending.swap(true, Ordering::AcqRel) {
        let slot_for_error = slot.clone();
        if ui
            .upgrade_in_event_loop(move |ui| ui.invoke_telemetry_wake())
            .is_err()
        {
            slot_for_error.pending.store(false, Ordering::Release);
            return false;
        }
    }
    true
}
