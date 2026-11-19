mod blocker;
mod blocker_ui;
mod chrome;
mod downloads;
mod dust;
mod extension;
mod page_clip;
mod preferences;
mod text_effect;
mod web_store;
mod worker_host;
use anyhow::{Context, Result, bail};
use chrome::Icon;
use extension::Extension;
use gtk::{gio, prelude::*};
use serde_json::{Value, json};
use std::{path::PathBuf, rc::Rc};
use webkit6::{UserContentManager, UserScript, WebView, prelude::*};

fn persistent_session() -> Result<webkit6::NetworkSession> {
    use std::fs::{DirBuilder, OpenOptions, Permissions};
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};

    let dir = gtk::glib::user_data_dir().join("nothing-browser");
    DirBuilder::new().recursive(true).mode(0o700).create(&dir)?;
    let directory = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(&dir)?;
    directory.set_permissions(Permissions::from_mode(0o700))?;
    let database = dir.join("cookies.sqlite");
    for suffix in ["", "-wal", "-shm", "-journal"] {
        let path = dir.join(format!("cookies.sqlite{suffix}"));
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(suffix.is_empty())
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path);
        match file {
            Ok(f) => {
                anyhow::ensure!(f.metadata()?.is_file(), "Expected a regular cookie file");
                f.set_permissions(Permissions::from_mode(0o600))?;
            }
            Err(e) if !suffix.is_empty() && e.kind() == std::io::ErrorKind::NotFound => (),
            Err(e) => return Err(e.into()),
        }
    }
    let session = webkit6::NetworkSession::default().context("WebKit session unavailable")?;
    session
        .cookie_manager()
        .context("WebKit cookie manager unavailable")?
        .set_persistent_storage(
            database.to_str().context("Invalid cookie database path")?,
            webkit6::CookiePersistentStorage::Sqlite,
        );
    Ok(session)
}

fn browsing_session() -> webkit6::NetworkSession {
    thread_local! {
        static SESSION: webkit6::NetworkSession = {
            let session = persistent_session().unwrap_or_else(|error| {
                eprintln!("Private profile unavailable ({error}); using an ephemeral session");
                webkit6::NetworkSession::new_ephemeral()
            });
            if let Some(manager) = session.website_data_manager() {
                manager.set_favicons_enabled(true);
            }
            session
        };
    }
    SESSION.with(Clone::clone)
}

fn page_settings() -> webkit6::Settings {
    static AUDIO_OUTPUT: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let available = *AUDIO_OUTPUT.get_or_init(|| {
        let available = gstreamer::init().is_ok()
            && gstreamer::ElementFactory::find("autoaudiosink").is_some();
        if !available {
            eprintln!("Missing GStreamer autoaudiosink. Web Audio is disabled. On Arch Linux install gst-plugins-good and restart Nothing Browser.");
        }
        available
    });
    webkit6::Settings::builder()
        .enable_webaudio(available)
        .build()
}

type PendingLoads = std::collections::HashMap<WebView, String>;

enum OffscreenResult {
    Ready(Value),
    Loading(WebView, url::Url),
}

async fn wait_extension_document(view: &WebView, page: &url::Url) -> bool {
    for _ in 0..200 {
        if !view.is_loading() && view.uri().as_deref() == Some(page.as_str()) {
            return true;
        }
        gtk::glib::timeout_future(std::time::Duration::from_millis(25)).await;
    }
    false
}

struct Browser {
    page_progress: gtk::glib::WeakRef<gtk::DrawingArea>,
    preferences: Rc<std::cell::RefCell<preferences::Preferences>>,
    pending_loads: std::cell::RefCell<Option<PendingLoads>>,
    filters: std::cell::RefCell<blocker::Filters>,
    pages: gtk::Notebook,
    address: gtk::Entry,
    extensions: std::cell::RefCell<Vec<Extension>>,
    store_ids: std::cell::RefCell<std::collections::BTreeSet<String>>,
    store_busy: std::cell::Cell<bool>,
}
impl Browser {
    fn view(&self, index: u32) -> Option<WebView> {
        self.pages
            .nth_page(Some(index))?
            .downcast::<gtk::Box>()
            .ok()?
            .first_child()?
            .downcast()
            .ok()
    }
    fn active(&self) -> Option<WebView> {
        self.view(self.pages.current_page()?)
    }
    fn update_page_progress(&self, view: Option<&WebView>) {
        if let Some(strip) = self.page_progress.upgrade() {
            strip.set_visible(view.is_some_and(|view| view.is_loading()));
            strip.queue_draw();
        }
    }

    fn configure(self: &Rc<Self>, manager: &UserContentManager) {
        manager.remove_all_scripts();
        manager.add_script(&UserScript::for_world(
            include_str!("web_store.js"),
            webkit6::UserContentInjectedFrames::TopFrame,
            webkit6::UserScriptInjectionTime::End,
            web_store::WORLD,
            &["https://chromewebstore.google.com/*"],
            &[],
        ));
        for (index, ext) in self.extensions.borrow().iter().enumerate() {
            ext.attach(manager);
            self.register_runtime(manager, index, ext, Some(&ext.world), None);
        }
    }
    fn register_runtime(
        self: &Rc<Self>,
        manager: &UserContentManager,
        index: usize,
        ext: &Extension,
        world: Option<&str>,
        document: Option<(&WebView, &url::Url)>,
    ) {
        let name = ext.bridge_name();
        if !manager.register_script_message_handler_with_reply(&name, world) {
            return;
        }
        let browser = Rc::downgrade(self);
        let document = document.map(|(view, uri)| (view.downgrade(), uri.to_string()));
        manager.connect_script_message_with_reply_received(Some(&name), move |_, value, reply| {
            let Some(browser) = browser.upgrade() else {
                reply.return_error_message("Browser has closed");
                return true;
            };
            if document.as_ref().is_some_and(|(view, uri)| view.upgrade().and_then(|v| v.uri()).as_deref() != Some(uri.as_str())) {
                reply.return_error_message("Extension document has changed");
                return true;
            }
            let Some(context) = value.context() else {
                reply.return_error_message("Missing JavaScript context");
                return true;
            };
            let packet = value.to_json(0).and_then(|json| serde_json::from_str::<Value>(&json).ok());
            if let Some(patch) = packet.as_ref().and_then(|p| p.get("set")) {
                let extensions = browser.extensions.borrow();
                let extension = &extensions[index];
                let world = extension.world.clone();
                match extension.update(patch) {
                    Ok(()) => {
                        // The sender applies its own patch after the native acknowledgement.
                        reply.return_value(&webkit6::javascriptcore::Value::new_undefined(&context));
                        let sender = document.as_ref().and_then(|(view, _)| view.upgrade());
                        let background = extension.background_view.borrow();
                        let offscreen = extension.offscreen_view.borrow();
                        let update = format!("globalThis.nbApply?.({patch})");
                        for target in [background.as_ref(), offscreen.as_ref().map(|(view, _)| view)].into_iter().flatten() {
                            if Some(target) != sender.as_ref() {
                                target.evaluate_javascript(&update, None, None, gio::Cancellable::NONE, |_| {});
                            }
                        }
                    }
                    Err(error) => {
                        reply.return_error_message(&error.to_string());
                        return true;
                    }
                }
                drop(extensions);
                for tab in 0..browser.pages.n_pages() {
                    if let Some(view) = browser.view(tab) {
                        if let Some(manager) = view.user_content_manager() {
                            browser.configure(&manager);
                        }
                        eval(&view, &world, &format!("globalThis.nbApply?.({patch})"));
                    }
                }
                return true;
            }
            let sender = document.as_ref().and_then(|(view, _)| view.upgrade());
            let is_background = browser.extensions.borrow()[index].background_view.borrow().as_ref()
                .is_some_and(|view| Some(view) == sender.as_ref());
            if is_background {
                browser.extensions.borrow()[index].background_activity.touch();
            }
            if let Some(item) = packet.as_ref().and_then(|p| p.get("createMenu")) {
                let result = (|| -> Result<()> {
                    let extensions = browser.extensions.borrow();
                    let extension = &extensions[index];
                    anyhow::ensure!(is_background && extension.can_use_menus, "contextMenus permission required");
                    let item: extension::MenuItem = serde_json::from_value(item.clone())?;
                    anyhow::ensure!((item.id.is_string() || item.id.is_u64()) && !item.title.trim().is_empty(), "Invalid context menu item");
                    let mut items = extension.menu_items.borrow_mut();
                    anyhow::ensure!(items.len() < 100 && items.iter().all(|existing| existing.id != item.id), "Duplicate context menu ID or too many items");
                    items.push(item);
                    Ok(())
                })();
                match result {
                    Ok(()) => reply.return_value(&webkit6::javascriptcore::Value::new_undefined(&context)),
                    Err(error) => reply.return_error_message(&error.to_string()),
                }
                return true;
            }
            if let Some(request) = packet.as_ref().and_then(|p| p.get("createTab")) {
                let uri = request["url"].as_str().and_then(|uri| url::Url::parse(uri).ok())
                    .filter(|uri| matches!(uri.scheme(), "http" | "https"));
                if !is_background || request["active"] == false || uri.is_none() {
                    reply.return_error_message("Only active HTTP(S) tabs can be opened from the background");
                } else if let Some(uri) = uri {
                    browser.new_tab();
                    browser.navigate(uri.as_str());
                    let tab = json!({"id":browser.pages.current_page(), "active":true});
                    reply.return_value(&webkit6::javascriptcore::Value::from_json(&context, &tab.to_string()));
                }
                return true;
            }
            if let Some(command) = packet.as_ref().and_then(|p| p["offscreen"].as_str()) {
                let result = browser.offscreen(index, is_background, command, &packet.as_ref().unwrap()["options"]);
                match result {
                    Ok(OffscreenResult::Ready(value)) => reply.return_value(&webkit6::javascriptcore::Value::from_json(&context, &value.to_string())),
                    Ok(OffscreenResult::Loading(view, uri)) => {
                        let reply = reply.clone();
                        let owner = Rc::downgrade(&browser);
                        gtk::glib::spawn_future_local(async move {
                            let loaded = wait_extension_document(&view, &uri).await;
                            let still_open = owner.upgrade().is_some_and(|owner| {
                                owner.extensions.borrow()[index].offscreen_view.borrow()
                                    .as_ref().is_some_and(|(current, _)| current == &view)
                            });
                            if loaded && still_open {
                                reply.return_value(&webkit6::javascriptcore::Value::new_undefined(&context));
                            } else {
                                if let Some(owner) = owner.upgrade() {
                                    let extensions = owner.extensions.borrow();
                                    let mut offscreen = extensions[index].offscreen_view.borrow_mut();
                                    if offscreen.as_ref().is_some_and(|(current, _)| current == &view) { offscreen.take(); }
                                }
                                reply.return_error_message("Offscreen document failed to load");
                            }
                        });
                    }
                    Err(error) => reply.return_error_message(&error.to_string()),
                }
                return true;
            }
            let source = match packet.as_ref() {
                Some(packet) if packet["port"].is_object() => {
                    let command = &packet["port"];
                    format!("return {{value: await globalThis.nbPortCommand({command})}};")
                }
                Some(packet) if packet.get("runtimeMessage").is_some() => {
                    let message = &packet["runtimeMessage"];
                    format!("if (typeof globalThis.nbReceiveMessage !== 'function') throw new Error('Extension document did not initialize'); return await globalThis.nbReceiveMessage({message}, {{}});")
                }
                _ => {
                    reply.return_error_message("Invalid extension request");
                    return true;
                }
            };
            let target = if is_background {
                browser.extensions.borrow()[index].offscreen_view.borrow().clone()
            } else {
                browser.background(index)
            };
            let Some((target, page)) = target else {
                reply.return_error_message("No runtime receiver is available");
                return true;
            };
            let activity = browser.extensions.borrow()[index].background_activity.begin();
            let callback = reply.clone();
            gtk::glib::spawn_future_local(async move {
                let _activity = activity;
                if !wait_extension_document(&target, &page).await {
                    callback.return_error_message("Extension document failed to load");
                    return;
                }
                match target.call_async_javascript_function_future(&source, None, None, None).await {
                    Ok(result) => {
                        let response = result.to_json(0)
                            .and_then(|data| serde_json::from_str::<Value>(&data).ok())
                            .and_then(|mut packet| packet.as_object_mut()?.remove("value"))
                            .unwrap_or(Value::Null);
                        callback.return_value(&webkit6::javascriptcore::Value::from_json(&context, &response.to_string()));
                    }
                    Err(error) => callback.return_error_message(error.message()),
                }
            });
            true
        });
    }

    fn background(self: &Rc<Self>, index: usize) -> Option<(WebView, url::Url)> {
        let extensions = self.extensions.borrow();
        let extension = &extensions[index];
        if let Some(view) = extension.background_view.borrow().as_ref() {
            let page = extension.background_host.borrow().as_ref()?.page.clone();
            return Some((view.clone(), page));
        }
        let host = match extension.worker_host() {
            Ok(host) => host?,
            Err(error) => {
                eprintln!("Extension worker: {error:#}");
                return None;
            }
        };
        let page = host.page.clone();
        extension.background_host.replace(Some(host));
        let view = self.extension_document(index, extension, &page, true);
        let owner = Rc::downgrade(self);
        view.connect_load_changed(move |view, event| {
            if event == webkit6::LoadEvent::Finished
                && let Some(owner) = owner.upgrade()
                && !owner.extensions.borrow()[index]
                    .background_initialized
                    .replace(true)
            {
                let view = view.clone();
                let activity = owner.extensions.borrow()[index].background_activity.begin();
                gtk::glib::spawn_future_local(async move {
                    let _activity = activity;
                    if let Err(error) = view
                        .call_async_javascript_function_future(
                            "await globalThis.nbExtensionInstalled();",
                            None,
                            None,
                            None,
                        )
                        .await
                    {
                        owner.extensions.borrow()[index]
                            .background_initialized
                            .set(false);
                        eprintln!("Extension startup: {error}");
                    }
                });
            }
        });
        let owner = Rc::downgrade(self);
        view.connect_web_process_terminated(move |view, _| {
            if let Some(owner) = owner.upgrade() {
                let extensions = owner.extensions.borrow();
                let mut current = extensions[index].background_view.borrow_mut();
                if current.as_ref() == Some(view) {
                    current.take();
                    extensions[index].background_host.take();
                }
            }
        });
        extension.background_activity.touch();
        let owner = Rc::downgrade(self);
        let weak_view = view.downgrade();
        gtk::glib::timeout_add_seconds_local(5, move || {
            let (Some(owner), Some(view)) = (owner.upgrade(), weak_view.upgrade()) else {
                return gtk::glib::ControlFlow::Break;
            };
            let extensions = owner.extensions.borrow();
            let extension = &extensions[index];
            let mut current = extension.background_view.borrow_mut();
            if current.as_ref() != Some(&view) {
                return gtk::glib::ControlFlow::Break;
            }
            let activity = &extension.background_activity;
            if !view.is_loading()
                && activity.pending.get() == 0
                && activity
                    .last
                    .get()
                    .is_some_and(|last| last.elapsed().as_secs() >= 30)
            {
                current.take();
                extension.background_host.take();
                return gtk::glib::ControlFlow::Break;
            }
            gtk::glib::ControlFlow::Continue
        });
        extension.background_view.replace(Some(view.clone()));
        view.load_uri(page.as_str());
        Some((view, page))
    }

    fn offscreen(
        self: &Rc<Self>,
        index: usize,
        is_background: bool,
        command: &str,
        options: &Value,
    ) -> Result<OffscreenResult> {
        let extensions = self.extensions.borrow();
        let extension = &extensions[index];
        anyhow::ensure!(
            is_background && extension.can_use_offscreen,
            "Offscreen documents require the offscreen permission in the background context"
        );
        let mut current = extension.offscreen_view.borrow_mut();
        match command {
            "has" => Ok(OffscreenResult::Ready(json!(current.is_some()))),
            "close" => {
                let (view, _) = current.take().context("No offscreen document")?;
                view.stop_loading();
                Ok(OffscreenResult::Ready(Value::Null))
            }
            "create" => {
                anyhow::ensure!(current.is_none(), "An offscreen document already exists");
                let page = extension
                    .offscreen_page(options["url"].as_str().context("Missing offscreen URL")?)?;
                let view = self.extension_document(index, extension, &page, false);
                current.replace((view.clone(), page.clone()));
                view.load_uri(page.as_str());
                Ok(OffscreenResult::Loading(view, page))
            }
            _ => bail!("Unknown offscreen operation"),
        }
    }

    fn extension_document(
        self: &Rc<Self>,
        index: usize,
        extension: &Extension,
        page: &url::Url,
        background: bool,
    ) -> WebView {
        let manager = UserContentManager::new();
        let source = if background {
            format!(
                "globalThis.nbRuntimeCall = packet => window.webkit.messageHandlers[{}].postMessage(packet);",
                json!(extension.bridge_name())
            )
        } else {
            format!("{}\n{}", extension.bootstrap(), extension.bridge_source())
        };
        let mut pattern = page.clone();
        if background {
            pattern.set_port(None).expect("HTTP URL");
        }
        manager.add_script(&UserScript::new(
            &source,
            webkit6::UserContentInjectedFrames::TopFrame,
            webkit6::UserScriptInjectionTime::Start,
            &[pattern.as_str()],
            &[],
        ));
        let view = WebView::builder()
            .network_session(&webkit6::NetworkSession::new_ephemeral())
            .user_content_manager(&manager)
            .build();
        self.register_runtime(&manager, index, extension, None, Some((&view, page)));
        let allowed = page.clone();
        view.connect_decide_policy(move |_, decision, kind| {
            if kind == webkit6::PolicyDecisionType::NewWindowAction {
                decision.ignore();
                return true;
            }
            if let Some(nav) = decision.downcast_ref::<webkit6::NavigationPolicyDecision>()
                && nav
                    .navigation_action()
                    .and_then(|a| a.request())
                    .and_then(|r| r.uri())
                    .as_deref()
                    != Some(allowed.as_str())
            {
                decision.ignore();
                return true;
            }
            false
        });
        view
    }

    fn start_menu_backgrounds(self: &Rc<Self>) {
        for (index, extension) in self.extensions.borrow().iter().enumerate() {
            if extension.can_use_menus {
                self.background(index);
            }
        }
    }

    fn add_context_items(
        self: &Rc<Self>,
        view: &WebView,
        menu: &webkit6::ContextMenu,
        hit: &webkit6::HitTestResult,
    ) {
        let Some(uri) = view.uri() else {
            return;
        };
        if !url::Url::parse(&uri).is_ok_and(|url| {
            matches!(url.scheme(), "http" | "https")
                && url.host_str() != Some("chromewebstore.google.com")
        }) {
            return;
        }
        for (index, extension) in self.extensions.borrow().iter().enumerate() {
            for (position, item) in extension.menu_items.borrow().iter().enumerate() {
                let matches = item.contexts.iter().any(|context| match context.as_str() {
                    "all" => true,
                    "selection" => hit.context_is_selection(),
                    "image" => hit.context_is_image(),
                    "link" => hit.context_is_link(),
                    "editable" => hit.context_is_editable(),
                    "page" => {
                        !hit.context_is_selection()
                            && !hit.context_is_image()
                            && !hit.context_is_link()
                            && !hit.context_is_editable()
                            && !hit.context_is_media()
                    }
                    _ => false,
                });
                if !matches {
                    continue;
                }
                let action =
                    gio::SimpleAction::new(&format!("nb-extension-{index}-{position}"), None);
                let owner = Rc::downgrade(self);
                let view = view.downgrade();
                let uri = uri.clone();
                let id = item.id.clone();
                let is_selection = hit.context_is_selection();
                let image = hit.image_uri();
                let link = hit.link_uri();
                action.connect_activate(move |_, _| {
                    let (Some(browser), Some(view)) = (owner.upgrade(), view.upgrade()) else {
                        return;
                    };
                    let Some((background, background_uri)) = browser.background(index) else {
                        return;
                    };
                    let uri = uri.clone();
                    let mut info = json!({"menuItemId":id,"pageUrl":uri.as_str()});
                    if let Some(image) = &image {
                        info["srcUrl"] = json!(image.as_str());
                        info["mediaType"] = json!("image");
                    }
                    if let Some(link) = &link {
                        info["linkUrl"] = json!(link.as_str());
                    }
                    let tab = json!({"id":browser.pages.current_page(),"active":true});
                    let activity = browser.extensions.borrow()[index]
                        .background_activity
                        .begin();
                    gtk::glib::spawn_future_local(async move {
                        let _activity = activity;
                        if !wait_extension_document(&background, &background_uri).await {
                            return;
                        }

                        if is_selection
                            && let Ok(value) = view
                                .evaluate_javascript_future(
                                    "window.getSelection().toString()",
                                    Some("nothing-browser-context-menu"),
                                    None,
                                )
                                .await
                        {
                            info["selectionText"] = json!(value.to_string());
                        }
                        if view.uri().as_ref() != Some(&uri) {
                            return;
                        }
                        let source =
                            format!("await globalThis.nbContextMenuClick?.({info}, {tab});");
                        let _ = background
                            .call_async_javascript_function_future(&source, None, None, None)
                            .await;
                    });
                });
                menu.append(&webkit6::ContextMenuItem::from_gaction(
                    &action,
                    &item.title,
                    None,
                ));
            }
        }
    }

    fn new_tab(&self) {
        let tab = gtk::Box::new(gtk::Orientation::Vertical, 0);
        tab.add_css_class("start-page");
        let search = gtk::Entry::builder()
            .placeholder_text("Search or enter address")
            .width_chars(1)
            .build();
        chrome::style_start_search(&search);
        text_effect::attach(&search);
        let address = self.address.downgrade();
        search.connect_activate(move |entry| {
            if !entry.text().trim().is_empty()
                && let Some(address) = address.upgrade()
            {
                address.set_text(&entry.text());
                address.emit_activate();
            }
        });
        tab.append(&chrome::start_surface(&search));
        self.append_tab(&tab, "New tab");
        search.grab_focus();
    }

    fn open_preferences(&self, on_saved: Rc<dyn Fn()>) {
        for index in 0..self.pages.n_pages() {
            if self
                .pages
                .nth_page(Some(index))
                .is_some_and(|page| page.has_css_class("preferences-tab"))
            {
                self.pages.set_current_page(Some(index));
                return;
            }
        }
        let tab = gtk::Box::new(gtk::Orientation::Vertical, 0);
        tab.add_css_class("preferences-tab");
        tab.append(&preferences::Preferences::page(
            self.preferences.clone(),
            on_saved,
        ));
        self.append_tab(&tab, "Settings");
    }

    fn append_tab(&self, tab: &gtk::Box, title: &str) {
        let label = gtk::Label::new(Some(title));
        label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        label.set_width_chars(8);
        label.set_max_width_chars(21);
        label.set_xalign(0.0);
        let heading = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        let icon = chrome::glyph(if tab.has_css_class("preferences-tab") {
            Icon::Settings
        } else {
            Icon::Site
        });
        icon.set_pixel_size(14);
        heading.append(&icon);
        heading.append(&label);
        let close = chrome::button(Icon::Close, "Close tab");
        close.add_css_class("flat");
        close.set_valign(gtk::Align::Center);
        let weak_pages = self.pages.downgrade();
        let weak_tab = tab.downgrade();
        close.connect_clicked(move |_| {
            if let (Some(pages), Some(tab)) = (weak_pages.upgrade(), weak_tab.upgrade())
                && let Some(idx) = pages.page_num(&tab)
            {
                if pages.n_pages() == 1 {
                    let _ = gtk::prelude::WidgetExt::activate_action(&pages, "win.close", None);
                } else {
                    pages.remove_page(Some(idx));
                }
            }
        });
        heading.append(&close);
        let idx = self.pages.append_page(tab, Some(&heading));
        self.pages.page(tab).set_tab_expand(false);
        self.pages.set_tab_reorderable(tab, true);
        self.pages.set_current_page(Some(idx));
        if self.pages.current_page() == Some(idx) {
            heading.add_css_class("active-tab");
        }
    }
    fn navigate(self: &Rc<Self>, uri: &str) {
        let Some(tab) = self
            .pages
            .current_page()
            .and_then(|i| self.pages.nth_page(Some(i)))
        else {
            return;
        };
        let view = self.active().unwrap_or_else(|| {
            let manager = UserContentManager::new();
            self.configure(&manager);
            for extension in self.extensions.borrow().iter() {
                extension.attach_styles(&manager);
            }
            self.filters.borrow().attach(&manager);
            let view = WebView::builder()
                .network_session(&browsing_session())
                .settings(&page_settings())
                .user_content_manager(&manager)
                .vexpand(true)
                .build();
            manager.register_script_message_handler("nothingStore", Some(web_store::WORLD));
            let page = view.downgrade();
            let owner = Rc::downgrade(self);
            manager.connect_script_message_received(Some("nothingStore"), move |_, _| {
                if let (Some(owner), Some(page)) = (owner.upgrade(), page.upgrade()) {
                    owner.install_store(&page);
                }
            });
            let owner = Rc::downgrade(self);
            view.connect_load_changed(move |view, event| {
                if event == webkit6::LoadEvent::Finished
                    && let Some(owner) = owner.upgrade()
                {
                    owner.tell_store(view);
                }
            });
            let owner = Rc::downgrade(self);
            view.connect_context_menu(move |view, menu, hit| {
                if let Some(browser) = owner.upgrade() {
                    browser.add_context_items(view, menu, hit);
                }
                false
            });
            let owner = Rc::downgrade(self);
            view.connect_estimated_load_progress_notify(move |view| {
                if let Some(browser) = owner.upgrade()
                    && browser.active().as_ref() == Some(view)
                {
                    browser.update_page_progress(Some(view));
                }
            });
            let owner = Rc::downgrade(self);
            view.connect_is_loading_notify(move |view| {
                if let Some(browser) = owner.upgrade()
                    && browser.active().as_ref() == Some(view)
                {
                    browser.update_page_progress(Some(view));
                }
            });
            // Let the tab background show until WebKit commits its first document.
            view.set_opacity(0.0);
            let heading = self.pages.tab_label(&tab).unwrap();
            let icon = heading
                .first_child()
                .unwrap()
                .downcast::<gtk::Image>()
                .unwrap();
            let label = icon
                .next_sibling()
                .unwrap()
                .downcast::<gtk::Label>()
                .unwrap();
            let fallback = icon.gicon().expect("tab fallback icon");
            view.connect_favicon_notify(move |view| {
                let thumbnail = view.favicon().and_then(|texture| {
                    let icon = gtk::gdk::pixbuf_get_from_texture(&texture)?;
                    icon.scale_simple(14, 14, gtk::gdk_pixbuf::InterpType::Bilinear)
                });
                if let Some(thumbnail) = thumbnail {
                    icon.set_paintable(Some(&gtk::gdk::Texture::for_pixbuf(&thumbnail)));
                } else {
                    icon.set_from_gicon(&fallback);
                }
            });
            view.connect_title_notify(move |v| {
                label.set_label(v.title().as_deref().unwrap_or("New tab"))
            });
            let weak = Rc::downgrade(self);
            view.connect_uri_notify(move |v| {
                if let Some(b) = weak.upgrade()
                    && b.active().as_ref() == Some(v)
                {
                    b.address.set_text(v.uri().as_deref().unwrap_or(""));
                }
            });
            let retry = gtk::Button::with_label("Page stopped responding · Reload");
            retry.set_visible(false);
            let weak_view = view.downgrade();
            retry.connect_clicked(move |_| {
                if let Some(view) = weak_view.upgrade() {
                    view.reload();
                }
            });
            let notice = retry.clone();
            view.connect_web_process_terminated(move |_, reason| {
                eprintln!("WebKit page process terminated: {reason:?}");
                notice.set_visible(true);
            });
            let notice = retry.clone();
            view.connect_load_changed(move |view, event| {
                if matches!(
                    event,
                    webkit6::LoadEvent::Committed | webkit6::LoadEvent::Finished
                ) {
                    view.set_opacity(1.0);
                }
                if event == webkit6::LoadEvent::Started {
                    notice.set_visible(false);
                }
            });
            let page = tab.downcast_ref::<gtk::Box>().unwrap();
            if let Some(start) = page.first_child() {
                page.remove(&start);
            }
            page.remove_css_class("start-page");
            page.append(&view);
            tab.downcast_ref::<gtk::Box>().unwrap().append(&retry);
            view
        });
        self.address.set_visible(true);
        if let Some(loads) = self.pending_loads.borrow_mut().as_mut() {
            loads.insert(view, uri.to_owned());
        } else {
            view.load_uri(uri);
        }
    }
    fn close(&self) {
        self.pages.remove_page(self.pages.current_page());
        if self.pages.n_pages() == 0 {
            self.new_tab()
        }
    }
    fn tell_store(&self, view: &WebView) {
        if view.uri().is_some_and(|uri| {
            url::Url::parse(&uri).is_ok_and(|url| {
                url.scheme() == "https" && url.host_str() == Some("chromewebstore.google.com")
            })
        }) {
            eval(
                view,
                web_store::WORLD,
                &format!(
                    "globalThis.nbStoreInstalled?.({})",
                    json!(*self.store_ids.borrow())
                ),
            );
        }
    }

    fn install_store(self: &Rc<Self>, view: &WebView) {
        let Some(uri) = view.uri() else { return };
        let Ok(id) = web_store::listing_id(&uri) else {
            return;
        };
        let Some(parent) = view.root().and_downcast::<gtk::Window>() else {
            return;
        };
        if self.store_ids.borrow().contains(&id) || self.store_busy.replace(true) {
            return;
        }
        let browser = self.clone();
        gtk::glib::spawn_future_local(async move {
            let result = async {
                let file = gio::spawn_blocking(move || web_store::fetch(&uri))
                    .await
                    .map_err(|_| anyhow::anyhow!("Store download stopped"))??;
                let extension = Extension::load(file.path())?;
                let dialog = gtk::AlertDialog::builder()
                    .message(format!("Add {} to Nothing?", extension.name))
                    .detail("This extension can run scripts on sites matched by its manifest.")
                    .buttons(["Cancel", "Add"])
                    .cancel_button(0)
                    .default_button(0)
                    .build();
                if !matches!(dialog.choose_future(Some(&parent)).await, Ok(1)) {
                    return Ok(());
                }
                file.persist_noclobber(web_store::directory().join(format!("{id}.crx")))?;
                browser.extensions.borrow_mut().push(extension);
                browser.start_menu_backgrounds();
                browser.store_ids.borrow_mut().insert(id);
                for index in 0..browser.pages.n_pages() {
                    if let Some(view) = browser.view(index) {
                        if let Some(manager) = view.user_content_manager() {
                            browser.configure(&manager);
                            if let Some(extension) = browser.extensions.borrow().last() {
                                extension.attach_styles(&manager);
                            }
                        }
                        browser.tell_store(&view);
                    }
                }
                Ok::<_, anyhow::Error>(())
            }
            .await;
            browser.store_busy.set(false);
            if let Err(error) = result {
                gtk::AlertDialog::builder()
                    .message("Could not add extension")
                    .detail(format!("{error:#}"))
                    .build()
                    .show(Some(&parent));
            }
        });
    }

    fn popup(self: &Rc<Self>, index: usize, parent: &gtk::Window) {
        let extensions = self.extensions.borrow();
        let ext = &extensions[index];
        let Some(uri) = ext.popup.as_ref() else {
            return;
        };
        let manager = UserContentManager::new();
        let granted_tab = self.active().and_then(|page| {
            let origin = url::Url::parse(&page.uri()?).ok()?.origin();
            Some((page.downgrade(), origin))
        });
        let tab_targets: Vec<_> = (0..self.pages.n_pages())
            .map(|index| self.view(index).map(|page| page.downgrade()))
            .collect();
        let tabs: Vec<_> = (0..self.pages.n_pages())
            .map(|i| {
                let mut tab = json!({"id": i, "active": Some(i) == self.pages.current_page()});
                if ext.can_read_tabs {
                    let uri = self.view(i).and_then(|v| v.uri());
                    tab["url"] = json!(uri.as_deref().unwrap_or("about:blank"));
                }
                tab
            })
            .collect();
        let script = format!(
            "{}\n{}\nconst nbTabs={};\nconst nbResourceBase={};\n{}",
            ext.bootstrap(),
            ext.bridge_source(),
            json!(tabs),
            json!(ext.resource_base().as_str()),
            include_str!("popup.js")
        );
        manager.add_script(&UserScript::new(
            &script,
            webkit6::UserContentInjectedFrames::TopFrame,
            webkit6::UserScriptInjectionTime::Start,
            &[uri.as_str()],
            &[],
        ));
        manager.register_script_message_handler("extension", None);
        let view = WebView::builder()
            .settings(&page_settings())
            .network_session(&webkit6::NetworkSession::new_ephemeral())
            .user_content_manager(&manager)
            .build();
        self.register_runtime(&manager, index, ext, None, Some((&view, uri)));
        let weak = view.downgrade();
        let browser = self.clone();
        manager.connect_script_message_received(Some("extension"), move |_, value| {
            let extensions = browser.extensions.borrow();
            let ext = &extensions[index];
            if weak.upgrade().and_then(|v| v.uri()).as_deref()
                != ext.popup.as_ref().map(|u| u.as_str())
            {
                return;
            }
            let Ok(msg) = serde_json::from_str::<Value>(&value.to_string()) else {
                return;
            };
            if let Some(request) = msg.get("execute") {
                let (Some(popup), Some(call)) = (weak.upgrade(), request["id"].as_u64()) else { return };
                let target = request["target"]["tabId"].as_u64().and_then(|id| u32::try_from(id).ok());
                let page = target.filter(|id| Some(*id) == browser.pages.current_page())
                    .and_then(|id| browser.view(id))
                    .filter(|page| page.uri().and_then(|uri| url::Url::parse(&uri).ok())
                        .is_some_and(|url| matches!(url.scheme(), "https" | "http")
                            && url.host_str() != Some("chromewebstore.google.com")
                                && granted_tab.as_ref().is_some_and(|(granted, origin)|
                                    granted.upgrade().as_ref() == Some(page) && *origin == url.origin())));
                let source = request["source"].as_str();
                let args = request["args"].as_array();
                let Some((page, source, args)) = page.zip(source).zip(args).map(|((page, source), args)| (page, source, args))
                    .filter(|_| ext.can_script_active_tab && request["target"]["allFrames"] != true
                        && request["target"].get("frameIds").is_none() && request["target"].get("documentIds").is_none())
                else {
                    reply_to_popup(&popup, call, Value::Null, Some("Only active-tab top-frame injection with activeTab and scripting permissions is supported"));
                    return;
                };
                let source = format!("return await (({source})(...{}));", json!(args));
                let world = ext.world.clone();
                let original_uri = popup.uri();
                gtk::glib::spawn_future_local(async move {
                    let result = page.call_async_javascript_function_future(&source, None, Some(&world), None).await;
                    if popup.uri() != original_uri { return; }
                    match result {
                        Ok(value) => {
                            let result = value.to_json(0).and_then(|json| serde_json::from_str::<Value>(&json).ok());
                            reply_to_popup(&popup, call, json!([{"frameId":0,"result":result}]), None);
                        }
                        Err(error) => reply_to_popup(&popup, call, Value::Null, Some(error.message())),
                    }
                });
                return;
            }
            if let Some(request) = msg.get("tabMessage")
                && let (Some(popup), Some(call)) = (weak.upgrade(), request["id"].as_u64())
            {
                let target = request["tab"].as_u64().and_then(|id| usize::try_from(id).ok())
                    .and_then(|id| tab_targets.get(id)).and_then(Option::as_ref)
                    .and_then(|page| page.upgrade())
                    .filter(|page| page.root().is_some());
                let Some(page) = target else {
                    reply_to_popup(&popup, call, Value::Null, Some("Tab no longer exists"));
                    return;
                };
                let world = ext.world.clone();
                let source = format!(
                    "if (typeof globalThis.nbReceiveMessage !== 'function') throw new Error('Receiving end does not exist'); return await globalThis.nbReceiveMessage({}, {{}});",
                    request["message"]
                );
                let original_uri = popup.uri();
                gtk::glib::spawn_future_local(async move {
                    let result = page.call_async_javascript_function_future(&source, None, Some(&world), None).await;
                    if popup.uri() != original_uri { return; }
                    match result {
                        Ok(value) => {
                            let response = value.to_json(0)
                                .and_then(|data| serde_json::from_str::<Value>(&data).ok())
                                .and_then(|mut packet| packet.as_object_mut()?.remove("value"))
                                .unwrap_or(Value::Null);
                            reply_to_popup(&popup, call, response, None);
                        }
                        Err(error) => reply_to_popup(&popup, call, Value::Null, Some(error.message())),
                    }
                });
            }
        });
        view.load_uri(uri.as_str());
        let dialog = gtk::Window::builder()
            .transient_for(parent)
            .destroy_with_parent(true)
            .modal(true)
            .title(&ext.name)
            .default_width(390)
            .default_height(540)
            .child(&view)
            .build();
        dialog.add_css_class("nothing-window");
        let bar = chrome::titlebar(&dialog);
        bar.set_title_widget(Some(&gtk::Label::new(Some(&ext.name))));
        dialog.set_titlebar(Some(&bar));
        dialog.present();
    }
}
fn reply_to_popup(view: &WebView, id: u64, result: Value, error: Option<&str>) {
    let source = format!("globalThis.nbApiResult?.({id}, {result}, {})", json!(error));
    view.evaluate_javascript(&source, None, None, gio::Cancellable::NONE, |_| {});
}

fn eval(view: &WebView, world: &str, js: &str) {
    view.evaluate_javascript(js, Some(world), None, gio::Cancellable::NONE, |r| {
        if let Err(e) = r {
            eprintln!("Extension: {e}")
        }
    });
}
fn destination(input: &str, search: preferences::SearchEngine) -> Option<String> {
    let input = input.trim();
    if input.is_empty() {
        return None;
    }
    if input == "about:blank" {
        return Some(input.into());
    }
    let host = input.split(['/', ':']).next().unwrap_or("");
    if !input.chars().any(char::is_whitespace)
        && (host.contains('.')
            || host == "localhost"
            || input.starts_with('[')
            || input.starts_with("http://")
            || input.starts_with("https://"))
    {
        let url = if input.starts_with("http://") || input.starts_with("https://") {
            input.into()
        } else {
            format!("https://{input}")
        };
        if let Ok(parsed) = url::Url::parse(&url) {
            return Some(parsed.into());
        }
    }
    Some(search.search(input))
}

fn main() -> Result<()> {
    let prefs = preferences::Preferences::load()?;
    let mut files = Vec::new();
    let mut store_urls = Vec::new();
    let mut filter_paths = Vec::new();
    let mut blocking = true;
    let mut start = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--no-adblock" => blocking = false,
            "--filter-list" => filter_paths.push(PathBuf::from(
                args.next().context("Expected path to filter list")?,
            )),
            "--web-store" => {
                store_urls.push(args.next().context("Expected a Chrome Web Store URL")?)
            }
            "--extension" => {
                files.push(PathBuf::from(args.next().context("Expected path to .crx")?))
            }
            "--help" | "-h" => {
                println!(
                    "nothing-browser [--extension FILE.crx]... [--web-store LISTING_URL]... [--filter-list FILE]... [--no-adblock] [URL]"
                );
                return Ok(());
            }
            _ if !arg.starts_with('-') && start.is_none() => {
                start = destination(&arg, prefs.search)
            }
            _ => bail!("Unexpected argument {arg}"),
        }
    }
    let mut extensions = files
        .iter()
        .map(|p| Extension::load(p).with_context(|| p.display().to_string()))
        .collect::<Result<Vec<_>>>()?;
    for listing in store_urls {
        let file = web_store::fetch(&listing).with_context(|| listing.clone())?;
        extensions.push(Extension::load(file.path()).with_context(|| listing.clone())?);
    }
    let mut installed = std::collections::BTreeSet::new();
    if let Ok(entries) = std::fs::read_dir(web_store::directory()) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("crx") {
                continue;
            }
            match Extension::load(&path) {
                Ok(extension) => {
                    installed.insert(path.file_stem().unwrap().to_string_lossy().into_owned());
                    if !extensions.iter().any(|e| e.world == extension.world) {
                        extensions.push(extension);
                    }
                }
                Err(error) => eprintln!("{}: {error:#}", path.display()),
            }
        }
    }
    gtk::init()?;
    let settings = Rc::new(std::cell::RefCell::new(blocker::Settings::read()?));
    let extra = Rc::new(
        filter_paths
            .iter()
            .map(|p| std::fs::read_to_string(p).with_context(|| p.display().to_string()))
            .collect::<Result<Vec<_>>>()?,
    );
    let app = gtk::Application::builder()
        .application_id("app.nothingbrowser.Browser")
        .flags(gio::ApplicationFlags::NON_UNIQUE)
        .build();
    let browser = std::cell::RefCell::new(Some(extensions));
    app.connect_activate(move |app| {
        if let Some(window) = app.active_window() {
            window.present();
            return;
        }
        if let Some(settings) = gtk::Settings::default() {
            settings.set_gtk_theme_name(Some("Adwaita"));
        }
        let window = gtk::ApplicationWindow::builder()
            .application(app)
            .title("Nothing Browser")
            .default_width(1050)
            .default_height(720)
            .build();
        let css = gtk::CssProvider::new();
        let base = include_str!("chrome.css");
        let display = gtk::prelude::WidgetExt::display(&window);
        if gtk::check_version(4, 24, 0).is_none() && display.type_().name() == "GdkWaylandDisplay" {
            css.load_from_data(&format!("{base}\n{}", include_str!("glass.css")));
            window.add_css_class("compositor-glass");
        } else {
            css.load_from_data(base);
        }
        gtk::style_context_add_provider_for_display(
            &gtk::prelude::WidgetExt::display(&window),
            &css,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
        window.add_css_class("nothing-window");
        if window.has_css_class("compositor-glass") {
            window.set_decorated(false);
            chrome::install_resize_edges(&window);
        } else {
            let empty_titlebar = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            window.set_titlebar(Some(&empty_titlebar));
            empty_titlebar.set_visible(false);
        }
        let bar = gtk::HeaderBar::builder().show_title_buttons(false).build();
        bar.add_css_class("navigation");
        bar.add_css_class("page-navigation");
        let b = Rc::new(Browser {
            page_progress: Default::default(),
            preferences: Rc::new(prefs.clone().into()),
            pending_loads: std::cell::RefCell::new(blocking.then(PendingLoads::new)),
            filters: Default::default(),
            extensions: browser.take().unwrap_or_default().into(),
            store_ids: installed.clone().into(),
            store_busy: Default::default(),
            pages: gtk::Notebook::new(),
            address: gtk::Entry::builder()
                .hexpand(false)
                .placeholder_text("Search or enter address")
                .build(),
        });
        text_effect::attach(&b.address);
        let panel = gtk::Revealer::builder()
            .transition_type(gtk::RevealerTransitionType::SlideUp)
            .transition_duration(160)
            .build();
        let controls = chrome::window_controls(window.upcast_ref());
        b.pages.set_action_widget(&controls, gtk::PackType::End);
        b.pages.set_vexpand(true);
        b.pages.set_scrollable(true);
        b.pages.set_tab_pos(gtk::PositionType::Top);
        b.pages.add_css_class("browser-tabs");
        chrome::track_tab_focus(&window);
        chrome::draggable_tab_strip(&b.pages);
        b.pages.set_show_border(false);
        b.address.set_width_chars(36);
        b.address.set_max_width_chars(60);
        for (name, icon, key) in [
            ("back", Some(Icon::Back), "<Alt>Left"),
            ("forward", Some(Icon::Forward), "<Alt>Right"),
            ("reload", Some(Icon::Reload), "<Control>r"),
            ("address", None, "<Control>l"),
            ("new", Some(Icon::New), "<Control>t"),
            ("close", None, "<Control>w"),
        ] {
            let action = gio::SimpleAction::new(name, None);
            let owner = b.clone();
            let panel = panel.clone();
            action.connect_activate(move |_, _| match name {
                "new" => owner.new_tab(),
                "close" => owner.close(),
                "address" => {
                    let field = if owner.active().is_some() {
                        panel.set_reveal_child(true);
                        Some(owner.address.clone())
                    } else {
                        owner
                            .pages
                            .current_page()
                            .and_then(|i| owner.pages.nth_page(Some(i)))
                            .and_then(|tab| tab.first_child())
                            .and_then(|surface| surface.first_child())
                            .and_then(|field| field.downcast::<gtk::Entry>().ok())
                    };
                    if let Some(field) = field {
                        field.grab_focus();
                        field.select_region(0, -1);
                    }
                }
                _ => {
                    if let Some(v) = owner.active() {
                        match name {
                            "back" => v.go_back(),
                            "forward" => v.go_forward(),
                            _ => v.reload(),
                        }
                    }
                }
            });
            window.add_action(&action);
            let full = format!("win.{name}");
            app.set_accels_for_action(&full, &[key]);
            if name == "close" {
                continue;
            }
            if name == "address" {
                bar.set_title_widget(Some(&b.address));
            } else if let Some(icon) = icon {
                let button = chrome::button(icon, name);
                if matches!(name, "back" | "forward" | "reload") {
                    b.address
                        .bind_property("visible", &button, "visible")
                        .sync_create()
                        .build();
                }
                button.set_action_name(Some(&full));
                button.add_css_class("flat");
                button.set_valign(gtk::Align::Center);
                if name == "new" {
                    let handle = gtk::WindowHandle::new();
                    let group = gtk::Box::new(gtk::Orientation::Horizontal, 0);
                    group.append(&button);
                    button.add_css_class("new-tab");
                    handle.set_child(Some(&group));
                    b.pages.set_action_widget(&handle, gtk::PackType::Start);
                } else if matches!(name, "back" | "forward") {
                    bar.pack_start(&button);
                } else {
                    bar.pack_end(&button);
                }
            }
        }
        let (save_strip, show_saved) = chrome::save_strip(&panel);
        let settings_action = gio::SimpleAction::new("preferences", None);
        let owner = Rc::downgrade(&b);
        settings_action.connect_activate(move |_, _| {
            if let Some(browser) = owner.upgrade() {
                browser.open_preferences(show_saved.clone());
            }
        });
        window.add_action(&settings_action);
        app.set_accels_for_action("win.preferences", &["<Control>comma"]);
        let settings_button = chrome::button(Icon::Settings, "Settings");
        settings_button.set_action_name(Some("win.preferences"));
        settings_button.set_valign(gtk::Align::Center);
        bar.pack_end(&settings_button);
        let weak = Rc::downgrade(&b);
        b.address.connect_activate(move |entry| {
            if let Some(b) = weak.upgrade()
                && let Some(url) = destination(&entry.text(), b.preferences.borrow().search)
            {
                let previous_view = b.active();
                b.navigate(&url);
                if let Some(view) = previous_view {
                    view.grab_focus();
                }
            }
        });
        let weak = Rc::downgrade(&b);
        b.pages.connect_switch_page(move |_, _, i| {
            if let Some(b) = weak.upgrade() {
                for index in 0..b.pages.n_pages() {
                    if let Some(heading) = b
                        .pages
                        .nth_page(Some(index))
                        .and_then(|page| b.pages.tab_label(&page))
                    {
                        if index == i {
                            heading.add_css_class("active-tab");
                        } else {
                            heading.remove_css_class("active-tab");
                        }
                    }
                }
                let view = b.view(i);
                b.update_page_progress(view.as_ref());
                b.address.set_visible(view.is_some());
                b.address
                    .set_text(view.and_then(|v| v.uri()).as_deref().unwrap_or(""));
            }
        });
        let protection = gtk::Popover::builder()
            .position(gtk::PositionType::Top)
            .has_arrow(false)
            .halign(gtk::Align::End)
            .build();
        protection.add_css_class("protection-panel");
        let shield = gtk::MenuButton::builder()
            .child(&chrome::glyph(chrome::Icon::Shield))
            .popover(&protection)
            .build();
        shield.set_valign(gtk::Align::Center);
        shield.set_sensitive(false);
        let weak_window = window.downgrade();
        let anchor = shield.downgrade();
        let owner = b.clone();
        let state = settings.clone();
        let cli = extra.clone();
        protection.connect_show(move |popover| {
            if let (Some(w), Some(anchor)) = (weak_window.upgrade(), anchor.upgrade()) {
                blocker_ui::open(
                    w.upcast_ref(),
                    popover,
                    &anchor,
                    &owner,
                    &state,
                    cli.clone(),
                );
            }
        });
        bar.pack_end(&shield);
        let extensions_menu = gtk::Box::new(gtk::Orientation::Vertical, 2);
        let popover = gtk::Popover::builder().child(&extensions_menu).build();
        let weak_window = window.downgrade();
        let owner = b.clone();
        popover.connect_show(move |popover| {
            while let Some(child) = extensions_menu.first_child() {
                extensions_menu.remove(&child);
            }
            for (index, extension) in owner.extensions.borrow().iter().enumerate() {
                let button = gtk::Button::with_label(&extension.name);
                button.set_sensitive(extension.popup.is_some());
                let window = weak_window.clone();
                let owner = owner.clone();
                let popover = popover.downgrade();
                button.connect_clicked(move |_| {
                    if let Some(popover) = popover.upgrade() {
                        popover.popdown();
                    }
                    if let Some(window) = window.upgrade() {
                        owner.popup(index, window.upcast_ref());
                    }
                });
                extensions_menu.append(&button);
            }
        });
        let menu = gtk::MenuButton::builder()
            .child(&chrome::glyph(Icon::Extensions))
            .popover(&popover)
            .build();
        menu.update_property(&[gtk::accessible::Property::Label("Extensions")]);
        menu.set_valign(gtk::Align::Center);
        bar.pack_end(&menu);
        let layout = gtk::Overlay::new();
        layout.set_child(Some(&b.pages));
        let (dock, strip) =
            chrome::navigation_dock(&bar, &panel, &popover, &protection, &save_strip);
        let keys = gtk::EventControllerKey::new();
        let weak = Rc::downgrade(&b);
        let escape_panel = panel.clone();
        keys.connect_key_pressed(move |_, key, _, _| {
            if key == gtk::gdk::Key::Escape
                && let Some(view) = weak.upgrade().and_then(|b| b.active())
            {
                view.grab_focus();
                escape_panel.set_reveal_child(false);
                return gtk::glib::Propagation::Stop;
            }
            gtk::glib::Propagation::Proceed
        });
        dock.add_controller(keys);
        // Keep the dock in its own strip under glass so KWin blurs the desktop
        // behind it; elsewhere it overlays the page, which needs no strip.
        let root = gtk::Overlay::new();
        if window.has_css_class("compositor-glass") {
            let frame = gtk::Box::new(gtk::Orientation::Vertical, 0);
            frame.append(&page_clip::PageClip::new(&layout));
            frame.append(&dock);
            root.set_child(Some(&frame));
        } else {
            layout.add_overlay(&dock);
            root.set_child(Some(&layout));
        }
        window.set_child(Some(&root));
        let progress = gtk::DrawingArea::new();
        progress.set_valign(gtk::Align::End);
        progress.set_content_height(3);
        progress.set_can_target(false);
        progress.set_visible(false);
        let owner = Rc::downgrade(&b);
        progress.set_draw_func(move |_, cr, width, height| {
            if let Some(view) = owner.upgrade().and_then(|browser| browser.active()) {
                cr.set_source_rgb(0.31, 0.58, 0.95);
                cr.rectangle(
                    0.,
                    0.,
                    f64::from(width) * view.estimated_load_progress(),
                    f64::from(height),
                );
                let _ = cr.fill();
            }
        });
        b.page_progress.set(Some(&progress));
        strip.add_overlay(&progress);
        downloads::install(&browsing_session(), &strip);
        strip.add_overlay(&save_strip);
        window.present();
        b.new_tab();
        b.start_menu_backgrounds();
        if blocking {
            let owner = b.clone();
            let initial_settings = settings.borrow().clone();
            let extra = extra.clone();
            let window = window.clone();
            gtk::glib::spawn_future_local(async move {
                match blocker::load(&initial_settings, &extra).await {
                    Ok(filters) => {
                        let loads = owner.pending_loads.take().unwrap_or_default();
                        for (view, uri) in loads {
                            if view.root().is_some() {
                                if let Some(manager) = view.user_content_manager() {
                                    filters.attach(&manager);
                                }
                                view.load_uri(&uri);
                            }
                        }
                        owner.filters.replace(filters);
                        shield.set_sensitive(true);
                    }
                    Err(error) => {
                        let dialog = gtk::AlertDialog::builder()
                            .message("Could not load content filters")
                            .detail(error.to_string())
                            .build();
                        dialog.choose(Some(&window), None::<&gio::Cancellable>, {
                            let window = window.clone();
                            move |_| window.close()
                        });
                    }
                }
            });
        }
        let startup = start.as_deref().unwrap_or(&prefs.startup_page);
        if !startup.is_empty() {
            b.navigate(startup)
        }
    });
    app.run_with_args::<&str>(&[]);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use gtk::glib;
    use std::time::Duration;
    #[test]
    fn address_and_search() {
        assert_eq!(
            destination("example.org", Default::default()).as_deref(),
            Some("https://example.org/")
        );
        assert_eq!(
            destination(" two words ", Default::default()).as_deref(),
            Some("https://duckduckgo.com/?q=two+words")
        );
        assert_eq!(destination(" ", Default::default()), None);
    }

    #[test]
    #[ignore = "requires a graphical session"]
    fn two_tabs_show_complete_titles() {
        gtk::init().unwrap();
        let browser = Browser {
            page_progress: Default::default(),
            preferences: Default::default(),
            pending_loads: Default::default(),
            filters: Default::default(),
            pages: gtk::Notebook::new(),
            address: gtk::Entry::new(),
            extensions: Default::default(),
            store_ids: Default::default(),
            store_busy: Default::default(),
        };
        browser.pages.set_scrollable(true);
        let window = gtk::Window::builder()
            .default_width(640)
            .default_height(240)
            .child(&browser.pages)
            .build();
        window.present();
        browser.new_tab();
        browser.new_tab();
        glib::MainContext::default().block_on(glib::timeout_future(Duration::from_millis(200)));
        for i in 0..2 {
            let tab = browser.pages.nth_page(Some(i)).unwrap();
            let heading = browser.pages.tab_label(&tab).unwrap();
            let label = heading
                .first_child()
                .unwrap()
                .next_sibling()
                .unwrap()
                .downcast::<gtk::Label>()
                .unwrap();
            assert!(label.width() > 0 && !label.layout().is_ellipsized());
        }
        window.close();
    }

    // Run with either reference package and an isolated XDG_DATA_HOME.
    #[test]
    #[ignore = "requires a graphical session and NB_TEST_CRX"]
    fn original_popup_changes_live_page() {
        gtk::init().unwrap();
        let path = std::env::var("NB_TEST_CRX").unwrap();
        let ext = Extension::load(std::path::Path::new(&path)).unwrap();
        let (class, control) = if ext.name.starts_with("NightShift") {
            ("nightshift-on", "masterSwitch")
        } else {
            ("smart-dark-mode-active", "globalToggle")
        };
        let browser = Rc::new(Browser {
            page_progress: Default::default(),
            preferences: Default::default(),
            pending_loads: Default::default(),
            filters: Default::default(),
            pages: gtk::Notebook::new(),
            address: gtk::Entry::new(),
            extensions: vec![ext].into(),
            store_ids: Default::default(),
            store_busy: Default::default(),
        });
        let window = gtk::Window::builder().child(&browser.pages).build();
        window.present();
        browser.new_tab();
        assert!(browser.active().is_none());
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let uri = format!("http://{}/", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            use std::io::{Read, Write};
            for stream in listener.incoming() {
                let mut stream = stream.unwrap();
                let mut buf = [0; 2048];
                if stream.read(&mut buf).unwrap_or(0) > 0 {
                    let _=stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nConnection: close\r\n\r\n<!doctype html><title>Local fixture</title><p>Page</p>");
                }
            }
        });
        browser.navigate(&uri);
        let view = browser.active().unwrap();
        let check = format!("document.documentElement.classList.contains('{class}')");
        glib::MainContext::default().block_on(async {
            wait_for(&view, &check).await;
            browser.popup(0, &window);
            let popup = gtk::Window::list_toplevels()
                .into_iter()
                .filter_map(|w| w.downcast::<gtk::Window>().ok())
                .find(|w| w.transient_for().as_ref() == Some(&window))
                .unwrap();
            let popup_view = popup.child().unwrap().downcast::<WebView>().unwrap();
            wait_for(
                &popup_view,
                &format!(
                    "document.readyState==='complete' && !!document.getElementById('{control}')"
                ),
            )
            .await;
            popup_view
                .evaluate_javascript_future(
                    &format!("document.getElementById('{control}').click(); true"),
                    None,
                    None,
                )
                .await
                .unwrap();
            wait_for(&view, &format!("!({check})")).await;
            assert!(
                browser.extensions.borrow()[0]
                    .state
                    .borrow()
                    .as_object()
                    .unwrap()
                    .values()
                    .any(|v| v == false)
            );
            popup.close();
            browser.close();
            assert!(browser.active().is_none());
        });
    }
    #[test]
    #[ignore = "requires a visible GTK window"]
    fn scroll_frame_timing() {
        gtk::init().unwrap();
        let browser = Rc::new(Browser {
            page_progress: Default::default(),
            preferences: Default::default(),
            pending_loads: Default::default(),
            filters: Default::default(),
            pages: gtk::Notebook::new(),
            address: gtk::Entry::new(),
            extensions: Default::default(),
            store_ids: Default::default(),
            store_busy: Default::default(),
        });
        let window = gtk::Window::builder()
            .default_width(1050)
            .default_height(720)
            .child(&browser.pages)
            .build();
        window.present();
        browser.new_tab();
        browser.navigate("about:blank");
        let view = browser.active().unwrap();
        glib::MainContext::default().block_on(async {
            wait_for(&view,"document.readyState === 'complete' && !!document.body").await;
            let timing = r#"
                document.body.innerHTML = '<article><h2>Nothing Browser</h2><p>Scroll sample</p></article>'.repeat(350);
                window.samples = [];
                let previous;
                function tick(now) {
                    if (previous !== undefined) samples.push(now-previous);
                    previous=now;
                    scrollBy(0,16);
                    if(samples.length<90) requestAnimationFrame(tick);
                }
                requestAnimationFrame(tick); true
            "#;
            view.evaluate_javascript_future(timing,None,None).await.unwrap();
            wait_for(&view,"samples.length === 90").await;
            let stats=view.evaluate_javascript_future("samples.sort((a,b)=>a-b); JSON.stringify({median_ms:samples[45],p95_ms:samples[85],web_audio:typeof AudioContext})",None,None).await.unwrap();
            println!("{stats}");
        });
    }

    #[test]
    #[ignore = "interactive Linux memory measurement"]
    fn memory_workload() {
        gtk::init().unwrap();
        let browser = Rc::new(Browser {
            page_progress: Default::default(),
            preferences: Default::default(),
            pending_loads: Default::default(),
            filters: Default::default(),
            pages: gtk::Notebook::new(),
            address: gtk::Entry::new(),
            extensions: Default::default(),
            store_ids: Default::default(),
            store_busy: Default::default(),
        });
        let window = gtk::Window::builder()
            .default_width(1050)
            .default_height(720)
            .child(&browser.pages)
            .build();
        window.present();
        glib::MainContext::default().block_on(async {
            browser.new_tab();
            sample("empty").await;
            for i in 0..5 {
                if i > 0 {
                    browser.new_tab();
                }
                browser.navigate("data:text/html,<title>Fixture</title><p>Memory sample</p>");
                wait_for(&browser.active().unwrap(), "document.title==='Fixture'").await;
            }
            sample("five").await;
            for _ in 0..4 {
                browser.close();
            }
            sample("one").await;
        });
    }

    async fn sample(stage: &str) {
        glib::timeout_future(Duration::from_secs(3)).await;
        println!("MEMORY {stage} {}", std::process::id());
        glib::timeout_future(Duration::from_secs(2)).await;
    }

    async fn wait_for(view: &WebView, condition: &str) {
        for _ in 0..100 {
            if view
                .evaluate_javascript_future(condition, None, None)
                .await
                .is_ok_and(|v| v.to_boolean())
            {
                return;
            }
            glib::timeout_future(Duration::from_millis(50)).await;
        }
        panic!("Timed out checking {condition}");
    }
}
