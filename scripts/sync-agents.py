#!/usr/bin/env python3
"""Write crates/devstack-skills/agents.toml from vercel-labs/skills' agent table.

devstack keeps the coding agents' user skill folders, and how each agent is
told installed, as data taken from `src/agents.ts` of
https://github.com/vercel-labs/skills (MIT, Copyright (c) Vercel, Inc.):
the skill folders a pool's sets go to, and the agents "Open with" knows
(ADR-0074). Run it to take a newer table:

    gh api repos/vercel-labs/skills/contents/src/agents.ts --jq .content | base64 -d > agents.ts
    python3 scripts/sync-agents.py agents.ts

Only agents with a user folder made of a base and fixed parts are taken; one
found another way (a function, the current directory) is left out. What
devstack adds to an agent (`[agent.open]`, how "Open with" starts it) is kept
from the file as it was.
"""

import re
import sys
from pathlib import Path

OUT = Path(__file__).resolve().parent.parent / "crates" / "devstack-skills" / "agents.toml"

# The bases `agents.ts` joins paths to, as devstack writes them: `~` is the
# user's home, `${VAR:-default}` a variable with its default.
BASES = {"home": "~", "configHome": "${XDG_CONFIG_HOME:-~/.config}"}


def bases(src: str) -> dict:
    b = dict(BASES)
    for name, var, rest in re.findall(
        r"const (\w+) = process\.env\.(\w+)\?\.trim\(\) \|\| join\(home, ([^)]*)\);", src
    ):
        b[name] = "${%s:-~/%s}" % (var, "/".join(parts(rest)))
    return b


def parts(args: str) -> list:
    return [p.strip("/") for p in re.findall(r"'([^']*)'", args)]


def path(expr: str, b: dict):
    """`join(base, 'a', 'b')` or `base` as a devstack path; None otherwise."""
    expr = expr.strip()
    lit = re.fullmatch(r"'(/[^']*)'", expr)
    if lit:
        return lit.group(1)
    m = re.fullmatch(r"join\((\w+)((?:,\s*'[^']*')*)\)", expr)
    if m:
        base, rest = m.group(1), m.group(2)
    elif re.fullmatch(r"\w+", expr):
        base, rest = expr, ""
    else:
        return None
    if base not in b:
        return None
    return "/".join([b[base]] + [p for p in parts(rest) if p])


def agents(src: str):
    b = bases(src)
    table = src[src.index("export const agents") :]
    for m in re.finditer(r"\n  '?([a-z0-9-]+)'?: \{\n(.*?)\n  \},", table, re.S):
        aid, body = m.group(1), m.group(2)
        name = re.search(r"displayName: '([^']*)'", body)
        skills = re.search(r"globalSkillsDir: ([^\n]*?),?\n", body)
        detect = re.search(r"detectInstalled: async \(\) => \{(.*?)\n    \}", body, re.S)
        if not (name and skills and detect):
            continue
        skills_path = path(skills.group(1), b)
        found = [path(e, b) for e in re.findall(r"existsSync\(((?:[^()]|\([^()]*\))*)\)", detect.group(1))]
        if not skills_path or not found or None in found:
            continue
        yield aid, name.group(1), skills_path, found


def kept(old: str) -> dict:
    """The `[agent.open]` blocks of the file as it is, by agent id."""
    out = {}
    for block in re.split(r"\n(?=\[\[agent\]\])", old):
        m = re.search(r'^id = "([^"]+)"', block, re.M)
        o = re.search(r"(\[agent\.open\].*?)(?=\n\n|\Z)", block, re.S)
        if m and o:
            out[m.group(1)] = o.group(1).strip()
    return out


def own(old: str) -> list:
    """devstack's own `[[agent]]` blocks (`own = true`), kept as they are."""
    blocks = re.split(r"\n\n+", old)
    return [b.strip() for b in blocks if re.search(r"^own = true$", b, re.M)]


def main():
    src = Path(sys.argv[1]).read_text(encoding="utf-8")
    old = OUT.read_text(encoding="utf-8") if OUT.exists() else ""
    opens = kept(old)
    mine = own(old)
    lines = [
        "# The coding agents devstack knows (ADR-0074): each one's user skill folder,",
        "# and the paths that tell it installed. Taken from src/agents.ts of",
        "# https://github.com/vercel-labs/skills (MIT, Copyright (c) Vercel, Inc.)",
        "# by scripts/sync-agents.py; `[agent.open]` is devstack's own (how",
        "# \"Open with\" starts the agent) and kept when the table is taken again.",
        "# `~` is the user's home, `${VAR:-default}` a variable or its default.",
        "",
    ]
    n = 0
    for aid, name, skills, detect in agents(src):
        n += 1
        lines += [
            "[[agent]]",
            f'id = "{aid}"',
            f'name = "{name}"',
            f'skills = "{skills}"',
            "detect = [" + ", ".join(f'"{d}"' for d in detect) + "]",
        ]
        if aid in opens:
            lines.append(opens[aid])
        lines.append("")
    for block in mine:
        lines += [block, ""]
    OUT.write_text("\n".join(lines), encoding="utf-8", newline="\n")
    print(f"{n} agents written to {OUT}")


if __name__ == "__main__":
    main()
