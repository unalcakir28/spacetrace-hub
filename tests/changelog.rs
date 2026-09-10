//! CHANGELOG.md is generated, so it can silently fall behind its source.
//!
//! The source is `crates/changelog/changelog.json` in the core repo, which
//! reaches here as a git dependency — so this file also fails when the pin is
//! advanced and the changelog is not regenerated, which is the mistake the
//! release procedure warns about.

use spacetrace_changelog::{changelog, render, Component};

#[test]
fn changelog_md_matches_its_source() {
    let expected = render::markdown(changelog(), Component::Hub);
    let actual = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/CHANGELOG.md"))
        .expect("CHANGELOG.md is committed at the repo root");

    assert_eq!(
        actual, expected,
        "CHANGELOG.md is stale. Regenerate it from the core repo:\n  \
         cargo run -p spacetrace-changelog -- markdown --component hub > CHANGELOG.md"
    );
}

/// The About page reads the hub's own entries. An empty component would render
/// a heading with nothing under it, which reads as a bug rather than as
/// "nothing shipped yet".
///
/// `releases` alone, not `releases || unreleased`: the page shows released
/// work only, so unreleased entries cannot stand in for having something to
/// say.
#[test]
fn the_hub_has_something_to_say_for_itself() {
    let log = changelog().component(Component::Hub);
    assert!(!log.releases.is_empty());
}
