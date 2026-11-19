# Performance: Solo, Zed and VS Code

[← README](../README.md)

Memory, CPU and startup time of Solo, Zed and VS Code on one Windows
machine, measured with
[`script/measure-editor-performance.ps1`](../script/measure-editor-performance.ps1).
These are the numbers of one machine on one day, not a benchmark suite: read
[Limitations](#limitations) before quoting one.

## Summary

Measured 2026-11-18, median of 3 runs. Private memory is the sum over all
of an editor's processes; "% of a core" is CPU time as a share of one
logical processor.

- **Solo's own process takes about what Zed's does.** Solo's main process
  used 113-122 MB with an empty window or a project open, Zed's 124-133 MB;
  after editing 20 files 406 MB against 418 MB. Solo's totals are higher by
  the terminal it opens with every window: a terminal host, a PowerShell
  and a second PowerShell for its input box's completions, about 78 MB
  together. Empty window: Solo 194 MB, Zed 129 MB, VS Code 1,169 MB. Project
  open: Solo 200 MB, Zed 138 MB, VS Code 1,474 MB.
- **VS Code takes 6-10 times as much with little open, but less after big
  searches and large files.** After three project-wide searches Solo held
  2.4 GB and Zed 2.5 GB, VS Code 1.5 GB: Solo and Zed keep every file with a
  match open in the search results. After opening the 30 MB log, Solo
  2.8 GB, Zed 3.0 GB, VS Code 1.7 GB (VS Code turns its features off for a
  file that large).
- **Ten terminals printing like agents cost Solo and Zed about 3.6 cores,
  VS Code 0.6.** Solo 364 %, Zed 362 %, VS Code 59 % of a core (language
  servers off). Solo inherited this from Zed; it was the clearest place where
  both were worse than VS Code. Since fixed in Solo: about 0.3 cores (see
  [Terminals that print](#terminals-that-print)). Solo's 10 terminals take 861 MB (Zed 674 MB,
  VS Code 956 MB); the shells themselves are about 650 MB of that in all
  three.
- **Idle, all three use about 1 % of a core.** With 10 idle terminals: Solo
  1.30 %, Zed 1.26 %, VS Code 0.90 %. With language servers running, Solo
  uses about 0.8 % of a core more than Zed (2.25 % against 1.32 %); see
  [Idle CPU](#idle-cpu), which also has how much of that a later change
  saved.
- **Startup:** with a fresh data folder, the window appears after 0.6-1.1 s
  in all three. Zed's picture is final at once (1.1 s); VS Code's after
  3.6 s; Solo's after 2.6 s, because it keeps changing until the shell in
  its terminal shows a prompt. Warm: Zed 0.7 s, Solo 2.1 s, VS Code 2.4 s.
- **The machine was busy** with other work during most runs (see
  [Limitations](#limitations)); the CPU and time numbers carry that noise.

## Results

### Memory, language servers off (MB, median)

| Scenario | Solo private | Zed private | VS Code private | Solo working set | Zed working set | VS Code working set |
|---|--:|--:|--:|--:|--:|--:|
| a. Empty window | 194 | 129 | 1,169 | 244 | 161 | 1,394 |
| b. Project open | 200 | 138 | 1,474 | 257 | 175 | 1,638 |
| c. 20 files edited | 480 | 423 | 1,565 | 536 | 460 | 1,720 |
| d. After 3 searches | 2,439 | 2,543 | 1,493 | 2,488 | 2,526 | 1,629 |
| e. 200k-line log | 2,797 | 2,950 | 1,725 | 2,852 | 2,964 | 1,853 |
| f. 10 terminals printing | 3,896 | 3,693 | 2,762 | 4,205 | 3,952 | 3,154 |
| f. 10 terminals idle | 3,876 | 3,695 | 2,545 | 4,141 | 3,957 | 2,939 |
| g. All closed | 396 | 302 | 1,702 | 438 | 333 | 1,837 |

### Memory, language servers on (MB, median)

| Scenario | Solo private | Zed private | VS Code private | Solo working set | Zed working set | VS Code working set |
|---|--:|--:|--:|--:|--:|--:|
| b. Project open | 199 | 138 | 1,547 | 256 | 175 | 1,795 |
| c. 20 files edited | 1,062 | 999 | 1,766 | 1,174 | 1,085 | 2,205 |
| d. After 3 searches | 3,103 | 3,062 | 1,767 | 3,166 | 3,138 | 2,204 |
| e. 200k-line log | 3,391 | 3,513 | 2,078 | 3,473 | 3,586 | 2,508 |
| f. 10 terminals printing | 4,548 | 4,278 | 3,116 | 4,874 | 4,570 | 3,805 |
| f. 10 terminals idle | 4,516 | 4,278 | 2,890 | 4,856 | 4,574 | 3,514 |
| g. All closed | 996 | 900 | 2,101 | 1,089 | 967 | 2,502 |

### Private memory by kind, language servers off (MB, median)

| Scenario | Editor | Main | Terminals | Language servers | Other | Total |
|---|---|--:|--:|--:|--:|--:|
| a. Empty window | Solo | 114 | 80 | 0 | 0 | 194 |
| a. Empty window | Zed | 124 | 0 | 0 | 5 | 129 |
| a. Empty window | VS Code | 519 | 1 | 0 | 650 | 1,169 |
| b. Project open | Solo | 122 | 78 | 0 | 0 | 200 |
| b. Project open | Zed | 133 | 0 | 0 | 5 | 138 |
| b. Project open | VS Code | 779 | 1 | 0 | 694 | 1,474 |
| c. 20 files edited | Solo | 406 | 76 | 0 | 0 | 480 |
| c. 20 files edited | Zed | 418 | 0 | 0 | 5 | 423 |
| c. 20 files edited | VS Code | 967 | 1 | 0 | 600 | 1,565 |
| d. After 3 searches | Solo | 2,372 | 77 | 0 | 0 | 2,439 |
| d. After 3 searches | Zed | 2,524 | 0 | 0 | 5 | 2,543 |
| d. After 3 searches | VS Code | 893 | 1 | 0 | 617 | 1,493 |
| e. 200k-line log | Solo | 2,720 | 76 | 0 | 0 | 2,797 |
| e. 200k-line log | Zed | 2,945 | 0 | 0 | 5 | 2,950 |
| e. 200k-line log | VS Code | 1,062 | 1 | 0 | 662 | 1,725 |
| f. 10 terminals printing | Solo | 3,022 | 861 | 0 | 3 | 3,896 |
| f. 10 terminals printing | Zed | 3,013 | 674 | 0 | 5 | 3,693 |
| f. 10 terminals printing | VS Code | 1,154 | 956 | 0 | 662 | 2,762 |
| f. 10 terminals idle | Solo | 3,027 | 849 | 0 | 0 | 3,876 |
| f. 10 terminals idle | Zed | 3,037 | 653 | 0 | 5 | 3,695 |
| f. 10 terminals idle | VS Code | 942 | 941 | 0 | 662 | 2,545 |
| g. All closed | Solo | 323 | 74 | 0 | 0 | 396 |
| g. All closed | Zed | 298 | 0 | 0 | 5 | 302 |
| g. All closed | VS Code | 823 | 1 | 0 | 872 | 1,702 |

### Private memory by kind, language servers on (MB, median)

| Scenario | Editor | Main | Terminals | Language servers | Other | Total |
|---|---|--:|--:|--:|--:|--:|
| b. Project open | Solo | 122 | 77 | 0 | 0 | 199 |
| b. Project open | Zed | 134 | 0 | 0 | 5 | 138 |
| b. Project open | VS Code | 817 | 1 | 26 | 725 | 1,547 |
| c. 20 files edited | Solo | 473 | 76 | 512 | 0 | 1,062 |
| c. 20 files edited | Zed | 486 | 0 | 508 | 5 | 999 |
| c. 20 files edited | VS Code | 737 | 1 | 367 | 658 | 1,766 |
| d. After 3 searches | Solo | 2,490 | 81 | 519 | 6 | 3,103 |
| d. After 3 searches | Zed | 2,537 | 1 | 516 | 6 | 3,062 |
| d. After 3 searches | VS Code | 841 | 1 | 273 | 671 | 1,767 |
| e. 200k-line log | Solo | 2,786 | 76 | 526 | 0 | 3,391 |
| e. 200k-line log | Zed | 2,981 | 0 | 526 | 5 | 3,513 |
| e. 200k-line log | VS Code | 1,061 | 1 | 275 | 753 | 2,078 |
| f. 10 terminals printing | Solo | 3,135 | 839 | 526 | 0 | 4,548 |
| f. 10 terminals printing | Zed | 3,071 | 674 | 526 | 5 | 4,278 |
| f. 10 terminals printing | VS Code | 1,117 | 1,004 | 275 | 692 | 3,116 |
| f. 10 terminals idle | Solo | 3,143 | 829 | 526 | 0 | 4,516 |
| f. 10 terminals idle | Zed | 3,093 | 650 | 525 | 5 | 4,278 |
| f. 10 terminals idle | VS Code | 938 | 954 | 275 | 687 | 2,890 |
| g. All closed | Solo | 391 | 74 | 528 | 0 | 996 |
| g. All closed | Zed | 366 | 0 | 525 | 5 | 900 |
| g. All closed | VS Code | 943 | 1 | 275 | 890 | 2,101 |

### CPU, language servers off (% of one logical processor, median)

"Other programs" is what the rest of the machine used during the same
window, from `GetSystemTimes`: 1,200 means all 12 logical processors were
busy.

| Scenario | Window | Solo | Zed | VS Code | Other programs meanwhile (Solo / Zed / VS Code) |
|---|--:|--:|--:|--:|--:|
| a. Empty window | 300 s | 1.09 | 1.22 | 1.28 | 518 / 592 / 503 |
| b. Project open | 300 s | 1.10 | 0.97 | 1.44 | 1,199 / 1,199 / 1,199 |
| c. 20 files edited | 60 s | 1.22 | 1.09 | 2.45 | 1,199 / 1,199 / 1,197 |
| d. After 3 searches | 60 s | 13.27 | 9.90 | 1.86 | 1,184 / 1,190 / 1,199 |
| e. 200k-line log | 60 s | 1.58 | 1.09 | 3.12 | 1,192 / 1,199 / 1,197 |
| f. 10 terminals printing | 60 s | 363.64 | 362.29 | 58.89 | 841 / 838 / 1,143 |
| f. 10 terminals idle | 300 s | 1.30 | 1.26 | 0.90 | 1,199 / 1,199 / 1,199 |
| g. All closed | 60 s | 1.18 | 1.06 | 1.52 | 1,199 / 1,198 / 1,199 |

### CPU, language servers on (% of one logical processor, median)

| Scenario | Window | Solo | Zed | VS Code | Other programs meanwhile (Solo / Zed / VS Code) |
|---|--:|--:|--:|--:|--:|
| b. Project open | 60 s | 1.11 | 0.96 | 2.92 | 1,200 / 1,199 / 1,197 |
| c. 20 files edited | 60 s | 2.23 | 1.52 | 2.28 | 1,198 / 1,199 / 1,197 |
| d. After 3 searches | 60 s | 13.17 | 15.20 | 1.52 | 1,187 / 1,185 / 1,198 |
| e. 200k-line log | 60 s | 11.56 | 1.09 | 3.53 | 1,188 / 1,199 / 1,195 |
| f. 10 terminals printing | 60 s | 353.69 | 409.62 | 76.15 | 847 / 791 / 1,124 |
| f. 10 terminals idle | 60 s | 2.25 | 1.32 | 0.86 | 1,198 / 1,199 / 1,199 |
| g. All closed | 60 s | 2.01 | 1.53 | 1.66 | 1,198 / 1,199 / 1,198 |

### Startup (s, median)

| | Solo | Zed | VS Code |
|---|--:|--:|--:|
| First launch, window shown | 0.57 | 1.05 | 0.79 |
| First launch, ready | 2.55 | 1.09 | 3.62 |
| Warm launch, window shown | 0.45 | 0.54 | 0.33 |
| Warm launch, ready | 2.10 | 0.67 | 2.40 |

### Search

The three project-wide searches took Solo and Zed minutes, VS Code a few
seconds. Measured as the time until the editor's CPU use fell below 3 % of
a core, capped at 120 s per search: Solo and Zed reached the cap in most
runs (sums of 277 s and 361 s with language servers off, 362 s and 361 s
with them on), VS Code took 7-9 s for all three. The machine's processors
were busy with other work, so the absolute times are high, but the gap
held in every run. Afterwards Solo and Zed held 2.4-3.1 GB (they open every
file with a match), and in most runs still used 10-17 % of a core in the
next minute.

**What the time went to** (2026-11-20, a build that records the time of
every task by where it was spawned; a release build searching a copy of
Godot's repository, 14,000 files, for `memnew(`: 731 files, 7,112 matches;
language servers off). The search itself (reading the files in parallel,
skipping binary files after their first 8 KB) took a small part. Most of
the time went to what happens to each file with a match: it's opened as a
buffer (as Zed does), parsed with tree-sitter (about 26 ms of CPU per file,
two parses per file), its git diff bases loaded, and blamed:

- **Git blame for every result.** Inline blame (on by default) blamed
  every buffer of the results, running `git blame` for each matching file,
  and again for all of them each time more results came in (every 2 s of
  streaming), although it only ever shows the line at the cursor. Fixed:
  in a multibuffer, inline blame now blames a buffer only when it's asked
  to show a line of it. Blame the user turns on (the blame gutter) still
  covers every buffer.
- **Parsing every result**, the git diff of every result, and redrawing
  the results as they stream in remain; they are how Zed's project search
  works (every result is an editor buffer), and changing that would be a
  large divergence. The parsing also makes Windows' thread pool grow to
  350-390 threads during a search (the tasks run long, so it adds threads),
  which adds memory and context switches.

The same searches before and after (release builds, 2 runs each,
alternating; "busy" is until Solo's process uses less than 3 % of a core
for 2 s, and doesn't include the `git` processes the blame started; the
machine was also compiling, 2-7 of its 12 logical processors busy):

| Query | Busy before | Busy after | CPU before | CPU after |
|---|--:|--:|--:|--:|
| `memnew(` (731 files) | 68-81 s | 48-52 s | 63-66 s | 51 s |
| `get_singleton()` (1,287 files by `git grep`) | 51-55 s | 24-27 s | 31 s | 20 s |

Memory after the search didn't change (2.1 GB and 1.5 GB).

### Terminals that print

While the 10 terminals printed, nearly all of Solo's CPU was in its own
process, not in the terminal hosts or shells (those together: about 7 % of
a core). Sampling the threads of Solo's process for 20 s (`-ThreadProfile`,
thread CPU times through `GetThreadTimes`): the main thread used 28-46 % of
a core, and 13-14 thread-pool workers (the background executor, which
Windows runs on its thread pool) up to 45 % each. Zed's process showed the
same picture (main thread 43-55 %, 10-12 busy workers). The numbers vary
between runs (Solo 161-455 % in the 20 s thread samples) because the
machine was busy.

**Found and fixed since** (2026-11-20). A build that records the time of
every task by where it was spawned showed where the workers' time went:
about 95 % of it was one task, the one that reads the terminal's process
again after output to update its title and folder
(`crates/terminal/src/pty_info.rs`, code from Zed). It ran after every
batch of output, up to 250 times a second per terminal, and on Windows
each run takes a snapshot of every process on the machine (12-24 ms of
CPU), so a terminal that kept printing kept a worker busy. Solo's own
per-output work (agent status, command blocks, tab titles, the terminal's
saved contents) came to about 0.2 % of a core in all. Two changes:

- The process is read again at most every 0.5 s after input went to the
  terminal or after a reading found a change (what usually changes the
  program or its folder), and every 5 s while a program prints on its own.
- A terminal in a background tab redraws the window once for its output,
  not after every batch: it isn't drawn, and its tab title is updated
  separately.

The same 10 terminals and script, measured the same way (release builds
of `main` and of the change, 60 s each, 3 runs each, alternating):

| % of a core (median of 3) | Before | After |
|---|--:|--:|
| Solo's main thread | 30.0 | 16.7 |
| Solo's other threads | 272.7 | 4.8 |
| Solo's process in all | 302.7 | 22.0 |
| Terminal hosts and shells | 5.3 | 7.5 |

That is about 0.3 of a core for Solo and its terminals, against VS Code's
0.6 above. The other programs on the machine used 1.2-4.6 cores during
the runs. The visible terminal's output looked the same.

### Idle CPU

Idle CPU is about 1 % of a core for all three, and the differences are
small. With language servers off, Solo's main process used 0.1-0.2 % of a
core more than Zed's (project open: 1.06 % against 0.89 %; 10 idle
terminals: 1.13 % against 1.01 %). With language servers on, Solo used
about 0.8 % more (10 idle terminals: main process 1.83 % against 1.03 %;
after closing everything, 1.96 % against 1.34 %), and the 60 s thread
samples put the difference in the thread-pool workers (Solo's main process
1.9-2.1 % in all, Zed's 0.7-1.0 %).

The likely cause was a Solo addition, not something from Zed: while any
language server runs, the language server button sampled the memory of
every process on the machine every 4 s, to show what each server uses and
to warn when they use too much (`sample_usage` in
`crates/language_tools/src/lsp_button.rs`, which called
`server_resources::sample_process_trees`, a `sysinfo` refresh of all
processes, on the background executor). That fits the extra CPU appearing
only with language servers on, and only in thread-pool workers.

**Since then** (2026-11-20), the button reads every process only when the
servers' process trees may have changed (the first sample, servers
starting or stopping, a server reporting progress, a known process
ending) and every 30 s otherwise; in between it reads only the memory of
the processes it already knows. Measured with a release build
(`solo-dist`) of each, `json-language-server` and `vtsls` running on a
small TypeScript project (5 `node.exe` processes), 60 s idle after 60 s to
settle, 3 runs each, alternating, while other work kept about 4 of the 12
logical processors busy:

| Solo's process, % of a core (median of 3) | Before | After |
|---|--:|--:|
| Main thread | 0.68 | 0.52 |
| Other threads | 0.78 | 0.49 |
| Total | 1.46 | 1.12 |
| The sampling itself (its tasks' time, in a build that records task times) | 0.34 | 0.05 |

Per minute, the sampling took 196-206 ms before (16 samples of about
12 ms) and 26-28 ms after. The status bar shows the same memory figure as
before (412 MB against 413 MB for these servers). The sampling was about
0.3 % of a core, less than the 0.8 % by which Solo exceeded Zed above, so
it explains only part of that difference; the rest wasn't measured again.

With language servers on, Solo also used 11-12 % of a core in the minute
after opening the 200,000-line log in two of three runs, where Zed used
about 1 %; what ran then wasn't sampled.

## What was measured

### Machine and versions

| | |
|---|---|
| CPU | Intel Core i5-10600KF, 6 cores, 12 logical processors |
| Memory | 24 GB (about 7-10 GB free during the runs) |
| GPU | AMD Radeon RX 5600 XT |
| OS | Windows 11 Home 25H2 (build 26200), 2560 x 1440 display |
| Disk | NVMe SSD (the project copy and all data folders) |

| Editor | Version | Installed from |
|---|---|---|
| Solo | Solo Code 2.4.2 (`solo.exe` 2.4.2) | Microsoft Store (package `lucas9801.SoloCode` 2.4.2.0) |
| Zed | 1.28.1 stable (`1.28.1+stable.390.e8bc16ac`) | `winget install ZedIndustries.Zed` |
| VS Code | 1.146.0 | The official zip (`update.code.visualstudio.com/latest/win32-x64-archive/stable`), run in portable mode |

Solo is based on Zed's `main` of 2026-10-28, which its `crates/zed/Cargo.toml`
calls 1.28.0 (see [UPSTREAM.md](../UPSTREAM.md)). The Zed measured here is
the stable 1.28.1 release, built from Zed's stable branch: close to Solo's
base, but not the same code, so a difference between Solo and Zed can come
from Zed's own changes as well as from Solo's.

### Setup

- Every editor ran with a **fresh data folder** for every session, under
  the work folder: Solo and Zed with `--user-data-dir` and `ZED_STATELESS=1`
  (no session database, no hand-off to a running instance), VS Code in
  portable mode (`VSCODE_PORTABLE` pointing at the session's folder, which
  holds its user data, extensions and `argv.json`).
- Auto-update and telemetry off in all three; no sign-in. VS Code: workspace
  trust off, `workbench.startupEditor` none. Solo and Zed:
  `session.trust_all_worktrees` on (no "Restricted Mode" prompt). Cursor
  blinking off in all three, so a still window gives a still picture.
- Everything else at its default. That includes what each editor shows on
  its own: Solo opens a terminal in every window (and its onboarding page on
  its first launch); VS Code opens its Chat view and starts
  `copilot-runtime.exe`; Zed shows its onboarding page on its first launch
  and installs its HTML extension (`auto_install_extensions`).
- Autosave after 300 ms in all three, so edits are saved without a key
  chord (see [Driving the editors](#driving-the-editors)).
- The project: a clone (one commit, no other history) of this repository at
  the commit this document was written on: about 5,600 files, 2,350 Rust
  files, 125 MB. Reset (`git checkout`, `git clean`) before each session.
- Only one editor ran at a time. Before each session the script waits (up to
  5 minutes) until other programs use less than 1.5 cores, and it records
  what other programs used during each CPU window (`background_cpu_pct` in
  the raw results).

### Scenarios

Scenario a and startup ran in a separate series (3 runs) from b-g (3 runs),
after the way the script closes an editor between launches was changed.

Each run starts every session from a fresh data folder; the tables give the
**median of 3 runs**.

| | Scenario | What the script does |
|---|---|---|
| startup | Startup | Three launches with no folder: the first with a fresh data folder, then two more with the same folder. |
| a | Empty window | The third launch above, left alone until it is quiet, then measured. |
| b | Project open | Launch with the project folder, no files open; wait until quiet. |
| c | 20 files | Open 20 files one by one with the editor's file finder (17 Rust, `Cargo.toml`, `assets/settings/default.json`, one JavaScript file); in each, type `perf edit `, save, page down 15 times and up 15 times. Measured after it goes quiet. |
| d | Project search | Three project-wide searches (`fn new(`, `impl Render for`, `cx.notify()`), each run to the end. |
| e | Large log | Copy a 200,000-line, 30 MB log into the project, open it, search it for `status=503` and go to the next match three times. |
| f-busy | 10 busy terminals | End the shells of terminals already open, then open 10 terminals and run a script in each that prints like a coding agent (a spinner redrawn in place 10 times a second, a colored line every 100 ms, a 40-line colored diff every 5 s). Measured while they print. |
| f-idle | 10 idle terminals | The same 10 terminals after the script returns to the prompt. |
| g | Closed | End the terminals' shells, close all editors and terminals. |

Language servers **off** and **on** are separate sessions. With them on,
each editor runs what it starts by default for the files of scenario c:

- Solo and Zed: `json-language-server`, `vtsls` (TypeScript/JavaScript),
  `eslint`, `tailwindcss-language-server` and Node.js for them, downloaded
  once into a seed folder and copied into each fresh data folder, so the
  runs don't measure downloads. **rust-analyzer is off** in these runs: on
  this project it took 7.4 GB (Zed) and 9.7 GB (Solo) of private memory
  while indexing (with build scripts, proc macros and cargo check off),
  more than the machine had free. `-WithRustAnalyzer` turns it on.
- VS Code: its built-in language features (TypeScript, JSON, CSS, HTML,
  Markdown); it has no Rust language server without an extension, and none
  was installed. With language servers off, those built-in extensions are
  disabled with `--disable-extension`.

### Metrics

- **Memory**: private bytes (commit charge, `PrivateUsage`) and working set
  of every process in the editor's process tree (the process the script
  started and everything started under it), summed. Split into:
  - *main*: Solo's or Zed's own process; for VS Code its main, renderer, GPU
    and network processes;
  - *terminals*: Solo's `solo-terminal-host.exe` processes, the console
    hosts (`conhost.exe`, `OpenConsole.exe`), the shells and whatever runs
    in them; for VS Code also its pty host process. Solo keeps one extra
    PowerShell process for command completion in its terminals' input box,
    counted here;
  - *language servers*: language server processes and what they start
    (`node.exe`, `npm`), and VS Code's language server processes;
  - *other*: the rest (VS Code's extension host, shared process, file
    watcher, crash reporter and `copilot-runtime.exe`; Zed's crash handler;
    `git.exe`).

  Working sets of different processes share pages (DLLs, the GPU driver),
  so summed working sets overstate what the tree takes; private bytes don't
  double count. Neither includes GPU memory.
- **CPU**: CPU time (user + kernel) of the whole tree over a window, as a
  percentage of one logical processor (100 % = one of the 12 fully busy).
  The idle windows are 5 minutes in scenarios a, b and f-idle, 60 s in the
  others and in every session with language servers on.
- **Startup**: from `CreateProcess` until the window is *ready*: a visible
  top-level window of the tree, whose thread answers a message within
  100 ms, whose picture (`PrintWindow`) has some content (at least 24
  colors among sampled pixels) and stays the same for three captures 250 ms
  apart ("the same": at most 2 of the 48 x 32 blocks of the picture change,
  so a blinking caret doesn't count); the time is that of the first of the
  three. "Window" is when the first visible window appeared. *First* is the
  launch with a fresh data folder; *warm* the mean of the next two
  launches. Between launches the editor is closed through its window
  (`WM_CLOSE`) and the script waits until all its processes have ended.

### Driving the editors

The script drives the editors only through their own windows, and only
windows it started: `PrintWindow` to look, `PostMessage` to type. It never
sends global input, never brings a window to the front and never touches
the clipboard. Actions without a plain key (file finder, project search,
new terminal, close all) are bound to F13-F24 in each editor's fresh key
bindings, and saving is left to autosave, because posted messages can't
hold Ctrl down (the editors read the real keyboard state). VS Code ignores
keys while it thinks it is inactive, so the script also posts it
`WM_ACTIVATE` and `WM_SETFOCUS`; that changes nothing outside VS Code's
window. Terminal commands are typed into each editor's default shell
(Windows PowerShell 5.1).

## Limitations

- **One machine, a busy one.** The machine was also running other work:
  Rust builds and test runs from other work, an animated
  wallpaper, a Git cache service. In most CPU windows those kept all 12
  logical processors busy (the "Other programs" column), and the script's
  wait for a quiet machine (5 minutes at most) almost always ran out. The
  editors' CPU *time* is still their own, but anything measured as elapsed
  time (startup, searches, how long a scenario took to go quiet) is slower
  than on an idle machine, and work that keeps up with a display (the busy
  terminals) may do less when it gets less CPU. Memory numbers are much
  less affected.
- **No true cold start.** Emptying Windows' file cache needs administrator
  rights, which the measurement didn't have. *First* startup is the first
  launch with a fresh data folder (it includes each editor's first-run
  work), not a start after a reboot; the editors' files were most likely in
  the file cache already.
- **Frame rate and input latency weren't measured.** Without global input
  and without a tracing session (Windows Performance Recorder needs
  administrator rights), there is no reliable way to time a key press to
  the frame that shows it: `PrintWindow` captures take tens of
  milliseconds, so they can't time frames. Scenarios c and f report CPU and
  memory only.
- **Scenario c doesn't measure typing.** The text is typed with posted
  messages, 25 ms apart; saving is autosave; scrolling is Page Down/Up.
  It exercises opening, editing, saving and scrolling, not the editors'
  typing speed.
- **"Quiet" is a threshold.** The script waits until the editor's tree uses
  less than 3 % of a core for 5 s in a row (at most 2 minutes, 15 with
  language servers on); search times (`search_busy_s` in the raw results)
  are measured the same way and are approximate.
- **rust-analyzer is off** with language servers on (see above), so "on"
  covers the JSON, TypeScript, ESLint and Tailwind servers for Solo and Zed
  and VS Code's built-in language features. VS Code has no Rust language
  server at all here.
- **Different defaults.** Each editor ran with its own defaults, which
  differ: VS Code starts its Copilot runtime and Chat view; Solo opens a
  terminal (and its completion shell) in every window. The tables split
  memory by kind so the editors' own processes can be compared.
- **GPU memory isn't counted**, and summed working sets double count
  shared pages (private bytes don't).
- **Zed is 1.28.1 stable, not Solo's exact base** (Zed's `main` of
  2026-10-28).

## Reproducing

On Windows with Solo (Microsoft Store), Zed and VS Code installed, in this
repository:

```powershell
# Once: let Solo and Zed download their language servers into seed folders.
powershell -ExecutionPolicy Bypass -File script\measure-editor-performance.ps1 `
    -WorkRoot D:\solo-perf -LsSeedRoot D:\solo-perf\ls-seed -PrepareLsSeed
# The whole run: 3 runs, all scenarios, both language-server modes.
powershell -ExecutionPolicy Bypass -File script\measure-editor-performance.ps1 `
    -WorkRoot D:\solo-perf -LsSeedRoot D:\solo-perf\ls-seed -ThreadProfile
```

- `-WorkRoot` holds the project copy, the data folders and the results
  (`results\<time>\`: `summary.csv`, `summary.json`, `results-raw.json`,
  `processes.json` with every process of every sample, `trace.log`, and a
  screenshot of each scenario). It needs a few GB.
- The script finds Solo through its Store package, Zed in
  `%LOCALAPPDATA%\Programs\Zed` and VS Code at `<WorkRoot>\apps\vscode`
  (the zip, extracted); `-SoloExe`, `-ZedExe` and `-CodeExe` override that.
- `-Editors`, `-Scenarios`, `-LanguageServers off|on|both`, `-Runs`,
  `-IdleCpuSeconds` and `-ShortCpuSeconds` narrow a run; `-SummarizeFrom`
  makes one table from the results of several invocations.
- One run of all three editors with both modes took about 3 hours here,
  much of it idle-CPU windows and waits for a quiet machine.
- Leave the machine alone while it runs: the windows appear on the screen
  (without taking the keyboard), and other programs' CPU use affects the
  results despite the wait for a quiet machine.
