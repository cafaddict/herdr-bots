#!/usr/bin/env python3
"""Adds a herdr-bots daemon on another machine to a Herdr window's Bots panel.

The panel always shows the daemon in config.js (the one install.sh set up). This lists more in
mods/bots/daemons.json, so bots from several machines show in one panel. Safe to run again: an
entry for the same machine is replaced.

Usage: add_daemon.py <herdr app home> <machine> <herdr-bots path there> <state folder there>
       add_daemon.py <herdr app home> --remove <machine>

<machine> is the name the window gives that machine (`hc machine ls` on the window's side).
Example, for a daemon on dev-server installed with ./install.sh there:

    add_daemon.py /mnt/c/Users/me/.config/herdr-app dev-server \\
        /home/me/.cargo/bin/herdr-bots /home/me/.herdr-bots/state
"""

import json
import os
import sys
from pathlib import Path


def main() -> int:
    args = sys.argv[1:]
    if len(args) == 3 and args[1] == "--remove":
        app, machine, entry = Path(args[0]).expanduser(), args[2], None
    elif len(args) == 4:
        app, machine = Path(args[0]).expanduser(), args[1]
        bin_path, state_dir = args[2], args[3]
        if not (bin_path.startswith("/") and state_dir.startswith("/")):
            print("the herdr-bots path and the state folder must be absolute paths on that machine", file=sys.stderr)
            return 2
        entry = {"machine": machine, "bin": bin_path, "state_dir": state_dir}
    else:
        print(__doc__, file=sys.stderr)
        return 2

    mod = app / "mods" / "bots"
    if not mod.is_dir():
        print(f"no Bots mod in {mod}; run install.sh for this window first", file=sys.stderr)
        return 1
    path = mod / "daemons.json"
    doc = json.loads(path.read_text()) if path.exists() else {}
    daemons = [d for d in doc.get("daemons", []) if d.get("machine") != machine]
    if entry:
        daemons.append(entry)
    doc["daemons"] = daemons
    tmp = path.with_suffix(".json.tmp")
    tmp.write_text(json.dumps(doc, indent=2) + "\n")
    os.replace(tmp, path)
    print(f"{'added' if entry else 'removed'} {machine} in {path} ({len(daemons)} extra daemon(s))")
    return 0


if __name__ == "__main__":
    sys.exit(main())
