//! Wallpaper: paints each Displays tile with the background its output
//! resolves to and adds a Wallpaper field to the inspector.
//!
//! Reads come from the appearance-profiles registry; writes go only through
//! the `lmtt` CLI (`io`). The display at the origin (0,0 in the editor)
//! owns the global wallpaper: choosing there writes `[background]` and
//! strips any override that output had. Any other display gets an Override
//! checkbox; ticked, Choose… writes that output's own rule, unticked removes
//! it. Everything is immediate: the registry is separate from the profile
//! the Displays page saves.
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use slint::{ComponentHandle, Rgba8Pixel, SharedPixelBuffer};

use crate::pages::displays::{self, TileIdentity};
use crate::{AppState, AppWindow};
use io::{LmttJob, OutputInfo};

pub mod io;

/// Longest edge of a decoded preview; tiles and the inspector share them.
const PREVIEW_EDGE: u32 = 512;

#[derive(Default)]
pub struct State {
    current: Option<io::Current>,
    /// Decoded previews by source path (UI thread only).
    images: HashMap<PathBuf, slint::Image>,
    /// Paths a worker is decoding.
    pending: HashSet<PathBuf>,
    /// Selectors whose Override box is ticked before an image was chosen.
    override_on: HashSet<String>,
    /// In-flight chooser/apply job (0 = idle); one at a time.
    job: u64,
    job_seq: u64,
    preview_gen: Arc<AtomicU64>,
}

/// Bind the inspector field's callbacks and read the registry.
pub fn attach(ui: &AppWindow, state: &Rc<RefCell<AppState>>) {
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_wallpaper_choose(move || {
            let ui = ui_weak.unwrap();
            start_chooser(&ui, &state);
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_wallpaper_override_changed(move |on| {
            let ui = ui_weak.unwrap();
            set_override(&ui, &state, on);
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_wallpaper_picked(move |target, path| {
            let ui = ui_weak.unwrap();
            apply_picked(&ui, &state, target.as_str(), path.as_str());
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_wallpaper_job_done(move |job, _ok, msg| {
            let ui = ui_weak.unwrap();
            finish_job(&ui, &state, job as u64, msg.to_string());
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_wallpaper_decoded(move |path, image| {
            let ui = ui_weak.unwrap();
            decoded(&ui, &state, Path::new(path.as_str()), image);
        });
    }
    reload(ui, state);
    on_canvas_updated(ui, state);
    on_selection_changed(ui, state);
}

/// Displays rebuilt its tiles: paint the ones whose preview is decoded.
pub fn on_canvas_updated(ui: &AppWindow, state: &Rc<RefCell<AppState>>) {
    let ids = displays::canvas_identities(&state.borrow().displays);
    if ids
        .iter()
        .any(|id| resolved_for(&state.borrow(), id).is_none())
        && state.borrow().wallpaper.current.is_some()
    {
        // A plugged output the last read did not know; re-read cheaply.
        reload(ui, state);
    }
    let st = state.borrow();
    for id in &ids {
        let image = resolved_for(&st, id)
            .and_then(|r| r.path.as_ref())
            .and_then(|p| st.wallpaper.images.get(p).cloned());
        displays::set_tile_image(&st.displays, id.row, image);
    }
}

/// Displays changed the selected monitor: refill the inspector field.
pub fn on_selection_changed(ui: &AppWindow, state: &Rc<RefCell<AppState>>) {
    let st = state.borrow();
    let Some(id) = displays::selected_identity(&st.displays) else {
        ui.set_wallpaper_enabled(false);
        return;
    };
    let Some(cur) = st.wallpaper.current.as_ref() else {
        ui.set_wallpaper_enabled(false);
        return;
    };
    let selector = io::selector_for(&id.connector, &id.description);
    let resolved = resolved_for(&st, &id).cloned().unwrap_or_default();
    let own = !resolved.override_keys.is_empty();
    let override_on = !id.origin && (own || st.wallpaper.override_on.contains(&selector));
    // Before an image is chosen, a ticked box shows what the display has now.
    let shown = if !id.origin && override_on && !own {
        cur.global.clone()
    } else {
        resolved.clone()
    };
    let name = shown
        .path
        .as_ref()
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let note = if id.origin {
        let mut n = String::from(
            "Global wallpaper: every display without its own uses it, and lmtt derives the palette from it.",
        );
        if own {
            n.push_str(" This display also has its own override; choosing replaces it.");
        }
        n
    } else if own {
        "Own wallpaper for this display.".to_string()
    } else if override_on {
        "Still showing the global wallpaper; choose an image for this display.".to_string()
    } else {
        "Uses the global wallpaper, set on the origin display.".to_string()
    };
    let image = shown
        .path
        .as_ref()
        .and_then(|p| st.wallpaper.images.get(p).cloned());
    ui.set_wallpaper_enabled(true);
    ui.set_wallpaper_is_origin(id.origin);
    ui.set_wallpaper_override(override_on);
    ui.set_wallpaper_has_image(image.is_some());
    ui.set_wallpaper_image(image.unwrap_or_default());
    ui.set_wallpaper_name(name.into());
    ui.set_wallpaper_path(
        shown
            .path
            .as_ref()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default()
            .into(),
    );
    ui.set_wallpaper_note(note.into());
    ui.set_wallpaper_can_apply(io::lmtt_available());
    if !io::lmtt_available() {
        ui.set_wallpaper_status("lmtt is not installed – needed to set the wallpaper.".into());
    } else if let Some(e) = &cur.error {
        ui.set_wallpaper_status(e.clone().into());
    }
}

fn resolved_for<'a>(st: &'a AppState, id: &TileIdentity) -> Option<&'a io::Resolved> {
    st.wallpaper
        .current
        .as_ref()?
        .outputs
        .iter()
        .find(|(o, _)| o.connector == id.connector && o.description == id.description)
        .map(|(_, r)| r)
}

/// Re-read the registry against the live outputs and decode new previews.
fn reload(ui: &AppWindow, state: &Rc<RefCell<AppState>>) {
    let outputs: Vec<OutputInfo> = state
        .borrow()
        .displays
        .live
        .iter()
        .map(|m| OutputInfo {
            connector: m.name.clone(),
            description: m.description.clone(),
            x: m.x,
            y: m.y,
        })
        .collect();
    let current = io::current(&outputs);
    let mut paths: Vec<PathBuf> = Vec::new();
    for r in std::iter::once(&current.global).chain(current.outputs.iter().map(|(_, r)| r)) {
        if let Some(p) = &r.path
            && !paths.contains(p)
        {
            paths.push(p.clone());
        }
    }
    let (to_decode, generation, gen_cell) = {
        let mut st = state.borrow_mut();
        let ws = &mut st.wallpaper;
        ws.current = Some(current);
        let to_decode: Vec<PathBuf> = paths
            .into_iter()
            .filter(|p| !ws.images.contains_key(p) && !ws.pending.contains(p))
            .collect();
        ws.pending.extend(to_decode.iter().cloned());
        let generation = ws.preview_gen.fetch_add(1, Ordering::AcqRel) + 1;
        (to_decode, generation, ws.preview_gen.clone())
    };
    if to_decode.is_empty() {
        return;
    }
    let ui_weak = ui.as_weak();
    std::thread::Builder::new()
        .name("wallpaper-preview".into())
        .spawn(move || {
            for path in to_decode {
                if gen_cell.load(Ordering::Acquire) != generation {
                    return;
                }
                let decoded = io::decode_preview(&path, PREVIEW_EDGE).ok();
                let key = path.to_string_lossy().into_owned();
                let sent = ui_weak.upgrade_in_event_loop(move |ui| {
                    let image = match decoded {
                        Some((w, h, rgba)) => {
                            slint::Image::from_rgba8(
                                SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(&rgba, w, h),
                            )
                        }
                        None => slint::Image::default(),
                    };
                    ui.invoke_wallpaper_decoded(key.into(), image);
                });
                if sent.is_err() {
                    return;
                }
            }
        })
        .ok();
}

/// A preview arrived on the UI thread: cache it and repaint.
fn decoded(ui: &AppWindow, state: &Rc<RefCell<AppState>>, path: &Path, image: slint::Image) {
    {
        let mut st = state.borrow_mut();
        st.wallpaper.pending.remove(path);
        if image.size().width > 0 {
            st.wallpaper.images.insert(path.to_path_buf(), image);
        }
    }
    on_canvas_updated(ui, state);
    on_selection_changed(ui, state);
}

/// Claim the single job slot; None while another job runs.
fn begin_job(ui: &AppWindow, state: &Rc<RefCell<AppState>>, status: &str) -> Option<u64> {
    let mut st = state.borrow_mut();
    if st.wallpaper.job != 0 {
        return None;
    }
    st.wallpaper.job_seq += 1;
    st.wallpaper.job = st.wallpaper.job_seq;
    ui.set_wallpaper_busy(true);
    ui.set_wallpaper_status(status.into());
    Some(st.wallpaper.job)
}

/// Choose… on the selected display: open the portal chooser for its target
/// ("" = the global wallpaper, else the output's selector).
fn start_chooser(ui: &AppWindow, state: &Rc<RefCell<AppState>>) {
    let (target, start_dir) = {
        let st = state.borrow();
        let Some(id) = displays::selected_identity(&st.displays) else {
            return;
        };
        let target = if id.origin {
            String::new()
        } else {
            io::selector_for(&id.connector, &id.description)
        };
        let start_dir = resolved_for(&st, &id)
            .and_then(|r| r.path.as_ref())
            .and_then(|p| p.parent().map(Path::to_path_buf));
        (target, start_dir)
    };
    let Some(job) = begin_job(ui, state, "Choosing…") else {
        return;
    };
    let ui_weak = ui.as_weak();
    std::thread::Builder::new()
        .name("wallpaper-chooser".into())
        .spawn(move || {
            let (picked, ok, msg) = match async_io::block_on(pick_image(start_dir)) {
                Ok(Some(path)) => (Some(path), true, String::new()),
                Ok(None) => (None, true, String::new()),
                Err(e) => (None, false, format!("File chooser: {e}")),
            };
            ui_weak
                .upgrade_in_event_loop(move |ui| match picked {
                    Some(path) => ui.invoke_wallpaper_picked(
                        target.into(),
                        path.to_string_lossy().into_owned().into(),
                    ),
                    None => ui.invoke_wallpaper_job_done(job as i32, ok, msg.into()),
                })
                .ok();
        })
        .ok();
}

/// org.freedesktop.portal.FileChooser.OpenFile → the chosen local file.
/// Ok(None) on cancel. No parent-window handle yet (Slint exposes none).
async fn pick_image(start_dir: Option<PathBuf>) -> Result<Option<PathBuf>, String> {
    use ashpd::desktop::file_chooser::{FileFilter, SelectedFiles};
    let build = || {
        SelectedFiles::open_file()
            .title("Choose a wallpaper")
            .accept_label("Set wallpaper")
            .modal(true)
            .multiple(false)
            .filter(
                FileFilter::new("Images")
                    .mimetype("image/png")
                    .mimetype("image/jpeg")
                    .mimetype("image/webp"),
            )
    };
    let request = match &start_dir {
        Some(dir) => build()
            .current_folder(dir.as_path())
            .unwrap_or_else(|_| build()),
        None => build(),
    };
    match request.send().await.and_then(|r| r.response()) {
        Ok(files) => {
            let Some(uri) = files.uris().first() else {
                return Ok(None);
            };
            let url = url::Url::parse(uri.as_str()).map_err(|e| e.to_string())?;
            url.to_file_path()
                .map(Some)
                .map_err(|_| format!("not a local file: {uri}"))
        }
        Err(ashpd::Error::Response(ashpd::desktop::ResponseError::Cancelled)) => Ok(None),
        Err(e) => Err(e.to_string()),
    }
}

/// Run one lmtt job on a worker thread under an already-claimed job id.
fn start_job(ui: &AppWindow, job_id: u64, job: LmttJob, done: String) {
    let ui_weak = ui.as_weak();
    std::thread::Builder::new()
        .name("wallpaper-apply".into())
        .spawn(move || {
            let lmtt = Path::new("lmtt");
            // lmtt's own theme_state first; the token file slint-kit reads is
            // written by every `lmtt switch`, so it only differs on a box that
            // never switched. run_job_with refuses anything but dark|light.
            let mode = io::current_mode_with(lmtt)
                .unwrap_or_else(|| slint_kit::load_tokens().mode.to_ascii_lowercase());
            let (ok, msg) = match io::run_job_with(lmtt, &job, &mode) {
                Ok(()) => (true, done),
                Err(e) => (false, e.to_string()),
            };
            ui_weak
                .upgrade_in_event_loop(move |ui| {
                    ui.invoke_wallpaper_job_done(job_id as i32, ok, msg.into())
                })
                .ok();
        })
        .ok();
}

/// The chooser picked a file for `target` ("" = global).
fn apply_picked(ui: &AppWindow, state: &Rc<RefCell<AppState>>, target: &str, path: &str) {
    let (job_id, job, done) = {
        let st = state.borrow();
        let job_id = st.wallpaper.job;
        if job_id == 0 {
            return;
        }
        if st.wallpaper.current.is_none() {
            return;
        }
        let path = PathBuf::from(path);
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if target.is_empty() {
            // The origin owns the global: drop any override it carried.
            let unset = displays::selected_identity(&st.displays)
                .filter(|id| id.origin)
                .and_then(|id| resolved_for(&st, &id).map(|r| r.override_keys.clone()))
                .unwrap_or_default();
            (
                job_id,
                LmttJob {
                    set: Some((path, None)),
                    unset,
                },
                format!("Wallpaper set: {name}"),
            )
        } else {
            (
                job_id,
                LmttJob {
                    set: Some((path, Some(target.to_string()))),
                    unset: Vec::new(),
                },
                format!("Wallpaper set for this display: {name}"),
            )
        }
    };
    ui.set_wallpaper_status("Applying…".into());
    start_job(ui, job_id, job, done);
}

/// The Override box: on reveals the selector and writes nothing; off
/// removes this display's own rule so it follows the global again.
fn set_override(ui: &AppWindow, state: &Rc<RefCell<AppState>>, on: bool) {
    let (selector, keys) = {
        let st = state.borrow();
        let Some(id) = displays::selected_identity(&st.displays) else {
            return;
        };
        if id.origin {
            return;
        }
        let keys = resolved_for(&st, &id)
            .map(|r| r.override_keys.clone())
            .unwrap_or_default();
        (io::selector_for(&id.connector, &id.description), keys)
    };
    if on {
        state.borrow_mut().wallpaper.override_on.insert(selector);
        on_selection_changed(ui, state);
        return;
    }
    state.borrow_mut().wallpaper.override_on.remove(&selector);
    if keys.is_empty() {
        on_selection_changed(ui, state);
        return;
    }
    let Some(job_id) = begin_job(ui, state, "Applying…") else {
        return;
    };
    start_job(
        ui,
        job_id,
        LmttJob {
            set: None,
            unset: keys,
        },
        "This display uses the global wallpaper again.".into(),
    );
}

fn finish_job(ui: &AppWindow, state: &Rc<RefCell<AppState>>, job_id: u64, msg: String) {
    {
        let mut st = state.borrow_mut();
        if st.wallpaper.job != job_id {
            return;
        }
        st.wallpaper.job = 0;
    }
    ui.set_wallpaper_busy(false);
    ui.set_wallpaper_status("".into());
    reload(ui, state);
    on_canvas_updated(ui, state);
    on_selection_changed(ui, state);
    if !msg.is_empty() {
        ui.set_wallpaper_status(msg.into());
    }
}
