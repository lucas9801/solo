use super::*;
use futures::{StreamExt as _, executor::block_on};
use http_client::FakeHttpClient;

fn leaked_guard() -> &'static Guard {
    Box::leak(Box::new(Guard::new()))
}

fn policy(mode: DownloadMode) -> Policy {
    Policy {
        mode,
        ..Policy::default()
    }
}

#[test]
fn local_hosts() {
    for host in [
        "localhost",
        "LOCALHOST",
        "api.localhost",
        "127.0.0.1",
        "127.1.2.3",
        "10.0.0.5",
        "172.16.0.1",
        "172.31.255.255",
        "192.168.1.20",
        "169.254.10.1",
        "0.0.0.0",
        "::1",
        "[::1]",
        "fd12:3456::1",
        "fe80::1",
        "::ffff:192.168.0.1",
        "nas",
        "printer.local",
        "build.lan",
        "ci.internal",
        "router.home.arpa",
    ] {
        assert!(is_local_host(host), "{host} should be local");
    }
    for host in [
        "registry.npmjs.org",
        "github.com",
        "api.github.com",
        "8.8.8.8",
        "172.32.0.1",
        "100.64.0.1",
        "2606:4700::1111",
        "[2606:4700::1111]",
        "::ffff:8.8.8.8",
        "localhost.example.com",
    ] {
        assert!(!is_local_host(host), "{host} should not be local");
    }
}

#[test]
fn reachable_offline_urls() {
    let reachable = |url: &str| reachable_offline(&Url::parse(url).unwrap());
    assert!(reachable("http://localhost:11434/api/tags"));
    assert!(reachable("http://192.168.1.5:1234/v1/models"));
    assert!(reachable("http://[::1]:8080/"));
    assert!(reachable("file:///C:/x"));
    assert!(!reachable("https://registry.npmjs.org/prettier"));
    assert!(!reachable("https://api.github.com/repos/x/y/releases"));
}

#[test]
fn offline_http_client_blocks_only_remote_hosts() {
    let guard = leaked_guard();
    let fake = FakeHttpClient::with_200_response();
    let client = OfflineHttpClient::with_guard(fake, guard);

    let get = |url: &str| block_on(client.get(url, AsyncBody::default(), false));

    // Online, everything goes through.
    assert!(get("https://github.com/").is_ok());

    guard.set_policy(Policy {
        offline: true,
        ..Policy::default()
    });
    let error = get("https://github.com/").err().unwrap();
    assert!(error.to_string().contains("github.com"), "{error}");
    assert!(get("https://registry.npmjs.org/prettier").is_err());
    assert!(get("http://localhost:11434/api/tags").is_ok());
    assert!(get("http://127.0.0.1:1234/v1/models").is_ok());
    assert!(get("http://10.1.2.3/").is_ok());
    assert!(get("http://[::1]:8080/").is_ok());

    guard.set_policy(Policy::default());
    assert!(get("https://github.com/").is_ok());
}

#[test]
fn decisions() {
    let none = HashSet::default();
    let ask = policy(DownloadMode::Ask);

    // Ask mode: a new component is asked for, an installed one updates.
    assert_eq!(decide(&ask, &none, "vtsls", false, true), Decision::Ask);
    assert_eq!(decide(&ask, &none, "vtsls", true, true), Decision::Allow);

    // Allowed components don't ask, denied ones never download.
    let mut answered = ask.clone();
    answered.allowed.insert("vtsls".into(), true);
    answered.allowed.insert("eslint".into(), false);
    assert_eq!(
        decide(&answered, &none, "vtsls", false, true),
        Decision::Allow
    );
    assert_eq!(
        decide(&answered, &none, "eslint", false, true),
        Decision::Block(Blocked::Denied)
    );
    assert_eq!(
        decide(&answered, &none, "eslint", true, true),
        Decision::Block(Blocked::Denied)
    );

    // "Not now" holds until restart.
    let declined = HashSet::from_iter(["vtsls".to_string()]);
    assert_eq!(
        decide(&ask, &declined, "vtsls", false, true),
        Decision::Block(Blocked::NotNow)
    );

    // Auto downloads for trusted projects, still asks for untrusted ones.
    let auto = policy(DownloadMode::Auto);
    assert_eq!(
        decide(&auto, &none, "prettier", false, true),
        Decision::Allow
    );
    assert_eq!(
        decide(&auto, &none, "prettier", false, false),
        Decision::Ask
    );
    assert_eq!(
        decide(&answered, &none, "vtsls", false, false),
        Decision::Allow
    );

    // Never, offline, and no update checks.
    let never = policy(DownloadMode::Never);
    assert_eq!(
        decide(&never, &none, "vtsls", true, true),
        Decision::Block(Blocked::Never)
    );
    let offline = Policy {
        offline: true,
        mode: DownloadMode::Auto,
        ..Policy::default()
    };
    assert_eq!(
        decide(&offline, &none, "vtsls", true, true),
        Decision::Block(Blocked::Offline)
    );
    let no_updates = Policy {
        check_updates: false,
        ..answered.clone()
    };
    assert_eq!(
        decide(&no_updates, &none, "vtsls", true, true),
        Decision::Block(Blocked::NoUpdates)
    );
    assert_eq!(
        decide(&no_updates, &none, "vtsls", false, true),
        Decision::Allow
    );
}

#[test]
fn without_a_ui_questions_are_allowed() {
    let guard = Guard::new();
    assert_eq!(
        block_on(guard.request(Component::language_server("vtsls"), false, true)),
        Ok(())
    );
    assert!(guard.pending().is_empty());
}

#[test]
fn asking_allowing_and_remembering() {
    let guard = Guard::new();
    guard.set_asks_user(true);
    let mut changes = guard.subscribe();

    let mut first = guard
        .request(Component::language_server("vtsls"), false, true)
        .boxed();
    let mut second = guard
        .request(Component::language_server("vtsls"), false, true)
        .boxed();
    let mut other = guard.request(Component::node(), false, true).boxed();
    assert!((&mut first).now_or_never().is_none());
    assert!((&mut second).now_or_never().is_none());
    assert!((&mut other).now_or_never().is_none());
    assert!(block_on(changes.next()).is_some());

    // Simultaneous requests are one question per component.
    let pending = guard.pending();
    assert_eq!(
        pending.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(),
        ["node", "vtsls"]
    );
    assert_eq!(pending[1].label(), "vtsls (registry.npmjs.org)");

    guard.answer(&["vtsls".to_string()], Answer::Allow);
    assert_eq!(block_on(first), Ok(()));
    assert_eq!(block_on(second), Ok(()));
    assert_eq!(guard.take_to_remember(), ["vtsls"]);
    assert!(guard.take_to_remember().is_empty());
    assert_eq!(guard.policy().allowed.get("vtsls"), Some(&true));

    // Later requests (updates included) don't ask again.
    assert_eq!(
        block_on(guard.request(Component::language_server("vtsls"), false, true)),
        Ok(())
    );

    guard.answer(&["node".to_string()], Answer::NotNow);
    let error = block_on(other).unwrap_err();
    assert_eq!(error.reason, Blocked::NotNow);
    assert!(guard.take_to_remember().is_empty());
    // "Not now" isn't asked again until restart.
    assert_eq!(
        block_on(guard.request(Component::node(), false, true))
            .unwrap_err()
            .reason,
        Blocked::NotNow
    );
    assert!(guard.pending().is_empty());

    // A retry asks again.
    guard.forget_declined("node");
    assert!(
        guard
            .request(Component::node(), false, true)
            .boxed()
            .now_or_never()
            .is_none()
    );
    assert_eq!(guard.pending().len(), 1);
}

#[test]
fn settings_settle_waiting_questions() {
    let guard = Guard::new();
    guard.set_asks_user(true);
    let waiting = guard.request(Component::prettier(), false, true).boxed();
    let untrusted = guard.request(Component::copilot(), false, false).boxed();
    assert_eq!(guard.pending().len(), 2);

    // Switching to "auto" allows what's waiting for trusted projects only.
    guard.set_policy(policy(DownloadMode::Auto));
    assert_eq!(block_on(waiting), Ok(()));
    assert_eq!(guard.pending().len(), 1);

    // Going offline turns the rest down.
    guard.set_policy(Policy {
        offline: true,
        ..Policy::default()
    });
    assert_eq!(block_on(untrusted).unwrap_err().reason, Blocked::NotNow);
    assert!(guard.pending().is_empty());
}

#[test]
fn trust_dialog_list() {
    let guard = Guard::new();
    guard.note_before_trust(Component::language_server("rust-analyzer"));
    guard.note_before_trust(Component::language_server("rust-analyzer"));
    guard.note_before_trust(Component::language_server("vtsls"));
    assert_eq!(guard.before_trust().len(), 2);

    // Allowing from the dialog remembers them, and they won't ask.
    guard.answer(
        &["rust-analyzer".to_string(), "vtsls".to_string()],
        Answer::Allow,
    );
    assert!(guard.before_trust().is_empty());
    assert_eq!(guard.take_to_remember().len(), 2);
    assert_eq!(
        guard.decision(&Component::language_server("vtsls"), false, true),
        Decision::Allow
    );

    // Already allowed components aren't listed.
    guard.note_before_trust(Component::language_server("vtsls"));
    assert!(guard.before_trust().is_empty());
}

#[test]
fn npm_network_subcommands() {
    assert!(npm_subcommand_uses_network("install"));
    assert!(npm_subcommand_uses_network("info"));
    assert!(!npm_subcommand_uses_network("run-script"));
    assert!(!npm_subcommand_uses_network("config"));
    assert!(!npm_subcommand_uses_network("--version"));
}
