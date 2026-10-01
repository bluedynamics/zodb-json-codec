"""The documentation's changelog page is the root CHANGES.md, not a copy (#63)."""

import re
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
CHANGES = ROOT / "CHANGES.md"
PAGE = ROOT / "docs" / "sources" / "reference" / "changelog.md"
MARKER = "<!-- changelog-start -->"


def test_page_includes_the_root_changelog():
    page = PAGE.read_text()
    assert "```{include} ../../../CHANGES.md" in page
    assert f":start-after: {MARKER}" in page
    assert (PAGE.parent / "../../../CHANGES.md").resolve() == CHANGES


def test_root_changelog_has_the_include_marker():
    text = CHANGES.read_text()
    assert text.count(MARKER) == 1
    # everything a reader needs is below the marker
    assert "## " in text.split(MARKER, 1)[1]


def test_page_carries_no_entries_of_its_own():
    # a second copy would drift; the page may only have its own title and prose
    assert not re.search(r"^## ", PAGE.read_text(), re.M)


def test_released_version_matches_cargo_toml():
    # during development the top section is "unreleased"; once it carries a
    # version, that version is what Cargo.toml says, so a release cannot ship
    # with a changelog header for another version
    top = re.search(r"^## (\S+)", CHANGES.read_text().split(MARKER, 1)[1], re.M).group(
        1
    )
    if top == "unreleased":
        return
    cargo = re.search(
        r'^version = "([^"]+)"', (ROOT / "Cargo.toml").read_text(), re.M
    ).group(1)
    assert top == cargo, f"changelog says {top}, Cargo.toml says {cargo}"
