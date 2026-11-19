# Nothing Browser

A minimalist web browser for Linux, built with Rust, GTK 4, and WebKitGTK. This project is vibe-coded with AI assistance.

- A retractable bottom toolbar and draggable tabs with site icons
- Built-in ad blocking with EasyList, per-site exceptions, and an element picker
- Support for a subset of Chrome Manifest V3 extensions, with installation from the Chrome Web Store or a local CRX package
- Configurable default search engine and startup page

Extension compatibility varies, and some Chrome APIs are still missing.

## Build and run

Install a current stable Rust toolchain, `pkg-config`, and the development packages for GTK 4 (4.10 or later), WebKitGTK 6.0, and GStreamer 1.0. On a Wayland compositor that supports the background effect protocol, GTK 4.24 or later enables system blur.

```sh
git clone https://github.com/pdlayer/nothing.git
cd nothing
cargo run --locked
```

Open Settings with **Ctrl+,** or the gear in the bottom toolbar. Use **Ctrl+L** to focus the address field, **Ctrl+T** to open a tab, and **Ctrl+W** to close it.

## Extensions

Open a public Chrome Web Store listing in Nothing, select **Add to Nothing**, and confirm the installation. The store flow requires `curl`. The puzzle menu in the toolbar opens extension popups.

To load a local extension for the current session:

```sh
cargo run --locked -- --extension /path/to/extension.crx
```

Store downloads use HTTPS. CRX signatures are currently unverified, so install packages from publishers you trust.

## Ad blocking

The shield in the bottom toolbar controls blocking on the current site, custom filter lists, and element hiding. EasyList comes bundled with the browser. You can also load a local list at launch:

```sh
cargo run --locked -- --filter-list /path/to/filters.txt
```

The filter engine supports a subset of ABP and uBlock Origin syntax. Scriptlets and procedural cosmetic filters are currently unsupported.

## Development

```sh
cargo fmt --check
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
```

Asset and filter attributions are listed in [Material Symbols](assets/material/NOTICE) and [EasyList](assets/filters/NOTICE).
