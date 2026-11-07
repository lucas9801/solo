//! Ported from Herdr's `src/detect/manifest/tests.rs` (Apache-2.0), plus
//! checks of the bundled rules on screens made after them.

use super::*;

fn manifest(rules: &str) -> Manifest {
    Manifest::parse(&format!("id = \"codex\"\n\n{rules}")).unwrap()
}

fn detect(manifest: &Manifest, screen: &str) -> Detection {
    manifest.detect(&DetectionInput {
        screen,
        ..DetectionInput::default()
    })
}

#[test]
fn no_match_is_idle() {
    let manifest = manifest(
        r#"
[[rules]]
id = "working"
state = "working"
contains = ["active-marker"]
"#,
    );
    let detection = detect(&manifest, "unmatched-marker");
    assert_eq!(detection.state, DetectedState::Idle);
    assert!(!detection.visible_idle);
    assert_eq!(detection.rule, None);
}

#[test]
fn gates_priority_and_line_regex() {
    let manifest = manifest(
        r#"
[[rules]]
id = "low_contains"
state = "idle"
priority = 1
contains = ["match"]

[[rules]]
id = "high_nested_gates"
state = "working"
priority = 10
contains = ["match"]
all = [{ any = [{ regex = ['w[io]n'] }, { contains = ["fallback"] }] }]
not = [{ contains = ["blocked"] }]

[[rules]]
id = "line_regex"
state = "blocked"
priority = 20
line_regex = ['^exact line$']
"#,
    );
    let rule = |screen: &str| detect(&manifest, screen);
    assert_eq!(rule("match win").state, DetectedState::Working);
    assert_eq!(rule("match win").rule.as_deref(), Some("high_nested_gates"));
    assert_eq!(
        rule("match win blocked").rule.as_deref(),
        Some("low_contains")
    );
    assert_eq!(rule("MATCH fallback").state, DetectedState::Working);
    assert_eq!(
        rule("before\nexact line\nafter").rule.as_deref(),
        Some("line_regex")
    );
}

#[test]
fn ties_go_to_the_earlier_rule() {
    let manifest = manifest(
        r#"
[[rules]]
id = "first"
state = "idle"
contains = ["x"]

[[rules]]
id = "second"
state = "working"
contains = ["x"]
"#,
    );
    assert_eq!(detect(&manifest, "x").rule.as_deref(), Some("first"));
}

#[test]
fn paired_alternatives_at_the_bottom() {
    let manifest = manifest(
        r#"
[[rules]]
id = "paired"
state = "blocked"
region = "bottom_non_empty_lines(3)"
visible_blocker = true
any = [
  { all = [{ contains = ["alpha heading"] }, { regex = ['alpha\s+control\s*\z'] }] },
  { all = [{ contains = ["beta heading"] }, { regex = ['beta\s+control\s*\z'] }] },
]
"#,
    );
    for screen in [
        "alpha heading\nalpha control",
        "beta heading\nbeta\ncontrol",
    ] {
        let detection = detect(&manifest, screen);
        assert_eq!(detection.state, DetectedState::Blocked, "{screen:?}");
        assert!(detection.visible_blocker);
    }
    for screen in [
        "alpha heading\nbeta control",
        "beta heading\nalpha control",
        "beta control",
        "beta heading",
        "beta heading\nbeta control\nnewer content",
        "beta heading\nfiller one\nfiller two\nbeta control",
    ] {
        let detection = detect(&manifest, screen);
        assert_eq!(detection.rule, None, "{screen:?}");
        assert!(!detection.visible_blocker);
    }
}

#[test]
fn title_and_progress_compete_with_the_screen() {
    let manifest = manifest(
        r#"
[[rules]]
id = "screen"
state = "idle"
priority = 10
visible_idle = true
contains = ["screen-marker"]

[[rules]]
id = "title"
state = "working"
priority = 20
region = "osc_title"
visible_working = true
regex = ['^title-marker$']

[[rules]]
id = "progress"
state = "blocked"
priority = 30
region = "osc_progress"
visible_blocker = true
regex = ['^progress-marker$']
"#,
    );
    let detect = |screen, osc_title, osc_progress| {
        manifest.detect(&DetectionInput {
            screen,
            osc_title,
            osc_progress,
        })
    };
    let cases = [
        (
            ("screen-marker", "", ""),
            Some("screen"),
            DetectedState::Idle,
        ),
        (
            ("screen-marker", "title-marker", ""),
            Some("title"),
            DetectedState::Working,
        ),
        (
            ("screen-marker", "title-marker", "progress-marker"),
            Some("progress"),
            DetectedState::Blocked,
        ),
        (
            ("screen-marker title-marker progress-marker", "", ""),
            Some("screen"),
            DetectedState::Idle,
        ),
        (
            ("", "progress-marker", "title-marker"),
            None,
            DetectedState::Idle,
        ),
    ];
    for ((screen, title, progress), rule, state) in cases {
        let detection = detect(screen, title, progress);
        assert_eq!(detection.rule.as_deref(), rule);
        assert_eq!(detection.state, state);
        assert_eq!(
            detection.visible_idle,
            rule.is_some() && state == DetectedState::Idle
        );
        assert_eq!(detection.visible_working, state == DetectedState::Working);
        assert_eq!(detection.visible_blocker, state == DetectedState::Blocked);
    }
}

#[test]
fn skip_rules_leave_the_state_alone() {
    let manifest = manifest(
        r#"
[[rules]]
id = "activity"
state = "working"
priority = 10
visible_working = true
contains = ["activity-marker"]

[[rules]]
id = "overlay"
state = "unknown"
priority = 20
skip_state_update = true
contains = ["overlay-marker"]
"#,
    );
    let detection = detect(&manifest, "activity-marker overlay-marker");
    assert_eq!(detection.state, DetectedState::Unknown);
    assert!(detection.skip_state_update);
    assert_eq!(detection.rule.as_deref(), Some("overlay"));
    assert!(!detection.visible_idle && !detection.visible_working && !detection.visible_blocker);
}

#[test]
fn regions() {
    let input = |screen| DetectionInput {
        screen,
        ..DetectionInput::default()
    };
    let region = |screen: &'static str, region: &str| -> &'static str {
        Region::parse(region).unwrap().text(&input(screen))
    };
    let prompt = "before\n› input\nafter\n";
    let cases = [
        ("old\n\nnew\n", "bottom_lines(2)", "\nnew\n"),
        (prompt, "after_last_prompt_marker", "after\n"),
        (prompt, "before_current_prompt_marker", "before\n"),
        (prompt, "whole_recent_without_current_prompt_marker", ""),
        (
            "no marker\n",
            "whole_recent_without_current_prompt_marker",
            "no marker\n",
        ),
        (
            "• old\n■ latest\n› input\n",
            "current_prompt_block_marker",
            "■ latest",
        ),
        (
            "• old\n■ latest\n› input\n",
            "after_current_prompt_block_marker",
            "■ latest\n› input\n",
        ),
        ("› old\n• new\n", "current_prompt_block_marker", ""),
        (
            "above\n\n───\nbody\n───\nfooter\n",
            "above_prompt_box",
            "above\n\n",
        ),
        (
            "above\n\n───\nbody\n───\nfooter\n",
            "last_non_empty_above_prompt_box",
            "above",
        ),
        (
            "above\n───\nbody\n───\nfooter\n",
            "prompt_box_body",
            "body\n",
        ),
        (
            "above\n───\nbody\n───\nfooter\n",
            "after_last_horizontal_rule",
            "footer\n",
        ),
        (
            "marker\nold\n\nmiddle\nmarker\nnew\n",
            "bottom_non_empty_lines(2)",
            "marker\nnew\n",
        ),
        (
            "\nmarker\nold\n\nmiddle\nmarker\nnew\n",
            "top_non_empty_lines(2)",
            "\nmarker\nold\n",
        ),
        ("short\n", "top_non_empty_lines(5)", "short\n"),
        ("", "bottom_non_empty_lines(3)", ""),
    ];
    for (screen, name, expected) in cases {
        assert_eq!(region(screen, name), expected, "{name} of {screen:?}");
    }
    assert!(is_horizontal_rule("─── name"));
    assert!(is_horizontal_rule("  ────  "));
    assert!(!is_horizontal_rule("── x"));
    assert!(!is_horizontal_rule("text ───"));
}

#[test]
fn region_counts_are_positive_and_bounded() {
    for good in [
        "top_non_empty_lines(1)",
        "top_non_empty_lines(65535)",
        " bottom_lines(3) ",
    ] {
        assert!(Region::parse(good).is_ok(), "{good}");
    }
    for bad in [
        "top_non_empty_lines(0)",
        "top_non_empty_lines(01)",
        "top_non_empty_lines(+1)",
        "top_non_empty_lines(65536)",
        "top_non_empty_lines(99999999999999999999999)",
        "after_last_promt_marker",
        "bottom_lines()",
    ] {
        assert!(Region::parse(bad).is_err(), "{bad}");
    }
}

#[test]
fn invalid_rules_are_rejected() {
    let bad = [
        // A misspelled key.
        "[[rules]]\nid = \"x\"\ncontain = [\"a\"]\n",
        // Nothing to match.
        "[[rules]]\nid = \"x\"\nstate = \"idle\"\n",
        "[[rules]]\nid = \"x\"\nregion = \"after_last_promt_marker\"\ncontains = [\"a\"]\n",
        "[[rules]]\nid = \"x\"\nregex = [\"[\"]\n",
        "[[rules]]\nid = \"x\"\nany = [{ line_regex = [\"[\"] }]\n",
        // Only `not`.
        "[[rules]]\nid = \"x\"\nnot = [{ contains = [\"a\"] }]\n",
        // A skip rule with a state.
        "[[rules]]\nid = \"x\"\nstate = \"idle\"\nskip_state_update = true\ncontains = [\"a\"]\n",
        "[[rules]]\nid = \"x\"\nskip_state_update = true\nvisible_blocker = true\ncontains = [\"a\"]\n",
        "[[rules]]\nid = \"\"\ncontains = [\"a\"]\n",
    ];
    for rules in bad {
        assert!(
            Manifest::parse(&format!("id = \"codex\"\n{rules}")).is_err(),
            "{rules}"
        );
    }
    assert!(Manifest::parse("id = \"codex\"\n").is_err(), "no rules");
    assert!(
        Manifest::parse(
            "id = \"codex\"\nmin_engine_version = 2\n[[rules]]\nid = \"x\"\nregion = \"top_non_empty_lines(1)\"\ncontains = [\"a\"]\n"
        )
        .is_err()
    );
    assert!(
        Manifest::parse(
            "id = \"codex\"\nmin_engine_version = 4\n[[rules]]\nid = \"x\"\ncontains = [\"a\"]\n"
        )
        .is_err()
    );
}

#[test]
fn limits() {
    let rules = (0..129)
        .map(|index| format!("[[rules]]\nid = \"r{index}\"\ncontains = [\"a\"]\n"))
        .collect::<String>();
    assert!(Manifest::parse(&format!("id = \"codex\"\n{rules}")).is_err());

    let mut gate = "{ contains = [\"a\"] }".to_string();
    for _ in 0..8 {
        gate = format!("{{ all = [{gate}] }}");
    }
    assert!(
        Manifest::parse(&format!(
            "id = \"codex\"\n[[rules]]\nid = \"deep\"\nall = [{gate}]\n"
        ))
        .is_err()
    );

    let many = (0..33)
        .map(|index| format!("\"m{index}\""))
        .collect::<Vec<_>>()
        .join(", ");
    assert!(
        Manifest::parse(&format!(
            "id = \"codex\"\n[[rules]]\nid = \"wide\"\ncontains = [{many}]\n"
        ))
        .is_err()
    );
}

#[test]
fn bundled_rules_are_valid() {
    for (file, text) in BUNDLED {
        Manifest::parse(text).unwrap_or_else(|error| panic!("{file}: {error:#}"));
    }
    assert_eq!(bundled().len(), BUNDLED.len());
    for name in [
        "claude",
        "Codex",
        "opencode",
        "open-code",
        "copilot",
        "ghcs",
        "gemini",
    ] {
        assert!(
            bundled().iter().any(|manifest| manifest.is_for(name)),
            "{name}"
        );
    }
}

/// The bundled rules on screens made after them (not recorded from the
/// agents).
#[test]
fn bundled_rules_read_agent_screens() {
    let agent = |name: &str| {
        bundled()
            .iter()
            .find(|manifest| manifest.is_for(name))
            .unwrap()
    };
    let detect = |name: &str, screen: &str, osc_title: &str| {
        agent(name).detect(&DetectionInput {
            screen,
            osc_title,
            osc_progress: "",
        })
    };
    assert_eq!(
        detect("claude", "", "⠂ Fixing tests").state,
        DetectedState::Working
    );
    let idle = detect("claude", "", "✳ Claude Code");
    assert_eq!(idle.state, DetectedState::Idle);
    assert!(idle.visible_idle);
    assert_eq!(
        detect("codex", "", "⠋ my-repo").state,
        DetectedState::Working
    );
    assert_eq!(
        detect("codex", "", "Action Required").state,
        DetectedState::Blocked
    );
    assert_eq!(
        detect("opencode", "△ Permission required\n", "").state,
        DetectedState::Blocked
    );
    assert_eq!(
        detect("claude", "nothing to see\n", "").state,
        DetectedState::Idle
    );
}

#[test]
fn overrides_replace_bundled_rules() {
    let directory =
        std::env::temp_dir().join(format!("solo-agent-detection-test-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(
        directory.join("codex.toml"),
        "id = \"codex\"\n[[rules]]\nid = \"mine\"\nstate = \"blocked\"\ncontains = [\"custom question\"]\n",
    )
    .unwrap();
    std::fs::write(directory.join("broken.toml"), "id = \"x\"\n[[rules]]\n").unwrap();
    let (manifests, errors) = load_with_overrides(&directory);
    std::fs::remove_dir_all(&directory).ok();
    assert_eq!(errors.len(), 1, "{errors:?}");
    let codex = &manifests["codex"];
    assert_eq!(
        detect(codex, "a custom question?").rule.as_deref(),
        Some("mine")
    );
    assert!(manifests.contains_key("claude"));
}
