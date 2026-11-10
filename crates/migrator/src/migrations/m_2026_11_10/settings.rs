//! SOLO: `terminal.open_changed_files` was removed: Solo no longer opens files
//! an agent changes; each turn lists them on the terminal's tab instead.

use anyhow::Result;
use serde_json::{Map, Value};

use crate::migrations::migrate_settings;

pub fn remove_terminal_open_changed_files(value: &mut Value) -> Result<()> {
    migrate_settings(value, &mut migrate_one)
}

fn migrate_one(settings: &mut Map<String, Value>) -> Result<()> {
    let Some(terminal) = settings.get_mut("terminal").and_then(Value::as_object_mut) else {
        return Ok(());
    };
    terminal.remove("open_changed_files");
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn removes_the_setting_in_all_settings_scopes() {
        let mut settings = json!({
            "terminal": { "open_changed_files": false, "font_size": 14 },
            "windows": { "terminal": { "open_changed_files": true } },
            "profiles": {
                "work": { "settings": { "terminal": { "open_changed_files": false } } }
            }
        });
        remove_terminal_open_changed_files(&mut settings).unwrap();
        assert_eq!(
            settings,
            json!({
                "terminal": { "font_size": 14 },
                "windows": { "terminal": {} },
                "profiles": { "work": { "settings": { "terminal": {} } } }
            })
        );
    }

    #[test]
    fn leaves_other_settings_alone() {
        let original =
            json!({ "terminal": "not an object", "editor": { "open_changed_files": 1 } });
        let mut settings = original.clone();
        remove_terminal_open_changed_files(&mut settings).unwrap();
        assert_eq!(settings, original);
    }
}
