//! SOLO: measures the main-thread work terminal output causes: which terminals
//! are laid out again, how often the tab bar updates, and what Solo's own
//! per-output code costs. Run with `--nocapture` to see the numbers; the
//! asserts keep the counts from coming back.

use std::{
    cell::Cell,
    rc::Rc,
    time::{Duration, Instant},
};

use gpui::{Entity, TestAppContext, VisualTestContext};
use terminal::Terminal;
use workspace::pane;

use crate::{
    TerminalView,
    terminal_element::test_stats,
    tests::{add_display_only_terminal, init_test_with_window},
};

const FRAMES: usize = 200;

/// A full-screen TUI redraw, the way an agent's UI repaints: home the cursor,
/// then every row with colors and a changing spinner.
fn tui_frame(frame: usize, rows: usize, cols: usize) -> Vec<u8> {
    const SPINNER: [char; 6] = ['·', '✢', '✳', '✶', '✻', '✽'];
    let mut out = String::from("\x1b[?25l\x1b[H");
    for row in 0..rows {
        let color = 16 + (row * 7 + frame) % 216;
        out.push_str(&format!("\x1b[{};1H\x1b[38;5;{color}m", row + 1));
        let text = if row == rows - 2 {
            format!(
                "{} Working… ({}s · esc to interrupt)",
                SPINNER[frame % SPINNER.len()],
                frame / 10
            )
        } else {
            format!("{row:>3} │ fn example_{row}() -> Result<()> {{ Ok(()) }} // frame {frame}")
        };
        out.extend(text.chars().take(cols));
        out.push_str("\x1b[0m\x1b[K");
    }
    out.push_str("\x1b[?25h");
    out.into_bytes()
}

/// Shell integration marks for a prompt where `claude` was started.
const START_CLAUDE: &[u8] =
    b"\x1b]133;A\x07$ \x1b]133;B\x07claude\r\n\x1b]633;E;claude\x07\x1b]133;C\x07";

fn write(terminal: &Entity<Terminal>, bytes: &[u8], cx: &mut VisualTestContext) {
    terminal.update(cx, |terminal, cx| terminal.write_raw_output(bytes, cx));
    cx.run_until_parked();
}

struct Measured {
    elapsed: Duration,
    tab_updates: usize,
    visible_layouts: (usize, Duration),
    background_layouts: (usize, Duration),
}

/// Two agents in one pane: one shown, one in a background tab. The
/// background one redraws its screen `FRAMES` times.
async fn background_agent_output(alt_screen: bool, cx: &mut TestAppContext) -> Measured {
    let (project, _workspace, window) = init_test_with_window(cx).await;
    let (pane, background, _) = add_display_only_terminal(&project, window, false, false, cx);
    let (_, visible, _) = add_display_only_terminal(&project, window, true, false, cx);
    let cx = &mut VisualTestContext::from_window(window.into(), cx);

    let tab_updates = Rc::new(Cell::new(0));
    let _subscription = cx.update(|_, cx| {
        let tab_updates = tab_updates.clone();
        cx.subscribe(&pane, move |_, event, _| {
            if matches!(event, pane::Event::ChangeItemTitle) {
                tab_updates.set(tab_updates.get() + 1);
            }
        })
    });

    write(&visible, START_CLAUDE, cx);
    write(&visible, &tui_frame(0, 30, 100), cx);
    write(&background, START_CLAUDE, cx);
    if alt_screen {
        write(&background, b"\x1b[?1049h", cx);
    }
    tab_updates.set(0);
    test_stats::take_prepaints(visible.entity_id());
    test_stats::take_prepaints(background.entity_id());

    let start = Instant::now();
    for frame in 0..FRAMES {
        write(&background, &tui_frame(frame, 30, 100), cx);
    }
    Measured {
        elapsed: start.elapsed(),
        tab_updates: tab_updates.get(),
        visible_layouts: test_stats::take_prepaints(visible.entity_id()),
        background_layouts: test_stats::take_prepaints(background.entity_id()),
    }
}

fn report(name: &str, measured: &Measured) {
    eprintln!(
        "{name}: {FRAMES} frames in {:?}; tab updates {}; visible terminal laid out {} times ({:?}); background terminal laid out {} times",
        measured.elapsed,
        measured.tab_updates,
        measured.visible_layouts.0,
        measured.visible_layouts.1,
        measured.background_layouts.0,
    );
}

#[gpui::test]
async fn test_background_agent_output(cx: &mut TestAppContext) {
    let measured = background_agent_output(false, cx).await;
    report("background agent, normal screen", &measured);
    assert_eq!(measured.background_layouts.0, 0);
    // The tab doesn't change while the agent keeps working, so neither the
    // tab bar nor the terminal shown next to it are drawn again.
    assert!(measured.tab_updates <= 3, "{}", measured.tab_updates);
    assert!(
        measured.visible_layouts.0 <= 3,
        "{}",
        measured.visible_layouts.0
    );
}

#[gpui::test]
async fn test_background_agent_output_alt_screen(cx: &mut TestAppContext) {
    let measured = background_agent_output(true, cx).await;
    report("background agent, alternate screen", &measured);
    assert_eq!(measured.background_layouts.0, 0);
    assert!(measured.tab_updates <= 3, "{}", measured.tab_updates);
    assert!(
        measured.visible_layouts.0 <= 3,
        "{}",
        measured.visible_layouts.0
    );
}

/// A working agent with nothing new to print: what the window draws per
/// display frame (a display refreshes 60 to 144 times a second).
#[gpui::test]
async fn test_working_agent_between_outputs(cx: &mut TestAppContext) {
    let (project, _workspace, window) = init_test_with_window(cx).await;
    let (_, background, background_view) =
        add_display_only_terminal(&project, window, false, false, cx);
    let (_, visible, _) = add_display_only_terminal(&project, window, true, false, cx);
    let cx = &mut VisualTestContext::from_window(window.into(), cx);
    write(&background, START_CLAUDE, cx);
    // Output right after the terminal opened is taken as answering its resize.
    std::thread::sleep(Duration::from_millis(500));
    write(&background, &tui_frame(0, 30, 100), cx);
    let status = background_view.read_with(cx, |view, _| view.agent_status().map(|(_, s)| s));
    assert_eq!(status, Some(crate::agent_status::AgentStatus::Working));
    test_stats::take_prepaints(visible.entity_id());

    const DISPLAY_FRAMES: usize = 120;
    let start = Instant::now();
    let mut frame_callbacks = 0;
    for _ in 0..DISPLAY_FRAMES {
        std::thread::sleep(Duration::from_millis(2));
        frame_callbacks += cx.update(|window, cx| window.simulate_next_frame(cx));
        cx.run_until_parked();
    }
    eprintln!("next-frame callbacks: {frame_callbacks}");
    let elapsed = start.elapsed();
    let (layouts, layout_time) = test_stats::take_prepaints(visible.entity_id());
    eprintln!(
        "working agent in a background tab, no output: {DISPLAY_FRAMES} display frames in {elapsed:?}; visible terminal laid out {layouts} times ({layout_time:?})"
    );
    assert!(layouts <= 2, "{layouts} layouts in {elapsed:?}");

    // The working icon still turns, 8 times a second.
    for _ in 0..8 {
        cx.executor().advance_clock(Duration::from_millis(125));
        cx.run_until_parked();
    }
    let (layouts, _) = test_stats::take_prepaints(visible.entity_id());
    assert!((7..=9).contains(&layouts), "{layouts}");
}

/// The shown agent redraws: what laying it out costs, for comparison with
/// Solo's own per-output work below.
#[gpui::test]
async fn test_visible_agent_output(cx: &mut TestAppContext) {
    let (project, _workspace, window) = init_test_with_window(cx).await;
    let (_, visible, view) = add_display_only_terminal(&project, window, true, false, cx);
    let cx = &mut VisualTestContext::from_window(window.into(), cx);
    write(&visible, START_CLAUDE, cx);
    test_stats::take_prepaints(visible.entity_id());

    let start = Instant::now();
    for frame in 0..FRAMES {
        write(&visible, &tui_frame(frame, 30, 100), cx);
    }
    let elapsed = start.elapsed();
    let (layouts, layout_time) = test_stats::take_prepaints(visible.entity_id());
    eprintln!(
        "visible agent: {FRAMES} frames in {elapsed:?}; laid out {layouts} times ({layout_time:?}, {:?} each)",
        layout_time / layouts.max(1) as u32
    );
    assert!(layouts >= FRAMES);

    // Solo's per-output bookkeeping, on its own.
    const CALLS: u32 = 2_000;
    let start = Instant::now();
    for _ in 0..CALLS {
        view.update_in(cx, |view: &mut TerminalView, window, cx| {
            view.agent_output(window, cx)
        });
    }
    let agent_output = start.elapsed();
    cx.run_until_parked();
    eprintln!(
        "agent_output: {:?} per call ({CALLS} calls)",
        agent_output / CALLS
    );
}
