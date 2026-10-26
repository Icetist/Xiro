//! Choosing which monitor to stream and mapping it to a DXGI output index.

use std::fmt;

use crate::geometry::Rect;

/// One active desktop monitor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Monitor {
    /// GDI device name, e.g. `\\.\DISPLAY2`.
    pub name: String,
    /// Adapter description, e.g. "Virtual Display Driver".
    pub adapter: String,
    pub is_primary: bool,
    pub is_virtual: bool,
    pub rect: Rect,
}

/// Which monitor the user asked for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MonitorChoice {
    /// A virtual display if present (it exists only for the iPad), else any secondary, else the primary.
    Auto,
    /// 1-based position in [`crate::list_monitors`] order.
    Index(usize),
    /// GDI device name, matched case-insensitively.
    Name(String),
}

/// Adapter-name fragments of indirect/virtual display drivers (matched case-insensitively).
const VIRTUAL_ADAPTER_HINTS: [&str; 6] = ["virtual", "idd", "parsec", "spacedesk", "usbmmidd", "dummy"];

/// True when the adapter looks like a virtual display driver rather than a real GPU output.
pub fn is_virtual_adapter(name: &str) -> bool {
    let lower = name.to_lowercase();
    VIRTUAL_ADAPTER_HINTS.iter().any(|hint| lower.contains(hint))
}

pub fn pick_monitor(monitors: &[Monitor], choice: &MonitorChoice) -> Result<Monitor, String> {
    if monitors.is_empty() {
        return Err("No active displays found".to_owned());
    }
    match choice {
        MonitorChoice::Index(i) => i
            .checked_sub(1)
            .and_then(|at| monitors.get(at))
            .cloned()
            .ok_or_else(|| format!("Display {i} does not exist (found {})", monitors.len())),
        MonitorChoice::Name(name) if name != "auto" => monitors
            .iter()
            .find(|m| m.name.to_lowercase() == name.to_lowercase())
            .cloned()
            .ok_or_else(|| format!("Display \"{name}\" not found")),
        _ => Ok(monitors
            .iter()
            .find(|m| m.is_virtual)
            .or_else(|| monitors.iter().find(|m| !m.is_primary))
            .unwrap_or(&monitors[0])
            .clone()),
    }
}

/// A DXGI output as the capture side sees it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OutputSize {
    pub index: usize,
    pub width: i32,
    pub height: i32,
}

/// DXGI output indices are per-adapter and carry no desktop position, so match by resolution,
/// using the monitor's rank among same-sized monitors (in `monitors` order) to break ties.
pub fn match_output_index(target: &Monitor, monitors: &[Monitor], outputs: &[OutputSize]) -> Option<usize> {
    let (w, h) = (target.rect.width, target.rect.height);
    let candidates: Vec<_> = outputs.iter().filter(|o| o.width == w && o.height == h).collect();
    let first = candidates.first()?;
    let rank = monitors.iter().filter(|m| m.rect.width == w && m.rect.height == h).position(|m| m == target);
    Some(rank.and_then(|r| candidates.get(r)).unwrap_or(first).index)
}

/// `\\.\DISPLAY2  2360x1640 at (1920,0)  Virtual Display Driver  [virtual]`
impl fmt::Display for Monitor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let r = self.rect;
        write!(f, "{}  {}x{} at ({},{})  {}", self.name, r.width, r.height, r.left, r.top, self.adapter)?;
        let tags: Vec<&str> = [(self.is_primary, "primary"), (self.is_virtual, "virtual")]
            .into_iter()
            .filter_map(|(on, tag)| on.then_some(tag))
            .collect();
        if !tags.is_empty() {
            write!(f, "  [{}]", tags.join(", "))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mon(name: &str, width: i32, height: i32) -> Monitor {
        Monitor { name: name.to_owned(), adapter: "GPU".to_owned(), is_primary: false, is_virtual: false, rect: Rect { left: 0, top: 0, width, height } }
    }

    #[test]
    fn auto_picks_virtual_then_non_primary_then_primary() {
        let primary = Monitor { is_primary: true, ..mon(r"\\.\DISPLAY1", 1920, 1080) };
        let second = mon(r"\\.\DISPLAY2", 1920, 1080);
        let virt = Monitor { is_virtual: true, ..mon(r"\\.\DISPLAY4", 2360, 1640) };
        let all = [primary.clone(), second.clone(), virt.clone()];
        assert_eq!(pick_monitor(&all, &MonitorChoice::Auto), Ok(virt));
        assert_eq!(pick_monitor(&all[..2], &MonitorChoice::Auto), Ok(second));
        assert_eq!(pick_monitor(&all[..1], &MonitorChoice::Auto), Ok(primary.clone()));
        assert_eq!(pick_monitor(&all[..1], &MonitorChoice::Name("auto".into())), Ok(primary));
    }

    #[test]
    fn accepts_index_and_device_name_and_rejects_unknown() {
        let ms = [mon(r"\\.\DISPLAY1", 1, 1), mon(r"\\.\DISPLAY2", 1, 1)];
        assert_eq!(pick_monitor(&ms, &MonitorChoice::Index(2)), Ok(ms[1].clone()));
        assert_eq!(pick_monitor(&ms, &MonitorChoice::Name(r"\\.\display1".into())), Ok(ms[0].clone()));
        assert!(pick_monitor(&ms, &MonitorChoice::Index(3)).unwrap_err().contains("does not exist"));
        assert!(pick_monitor(&ms, &MonitorChoice::Index(0)).unwrap_err().contains("does not exist"));
        assert!(pick_monitor(&ms, &MonitorChoice::Name("nope".into())).unwrap_err().contains("not found"));
        assert!(pick_monitor(&[], &MonitorChoice::Auto).unwrap_err().contains("No active displays"));
    }

    #[test]
    fn matches_dxgi_outputs_by_size_and_breaks_ties_by_order() {
        let (a, b, c) = (mon("A", 1920, 1080), mon("B", 2360, 1640), mon("C", 1920, 1080));
        let out = |index, width, height| OutputSize { index, width, height };
        let outputs = [out(0, 1920, 1080), out(1, 2360, 1640), out(2, 1920, 1080)];
        let all = [a.clone(), b.clone(), c.clone()];
        assert_eq!(match_output_index(&b, &all, &outputs), Some(1));
        assert_eq!(match_output_index(&c, &all, &outputs), Some(2));
        assert_eq!(match_output_index(&mon("D", 800, 600), &[a], &outputs), None);
    }

    #[test]
    fn match_falls_back_to_first_candidate_when_rank_is_unknown() {
        let outputs = [OutputSize { index: 5, width: 10, height: 10 }];
        assert_eq!(match_output_index(&mon("X", 10, 10), &[], &outputs), Some(5));
    }

    #[test]
    fn recognises_common_virtual_display_drivers() {
        assert!(is_virtual_adapter("Virtual Display Driver"));
        assert!(is_virtual_adapter("Parsec Virtual Display Adapter"));
        assert!(is_virtual_adapter("SpaceDesk Graphics"));
        assert!(!is_virtual_adapter("Intel(R) HD Graphics Family"));
        assert!(!is_virtual_adapter(""));
    }

    #[test]
    fn describes_monitors_with_tags() {
        let mut m = Monitor { adapter: "VDD".into(), rect: Rect { left: 1920, top: 0, width: 2360, height: 1640 }, ..mon(r"\\.\DISPLAY2", 0, 0) };
        assert_eq!(m.to_string(), r"\\.\DISPLAY2  2360x1640 at (1920,0)  VDD");
        m.is_primary = true;
        m.is_virtual = true;
        assert_eq!(m.to_string(), r"\\.\DISPLAY2  2360x1640 at (1920,0)  VDD  [primary, virtual]");
    }
}
