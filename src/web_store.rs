use anyhow::{Context, Result, ensure};
use std::{io::Write, process::Command};
use url::Url;

pub fn listing_id(input: &str) -> Result<String> {
    let listing = Url::parse(input).context("Expected a Chrome Web Store listing URL")?;
    ensure!(
        listing.scheme() == "https"
            && listing.host_str() == Some("chromewebstore.google.com")
            && listing.path().starts_with("/detail/"),
        "Expected a Chrome Web Store listing URL"
    );
    let id = listing
        .path_segments()
        .and_then(|mut parts| parts.rfind(|p| !p.is_empty()))
        .context("Missing extension ID")?;
    ensure!(
        id.len() == 32 && id.bytes().all(|c| (b'a'..=b'p').contains(&c)),
        "Invalid Chrome Web Store extension ID"
    );
    Ok(id.to_owned())
}

pub const WORLD: &str = "nothing-browser-web-store";

pub fn directory() -> std::path::PathBuf {
    gtk::glib::user_data_dir().join("nothing-browser/store-extensions")
}

pub fn fetch(input: &str) -> Result<tempfile::NamedTempFile> {
    let id = listing_id(input)?;
    let mut endpoint = Url::parse("https://clients2.google.com/service/update2/crx")?;
    endpoint
        .query_pairs_mut()
        .append_pair("response", "redirect")
        .append_pair("prodversion", "130.0.0.0")
        .append_pair("acceptformat", "crx3")
        .append_pair("x", &format!("id={id}&uc"));
    let redirect = curl(&[
        "--output",
        "/dev/null",
        "--write-out",
        "%{redirect_url}",
        endpoint.as_str(),
    ])?;
    let package = Url::parse(std::str::from_utf8(&redirect)?)
        .context("Store did not offer a CRX download")?;
    ensure!(
        package.scheme() == "https"
            && package.host_str() == Some("clients2.googleusercontent.com")
            && package.port().is_none()
            && package.path().starts_with("/crx/blobs/"),
        "Unexpected Chrome Web Store download location"
    );
    let bytes = curl(&[package.as_str()])?;
    ensure!(
        bytes.starts_with(b"Cr24\x03\x00\x00\x00"),
        "Store did not return a CRX3 package"
    );
    let dir = directory();
    std::fs::create_dir_all(&dir)?;
    let mut file = tempfile::NamedTempFile::new_in(dir)?;
    file.write_all(&bytes)?;
    Ok(file)
}

fn curl(args: &[&str]) -> Result<Vec<u8>> {
    let result = Command::new("curl")
        .args([
            "--disable",
            "--silent",
            "--show-error",
            "--fail",
            "--proto",
            "=https",
            "--max-time",
            "60",
            "--max-filesize",
            "33554432",
        ])
        .args(args)
        .output()
        .context("Chrome Web Store downloads require curl")?;
    ensure!(
        result.status.success(),
        "Chrome Web Store request failed: {}",
        String::from_utf8_lossy(&result.stderr).trim()
    );
    Ok(result.stdout)
}
