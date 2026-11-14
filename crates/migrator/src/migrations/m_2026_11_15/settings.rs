//! SOLO: `terminal.program_notifications` used to also turn off the
//! notifications agents show when they need your input. Those now have their
//! own setting, `terminal.agent_input_notifications`; whoever had turned
//! program notifications off keeps agents' off too.

use anyhow::Result;
use serde_json::{Map, Value};

use crate::migrations::migrate_settings;

pub fn split_terminal_agent_input_notifications(value: &mut Value) -> Result<()> {
    migrate_settings(value, &mut migrate_one)
}

fn migrate_one(settings: &mut Map<String, Value>) -> Result<()> {
    let Some(terminal) = settings.get_mut("terminal").and_then(Value::as_object_mut) else {
        return Ok(());
    };
    if terminal.get("program_notifications") == Some(&Value::Bool(false))
        && !terminal.contains_key("agent_input_notifications")
    {
        terminal.insert("agent_input_notifications".to_string(), Value::Bool(false));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn turns_agent_input_notifications_off_where_program_notifications_were() {
        let mut settings = json!({
            "terminal": { "program_notifications": false, "font_size": 14 },
            "windows": { "terminal": { "program_notifications": false } },
            "profiles": {
                "work": { "settings": { "terminal": { "program_notifications": false } } }
            }
        });
        split_terminal_agent_input_notifications(&mut settings).unwrap();
        assert_eq!(
            settings,
            json!({
                "terminal": {
                    "program_notifications": false,
                    "font_size": 14,
                    "agent_input_notifications": false
                },
                "windows": {
                    "terminal": {
                        "program_notifications": false,
                        "agent_input_notifications": false
                    }
                },
                "profiles": {
                    "work": {
                        "settings": {
                            "terminal": {
                                "program_notifications": false,
                                "agent_input_notifications": false
                            }
                        }
                    }
                }
            })
        );
    }

    #[test]
    fn leaves_other_settings_alone() {
        for original in [
            json!({ "terminal": { "program_notifications": true } }),
            json!({ "terminal": { "font_size": 14 } }),
            json!({
                "terminal": {
                    "program_notifications": false,
                    "agent_input_notifications": true
                }
            }),
            json!({ "terminal": "not an object" }),
        ] {
            let mut settings = original.clone();
            split_terminal_agent_input_notifications(&mut settings).unwrap();
            assert_eq!(settings, original);
        }
    }
}
