//! Which keystrokes Solo takes over while a web page has keyboard focus.

use gpui::Modifiers;

/// Keys pages need for editing text and moving around, even though Solo binds
/// some of them: select all, clipboard, undo/redo, word-wise moves and deletes.
const PAGE_KEYS: &[&str] = &[
    "ctrl-a",
    "ctrl-c",
    "ctrl-v",
    "ctrl-x",
    "ctrl-z",
    "ctrl-y",
    "ctrl-shift-z",
    "ctrl-shift-v",
    "ctrl-b",
    "ctrl-i",
    "ctrl-u",
    "ctrl-k",
    "ctrl-enter",
    "ctrl-shift-enter",
    "ctrl-backspace",
    "ctrl-delete",
    "ctrl-left",
    "ctrl-right",
    "ctrl-up",
    "ctrl-down",
    "ctrl-home",
    "ctrl-end",
    "ctrl-shift-left",
    "ctrl-shift-right",
    "ctrl-shift-up",
    "ctrl-shift-down",
    "ctrl-shift-home",
    "ctrl-shift-end",
];

/// Whether a binding whose first keystroke is `modifiers`+`key` should be taken
/// from the page and handled by Solo. Only Ctrl/Alt combinations and function
/// keys are ever taken; plain typing, Esc and Tab always stay with the page.
pub fn forward_to_solo(modifiers: &Modifiers, key: &str) -> bool {
    let function_key = key.len() > 1 && key.starts_with('f') && key[1..].parse::<u8>().is_ok();
    if !(modifiers.control || modifiers.alt || function_key) {
        return false;
    }
    if modifiers.platform || modifiers.function {
        return false;
    }
    let mut name = String::new();
    if modifiers.control {
        name.push_str("ctrl-");
    }
    if modifiers.alt {
        name.push_str("alt-");
    }
    if modifiers.shift {
        name.push_str("shift-");
    }
    name.push_str(key);
    !PAGE_KEYS.contains(&name.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctrl() -> Modifiers {
        Modifiers::control()
    }

    #[test]
    fn solo_shortcuts_are_forwarded() {
        assert!(forward_to_solo(&Modifiers::control_shift(), "p"));
        assert!(forward_to_solo(&ctrl(), "`"));
        assert!(forward_to_solo(&ctrl(), "w"));
        assert!(forward_to_solo(&Modifiers::default(), "f12"));
        assert!(forward_to_solo(&Modifiers::alt(), "left"));
    }

    #[test]
    fn page_keys_stay_with_the_page() {
        assert!(!forward_to_solo(&ctrl(), "a"));
        assert!(!forward_to_solo(&ctrl(), "c"));
        assert!(!forward_to_solo(&ctrl(), "v"));
        assert!(!forward_to_solo(&ctrl(), "z"));
        assert!(!forward_to_solo(&Modifiers::control_shift(), "left"));
        assert!(!forward_to_solo(&Modifiers::default(), "escape"));
        assert!(!forward_to_solo(&Modifiers::default(), "a"));
        assert!(!forward_to_solo(&Modifiers::shift(), "tab"));
    }
}
