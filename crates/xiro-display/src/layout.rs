//! Where the user last put the virtual monitor. Format `{"x": 1366, "y": 0}` (pretty-printed);
//! keep it stable so existing installs keep their spot.

use std::fs;
use std::path::PathBuf;

use crate::gdi::Position;

pub(crate) struct LayoutFile {
    file: PathBuf,
}

impl LayoutFile {
    pub(crate) fn new(file: PathBuf) -> Self {
        Self { file }
    }

    /// `None` when nothing (valid) was saved yet.
    pub(crate) fn get(&self) -> Option<Position> {
        parse(&fs::read_to_string(&self.file).ok()?)
    }

    /// Atomic (temp file + rename): a crash mid-write leaves the previous file.
    pub(crate) fn set(&self, position: Position) -> std::io::Result<()> {
        if let Some(dir) = self.file.parent() {
            fs::create_dir_all(dir)?;
        }
        let mut tmp = self.file.as_os_str().to_owned();
        tmp.push(".tmp");
        let tmp = PathBuf::from(tmp);
        fs::write(&tmp, format!("{{\n  \"x\": {},\n  \"y\": {}\n}}", position.x, position.y))?;
        fs::rename(&tmp, &self.file)
    }
}

/// `x` and `y` from a flat JSON object of integers; anything else counts as not saved.
fn parse(text: &str) -> Option<Position> {
    let body = text.trim().strip_prefix('{')?.strip_suffix('}')?;
    let (mut x, mut y) = (None, None);
    for member in body.split(',').filter(|m| !m.trim().is_empty()) {
        let (key, value) = member.split_once(':')?;
        let key = key.trim().strip_prefix('"')?.strip_suffix('"')?;
        let value: i32 = value.trim().parse().ok()?;
        match key {
            "x" => x = Some(value),
            "y" => y = Some(value),
            _ => {}
        }
    }
    Some(Position { x: x?, y: y? })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_what_the_node_host_wrote() {
        assert_eq!(parse("{\n  \"x\": 1366,\n  \"y\": 0\n}"), Some(Position { x: 1366, y: 0 }));
        assert_eq!(parse(r#"{"x":-2360,"y":100}"#), Some(Position { x: -2360, y: 100 }));
    }

    #[test]
    fn anything_else_counts_as_not_saved() {
        for bad in ["", "null", "{}", r#"{"x":1}"#, r#"{"x":"1","y":2}"#, r#"{"x":1.5,"y":2}"#, "[1,2]"] {
            assert_eq!(parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn round_trips_through_the_file() {
        let file = std::env::temp_dir().join(format!("xiro-display-layout-{}", std::process::id())).join("layout.json");
        let layout = LayoutFile::new(file.clone());
        assert_eq!(layout.get(), None);
        layout.set(Position { x: -800, y: 42 }).unwrap();
        assert_eq!(layout.get(), Some(Position { x: -800, y: 42 }));
        fs::remove_dir_all(file.parent().unwrap()).unwrap();
    }
}
