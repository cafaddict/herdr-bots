#!/usr/bin/env bash
# Installs herdr-bots: the binary (cargo), the daemon service (launchd / systemd --user) and the
# Herdr panel mod. Safe to run again: it updates each piece in place and keeps your bots.
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
app_home=${HERDR_APP_HOME:-$HOME/.config/herdr-app}
# The Herdr window may run on another machine than the daemon, e.g. the Windows app with the
# daemon in WSL. Then point HERDR_APP_HOME at that app's folder, HERDR_APP_BIN at its herdr-app,
# and set HERDR_BOTS_MACHINE to the name the window gives this machine (`hc machine ls` there).
app_bin=${HERDR_APP_BIN:-herdr-app}
machine=${HERDR_BOTS_MACHINE:-}
layout_name=${HERDR_BOTS_LAYOUT:-main-bots}
bots_home=${HERDR_BOTS_HOME:-$HOME/.herdr-bots}

# herdr-app.exe run from WSL hangs on an inherited stdin and wants Windows paths.
app() { "$app_bin" "$@" </dev/null; }
app_path() {
  if [[ "$app_bin" == *.exe ]] && command -v wslpath >/dev/null 2>&1; then wslpath -w "$1"; else printf '%s\n' "$1"; fi
}
have_app() { command -v "$app_bin" >/dev/null 2>&1 || [ -x "$app_bin" ]; }

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
  cp "$here/mod/bots/main.js" "$here/mod/bots/mod.json" "$here/mod/bots/bots.svg" "$app_home/mods/bots/"
  if [ -n "$machine" ]; then
    machine_js="\"$machine\""; state_js="\"$bots_home/state\""
  else
    machine_js=null; state_js=null
  fi
  {
    echo "// Written by install.sh: the herdr-bots binary. Edit if you move it."
    echo "export const BIN = \"$bin\";"
    echo "// The machine the daemon runs on, as this Herdr window names it, or null for this computer."
    echo "export const MACHINE = $machine_js;"
    echo "// The daemon's state folder on that machine, or null for ~/.herdr-bots/state here."
    echo "export const STATE_DIR = $state_js;"
  } > "$app_home/mods/bots/config.js"
  if have_app; then app check "$(app_path "$app_home/mods/bots")"; fi
else
  echo "note: $app_home not found; skipping the panel (run again after installing Herdr)" >&2
fi

# Herdr's own layouts and mods are read-only, so Bots lives in copies: a layout (`main-bots`,
# made once from `main`; or your own layout named in HERDR_BOTS_LAYOUT) and mod `tabs-bots`
# (re-copied each run). See mod/setup_layout.py.
if [ -d "$app_home" ] && have_app && command -v python3 >/dev/null 2>&1; then
  layout="$app_home/layouts/$layout_name.json"
  if [ ! -f "$layout" ]; then
    echo "==> making layout $layout_name (a copy of Herdr's main)"
    app do layout.clone "{\"name\":\"main\",\"as\":\"$layout_name\"}" >/dev/null
  fi
  echo "==> adding Bots to $layout_name (sidebar list, Bots button and right-panel chat)"
  python3 "$here/mod/setup_layout.py" "$app_home" "$layout_name"
  app do layout.switch "{\"name\":\"$layout_name\"}" >/dev/null || true
fi

echo "==> registering and starting the daemon"
"$bin" install-service

echo
echo "done. next:"
echo "  herdr-bots new <id>      # make a bot, then edit ~/.herdr-bots/bots/<id>/SOUL.md and bot.toml"
echo "  herdr-bots status"
