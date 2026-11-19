use gtk::{cairo, glib, prelude::*};
use std::{
    cell::RefCell,
    path::{Path, PathBuf},
    rc::Rc,
    time::Instant,
};
use webkit6::{Download, NetworkSession};

const BAR_HEIGHT: f64 = 3.0;

const HOLD_SECS: f64 = 0.45;
const RED_HOLD_SECS: f64 = 0.30;
const FADE_SECS: f64 = 0.26;

const BLUE: (f64, f64, f64) = (0.20, 0.46, 0.96);
const RED: (f64, f64, f64) = (0.85, 0.26, 0.23);

struct Active {
    download: Download,
    received: u64,
    total: Option<u64>,
    finished: bool,
}

#[derive(Clone, Copy)]
struct Finish {
    started: Instant,
    red: bool,
}

struct Model {
    area: glib::object::WeakRef<gtk::DrawingArea>,
    hit: glib::object::WeakRef<gtk::Box>,
    active: Vec<Active>,
    finish: Option<Finish>,

    progress: f64,
    indeterminate: bool,
    error: bool,
    hover: bool,
    epoch: Instant,
    ticking: bool,
}

impl Model {
    fn recompute(&mut self) {
        let mut known = !self.active.is_empty();
        let mut received = 0u64;
        let mut total = 0u64;
        for item in &self.active {
            received += item.received;
            match item.total {
                Some(value) if value > 0 => total += value,
                _ => known = false,
            }
        }
        if !self.active.is_empty() && self.active.iter().all(|item| item.finished) {
            self.progress = 1.0;
            self.indeterminate = false;
        } else if known && total > 0 {
            self.progress = (received as f64 / total as f64).clamp(0.0, 0.999);
            self.indeterminate = false;
        } else {
            self.progress = 0.0;
            self.indeterminate = !self.active.is_empty();
        }
    }

    fn advance(&mut self, now: Instant) {
        if !self.active.is_empty()
            && self.active.iter().all(|item| item.finished)
            && self.finish.is_none()
        {
            self.recompute();
            self.finish = Some(Finish {
                started: now,
                red: self.error,
            });
        }
        if let Some(finish) = self.finish {
            let hold = if finish.red { RED_HOLD_SECS } else { HOLD_SECS };
            if now.saturating_duration_since(finish.started).as_secs_f64() >= hold + FADE_SECS {
                self.reset();
            }
        }
    }

    fn reset(&mut self) {
        self.active.clear();
        self.finish = None;

        self.progress = 0.0;
        self.indeterminate = false;
        self.error = false;
        self.hover = false;
    }

    fn animating(&self, now: Instant) -> bool {
        if let Some(finish) = self.finish {
            let hold = if finish.red { RED_HOLD_SECS } else { HOLD_SECS };
            if now.saturating_duration_since(finish.started).as_secs_f64() < hold + FADE_SECS {
                return true;
            }
        }
        self.indeterminate
    }

    fn visible(&self) -> bool {
        self.finish.is_some() || !self.active.is_empty()
    }

    fn alpha(&self, now: Instant) -> f64 {
        let Some(finish) = self.finish else {
            return 1.0;
        };
        let hold = if finish.red { RED_HOLD_SECS } else { HOLD_SECS };
        let age = now.saturating_duration_since(finish.started).as_secs_f64();
        if age <= hold {
            1.0
        } else {
            (1.0 - (age - hold) / FADE_SECS).clamp(0.0, 1.0)
        }
    }

    fn red(&self) -> f64 {
        if self.hover && !self.active.is_empty()
            || self.error
            || self.finish.is_some_and(|finish| finish.red)
        {
            1.0
        } else {
            0.0
        }
    }
}

fn mix(a: (f64, f64, f64), b: (f64, f64, f64), t: f64) -> (f64, f64, f64) {
    (
        a.0 + (b.0 - a.0) * t,
        a.1 + (b.1 - a.1) * t,
        a.2 + (b.2 - a.2) * t,
    )
}

fn download_dir() -> PathBuf {
    glib::user_special_dir(glib::UserDirectory::Downloads)
        .filter(|path| path.is_dir())
        .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("."))
}

fn unique_path(dir: &Path, suggested: &str) -> PathBuf {
    let name = Path::new(suggested)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.trim_matches('.').is_empty())
        .unwrap_or("download");
    let candidate = dir.join(name);
    if !candidate.exists() {
        return candidate;
    }
    let path = Path::new(name);
    let stem = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("download");
    let extension = path.extension().and_then(|extension| extension.to_str());
    for index in 1..10_000 {
        let name = match extension {
            Some(extension) => format!("{stem} ({index}).{extension}"),
            None => format!("{stem} ({index})"),
        };
        let candidate = dir.join(name);
        if !candidate.exists() {
            return candidate;
        }
    }
    dir.join(name)
}

fn kick(model: &Rc<RefCell<Model>>) {
    let Some(area) = model.borrow().area.upgrade() else {
        return;
    };
    if model.borrow().ticking {
        return;
    }
    model.borrow_mut().ticking = true;
    let weak = Rc::downgrade(model);
    area.add_tick_callback(move |area, _| {
        let Some(model) = weak.upgrade() else {
            return glib::ControlFlow::Break;
        };
        let now = Instant::now();
        let width = f64::from(area.width());
        let mapped = area.is_mapped() && width > 0.0;
        let (animating, visible) = {
            let mut state = model.borrow_mut();
            if mapped {
                state.advance(now);
            }
            (state.animating(now), state.visible())
        };
        area.set_visible(visible);
        if visible {
            area.queue_draw();
        }
        let hit = model.borrow().hit.upgrade();
        if let Some(hit) = &hit {
            hit.set_visible(visible && !model.borrow().active.is_empty());
        }
        if !animating {
            model.borrow_mut().ticking = false;
            return glib::ControlFlow::Break;
        }
        glib::ControlFlow::Continue
    });
}

fn enqueue(model: &Rc<RefCell<Model>>, download: &Download) {
    let download = download.clone();
    let dir = download_dir();
    download.connect_decide_destination(move |download, suggested| {
        let _ = std::fs::create_dir_all(&dir);
        let path = unique_path(&dir, suggested);
        download.set_destination(path.to_string_lossy().as_ref());
        true
    });
    attach(model, &download);
    {
        let mut state = model.borrow_mut();
        state.finish = None;
        state.error = false;
        state.active.push(Active {
            received: download.received_data_length(),
            total: download
                .response()
                .map(|response| response.content_length()),
            finished: false,
            download: download.clone(),
        });
        state.recompute();
    }
    if let Some(area) = model.borrow().area.upgrade() {
        area.set_visible(true);
    }
    if let Some(hit) = model.borrow().hit.upgrade() {
        hit.set_visible(true);
    }
    kick(model);
}

fn attach(model: &Rc<RefCell<Model>>, download: &Download) {
    let weak = Rc::downgrade(model);
    download.connect_response_notify(move |download| {
        let Some(model) = weak.upgrade() else { return };
        {
            let mut state = model.borrow_mut();
            if let Some(item) = state
                .active
                .iter_mut()
                .find(|item| item.download == *download)
                && item.total.is_none()
                && let Some(response) = download.response()
            {
                let length = response.content_length();
                if length > 0 {
                    item.total = Some(length);
                }
            }
            state.recompute();
        }
        if let Some(area) = model.borrow().area.upgrade() {
            area.queue_draw();
        }
    });
    let weak = Rc::downgrade(model);
    download.connect_received_data(move |download, _| {
        let Some(model) = weak.upgrade() else { return };
        {
            let mut state = model.borrow_mut();
            if let Some(item) = state
                .active
                .iter_mut()
                .find(|item| item.download == *download)
            {
                item.received = download.received_data_length();
                if item.total.is_none()
                    && let Some(response) = download.response()
                {
                    let length = response.content_length();
                    if length > 0 {
                        item.total = Some(length);
                    }
                }
            }
            state.recompute();
        }
        if let Some(area) = model.borrow().area.upgrade() {
            area.queue_draw();
        }
    });
    let weak = Rc::downgrade(model);
    download.connect_finished(move |download| {
        let Some(model) = weak.upgrade() else { return };
        {
            let mut state = model.borrow_mut();
            if let Some(item) = state
                .active
                .iter_mut()
                .find(|item| item.download == *download)
            {
                item.finished = true;
            }
            state.recompute();
        }
        kick(&model);
    });
    let weak = Rc::downgrade(model);
    download.connect_failed(move |download, error| {
        eprintln!("Download failed: {error}");
        let Some(model) = weak.upgrade() else { return };
        {
            let mut state = model.borrow_mut();
            state.active.retain(|item| item.download != *download);
            state.error = true;
            state.hover = false;
            state.recompute();
            if state.active.is_empty() {
                state.finish = Some(Finish {
                    started: Instant::now(),
                    red: true,
                });
            }
        }
        kick(&model);
    });
}

fn cancel(model: &Rc<RefCell<Model>>) {
    let downloads: Vec<Download> = model
        .borrow()
        .active
        .iter()
        .map(|item| item.download.clone())
        .collect();
    if downloads.is_empty() {
        return;
    }
    for download in downloads {
        download.cancel();
    }
    {
        let mut state = model.borrow_mut();
        state.active.clear();
        state.error = true;
        state.hover = false;
        state.progress = 0.0;
        state.indeterminate = false;
        state.finish = Some(Finish {
            started: Instant::now(),
            red: true,
        });
    }
    kick(model);
}

pub fn install(session: &NetworkSession, strip: &gtk::Overlay) {
    let area = gtk::DrawingArea::new();
    area.set_hexpand(true);
    area.set_valign(gtk::Align::End);
    area.set_content_height(BAR_HEIGHT as i32);
    area.set_can_target(false);
    area.set_visible(false);
    strip.add_overlay(&area);

    let hit = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    hit.set_valign(gtk::Align::Fill);
    hit.set_hexpand(true);
    hit.set_visible(false);
    hit.set_cursor_from_name(Some("pointer"));
    hit.add_css_class("download-hit");
    strip.add_overlay(&hit);

    let model = Rc::new(RefCell::new(Model {
        area: area.downgrade(),
        hit: hit.downgrade(),
        active: Vec::new(),
        finish: None,

        progress: 0.0,
        indeterminate: false,
        error: false,
        hover: false,
        epoch: Instant::now(),
        ticking: false,
    }));

    let painter = model.clone();
    area.set_draw_func(move |_, cr, width, height| {
        let now = Instant::now();
        let state = painter.borrow();
        draw_bar(
            cr,
            f64::from(width),
            f64::from(height),
            &state,
            now,
            state.alpha(now),
        );
    });

    let motion = gtk::EventControllerMotion::new();
    let state = model.clone();
    motion.connect_enter(move |_, _, _| {
        state.borrow_mut().hover = true;
        if let Some(area) = state.borrow().area.upgrade() {
            area.queue_draw();
        }
    });
    let state = model.clone();
    motion.connect_leave(move |_| {
        state.borrow_mut().hover = false;
        if let Some(area) = state.borrow().area.upgrade() {
            area.queue_draw();
        }
    });
    hit.add_controller(motion);

    let state = model.clone();
    let press = gtk::GestureClick::new();
    press.connect_released(move |_, _, _, _| cancel(&state));
    hit.add_controller(press);

    let weak = Rc::downgrade(&model);
    session.connect_download_started(move |_, download| {
        if let Some(model) = weak.upgrade() {
            enqueue(&model, download);
        }
    });
}

fn draw_bar(cr: &cairo::Context, width: f64, height: f64, state: &Model, now: Instant, alpha: f64) {
    let color = mix(BLUE, RED, state.red());
    cr.set_source_rgba(0.16, 0.17, 0.19, alpha);
    cr.rectangle(0., height - BAR_HEIGHT, width, BAR_HEIGHT);
    let _ = cr.fill();
    let (fill_left, fill_right) = if state.indeterminate {
        let band = width * 0.28;
        let phase = now.saturating_duration_since(state.epoch).as_secs_f64();
        let position = ((phase / 1.8) * std::f64::consts::TAU).sin() * 0.5 + 0.5;
        let start = position * (width - band);
        (start, start + band)
    } else {
        (0.0, width * state.progress)
    };
    cr.set_source_rgba(color.0, color.1, color.2, alpha);
    cr.rectangle(
        fill_left,
        height - BAR_HEIGHT,
        fill_right - fill_left,
        BAR_HEIGHT,
    );
    let _ = cr.fill();
}
