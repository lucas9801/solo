//! SOLO: the programs running under a terminal's shell, found from the
//! process tree. A terminal connected again to a host an earlier Solo left
//! running gets the host's screen, not the shell's marks (see `host`), and on
//! Windows the terminal's foreground process is always the shell itself: so
//! what runs in it (an agent, say) is found here instead, until the shell's
//! next prompt.

use std::path::Path;

/// A program running under the shell.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShellProgram {
    pub pid: u32,
    /// What it runs, as a command line starting with the program's name:
    /// `claude --resume` for `node …\claude-code\cli.js --resume`, or for
    /// `cmd /c "…\claude.cmd" --resume`.
    pub command: String,
    /// Its whole command line.
    pub command_line: String,
}

/// How deep below the shell to look: a program the shell started through a
/// script runner (`cmd /c x.cmd`, a PowerShell script) is one level down.
const MAX_DEPTH: usize = 3;

/// Programs that are part of every console, not something the shell runs.
const CONSOLE_PROGRAMS: &[&str] = &["conhost", "openconsole"];

/// The programs running under the process `shell_pid`, nearest first.
#[cfg(windows)]
pub fn shell_programs(shell_pid: u32) -> Vec<ShellProgram> {
    use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

    let mut system = System::new();
    system.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing()
            .with_cmd(UpdateKind::Always)
            .with_exe(UpdateKind::Always)
            .without_tasks(),
    );
    let processes: Vec<(u32, u32, Vec<String>)> = system
        .processes()
        .values()
        .filter_map(|process| {
            let parent = process.parent()?.as_u32();
            let argv = process
                .cmd()
                .iter()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect::<Vec<_>>();
            let argv = if argv.is_empty() {
                vec![process.name().to_string_lossy().into_owned()]
            } else {
                argv
            };
            Some((process.pid().as_u32(), parent, argv))
        })
        .collect();
    descendants(shell_pid, &processes)
}

#[cfg(not(windows))]
pub fn shell_programs(_shell_pid: u32) -> Vec<ShellProgram> {
    Vec::new()
}

/// Whether the process `pid` still runs.
#[cfg(windows)]
pub fn is_running(pid: u32) -> bool {
    use windows::Win32::Foundation::{CloseHandle, WAIT_TIMEOUT};
    use windows::Win32::System::Threading::{
        OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject,
    };
    unsafe {
        let Ok(process) = OpenProcess(PROCESS_SYNCHRONIZE, false, pid) else {
            return false;
        };
        let running = WaitForSingleObject(process, 0) == WAIT_TIMEOUT;
        CloseHandle(process).ok();
        running
    }
}

#[cfg(not(windows))]
pub fn is_running(_pid: u32) -> bool {
    false
}

/// Of `processes` (pid, parent pid, argv), those below `root`, nearest first.
fn descendants(root: u32, processes: &[(u32, u32, Vec<String>)]) -> Vec<ShellProgram> {
    let mut found = Vec::new();
    let mut level = vec![root];
    for _ in 0..MAX_DEPTH {
        let mut next = Vec::new();
        for (pid, parent, argv) in processes {
            // A pid is reused only once its process is gone, which a parent
            // that's still running isn't; its own pid can't be its parent.
            if !level.contains(parent) || *pid == *parent || *pid == root {
                continue;
            }
            let Some(command) = program_command(argv) else {
                continue;
            };
            next.push(*pid);
            let name = command.split(' ').next().unwrap_or_default();
            if CONSOLE_PROGRAMS.contains(&name) {
                continue;
            }
            found.push(ShellProgram {
                pid: *pid,
                command,
                command_line: argv.join(" "),
            });
        }
        if next.is_empty() {
            break;
        }
        level = next;
    }
    found
}

/// The command line `argv` runs, starting with the name of the program: the
/// script a script runner runs, rather than the runner.
pub(crate) fn program_command(argv: &[String]) -> Option<String> {
    let (first, rest) = argv.split_first()?;
    let name = file_name(first)?;
    let rest_from = |ix: usize| rest.get(ix..).unwrap_or_default().join(" ");
    let joined = |name: String, rest: String| {
        if rest.trim().is_empty() {
            name
        } else {
            format!("{name} {}", rest.trim())
        }
    };
    match name.as_str() {
        "cmd" => {
            let ix = rest
                .iter()
                .position(|arg| arg.eq_ignore_ascii_case("/c") || arg.eq_ignore_ascii_case("/k"))?;
            // What follows `/c` is one command line, often in one argument.
            let line = rest_from(ix + 1);
            let line = line.trim().trim_start_matches('"');
            let quoted = rest_from(ix + 1).trim().starts_with('"');
            let end = if quoted {
                line.find('"')
            } else {
                line.find(' ')
            };
            let (program, args) = match end {
                Some(end) => (&line[..end], line[end..].trim_start_matches('"')),
                None => (line, ""),
            };
            Some(joined(file_name(program)?, args.to_string()))
        }
        "powershell" | "pwsh" => {
            let ix = rest.iter().position(|arg| {
                ["-file", "-f", "-command", "-c"].contains(&arg.to_ascii_lowercase().as_str())
            });
            match ix {
                Some(ix) => {
                    let script = rest.get(ix + 1)?;
                    let script = script.split_whitespace().next().unwrap_or(script);
                    Some(joined(file_name(script)?, rest_from(ix + 2)))
                }
                None => Some(name),
            }
        }
        "node" | "bun" | "deno" | "python" | "python3" => {
            let ix = rest.iter().position(|arg| !arg.starts_with('-'));
            match ix {
                Some(ix) => Some(joined(script_name(&rest[ix])?, rest_from(ix + 1))),
                None => Some(name),
            }
        }
        _ => Some(joined(name, rest_from(0))),
    }
}

/// The lowercase name of the program at `path`, without its extension.
fn file_name(path: &str) -> Option<String> {
    let path = path.trim().trim_matches('"');
    let stem = Path::new(path).file_stem()?.to_string_lossy().to_lowercase();
    (!stem.is_empty()).then_some(stem)
}

/// The name of the program a script runner runs from `script`: its own name,
/// or for a package's `index.js` or `cli.js`, the package's (`claude-code`'s
/// `cli.js` is `claude`).
fn script_name(script: &str) -> Option<String> {
    let name = file_name(script)?;
    if !matches!(name.as_str(), "index" | "cli" | "main") {
        return Some(name);
    }
    let path = Path::new(script.trim_matches('"'));
    let package = path
        .ancestors()
        .skip(1)
        .filter_map(|ancestor| ancestor.file_name())
        .map(|name| name.to_string_lossy().to_lowercase())
        .find(|name| !matches!(name.as_str(), "bin" | "dist" | "lib" | "src" | "build"))?;
    let package = package
        .strip_suffix("-cli")
        .or_else(|| package.strip_suffix("-code"))
        .unwrap_or(&package)
        .to_string();
    Some(package)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(args: &[&str]) -> Vec<String> {
        args.iter().map(|arg| arg.to_string()).collect()
    }

    #[test]
    fn test_program_command() {
        let command = |args: &[&str]| program_command(&argv(args));
        assert_eq!(
            command(&[r"C:\Users\a\.local\bin\claude.exe", "--settings", "x.json"]),
            Some("claude --settings x.json".to_string())
        );
        assert_eq!(
            command(&[
                r"C:\Program Files\nodejs\node.exe",
                r"C:\Users\a\AppData\Roaming\npm\node_modules\@anthropic-ai\claude-code\cli.js",
                "--resume"
            ]),
            Some("claude --resume".to_string())
        );
        assert_eq!(
            command(&[
                "node",
                r"C:\Users\a\AppData\Roaming\npm\node_modules\@openai\codex\bin\codex.js",
            ]),
            Some("codex".to_string())
        );
        assert_eq!(
            command(&[
                r"C:\WINDOWS\system32\cmd.exe",
                "/d",
                "/c",
                r#""E:\fakebin\claude.cmd" --settings x.json"#
            ]),
            Some("claude --settings x.json".to_string())
        );
        assert_eq!(
            command(&["pwsh", "-NoProfile", "-File", r"C:\npm\codex.ps1", "exec"]),
            Some("codex exec".to_string())
        );
        assert_eq!(command(&["powershell.exe"]), Some("powershell".to_string()));
        assert_eq!(command(&[]), None);
    }

    #[test]
    fn test_descendants() {
        let processes = vec![
            (10, 1, argv(&["pwsh.exe", "-NoLogo"])),
            (11, 10, argv(&["conhost.exe", "0x4"])),
            (12, 10, argv(&["cmd.exe", "/c", r#""C:\bin\claude.cmd" -c"#])),
            (13, 12, argv(&["powershell", "-File", r"C:\bin\fake.ps1", "claude"])),
            (14, 13, argv(&["ping", "127.0.0.1"])),
            (15, 14, argv(&["too-deep"])),
            (20, 2, argv(&["unrelated"])),
        ];
        let commands: Vec<String> = descendants(10, &processes)
            .into_iter()
            .map(|program| program.command)
            .collect();
        assert_eq!(commands, ["claude -c", "fake claude", "ping 127.0.0.1"]);
    }
}
