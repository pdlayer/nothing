use adblock::lists::{FilterSet, ParseOptions};
use anyhow::{Context, Result};
use gtk::glib;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Write,
    path::PathBuf,
};
use webkit6::{UserContentFilter, UserContentFilterStore};

#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    pub enabled: bool,
    pub easylist: bool,
    pub exceptions: BTreeSet<String>,
    pub lists: Vec<LocalList>,
    #[serde(default)]
    pub hidden: BTreeMap<String, BTreeSet<String>>,
}
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct LocalList {
    pub name: String,
    pub text: String,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: true,
            easylist: true,
            exceptions: BTreeSet::new(),
            lists: vec![],
            hidden: Default::default(),
        }
    }
}
impl Settings {
    fn path() -> PathBuf {
        glib::user_config_dir().join("nothing-browser/adblock.json")
    }
    pub fn read() -> Result<Self> {
        match std::fs::read(Self::path()) {
            Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e.into()),
        }
    }
    pub fn save(&self) -> Result<()> {
        let path = Self::path();
        let dir = path.parent().unwrap();
        std::fs::create_dir_all(dir)?;
        let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
        serde_json::to_writer(&mut tmp, self)?;
        tmp.flush()?;
        tmp.persist(path)?;
        Ok(())
    }
}

/// Split filters so a picked element only recompiles the small cosmetic set.
#[derive(Default)]
pub struct Filters {
    pub base: Option<UserContentFilter>,
    pub cosmetic: Vec<webkit6::UserStyleSheet>,
}

impl Filters {
    pub fn attach(&self, manager: &webkit6::UserContentManager) {
        for sheet in &self.cosmetic {
            manager.add_style_sheet(sheet);
        }
        if let Some(filter) = &self.base {
            manager.add_filter(filter);
        }
    }
}

fn escaped(host: &str) -> String {
    host.replace('.', r"\.")
        .replace('[', r"\[")
        .replace(']', r"\]")
}

fn digest(prefix: &str, parts: &[&[u8]]) -> String {
    let mut hash = glib::Checksum::new(glib::ChecksumType::Sha256).unwrap();
    for part in parts {
        hash.update(part);
    }
    format!("{prefix}-{}", hash.string().unwrap())
}

// Use the top document URL so a site exception also covers its subresources.
fn compile(source: String, exceptions: BTreeSet<String>) -> Result<Vec<u8>> {
    let mut set = FilterSet::new(true);
    set.add_filter_list(source, ParseOptions::default());
    let (rules, _) = set
        .into_content_blocking()
        .map_err(|_| anyhow::anyhow!("Cannot convert filters"))?;
    use serde::ser::{SerializeSeq, Serializer};
    let mut json = Vec::new();
    let mut serializer = serde_json::Serializer::new(&mut json);
    let mut sequence = serializer.serialize_seq(None)?;
    for rule in rules {
        sequence.serialize_element(&rule)?;
    }
    for host in exceptions {
        let escaped = escaped(&host);
        sequence.serialize_element(&serde_json::json!({"trigger":{"url-filter":".*","if-top-url":[format!("^https?://{escaped}[:/]")]},"action":{"type":"ignore-previous-rules"}}))?;
    }
    sequence.end()?;
    Ok(json)
}

pub fn cosmetic(settings: &Settings) -> Vec<webkit6::UserStyleSheet> {
    settings
        .hidden
        .iter()
        .filter(|(host, _)| settings.enabled && !settings.exceptions.contains(*host))
        .map(|(host, selectors)| {
            let css = selectors
                .iter()
                .map(|s| format!("{s} {{ display: none !important; }}\n"))
                .collect::<String>();
            webkit6::UserStyleSheet::new(
                &css,
                webkit6::UserContentInjectedFrames::TopFrame,
                webkit6::UserStyleLevel::User,
                &[&format!("http://{host}/*"), &format!("https://{host}/*")],
                &[],
            )
        })
        .collect()
}

async fn cached(
    store: &UserContentFilterStore,
    key: &str,
    build: impl FnOnce() -> Result<Vec<u8>> + Send + 'static,
) -> Result<UserContentFilter> {
    if let Ok(filter) = store.load_future(key).await {
        return Ok(filter);
    }
    let json = gtk::gio::spawn_blocking(build)
        .await
        .map_err(|_| anyhow::anyhow!("Filter compiler stopped"))??;
    store
        .save_future(key, &glib::Bytes::from_owned(json))
        .await
        .context("WebKit could not compile the content filter")
}

pub async fn load(settings: &Settings, extra: &[String]) -> Result<Filters> {
    if !settings.enabled {
        return Ok(Filters::default());
    }
    let mut source = String::from(include_str!("../assets/filters/default.txt"));
    if settings.easylist {
        source.push('\n');
        source.push_str(include_str!("../assets/filters/easylist.txt"));
    }
    for list in settings.lists.iter().map(|l| &l.text).chain(extra) {
        source.push('\n');
        source.push_str(list);
    }
    let dir = glib::user_cache_dir().join("nothing-browser/content-filters");
    std::fs::create_dir_all(&dir)?;
    let store = UserContentFilterStore::new(dir.to_str().context("Invalid filter cache path")?);
    let exceptions = serde_json::to_vec(&settings.exceptions)?;
    let key = digest("adblock-v4-base", &[source.as_bytes(), &[0], &exceptions]);
    let exceptions = settings.exceptions.clone();
    let base = cached(&store, &key, move || compile(source, exceptions)).await?;
    Ok(Filters {
        base: Some(base),
        cosmetic: cosmetic(settings),
    })
}
