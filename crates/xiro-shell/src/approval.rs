//! Pairing approval dialog: Allow arms after a delay; Enter, Esc and timeout mean no.

use std::time::Duration;

use crate::{Badge, Choice, Prompt};

const DENY: usize = 0;
const ALLOW: usize = 1;
/// "Allow" stays disabled this long after the dialog appears.
const ARM_DELAY: Duration = Duration::from_millis(1500);
/// No answer within this time means no.
pub const ANSWER_TIMEOUT: Duration = Duration::from_secs(60);

fn pairing_prompt(title: &str, detail: &str, text: &str) -> Prompt {
    Prompt {
        caption: "Xiro",
        badge: Badge::Device,
        title: title.to_owned(),
        body: text.to_owned(),
        detail: Some(detail.to_owned()),
        buttons: vec![("Don\u{2019}t Allow".to_owned(), Choice::Plain), ("Allow".to_owned(), Choice::Main)],
        default: DENY,
        cancel: DENY,
        arm: Some((ALLOW, ARM_DELAY)),
        timeout: Some(ANSWER_TIMEOUT),
        topmost: true,
    }
}

/// Asks whether a device may pair; false unless "Allow" was chosen in time.
pub fn ask_to_pair(title: &str, detail: &str, text: &str) -> bool {
    // Not a message box: it could not delay Allow or time out.
    crate::ui::prompt::ask(pairing_prompt(title, detail, text)) == Some(ALLOW)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saying_nothing_or_pressing_a_key_never_allows() {
        let p = pairing_prompt("Allow?", "192.168.1.27", "A device wants to pair.");
        assert_eq!(p.buttons[p.default].0, "Don\u{2019}t Allow");
        assert_eq!(p.cancel, DENY);
        assert_eq!(p.arm, Some((ALLOW, ARM_DELAY)));
        assert_eq!(p.timeout, Some(ANSWER_TIMEOUT));
        assert!(p.topmost);
        assert_eq!(p.buttons[ALLOW].1, Choice::Main);
    }
}
