use crate::{
    Browser,
    blocker::{self, LocalList, Settings},
};
use gtk::{glib, prelude::*};
use std::{cell::RefCell, rc::Rc};
use webkit6::prelude::*;

fn update_cosmetic(browser: &Browser, settings: &Settings) {
    let sheets = blocker::cosmetic(settings);
    let mut filters = browser.filters.borrow_mut();
    for i in 0..browser.pages.n_pages() {
        if let Some(manager) = browser.view(i).and_then(|v| v.user_content_manager()) {
            for old in &filters.cosmetic {
                manager.remove_style_sheet(old);
            }
            for sheet in &sheets {
                manager.add_style_sheet(sheet);
            }
        }
    }
    filters.cosmetic = sheets;
}

fn list_row(
    rows: &gtk::Box,
    scroll: &gtk::ScrolledWindow,
    list: &LocalList,
    draft: &Rc<RefCell<Settings>>,
) {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let title = gtk::Label::builder()
        .label(&list.name)
        .xalign(0.)
        .hexpand(true)
        .ellipsize(gtk::pango::EllipsizeMode::Middle)
        .build();
    row.append(&title);
    let remove = crate::chrome::button(crate::chrome::Icon::Close, "Remove filter list");
    let name = list.name.clone();
    let draft = draft.clone();
    let weak = row.downgrade();
    let weak_scroll = scroll.downgrade();
    remove.connect_clicked(move |_| {
        draft.borrow_mut().lists.retain(|l| l.name != name);
        if let Some(row) = weak.upgrade()
            && let Some(parent) = row.parent().and_downcast::<gtk::Box>()
        {
            parent.remove(&row);
            if let Some(scroll) = weak_scroll.upgrade() {
                scroll.set_visible(parent.first_child().is_some());
            }
        }
    });
    row.append(&remove);
    rows.append(&row);
    scroll.set_visible(true);
}

fn selected_button(label: &str) -> gtk::ToggleButton {
    let button = gtk::ToggleButton::with_label(label);
    button.set_focus_on_click(false);
    button
}

fn toggle(parent: &gtk::Box, title: &str, active: bool) -> gtk::ToggleButton {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    let label = gtk::Label::new(Some(title));
    label.set_xalign(0.);
    label.set_hexpand(true);
    let control = selected_button(if active { "On" } else { "Off" });
    control.add_css_class("blocker-toggle");
    control.set_active(active);
    control.update_property(&[gtk::accessible::Property::Label(title)]);
    control.connect_toggled(|control| {
        control.set_label(if control.is_active() { "On" } else { "Off" })
    });
    row.append(&label);
    row.append(&control);
    parent.append(&row);
    control
}

pub fn open(
    parent: &gtk::Window,
    panel: &gtk::Popover,
    anchor: &gtk::MenuButton,
    browser: &Rc<Browser>,
    settings: &Rc<RefCell<Settings>>,
    extra: Rc<Vec<String>>,
) {
    let body = gtk::Box::new(gtk::Orientation::Vertical, 14);
    body.set_margin_top(16);
    body.set_margin_bottom(16);
    body.set_margin_start(16);
    body.set_margin_end(16);
    body.set_width_request(280);
    body.add_css_class("blocker-settings");
    let draft = Rc::new(RefCell::new(settings.borrow().clone()));
    let title = gtk::Label::new(Some("Ad blocker"));
    title.set_xalign(0.);
    title.add_css_class("panel-title");
    body.append(&title);
    let host = browser
        .active()
        .and_then(|v| v.uri())
        .and_then(|u| url::Url::parse(&u).ok())
        .filter(|u| matches!(u.scheme(), "http" | "https"))
        .and_then(|u| u.host_str().map(str::to_owned));
    let allow = host.as_ref().map(|host| {
        let site = gtk::Box::new(gtk::Orientation::Vertical, 8);
        let name = gtk::Label::new(Some(host));
        name.set_xalign(0.);
        name.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        name.add_css_class("site-name");
        site.append(&name);
        let choices = gtk::Box::new(gtk::Orientation::Horizontal, 3);
        choices.add_css_class("blocker-site-choice");
        choices.set_homogeneous(true);
        let block = selected_button("Block ads");
        let allow = selected_button("Allow ads");
        allow.set_group(Some(&block));
        if draft.borrow().exceptions.contains(host) {
            allow.set_active(true);
        } else {
            block.set_active(true);
        }
        choices.append(&block);
        choices.append(&allow);
        site.append(&choices);
        body.append(&site);
        allow
    });
    let enabled = toggle(&body, "Ad blocking on all sites", draft.borrow().enabled);
    let pick = gtk::Button::new();
    pick.add_css_class("blocker-tool");
    let pick_label = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    pick_label.set_halign(gtk::Align::Start);
    pick_label.append(&crate::chrome::glyph(crate::chrome::Icon::Picker));
    pick_label.append(&gtk::Label::new(Some("Hide an element")));
    pick.set_child(Some(&pick_label));
    pick.set_tooltip_text(Some("Click an element to hide it. Escape cancels."));
    if host.is_some() {
        body.append(&pick);
    }
    let details = gtk::Expander::new(Some("Filter lists"));
    details.add_css_class("blocker-lists");
    body.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    let lists = gtk::Box::new(gtk::Orientation::Vertical, 8);
    lists.set_margin_top(8);
    let easy = toggle(&lists, "EasyList", draft.borrow().easylist);
    details.set_child(Some(&lists));
    body.append(&details);
    let rows = gtk::Box::new(gtk::Orientation::Vertical, 4);
    let scroll = gtk::ScrolledWindow::builder()
        .child(&rows)
        .propagate_natural_height(true)
        .max_content_height(180)
        .min_content_height(0)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .visible(false)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .build();
    for list in &draft.borrow().lists {
        list_row(&rows, &scroll, list, &draft);
    }
    lists.append(&scroll);
    let add = gtk::Button::with_label("Import filter list…");
    lists.append(&add);
    let status = gtk::Label::builder()
        .wrap(true)
        .xalign(0.)
        .visible(false)
        .build();
    status.add_css_class("blocker-status");
    body.append(&status);
    let weak = parent.downgrade();
    let draft_add = draft.clone();
    let notice = status.clone();
    add.connect_clicked(move |_| {
        let Some(parent) = weak.upgrade() else { return };
        let chooser = gtk::FileDialog::builder()
            .title("Import filter list")
            .accept_label("Import")
            .build();
        let draft = draft_add.clone();
        let rows = rows.clone();
        let scroll = scroll.clone();
        let notice = notice.clone();
        chooser.open(Some(&parent), gtk::gio::Cancellable::NONE, move |result| {
            let Ok(file) = result else {
                return;
            };
            let Some(path) = file.path() else {
                notice.set_label("Choose a local text file.");
                notice.set_visible(true);
                return;
            };
            let name = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            match std::fs::read_to_string(path) {
                Ok(text) => {
                    let list = LocalList { name, text };
                    let mut state = draft.borrow_mut();
                    if let Some(existing) = state.lists.iter_mut().find(|l| l.name == list.name) {
                        *existing = list;
                    } else {
                        list_row(&rows, &scroll, &list, &draft);
                        state.lists.push(list);
                    }
                }
                Err(e) => {
                    notice.set_label(&e.to_string());
                    notice.set_visible(true);
                }
            }
        });
    });
    if let Some(host) = &host
        && draft.borrow().hidden.contains_key(host)
    {
        let reset = gtk::Button::with_label("Restore hidden elements");
        let draft = draft.clone();
        let host = host.clone();
        reset.connect_clicked(move |button| {
            draft.borrow_mut().hidden.remove(&host);
            button.set_sensitive(false);
        });
        body.append(&reset);
    }
    let apply = gtk::Button::with_label("Apply changes");
    apply.add_css_class("blocker-apply");
    body.append(&apply);
    let browser = browser.clone();
    let settings = settings.clone();
    let pick_browser = browser.clone();
    let pick_settings = settings.clone();

    let weak = panel.downgrade();
    let weak_anchor = anchor.downgrade();
    let picker_host = host.clone();

    let picker_view = browser.active().map(|v| v.downgrade());
    let picker_enabled = enabled.clone();
    let picker_allow = allow.clone();
    apply.connect_clicked(move |_| {
        let Some(window) = weak.upgrade() else { return };
        let mut next = draft.borrow().clone();
        next.enabled = enabled.is_active();
        next.easylist = easy.is_active();
        if let Some(host) = &host {
            if allow.as_ref().is_some_and(|s| s.is_active()) {
                next.exceptions.insert(host.clone());
            } else {
                next.exceptions.remove(host);
            }
        }
        let cosmetic_only = {
            let prev = settings.borrow();
            prev.enabled == next.enabled
                && prev.exceptions == next.exceptions
                && prev.easylist == next.easylist
                && prev.lists == next.lists
        };
        if cosmetic_only {
            match next.save() {
                Ok(()) => {
                    update_cosmetic(&browser, &next);
                    settings.replace(next);
                    window.popdown();
                }
                Err(error) => {
                    status.set_label(&error.to_string());
                    status.set_visible(true);
                }
            }
            return;
        }
        status.set_label("Applying filters…");
        status.set_visible(true);
        window.set_autohide(false);
        window.set_sensitive(false);
        let anchor = weak_anchor.upgrade();
        if let Some(a) = &anchor {
            a.set_sensitive(false);
        }
        let settings = settings.clone();
        let browser = browser.clone();
        let extra = extra.clone();
        let status = status.clone();
        glib::MainContext::default().spawn_local(async move {
            let result = async {
                let filters = blocker::load(&next, &extra).await?;
                next.save()?;
                Ok::<_, anyhow::Error>(filters)
            }
            .await;
            if let Some(a) = anchor {
                a.set_sensitive(true);
            }
            window.set_sensitive(true);
            window.set_autohide(true);
            match result {
                Ok(filters) => {
                    for i in 0..browser.pages.n_pages() {
                        if let Some(view) = browser.view(i)
                            && let Some(manager) = view.user_content_manager()
                        {
                            for old in &browser.filters.borrow().cosmetic {
                                manager.remove_style_sheet(old);
                            }
                            manager.remove_all_filters();
                            filters.attach(&manager);
                        }
                    }
                    browser.filters.replace(filters);
                    settings.replace(next);
                    window.popdown();
                    if let Some(view) = browser.active() {
                        view.reload();
                    }
                }
                Err(e) => {
                    window.popup();
                    status.set_label(&format!("{e:#}"));
                }
            }
        });
    });
    if let (Some(host), Some(weak_view)) = (picker_host, picker_view) {
        let panel = panel.downgrade();
        let anchor = anchor.downgrade();
        let update_pick = {
            let pick = pick.downgrade();
            let enabled = picker_enabled.downgrade();
            let allow = picker_allow.as_ref().map(|s| s.downgrade());
            Rc::new(move || {
                if let (Some(pick), Some(enabled)) = (pick.upgrade(), enabled.upgrade()) {
                    pick.set_sensitive(
                        enabled.is_active()
                            && !allow
                                .as_ref()
                                .and_then(|w| w.upgrade())
                                .is_some_and(|s| s.is_active()),
                    );
                }
            })
        };
        update_pick();
        let update = update_pick.clone();
        picker_enabled.connect_active_notify(move |_| update());
        if let Some(allow) = picker_allow {
            allow.connect_active_notify(move |_| update_pick());
        }
        pick.connect_clicked(move |_| {
            let (Some(panel), Some(anchor), Some(view)) =
                (panel.upgrade(), anchor.upgrade(), weak_view.upgrade())
            else {
                return;
            };
            panel.popdown();
            anchor.set_sensitive(false);
            view.grab_focus();

            let host = host.clone();
            let pick_browser = pick_browser.clone();
            let pick_settings = pick_settings.clone();

            glib::MainContext::default().spawn_local(async move {
                let result = view
                    .call_async_javascript_function_future(
                        include_str!("picker.js"),
                        None,
                        Some("nothing-browser-picker"),
                        None,
                    )
                    .await;
                if let Ok(value) = result
                    && let Ok(selection) =
                        serde_json::from_str::<crate::dust::Selection>(&value.to_string())
                {
                    crate::dust::animate(&view, &selection).await;
                    let selector = selection.rule;
                    let current = view.uri().and_then(|u| url::Url::parse(&u).ok());
                    if !selector.is_empty()
                        && current.as_ref().and_then(url::Url::host_str) == Some(host.as_str())
                    {
                        let mut next = pick_settings.borrow().clone();
                        next.hidden.entry(host).or_default().insert(selector);
                        match next.save() {
                            Ok(()) => {
                                update_cosmetic(&pick_browser, &next);
                                pick_settings.replace(next);
                            }
                            Err(error) => eprintln!("Could not save picker rule: {error}"),
                        }
                        anchor.set_sensitive(true);
                        return;
                    }
                }
                anchor.set_sensitive(true);
            });
        });
    }
    panel.set_child(Some(&body));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires a graphical session"]
    fn apply_saves_preferences() {
        // Only the child writes settings, with XDG_CONFIG_HOME in a disposable profile.
        if std::env::var_os("NB_BLOCKER_TEST_CHILD").is_none() {
            let dir = tempfile::tempdir().unwrap();
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "blocker_ui::tests::apply_saves_preferences",
                    "--ignored",
                ])
                .env("XDG_CONFIG_HOME", dir.path())
                .env("NB_BLOCKER_TEST_CHILD", "1")
                .status()
                .unwrap();
            assert!(status.success());
            return;
        }
        gtk::init().unwrap();
        let parent = gtk::Window::new();
        let browser = Rc::new(Browser {
            page_progress: Default::default(),
            preferences: Default::default(),
            pending_loads: Default::default(),
            filters: RefCell::default(),
            pages: gtk::Notebook::new(),
            address: gtk::Entry::new(),
            extensions: Default::default(),
            store_ids: Default::default(),
            store_busy: Default::default(),
        });
        let settings = Rc::new(RefCell::new(Settings::default()));
        let panel = gtk::Popover::new();
        let anchor = gtk::MenuButton::builder().popover(&panel).build();
        parent.set_child(Some(&anchor));
        parent.present();
        open(
            &parent,
            &panel,
            &anchor,
            &browser,
            &settings,
            Rc::new(vec![]),
        );
        let body = panel.child().unwrap();
        body.first_child()
            .unwrap()
            .next_sibling()
            .unwrap()
            .last_child()
            .unwrap()
            .downcast::<gtk::ToggleButton>()
            .unwrap()
            .set_active(false);
        body.last_child()
            .unwrap()
            .downcast::<gtk::Button>()
            .unwrap()
            .emit_clicked();
        glib::MainContext::default()
            .block_on(glib::timeout_future(std::time::Duration::from_millis(100)));
        assert!(!settings.borrow().enabled);
        assert!(!Settings::read().unwrap().enabled);
    }
}
