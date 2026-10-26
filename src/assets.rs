//! The Safari client, embedded in the executable: one file to ship, and nothing on
//! disk anyone could swap. Only these paths are ever served (no path traversal).

pub struct Asset {
    pub path: &'static str,
    pub mime: &'static str,
    pub body: &'static [u8],
}

macro_rules! asset {
    ($name:literal, $mime:literal) => {
        Asset { path: concat!("/", $name), mime: $mime, body: include_bytes!(concat!("../web/", $name)) }
    };
}

pub const ASSETS: [Asset; 11] = [
    asset!("index.html", "text/html; charset=utf-8"),
    asset!("app.js", "text/javascript; charset=utf-8"),
    asset!("input.js", "text/javascript; charset=utf-8"),
    asset!("protocol.js", "text/javascript; charset=utf-8"),
    asset!("video.js", "text/javascript; charset=utf-8"),
    asset!("setup.html", "text/html; charset=utf-8"),
    asset!("setup.js", "text/javascript; charset=utf-8"),
    asset!("ui.css", "text/css; charset=utf-8"),
    asset!("icon.svg", "image/svg+xml"),
    asset!("apple-touch-icon.png", "image/png"),
    asset!("manifest.webmanifest", "application/manifest+json"),
];

/// Files the plain-HTTP pairing server may serve.
pub const SETUP_FILES: [&str; 4] = ["/setup.html", "/setup.js", "/ui.css", "/icon.svg"];

pub fn find(path: &str) -> Option<&'static Asset> {
    ASSETS.iter().find(|a| a.path == path)
}

#[cfg(test)]
mod tests {
    use super::*;

    const HTML: &str = "text/html; charset=utf-8";
    const JS: &str = "text/javascript; charset=utf-8";

    #[test]
    fn every_asset_is_embedded_with_a_type() {
        for asset in &ASSETS {
            assert!(!asset.body.is_empty(), "{} is empty", asset.path);
            assert!(asset.mime.contains('/'));
        }
        assert_eq!(find("/index.html").unwrap().mime, HTML);
        assert_eq!(find("/app.js").unwrap().mime, JS);
        assert!(find("/../Cargo.toml").is_none());
        assert!(SETUP_FILES.iter().all(|f| find(f).is_some()));
    }
}
