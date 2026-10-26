//! Manual check of the pairing approval dialog: `cargo run -p xiro-shell --example approval_demo`.

fn main() {
    let started = std::time::Instant::now();
    let allowed = xiro_shell::ask_to_pair("Xiro: allow this device?", "Allow this device to pair?", "A device at 192.168.137.23 wants to pair.");
    println!("allowed: {allowed} after {:?}", started.elapsed());
}
