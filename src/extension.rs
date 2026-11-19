use anyhow::{Context, Result, bail, ensure};
use gtk::glib;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    cell::RefCell,
    fs,
    io::{Cursor, Read},
    path::{Component, Path, PathBuf},
};
use webkit6::{UserContentInjectedFrames as Frames, UserScript, UserScriptInjectionTime as Time};
use zip::ZipArchive;

#[derive(Deserialize)]
struct Manifest {
    manifest_version: u8,
    #[serde(default)]
    permissions: Vec<String>,
    name: String,
    default_locale: Option<String>,
    #[serde(default)]
    content_scripts: Vec<Content>,
    action: Option<Action>,
    background: Option<Background>,
}
#[derive(Deserialize)]
pub struct Background {
    service_worker: String,
    #[serde(rename = "type")]
    kind: Option<String>,
}

#[derive(Deserialize)]
struct Action {
    default_popup: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Content {
    matches: Vec<String>,
    #[serde(default)]
    js: Vec<String>,
    #[serde(default)]
    css: Vec<String>,
    #[serde(default)]
    exclude_matches: Vec<String>,
    run_at: Option<String>,
    #[serde(default)]
    all_frames: bool,
}

struct ScriptGroup {
    time: Time,
    frames: Frames,
    allow: Vec<String>,
    exclude: Vec<String>,
    sources: Vec<String>,
    css: Vec<String>,
}

fn patterns(input: &[String]) -> Result<Vec<String>> {
    let mut result = Vec::new();
    for pattern in input {
        if pattern == "<all_urls>" {
            result.extend(["http://*/*".into(), "https://*/*".into()]);
            continue;
        }
        let (scheme, rest) = pattern.split_once("://").context("Invalid match pattern")?;
        let (host, _) = rest.split_once('/').context("Missing match path")?;
        ensure!(
            matches!(scheme, "http" | "https" | "*")
                && !host.is_empty()
                && !host.contains([':', '@', '?', '#'])
                && !host.trim_start_matches("*.").contains('*')
                || matches!(scheme, "http" | "https" | "*") && host == "*",
            "Unsupported match pattern {pattern}"
        );
        if scheme == "*" {
            result.extend([format!("http://{rest}"), format!("https://{rest}")]);
        } else {
            result.push(pattern.clone());
        }
    }
    Ok(result)
}

#[derive(Clone, Deserialize)]
pub struct MenuItem {
    pub id: Value,
    pub title: String,
    #[serde(default = "page_context")]
    pub contexts: Vec<String>,
}

fn page_context() -> Vec<String> {
    vec!["page".into()]
}

#[derive(Default)]
pub struct BackgroundActivity {
    pub last: std::cell::Cell<Option<std::time::Instant>>,
    pub pending: std::cell::Cell<usize>,
}

impl BackgroundActivity {
    pub fn touch(&self) {
        self.last.set(Some(std::time::Instant::now()));
    }

    pub fn begin(self: &std::rc::Rc<Self>) -> BackgroundRequest {
        self.pending.set(self.pending.get() + 1);
        self.touch();
        BackgroundRequest(self.clone())
    }
}

pub struct BackgroundRequest(std::rc::Rc<BackgroundActivity>);

impl Drop for BackgroundRequest {
    fn drop(&mut self) {
        self.0.pending.set(self.0.pending.get() - 1);
        self.0.touch();
    }
}

pub struct Extension {
    pub name: String,
    pub can_read_tabs: bool,
    pub can_script_active_tab: bool,
    messages: serde_json::Map<String, Value>,
    pub world: String,
    pub popup: Option<url::Url>,
    background: Option<Background>,
    pub background_host: RefCell<Option<crate::worker_host::WorkerHost>>,
    pub can_use_offscreen: bool,
    pub can_use_menus: bool,
    pub menu_items: RefCell<Vec<MenuItem>>,
    pub offscreen_view: RefCell<Option<(webkit6::WebView, url::Url)>>,
    pub background_view: RefCell<Option<webkit6::WebView>>,
    pub background_activity: std::rc::Rc<BackgroundActivity>,
    pub background_initialized: std::cell::Cell<bool>,
    scripts: Vec<ScriptGroup>,
    pub state: RefCell<Value>,
    settings: PathBuf,
    resources: tempfile::TempDir,
}

fn relative(name: &str) -> Result<&Path> {
    let path = Path::new(name);
    ensure!(
        !name.is_empty() && path.components().all(|c| matches!(c, Component::Normal(_))),
        "Invalid package path {name}"
    );
    Ok(path)
}

fn archive(data: &[u8]) -> Result<&[u8]> {
    ensure!(data.starts_with(b"Cr24"), "Expected a CRX package");
    let number = |at: usize| -> Result<usize> {
        Ok(u32::from_le_bytes(
            data.get(at..at + 4)
                .context("Incomplete CRX header")?
                .try_into()?,
        ) as usize)
    };
    let offset = match number(4)? {
        2 => 16usize
            .checked_add(number(8)?)
            .and_then(|n| n.checked_add(number(12).ok()?)),
        3 => 12usize.checked_add(number(8)?),
        _ => bail!("Unsupported CRX version"),
    }
    .context("Invalid header length")?;
    let payload = data.get(offset..).context("Incomplete CRX")?;
    ensure!(payload.starts_with(b"PK\x03\x04"), "Missing ZIP payload");
    Ok(payload)
}

impl Extension {
    pub fn load(path: &Path) -> Result<Self> {
        const LIMIT: u64 = 32 << 20;
        let data = read_limited(fs::File::open(path)?, LIMIT)?;
        let world = glib::compute_checksum_for_data(glib::ChecksumType::Sha256, &data)
            .unwrap()
            .to_string();
        let mut zip = ZipArchive::new(Cursor::new(archive(&data)?))?;
        ensure!(zip.len() <= 1024, "Too many archive entries");
        let dir = tempfile::tempdir()?;
        let mut remaining = LIMIT;
        for i in 0..zip.len() {
            let entry = zip.by_index(i)?;
            let dest = dir.path().join(relative(entry.name())?);
            if entry.is_dir() {
                fs::create_dir_all(dest)?;
                continue;
            }
            let bytes = read_limited(entry, remaining)?;
            remaining -= bytes.len() as u64;
            fs::create_dir_all(dest.parent().unwrap())?;
            fs::write(dest, bytes)?;
        }
        let manifest: Manifest =
            serde_json::from_slice(&fs::read(dir.path().join("manifest.json"))?)?;
        let mut manifest = manifest;
        let mut messages = serde_json::Map::new();
        let mut locales = manifest.default_locale.iter().cloned().collect::<Vec<_>>();
        locales.extend(glib::language_names().iter().rev().map(|lang| {
            lang.split(['.', '@'])
                .next()
                .unwrap_or_default()
                .replace('-', "_")
        }));
        for locale in locales {
            let path = dir
                .path()
                .join("_locales")
                .join(relative(&locale)?)
                .join("messages.json");
            if let Ok(bytes) = fs::read(path)
                && let Ok(Value::Object(catalog)) = serde_json::from_slice::<Value>(&bytes)
            {
                messages.extend(
                    catalog
                        .into_iter()
                        .map(|(key, value)| (key.to_ascii_lowercase(), value)),
                );
            }
        }
        if let Some(key) = manifest
            .name
            .strip_prefix("__MSG_")
            .and_then(|value| value.strip_suffix("__"))
            && let Some(message) = messages
                .get(&key.to_ascii_lowercase())
                .and_then(|value| value["message"].as_str())
        {
            manifest.name = message.to_owned();
        }
        ensure!(manifest.manifest_version == 3, "Expected Manifest V3");
        if let Some(background) = &manifest.background {
            ensure!(
                dir.path()
                    .join(relative(&background.service_worker)?)
                    .is_file(),
                "Missing background script"
            );
        }
        let background = manifest.background;
        let mut scripts = Vec::new();
        for group in manifest.content_scripts {
            let time = match group.run_at.as_deref() {
                Some("document_start") => Time::Start,
                Some("document_end") | Some("document_idle") | None => Time::End,
                _ => bail!("Unknown run_at"),
            };
            let allow = patterns(&group.matches)?;
            ensure!(!allow.is_empty(), "Empty content script match list");
            scripts.push(ScriptGroup {
                time,
                frames: if group.all_frames {
                    Frames::AllFrames
                } else {
                    Frames::TopFrame
                },
                allow,
                exclude: patterns(&group.exclude_matches)?,
                css: group
                    .css
                    .iter()
                    .map(|file| -> Result<_> {
                        Ok(fs::read_to_string(dir.path().join(relative(file)?))?)
                    })
                    .collect::<Result<_>>()?,
                sources: group
                    .js
                    .iter()
                    .map(|file| -> Result<_> {
                        Ok(fs::read_to_string(dir.path().join(relative(file)?))?)
                    })
                    .collect::<Result<_>>()?,
            });
        }
        let popup = manifest
            .action
            .and_then(|a| a.default_popup)
            .map(|name| -> Result<_> {
                let path = dir.path().join(relative(&name)?);
                ensure!(path.is_file(), "Missing popup");
                url::Url::from_file_path(path).map_err(|_| anyhow::anyhow!("Invalid popup path"))
            })
            .transpose()?;
        let settings = glib::user_data_dir()
            .join("nothing-browser")
            .join(format!("{world}.json"));
        let state = match fs::read(&settings) {
            Ok(bytes) => serde_json::from_slice(&bytes)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => json!({}),
            Err(e) => return Err(e.into()),
        };
        ensure!(state.is_object(), "Invalid extension settings");
        Ok(Self {
            name: manifest.name,
            can_read_tabs: manifest.permissions.iter().any(|p| p == "tabs"),
            can_script_active_tab: ["scripting", "activeTab"]
                .iter()
                .all(|name| manifest.permissions.iter().any(|p| p == name)),
            messages,
            world,
            popup,
            background,
            background_host: RefCell::default(),
            can_use_offscreen: manifest.permissions.iter().any(|p| p == "offscreen"),
            can_use_menus: manifest.permissions.iter().any(|p| p == "contextMenus"),
            menu_items: RefCell::default(),
            offscreen_view: RefCell::default(),
            background_view: RefCell::default(),
            background_activity: Default::default(),
            background_initialized: Default::default(),
            scripts,
            state: RefCell::new(state),
            settings,
            resources: dir,
        })
    }
    pub fn worker_host(&self) -> Result<Option<crate::worker_host::WorkerHost>> {
        let Some(background) = &self.background else {
            return Ok(None);
        };
        let menus = include_str!("context_menus.js").replace(
            "NB_CAN_USE_MENUS",
            if self.can_use_menus { "true" } else { "false" },
        );
        let source = format!("{}\n{}\n{menus}", self.bootstrap(), self.bridge_source());
        crate::worker_host::WorkerHost::start(
            self.resources.path(),
            &background.service_worker,
            background.kind.as_deref() == Some("module"),
            source,
        )
        .map(Some)
    }

    pub fn offscreen_page(&self, name: &str) -> Result<url::Url> {
        let path = self.resources.path().join(relative(name)?);
        ensure!(
            path.is_file()
                && path
                    .extension()
                    .is_some_and(|ext| matches!(ext.to_str(), Some("html" | "htm"))),
            "Expected an HTML document contained in the extension"
        );
        url::Url::from_file_path(path)
            .map_err(|_| anyhow::anyhow!("Invalid offscreen document path"))
    }

    pub fn resource_base(&self) -> url::Url {
        url::Url::from_directory_path(self.resources.path()).expect("absolute extension directory")
    }

    pub fn bootstrap(&self) -> String {
        format!(
            "globalThis.nbState={};\n{}\n{}",
            self.state.borrow(),
            include_str!("storage.js"),
            include_str!("i18n.js").replace("NB_MESSAGES_JSON", &json!(self.messages).to_string())
        )
    }
    pub fn bridge_name(&self) -> String {
        format!("nbRuntime{}", self.world)
    }

    pub fn bridge_source(&self) -> String {
        let bridge = include_str!("runtime_bridge.js")
            .replace("NB_RUNTIME_HANDLER", &json!(self.bridge_name()).to_string())
            .replace(
                "NB_RUNTIME_BASE",
                &json!(self.resource_base().as_str()).to_string(),
            );
        format!("{bridge}\n{}", include_str!("runtime_ports.js"))
    }

    pub fn attach(&self, manager: &webkit6::UserContentManager) {
        for group in &self.scripts {
            let allow: Vec<_> = group.allow.iter().map(String::as_str).collect();
            let exclude: Vec<_> = group.exclude.iter().map(String::as_str).collect();
            let source = format!(
                "if (!globalThis.chrome) {{ {}\n{} }}",
                self.bootstrap(),
                self.bridge_source()
            );
            for (source, time) in std::iter::once((source.as_str(), Time::Start)).chain(
                group
                    .sources
                    .iter()
                    .map(|source| (source.as_str(), group.time)),
            ) {
                manager.add_script(&UserScript::for_world(
                    source,
                    group.frames,
                    time,
                    &self.world,
                    &allow,
                    &exclude,
                ));
            }
        }
    }
    pub fn attach_styles(&self, manager: &webkit6::UserContentManager) {
        for group in &self.scripts {
            let allow: Vec<_> = group.allow.iter().map(String::as_str).collect();
            let exclude: Vec<_> = group.exclude.iter().map(String::as_str).collect();
            for css in &group.css {
                manager.add_style_sheet(&webkit6::UserStyleSheet::for_world(
                    css,
                    group.frames,
                    webkit6::UserStyleLevel::Author,
                    &self.world,
                    &allow,
                    &exclude,
                ));
            }
        }
    }

    pub fn update(&self, patch: &Value) -> Result<()> {
        let mut state = self.state.borrow().clone();
        state.as_object_mut().unwrap().extend(
            patch
                .as_object()
                .context("Expected settings object")?
                .clone(),
        );
        let bytes = serde_json::to_vec(&state)?;
        ensure!(bytes.len() <= 1024 * 1024, "Settings exceed 1 MiB");
        let parent = self.settings.parent().unwrap();
        fs::create_dir_all(parent)?;
        let tmp = tempfile::NamedTempFile::new_in(parent)?;
        fs::write(tmp.path(), bytes)?;
        tmp.persist(&self.settings)?;
        self.state.replace(state);
        Ok(())
    }
}
fn read_limited(reader: impl Read, limit: u64) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader.take(limit + 1).read_to_end(&mut bytes)?;
    ensure!(bytes.len() as u64 <= limit, "Package exceeds size limit");
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn content_match_patterns_keep_scheme_and_path_scope() {
        assert_eq!(
            patterns(&["*://*.example.org/docs/*".to_owned()]).unwrap(),
            [
                "http://*.example.org/docs/*",
                "https://*.example.org/docs/*"
            ]
        );
        assert!(patterns(&["file:///*".to_owned()]).is_err());
        assert!(patterns(&["https://exam*ple.org/*".to_owned()]).is_err());
    }

    #[test]
    fn finds_crx_payload_and_rejects_truncation() {
        for (version, header) in [
            (2u32, vec![2, 0, 0, 0, 1, 0, 0, 0, 7, 8, 9]),
            (3, vec![2, 0, 0, 0, 7, 8]),
        ] {
            let mut data = b"Cr24".to_vec();
            data.extend(version.to_le_bytes());
            data.extend(header);
            assert!(archive(&data).is_err());
            data.extend(b"PK\x03\x04");
            assert_eq!(archive(&data).unwrap(), b"PK\x03\x04");
        }
        assert!(relative("../outside").is_err());
        assert!(relative("/outside").is_err());
    }
}
