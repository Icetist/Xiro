//! The seam between the injector state machine and the OS, so the state machine is testable
//! without ever touching the live desktop.

use windows::Win32::UI::Controls::POINTER_TYPE_INFO;
use windows::Win32::UI::Input::KeyboardAndMouse::INPUT;

use crate::geometry::Rect;

/// Which synthetic pointer device a frame is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DeviceKind {
    Touch,
    Pen,
}

/// Where injected input goes. The real implementation is [`crate::win32::Win32Sink`].
pub(crate) trait InputSink {
    /// Bounds of the whole virtual desktop, for SendInput absolute coordinates.
    fn virtual_screen(&mut self) -> Rect;
    /// Mouse and keyboard records, in order.
    fn send_input(&mut self, inputs: &[INPUT]);
    /// Creates the synthetic device for `kind`; `false` when Windows refuses (pre-1809, policy...).
    fn create_pointer_device(&mut self, kind: DeviceKind, max_contacts: u32) -> bool;
    /// One frame for a device previously created; `false` when Windows rejects it.
    fn inject_pointer(&mut self, kind: DeviceKind, records: &[POINTER_TYPE_INFO]) -> bool;
}

#[cfg(test)]
pub(crate) mod fake {
    use std::sync::{Arc, Mutex, MutexGuard};

    use super::*;

    /// Everything the injector asked the OS to do, in order.
    #[derive(Clone)]
    pub(crate) enum Event {
        Send(Vec<INPUT>),
        Create(DeviceKind, u32),
        Inject(DeviceKind, Vec<POINTER_TYPE_INFO>),
    }

    #[derive(Default)]
    pub(crate) struct State {
        pub events: Vec<Event>,
        pub vs: Rect,
        pub refuse_devices: bool,
        pub reject_injection: bool,
        pub vs_queries: usize,
    }

    // SAFETY: the only raw pointers (`sourceDevice`, `hwndTarget`) stay null and are never dereferenced.
    unsafe impl Send for State {}

    /// Records calls in shared state for tests to inspect.
    #[derive(Clone, Default)]
    pub(crate) struct FakeSink(pub Arc<Mutex<State>>);

    impl FakeSink {
        pub fn state(&self) -> MutexGuard<'_, State> {
            self.0.lock().expect("fake sink poisoned")
        }

        pub fn sent(&self) -> Vec<INPUT> {
            self.state().events.iter().filter_map(|e| if let Event::Send(v) = e { Some(v.clone()) } else { None }).flatten().collect()
        }

        pub fn frames(&self, kind: DeviceKind) -> Vec<Vec<POINTER_TYPE_INFO>> {
            self.state()
                .events
                .iter()
                .filter_map(|e| match e {
                    Event::Inject(k, v) if *k == kind => Some(v.clone()),
                    _ => None,
                })
                .collect()
        }

        pub fn creates(&self) -> Vec<(DeviceKind, u32)> {
            self.state().events.iter().filter_map(|e| if let Event::Create(k, n) = e { Some((*k, *n)) } else { None }).collect()
        }

        pub fn clear(&self) {
            self.state().events.clear();
        }
    }

    impl InputSink for FakeSink {
        fn virtual_screen(&mut self) -> Rect {
            let mut s = self.state();
            s.vs_queries += 1;
            s.vs
        }

        fn send_input(&mut self, inputs: &[INPUT]) {
            if !inputs.is_empty() {
                self.state().events.push(Event::Send(inputs.to_vec()));
            }
        }

        fn create_pointer_device(&mut self, kind: DeviceKind, max_contacts: u32) -> bool {
            let mut s = self.state();
            s.events.push(Event::Create(kind, max_contacts));
            !s.refuse_devices
        }

        fn inject_pointer(&mut self, kind: DeviceKind, records: &[POINTER_TYPE_INFO]) -> bool {
            let mut s = self.state();
            s.events.push(Event::Inject(kind, records.to_vec()));
            !s.reject_injection
        }
    }
}
