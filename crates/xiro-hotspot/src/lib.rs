//! Xiro Wi-Fi: a Wi-Fi Direct "legacy" access point the iPad joins directly. Needs no admin
//! rights; the PC stays on its other networks and the iPad gets no internet through it.

mod credentials;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use windows::Devices::WiFiDirect::{
    WiFiDirectAdvertisementListenStateDiscoverability, WiFiDirectAdvertisementPublisher,
    WiFiDirectAdvertisementPublisherStatus, WiFiDirectAdvertisementPublisherStatusChangedEventArgs, WiFiDirectError,
};
use windows::Foundation::TypedEventHandler;
use windows::Security::Credentials::PasswordCredential;
use windows::core::HSTRING;

pub use credentials::{Credentials, network_name};

/// How long Windows may take to bring the network up.
const START_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    On,
    Off,
    Failed(String),
}

/// Running while this value lives; dropping it takes the network down.
pub struct Hotspot {
    publisher: WiFiDirectAdvertisementPublisher,
    token: i64,
    /// Follows every status change, so a network Windows took down never looks up.
    up: Arc<AtomicBool>,
}

type Report = Arc<dyn Fn(State) + Send + Sync>;

/// Routes status changes: the first answers `start`, later ones go to the caller (unless
/// `start` already gave up, so a late "started" can't announce a network nobody holds).
struct Router {
    first: Mutex<Option<mpsc::Sender<State>>>,
    caller_listens: AtomicBool,
    up: Arc<AtomicBool>,
    report: Report,
}

impl Router {
    fn route(&self, state: State) {
        self.up.store(state == State::On, Ordering::SeqCst);
        let first = self.first.lock().ok().and_then(|mut slot| slot.take());
        match first {
            Some(tx) => {
                let _ = tx.send(state);
            }
            None if self.caller_listens.load(Ordering::SeqCst) => (self.report)(state),
            None => {}
        }
    }
}

fn reason(error: WiFiDirectError) -> String {
    match error {
        WiFiDirectError::RadioNotAvailable => "Wi-Fi is turned off on this PC.".into(),
        WiFiDirectError::ResourceInUse => {
            "The Wi-Fi adapter is busy (Mobile hotspot or a wireless display may be using it).".into()
        }
        _ => "This PC's Wi-Fi adapter can't host a network.".into(),
    }
}

fn state_of(args: &WiFiDirectAdvertisementPublisherStatusChangedEventArgs) -> Option<State> {
    match args.Status().ok()? {
        WiFiDirectAdvertisementPublisherStatus::Started => Some(State::On),
        WiFiDirectAdvertisementPublisherStatus::Stopped => Some(State::Off),
        WiFiDirectAdvertisementPublisherStatus::Aborted => {
            Some(State::Failed(args.Error().map_or_else(|_| reason(WiFiDirectError(-1)), reason)))
        }
        _ => None,
    }
}

fn configure(publisher: &WiFiDirectAdvertisementPublisher, credentials: &Credentials) -> windows::core::Result<()> {
    let advertisement = publisher.Advertisement()?;
    advertisement.SetIsAutonomousGroupOwnerEnabled(true)?;
    advertisement.SetListenStateDiscoverability(WiFiDirectAdvertisementListenStateDiscoverability::Normal)?;
    let legacy = advertisement.LegacySettings()?;
    legacy.SetIsEnabled(true)?;
    legacy.SetSsid(&HSTRING::from(credentials.ssid.as_str()))?;
    let passphrase = PasswordCredential::new()?;
    passphrase.SetPassword(&HSTRING::from(credentials.passphrase.as_str()))?;
    legacy.SetPassphrase(&passphrase)
}

impl Hotspot {
    /// Brings the network up and waits until it is (or can't be). Later changes, such as
    /// Wi-Fi being switched off, are reported through `on_state`.
    pub fn start(credentials: &Credentials, on_state: impl Fn(State) + Send + Sync + 'static) -> Result<Self, String> {
        if !credentials.is_valid() {
            return Err("The Xiro Wi-Fi name or password is not valid.".into());
        }
        let fail = |e: windows::core::Error| format!("Could not start Xiro Wi-Fi: {}", e.message());
        let publisher = WiFiDirectAdvertisementPublisher::new().map_err(fail)?;
        configure(&publisher, credentials).map_err(fail)?;
        let (first_tx, first) = mpsc::channel::<State>();
        let up = Arc::new(AtomicBool::new(false));
        let router = Arc::new(Router {
            first: Mutex::new(Some(first_tx)),
            caller_listens: AtomicBool::new(true),
            up: up.clone(),
            report: Arc::new(on_state),
        });
        let routed = router.clone();
        let handler = TypedEventHandler::new(move |_, args: windows::core::Ref<WiFiDirectAdvertisementPublisherStatusChangedEventArgs>| {
            if let Some(state) = args.ok().ok().and_then(state_of) {
                routed.route(state);
            }
            Ok(())
        });
        let token = publisher.StatusChanged(&handler).map_err(fail)?;
        let hotspot = Self { publisher, token, up };
        let outcome = match hotspot.publisher.Start().map_err(fail) {
            Ok(()) => first.recv_timeout(START_TIMEOUT).unwrap_or(State::Off),
            Err(why) => State::Failed(why),
        };
        match outcome {
            State::On => Ok(hotspot),
            failed => {
                router.caller_listens.store(false, Ordering::SeqCst);
                Err(match failed {
                    State::Failed(why) => why,
                    _ => "Xiro Wi-Fi did not start. Is Wi-Fi turned on?".into(),
                })
            }
        }
    }

    /// False once Windows has taken the network down (Wi-Fi switched off, sleep...).
    pub fn is_up(&self) -> bool {
        self.up.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn router(listening: bool) -> (Router, mpsc::Receiver<State>, Arc<Mutex<Vec<State>>>) {
        let (tx, rx) = mpsc::channel();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = seen.clone();
        let router = Router {
            first: Mutex::new(Some(tx)),
            caller_listens: AtomicBool::new(listening),
            up: Arc::new(AtomicBool::new(false)),
            report: Arc::new(move |s| sink.lock().unwrap().push(s)),
        };
        (router, rx, seen)
    }

    #[test]
    fn first_answer_goes_to_start_and_later_ones_to_the_caller() {
        let (router, rx, seen) = router(true);
        router.route(State::On);
        assert_eq!(rx.try_recv(), Ok(State::On));
        assert!(router.up.load(Ordering::SeqCst));
        router.route(State::Failed("Wi-Fi is turned off on this PC.".into()));
        assert!(!router.up.load(Ordering::SeqCst), "a network Windows took down never looks up");
        assert_eq!(seen.lock().unwrap().len(), 1);
    }

    #[test]
    fn nothing_is_announced_after_start_gave_up() {
        let (router, _rx, seen) = router(false);
        router.route(State::Off);
        router.route(State::On); // late "started" after a timeout
        assert!(seen.lock().unwrap().is_empty());
    }
}

impl Drop for Hotspot {
    fn drop(&mut self) {
        let _ = self.publisher.RemoveStatusChanged(self.token);
        let _ = self.publisher.Stop();
    }
}
