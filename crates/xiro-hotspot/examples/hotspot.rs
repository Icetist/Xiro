//! Manual check: brings Xiro Wi-Fi up for a number of seconds (default 20).
//! `cargo run -p xiro-hotspot --example hotspot -- 60`

fn main() {
    let seconds: u64 = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(20);
    let credentials = xiro_hotspot::Credentials::generate("Test").expect("random bytes");
    match xiro_hotspot::Hotspot::start(&credentials, |state| println!("state: {state:?}")) {
        Ok(hotspot) => {
            println!("Up: {} / {} for {seconds} s", credentials.ssid, credentials.passphrase);
            std::thread::sleep(std::time::Duration::from_secs(seconds));
            drop(hotspot);
            println!("Stopped");
        }
        Err(why) => println!("Failed: {why}"),
    }
}
