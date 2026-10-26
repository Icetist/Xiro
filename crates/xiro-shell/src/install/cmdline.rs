//! Hand-built command lines: elevated self-relaunch arguments and the delayed `cmd /c` clean-up.

use std::path::Path;

/// Ping count (about seconds) so the uninstalling exe has exited before its folder is removed.
const CLEANUP_DELAY_PINGS: u32 = 6;

/// Quotes for CommandLineToArgvW: backslashes are literal except before a quote, where they double.
pub(crate) fn quote_arg(arg: &str) -> String {
    if !arg.is_empty() && !arg.contains([' ', '\t', '\n', '\u{b}', '"']) {
        return arg.to_owned();
    }
    let mut quoted = String::with_capacity(arg.len() + 2);
    quoted.push('"');
    let mut backslashes = 0;
    for ch in arg.chars() {
        match ch {
            '\\' => backslashes += 1,
            '"' => {
                quoted.extend(std::iter::repeat_n('\\', backslashes * 2 + 1));
                quoted.push('"');
                backslashes = 0;
            }
            _ => {
                quoted.extend(std::iter::repeat_n('\\', backslashes));
                quoted.push(ch);
                backslashes = 0;
            }
        }
    }
    quoted.extend(std::iter::repeat_n('\\', backslashes * 2));
    quoted.push('"');
    quoted
}

pub(crate) fn join_args(args: &[&str]) -> String {
    args.iter().map(|a| quote_arg(a)).collect::<Vec<_>>().join(" ")
}

/// cmd.exe expands `%VAR%` and mishandles `"` even in quotes, so paths with either are refused.
fn cmd_safe(path: &Path) -> Result<String, String> {
    let text = path.display().to_string();
    if text.contains(['"', '%', '\r', '\n']) || text.trim().is_empty() {
        return Err(format!("refusing to schedule removal of an unusual path: {text}"));
    }
    Ok(text)
}

/// `cmd.exe` arguments: wait, then remove each folder; `&` so one missing folder doesn't stop the rest.
pub(crate) fn cleanup_command(folders: &[&Path]) -> Result<String, String> {
    let mut command = format!("/d /c ping -n {CLEANUP_DELAY_PINGS} 127.0.0.1 >nul");
    for folder in folders {
        command.push_str(&format!(" & rmdir /s /q \"{}\"", cmd_safe(folder)?));
    }
    Ok(command)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_arguments_are_left_alone() {
        assert_eq!(quote_arg("--add-firewall-rule"), "--add-firewall-rule");
        assert_eq!(join_args(&["--uninstall", "--keep-data"]), "--uninstall --keep-data");
    }

    #[test]
    fn spaces_quotes_and_trailing_backslashes_survive_argv_parsing() {
        assert_eq!(quote_arg(""), "\"\"");
        assert_eq!(quote_arg(r"C:\Program Files\Xiro"), r#""C:\Program Files\Xiro""#);
        assert_eq!(quote_arg(r"C:\My Dir\"), r#""C:\My Dir\\""#);
        assert_eq!(quote_arg(r#"say "hi""#), r#""say \"hi\"""#);
        assert_eq!(quote_arg(r#"a\"b c"#), r#""a\\\"b c""#);
    }

    #[test]
    fn cleanup_waits_then_removes_each_quoted_folder() {
        let app = Path::new(r"C:\Users\Me Too\AppData\Local\Programs\Xiro");
        let data = Path::new(r"C:\Users\Me Too\AppData\Local\Xiro");
        assert_eq!(
            cleanup_command(&[app, data]).unwrap(),
            r#"/d /c ping -n 6 127.0.0.1 >nul & rmdir /s /q "C:\Users\Me Too\AppData\Local\Programs\Xiro" & rmdir /s /q "C:\Users\Me Too\AppData\Local\Xiro""#
        );
        assert!(cleanup_command(&[Path::new(r"C:\Users\%USERNAME%\x")]).is_err());
        assert!(cleanup_command(&[Path::new("")]).is_err());
    }
}
