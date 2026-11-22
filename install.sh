#!/usr/bin/env bash
# Installs herdr-bots: the binary (cargo), the daemon service (launchd / systemd --user) and the
# Herdr panel mod. Safe to run again: it updates each piece in place and keeps your bots.
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
app_home=${HERDR_APP_HOME:-$HOME/.config/herdr-app}

need() { command -v "$1" >/dev/null 2>&1 || { echo "missing: $1 ($2)" >&2; exit 1; }; }
need cargo "https://rustup.rs"
need hc "herdr-core; install the Herdr app first"
command -v gh >/dev/null 2>&1 || echo "note: gh not found; GitHub triggers stay off" >&2

echo "==> building and installing the binary"
cargo install --path "$here" --locked --force --quiet
bin=$(command -v herdr-bots || echo "$HOME/.cargo/bin/herdr-bots")
[ -x "$bin" ] || { echo "herdr-bots was not installed where expected ($bin)" >&2; exit 1; }

echo "==> installing the Herdr panel mod into $app_home/mods/bots"
if [ -d "$app_home" ]; then
  mkdir -p "$app_home/mods/bots"
  cp "$here/mod/bots/main.js" "$here/mod/bots/mod.json" "$app_home/mods/bots/"
  printf '// Written by install.sh: the herdr-bots binary. Edit if you move it.\nexport const BIN = %s;\n' \
    "\"$bin\"" > "$app_home/mods/bots/config.js"
  if command -v herdr-app >/dev/null 2>&1; then herdr-app check "$app_home/mods/bots"; fi
else
  echo "note: $app_home not found; skipping the panel (run again after installing Herdr)" >&2
fi

# Herdr's own layouts are read-only, so Bots lives in a clone of `main` called `main-bots`:
# a list above Agents in the sidebar, and each bot's chat in the right panel beside
# Files/Changes. The clone is made once; adding the blocks is repeated (and skipped when there).
if [ -d "$app_home" ] && command -v herdr-app >/dev/null 2>&1 && command -v python3 >/dev/null 2>&1; then
  layout="$app_home/layouts/main-bots.json"
  if [ ! -f "$layout" ]; then
    echo "==> making layout main-bots (a copy of Herdr's main)"
    herdr-app do layout.clone '{"name":"main","as":"main-bots"}' >/dev/null
  fi
  echo "==> adding Bots to main-bots (sidebar list, right-panel chat)"
  python3 - "$layout" <<'PY'
import json, sys
path = sys.argv[1]
doc = json.load(open(path))
def find(node, id):
    if isinstance(node, dict):
        if node.get("id") == id:
            return node
        for child in node.get("children", []):
            hit = find(child, id)
            if hit:
                return hit
root = doc.get("root", doc)
changed = False
lists = find(root, "sidebar-lists")
ids = [c.get("id") for c in lists["children"]] if lists else []
if lists and "bots" not in ids:
    at = ids.index("agents") if "agents" in ids else len(ids)
    lists["children"].insert(at, {"id": "bots", "type": "block", "size": {"px": 160}, "min": 60,
                                  "content": {"mod": "bots", "props": {"sidebar": True}}})
    changed = True
body = find(root, "panel-body")
if body and not find(body, "bots-chat"):
    body["children"].append({"id": "bots-chat", "type": "block",
                             "content": {"mod": "bots", "props": {"chat": True}}})
    changed = True
if not lists or not body:
    print("note: main-bots has no sidebar-lists/panel-body; add the bots blocks by hand", file=sys.stderr)
if changed:
    json.dump(doc, open(path, "w"), indent=2)
PY
  herdr-app do layout.switch '{"name":"main-bots"}' >/dev/null || true
fi

echo "==> registering and starting the daemon"
"$bin" install-service

echo
echo "done. next:"
echo "  herdr-bots new <id>      # make a bot, then edit ~/.herdr-bots/bots/<id>/SOUL.md and bot.toml"
echo "  herdr-bots status"
