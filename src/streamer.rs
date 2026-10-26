//! Fans frames out to viewers, latency first: a backed-up viewer drops frames until the next
//! key frame. The current GOP is cached so a joining or reset viewer gets a picture at once.

use std::sync::Arc;

use base64::Engine;
use bytes::Bytes;
use serde_json::json;
use xiro_video::{VideoConfig, VideoFrame};

use crate::protocol::{VIDEO_FLAG_REPLAY, video_message};

/// Bytes queued in user space before inter frames are dropped.
const MIN_BUFFER_BYTES: usize = 192 * 1024;
const BUFFER_SECONDS: f64 = 0.05;
/// Key frames are allowed a deeper queue since they are what recovery waits for.
const KEY_FRAME_BUFFER_FACTOR: usize = 4;
/// Larger GOPs are not cached (replaying them would cost more than waiting).
const MAX_GOP_BYTES: usize = 8 * 1024 * 1024;

pub type ViewerId = u64;

/// A viewer's outgoing side (the WebSocket writer task).
pub trait Link: Send + Sync {
    fn is_open(&self) -> bool;
    /// Bytes queued but not yet written to the socket (like WebSocket.bufferedAmount).
    fn buffered(&self) -> usize;
    fn send_binary(&self, data: Bytes);
    fn send_text(&self, text: String);
    /// Disconnects the viewer for good: its pairing was revoked at the PC.
    fn unlink(&self);
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ViewerStats {
    pub sent: u64,
    pub dropped: u64,
    pub replays: u64,
}

struct Viewer {
    id: ViewerId,
    link: Arc<dyn Link>,
    needs_key: bool,
    stats: ViewerStats,
}

pub struct Streamer {
    viewers: Vec<Viewer>,
    config: Option<VideoConfig>,
    gop: Vec<Bytes>,
    gop_bytes: usize,
    max_buffered: usize,
}

pub fn buffer_limit(bitrate: u32) -> usize {
    MIN_BUFFER_BYTES.max(((bitrate as f64 / 8.0) * BUFFER_SECONDS).round() as usize)
}

impl Streamer {
    pub fn new(bitrate: u32) -> Self {
        Self { viewers: Vec::new(), config: None, gop: Vec::new(), gop_bytes: 0, max_buffered: buffer_limit(bitrate) }
    }

    pub fn set_bitrate(&mut self, bitrate: u32) {
        self.max_buffered = buffer_limit(bitrate);
    }

    pub fn ids(&self) -> Vec<ViewerId> {
        self.viewers.iter().map(|v| v.id).collect()
    }

    pub fn add(&mut self, id: ViewerId, link: Arc<dyn Link>) {
        self.viewers.push(Viewer { id, link, needs_key: true, stats: ViewerStats::default() });
        if self.config.is_some() {
            self.send_config(id);
            self.replay(id);
        }
    }

    /// Adds the viewer unless `max` are already connected (one lock: no check-then-act race).
    pub fn try_add(&mut self, id: ViewerId, link: Arc<dyn Link>, max: usize) -> bool {
        if self.viewers.len() >= max {
            return false;
        }
        self.add(id, link);
        true
    }

    pub fn remove(&mut self, id: ViewerId) -> Option<ViewerStats> {
        let index = self.viewers.iter().position(|v| v.id == id)?;
        Some(self.viewers.remove(index).stats)
    }

    #[cfg(test)]
    pub fn has_config(&self) -> bool {
        self.config.is_some()
    }

    pub fn set_config(&mut self, config: VideoConfig) {
        self.config = Some(config);
        self.reset_gop();
        for id in self.ids() {
            self.send_config(id);
        }
    }

    /// Forgets the stream (its display is gone), so a later viewer never gets stale frames.
    pub fn reset(&mut self) {
        self.config = None;
        self.reset_gop();
    }

    fn reset_gop(&mut self) {
        self.gop.clear();
        self.gop_bytes = 0;
    }

    fn viewer(&mut self, id: ViewerId) -> Option<&mut Viewer> {
        self.viewers.iter_mut().find(|v| v.id == id)
    }

    fn send_config(&mut self, id: ViewerId) {
        let Some(config) = &self.config else { return };
        let text = json!({
            "t": "config",
            "codec": config.codec,
            "description": base64::engine::general_purpose::STANDARD.encode(&config.avcc),
        })
        .to_string();
        if let Some(viewer) = self.viewer(id) {
            viewer.needs_key = true;
            if viewer.link.is_open() {
                viewer.link.send_text(text);
            }
        }
    }

    /// Re-sends the cached GOP so the viewer can decode the current picture right away.
    /// False when nothing could be sent (no GOP yet, or the viewer is congested).
    pub fn replay(&mut self, id: ViewerId) -> bool {
        let (gop, limit) = (self.gop.clone(), self.max_buffered);
        let Some(viewer) = self.viewer(id) else { return false };
        if gop.is_empty() || !viewer.link.is_open() || viewer.link.buffered() > limit {
            return false;
        }
        for message in gop {
            let mut copy = message.to_vec();
            copy[1] |= VIDEO_FLAG_REPLAY;
            viewer.link.send_binary(Bytes::from(copy));
        }
        viewer.needs_key = false;
        viewer.stats.replays += 1;
        true
    }

    fn cache(&mut self, message: &Bytes, is_key: bool) {
        if is_key {
            self.reset_gop();
        } else if self.gop.is_empty() {
            return;
        }
        if self.gop_bytes + message.len() > MAX_GOP_BYTES {
            self.reset_gop();
            return;
        }
        self.gop_bytes += message.len();
        self.gop.push(message.clone());
    }

    /// Sends a frame to every viewer that can take it now.
    pub fn push(&mut self, frame: &VideoFrame, sent_at: f64) {
        let message = Bytes::from(video_message(frame.is_key, sent_at, &frame.data));
        self.cache(&message, frame.is_key);
        let limit = if frame.is_key { self.max_buffered * KEY_FRAME_BUFFER_FACTOR } else { self.max_buffered };
        for viewer in &mut self.viewers {
            if !viewer.link.is_open() || (viewer.needs_key && !frame.is_key) {
                continue;
            }
            if viewer.link.buffered() > limit {
                viewer.needs_key = true;
                viewer.stats.dropped += 1;
                continue;
            }
            viewer.link.send_binary(message.clone());
            viewer.needs_key = false;
            viewer.stats.sent += 1;
        }
    }

    /// Every viewer is signed out (their pairing token no longer works); returns how many.
    pub fn unlink_all(&self) -> usize {
        self.viewers.iter().filter(|v| v.link.is_open()).map(|v| v.link.unlink()).count()
    }

    pub fn broadcast_text(&self, text: &str) {
        for viewer in self.viewers.iter().filter(|v| v.link.is_open()) {
            viewer.link.send_text(text.to_owned());
        }
    }

    pub fn send_text(&self, id: ViewerId, text: &str) {
        if let Some(viewer) = self.viewers.iter().find(|v| v.id == id && v.link.is_open()) {
            viewer.link.send_text(text.to_owned());
        }
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    #[derive(Default)]
    pub struct FakeLink {
        pub closed: AtomicBool,
        pub backlog: AtomicUsize,
        pub binary: Mutex<Vec<Bytes>>,
        pub text: Mutex<Vec<String>>,
    }

    impl Link for FakeLink {
        fn is_open(&self) -> bool {
            !self.closed.load(Ordering::Relaxed)
        }
        fn buffered(&self) -> usize {
            self.backlog.load(Ordering::Relaxed)
        }
        fn send_binary(&self, data: Bytes) {
            self.binary.lock().unwrap().push(data);
        }
        fn send_text(&self, text: String) {
            self.text.lock().unwrap().push(text);
        }
        fn unlink(&self) {
            self.closed.store(true, Ordering::Relaxed);
        }
    }

    fn frame(is_key: bool, byte: u8) -> VideoFrame {
        VideoFrame { is_key, data: vec![byte; 4], captured_at: 1.0 }
    }

    fn config() -> VideoConfig {
        VideoConfig { codec: "avc1.640028".into(), avcc: vec![1, 0x64, 0, 0x28] }
    }

    fn setup() -> (Streamer, Arc<FakeLink>) {
        let mut s = Streamer::new(20_000_000);
        let link = Arc::new(FakeLink::default());
        s.set_config(config());
        s.add(1, link.clone());
        (s, link)
    }

    #[test]
    fn unlinking_disconnects_every_open_viewer() {
        let (mut s, first) = setup();
        let second = Arc::new(FakeLink::default());
        s.add(2, second.clone());
        assert_eq!(s.unlink_all(), 2);
        assert!(!first.is_open() && !second.is_open());
        assert_eq!(s.unlink_all(), 0, "already gone");
    }

    #[test]
    fn new_viewer_gets_config_then_waits_for_a_key_frame() {
        let (mut s, link) = setup();
        assert!(link.text.lock().unwrap()[0].contains("\"codec\":\"avc1.640028\""));
        s.push(&frame(false, 1), 5.0);
        assert!(link.binary.lock().unwrap().is_empty(), "delta before key is useless");
        s.push(&frame(true, 2), 6.0);
        s.push(&frame(false, 3), 7.0);
        let sent = link.binary.lock().unwrap();
        assert_eq!(sent.len(), 2);
        assert_eq!(sent[0][1], 1, "key flag");
        assert_eq!(f64::from_le_bytes(sent[0][2..10].try_into().unwrap()), 6.0);
    }

    #[test]
    fn congested_viewer_drops_until_the_next_key_frame() {
        let (mut s, link) = setup();
        s.push(&frame(true, 1), 1.0);
        link.backlog.store(buffer_limit(20_000_000) + 1, Ordering::Relaxed);
        s.push(&frame(false, 2), 2.0);
        link.backlog.store(0, Ordering::Relaxed);
        s.push(&frame(false, 3), 3.0);
        s.push(&frame(true, 4), 4.0);
        let sent: Vec<u8> = link.binary.lock().unwrap().iter().map(|m| m[10]).collect();
        assert_eq!(sent, vec![1, 4]);
        assert_eq!(s.remove(1).unwrap(), ViewerStats { sent: 2, dropped: 1, replays: 0 });
    }

    #[test]
    fn late_joiner_gets_the_current_gop_flagged_as_replay() {
        let (mut s, _) = setup();
        s.push(&frame(true, 1), 1.0);
        s.push(&frame(false, 2), 2.0);
        let late = Arc::new(FakeLink::default());
        s.add(2, late.clone());
        let got = late.binary.lock().unwrap();
        assert_eq!(got.len(), 2);
        assert!(got.iter().all(|m| m[1] & VIDEO_FLAG_REPLAY != 0));
    }

    #[test]
    fn reset_forgets_config_and_gop() {
        let (mut s, _) = setup();
        s.push(&frame(true, 1), 1.0);
        s.reset();
        assert!(!s.has_config());
        assert!(!s.replay(1));
        let late = Arc::new(FakeLink::default());
        s.add(3, late.clone());
        assert!(late.text.lock().unwrap().is_empty());
    }

    #[test]
    fn closed_links_are_skipped_and_text_reaches_open_ones() {
        let (mut s, link) = setup();
        let other = Arc::new(FakeLink::default());
        s.add(2, other.clone());
        link.closed.store(true, Ordering::Relaxed);
        s.push(&frame(true, 1), 1.0);
        s.broadcast_text("{\"t\":\"status\"}");
        s.send_text(2, "hi");
        assert!(link.binary.lock().unwrap().is_empty());
        assert_eq!(other.binary.lock().unwrap().len(), 1);
        assert_eq!(other.text.lock().unwrap().last().unwrap(), "hi");
        assert_eq!(s.ids(), vec![1, 2]);
        assert!(s.remove(9).is_none());
    }

    #[test]
    fn oversized_gops_are_not_cached() {
        let (mut s, _) = setup();
        let big = VideoFrame { is_key: true, data: vec![0; MAX_GOP_BYTES + 1], captured_at: 0.0 };
        s.push(&big, 1.0);
        assert!(!s.replay(1));
        s.set_bitrate(400_000_000);
        assert_eq!(s.max_buffered, buffer_limit(400_000_000));
    }
}
