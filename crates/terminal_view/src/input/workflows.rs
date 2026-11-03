//! Workflows: saved commands, like Warp's. They're listed with
//! ctrl-shift-r and fill the input; `{{argument}}` placeholders are then
//! selected one after another with Tab.
//!
//! The user's workflows live in `workflows.json` in Solo's config directory,
//! next to a few built-in ones for the shell in use.

use std::ops::Range;
use std::path::PathBuf;

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};

use super::completion::ShellFlavor;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Workflow {
    pub name: String,
    pub command: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

pub fn workflows_file() -> PathBuf {
    paths::config_dir().join("workflows.json")
}

/// The user's workflows, then the built-in ones for `flavor`.
pub fn load(flavor: ShellFlavor) -> Vec<Workflow> {
    let mut workflows = match load_user_workflows() {
        Ok(workflows) => workflows,
        Err(error) => {
            log::warn!("failed to read workflows: {error:#}");
            Vec::new()
        }
    };
    workflows.extend(built_in(flavor));
    workflows
}

fn load_user_workflows() -> Result<Vec<Workflow>> {
    let path = workflows_file();
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error).context(format!("reading {}", path.display())),
    };
    if text.trim().is_empty() {
        return Ok(Vec::new());
    }
    serde_json_lenient::from_str(&text).context(format!("parsing {}", path.display()))
}

/// Adds `command` to the user's workflows, unless it's there already.
pub fn save(command: &str) -> Result<()> {
    let mut workflows = load_user_workflows()?;
    if workflows.iter().any(|workflow| workflow.command == command) {
        return Ok(());
    }
    let workflow = Workflow {
        name: command.lines().next().unwrap_or_default().to_string(),
        command: command.to_string(),
        description: None,
    };
    let text = std::fs::read_to_string(workflows_file()).unwrap_or_default();
    workflows.push(workflow.clone());
    match append_to_file_text(&text, &workflow) {
        // Only if the edit reads back as intended, as comments could hide
        // brackets.
        Some(new_text)
            if serde_json_lenient::from_str::<Vec<Workflow>>(&new_text).ok()
                == Some(workflows.clone()) =>
        {
            std::fs::write(workflows_file(), new_text)
                .context(format!("writing {}", workflows_file().display()))
        }
        _ => write(&workflows),
    }
}

/// The workflows file's text with `workflow` added before the array's closing
/// bracket, keeping the user's comments and formatting.
fn append_to_file_text(text: &str, workflow: &Workflow) -> Option<String> {
    let close = text.rfind(']')?;
    let before = text[..close].trim_end();
    let separator = if before.ends_with('[') || before.ends_with(',') {
        ""
    } else {
        ","
    };
    let entry = serde_json::to_string_pretty(workflow)
        .ok()?
        .replace('\n', "\n  ");
    Some(format!(
        "{before}{separator}\n  {entry}\n{}",
        &text[close..]
    ))
}

/// Creates the workflows file with an example if there is none, so it can be
/// opened for editing.
pub fn ensure_file() -> Result<PathBuf> {
    let path = workflows_file();
    if !path.exists() {
        write(&[Workflow {
            name: util::i18n::tr(
                "Example: search the code for a word",
                "示例：在代码中搜索一个词",
            )
            .to_string(),
            command: "git grep -n {{word}}".to_string(),
            description: Some(
                util::i18n::tr(
                    "Workflows fill the terminal input; Tab selects each {{argument}}.",
                    "Workflow 会填入终端输入框；按 Tab 依次选中每个 {{argument}}。",
                )
                .to_string(),
            ),
        }])?;
    }
    Ok(path)
}

fn write(workflows: &[Workflow]) -> Result<()> {
    let path = workflows_file();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let text = serde_json::to_string_pretty(workflows)?;
    std::fs::write(&path, text + "\n").context(format!("writing {}", path.display()))
}

/// Byte range of the first `{{argument}}` placeholder at or after `from`,
/// wrapping around to the start.
pub fn next_placeholder(text: &str, from: usize) -> Option<Range<usize>> {
    let placeholders = placeholders(text);
    placeholders
        .iter()
        .find(|range| range.start >= from)
        .or_else(|| placeholders.first())
        .cloned()
}

fn placeholders(text: &str) -> Vec<Range<usize>> {
    let mut ranges = Vec::new();
    let mut offset = 0;
    while let Some(start) = text[offset..].find("{{").map(|ix| ix + offset) {
        let Some(len) = text[start + 2..].find("}}") else {
            break;
        };
        let end = start + 2 + len + 2;
        let name = &text[start + 2..end - 2];
        if !name.trim().is_empty() && !name.contains(['{', '}', '\n']) {
            ranges.push(start..end);
            offset = end;
        } else {
            offset = start + 2;
        }
    }
    ranges
}

fn built_in(flavor: ShellFlavor) -> Vec<Workflow> {
    use util::i18n::tr;
    let mut workflows = vec![
        (
            tr(
                "Undo the last git commit, keeping its changes",
                "撤销上一次 git 提交，保留改动",
            ),
            "git reset --soft HEAD~1",
            "git",
        ),
        (
            tr(
                "Create a git branch and switch to it",
                "新建 git 分支并切换过去",
            ),
            "git switch -c {{branch}}",
            "git",
        ),
        (
            tr(
                "Show what changed in the last git commit",
                "查看上一次 git 提交改了什么",
            ),
            "git show --stat HEAD",
            "git",
        ),
        (
            tr(
                "Delete local git branches already merged",
                "删除已合并的本地 git 分支",
            ),
            "git branch --merged | grep -v '^\\*' | xargs -r git branch -d",
            "git",
        ),
        (
            tr(
                "Ask Claude Code about this directory",
                "向 Claude Code 询问这个目录",
            ),
            "claude \"{{question}}\"",
            "AI",
        ),
    ];
    let find_process = tr(
        "Find the process listening on a port",
        "查找监听某端口的进程",
    );
    let find_files = tr("Find files by name", "按名称查找文件");
    let search_contents = tr("Search file contents", "搜索文件内容");
    let largest_files = tr("Largest files in this directory", "此目录中最大的文件");
    let set_variable = tr(
        "Set an environment variable for this session",
        "为当前会话设置环境变量",
    );
    workflows.extend(match flavor {
        ShellFlavor::PowerShell => [
            (
                find_process,
                "Get-Process -Id (Get-NetTCPConnection -LocalPort {{port}}).OwningProcess",
                "PowerShell",
            ),
            (
                find_files,
                "Get-ChildItem -Recurse -Filter '{{pattern}}' | Select-Object FullName",
                "PowerShell",
            ),
            (
                search_contents,
                "Select-String -Path {{files}} -Pattern '{{text}}'",
                "PowerShell",
            ),
            (
                largest_files,
                "Get-ChildItem -Recurse -File | Sort-Object Length -Descending | Select-Object -First 10 FullName, Length",
                "PowerShell",
            ),
            (set_variable, "$env:{{name}} = '{{value}}'", "PowerShell"),
        ],
        ShellFlavor::Posix => [
            (find_process, "lsof -i :{{port}}", "shell"),
            (find_files, "find . -name '{{pattern}}'", "shell"),
            (search_contents, "grep -rn '{{text}}' .", "shell"),
            (largest_files, "du -ah . | sort -rh | head -n 10", "shell"),
            (set_variable, "export {{name}}={{value}}", "shell"),
        ],
    });
    let built_in = tr("Built-in", "内置");
    workflows
        .into_iter()
        .map(|(name, command, tag)| Workflow {
            name: name.to_string(),
            command: command.to_string(),
            description: Some(format!("{built_in} · {tag}")),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_placeholders_in_order_and_wraps() {
        let text = "git commit -m {{message}} --author {{who}}";
        let first = next_placeholder(text, 0).unwrap();
        assert_eq!(&text[first.clone()], "{{message}}");
        let second = next_placeholder(text, first.end).unwrap();
        assert_eq!(&text[second.clone()], "{{who}}");
        assert_eq!(next_placeholder(text, second.end), Some(first));
        assert_eq!(next_placeholder("echo {{}} {{ }}", 0), None);
        assert_eq!(next_placeholder("echo ${x}", 0), None);
    }

    #[test]
    fn appends_keeping_comments() {
        let workflow = Workflow {
            name: "ls".into(),
            command: "ls".into(),
            description: None,
        };
        let text = "// mine\n[\n  { \"name\": \"a\", \"command\": \"a\" }, // first\n]\n";
        let new_text = append_to_file_text(text, &workflow).unwrap();
        assert!(
            new_text.starts_with("// mine\n[\n  { \"name\": \"a\", \"command\": \"a\" }, // first")
        );
        let workflows: Vec<Workflow> = serde_json_lenient::from_str(&new_text).unwrap();
        assert_eq!(workflows.len(), 2);
        assert_eq!(workflows[1], workflow);

        let new_text = append_to_file_text("[]", &workflow).unwrap();
        let workflows: Vec<Workflow> = serde_json_lenient::from_str(&new_text).unwrap();
        assert_eq!(workflows, [workflow]);
    }

    #[test]
    fn built_ins_exist_for_each_shell() {
        for flavor in [ShellFlavor::PowerShell, ShellFlavor::Posix] {
            let workflows = built_in(flavor);
            assert!(workflows.len() >= 8);
            assert!(workflows.iter().all(|workflow| !workflow.name.is_empty()));
        }
    }
}
