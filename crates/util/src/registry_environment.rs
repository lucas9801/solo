//! SOLO: The environment a new local terminal gets on Windows, read again from
//! the registry the way Windows Terminal does for each new tab.
//!
//! Solo's own environment can be stale or short: it's inherited from whatever
//! started Solo (Explorer, an installer's finish page), and Explorer's logon
//! environment can lack the user's `Path` entirely (seen with a 4.9 KB user
//! `Path`: Explorer started Solo with the machine `Path` only, so `claude` and
//! `codex` weren't found in its terminals, even after a restart).
//!
//! How the values merge:
//! - Every `REG_SZ`/`REG_EXPAND_SZ` variable of
//!   `HKLM\SYSTEM\CurrentControlSet\Control\Session Manager\Environment` and
//!   then `HKCU\Environment` is read, `REG_EXPAND_SZ` values expanded
//!   (`%NAME%`, from the values read so far, then Solo's environment). A user
//!   variable replaces the machine one, except `Path`, `LIBPATH` and
//!   `Os2LibPath`, which are the machine value followed by the user's, as at
//!   logon. That's also how Windows Terminal builds a new tab's environment.
//! - `PATH` is the registry's entries (machine, then user) followed by the
//!   entries of Solo's own `PATH` that the registry doesn't have (what its
//!   launcher or Solo itself added), without duplicates (compared ignoring
//!   case and a trailing `\`). Windows Terminal drops those; Solo keeps them,
//!   after the registry's.
//! - Variables of the logon session (`USERNAME`, `USERPROFILE`, ...; the
//!   machine key has `USERNAME=SYSTEM`) and Solo's own `SOLO_*` variables keep
//!   Solo's values.
//! - Variables a terminal is given explicitly (a task's or the settings' `env`,
//!   the environment passed by `solo` on the command line, Solo's
//!   integrations) win over all of this: see [`apply`].
//! - Variables Solo's environment has and the registry doesn't stay.

use collections::HashMap;

/// One string value of an environment key in the registry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegistryValue {
    pub name: String,
    pub value: String,
    /// `REG_EXPAND_SZ`: `%NAME%` in it is expanded.
    pub expand: bool,
}

/// The machine's and the user's environment variables in the registry.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RegistryEnvironment {
    pub machine: Vec<RegistryValue>,
    pub user: Vec<RegistryValue>,
}

/// Variables whose user value follows the machine's instead of replacing it.
const CONCATENATED: [&str; 3] = ["PATH", "LIBPATH", "OS2LIBPATH"];

/// Variables the logon session sets, which keep Solo's values.
const SESSION_VARIABLES: [&str; 13] = [
    "USERNAME",
    "USERDOMAIN",
    "USERDOMAIN_ROAMINGPROFILE",
    "USERDNSDOMAIN",
    "USERPROFILE",
    "APPDATA",
    "LOCALAPPDATA",
    "HOMEDRIVE",
    "HOMEPATH",
    "HOMESHARE",
    "LOGONSERVER",
    "SESSIONNAME",
    "COMPUTERNAME",
];

fn is_kept_from_process(name: &str) -> bool {
    let upper = name.to_uppercase();
    upper.starts_with("SOLO_") || SESSION_VARIABLES.contains(&upper.as_str())
}

/// The variables a new local terminal should get from `registry`, merged with
/// `process` (Solo's own environment) as described in the module docs.
pub fn fresh_variables(
    registry: &RegistryEnvironment,
    process: &[(String, String)],
) -> Vec<(String, String)> {
    let mut variables: Vec<(String, String)> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::default();
    let process_value = |name: &str| {
        process
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.clone())
    };

    for (values, is_user) in [(&registry.machine, false), (&registry.user, true)] {
        for value in values {
            if value.name.is_empty() || is_kept_from_process(&value.name) {
                continue;
            }
            let text = if value.expand {
                expand(&value.value, |name| {
                    index
                        .get(&name.to_uppercase())
                        .map(|&ix| variables[ix].1.clone())
                        .or_else(|| process_value(name))
                })
            } else {
                value.value.clone()
            };
            let key = value.name.to_uppercase();
            match index.get(&key) {
                Some(&ix) if is_user && CONCATENATED.contains(&key.as_str()) => {
                    let machine = &variables[ix].1;
                    variables[ix].1 = if machine.is_empty() {
                        text
                    } else if text.is_empty() {
                        machine.clone()
                    } else {
                        format!("{};{text}", machine.trim_end_matches(';'))
                    };
                }
                Some(&ix) => variables[ix].1 = text,
                None => {
                    index.insert(key, variables.len());
                    variables.push((value.name.clone(), text));
                }
            }
        }
    }

    if let Some(&ix) = index.get("PATH") {
        let process_path = process_value("PATH").unwrap_or_default();
        variables[ix].1 = merge_paths(&variables[ix].1, &process_path);
    }
    variables
}

/// `registry`'s entries, then `process`'s that aren't among them, each once.
pub fn merge_paths(registry: &str, process: &str) -> String {
    let mut seen = std::collections::HashSet::new();
    let mut entries = Vec::new();
    for entry in registry.split(';').chain(process.split(';')) {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        let mut normalized = entry.to_lowercase();
        while normalized.len() > 1 && normalized.ends_with('\\') && !normalized.ends_with(":\\") {
            normalized.pop();
        }
        if seen.insert(normalized) {
            entries.push(entry);
        }
    }
    entries.join(";")
}

/// Expands each `%NAME%` that `lookup` knows; others stay as written, as
/// Windows leaves them.
fn expand(value: &str, lookup: impl Fn(&str) -> Option<String>) -> String {
    let mut result = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(start) = rest.find('%') {
        result.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        match after.find('%') {
            Some(end) => {
                let name = &after[..end];
                match (!name.is_empty()).then(|| lookup(name)).flatten() {
                    Some(expanded) => {
                        result.push_str(&expanded);
                        rest = &after[end + 1..];
                    }
                    None => {
                        // The closing `%` may open the next name.
                        result.push('%');
                        result.push_str(name);
                        rest = &after[end..];
                    }
                }
            }
            None => {
                result.push('%');
                rest = after;
            }
        }
    }
    result.push_str(rest);
    result
}

/// Puts `fresh` into `env`, except variables `env` already sets (whatever
/// their case): those were given explicitly.
pub fn apply(env: &mut HashMap<String, String>, fresh: Vec<(String, String)>) {
    for (name, value) in fresh {
        if !env.keys().any(|key| key.eq_ignore_ascii_case(&name)) {
            env.insert(name, value);
        }
    }
}

/// Reads the environment keys of the registry (read-only). A key that can't
/// be read counts as empty.
#[cfg(windows)]
pub fn read_registry() -> RegistryEnvironment {
    use windows_registry::{CURRENT_USER, LOCAL_MACHINE, Type};

    fn read(root: &windows_registry::Key, path: &str) -> Vec<RegistryValue> {
        let key = match root.open(path) {
            Ok(key) => key,
            Err(error) => {
                log::warn!("reading the environment from the registry key {path}: {error}");
                return Vec::new();
            }
        };
        let values = match key.values() {
            Ok(values) => values,
            Err(error) => {
                log::warn!("reading the environment from the registry key {path}: {error}");
                return Vec::new();
            }
        };
        values
            .filter_map(|(name, value)| {
                let expand = match value.ty() {
                    Type::String => false,
                    Type::ExpandString => true,
                    _ => return None,
                };
                let value = String::try_from(value).ok()?;
                Some(RegistryValue {
                    name,
                    value,
                    expand,
                })
            })
            .collect()
    }

    RegistryEnvironment {
        machine: read(
            &LOCAL_MACHINE,
            r"SYSTEM\CurrentControlSet\Control\Session Manager\Environment",
        ),
        user: read(&CURRENT_USER, "Environment"),
    }
}

/// Solo's own environment, as strings.
pub fn process_variables() -> Vec<(String, String)> {
    std::env::vars_os()
        .map(|(key, value)| {
            (
                key.to_string_lossy().into_owned(),
                value.to_string_lossy().into_owned(),
            )
        })
        .collect()
}

/// The registry's current environment merged with Solo's, for a new local
/// process on Windows; nothing elsewhere.
pub fn current_fresh_variables() -> Vec<(String, String)> {
    #[cfg(windows)]
    {
        fresh_variables(&read_registry(), &process_variables())
    }
    #[cfg(not(windows))]
    {
        Vec::new()
    }
}

/// Refreshes `env`, the environment of a new local terminal, from the
/// registry on Windows (see the module docs); variables it sets stay.
pub fn refresh(env: &mut HashMap<String, String>) {
    apply(env, current_fresh_variables());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn value(name: &str, value: &str, expand: bool) -> RegistryValue {
        RegistryValue {
            name: name.to_string(),
            value: value.to_string(),
            expand,
        }
    }

    fn get<'a>(variables: &'a [(String, String)], name: &str) -> Option<&'a str> {
        variables
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    fn process(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect()
    }

    #[test]
    fn path_is_machine_then_user_then_what_solo_has_besides() {
        let registry = RegistryEnvironment {
            machine: vec![
                value(
                    "Path",
                    r"%SystemRoot%\system32;%SystemRoot%;C:\Program Files\Git\cmd",
                    true,
                ),
                value("SystemRoot", r"C:\Windows", false),
            ],
            user: vec![value(
                "Path",
                r"%USERPROFILE%\.local\bin;%USERPROFILE%\AppData\Roaming\npm\",
                true,
            )],
        };
        // Started by Explorer without the user's Path, plus a folder added
        // by Solo's launcher; Windows spells the name `Path`.
        let solo = process(&[
            (
                "Path",
                r"C:\WINDOWS\system32;C:\WINDOWS;C:\Program Files\Git\cmd;E:\Solo\bin",
            ),
            ("SystemRoot", r"C:\WINDOWS"),
            ("USERPROFILE", r"C:\Users\Admin"),
        ]);
        let fresh = fresh_variables(&registry, &solo);
        assert_eq!(
            get(&fresh, "PATH"),
            Some(
                r"C:\WINDOWS\system32;C:\WINDOWS;C:\Program Files\Git\cmd;C:\Users\Admin\.local\bin;C:\Users\Admin\AppData\Roaming\npm\;E:\Solo\bin"
            )
        );
    }

    #[test]
    fn a_user_path_longer_than_the_old_limits_is_kept_whole() {
        let user_entries: Vec<String> = (0..120)
            .map(|ix| format!(r"%USERPROFILE%\tools\some-long-folder-name-{ix:03}\bin"))
            .collect();
        let user_path = user_entries.join(";");
        assert!(user_path.len() > 4096);
        let registry = RegistryEnvironment {
            machine: vec![value("Path", r"C:\Windows\system32;C:\Windows", true)],
            user: vec![value("Path", &user_path, true)],
        };
        let solo = process(&[
            ("PATH", r"C:\Windows\system32;C:\Windows"),
            ("USERPROFILE", r"C:\Users\Admin"),
        ]);
        let fresh = fresh_variables(&registry, &solo);
        let path = get(&fresh, "PATH").unwrap();
        let entries: Vec<&str> = path.split(';').collect();
        assert_eq!(entries.len(), 122);
        assert_eq!(
            entries[2],
            r"C:\Users\Admin\tools\some-long-folder-name-000\bin"
        );
        assert_eq!(
            entries[121],
            r"C:\Users\Admin\tools\some-long-folder-name-119\bin"
        );
    }

    #[test]
    fn user_values_replace_machine_ones_and_expand() {
        let registry = RegistryEnvironment {
            machine: vec![
                value("TEMP", r"%SystemRoot%\TEMP", true),
                value("PATHEXT", ".COM;.EXE;.BAT;.CMD", false),
                value("USERNAME", "SYSTEM", false),
                value("LITERAL", "%NOT_EXPANDED%", false),
                value("LIBPATH", r"C:\machine", false),
            ],
            user: vec![
                value("TEMP", r"%USERPROFILE%\AppData\Local\Temp", true),
                value("TOOL_HOME", r"%TEMP%\tool;%UNKNOWN%;100%", true),
                value("LIBPATH", r"C:\user", false),
                value("SOLO_SOMETHING", "from registry", false),
            ],
        };
        let solo = process(&[
            ("SystemRoot", r"C:\Windows"),
            ("USERPROFILE", r"C:\Users\Admin"),
            ("USERNAME", "Admin"),
            ("TEMP", r"C:\stale"),
        ]);
        let fresh = fresh_variables(&registry, &solo);
        assert_eq!(
            get(&fresh, "TEMP"),
            Some(r"C:\Users\Admin\AppData\Local\Temp")
        );
        assert_eq!(get(&fresh, "PATHEXT"), Some(".COM;.EXE;.BAT;.CMD"));
        assert_eq!(get(&fresh, "LITERAL"), Some("%NOT_EXPANDED%"));
        assert_eq!(
            get(&fresh, "TOOL_HOME"),
            Some(r"C:\Users\Admin\AppData\Local\Temp\tool;%UNKNOWN%;100%")
        );
        assert_eq!(get(&fresh, "LIBPATH"), Some(r"C:\machine;C:\user"));
        // Session and Solo variables keep Solo's values.
        assert_eq!(get(&fresh, "USERNAME"), None);
        assert_eq!(get(&fresh, "SOLO_SOMETHING"), None);
        // Without a Path in the registry, Solo's stays.
        assert_eq!(get(&fresh, "PATH"), None);
    }

    #[test]
    fn explicit_variables_win() {
        let mut env = HashMap::default();
        env.insert("Path".to_string(), r"C:\from-the-cli".to_string());
        env.insert("MY_TASK_VAR".to_string(), "task".to_string());
        apply(
            &mut env,
            vec![
                ("PATH".to_string(), r"C:\registry".to_string()),
                ("MY_TASK_VAR".to_string(), "registry".to_string()),
                ("PATHEXT".to_string(), ".EXE".to_string()),
            ],
        );
        assert_eq!(env.len(), 3);
        assert_eq!(env["Path"], r"C:\from-the-cli");
        assert_eq!(env["MY_TASK_VAR"], "task");
        assert_eq!(env["PATHEXT"], ".EXE");
    }

    #[cfg(windows)]
    #[test]
    fn reads_the_machine_path_from_the_registry() {
        let registry = read_registry();
        let path = registry
            .machine
            .iter()
            .find(|value| value.name.eq_ignore_ascii_case("PATH"))
            .expect("the machine has a Path");
        assert!(path.value.to_lowercase().contains("system32"));
        let fresh = fresh_variables(&registry, &process_variables());
        let path = get(&fresh, "PATH").unwrap().to_lowercase();
        assert!(!path.contains("%systemroot%"), "expanded: {path}");
    }

    #[test]
    fn merging_paths_drops_duplicates_and_empty_entries() {
        assert_eq!(
            merge_paths(r"C:\A;;c:\b\;C:\", r"C:\a\;C:\B;D:\new;c:\;D:\NEW"),
            r"C:\A;c:\b\;C:\;D:\new"
        );
    }
}
