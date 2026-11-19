use anyhow::{Context, Result, ensure};
use gtk::{glib, prelude::*};
use serde::{Deserialize, Serialize};
use std::{cell::RefCell, path::PathBuf, rc::Rc};

#[derive(Clone, Copy, Default, Deserialize, PartialEq, Serialize)]
pub enum SearchEngine {
    #[default]
    DuckDuckGo,
    Google,
    Bing,
}

impl SearchEngine {
    const ALL: [Self; 3] = [Self::DuckDuckGo, Self::Google, Self::Bing];
    const LABELS: [&str; 3] = ["DuckDuckGo", "Google", "Bing"];

    pub fn search(self, query: &str) -> String {
        let endpoint = match self {
            Self::DuckDuckGo => "https://duckduckgo.com/",
            Self::Google => "https://www.google.com/search",
            Self::Bing => "https://www.bing.com/search",
        };
        url::Url::parse_with_params(endpoint, [("q", query)])
            .expect("valid search endpoint")
            .into()
    }
}

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct Preferences {
    pub search: SearchEngine,
    pub startup_page: String,
}

impl Preferences {
    fn path() -> PathBuf {
        glib::user_config_dir().join("nothing-browser/preferences.json")
    }

    pub fn load() -> Result<Self> {
        match std::fs::read(Self::path()) {
            Ok(bytes) => Ok(serde_json::from_slice(&bytes).context("Invalid browser preferences")?),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(error.into()),
        }
    }

    fn save(&self) -> Result<()> {
        let path = Self::path();
        let directory = path.parent().expect("preferences directory");
        std::fs::create_dir_all(directory)?;
        let file = tempfile::NamedTempFile::new_in(directory)?;
        serde_json::to_writer(file.as_file(), self)?;
        file.persist(path)?;
        Ok(())
    }

    pub fn page(state: Rc<RefCell<Self>>, on_saved: Rc<dyn Fn()>) -> gtk::ScrolledWindow {
        let panel = gtk::Box::new(gtk::Orientation::Vertical, 24);
        panel.add_css_class("browser-preferences");
        panel.set_halign(gtk::Align::Center);
        panel.set_margin_top(48);
        panel.set_margin_bottom(32);
        panel.set_margin_start(24);
        panel.set_margin_end(24);
        let title = gtk::Label::new(Some("Settings"));
        title.set_xalign(0.);
        title.add_css_class("preferences-title");
        panel.append(&title);

        let search = gtk::Box::new(gtk::Orientation::Vertical, 8);
        let label = gtk::Label::new(Some("Default search engine"));
        label.set_xalign(0.);
        search.append(&label);
        let track = gtk::Box::new(gtk::Orientation::Vertical, 0);
        track.add_css_class("preferences-engines");
        let engines = gtk::Box::new(gtk::Orientation::Horizontal, 2);
        engines.set_homogeneous(true);
        let selected = Rc::new(std::cell::Cell::new(state.borrow().search));
        let indicator = gtk::DrawingArea::new();
        indicator.set_can_target(false);
        let position = Rc::new(std::cell::Cell::new(f64::from(selected.get() as u32)));
        let tick = Rc::new(RefCell::new(None::<gtk::TickCallbackId>));
        let current = position.clone();
        indicator.set_draw_func(move |_, cr, width, height| {
            if width <= 4 || height < 12 {
                return;
            }
            let segment = (f64::from(width) - 4.) / 3.;
            let x = current.get() * (segment + 2.);
            let bottom = f64::from(height);
            let right = x + segment;
            let radius = 6.;
            let quarter = std::f64::consts::FRAC_PI_2;
            cr.arc(right - radius, radius, radius, -quarter, 0.);
            cr.arc(right - radius, bottom - radius, radius, 0., quarter);
            cr.arc(x + radius, bottom - radius, radius, quarter, quarter * 2.);
            cr.arc(x + radius, radius, radius, quarter * 2., quarter * 3.);
            cr.close_path();
            cr.set_source_rgb(56. / 255., 56. / 255., 54. / 255.);
            let _ = cr.fill();
        });
        let overlay = gtk::Overlay::new();
        overlay.set_child(Some(&indicator));
        overlay.add_overlay(&engines);
        overlay.set_measure_overlay(&engines, true);
        track.append(&overlay);
        let mut first: Option<gtk::ToggleButton> = None;
        for (index, engine) in SearchEngine::ALL.into_iter().enumerate() {
            let choice = gtk::ToggleButton::with_label(SearchEngine::LABELS[index]);
            choice.set_focus_on_click(false);
            if let Some(first) = &first {
                choice.set_group(Some(first));
            } else {
                first = Some(choice.clone());
            }
            choice.set_active(index == selected.get() as usize);
            let selected = selected.clone();
            let position = position.clone();
            let indicator = indicator.downgrade();
            let tick = tick.clone();
            choice.connect_toggled(move |choice| {
                if !choice.is_active() {
                    return;
                }
                selected.set(engine);
                let Some(indicator) = indicator.upgrade() else {
                    return;
                };
                if let Some(previous) = tick.borrow_mut().take() {
                    previous.remove();
                }
                let destination = index as f64;
                if !indicator.settings().is_gtk_enable_animations() {
                    position.set(destination);
                    indicator.queue_draw();
                    return;
                }
                let start = position.get();
                let position = position.clone();
                let started = std::time::Instant::now();
                let active = tick.clone();
                let id = indicator.add_tick_callback(move |indicator, _| {
                    let elapsed = (started.elapsed().as_secs_f64() / 0.17).min(1.);
                    let ease = 1. - (1. - elapsed).powi(3);
                    position.set(start + (destination - start) * ease);
                    indicator.queue_draw();
                    if elapsed >= 1. {
                        active.borrow_mut().take();
                        glib::ControlFlow::Break
                    } else {
                        glib::ControlFlow::Continue
                    }
                });
                tick.replace(Some(id));
            });
            engines.append(&choice);
        }
        search.append(&track);
        panel.append(&search);
        panel.append(&gtk::Separator::new(gtk::Orientation::Horizontal));

        let startup = gtk::Entry::builder()
            .placeholder_text("https://example.com")
            .text(&state.borrow().startup_page)
            .hexpand(true)
            .build();
        startup.add_css_class("preferences-address");
        crate::text_effect::attach(&startup);
        let label = gtk::Label::new(Some("Open on startup"));
        label.set_xalign(0.);
        startup.update_property(&[gtk::accessible::Property::Label("Open on startup")]);
        let section = gtk::Box::new(gtk::Orientation::Vertical, 8);
        section.append(&label);
        section.append(&startup);
        let hint = gtk::Label::new(Some("Leave blank to open a new tab."));
        hint.set_xalign(0.);
        hint.add_css_class("dim-label");
        section.append(&hint);
        panel.append(&section);

        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        let status = gtk::Label::new(None);
        status.set_wrap(true);
        status.add_css_class("preferences-error");
        status.set_xalign(0.);
        status.set_hexpand(true);
        let save = gtk::Button::with_label("Save");
        save.add_css_class("preferences-save");
        save.set_sensitive(false);
        save.set_focus_on_click(false);
        actions.append(&status);
        actions.append(&save);
        panel.append(&actions);
        let refresh = {
            let state = state.clone();
            let selected = selected.clone();
            let startup = startup.downgrade();
            let save = save.downgrade();
            let status = status.downgrade();
            Rc::new(move || {
                if let (Some(startup), Some(save), Some(status)) =
                    (startup.upgrade(), save.upgrade(), status.upgrade())
                {
                    let saved = state.borrow();
                    save.set_sensitive(
                        selected.get() != saved.search
                            || startup.text().trim() != saved.startup_page,
                    );
                    status.set_label("");
                }
            })
        };
        let on_edit = refresh.clone();
        startup.connect_changed(move |_| on_edit());
        let mut child = engines.first_child();
        while let Some(widget) = child {
            child = widget.next_sibling();
            let choice = widget
                .downcast::<gtk::ToggleButton>()
                .expect("search engine choice");
            let on_choice = refresh.clone();
            choice.connect_toggled(move |choice| {
                if choice.is_active() {
                    on_choice();
                }
            });
        }

        let save_button = save.downgrade();
        startup.connect_activate(move |_| {
            if let Some(button) = save_button.upgrade()
                && button.is_sensitive()
            {
                button.emit_clicked();
            }
        });
        save.connect_clicked(move |_| {
            let result = (|| -> Result<Self> {
                let input = startup.text();
                let input = input.trim();
                let startup_page = if input.is_empty() {
                    String::new()
                } else {
                    let url = url::Url::parse(input).context("Enter a full web address.")?;
                    ensure!(
                        matches!(url.scheme(), "http" | "https") && url.has_host(),
                        "Use an HTTP or HTTPS address."
                    );
                    url.into()
                };
                let next = Self {
                    search: selected.get(),
                    startup_page,
                };
                next.save()?;
                Ok(next)
            })();
            match result {
                Ok(next) => {
                    let page = next.startup_page.clone();
                    state.replace(next);
                    startup.set_text(&page);
                    refresh();
                    on_saved();
                }
                Err(error) => status.set_label(&error.to_string()),
            }
        });
        let keyboard = gtk::EventControllerKey::new();
        keyboard.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak_panel = panel.downgrade();
        keyboard.connect_key_pressed(move |_, key, _, _| {
            if matches!(key, gtk::gdk::Key::Tab | gtk::gdk::Key::ISO_Left_Tab)
                && let Some(panel) = weak_panel.upgrade()
            {
                panel.add_css_class("preferences-keyboard");
            }
            glib::Propagation::Proceed
        });
        let pointer = gtk::GestureClick::builder().button(0).build();
        pointer.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak_panel = panel.downgrade();
        pointer.connect_pressed(move |_, _, _, _| {
            if let Some(panel) = weak_panel.upgrade() {
                panel.remove_css_class("preferences-keyboard");
            }
        });
        let page = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .child(&panel)
            .vexpand(true)
            .build();
        page.add_controller(keyboard);
        page.add_controller(pointer);
        page
    }
}
