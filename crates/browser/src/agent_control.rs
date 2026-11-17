//! Letting agents started in Solo's terminal control Solo's browser.
//!
//! When the `browser.allow_agent_control` setting is on as Solo starts, the
//! browser process opens a remote debugging port on 127.0.0.1 (a random free
//! one), and terminals hand it to agents (`terminal::shell_integration::
//! integrate_browser`). The port gives full control of the browser, signed-in
//! pages included, to any program on this computer that finds it, so it is off
//! by default and never listens beyond 127.0.0.1.

use std::sync::OnceLock;

use gpui::App;
use settings::Settings as _;

use crate::BrowserSettings;

static PORT: OnceLock<Option<u16>> = OnceLock::new();

pub fn init(cx: &mut App) {
    let port = debugging_port(cx);
    if port.is_some() && !node_available() {
        log::warn!(
            "agents can't use Solo's browser: Playwright MCP needs Node.js, which isn't on the PATH"
        );
    }
    terminal::shell_integration::set_browser_debugging_port(port);
}

/// The remote debugging port the browser process opens, if agents may control
/// it. Decided once: the setting takes effect after Solo restarts.
pub fn debugging_port(cx: &App) -> Option<u16> {
    *PORT.get_or_init(|| {
        if !BrowserSettings::get_global(cx).allow_agent_control {
            return None;
        }
        match free_local_port() {
            Ok(port) => Some(port),
            Err(error) => {
                log::error!("no free port for the browser's debugging port: {error}");
                None
            }
        }
    })
}

/// A port nothing listens on now, on 127.0.0.1.
pub(crate) fn free_local_port() -> std::io::Result<u16> {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0))?;
    Ok(listener.local_addr()?.port())
}

/// Whether Node.js (which runs the Playwright MCP server) is on the PATH.
pub fn node_available() -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    let names: &[&str] = if cfg!(windows) {
        &["node.exe", "npx.cmd"]
    } else {
        &["node", "npx"]
    };
    names
        .iter()
        .all(|name| std::env::split_paths(&path).any(|dir| dir.join(name).is_file()))
}
