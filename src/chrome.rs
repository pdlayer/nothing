use gtk::prelude::*;

#[derive(Clone, Copy)]
pub enum Icon {
    Back,
    Forward,
    Reload,
    New,
    Close,
    Extensions,
    Shield,
    Settings,
    Site,
    Picker,
    WindowClose,
    Minimize,
    Maximize,
    Restore,
}

pub fn glyph(icon: Icon) -> gtk::Image {
    let svg: &'static [u8] = match icon {
        Icon::Minimize => include_bytes!("../assets/material/remove.svg"),
        Icon::Maximize => include_bytes!("../assets/material/crop_square.svg"),
        Icon::Restore => include_bytes!("../assets/material/filter_none.svg"),
        Icon::Back => include_bytes!("../assets/material/arrow_back_ios_new.svg"),
        Icon::Forward => include_bytes!("../assets/material/arrow_forward_ios.svg"),
        Icon::Reload => include_bytes!("../assets/material/refresh.svg"),
        Icon::New => include_bytes!("../assets/material/add.svg"),
        Icon::WindowClose | Icon::Close => include_bytes!("../assets/material/close.svg"),
        Icon::Picker => include_bytes!("../assets/material/colorize.svg"),
        Icon::Site => include_bytes!("../assets/material/language.svg"),
        Icon::Settings => include_bytes!("../assets/material/settings.svg"),
        Icon::Shield => include_bytes!("../assets/material/shield.svg"),
        Icon::Extensions => include_bytes!("../assets/material/extension.svg"),
    };
    let bytes = gtk::glib::Bytes::from_static(svg);
    let image = gtk::Image::from_gicon(&gtk::gio::BytesIcon::new(&bytes));
    image.set_pixel_size(match icon {
        Icon::Back | Icon::Forward => 16,
        Icon::New => 24,
        Icon::Close | Icon::Site => 16,
        Icon::Minimize | Icon::WindowClose => 20,
        Icon::Maximize | Icon::Restore => 13,
        Icon::Reload | Icon::Extensions | Icon::Shield | Icon::Picker | Icon::Settings => 20,
    });
    image
}

pub fn button(icon: Icon, label: &str) -> gtk::Button {
    let button = gtk::Button::builder().child(&glyph(icon)).build();
    button.update_property(&[gtk::accessible::Property::Label(label)]);
    button
}

pub fn titlebar(window: &gtk::Window) -> gtk::HeaderBar {
    let bar = gtk::HeaderBar::builder().show_title_buttons(false).build();
    bar.add_css_class("navigation");
    bar.add_css_class("window-bar");
    bar.pack_end(&window_controls(window));
    bar
}

pub fn window_controls(window: &gtk::Window) -> gtk::WindowHandle {
    let controls = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    controls.add_css_class("window-controls");
    let minimize = button(Icon::Minimize, "Minimize");
    let maximize = button(Icon::Maximize, "Maximize");
    maximize.add_css_class("maximize-window");
    let close = button(Icon::WindowClose, "Close window");
    close.add_css_class("close-window");
    let weak = window.downgrade();
    minimize.connect_clicked(move |_| {
        if let Some(w) = weak.upgrade() {
            w.minimize();
        }
    });
    let weak = window.downgrade();
    maximize.connect_clicked(move |_| {
        if let Some(w) = weak.upgrade() {
            if w.is_maximized() {
                w.unmaximize();
            } else {
                w.maximize();
            }
        }
    });
    let weak_button = maximize.downgrade();
    window.connect_maximized_notify(move |w| {
        if let Some(button) = weak_button.upgrade() {
            let (icon, label) = if w.is_maximized() {
                (Icon::Restore, "Restore")
            } else {
                (Icon::Maximize, "Maximize")
            };
            button.set_child(Some(&glyph(icon)));
            button.update_property(&[gtk::accessible::Property::Label(label)]);
        }
    });
    let weak = window.downgrade();
    close.connect_clicked(move |_| {
        if let Some(w) = weak.upgrade() {
            w.close();
        }
    });
    controls.append(&minimize);
    controls.append(&maximize);
    controls.append(&close);
    let handle = gtk::WindowHandle::new();
    controls.set_margin_start(16);
    handle.set_child(Some(&controls));
    handle
}

pub fn start_surface(search: &gtk::Entry) -> gtk::Box {
    use gtk::ConstraintAttribute::{CenterX, CenterY, Width};
    use gtk::ConstraintRelation::{Eq, Le};
    use gtk::glib::translate::IntoGlib;
    let surface = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let layout = gtk::ConstraintLayout::new();
    surface.set_vexpand(true);
    surface.set_layout_manager(Some(layout.clone()));
    search.set_parent(&surface);
    let required = gtk::ConstraintStrength::Required.into_glib();
    for axis in [CenterX, CenterY] {
        layout.add_constraint(gtk::Constraint::new(
            Some(search),
            axis,
            Eq,
            None::<&gtk::Widget>,
            axis,
            1.,
            0.,
            required,
        ));
    }
    layout.add_constraint(gtk::Constraint::new(
        Some(search),
        Width,
        Eq,
        None::<&gtk::Widget>,
        Width,
        0.68,
        0.,
        gtk::ConstraintStrength::Strong.into_glib(),
    ));
    layout.add_constraint(gtk::Constraint::new_constant(
        Some(search),
        Width,
        Le,
        680.,
        required,
    ));
    surface
}

pub fn style_start_search(entry: &gtk::Entry) {
    use gtk::EntryIconPosition::{Primary, Secondary};
    entry.add_css_class("start-search");
    for (position, svg) in [
        (
            Primary,
            include_bytes!("../assets/material/search.svg").as_slice(),
        ),
        (
            Secondary,
            include_bytes!("../assets/material/keyboard_return.svg").as_slice(),
        ),
    ] {
        let icon = gtk::gio::BytesIcon::new(&gtk::glib::Bytes::from_static(svg));
        entry.set_icon_from_gicon(position, Some(&icon));
    }
    entry.set_icon_activatable(Primary, false);
    entry.connect_icon_release(|entry, position| {
        if position == Secondary {
            entry.emit_activate();
        }
    });
}

/// Overlay navigation so revealing it does not resize the web page.
pub fn save_strip(panel: &gtk::Revealer) -> (gtk::DrawingArea, std::rc::Rc<dyn Fn()>) {
    use std::{
        cell::{Cell, RefCell},
        rc::Rc,
        time::Instant,
    };

    let strip = gtk::DrawingArea::new();
    strip.set_valign(gtk::Align::End);
    strip.set_content_height(3);
    strip.set_can_target(false);
    strip.set_visible(false);
    let length = Rc::new(Cell::new(1.0_f64));
    let drawn = length.clone();
    strip.set_draw_func(move |_, cr, width, height| {
        let width = f64::from(width);
        let filled = width * drawn.get();
        cr.set_source_rgb(119. / 255., 187. / 255., 134. / 255.);
        cr.rectangle(width - filled, 0., filled, f64::from(height));
        let _ = cr.fill();
    });

    let target = strip.downgrade();
    let panel = panel.downgrade();
    let speed = Rc::new(Cell::new(0.0_f64));
    let tick = Rc::new(RefCell::new(None::<gtk::TickCallbackId>));
    let show = Rc::new(move || {
        let Some(strip) = target.upgrade() else {
            return;
        };
        let previous = tick.borrow_mut().take();
        let repeating = previous.is_some();
        if let Some(previous) = previous {
            previous.remove();
        }
        let animated = strip.settings().is_gtk_enable_animations();
        let (from, velocity) = if repeating && animated {
            (length.get(), speed.get())
        } else {
            (1., 0.)
        };
        let intro = if animated {
            0.16 + 0.14 * (1. - from)
        } else {
            0.
        };
        let opacity = if repeating || !animated {
            strip.opacity()
        } else {
            0.
        };
        length.set(from);
        strip.set_opacity(opacity);
        strip.set_visible(true);
        strip.queue_draw();

        let started = std::cell::OnceCell::new();
        let panel = panel.clone();
        let length = length.clone();
        let speed = speed.clone();
        let active = tick.clone();
        let id = strip.add_tick_callback(move |strip, _| {
            if started.get().is_none()
                && panel
                    .upgrade()
                    .is_some_and(|panel| !panel.is_child_revealed())
            {
                return gtk::glib::ControlFlow::Continue;
            }
            let time = started.get_or_init(Instant::now).elapsed().as_secs_f64();
            if time < intro {
                let t = time / intro;
                let distance = 1. - from;
                let tangent = (velocity * intro).min(3. * distance);
                length.set(
                    (from + distance * t * t * (3. - 2. * t) + tangent * t * (1. - t).powi(2))
                        .clamp(0., 1.),
                );
                speed.set(
                    (6. * distance * t * (1. - t) + tangent * (1. - t) * (1. - 3. * t)) / intro,
                );
                strip.set_opacity(1. - (1. - opacity) * (1. - t).powi(2));
            } else {
                strip.set_opacity(1.);
                let elapsed = time - intro;
                if elapsed >= 2.4 {
                    strip.set_visible(false);
                    active.borrow_mut().take();
                    return gtk::glib::ControlFlow::Break;
                }
                if animated {
                    let ramp = 0.12;
                    let travel = if elapsed < ramp {
                        elapsed.powi(2) / (2. * ramp)
                    } else {
                        elapsed - ramp / 2.
                    };
                    length.set(1. - travel / (2.4 - ramp / 2.));
                    speed.set(-(elapsed / ramp).min(1.) / (2.4 - ramp / 2.));
                }
            }
            strip.queue_draw();
            gtk::glib::ControlFlow::Continue
        });
        tick.replace(Some(id));
    });
    (strip, show)
}

pub fn navigation_dock(
    bar: &gtk::HeaderBar,
    panel: &gtk::Revealer,
    menu: &gtk::Popover,
    protection: &gtk::Popover,
    save_strip: &gtk::DrawingArea,
) -> (gtk::Box, gtk::Overlay) {
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::time::Duration;

    let dock = gtk::Box::new(gtk::Orientation::Vertical, 0);
    dock.set_valign(gtk::Align::End);
    let edge = gtk::Box::new(gtk::Orientation::Vertical, 0);
    edge.add_css_class("navigation-edge");
    let grip = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    grip.add_css_class("navigation-grip");
    grip.set_hexpand(true);
    edge.append(&grip);
    let strip = gtk::Overlay::new();
    strip.set_child(Some(&edge));
    dock.append(&strip);
    panel.set_child(Some(bar));
    dock.append(panel);

    let pointer = gtk::EventControllerMotion::new();
    let focus = gtk::EventControllerFocus::new();
    dock.add_controller(pointer.clone());
    dock.add_controller(focus.clone());
    let pending = Rc::new(RefCell::new(None::<gtk::glib::SourceId>));
    let refresh = Rc::new({
        let panel = panel.downgrade();
        let pointer = pointer.downgrade();
        let focus = focus.downgrade();
        let menu = menu.downgrade();
        let protection = protection.downgrade();
        let save_strip = save_strip.downgrade();
        move || {
            if let Some(id) = pending.borrow_mut().take() {
                id.remove();
            }
            let (Some(panel), Some(pointer), Some(focus)) =
                (panel.upgrade(), pointer.upgrade(), focus.upgrade())
            else {
                return;
            };
            if pointer.contains_pointer()
                || focus.contains_focus()
                || save_strip.upgrade().is_some_and(|strip| strip.is_visible())
                || menu.upgrade().is_some_and(|m| m.is_visible())
                || protection.upgrade().is_some_and(|m| m.is_visible())
            {
                panel.set_reveal_child(true);
            } else {
                let timer_slot = pending.clone();
                let weak = panel.downgrade();
                let id = gtk::glib::timeout_add_local_once(Duration::from_millis(250), move || {
                    timer_slot.borrow_mut().take();
                    if let Some(panel) = weak.upgrade() {
                        panel.set_reveal_child(false);
                    }
                });
                pending.borrow_mut().replace(id);
            }
        }
    });
    let update = refresh.clone();
    pointer.connect_contains_pointer_notify(move |_| update());
    let update = refresh.clone();
    focus.connect_contains_focus_notify(move |_| update());
    let update = refresh.clone();
    menu.connect_visible_notify(move |_| update());
    let update = refresh.clone();
    protection.connect_visible_notify(move |_| update());
    save_strip.connect_visible_notify(move |_| refresh());
    (dock, strip)
}

/// GTK's notebook header is part of the client area. Hand presses on its
/// unused space to the compositor, leaving the notebook's own controls alone.
pub fn track_tab_focus(window: &gtk::ApplicationWindow) {
    use gtk::gdk::{Key, ModifierType};
    let keyboard = gtk::EventControllerKey::new();
    keyboard.set_propagation_phase(gtk::PropagationPhase::Capture);
    let owner = window.downgrade();
    keyboard.connect_key_pressed(move |_, key, _, modifiers| {
        if let Some(window) = owner.upgrade() {
            let command = modifiers.intersects(
                ModifierType::ALT_MASK | ModifierType::CONTROL_MASK | ModifierType::SUPER_MASK,
            );
            if matches!(key, Key::Tab | Key::ISO_Left_Tab) && !command {
                window.add_css_class("tab-keyboard");
            } else if command || matches!(key, Key::Alt_L | Key::Alt_R) {
                window.remove_css_class("tab-keyboard");
            }
        }
        gtk::glib::Propagation::Proceed
    });
    window.add_controller(keyboard);
    let pointer = gtk::GestureClick::builder().button(0).build();
    pointer.set_propagation_phase(gtk::PropagationPhase::Capture);
    let owner = window.downgrade();
    pointer.connect_pressed(move |_, _, _, _| {
        if let Some(window) = owner.upgrade() {
            window.remove_css_class("tab-keyboard");
        }
    });
    window.add_controller(pointer);
    window.connect_is_active_notify(|window| {
        if !window.is_active() {
            window.remove_css_class("tab-keyboard");
        }
    });
}

pub fn draggable_tab_strip(pages: &gtk::Notebook) {
    let press = gtk::GestureClick::builder().button(1).build();
    press.set_propagation_phase(gtk::PropagationPhase::Capture);
    press.connect_pressed(|gesture, _, x, y| {
        let Some(notebook) = gesture.widget() else {
            return;
        };
        let mut hit = notebook.pick(x, y, gtk::PickFlags::DEFAULT);
        while let Some(widget) = hit {
            if widget.is::<gtk::Button>() || widget.css_name() == "tab" || widget == notebook {
                return;
            }
            if widget.css_name() == "header" && widget.parent().as_ref() == Some(&notebook) {
                if let Some(event) = gesture.current_event()
                    && let Some(surface) = event.surface().and_downcast::<gtk::gdk::Toplevel>()
                    && let Some(device) = event.device()
                    && let Some((sx, sy)) = event.position()
                {
                    gesture.set_state(gtk::EventSequenceState::Claimed);
                    surface.begin_move(&device, 1, sx, sy, event.time());
                }
                return;
            }
            hit = widget.parent();
        }
    });
    pages.add_controller(press);
}

/// Keep interactive resizing inside the glass surface instead of allocating
/// transparent client-side decoration margins outside KWin's blur region.
pub fn install_resize_edges(window: &gtk::ApplicationWindow) {
    use gtk::gdk::SurfaceEdge::*;
    let press = gtk::GestureClick::builder().button(1).build();
    press.set_propagation_phase(gtk::PropagationPhase::Capture);
    let weak = window.downgrade();
    press.connect_pressed(move |gesture, _, x, y| {
        let Some(window) = weak.upgrade() else { return };
        if window.is_maximized() || window.is_fullscreen() {
            return;
        }
        let dx = window.width() as f64 - x;
        let dy = window.height() as f64 - y;
        let edge = match (x < 4., dx <= 4., y < 4., dy <= 4.) {
            (true, _, true, _) => NorthWest,
            (_, true, true, _) => NorthEast,
            (true, _, _, true) => SouthWest,
            (_, true, _, true) => SouthEast,
            (true, _, _, _) => West,
            (_, true, _, _) => East,
            (_, _, true, _) => North,
            (_, _, _, true) => South,
            _ => return,
        };
        if let Some(event) = gesture.current_event()
            && let Some(surface) = event.surface().and_downcast::<gtk::gdk::Toplevel>()
            && let Some(device) = event.device()
            && let Some((sx, sy)) = event.position()
        {
            gesture.set_state(gtk::EventSequenceState::Claimed);
            surface.begin_resize(edge, Some(&device), 1, sx, sy, event.time());
        }
    });
    window.add_controller(press);
}
