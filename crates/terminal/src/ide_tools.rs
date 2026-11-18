//! SOLO: Gives agents started in Solo's terminals Solo's IDE tools: an MCP
//! server (the `solo_ide_tools` crate) on the loopback interface that answers
//! workspace symbol, definition, reference and diagnostics queries from the
//! language servers Solo already runs, for the workspace the terminal belongs
//! to. Turned off with the `terminal.ide_tools` setting.
//!
//! Each workspace has its own token, which only its terminals get (in
//! `SOLO_IDE_TOKEN`), so an agent can only ask about its own workspace.
//! Claude Code gets the server through the `claude` wrapper's `--mcp-config`
//! (`SOLO_CLAUDE_MCP_CONFIG`, which adds to the user's own servers) and Codex
//! through a `-c mcp_servers...` override in `SOLO_CODEX_ARGS`; neither's own
//! config changes. Other agents can be pointed at `SOLO_IDE_MCP_URL` with the
//! token as a bearer token.

use std::rc::Rc;

use collections::HashMap;
use gpui::{AnyWeakEntity, App, Global};

use crate::shell_integration::write_script;

/// The name agents see the server under.
pub const SERVER_NAME: &str = "solo-ide";
/// The environment variable with the workspace's token.
pub const TOKEN_ENV: &str = "SOLO_IDE_TOKEN";
/// The environment variable with the server's URL.
pub const URL_ENV: &str = "SOLO_IDE_MCP_URL";
/// The environment variable with the `--mcp-config` file the `claude`
/// wrapper passes. Solo's browser adds its server to the same file.
pub const CLAUDE_MCP_CONFIG_ENV: &str = "SOLO_CLAUDE_MCP_CONFIG";
/// How long Codex waits for a tool: a language server that just started may
/// take a while to answer.
const CODEX_TOOL_TIMEOUT_SECS: u32 = 120;

/// Where a workspace's IDE tools are.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Endpoint {
    pub url: String,
    pub token: String,
}

/// Gives the endpoint for a project (`project::Project`, which this crate
/// can't name), starting the server if needed; `None` if it can't be.
#[derive(Clone)]
pub struct Provider(pub Rc<dyn Fn(AnyWeakEntity, &mut App) -> Option<Endpoint>>);

impl Global for Provider {}

/// Adds the IDE tools of `project`'s workspace to a terminal's environment,
/// when a provider was set (by `solo_ide_tools::init`). Call this after the
/// agents' hooks were added to `env`: it goes through their wrappers.
pub fn integrate(project: AnyWeakEntity, env: &mut HashMap<String, String>, cx: &mut App) {
    let Some(provider) = cx.try_global::<Provider>().cloned() else {
        return;
    };
    if let Some(endpoint) = (provider.0)(project, cx) {
        add_endpoint(&endpoint, env);
    }
}

fn add_endpoint(endpoint: &Endpoint, env: &mut HashMap<String, String>) {
    env.insert(URL_ENV.to_string(), endpoint.url.clone());
    env.insert(TOKEN_ENV.to_string(), endpoint.token.clone());
    bypass_proxy_for_loopback(env);
    // Only the wrapper Claude Code's hooks set up passes the file.
    if env.contains_key("SOLO_CLAUDE_SETTINGS") {
        let existing = env
            .get(CLAUDE_MCP_CONFIG_ENV)
            .and_then(|path| std::fs::read_to_string(path).ok());
        match write_claude_mcp_config(endpoint, existing.as_deref()) {
            Ok(path) => {
                env.insert(CLAUDE_MCP_CONFIG_ENV.to_string(), path);
            }
            Err(error) => log::warn!("failed to write the IDE tools' MCP config: {error}"),
        }
    }
    let args = env.entry("SOLO_CODEX_ARGS".to_string()).or_default();
    if !args.is_empty() {
        args.push('\n');
    }
    args.push_str("-c\n");
    args.push_str(&codex_override(&endpoint.url));
}

/// SOLO: Adds the loopback addresses to a terminal's `NO_PROXY`, for agents
/// that reach Solo on this machine (its IDE tools, Codex's telemetry
/// receiver).
///
/// A proxy can't reach this machine's loopback interface: the request fails,
/// e.g. with a 503. Agents send HTTP through the proxy in `HTTP_PROXY` and the
/// like, and Codex also through the system proxy (Windows' Internet Options),
/// whose bypass list it ignores, so even `127.0.0.1` goes to the proxy. They
/// all honor `NO_PROXY`, so the loopback addresses are always added to it,
/// with or without a proxy in the environment: a system proxy can also be
/// turned on while the terminal runs.
pub(crate) fn bypass_proxy_for_loopback(env: &mut HashMap<String, String>) {
    bypass_proxy(env, |name| std::env::var(name).ok());
}

fn bypass_proxy(env: &mut HashMap<String, String>, process_env: impl Fn(&str) -> Option<String>) {
    // Windows' variables have one value whatever their case.
    let lookup = |env: &HashMap<String, String>, name: &str| -> Option<(String, String)> {
        let found = if cfg!(windows) {
            env.iter().find(|(key, _)| key.eq_ignore_ascii_case(name))
        } else {
            env.get_key_value(name)
        };
        found
            .map(|(key, value)| (key.clone(), value.clone()))
            .or_else(|| process_env(name).map(|value| (name.to_string(), value)))
    };
    // Tools elsewhere read either spelling.
    let names: &[&str] = if cfg!(windows) {
        &["NO_PROXY"]
    } else {
        &["NO_PROXY", "no_proxy"]
    };
    for name in names {
        let (key, current) = lookup(env, name).unwrap_or_else(|| (name.to_string(), String::new()));
        let mut hosts: Vec<String> = current
            .split(',')
            .map(|host| host.trim().to_string())
            .filter(|host| !host.is_empty())
            .collect();
        for host in ["127.0.0.1", "localhost"] {
            if !hosts
                .iter()
                .any(|existing| existing.eq_ignore_ascii_case(host))
            {
                hosts.push(host.to_string());
            }
        }
        env.insert(key, hosts.join(","));
    }
}

/// Claude Code's `--mcp-config` with the IDE tools added to the servers
/// already in `existing` (another config Solo passes, e.g. its browser's).
fn claude_mcp_config(endpoint: &Endpoint, existing: Option<&str>) -> serde_json::Value {
    let mut config = existing
        .and_then(|text| serde_json::from_str::<serde_json::Value>(text).ok())
        .filter(|config| config.get("mcpServers").is_some_and(|s| s.is_object()))
        .unwrap_or_else(|| serde_json::json!({ "mcpServers": {} }));
    config["mcpServers"][SERVER_NAME] = serde_json::json!({
        "type": "http",
        "url": endpoint.url,
        "headers": { "Authorization": format!("Bearer {}", endpoint.token) },
    });
    config
}

/// Writes the config to the private scripts dir, named after its contents
/// (it holds the workspace's token, so each workspace has its own).
fn write_claude_mcp_config(endpoint: &Endpoint, existing: Option<&str>) -> std::io::Result<String> {
    use sha2::Digest as _;
    let config = claude_mcp_config(endpoint, existing);
    let config = serde_json::to_string_pretty(&config).map_err(std::io::Error::other)?;
    let digest = sha2::Sha256::digest(config.as_bytes());
    let short: String = digest[..8]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    write_script(&format!("claude-mcp-{short}.json"), &config)
}

/// Codex's `-c` override that adds the server for one session: as TOML with
/// literal strings, which Windows PowerShell passes on as they are. The token
/// stays in the environment (`bearer_token_env_var`), off the command line.
fn codex_override(url: &str) -> String {
    format!(
        "mcp_servers.{SERVER_NAME}={{url='{}',bearer_token_env_var='{TOKEN_ENV}',tool_timeout_sec={CODEX_TOOL_TIMEOUT_SECS}}}",
        url.replace('\'', "")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn endpoint() -> Endpoint {
        Endpoint {
            url: "http://127.0.0.1:41234/mcp".to_string(),
            token: "abc123".to_string(),
        }
    }

    #[test]
    fn claude_gets_a_config_and_codex_an_override() {
        let mut env = HashMap::default();
        env.insert("SOLO_CLAUDE_SETTINGS".to_string(), "s.json".to_string());
        env.insert("SOLO_CODEX_ARGS".to_string(), "-c\nhooks=x".to_string());
        add_endpoint(&endpoint(), &mut env);

        assert_eq!(env[URL_ENV], "http://127.0.0.1:41234/mcp");
        assert_eq!(env[TOKEN_ENV], "abc123");
        let config = std::fs::read_to_string(&env[CLAUDE_MCP_CONFIG_ENV]).unwrap();
        let config: serde_json::Value = serde_json::from_str(&config).unwrap();
        let server = &config["mcpServers"][SERVER_NAME];
        assert_eq!(server["type"], "http");
        assert_eq!(server["url"], "http://127.0.0.1:41234/mcp");
        assert_eq!(server["headers"]["Authorization"], "Bearer abc123");

        let codex: Vec<&str> = env["SOLO_CODEX_ARGS"].split('\n').collect();
        assert_eq!(
            codex,
            [
                "-c",
                "hooks=x",
                "-c",
                "mcp_servers.solo-ide={url='http://127.0.0.1:41234/mcp',bearer_token_env_var='SOLO_IDE_TOKEN',tool_timeout_sec=120}",
            ]
        );
        // The override is TOML Codex can read.
        let (key, value) = codex[3].split_once('=').unwrap();
        assert_eq!(key, "mcp_servers.solo-ide");
        let value: toml::Value = toml::from_str(&format!("v = {value}")).unwrap();
        assert_eq!(value["v"]["bearer_token_env_var"].as_str(), Some(TOKEN_ENV));
        assert!(!codex[3].contains("abc123"));
    }

    #[test]
    fn without_claude_hooks_only_codex_and_the_environment() {
        let mut env = HashMap::default();
        add_endpoint(&endpoint(), &mut env);
        assert!(!env.contains_key(CLAUDE_MCP_CONFIG_ENV));
        assert!(env["SOLO_CODEX_ARGS"].starts_with("-c\nmcp_servers.solo-ide="));
        assert_eq!(env[TOKEN_ENV], "abc123");
        // Without a proxy set too: Codex would use the system proxy.
        let no_proxy = env
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("NO_PROXY"));
        assert!(
            no_proxy.is_some_and(|(_, hosts)| hosts.split(',').any(|host| host == "127.0.0.1")),
            "{no_proxy:?}"
        );
    }

    #[test]
    fn loopback_bypasses_a_proxy() {
        let none = |_: &str| None;
        // Without a proxy in the environment too: Codex uses the system
        // proxy, ignoring its bypass list.
        let mut env = HashMap::default();
        bypass_proxy(&mut env, none);
        assert_eq!(env["NO_PROXY"], "127.0.0.1,localhost");

        env.insert(
            "HTTPS_PROXY".to_string(),
            "http://127.0.0.1:10808".to_string(),
        );
        env.insert("NO_PROXY".to_string(), "corp.example".to_string());
        bypass_proxy(&mut env, none);
        assert_eq!(env["NO_PROXY"], "corp.example,127.0.0.1,localhost");
        // Again: nothing added twice.
        bypass_proxy(&mut env, none);
        assert_eq!(env["NO_PROXY"], "corp.example,127.0.0.1,localhost");

        // Solo's own `NO_PROXY` is kept.
        let mut env = HashMap::default();
        bypass_proxy(&mut env, |name| {
            (name == "NO_PROXY").then(|| "corp.example".to_string())
        });
        assert_eq!(env["NO_PROXY"], "corp.example,127.0.0.1,localhost");
    }

    #[test]
    fn keeps_the_servers_already_passed_to_claude() {
        let existing = r#"{"mcpServers":{"solo-browser":{"type":"stdio","command":"npx"}}}"#;
        let config = claude_mcp_config(&endpoint(), Some(existing));
        assert_eq!(config["mcpServers"]["solo-browser"]["command"], "npx");
        assert_eq!(config["mcpServers"][SERVER_NAME]["type"], "http");

        let config = claude_mcp_config(&endpoint(), Some("not json"));
        assert_eq!(config["mcpServers"].as_object().unwrap().len(), 1);
    }

    /// After Solo's browser added its server (`integrate_browser`, which
    /// runs first), both reach Claude Code in one file and Codex in two
    /// overrides.
    #[test]
    fn comes_with_the_browsers_server() {
        let dir = tempfile::tempdir().unwrap();
        let browser = dir.path().join("browser-mcp.json");
        std::fs::write(
            &browser,
            r#"{"mcpServers":{"solo-browser":{"type":"stdio","command":"npx","args":["-y"]}}}"#,
        )
        .unwrap();
        let mut env = HashMap::default();
        env.insert("SOLO_CLAUDE_SETTINGS".to_string(), "s.json".to_string());
        env.insert(
            CLAUDE_MCP_CONFIG_ENV.to_string(),
            browser.to_string_lossy().into_owned(),
        );
        env.insert(
            "SOLO_CODEX_ARGS".to_string(),
            "-c\nhooks=x\n-c\nmcp_servers.solo-browser={command='npx'}".to_string(),
        );
        add_endpoint(&endpoint(), &mut env);

        let config = std::fs::read_to_string(&env[CLAUDE_MCP_CONFIG_ENV]).unwrap();
        let config: serde_json::Value = serde_json::from_str(&config).unwrap();
        let servers = config["mcpServers"].as_object().unwrap();
        assert_eq!(servers["solo-browser"]["command"], "npx");
        assert_eq!(servers[SERVER_NAME]["type"], "http");
        let codex: Vec<&str> = env["SOLO_CODEX_ARGS"].split('\n').collect();
        assert_eq!(codex.len(), 6);
        assert!(codex[3].starts_with("mcp_servers.solo-browser="));
        assert_eq!(codex[4], "-c");
        assert!(codex[5].starts_with("mcp_servers.solo-ide="));
    }

    #[test]
    fn each_workspace_has_its_own_config_file() {
        let mut first = HashMap::default();
        first.insert("SOLO_CLAUDE_SETTINGS".to_string(), "s.json".to_string());
        let mut second = first.clone();
        add_endpoint(&endpoint(), &mut first);
        add_endpoint(
            &Endpoint {
                token: "other".to_string(),
                ..endpoint()
            },
            &mut second,
        );
        assert_ne!(first[CLAUDE_MCP_CONFIG_ENV], second[CLAUDE_MCP_CONFIG_ENV]);
    }

    /// The PowerShell wrappers pass the config to a fake `claude` and the
    /// override to a fake `codex`, which print what they were given.
    #[cfg(windows)]
    #[test]
    #[allow(
        clippy::disallowed_methods,
        reason = "a test waiting for a short shell script"
    )]
    fn powershell_wrappers_pass_the_server() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        for name in ["claude", "codex"] {
            std::fs::write(
                bin.join(format!("{name}.ps1")),
                format!("Write-Output (\"{name} ARGS: \" + ($args -join ' '))\n"),
            )
            .unwrap();
        }
        let script = dir.path().join("integration.ps1");
        std::fs::write(&script, crate::shell_integration::POWERSHELL_SCRIPT).unwrap();
        let settings = dir.path().join("settings.json");
        std::fs::write(&settings, "{}").unwrap();

        let mut env = HashMap::default();
        env.insert(
            "SOLO_CLAUDE_SETTINGS".to_string(),
            settings.to_string_lossy().into_owned(),
        );
        add_endpoint(&endpoint(), &mut env);
        let path = format!(
            "{};{}",
            bin.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let Ok(output) = std::process::Command::new("powershell.exe")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-ExecutionPolicy",
                "Bypass",
            ])
            .arg("-Command")
            .arg(format!(
                ". ([scriptblock]::Create([IO.File]::ReadAllText('{}'))); claude hi; codex exec hi",
                script.display()
            ))
            .envs(env.iter())
            .env("PATH", path)
            .output()
        else {
            return;
        };
        let stdout = String::from_utf8_lossy(&output.stdout);
        let args = |name: &str| {
            stdout
                .lines()
                .find_map(|line| Some(line.split_once(&format!("{name} ARGS: "))?.1.to_string()))
                .unwrap_or_else(|| panic!("no {name} output in {stdout}"))
        };
        assert_eq!(
            args("claude"),
            format!(
                "--mcp-config {} --settings {} hi",
                env[CLAUDE_MCP_CONFIG_ENV],
                settings.display()
            )
        );
        assert_eq!(
            args("codex"),
            "-c mcp_servers.solo-ide={url='http://127.0.0.1:41234/mcp',bearer_token_env_var='SOLO_IDE_TOKEN',tool_timeout_sec=120} exec hi"
        );
    }

    /// The bash wrapper passes the config to a fake `claude` (PATH holds
    /// nothing else) before `--settings`, which ends `--mcp-config`'s list.
    #[test]
    #[allow(
        clippy::disallowed_methods,
        reason = "a test waiting for a short shell script"
    )]
    fn bash_claude_wrapper_passes_the_config() {
        use std::path::Path;
        // Not WSL's bash in System32: it has its own file system.
        let Some(bash) = std::env::var_os("PATH").and_then(|path| {
            std::env::split_paths(&path)
                .filter(|dir| !dir.to_string_lossy().to_lowercase().contains("system32"))
                .map(|dir| dir.join(if cfg!(windows) { "bash.exe" } else { "bash" }))
                .find(|bash| bash.is_file())
        }) else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let fake = bin.join("claude");
        std::fs::write(&fake, "#!/bin/sh\necho \"ARGS: $*\"\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let script = dir.path().join("integration.sh");
        std::fs::write(&script, crate::shell_integration::BASH_SCRIPT).unwrap();
        let settings = dir.path().join("settings.json");
        std::fs::write(&settings, "{}").unwrap();
        let posix = |path: &Path| {
            let path = path.to_string_lossy().replace('\\', "/");
            match path.split_once(":/") {
                Some((drive, rest)) if cfg!(windows) && drive.len() == 1 => {
                    format!("/{}/{rest}", drive.to_ascii_lowercase())
                }
                _ => path,
            }
        };
        let mut env = HashMap::default();
        env.insert("SOLO_CLAUDE_SETTINGS".to_string(), posix(&settings));
        add_endpoint(&endpoint(), &mut env);
        let mcp = std::path::PathBuf::from(&env[CLAUDE_MCP_CONFIG_ENV]);
        let output = std::process::Command::new(&bash)
            .env_clear()
            .env("PATH", posix(&bin))
            .env("SOLO_CLAUDE_SETTINGS", posix(&settings))
            .env(CLAUDE_MCP_CONFIG_ENV, posix(&mcp))
            .args(["--norc", "--noprofile", "-c"])
            .arg(format!(
                "source '{}'; [ \"$(command -v claude)\" = claude ] && claude hi",
                posix(&script)
            ))
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let Some(args) = stdout.lines().find_map(|line| line.strip_prefix("ARGS: ")) else {
            // This bash can't run the fake (e.g. WSL's bash.exe): nothing to check.
            return;
        };
        assert_eq!(
            args,
            format!(
                "--mcp-config {} --settings {} hi",
                posix(&mcp),
                posix(&settings)
            )
        );
    }
}
