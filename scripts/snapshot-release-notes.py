#!/usr/bin/env python3
"""Bundle the merged upstream release and its predecessor for the Linux fork.

GitHub is consulted only by the maintenance command, never by the desktop.
If the API is unavailable, local upstream commit subjects provide the notes.
"""
import argparse
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tomllib

ROOT = Path(__file__).resolve().parents[1]
CATALOG = Path("docs/releases/changelog.json")
UPSTREAM = "zeronsh/zeron"
FORK = "IIRoan/zeron"
MAX_NOTES_BYTES = 64 * 1024
VERSION = re.compile(r"^v?(\d+)\.(\d+)\.(\d+)$")


def version_key(value):
    match = VERSION.fullmatch(value)
    if not match:
        raise ValueError(f"Expected a stable version, got {value!r}")
    return tuple(map(int, match.groups()))


def git(root, *args):
    return subprocess.check_output(
        ["git", "-C", str(root), *args], text=True, timeout=30,
        stderr=subprocess.PIPE,
    ).strip()


def package_version(source):
    return tomllib.loads(source)["workspace"]["package"]["version"]


def read_catalog(root):
    path = root / CATALOG
    if not path.exists():
        return {"schema_version": 1, "upstream_repository": UPSTREAM,
                "fork_repository": FORK, "releases": []}
    catalog = json.loads(path.read_text())
    if catalog.get("schema_version") != 1:
        raise ValueError("Unsupported changelog schema")
    if catalog.get("upstream_repository") != UPSTREAM or catalog.get("fork_repository") != FORK:
        raise ValueError("Changelog belongs to a different repository")
    return catalog


def previous_release(root, tag):
    current = version_key(package_version(git(root, "show", f"{tag}:Cargo.toml")))
    # Release tags are not necessarily all present locally. Read the version
    # history that arrived with the fetched release instead of guessing N-1.
    for commit in git(root, "log", "-n", "100", "--first-parent", "--format=%H",
                      tag, "--", "Cargo.toml").splitlines():
        try:
            version = package_version(git(root, "show", f"{commit}:Cargo.toml"))
            if version_key(version) < current:
                return "v" + version, commit
        except (KeyError, ValueError, subprocess.CalledProcessError):
            continue
    return None


def release_body(tag, offline):
    if offline:
        return None
    try:
        data = json.loads(subprocess.check_output(
            ["gh", "api", f"repos/{UPSTREAM}/releases/tags/{tag}"],
            text=True, timeout=20, stderr=subprocess.PIPE,
        ))
        if not isinstance(data, dict):
            return None
        body = data.get("body") or ""
        if not isinstance(body, str):
            return None
        if (data.get("tag_name") != tag or data.get("draft") or data.get("prerelease")
                or not body.strip() or len(body.encode()) > MAX_NOTES_BYTES):
            return None
        return body.strip(), data.get("published_at")
    except (OSError, subprocess.SubprocessError, ValueError):
        return None


def commit_notes(root, base, head, first_parent=False):
    args = ["log", "--no-merges", "--format=%s", "-n", "80"]
    if first_parent:
        args.append("--first-parent")
    args.extend([f"{base}..{head}" if base else head, "--"])
    subjects = git(root, *args).splitlines()
    subjects = [s for s in subjects if not re.match(r"(?i)^bump (?:the )?version\b", s)]
    # JSON carries plain subjects; Markdown metacharacters are escaped for the
    # Git fallback, so an unusual commit title cannot become a heading or link.
    return list(dict.fromkeys(subjects))


def snapshot(root, tag, fork_head, offline=False):
    version_key(tag)
    tag = "v" + tag.removeprefix("v")
    upstream_commit = git(root, "rev-parse", f"{tag}^{{commit}}")
    version = tag.removeprefix("v")
    if package_version(git(root, "show", f"{upstream_commit}:Cargo.toml")) != version:
        raise ValueError("Release tag and upstream package version disagree")
    if package_version((root / "Cargo.toml").read_text()) != version:
        raise ValueError("Merge the release before preparing its notes")
    catalog = read_catalog(root)
    old = {entry["version"]: entry for entry in catalog["releases"]}

    def entry(release_tag, commit, base):
        release_version = release_tag.removeprefix("v")
        cached = old.get(release_version)
        if cached and cached["upstream_commit"] == commit:
            refreshed = dict(cached)
            if cached["notes_source"] == "git_history" and not offline:
                remote = release_body(release_tag, False)
                if remote:
                    refreshed.update(upstream_notes=remote[0], published_at=remote[1],
                                     notes_source="github_release")
            return refreshed
        remote = release_body(release_tag, offline)
        if remote:
            notes, published = remote
            source = "github_release"
        else:
            subjects = commit_notes(root, base, commit)
            notes = "\n".join("- " + re.sub(r"([\\`*_{}\[\]()<>#+.!|])", r"\\\1", s)
                              for s in subjects) or "Maintenance release."
            published = None
            source = "git_history"
        return {"version": release_version, "upstream_commit": commit,
                "upstream_url": f"https://github.com/{UPSTREAM}/releases/tag/{release_tag}",
                "published_at": published, "notes_source": source,
                "upstream_notes": notes, "fork_changes": [], "fork_through_commit": fork_head}

    predecessor = previous_release(root, tag)
    previous = None
    if predecessor:
        previous_tag, previous_commit = predecessor
        # Prefer the actual local release tag if available and contained in the
        # incoming release; the version-change commit is the offline fallback.
        try:
            tagged = git(root, "rev-parse", f"{previous_tag}^{{commit}}")
            git(root, "merge-base", "--is-ancestor", tagged, upstream_commit)
            previous_commit = tagged
        except subprocess.CalledProcessError:
            pass
        older = previous_release(root, previous_commit)
        previous = entry(previous_tag, previous_commit, older[1] if older else None)
    current = entry(tag, upstream_commit, previous["upstream_commit"] if previous else None)
    if version not in old:
        # Only newly added fork commits, never the entire upstream history or
        # every customization from every earlier release.
        # The last committed catalog represents an already described fork
        # build, including fork-only releases with the same upstream version.
        snapshots = git(root, "log", "--first-parent", "-n", "1", "--format=%H",
                        fork_head, "--", str(CATALOG)).splitlines()
        baseline = snapshots[0] if snapshots else (
            catalog["releases"][0].get("fork_through_commit") if catalog["releases"] else None)
        if baseline:
            try:
                git(root, "merge-base", "--is-ancestor", baseline, fork_head)
            except subprocess.CalledProcessError:
                baseline = None
        if not baseline:
            merges = git(root, "log", "--first-parent", "--merges", "-n", "1",
                         "--format=%H", fork_head).splitlines()
            baseline = merges[0] if merges else upstream_commit
        current["fork_changes"] = commit_notes(root, baseline, fork_head, first_parent=True)
        current["fork_through_commit"] = fork_head
    releases = [current] + ([previous] if previous else [])
    versions = {entry["version"] for entry in releases}
    releases.extend(entry for entry in catalog["releases"] if entry["version"] not in versions
                    and version_key(entry["version"]) < version_key(version))
    catalog["releases"] = releases[:20]
    path = root / CATALOG
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(catalog, indent=2, ensure_ascii=False) + "\n")
    return catalog


def check(root):
    catalog = read_catalog(root)
    version = package_version((root / "Cargo.toml").read_text())
    entries = catalog["releases"]
    if not entries or entries[0]["version"] != version:
        raise ValueError("Bundled changelog does not match this build; run scripts/snapshot-release-notes.py")
    versions = [version_key(entry["version"]) for entry in entries]
    if versions != sorted(set(versions), reverse=True):
        raise ValueError("Changelog versions must be unique and newest first")
    for entry in entries:
        if not re.fullmatch(r"[0-9a-f]{40}", entry["upstream_commit"]):
            raise ValueError("Invalid upstream commit")
        expected = f"https://github.com/{UPSTREAM}/releases/tag/v{entry['version']}"
        if entry["upstream_url"] != expected or entry["notes_source"] not in ("github_release", "git_history"):
            raise ValueError("Invalid release provenance")
        if not entry["upstream_notes"].strip() or len(entry["upstream_notes"].encode()) > MAX_NOTES_BYTES:
            raise ValueError("Missing or oversized release notes")
        if not isinstance(entry["fork_changes"], list) or not all(isinstance(s, str) for s in entry["fork_changes"]):
            raise ValueError("Invalid fork changes")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--tag")
    parser.add_argument("--fork-head", default="HEAD")
    parser.add_argument("--offline", action="store_true")
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    try:
        if args.check:
            check(ROOT)
            print("Bundled release notes match the Linux fork build.")
        else:
            tag = args.tag or "v" + package_version((ROOT / "Cargo.toml").read_text())
            result = snapshot(ROOT, tag, git(ROOT, "rev-parse", args.fork_head),
                              args.offline or os.environ.get("ZERON_RELEASE_NOTES_OFFLINE") == "1")
            print("Prepared release notes for " + ", ".join(e["version"] for e in result["releases"][:2]))
    except (OSError, subprocess.SubprocessError, ValueError, KeyError) as error:
        print(f"Release notes: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
