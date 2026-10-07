#!/usr/bin/env python3
"""Fail closed when an upstream merge introduces unguarded workflow jobs."""
from pathlib import Path
import re
import sys


UPSTREAM = "github.repository == 'zeronsh/zeron'"
MANUAL = ("github.repository == 'IIRoan/zeron' && github.ref == 'refs/heads/main'"
          " && github.event_name == 'workflow_dispatch'")


def section(text, name):
    match = re.search(rf"^{re.escape(name)}:\n(.*?)(?=^\S|\Z)", text, re.M | re.S)
    return match.group(1) if match else ""


def upstream_only(condition):
    if condition.startswith("${{") and condition.endswith("}}"):
        condition = condition[3:-2].strip()
    if condition == UPSTREAM:
        return True
    prefix = UPSTREAM + " && "
    if not condition.startswith(prefix):
        return False
    expression = condition[len(prefix):]
    if not expression.startswith("(") or not expression.endswith(")"):
        return False
    # The original condition must be fully enclosed, so a trailing || cannot
    # bypass the repository guard. Ignore parentheses inside GitHub literals.
    depth, quoted = 0, False
    for index, char in enumerate(expression):
        if char == "'":
            quoted = not quoted
        elif not quoted:
            depth += (char == "(") - (char == ")")
            if depth < 0 or (depth == 0 and index < len(expression) - 1):
                return False
    return depth == 0 and not quoted


def errors_for(path):
    text = path.read_text()
    jobs = section(text, "jobs")
    names = list(re.finditer(r"^  ([A-Za-z0-9_-]+):[ \t]*\n", jobs, re.M))
    entries = re.findall(r"^  (?!#)(\S[^\n]*)$", jobs, re.M)
    if (len(re.findall(r"^jobs:", text, re.M)) != 1 or not names
            or len(entries) != len(names)):
        return [f"{path.name}: expected jobs in the project's YAML format"]
    errors = []
    manual = path.name == "update-linux-fork.yml"
    if manual:
        events = re.findall(r"^  ([A-Za-z_]+):", section(text, "on"), re.M)
        if events != ["workflow_dispatch"]:
            errors.append(f"{path.name}: the fork updater must be manually triggered only")
        if section(text, "permissions").strip() != "contents: read":
            errors.append(f"{path.name}: the fork updater must have read-only token permissions")
    for index, name in enumerate(names):
        end = names[index + 1].start() if index + 1 < len(names) else len(jobs)
        block = jobs[name.end():end]
        conditions = re.findall(r"^    if: (.+)$", block, re.M)
        valid = len(conditions) == 1 and (
            conditions[0].strip() == MANUAL if manual else upstream_only(conditions[0].strip())
        )
        if not valid:
            expected = MANUAL if manual else UPSTREAM
            errors.append(f"{path.name}/{name.group(1)}: requires repository guard {expected}")
        if manual and re.search(r"^    permissions:", block, re.M):
            errors.append(f"{path.name}/{name.group(1)}: cannot override read-only permissions")
    return errors


def check(directory):
    paths = sorted([*directory.glob("*.yml"), *directory.glob("*.yaml")])
    if not (directory / "update-linux-fork.yml").is_file():
        return ["Missing the manually triggered Linux fork updater"]
    return [error for path in paths for error in errors_for(path)]


if __name__ == "__main__":
    failures = check(Path(__file__).resolve().parents[1] / ".github/workflows")
    if failures:
        print("Fork workflow isolation check failed:\n" + "\n".join(failures), file=sys.stderr)
        sys.exit(1)
    print("Fork workflow isolation verified: upstream jobs are guarded; updater is manual and read-only.")
