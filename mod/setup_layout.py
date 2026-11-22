#!/usr/bin/env python3
"""Puts Bots into Herdr's window. Run by install.sh; safe to run again.

Herdr's own layouts and mods are read-only (replaced on every start), so this works on copies:

* layouts/main-bots.json  a copy of `main` (made with `herdr-app do layout.clone` beforehand)
  - sidebar: a Bots list above Agents            (block `bots`, mod bots {sidebar: true})
  - right panel: a bot's chat beside Files/Changes (block `bots-chat`, mod bots {chat: true})
  - title bar: uses `tabs-bots` instead of `tabs`
* mods/tabs-bots/         a fresh copy of Herdr's current `tabs` mod on every run, with a
  "Bots" button next to Files and Changes. Re-copying keeps it in step with Herdr updates.

Usage: setup_layout.py <herdr app home>
"""

import json
import shutil
import sys
from pathlib import Path

PANEL = "bots-chat"

# The two places in Herdr's tabs mod that know the right-panel buttons. If Herdr changes them,
# the copy is left unpatched (and the layout keeps the stock `tabs`) instead of breaking.
ENABLED_OLD = 'const enabled = shared("workspace.panels") ?? ["files", "changes"];'
ENABLED_NEW = 'const enabled = [...(shared("workspace.panels") ?? ["files", "changes"]), "bots-chat"];'
BUTTON_ANCHOR = '.when(enabled.includes("changes"), el => el.child(panelButton("changes", "Changes", "mods/changes/changes.svg")))'
BUTTON_NEW = BUTTON_ANCHOR + '\n      .child(panelButton("bots-chat", "Bots", "mods/bots/bots.svg"))'


def find(node, id_):
    if isinstance(node, dict):
        if node.get("id") == id_:
            return node
        for child in node.get("children", []):
            hit = find(child, id_)
            if hit:
                return hit
    return None


def make_tabs_copy(app: Path) -> bool:
    src, dst = app / "mods" / "tabs", app / "mods" / "tabs-bots"
    main = (src / "main.js").read_text()
    if ENABLED_OLD not in main or BUTTON_ANCHOR not in main:
        print("note: Herdr's tabs mod changed shape; the Bots button was not added", file=sys.stderr)
        return False
    dst.mkdir(exist_ok=True)
    for f in src.iterdir():
        if f.is_file():
            shutil.copy2(f, dst / f.name)
    header = "// Copied from Herdr's `tabs` by herdr-bots install.sh (adds the Bots panel button). Re-copied on every install.\n"
    (dst / "main.js").write_text(header + main.replace(ENABLED_OLD, ENABLED_NEW).replace(BUTTON_ANCHOR, BUTTON_NEW))
    return True


def patch_layout(path: Path, tabs_ok: bool) -> None:
    doc = json.loads(path.read_text())
    root = doc.get("root", doc)

    lists = find(root, "sidebar-lists")
    if lists is not None and not find(lists, "bots"):
        ids = [c.get("id") for c in lists["children"]]
        at = ids.index("agents") if "agents" in ids else len(ids)
        lists["children"].insert(at, {"id": "bots", "type": "block", "size": {"px": 160}, "min": 60,
                                      "content": {"mod": "bots", "props": {"sidebar": True}}})

    body = find(root, "panel-body")
    if body is not None and not find(body, PANEL):
        body["children"].append({"id": PANEL, "type": "block", "content": {"mod": "bots", "props": {"chat": True}}})

    tabs = find(root, "tabs")
    if tabs is not None and tabs_ok:
        tabs.setdefault("content", {})["mod"] = "tabs-bots"

    if lists is None or body is None or tabs is None:
        print("note: main-bots lacks sidebar-lists/panel-body/tabs; add the bots blocks by hand", file=sys.stderr)
    path.write_text(json.dumps(doc, indent=2) + "\n")


def main() -> None:
    app = Path(sys.argv[1]).expanduser()
    tabs_ok = make_tabs_copy(app)
    patch_layout(app / "layouts" / "main-bots.json", tabs_ok)


if __name__ == "__main__":
    main()
