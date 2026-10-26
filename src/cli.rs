//! Command-line options (the tray app passes none; they are for power users and scripts).

use crate::config::DisplayChoice;

pub const HELP: &str = r"
Usage: xiro [options]

  --display <n|name>   Stream an existing display (1-based index or \.\DISPLAYn) instead of
                       the iPad-shaped virtual display
  --fps <n>            Frame-rate cap (default: the iPad's refresh rate)
  --bitrate <mbps>     Video bitrate in Mbit/s (default: based on resolution)
  --encoder <name>     auto, hardware, software, or part of an encoder's name
  --reprobe            Re-test encoders instead of using the remembered choice
  --list-displays      Show displays and exit
  --new-token          Generate a new pairing code (disconnects paired iPads)
  --pair               Show the pairing QR code and exit
  --autostart <on|off|status>  Start Xiro silently when you sign in to Windows
  --console            Run in this terminal instead of the tray
  --uninstall          Remove Xiro from this PC: display driver, firewall rule, settings
  --background         Start quietly in the tray (used by autostart)
  -h, --help           Show this help
";

#[derive(Debug, Default, Clone, PartialEq)]
pub struct Cli {
    pub display: Option<DisplayChoice>,
    pub fps: Option<u32>,
    pub bitrate_mbps: Option<f64>,
    pub encoder: Option<String>,
    pub reprobe: bool,
    pub list_displays: bool,
    pub new_token: bool,
    pub pair: bool,
    pub autostart: Option<String>,
    pub background: bool,
    /// Run in the terminal (log + QR code there) instead of the tray.
    pub console: bool,
    pub uninstall: bool,
    /// Internal: started by an exiting Xiro (after a driver fix); waits for it, then opens the window.
    pub restart: bool,
    /// Not an option: set after the driver was just set up, so startup waits for it.
    pub wait_for_driver: bool,
    /// Internal, elevated: install the driver, its settings and the firewall rule.
    pub setup_system: bool,
    /// Internal, elevated: remove the firewall rule.
    pub remove_system: bool,
    /// Internal, with --setup-system: the ports the firewall rule opens.
    pub ports: Vec<u16>,
    /// Internal, with --setup-system: the device screens that connected, newest first.
    pub screens: Vec<xiro_display::Size>,
    pub help: bool,
}

fn value(args: &mut impl Iterator<Item = String>, flag: &str) -> Result<String, String> {
    args.next().ok_or_else(|| format!("{flag} needs a value"))
}

fn number<T: std::str::FromStr>(text: &str, flag: &str) -> Result<T, String> {
    text.parse().map_err(|_| format!("{flag} must be a number, got \"{text}\""))
}

pub fn parse(args: impl IntoIterator<Item = String>) -> Result<Cli, String> {
    let mut cli = Cli::default();
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--display" => {
                let v = value(&mut args, &arg)?;
                cli.display = Some(match v.parse::<usize>() {
                    Ok(0) => return Err("--display numbers start at 1".into()),
                    Ok(i) => DisplayChoice::Index(i),
                    Err(_) if v == "auto" => DisplayChoice::Auto,
                    Err(_) => DisplayChoice::Name(v),
                });
            }
            "--fps" => cli.fps = Some(number(&value(&mut args, &arg)?, &arg)?),
            "--bitrate" => cli.bitrate_mbps = Some(number(&value(&mut args, &arg)?, &arg)?),
            "--encoder" => cli.encoder = Some(value(&mut args, &arg)?),
            "--autostart" => cli.autostart = Some(value(&mut args, &arg)?),
            "--reprobe" => cli.reprobe = true,
            "--list-displays" => cli.list_displays = true,
            "--new-token" => cli.new_token = true,
            "--pair" => cli.pair = true,
            "--background" => cli.background = true,
            "--console" => cli.console = true,
            // Accepted for old shortcuts; Xiro is always portable.
            "--portable" => {}
            "--restart" => cli.restart = true,
            "--uninstall" => cli.uninstall = true,
            "--setup-system" => cli.setup_system = true,
            "--remove-system" => cli.remove_system = true,
            "--ports" => {
                let list = value(&mut args, &arg)?;
                cli.ports = list.split(',').map(|p| number(p, &arg)).collect::<Result<_, _>>()?;
            }
            "--screens" => cli.screens = crate::screens::parse(&value(&mut args, &arg)?),
            "-h" | "--help" => cli.help = true,
            other => return Err(format!("Unknown option {other}. Run with --help for the list.")),
        }
    }
    Ok(cli)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parses_every_option() {
        let cli = parse(args(&["--display", "2", "--fps", "120", "--bitrate", "30.5", "--encoder", "software", "--reprobe", "--pair", "--background"])).unwrap();
        assert_eq!(cli.display, Some(DisplayChoice::Index(2)));
        assert_eq!((cli.fps, cli.bitrate_mbps), (Some(120), Some(30.5)));
        assert_eq!(cli.encoder.as_deref(), Some("software"));
        assert!(cli.reprobe && cli.pair && cli.background && !cli.help);
        assert_eq!(parse(args(&["--display", r"\.\DISPLAY3"])).unwrap().display, Some(DisplayChoice::Name(r"\.\DISPLAY3".into())));
        assert_eq!(parse(args(&["--autostart", "on"])).unwrap().autostart.as_deref(), Some("on"));
    }

    #[test]
    fn rejects_bad_input_with_a_reason() {
        assert!(parse(args(&["--fps", "fast"])).unwrap_err().contains("--fps"));
        assert!(parse(args(&["--fps"])).unwrap_err().contains("needs a value"));
        assert!(parse(args(&["--display", "0"])).is_err());
        assert!(parse(args(&["--bogus"])).unwrap_err().contains("--bogus"));
        assert!(parse(args(&["-h"])).unwrap().help);
        let c = parse(args(&["--console", "--portable", "--uninstall", "--setup-system", "--remove-system", "--restart"])).unwrap();
        assert!(c.console && c.uninstall && c.setup_system && c.remove_system && c.restart);
        assert!(!c.wait_for_driver, "never set from the command line");
        assert_eq!(parse(args(&["--ports", "8080,8443"])).unwrap().ports, vec![8080, 8443]);
        assert!(parse(args(&["--ports", "8080,x"])).is_err());
        let screens = parse(args(&["--screens", "1179x2556,junk"])).unwrap().screens;
        assert_eq!(screens, vec![xiro_display::Size { width: 2556, height: 1179 }]);
        assert!(parse(args(&["--screens", ""])).unwrap().screens.is_empty());
    }
}
