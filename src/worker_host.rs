use anyhow::{Context, Result, ensure};
use gtk::glib;
use serde_json::json;
use std::{
    fs,
    path::{Component, Path},
};
use url::Url;
use webkit6::soup::{self, prelude::*};

pub struct WorkerHost {
    server: soup::Server,
    pub page: Url,
}

impl Drop for WorkerHost {
    fn drop(&mut self) {
        ServerExt::disconnect(&self.server);
    }
}

impl WorkerHost {
    pub fn start(root: &Path, entry: &str, module: bool, bootstrap: String) -> Result<Self> {
        let server = soup::Server::builder().raw_paths(true).build();
        server.listen_local(0, soup::ServerListenOptions::IPV4_ONLY)?;
        let port = server
            .uris()
            .first()
            .context("Missing worker server address")?
            .port();
        let prefix = format!("/{}/", glib::uuid_string_random());
        let base = Url::parse(&format!("http://127.0.0.1:{port}{prefix}"))?;
        let page = base.join("host.html")?;
        let resources = base.join("resources/")?;
        let prelude = base.join("bootstrap.js")?;
        let mut entry_url = resources.clone();
        entry_url
            .path_segments_mut()
            .map_err(|_| anyhow::anyhow!("Invalid resource URL"))?
            .pop_if_empty()
            .extend(entry.split('/'));
        let script = entry_url.join(&format!(".nothing-{}.js", glib::uuid_string_random()))?;
        let script_path = script
            .path()
            .strip_prefix(&prefix)
            .context("Invalid worker URL")?
            .to_owned();
        let source = if module {
            format!(
                "import {};\nimport {};",
                json!(prelude.as_str()),
                json!(entry_url.as_str())
            )
        } else {
            format!(
                "importScripts({}, {});",
                json!(prelude.as_str()),
                json!(entry_url.as_str())
            )
        };
        let bridge = include_str!("worker_client.js")
            .replace("NB_WORKER_URL", &json!(script.as_str()).to_string())
            .replace("NB_WORKER_SCOPE", &json!(base.as_str()).to_string())
            .replace(
                "NB_WORKER_TYPE",
                if module { "\"module\"" } else { "\"classic\"" },
            );
        let bootstrap = include_str!("worker_runtime.js")
            .replace("NB_WORKER_CLIENT", &json!(page.as_str()).to_string())
            .replace(
                "NB_WORKER_RESOURCES",
                &json!(resources.as_str()).to_string(),
            )
            + "\n"
            + &bootstrap;
        let root = root.to_path_buf();
        server.add_handler(None, move |_, request, path, _| {
            let valid_host = request.uri().is_some_and(|uri| {
                uri.host().as_deref() == Some("127.0.0.1") && uri.port() == port
            });
            let cross_site = request
                .request_headers()
                .and_then(|headers| headers.one("Sec-Fetch-Site"))
                .is_some_and(|site| !matches!(site.as_str(), "same-origin" | "none"));
            let Some(path) = path.strip_prefix(&prefix).filter(|_| {
                valid_host && !cross_site && request.method().as_deref() == Some("GET")
            }) else {
                request.set_status(404, None);
                return;
            };
            let resource;
            let (kind, data) = match path {
                "host.html" => (
                    "text/html",
                    b"<!doctype html><script src=host.js></script>".as_slice(),
                ),
                "host.js" => ("text/javascript", bridge.as_bytes()),
                path if path == script_path => ("text/javascript", source.as_bytes()),
                "bootstrap.js" => ("text/javascript", bootstrap.as_bytes()),
                path if path.starts_with("resources/") => {
                    let result = (|| -> Result<_> {
                        let decoded = glib::Uri::unescape_string(&path["resources/".len()..], None)
                            .context("Invalid resource path")?;
                        let relative = Path::new(decoded.as_str());
                        ensure!(
                            relative
                                .components()
                                .all(|p| matches!(p, Component::Normal(_))),
                            "Invalid path"
                        );
                        let kind = match relative
                            .extension()
                            .and_then(|extension| extension.to_str())
                        {
                            Some("js" | "mjs") => "text/javascript",
                            Some("json") => "application/json",
                            Some("wasm") => "application/wasm",
                            _ => "application/octet-stream",
                        };
                        Ok((kind, fs::read(root.join(relative))?))
                    })();
                    let Ok((kind, bytes)) = result else {
                        request.set_status(404, None);
                        return;
                    };
                    resource = bytes;
                    (kind, resource.as_slice())
                }
                _ => {
                    request.set_status(404, None);
                    return;
                }
            };
            if let Some(headers) = request.response_headers() {
                if path == script_path {
                    headers.append("Service-Worker-Allowed", &prefix);
                }
                headers.append("Cache-Control", "no-store");
                headers.append("Cross-Origin-Resource-Policy", "same-origin");
                headers.append("Referrer-Policy", "no-referrer");
                headers.append("X-Content-Type-Options", "nosniff");
                headers.append(
                    "Content-Security-Policy",
                    "frame-ancestors 'none'; base-uri 'none'",
                );
            }
            request.set_status(200, None);
            request.set_response(Some(kind), soup::MemoryUse::Copy, data);
        });
        Ok(Self { server, page })
    }
}
