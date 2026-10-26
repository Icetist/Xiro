//! Dev helper: `hold attach W H` turns the virtual monitor on, `hold detach` turns it off.
fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let layout = std::env::temp_dir().join("xiro-hold-layout.json");
    let display = xiro_display::VirtualDisplay::new(layout);
    match a.first().map(String::as_str) {
        Some("attach") => {
            let (w, h) = (a[1].parse().unwrap(), a[2].parse().unwrap());
            println!("{:?}", display.attach(xiro_display::Mode { width: w, height: h, hz: 60 }, 200));
        }
        _ => println!("{:?}", display.detach()),
    }
}
