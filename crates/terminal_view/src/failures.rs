//! SOLO: what failed in a command's output, such as a build's errors or a
//! test run's failed tests, to send to the agent working on the code.

/// Lines kept at most, so a long failing run stays a readable message.
const MAX_LINES: usize = 80;
/// Lines kept after a line saying what failed, for where and why.
const FOLLOWING_LINES: usize = 6;
/// Lines of the output's end kept when nothing in it looks like a failure.
const TAIL_LINES: usize = 30;

/// Whether `line` starts the report of an error or a failed test, in the
/// formats of common compilers and test runners.
fn starts_failure(line: &str) -> bool {
    let trimmed = line.trim_start();
    let lower = trimmed.to_lowercase();
    // rustc and cargo: `error[E0308]: ...`, `error: ...`.
    (trimmed.starts_with("error[") || trimmed.starts_with("error:"))
        // gcc, clang, tsc, MSVC: `path:3:5: error: ...`, `path(3,5): error TS2322: ...`.
        || lower.contains(": error:")
        || lower.contains(": fatal error:")
        || contains_coded_error(&lower)
        // A Rust test's output and its panic.
        || (trimmed.starts_with("---- ") && trimmed.ends_with(" stdout ----"))
        || trimmed.contains("panicked at")
        // libtest, pytest, go test, jest and vitest.
        || (trimmed.starts_with("test ") && trimmed.ends_with("... FAILED"))
        || trimmed.starts_with("FAILED ")
        || trimmed.starts_with("FAIL ")
        || trimmed.starts_with("--- FAIL:")
        || trimmed.starts_with("● ")
        || trimmed.starts_with("E   ")
        || trimmed.starts_with("Traceback (most recent call last)")
        || trimmed.starts_with("AssertionError")
}

/// `error TS2322:`, `error C2065:`, `error CS0103:`: an error with a code.
fn contains_coded_error(lower: &str) -> bool {
    lower.match_indices("error ").any(|(ix, _)| {
        let code = &lower[ix + "error ".len()..];
        let letters = code.chars().take_while(|c| c.is_ascii_alphabetic()).count();
        let digits = code[letters..]
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .count();
        letters > 0 && digits > 0 && code[letters + digits..].starts_with(':')
    })
}

/// The lines of `output` saying what failed: each error or failed test with
/// the lines after it, up to a blank line. When none look like one, the end
/// of the output.
pub(crate) fn failure_lines(output: &str) -> Vec<&str> {
    let lines: Vec<&str> = output.lines().collect();
    let mut kept: Vec<usize> = Vec::new();
    let mut ix = 0;
    while ix < lines.len() && kept.len() < MAX_LINES {
        if !starts_failure(lines[ix]) {
            ix += 1;
            continue;
        }
        kept.push(ix);
        let mut following = 0;
        ix += 1;
        while ix < lines.len()
            && following < FOLLOWING_LINES
            && !lines[ix].trim().is_empty()
            && !starts_failure(lines[ix])
        {
            kept.push(ix);
            following += 1;
            ix += 1;
        }
    }
    if kept.is_empty() {
        let lines: Vec<&str> = lines
            .into_iter()
            .rev()
            .skip_while(|line| line.trim().is_empty())
            .take(TAIL_LINES)
            .collect();
        return lines.into_iter().rev().collect();
    }
    kept.truncate(MAX_LINES);
    kept.into_iter().map(|ix| lines[ix]).collect()
}

/// The message telling an agent what failed when `command` ran.
pub(crate) fn failure_message(command: &str, output: &str, exit_code: Option<i32>) -> String {
    let lines = failure_lines(output);
    let status = match exit_code {
        Some(code) => format!("failed with exit code {code}"),
        None => "failed".to_string(),
    };
    let body = lines.join("\n");
    let fence = "`".repeat(
        body.split(|c| c != '`')
            .map(str::len)
            .max()
            .unwrap_or(0)
            .max(2)
            + 1,
    );
    let mut message = format!("`{}` {status}. Please fix it:\n", command.trim());
    if !body.is_empty() {
        message.push_str(&format!("{fence}\n{body}\n{fence}\n"));
    }
    message
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rust_build_errors() {
        let output = "   Compiling app v0.1.0\n\
            error[E0308]: mismatched types\n \
            --> src/main.rs:3:18\n  \
            |\n\
            3 |     let x: u32 = \"a\";\n  \
            |            ---   ^^^ expected `u32`, found `&str`\n\
            \n\
            For more information about this error, try `rustc --explain E0308`.\n\
            error: could not compile `app` (bin \"app\") due to 1 previous error\n";
        assert_eq!(
            failure_lines(output),
            vec![
                "error[E0308]: mismatched types",
                " --> src/main.rs:3:18",
                "  |",
                "3 |     let x: u32 = \"a\";",
                "  |            ---   ^^^ expected `u32`, found `&str`",
                "error: could not compile `app` (bin \"app\") due to 1 previous error",
            ]
        );
    }

    #[test]
    fn test_rust_test_failures() {
        let output = "running 2 tests\n\
            test tests::one ... ok\n\
            test tests::two ... FAILED\n\
            \n\
            failures:\n\
            \n\
            ---- tests::two stdout ----\n\
            \n\
            thread 'tests::two' panicked at src/lib.rs:10:9:\n\
            assertion `left == right` failed\n  \
            left: 1\n \
            right: 2\n\
            \n\
            test result: FAILED. 1 passed; 1 failed\n";
        assert_eq!(
            failure_lines(output),
            vec![
                "test tests::two ... FAILED",
                "---- tests::two stdout ----",
                "thread 'tests::two' panicked at src/lib.rs:10:9:",
                "assertion `left == right` failed",
                "  left: 1",
                " right: 2",
            ]
        );
    }

    #[test]
    fn test_other_tools() {
        // tsc, gcc, MSVC, pytest, go.
        for line in [
            "src/app.ts(4,7): error TS2322: Type 'string' is not assignable to type 'number'.",
            "main.c:3:5: error: unknown type name 'foo'",
            r"C:\src\main.cpp(12): error C2065: 'x': undeclared identifier",
            "FAILED tests/test_app.py::test_add - assert 1 == 2",
            "--- FAIL: TestAdd (0.00s)",
        ] {
            assert_eq!(
                failure_lines(&format!("noise\n{line}\nmore noise")),
                vec![line, "more noise"]
            );
        }
        // Not failures.
        assert!(!starts_failure("warning: unused variable `x`"));
        assert!(!starts_failure("0 errors found"));
        assert!(!starts_failure("let error = 1;"));
    }

    #[test]
    fn test_nothing_recognized_keeps_the_end() {
        let output: String = (1..=50).map(|n| format!("line {n}\n")).collect();
        let output = format!("{output}\n\n");
        let lines = failure_lines(&output);
        assert_eq!(lines.len(), TAIL_LINES);
        assert_eq!(lines.first(), Some(&"line 21"));
        assert_eq!(lines.last(), Some(&"line 50"));
    }

    #[test]
    fn test_failure_message() {
        assert_eq!(
            failure_message("cargo build ", "error: oops\n", Some(101)),
            "`cargo build` failed with exit code 101. Please fix it:\n```\nerror: oops\n```\n"
        );
        // A fence the output can't end.
        let message = failure_message("make", "error: ``` here\n", Some(2));
        assert!(message.contains("````\nerror: ``` here\n````"));
    }
}
