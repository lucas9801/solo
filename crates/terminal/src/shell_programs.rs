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
    /// When it started, in seconds since the epoch: a pid is reused once
    /// its process ended.
    pub start_time: u64,
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
    let processes: Vec<Process> = system
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
            Some(Process {
                pid: process.pid().as_u32(),
                parent,
                start_time: process.start_time(),
                argv,
            })
        })
        .collect();
    descendants(shell_pid, &processes)
}

/// A process, as [`descendants`] takes it.
struct Process {
    pid: u32,
    parent: u32,
    start_time: u64,
    argv: Vec<String>,
}

#[cfg(not(windows))]
pub fn shell_programs(_shell_pid: u32) -> Vec<ShellProgram> {
    Vec::new()
}

/// Whether `program` still runs: a process with its pid that started when
/// it did (not a later one given its pid).
#[cfg(windows)]
pub fn is_running(program: &ShellProgram) -> bool {
    use windows::Win32::Foundation::{CloseHandle, FILETIME, WAIT_TIMEOUT};
    use windows::Win32::System::Threading::{
        GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
        WaitForSingleObject,
    };
    unsafe {
        let Ok(process) = OpenProcess(
            PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION,
            false,
            program.pid,
        ) else {
            return false;
        };
        let running = WaitForSingleObject(process, 0) == WAIT_TIMEOUT;
        let mut times = [FILETIME::default(); 4];
        let [creation, exit, kernel, user] = &mut times;
        let started = GetProcessTimes(process, creation, exit, kernel, user)
            .is_ok()
            .then(|| {
                let ticks = (u64::from(times[0].dwHighDateTime) << 32)
                    | u64::from(times[0].dwLowDateTime);
                // 100 ns ticks since 1601 to seconds since 1970.
                (ticks / 10_000_000).saturating_sub(11_644_473_600)
            });
        CloseHandle(process).ok();
        running && started.is_none_or(|started| started.abs_diff(program.start_time) <= 1)
    }
}

#[cfg(not(windows))]
pub fn is_running(_program: &ShellProgram) -> bool {
    false
}

/// Of `processes`, those below `root`, nearest first. A process whose
/// parent's pid is that of a process started after it isn't that one's
/// child: its parent ended, and the pid went to the later one.
fn descendants(root: u32, processes: &[Process]) -> Vec<ShellProgram> {
    let started = |pid: u32| {
        processes
            .iter()
            .find(|process| process.pid == pid)
            .map(|process| process.start_time)
    };
    let mut found = Vec::new();
    let mut level = vec![(root, started(root).unwrap_or(0))];
    for _ in 0..MAX_DEPTH {
        let mut next = Vec::new();
        for process in processes {
            let Some((_, parent_started)) =
                level.iter().find(|(pid, _)| *pid == process.parent)
            else {
                continue;
            };
            if process.pid == process.parent
                || process.pid == root
                || process.start_time < *parent_started
            {
                continue;
            }
            let Some(command) = program_command(&process.argv) else {
                continue;
            };
            next.push((process.pid, process.start_time));
            let name = command.split(' ').next().unwrap_or_default();
            if CONSOLE_PROGRAMS.contains(&name) {
                continue;
            }
            found.push(ShellProgram {
                pid: process.pid,
                start_time: process.start_time,
                command,
                command_line: process.argv.join(" "),
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

    fn process(pid: u32, parent: u32, start_time: u64, args: &[&str]) -> Process {
        Process {
            pid,
            parent,
            start_time,
            argv: argv(args),
        }
    }

    #[test]
    fn test_descendants() {
        let processes = vec![
            process(10, 1, 100, &["pwsh.exe", "-NoLogo"]),
            process(11, 10, 100, &["conhost.exe", "0x4"]),
            process(12, 10, 101, &["cmd.exe", "/c", r#""C:\bin\claude.cmd" -c"#]),
            process(13, 12, 102, &["powershell", "-File", r"C:\bin\fake.ps1", "claude"]),
            process(14, 13, 103, &["ping", "127.0.0.1"]),
            process(15, 14, 104, &["too-deep"]),
            process(20, 2, 50, &["unrelated"]),
            // Started before the shell: its parent was an earlier process
            // with the shell's pid.
            process(30, 10, 90, &["orphan"]),
        ];
        let commands: Vec<String> = descendants(10, &processes)
            .into_iter()
            .map(|program| program.command)
            .collect();
        assert_eq!(commands, ["claude -c", "fake claude", "ping 127.0.0.1"]);
    }
}
