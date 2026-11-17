#!/usr/bin/env python3
"""Carry an upstream merge into Zeron Beta's fork plugins and its zgx fork.

scripts/update-upstream.sh runs this after merging a release:

* Each fork plugin file that copies an upstream file
  (apps/zgx/plugins-fork/upstream-copies.txt) is three-way merged with
  `git merge-file`: base = the upstream file at the copy's recorded commit,
  theirs = the file at --to, ours = the copy. --to becomes its recorded
  commit. A conflict is left as git leaves one: markers in the file and its
  three versions in the index, so git status lists it and git commit waits
  for `git add` (`git checkout -m <file>` starts it over).
* The plugins' SDK copies are refreshed from apps/zgx/plugins-sdk and
  apps/zgx/plugins-fork.
* If upstream's zgx pin in apps/zgx/Cargo.toml isn't in the zgx fork's
  commit, the fork's branch is rebased onto it (the old commit kept as
  backup/<branch>-<commit>), Cargo.toml's [patch] takes the new commit and
  Cargo.lock follows.

Re-running is safe: a copy already at --to and a fork already on the pin are
left alone. --check changes nothing and fails on a copy without a recorded
base, an unresolved copy, or a zgx fork that misses upstream's pin.
"""
import argparse
from pathlib import Path
import re
import shutil
import subprocess
import tempfile
import tomllib

ROOT = Path(__file__).resolve().parents[1]
APP = ROOT / "apps/zgx"
FORK = APP / "plugins-fork"
MANIFEST = FORK / "upstream-copies.txt"
SDK = {"zeron.luau": APP / "plugins-sdk/zeron.luau",
       "fork_sdk.luau": FORK / "fork_sdk.luau",
       "fork_motion.luau": FORK / "fork_motion.luau"}
MARKER = re.compile(rb"^(<{7}|>{7}) ", re.M)


def run(*args, cwd=ROOT, input=None):
    return subprocess.run(args, cwd=cwd, input=input, capture_output=True)


def git(*args, cwd=ROOT, input=None):
    result = run("git", *args, cwd=cwd, input=input)
    if result.returncode:
        raise SystemExit(f"git {' '.join(map(str, args))} failed: {result.stderr.decode().strip()}")
    return result.stdout.decode().strip()


def rel(path):
    return str(path.relative_to(ROOT))


def describe(rev):
    return run("git", "describe", "--tags", "--always", rev).stdout.decode().strip() or rev[:12]


def entries():
    """(line index, copy, upstream, base) for each line of the manifest."""
    for index, line in enumerate(MANIFEST.read_text().splitlines()):
        fields = line.split("#")[0].split()
        if fields:
            if len(fields) != 3:
                raise SystemExit(f"{rel(MANIFEST)}:{index + 1}: expected <copy> <upstream> <base>")
            yield index, *fields


def upstream_file(rev, path):
    result = run("git", "show", f"{rev}:apps/zgx/{path}")
    if result.returncode:
        return None
    if path.startswith("ui/ds/"):
        return result.stdout.replace(b'require("@self/', b'require("@ds/')
    return result.stdout


def unmerged(path):
    return bool(git("ls-files", "--unmerged", "--", rel(path)))


def leave_conflict(path, base, ours, theirs):
    """Put a copy's three versions in the index, as git merge does."""
    blob = lambda data: git("hash-object", "-w", "--stdin", input=data)
    name = rel(path)
    info = f"0 {'0' * 40}\t{name}\n" + "".join(
        f"100644 {blob(data)} {stage}\t{name}\n" for stage, data in ((1, base), (2, ours), (3, theirs)))
    git("update-index", "--index-info", input=info.encode())


def sync_copies(to, report):
    lines = MANIFEST.read_text().splitlines()
    clean = []
    for index, copy, upstream, base in entries():
        path = FORK / copy
        if unmerged(path):
            report["conflicts"].append(copy)
            continue
        if base == to:
            continue
        old, new = upstream_file(base, upstream), upstream_file(to, upstream)
        if old is None:
            report["problems"].append(f"{copy}: no apps/zgx/{upstream} at its recorded base {base[:12]}")
            continue
        if new is None:
            report["problems"].append(
                f"{copy}: upstream has no apps/zgx/{upstream} at {describe(to)} (moved or removed?); "
                f"merge it by hand and fix its line in {rel(MANIFEST)}")
            continue
        if not path.is_file():
            report["problems"].append(f"{copy}: missing")
            continue
        if old != new:
            ours = path.read_bytes()
            with tempfile.TemporaryDirectory() as temp:
                (Path(temp) / "base").write_bytes(old)
                (Path(temp) / "theirs").write_bytes(new)
                merged = run("git", "merge-file", "-p", "-L", copy, "-L", f"upstream {base[:12]}",
                             "-L", f"upstream {to[:12]}", path, Path(temp) / "base", Path(temp) / "theirs")
            if merged.returncode > 127:
                report["problems"].append(f"{copy}: git merge-file failed: {merged.stderr.decode().strip()}")
                continue
            path.write_bytes(merged.stdout)
            if merged.returncode:
                leave_conflict(path, old, ours, new)
                report["conflicts"].append(copy)
            else:
                clean.append(path)
                report["merged"].append(copy)
        lines[index] = lines[index][:lines[index].rindex(base)] + to
    MANIFEST.write_text("\n".join(lines) + "\n")
    git("add", "--", rel(MANIFEST), *map(rel, clean))


def refresh_sdk(report):
    changed = []
    for name, canonical in SDK.items():
        data = canonical.read_bytes()
        if MARKER.search(data):
            report["problems"].append(f"{rel(canonical)} has conflict markers: resolve it, then run this again")
            continue
        for copy in sorted(FORK.glob(f"*/**/{name}")):
            if copy.read_bytes() != data:
                copy.write_bytes(data)
                changed.append(copy)
    if changed:
        git("add", "--", *map(rel, changed))
        report["sdk"] = [str(p.relative_to(FORK)) for p in changed]


def zgx_patch(report):
    """(upstream url, pin, fork repo, fork commit) from apps/zgx/Cargo.toml, or None."""
    if not (APP / "Cargo.toml").exists():
        return None
    try:
        manifest = tomllib.loads((APP / "Cargo.toml").read_text())
    except tomllib.TOMLDecodeError as error:
        report["problems"].append(f"apps/zgx/Cargo.toml doesn't parse ({error}): resolve it, then run this again")
        return None
    patches = {url: table for url, table in manifest.get("patch", {}).items() if url != "crates-io"}
    if len(patches) != 1:
        if patches:
            report["problems"].append(f"apps/zgx/Cargo.toml: expected one zgx [patch], found {sorted(patches)}")
        return None
    (url, table), = patches.items()
    forks = {(entry.get("git"), entry.get("rev")) for entry in table.values()}
    pins = set()

    def walk(value):
        if isinstance(value, dict):
            if value.get("git") == url:
                pins.add(value.get("rev"))
            for item in value.values():
                walk(item)
    walk(manifest)
    if len(forks) != 1 or len(pins) != 1 or None in pins or not next(iter(forks))[0].startswith("file://"):
        report["problems"].append(f"apps/zgx/Cargo.toml: expected one upstream zgx rev and one fork "
                                  f"file:// rev, found {sorted(map(str, pins))} and {sorted(map(str, forks))}")
        return None
    (fork_url, fork), = forks
    return url, pins.pop(), Path(fork_url.removeprefix("file://")), fork


def contains(repo, ancestor, commit):
    return run("git", "merge-base", "--is-ancestor", ancestor, commit, cwd=repo).returncode == 0


def checked_out(repo, branch):
    """The worktree that has the branch checked out, if one does."""
    path = None
    for line in git("worktree", "list", "--porcelain", cwd=repo).splitlines():
        if line.startswith("worktree "):
            path = Path(line.removeprefix("worktree "))
        elif line == f"branch refs/heads/{branch}":
            return path
    return None


def sync_zgx(branch, report):
    """Rebase the zgx fork onto upstream's pin when it moved; True when the fork's commit changed."""
    patch = zgx_patch(report)
    if not patch:
        return False
    url, pin, repo, fork = patch
    if not (repo / ".git").exists():
        report["zgx"] = f"skipped: the zgx fork ({repo}) isn't on this machine"
        return False
    if run("git", "cat-file", "-e", f"{pin}^{{commit}}", cwd=repo).returncode:
        fetched = run("git", "fetch", "--no-tags", url, pin, cwd=repo)
        if fetched.returncode:
            report["problems"].append(f"zgx: can't fetch upstream's pin {pin[:12]} from {url}: "
                                      f"{fetched.stderr.decode().strip()}")
            return False
    if contains(repo, pin, fork):
        report["zgx"] = f"fork {fork[:12]} already contains upstream's pin {pin[:12]}"
        return False
    tip = run("git", "rev-parse", "--verify", "--quiet", f"refs/heads/{branch}", cwd=repo).stdout.decode().strip()
    spare = ROOT / "target" / f"zgx-rebase-{branch}"
    if tip and tip != fork and contains(repo, pin, tip):
        new = tip  # Rebased already: by hand, or by a run that stopped on conflicts.
    elif tip != fork:
        report["problems"].append(
            f"zgx: Cargo.toml takes {fork[:12]}, but branch {branch} of {repo} is at {tip[:12] or 'nothing'} "
            f"and neither contains upstream's pin {pin[:12]}. Rebase it onto the pin "
            f"(git -C {repo} rebase --onto {pin[:12]} <old pin> {branch}), then run this again.")
        return False
    else:
        old_base = git("merge-base", fork, pin, cwd=repo)
        commits = "\n".join("    " + line for line in git(
            "log", "--reverse", "--format=%h %s", f"{old_base}..{fork}", cwd=repo).splitlines())
        backup = f"backup/{branch}-{fork[:12]}"
        if run("git", "rev-parse", "--verify", "--quiet", f"refs/heads/{backup}", cwd=repo).returncode:
            git("branch", backup, fork, cwd=repo)
        where = checked_out(repo, branch)
        if where is None:
            if spare.exists():
                report["problems"].append(
                    f"zgx: {spare} is left from an earlier rebase: finish it (git -C {spare} rebase --continue) "
                    f"or drop it (git -C {spare} rebase --abort; git -C {repo} worktree remove {spare}), "
                    "then run this again.")
                return False
            added = run("git", "worktree", "add", "--quiet", spare, branch, cwd=repo)
            if added.returncode:
                report["problems"].append(f"zgx: can't check out {branch} to rebase it (a rebase under way "
                                          f"elsewhere?): {added.stderr.decode().strip()}")
                return False
            where = spare
        rebased = run("git", "rebase", "--onto", pin, old_base, cwd=where)
        if rebased.returncode:
            said = [line for line in (rebased.stdout + rebased.stderr).decode().splitlines()
                    if line.strip() and not line.startswith("hint:")]
            report["problems"].append(
                f"zgx: rebasing the fork's commits onto upstream's pin {pin[:12]} stopped in {where}:\n"
                + "\n".join(f"    {line}" for line in said[-6:]) +
                f"\n  Resolve it there and `git -C {where} rebase --continue`, then run this again. "
                f"(The old fork is {backup}; `git -C {where} rebase --abort` gives up.) Its commits:\n{commits}")
            return False
        new = git("rev-parse", f"refs/heads/{branch}", cwd=repo)
        report["zgx_commits"] = commits
    if spare.exists() and checked_out(repo, branch) == spare:
        run("git", "worktree", "remove", spare, cwd=repo)
    cargo = APP / "Cargo.toml"
    text, out, inside = cargo.read_text().splitlines(keepends=True), [], False
    for line in text:
        if line.startswith("["):
            inside = line.strip() == f'[patch."{url}"]'
        out.append(line.replace(f'rev = "{fork}"', f'rev = "{new}"') if inside else line)
    cargo.write_text("".join(out))
    git("add", "--", rel(cargo))
    report["zgx"] = f"fork rebased onto upstream's pin {pin[:12]}: {fork[:12]} -> {new[:12]} (branch {branch})"
    return True


def refresh_lock(report):
    """Cargo.lock as cargo resolves it with the patch: upstream's, if git couldn't merge it."""
    lock = APP / "Cargo.lock"
    if not lock.exists():
        return
    if not shutil.which("cargo"):
        report["problems"].append("apps/zgx/Cargo.lock needs `cargo update --workspace` (no cargo here)")
        return
    if unmerged(lock):
        git("checkout", "--theirs", "--", rel(lock))
    updated = run("cargo", "update", "--workspace", "--manifest-path", APP / "Cargo.toml")
    if updated.returncode:
        report["problems"].append(f"cargo update --workspace in apps/zgx failed:\n{updated.stderr.decode()[-2000:]}")
        return
    git("add", "--", rel(lock))
    report["lock"] = "apps/zgx/Cargo.lock follows the zgx fork"


def check(report):
    for _, copy, upstream, base in entries():
        path = FORK / copy
        if not re.fullmatch(r"[0-9a-f]{40}", base) or upstream_file(base, upstream) is None:
            report["problems"].append(f"{copy}: no recorded base with apps/zgx/{upstream} in it ({base})")
        if not path.is_file():
            report["problems"].append(f"{copy}: missing")
        elif unmerged(path) or MARKER.search(path.read_bytes()):
            report["conflicts"].append(copy)
    patch = zgx_patch(report)
    if patch:
        url, pin, repo, fork = patch
        lock = (APP / "Cargo.lock").read_text() if (APP / "Cargo.lock").exists() else None
        if lock is not None and (f'source = "git+file://{repo}?rev={fork}#{fork}"' not in lock
                                 or "[[patch.unused]]" in lock):
            report["problems"].append("apps/zgx/Cargo.lock doesn't build the zgx fork's commit: "
                                      "run `cargo update --workspace` in apps/zgx")
        if (repo / ".git").exists() and not contains(repo, pin, fork):
            report["problems"].append(f"zgx: fork {fork[:12]} doesn't contain upstream's pin {pin[:12]}: "
                                      "run scripts/sync-zgx-fork.py")


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--to", help="the upstream commit just merged (a tag, branch or SHA); "
                                     "without it, only the SDK copies and the zgx fork are synced")
    parser.add_argument("--zgx-branch", default="zeron-fork", help="the zgx fork's branch (default: zeron-fork)")
    parser.add_argument("--check", action="store_true", help="only verify")
    args = parser.parse_args()
    report = {"merged": [], "conflicts": [], "problems": []}
    if args.check:
        check(report)
    else:
        if args.to:
            to = git("rev-parse", "--verify", f"{args.to}^{{commit}}")
            sync_copies(to, report)
            print(f"Fork plugin copies: synced to upstream {describe(to)} ({to[:12]})")
        refresh_sdk(report)
        lock = APP / "Cargo.lock"
        if sync_zgx(args.zgx_branch, report) or (lock.exists() and unmerged(lock)
                                                  and not unmerged(APP / "Cargo.toml")):
            refresh_lock(report)
    for copy in report["merged"]:
        print(f"  merged upstream's changes: {copy}")
    if report.get("sdk"):
        print("  SDK copies refreshed: " + ", ".join(report["sdk"]))
    for key in ("zgx", "zgx_commits", "lock"):
        if report.get(key):
            print(f"  {report[key]}" if key != "zgx_commits" else f"  fork commits carried:\n{report[key]}")
    for copy in report["conflicts"]:
        print(f"  CONFLICT: apps/zgx/plugins-fork/{copy}")
    for problem in report["problems"]:
        print(f"  PROBLEM: {problem}")
    if report["conflicts"]:
        print("Resolve each CONFLICT (keep the fork's change and upstream's), then `git add` it.")
    if report["conflicts"] or report["problems"]:
        raise SystemExit(1)
    print("Zeron Beta copies are in step." if args.check else "Zeron Beta is in step with the merge.")


if __name__ == "__main__":
    main()
