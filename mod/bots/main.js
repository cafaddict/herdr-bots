// Bots: the herdr-bots daemon's bots, their work items and workers.
//
// One mod, three shapes (chosen by props):
//   { sidebar: true }  a compact list for the left sidebar, above Agents; a click opens the chat
//   { chat: true }     the right-hand panel (beside Files/Changes): the bot in shared "bots.chat"
//   { chat: "<bot>" }  the same conversation as a window, for layouts without that panel
//   { newBot: true }   the "new bot" dialog
//   { deleteBot: id }  the "delete bot?" dialog
//   { renameBot: id }  the "rename bot" dialog
//   (none)             everything at once, as a window (`mod.open bots`)
//
// The daemon writes ~/.herdr-bots/state/status.json on every change; this mod watches that
// folder and calls the `herdr-bots` CLI for actions. A bot's live agent opens as a window.
//
// The daemon may run on another machine than this window: a WSL distro or a server the window
// reaches over hc (`hc machine ls`). config.js then names it in MACHINE, with the daemon's
// state folder in STATE_DIR, and every file read and command goes there.
//
// More daemons, say one per machine, can be listed in daemons.json next to this file
// (mod/add_daemon.py). Their bots then show together; each bot and item remembers its daemon,
// and with more than one daemon a bot's key is "<machine>/<id>".

import { View, div } from "gpui-kit";
import { v_flex, h_flex, InputState, Input, TextView, ScrollHandle, EditorState, Editor } from "gpui-base";
import {
  exec, home, appHome, checking, readTextFile, watchFolder, terminals, openOverlay, closeOverlay,
  notify, setBadge, playSound, props, theme, layout, run, shared, setShared,
} from "herdr";
// A namespace import, so a config.js written before MACHINE/STATE_DIR existed still loads.
import * as config from "./config.js";

const SPINNER = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const TYPING = ["●  ·  ·", "·  ●  ·", "·  ·  ●"];
const hhmm = (ms) => {
  if (!ms) return "";
  const d = new Date(ms);
  return `${String(d.getHours()).padStart(2, "0")}:${String(d.getMinutes()).padStart(2, "0")}`;
};
// The daemon appends each answer to an item's body as "\n\n[answer] <text>".
const ANSWER_SEP = "\n\n[answer] ";
const SOURCE_LABEL = { schedule: "⏰ Schedule", github: "GitHub", linear: "Linear" };
// A daemon: { machine (null: this computer), bin, stateDir (null: ~/.herdr-bots/state) }.
const MAIN_DAEMON = { machine: config.MACHINE ?? null, bin: config.BIN, stateDir: config.STATE_DIR ?? null };
async function loadDaemons() {
  const list = [MAIN_DAEMON];
  const text = await readTextFile(`${appHome()}/mods/bots/daemons.json`).catch(() => null);
  try {
    for (const d of JSON.parse(text ?? "{}").daemons ?? []) {
      const machine = d?.machine ?? null;
      if (!d?.bin || list.some((x) => x.machine === machine)) continue;
      list.push({ machine, bin: d.bin, stateDir: d.state_dir ?? null });
    }
  } catch { /* a broken daemons.json leaves the main daemon alone */ }
  return list;
}
// Saving daemons.json reloads the mod, so one load per module is enough.
let daemonsLoaded = null;
const daemons = () => (daemonsLoaded ??= loadDaemons());
const stateDirOf = (d) => d.stateDir ?? `${home()}/.herdr-bots/state`;
const readStatusOf = (d) => readTextFile(`${stateDirOf(d)}/status.json`, { on: d.machine }).catch(() => null);
const runOn = (d, args) => exec(d.bin, args, { on: d.machine }).catch((e) => ({ code: 1, stderr: String(e) }));
const machineLabel = (d) => d.machine ?? "this computer";
// Another machine's terminals have addresses like "wsl/t3"; this computer's are bare ("t3").
const onMachine = (d, t) => (d.machine ? String(t.address).startsWith(`${d.machine}/`) : !String(t.address).includes("/"));
const terminalOf = (bot) => terminals().find((t) => t.name === `bot-${bot.name}` && t.state === "live" && onMachine(bot.daemon, t));
// One daemon: keys are the plain bot ids, as before. More: "<machine>/<id>".
const botKey = (d, id, multi) => (multi ? `${d.machine ?? "local"}/${id}` : id);

// Every daemon's status.json as one snapshot. Bots, items and workers carry their daemon,
// the daemon's own id (name, num) and a key that is unique across daemons.
function mergeSnaps(list) {
  const multi = list.length > 1;
  const out = { bots: [], items: [], workers: [], bot_errors: {}, version: null, daemons: [] };
  for (const { daemon: d, snap } of list) {
    out.daemons.push({ daemon: d, up: Boolean(snap) });
    if (!snap) continue;
    out.version ??= snap.version;
    const key = (id) => botKey(d, id, multi);
    for (const b of snap.bots ?? []) out.bots.push({ ...b, id: key(b.id), name: b.id, daemon: d });
    for (const i of snap.items ?? []) out.items.push({ ...i, bot: key(i.bot), botName: i.bot, num: i.id, key: `${d.machine ?? "local"}#${i.id}`, daemon: d });
    for (const w of snap.workers ?? []) out.workers.push({ ...w, bot: key(w.bot) });
    for (const [id, e] of Object.entries(snap.bot_errors ?? {})) out.bot_errors[key(id)] = e;
  }
  return out;
}

function find(node, id) {
  if (node?.id === id) return node;
  for (const child of node?.children ?? []) {
    const found = find(child, id);
    if (found) return found;
  }
  return null;
}
// The sidebar rail (dragged narrow) shows only status dots, like Agents does.
const narrow = () => (find(layout(), "sidebar")?.size?.px ?? Infinity) < 160;

// The right-hand panel when the layout has it (install.sh adds it), else a window.
const PANEL = "bots-chat";
const panelShown = () => {
  const panel = find(layout(), "panel"), body = find(layout(), "panel-body");
  return Boolean(panel && !panel.hidden && body?.active === PANEL);
};
function openChat(bot) {
  if (find(layout(), PANEL)) {
    setShared("bots.chat", bot);
    run("layout.select", { stack: "panel-body", child: PANEL });
    run("layout.show", { id: "panel" });
    return;
  }
  openOverlay({ kind: "window", key: `bots-chat-${bot}`, keep: true, width: 440, height: 620, mod: "bots", props: { chat: bot } });
}

const BOT_ID = /^[a-z0-9_-]{1,24}$/;

// Copy, paste, cut and select-all for one text box, for keyboards that send other chords than
// Ctrl+C/V. On Windows, mac-style setups (AutoHotkey turning ⌘ into Ctrl+Shift for the
// terminal) send Ctrl+Shift+C/V, and ⌘A/⌘X may arrive as Alt+A/Alt+X; Ctrl+Insert and
// Shift+Insert are the old copy/paste keys. A text box knows none of them, so the wrapper
// around it handles them. Plain Ctrl+C/V/X/A still go to the box itself.
function textKeys(state, readonly = false) {
  return (e, cx) => {
    const m = e.modifiers, key = String(e.key).toLowerCase();
    const value = state.value(), r = state.selected_range();
    const selected = value.slice(Math.min(r.start, r.end), Math.max(r.start, r.end));
    if (readonly && !((m.control && (key === "c" || key === "insert")) || (m.alt && key === "a"))) return;
    if ((m.control && m.shift && key === "v") || (m.shift && !m.control && key === "insert")) {
      const text = cx.read_from_clipboard();
      if (text) state.replace(text);
    } else if ((m.control && m.shift && key === "c") || (m.control && !m.shift && key === "insert")) {
      if (selected) cx.write_to_clipboard(selected);
    } else if (m.alt && !m.control && key === "x") {   // not Ctrl+Shift+X: Herdr's pane.close
      if (selected) { cx.write_to_clipboard(selected); state.replace(""); }
    } else if (m.alt && !m.control && key === "a") {
      state.set_selected_range({ start: 0, end: value.length });
    } else {
      return;
    }
    cx.stop_propagation();
  };
}

function openDeleteBot(bot) {
  openOverlay({ mod: "bots", kind: "dialog", width: 420, props: { deleteBot: bot.name, machine: bot.daemon.machine, key: bot.id } });
}

function openRenameBot(bot) {
  openOverlay({ mod: "bots", kind: "dialog", width: 420, props: { renameBot: bot.name, machine: bot.daemon.machine, key: bot.id } });
}

function openNewBot() {
  openOverlay({ mod: "bots", kind: "dialog", width: 420, props: { newBot: true } });
}

export default class Bots extends View {
  init(_props, cx) {
    const p = props() ?? {};
    this.mode = p.titlebar ? "titlebar" : p.newBot ? "new" : p.deleteBot ? "delete" : p.renameBot ? "rename"
      : p.chat ? "chat" : p.sidebar ? "sidebar" : "full";
    this.inPanel = p.chat === true; // follows shared "bots.chat" instead of a fixed bot
    if (this.mode === "titlebar") return; // a terminal window's title bar needs no data
    if (this.mode === "new") return this.initNew(cx);
    if (this.mode === "delete") return this.initDelete(cx);
    if (this.mode === "rename") return this.initRename(cx);
    this.snap = null;
    this.error = null;
    this.selected = this.inPanel ? (shared("bots.chat") ?? null) : (p.chat ?? null);
    this.answering = null; // the needs_you item the message box answers
    this.seenQuestions = new Set();
    this.firstLoad = true;
    this.input = InputState.new({ placeholder: "Message…" });
    this.chatScroll = ScrollHandle.new(); // keeps the newest turn in view
    this.selecting = new Map();           // item key -> read-only EditorState showing the reply's source
    this.input.on("submit", (_e, cx) => this.send(cx));
    if (checking()) return;
    cx.spawn(async (cx) => {
      this.daemonList = await daemons();
      this.reload(cx);
      const local = this.daemonList.find((d) => !d.machine);
      if (this.daemonList.some((d) => d.machine)) {
        // watchFolder sees only this computer: read other machines' status files instead.
        // reload() redraws only when something changed.
        cx.timer.every(2000, (cx) => this.reload(cx));
      } else {
        // The folder may not exist until the daemon first starts; poll gently as a fallback.
        cx.timer.every(15000, (cx) => this.reload(cx));
      }
      if (local) {
        for (;;) {
          try { await watchFolder(stateDirOf(local)); } catch { await new Promise((r) => cx.timer.after(5000, r)); }
          this.reload(cx);
        }
      }
    });
    if (this.mode === "chat") cx.timer.after(300, () => this.input.focus());
  }

  reload(cx) {
    cx.spawn(async (cx) => { if (await this.load()) cx.notify(); });
  }

  // Reads every daemon's status. True when something changed and the view should redraw.
  async load() {
    if (!this.daemonList) return false;
    const texts = await Promise.all(this.daemonList.map(readStatusOf));
    const joined = JSON.stringify(texts);
    if (joined === this.lastText && this.snap) return false;
    const list = [];
    for (const [n, text] of texts.entries()) {
      let snap = null;
      try { snap = text ? JSON.parse(text) : null; } catch { return false; } // mid-write; the next change rereads
      list.push({ daemon: this.daemonList[n], snap });
    }
    this.lastText = joined;
    this.loaded = true;
    this.snap = list.some((x) => x.snap) ? mergeSnaps(list) : null;
    if (!this.snap) return true;
    if (this.mode === "full" && !this.snap.bots.some((b) => b.id === this.selected)) this.selected = this.snap.bots[0]?.id ?? null;
    // The sidebar copy is always shown, so it alone owns the badge and the alerts.
    if (this.mode === "sidebar") this.announce();
    return true;
  }

  // Dock badge = open questions; a sound and a toast for each new one.
  announce() {
    const asking = this.snap.items.filter((i) => i.status === "needs_you");
    setBadge(asking.length ? String(asking.length) : null);
    for (const item of asking) {
      if (this.seenQuestions.has(item.key)) continue;
      this.seenQuestions.add(item.key);
      if (this.firstLoad) continue;
      playSound("blocked");
      notify({ title: `${item.botName} needs you`, body: item.question ?? item.title, level: "warning" });
    }
    this.firstLoad = false;
  }

  send(cx) {
    const text = this.input.value().trim();
    const bot = this.snap?.bots.find((b) => b.id === this.selected);
    if (!text || !bot) return;
    const args = ["ask", bot.name, text];
    const answering = this.snap.items.find((i) => i.key === this.answering);
    if (answering) args.push("--item", String(answering.num));
    this.input.set_value("");
    this.answering = null;
    // Shown at once; the daemon's own item replaces it after the next read (a few seconds on
    // another machine, where the status file is read every 2 s over hc).
    this.pending = { bot: bot.id, text, answering: answering ? answering.num : null };
    this.run(cx, bot.daemon, args);
  }

  // Runs a herdr-bots command on that daemon, then rereads at once so the change shows.
  run(cx, daemon, args) {
    cx.notify();
    cx.spawn(async (cx) => {
      const r = await runOn(daemon, args);
      this.error = r.code === 0 ? null : (r.stderr || r.stdout || "failed").trim();
      await this.load();
      this.pending = null;
      cx.notify();
    });
  }

  openTerminal(bot) {
    const t = terminalOf(bot);
    if (!t) { notify({ title: `${bot.name} isn't running`, body: "It starts with its first message.", level: "info" }); return; }
    openOverlay({
      kind: "window", key: `bot-${bot.id}`, width: 760, height: 520,
      layout: { type: "column", children: [
        { type: "block", size: { px: 30 }, content: { mod: "bots", props: { titlebar: { bot: bot.id } } } },
        { type: "block", content: { terminal: t.address } },
      ] },
    });
  }

  render(cx) {
    const t = theme();
    switch (this.mode) {
      case "titlebar": return this.renderTitlebar(t);
      case "sidebar": return this.renderSidebar(t);
      case "chat": return this.renderChat(t);
      case "new": return this.renderNew(t);
      case "delete": return this.renderDelete(t);
      case "rename": return this.renderRename(t);
      default: return this.renderFull(t);
    }
  }

  // ---------- sidebar ----------

  renderSidebar(t) {
    const bots = this.snap?.bots ?? [];
    const root = v_flex().id("bots-side").size_full().text_size(13).text_color(t.foreground);
    if (narrow()) {
      return root.pt(6).children(bots.map((b) => h_flex().id(`sb-${b.id}`).h(26).px(8).flex_none().items_center()
        .justify_end().hover((s) => s.bg(t.hover)).on_click(() => openChat(b.id)).tooltip(this.label(b))
        .child(this.dot(t, b.paused ? "paused" : b.state))));
    }
    const header = h_flex().px(16).h(41).pt(14).pb(5).flex_none().items_center().justify_between()
      .child(div().text_size(11.5).font_medium().text_color(t.dim).child("Bots"))
      .child(h_flex().gap(10).items_center()
        .child(div().id("bots-all").text_size(11).text_color(t.dim).cursor_pointer()
          .hover((s) => s.text_color(t.foreground)).tooltip("All bots, items and workers")
          .on_click(() => openOverlay({ mod: "bots", kind: "window", key: "herdr-bots", width: 360, height: 640 }))
          .child("All"))
        .child(div().id("bots-new").text_size(15).text_color(t.dim).cursor_pointer()
          .hover((s) => s.text_color(t.foreground)).tooltip("New bot")
          .on_click(() => openNewBot())
          .child("+")));
    let body;
    if (!this.snap) {
      body = div().px(16).text_size(12).text_color(t.dim).child(checking() ? "" : !this.loaded ? "Loading…" : "herdr-bots isn't running. Run herdr-bots install-service.");
    } else if (!bots.length) {
      body = div().px(16).text_size(12).text_color(t.dim).child("No bots yet. Run herdr-bots new <id>.");
    } else {
      body = v_flex().px(8).children(bots.map((b) => this.sidebarRow(t, b)));
    }
    return root.child(header).child(v_flex().id("bots-side-list").flex_1().min_h(0).overflow_y_scrollbar().child(body));
  }

  sidebarRow(t, b) {
    const question = this.snap.items.find((i) => i.bot === b.id && i.status === "needs_you");
    const working = this.snap.items.find((i) => i.bot === b.id && i.status === "working");
    const sub = b.paused ? "paused"
      : question ? (question.question ?? question.title)
      : working ? working.title
      : b.planned ? `${b.planned} queued` : b.agent;
    const open = panelShown() && shared("bots.chat") === b.id;
    const hovered = this.hoveredRow === b.id;
    return h_flex().id(`sb-${b.id}`).h(34).px(8).gap(8).flex_none().items_center().rounded(6).cursor_pointer()
      .when(open, (el) => el.bg(t.selected))
      .hover((s) => s.bg(open ? t.selected : t.hover))
      .on_hover((on, cx) => { this.hoveredRow = on ? b.id : (this.hoveredRow === b.id ? null : this.hoveredRow); cx.notify(); })
      .on_click(() => openChat(b.id))
      .child(h_flex().w(14).flex_none().justify_center().child(this.dot(t, b.paused ? "paused" : b.state)))
      .child(v_flex().flex_1().min_w(0)
        .child(div().text_size(13).truncate().child(this.label(b)))
        .child(div().text_size(11).truncate().text_color(question ? t.blocked : t.dim).child(sub)))
      .when(b.needs_you > 0 && !hovered, (el) => el.child(div().flex_none().px(6).rounded_full().bg(t.blocked)
        .text_size(10.5).text_color(t.background).child(String(b.needs_you))))
      // Shown on hover only, so the list stays quiet.
      .when(hovered, (el) => el.child(div().id(`sb-del-${b.id}`).flex_none().px(6).py(2).rounded(5).text_size(11)
        .text_color(t.dim).hover((s) => s.bg(t.raised).text_color(t.blocked))
        .on_mouse_down("left", (_e, cx) => cx.stop_propagation())
        .on_click((_e, cx) => { cx.stop_propagation(); openDeleteBot(b); })
        .child("Delete")))
      .when(hovered, (el) => el.child(div().id(`sb-ren-${b.id}`).flex_none().px(6).py(2).rounded(5).text_size(11)
        .text_color(t.dim).hover((s) => s.bg(t.raised).text_color(t.foreground))
        .on_mouse_down("left", (_e, cx) => cx.stop_propagation())
        .on_click((_e, cx) => { cx.stop_propagation(); openRenameBot(b); })
        .child("Rename")));
  }

  // ---------- chat window ----------

  renderChat(t) {
    if (this.inPanel) {
      const want = shared("bots.chat") ?? null;
      if (want !== this.selected) { this.selected = want; this.answering = null; this.error = null; }
    }
    const bot = this.snap?.bots.find((b) => b.id === this.selected);
    const root = v_flex().id("bots-chat").size_full().bg(this.inPanel ? t.chrome : t.popover).text_color(t.foreground).text_size(12.5);
    // In the panel, ‹ goes back to the bot list (where + New bot is); in a window there's no list.
    const header = h_flex().h(40).px(12).gap(6).flex_none().items_center().border_b_1().border_color(t.border)
      .when(this.inPanel && Boolean(this.selected), (el) => el.child(this.button(t, "chat-back", "‹ Bots", () => setShared("bots.chat", null))))
      .when(Boolean(bot), (el) => el.child(this.dot(t, bot.paused ? "paused" : bot.state)))
      // The name shortens before the buttons do (a machine label makes it long); the details
      // (agent · permission) go first, then the name is cut with an ellipsis.
      .child(div().id("chat-name").flex_shrink_1().min_w(0).truncate().font_semibold()
        .when(Boolean(bot), (el) => el.cursor_pointer().hover((s) => s.text_color(t.accent))
          .tooltip(`${this.label(bot)} · click to rename`).on_click(() => openRenameBot(bot)))
        .child(bot ? this.label(bot) : (this.selected ?? "Bots")))
      .child(div().flex_1().min_w(0).text_size(11).text_color(t.dim).truncate().child(bot ? `${bot.agent} · ${bot.permission}` : ""))
      .when(Boolean(bot), (el) => el
        .child(this.button(t, "chat-pause", bot.paused ? "Resume" : "Pause", (cx) => this.run(cx, bot.daemon, [bot.paused ? "resume" : "pause", bot.name])))
        .child(this.button(t, "chat-term", "Terminal", () => this.openTerminal(bot)))
        .child(this.button(t, "chat-delete", "Delete", () => openDeleteBot(bot))))
      .when(!this.selected, (el) => el.child(this.button(t, "chat-new", "+ New bot", () => openNewBot())))
      .child(this.closeButton(t));
    if (!this.snap) return root.child(header).child(div().p(12).text_color(t.muted).child(checking() ? "" : !this.loaded ? "Loading…" : "herdr-bots isn't running."));
    if (!this.selected) {
      // Opened from the title bar's Bots button with nothing picked yet: offer the bots here.
      const bots = this.snap.bots;
      return root.child(header).child(v_flex().p(8).gap(2)
        .child(div().px(8).pb(6).text_size(11.5).text_color(t.dim).child(bots.length ? "Pick a bot" : "No bots yet."))
        .children(bots.map((b) => this.sidebarRow(t, b)))
        .child(h_flex().id("chat-new-row").h(34).px(8).gap(8).flex_none().items_center().rounded(6).cursor_pointer()
          .text_color(t.muted).hover((s) => s.bg(t.hover).text_color(t.foreground))
          .on_click(() => openNewBot())
          .child(h_flex().w(14).flex_none().justify_center().child("+"))
          .child(div().text_size(13).child("New bot"))));
    }
    if (!bot) return root.child(header).child(div().p(12).text_color(t.muted).child(`No bot ${this.selected}.`));
    return root.child(header)
      .child(this.renderConversation(t))
      .when(Boolean(this.error), (el) => el.child(div().px(12).py(6).text_size(11.5).text_color(t.blocked).child(this.error)))
      .child(this.renderInput(t));
  }

  // ---------- chat: one turn per item, as message bubbles ----------
  //
  // An item reads as a short conversation: what started it (my message on the right, or a
  // line for a schedule / GitHub / Linear item), my answers, then the bot on the left: its
  // reply, its open question, or a typing bubble while it works. status.json holds an item's
  // body (with my answers appended), its latest question and its latest reply, so earlier
  // questions of a long exchange are not shown.

  renderConversation(t) {
    const items = this.snap.items.filter((i) => i.bot === this.selected).sort((a, b) => a.num - b.num).slice(-30);
    const pending = this.pending?.bot === this.selected ? this.pending : null;
    const turns = items.map((i) => this.renderTurn(t, i));
    if (pending && !pending.answering) turns.push(this.userBubble(t, "pending", pending.text, "sending…", true));
    if (!turns.length) {
      return v_flex().flex_1().min_h(0).items_center().justify_center().text_size(12).text_color(t.dim).child("Nothing yet. Say something below.");
    }
    // Bring the newest turn into view when one arrives or changes.
    const last = items.at(-1);
    const sig = `${this.selected}|${items.length}|${last?.status}|${last?.updated_at}|${pending ? 1 : 0}`;
    if (sig !== this.scrollSig) { this.scrollSig = sig; this.chatScroll.scroll_to_item(turns.length - 1); }
    return v_flex().id("bots-conversation").flex_1().min_h(0).overflow_y_scrollbar().track_scroll(this.chatScroll)
      .px(12).pt(12).pb(4).gap(16).children(turns).child(this.renderWorkers(t));
  }

  renderTurn(t, i) {
    const [first, ...answers] = (i.body || i.title || "").split(ANSWER_SEP);
    const rows = [];
    if (i.source === "chat") rows.push(this.userBubble(t, `u-${i.key}`, first, hhmm(i.created_at)));
    else rows.push(this.originLine(t, i, first));
    answers.forEach((a, n) => rows.push(this.userBubble(t, `a-${i.key}-${n}`, a, "answer")));
    if (this.pending?.answering === i.num && this.pending.bot === i.bot) {
      rows.push(this.userBubble(t, `p-${i.key}`, this.pending.text, "sending…", true));
    }
    const reply = (i.reply ?? "").trim();
    const source = this.selecting.get(i.key);
    if (reply && source) {
      // The reply's markdown source in a read-only editor: its selection is readable, so a copy
      // chord the rendered view ignores (⌘C sent as Ctrl+Shift+C) still copies a part.
      const height = Math.min(reply.split("\n").length * 19 + 16, 480);
      rows.push(this.botBubble(t, `r-${i.key}`, div().w_full().on_key_down(textKeys(source, true))
        .child(Editor.new(source).w_full().h(height)), true));
    } else if (reply) {
      rows.push(this.botBubble(t, `r-${i.key}`, TextView.markdown(`reply-${i.key}`, reply).selectable(true)));
    }
    if (i.status === "needs_you" && i.question) rows.push(this.questionBubble(t, i));
    if (i.status === "working" || i.status === "planned") {
      rows.push(this.botBubble(t, `w-${i.key}`, h_flex().gap(8).items_center()
        .child(div().text_color(t.muted).frames(TYPING, 320))
        .child(div().text_size(11.5).text_color(t.dim).child(i.status === "planned" ? "queued" : "working"))));
    }
    rows.push(this.turnMeta(t, i));
    return v_flex().id(`turn-${i.key}`).flex_shrink_0().gap(6).children(rows);
  }

  // Mine: right-aligned, tinted. `note` goes under it (time, "answer", "sending…").
  userBubble(t, id, text, note, faded = false) {
    return v_flex().id(id).flex_shrink_0().w_full().items_end().gap(2)
      .child(h_flex().w_full().justify_end().pl(40)
        .child(div().min_w(0).px(11).py(7).rounded(12).bg(t.accent + "30").text_size(12.5)
          .text_color(faded ? t.muted : t.foreground).whitespace_normal().child(text)))
      .when(Boolean(note), (el) => el.child(div().pr(4).text_size(10.5).text_color(t.dim).child(note)));
  }

  // The bot's: left-aligned on the raised surface, any element inside (markdown, typing dots).
  // `wide`: take the row's width, for content that sizes to its parent (an editor) and would
  // otherwise collapse inside a bubble that sizes to its content.
  botBubble(t, id, content, wide = false) {
    return h_flex().id(id).flex_shrink_0().w_full().pr(20)
      .child(div().min_w(0).px(12).py(8).rounded(12).bg(t.raised).text_size(12.5)
        .when(wide, (el) => el.flex_1()).child(content));
  }

  questionBubble(t, i) {
    const answering = this.answering === i.key;
    return h_flex().id(`q-${i.key}`).flex_shrink_0().w_full().pr(20)
      .child(v_flex().min_w(0).gap(6).px(12).py(8).rounded(12).bg(t.raised).border_1().border_color(t.blocked)
        .child(div().text_size(10.5).text_color(t.blocked).child("needs you"))
        .child(div().text_size(12.5).whitespace_normal().child(i.question))
        .child(h_flex().child(this.button(t, `reply-to-${i.key}`, answering ? "Replying…" : "Reply", (cx) => {
          this.answering = i.key; this.input.focus(); cx.notify();
        }, answering))));
  }

  // A schedule, GitHub or Linear item: one quiet line instead of a bubble of mine.
  originLine(t, i, text) {
    return h_flex().id(`o-${i.key}`).flex_shrink_0().w_full().gap(6).items_center().text_size(11)
      .child(div().flex_none().text_color(t.muted).child(SOURCE_LABEL[i.source] ?? i.source))
      .child(div().flex_1().min_w(0).truncate().text_color(t.foreground).tooltip(i.url ?? text).child(i.title || text))
      .child(div().flex_none().text_color(t.dim).child(hhmm(i.created_at)));
  }

  // Under the bot's side: who, when, the state, and what can be done with the item.
  turnMeta(t, i) {
    const state = { done: "done", canceled: "canceled", needs_you: "needs you", working: "working", planned: "queued" }[i.status] ?? i.status;
    const color = { needs_you: t.blocked, working: t.working, done: t.dim, canceled: t.dim, planned: t.muted }[i.status] ?? t.dim;
    const link = (id, label, tip, args) => div().id(id).flex_none().text_color(t.dim).cursor_pointer()
      .hover((s) => s.text_color(t.foreground)).tooltip(tip)
      .on_click((_e, cx) => this.run(cx, i.daemon, args)).child(label);
    const reply = (i.reply ?? "").trim();
    return h_flex().id(`m-${i.key}`).flex_shrink_0().gap(8).items_center().pl(4).text_size(10.5)
      .child(div().text_color(t.dim).child(`${i.botName} · #${i.num} · ${hhmm(i.updated_at)}`))
      .child(div().text_color(color).child(state))
      // The whole reply as markdown source, for keyboards whose copy chord a text view ignores.
      .when(Boolean(reply), (el) => el.child(div().id(`copy-${i.key}`).flex_none().text_color(t.dim).cursor_pointer()
        .hover((s) => s.text_color(t.foreground)).tooltip("Copy the reply (markdown)")
        .on_click((_e, cx) => { cx.write_to_clipboard(reply); this.copied = i.key; cx.notify();
          cx.timer.after(1500, (cx) => { if (this.copied === i.key) { this.copied = null; cx.notify(); } }); })
        .child(this.copied === i.key ? "Copied" : "Copy")))
      .when(Boolean(reply), (el) => el.child(div().id(`select-${i.key}`).flex_none().text_color(t.dim).cursor_pointer()
        .hover((s) => s.text_color(t.foreground))
        .tooltip(this.selecting.has(i.key) ? "Back to the rendered reply" : "Show the source, to select a part and copy it")
        .on_click((_e, cx) => { this.toggleSource(i.key, reply); cx.notify(); })
        .child(this.selecting.has(i.key) ? "Rendered" : "Select")))
      .when(this.canCloseAsDone(i), (el) => el.child(link(`done-${i.key}`, "Done", "Close it as done", ["done", String(i.num)])))
      .when(!["done", "canceled"].includes(i.status), (el) => el.child(link(`cancel-${i.key}`, "Cancel",
        i.status === "working" ? "Stop waiting for it. The bot's current turn keeps going; Pause stops it."
          : i.status === "planned" ? "Drop it before it runs" : "Close it without an answer",
        ["cancel", String(i.num)])));
  }

  // ---------- full window ----------

  renderFull(t) {
    const root = v_flex().id("bots").size_full().bg(t.chrome).text_color(t.foreground).text_size(12.5);
    const header = h_flex().h(36).px(12).flex_none().items_center().justify_between().border_b_1().border_color(t.border)
      .child(div().font_semibold().child("Bots"))
      .child(h_flex().gap(8).items_center()
        .child(div().text_size(11).text_color(t.dim).child(this.snap ? `v${this.snap.version}` : ""))
        .when(Boolean(props()?.overlay), (el) => el.child(this.closeButton(t))));
    if (checking()) return root.child(header);
    if (!this.snap && !this.loaded) return root.child(header).child(div().p(12).text_color(t.muted).child("Loading…"));
    if (!this.snap) {
      return root.child(header).child(v_flex().p(12).gap(6).text_color(t.muted)
        .child("The herdr-bots daemon isn't running.")
        .child(div().text_color(t.dim).child("Run herdr-bots install-service, then herdr-bots new <id>.")));
    }
    return root.child(header)
      .child(this.renderBotList(t))
      .child(v_flex().flex_1().min_h(0).overflow_y_scrollbar().child(this.renderItems(t, false)).child(this.renderWorkers(t)))
      .when(Boolean(this.error), (el) => el.child(div().px(12).py(6).text_size(11.5).text_color(t.blocked).child(this.error)))
      .child(this.renderInput(t));
  }

  renderBotList(t) {
    const rows = this.snap.bots.map((b) => {
      const selected = b.id === this.selected;
      const counts = [b.needs_you && `${b.needs_you} needs you`, b.working && "working", b.planned && `${b.planned} queued`]
        .filter(Boolean).join(" · ");
      return h_flex().id(`bot-${b.id}`).px(12).py(6).gap(6).flex_shrink_0().items_center().cursor_pointer()
        .bg(selected ? t.selected : t.chrome).hover((s) => s.bg(selected ? t.selected : t.hover))
        .on_click((_e, cx) => { this.selected = b.id; this.answering = null; cx.notify(); })
        .child(this.dot(t, b.paused ? "paused" : b.state))
        .child(v_flex().flex_1().min_w(0)
          .child(h_flex().gap(6).child(div().font_medium().truncate().child(this.label(b)))
            .child(div().text_size(11).text_color(t.dim).child(`${b.agent}${b.paused ? " · paused" : ""}`)))
          .when(Boolean(counts), (el) => el.child(div().text_size(11).text_color(t.muted).truncate().child(counts))))
        .when(selected, (el) => el
          .child(this.button(t, `chat-${b.id}`, "Chat", () => openChat(b.id)))
          .child(this.button(t, `term-${b.id}`, "Terminal", () => this.openTerminal(b))));
    });
    const broken = Object.entries(this.snap.bot_errors ?? {}).map(([id, e]) =>
      div().px(12).py(4).text_size(11).text_color(t.blocked).truncate().child(`${id}: ${e}`));
    if (!rows.length && !broken.length) {
      return div().p(12).text_color(t.muted).child("No bots yet. Run herdr-bots new <id>.");
    }
    return v_flex().py(4).flex_shrink_0().border_b_1().border_color(t.border).children([...rows, ...broken]);
  }

  // ---------- shared pieces ----------

  renderTitlebar(t) {
    const { bot } = props().titlebar;
    return h_flex().id("bots-title").size_full().px(10).items_center().justify_between().bg(t.chrome)
      .border_b_1().border_color(t.border).text_size(12).text_color(t.foreground)
      .child(div().font_semibold().child(`bot · ${bot}`))
      .child(this.closeButton(t));
  }

  closeButton(t) {
    return div().id("bots-close").px(6).rounded(6).cursor_pointer().text_color(t.muted)
      .hover((s) => s.bg(t.hover))
      .on_mouse_down("left", (_e, cx) => cx.stop_propagation())
      .on_click(() => (this.inPanel ? run("layout.hide", { id: "panel" }) : closeOverlay(props().overlay))).child("✕");
  }

  // ---------- delete bot dialog ----------

  initDelete(cx) {
    this.target = props().deleteBot;   // the daemon's own id for the bot
    this.targetKey = props().key ?? this.target;
    this.machine = props().machine ?? null;
    this.force = false;
    this.busy = false;
    this.error = null;
    this.workers = 0;
    this.handle = cx.focus_handle();
    if (checking()) return;
    cx.timer.after(100, () => this.handle.focus());
    cx.spawn(async (cx) => {
      this.daemon = (await daemons()).find((d) => d.machine === this.machine) ?? MAIN_DAEMON;
      const text = await readStatusOf(this.daemon);
      try {
        const snap = JSON.parse(text ?? "{}");
        this.workers = (snap.workers ?? []).filter((w) => w.bot === this.target && w.status === "running").length;
      } catch { /* shown without the worker count */ }
      cx.notify();
    });
  }

  remove(cx) {
    if (this.busy) return;
    if (this.workers && !this.force) { this.error = "Tick the box to stop its workers too."; cx.notify(); return; }
    this.busy = true;
    const args = ["rm", this.target];
    if (this.force) args.push("--force");
    cx.spawn(async (cx) => {
      const r = await runOn(this.daemon ?? MAIN_DAEMON, args);
      this.busy = false;
      if (r.code !== 0) { this.error = (r.stderr || r.stdout || "failed").trim(); cx.notify(); return; }
      if (shared("bots.chat") === this.targetKey) setShared("bots.chat", null);
      closeOverlay(props().overlay);
      notify({ title: `Deleted ${this.target}`, body: "Its folder is in ~/.herdr-bots/trash.", level: "info" });
    });
  }

  renderDelete(t) {
    const line = (text) => div().text_size(12.5).text_color(t.muted).whitespace_normal().child(text);
    return v_flex().id("bots-delete").track_focus(this.handle)
      .on_key_down((e, cx) => {
        if (e.key === "escape") { closeOverlay(props().overlay); cx.stop_propagation(); }
        else if (e.key === "enter") { this.remove(cx); cx.stop_propagation(); }
      })
      .w_full().p(18).gap(8).bg(t.popover).text_color(t.foreground)
      .child(div().text_size(15).font_semibold().child(`Delete ${this.target}${this.machine ? ` on ${this.machine}` : ""}?`))
      .child(line("Its open items are canceled and its agent's terminal ends."))
      .child(line("Its folder (SOUL.md, MEMORY.md, bot.toml) moves to ~/.herdr-bots/trash, so you can bring it back."))
      .when(this.workers > 0, (el) => el.child(h_flex().id("del-force").gap(8).pt(4).items_center().cursor_pointer()
        .on_click((_e, cx) => { this.force = !this.force; this.error = null; cx.notify(); })
        .child(div().w(16).h(16).rounded(4).border_1().border_color(this.force ? t.blocked : t.input)
          .bg(this.force ? t.blocked : t.background).text_size(11).text_color(t.background).child(this.force ? "✓" : ""))
        .child(div().text_size(12.5).child(`Also stop its ${this.workers} running worker${this.workers > 1 ? "s" : ""} (worktrees are kept)`))))
      .when(Boolean(this.error), (el) => el.child(div().text_size(11.5).text_color(t.blocked).child(this.error)))
      .child(h_flex().pt(12).gap(8).justify_end()
        .child(this.button(t, "del-cancel", "Cancel", () => closeOverlay(props().overlay)))
        .child(div().id("del-yes").px(10).py(4).rounded(6).text_size(12).cursor_pointer().bg(t.blocked).text_color(t.background)
          .hover((s) => s.opacity(0.85)).on_click((_e, cx) => this.remove(cx))
          .child(this.busy ? "Deleting…" : "Delete")));
  }

  // ---------- rename bot dialog ----------

  initRename(cx) {
    this.target = props().renameBot;   // the daemon's own id for the bot
    this.targetKey = props().key ?? this.target;
    this.machine = props().machine ?? null;
    this.busy = false;
    this.error = null;
    this.newName = InputState.new({ value: this.target, placeholder: this.target });
    this.newName.on("submit", (_e, cx) => this.rename(cx));
    this.newName.on("change", (_e, cx) => cx.notify());
    this.handle = cx.focus_handle();
    if (!checking()) cx.timer.after(150, () => this.newName.focus());
  }

  rename(cx) {
    const to = this.newName.value().trim();
    if (!BOT_ID.test(to)) { this.error = "Name: 1-24 of a-z 0-9 - _"; cx.notify(); return; }
    if (to === this.target || this.busy) return;
    this.busy = true;
    cx.spawn(async (cx) => {
      const list = await daemons();
      const daemon = list.find((d) => d.machine === this.machine) ?? MAIN_DAEMON;
      const r = await runOn(daemon, ["rename", this.target, to]);
      this.busy = false;
      if (r.code !== 0) { this.error = (r.stderr || r.stdout || "failed").trim(); cx.notify(); return; }
      if (shared("bots.chat") === this.targetKey) setShared("bots.chat", botKey(daemon, to, list.length > 1));
      closeOverlay(props().overlay);
      notify({ title: `Renamed ${this.target} to ${to}`, body: "Its next turn starts a new session; MEMORY.md and SOUL.md stay.", level: "info" });
    });
  }

  renderRename(t) {
    const to = this.newName.value().trim();
    const valid = BOT_ID.test(to) && to !== this.target;
    return v_flex().id("bots-rename").track_focus(this.handle)
      .on_key_down((e, cx) => { if (e.key === "escape") { closeOverlay(props().overlay); cx.stop_propagation(); } })
      .w_full().p(18).gap(8).bg(t.popover).text_color(t.foreground)
      .child(div().text_size(15).font_semibold().child(`Rename ${this.target}${this.machine ? ` on ${this.machine}` : ""}`))
      .child(div().text_size(12.5).text_color(t.muted).whitespace_normal()
        .child("Its folder, items and triggers follow. Its terminal ends, so the next turn starts a new session; SOUL.md and MEMORY.md stay. Not while it is on a turn."))
      .child(div().on_key_down(textKeys(this.newName))
        .child(Input.new(this.newName).h(34).px(10).rounded(8).border_1().border_color(t.input).bg(t.background).text_size(12.8)))
      .when(Boolean(this.error), (el) => el.child(div().text_size(11.5).text_color(t.blocked).whitespace_normal().child(this.error)))
      .child(h_flex().pt(12).gap(8).justify_end()
        .child(this.button(t, "ren-cancel", "Cancel", () => closeOverlay(props().overlay)))
        .child(this.button(t, "ren-yes", this.busy ? "Renaming…" : "Rename", (cx) => this.rename(cx), valid)));
  }

  // ---------- new bot dialog ----------

  initNew(cx) {
    this.newId = InputState.new({ placeholder: "reviewer" });
    this.newRole = InputState.new({ placeholder: "Reviews my PRs and summarizes what to fix" });
    this.newAgent = "claude";
    this.newPermission = "full-access";
    this.newDaemon = 0;          // index into this.daemonList
    this.daemonList = [MAIN_DAEMON];
    this.error = null;
    this.busy = false;
    if (!checking()) cx.spawn(async (cx) => { this.daemonList = await daemons(); cx.notify(); });
    this.newId.on("submit", (_e, cx) => this.create(cx));
    this.newRole.on("submit", (_e, cx) => this.create(cx));
    this.newId.on("change", (_e, cx) => cx.notify());
    this.handle = cx.focus_handle();
    if (!checking()) cx.timer.after(200, () => this.newId.focus());
  }

  create(cx) {
    const id = this.newId.value().trim();
    if (!BOT_ID.test(id)) { this.error = "Name: 1-24 of a-z 0-9 - _"; cx.notify(); return; }
    if (this.busy) return;
    this.busy = true;
    const args = ["new", id, "--agent", this.newAgent, "--permission", this.newPermission];
    const role = this.newRole.value().trim();
    if (role) args.push("--role", role);
    const daemon = this.daemonList[this.newDaemon] ?? MAIN_DAEMON;
    cx.spawn(async (cx) => {
      const r = await runOn(daemon, args);
      this.busy = false;
      if (r.code !== 0) { this.error = (r.stderr || r.stdout || "failed").trim(); cx.notify(); return; }
      closeOverlay(props().overlay);
      openChat(botKey(daemon, id, this.daemonList.length > 1));
    });
  }

  renderNew(t) {
    const label = (text) => div().pt(14).pb(6).text_size(11.5).text_color(t.dim).child(text);
    const field = (state) => div().on_key_down(textKeys(state))
      .child(Input.new(state).h(34).px(10).rounded(8).border_1().border_color(t.input).bg(t.background).text_size(12.8));
    const choice = (group, value, text) => {
      const on = this[group] === value;
      return div().id(`${group}-${value}`).px(10).py(5).rounded(7).text_size(12).cursor_pointer()
        .border_1().border_color(on ? t.accent : t.input).bg(on ? t.selected : t.raised)
        .text_color(on ? t.foreground : t.muted)
        .on_click((_e, cx) => { this[group] = value; cx.notify(); }).child(text);
    };
    const id = this.newId.value().trim();
    const valid = BOT_ID.test(id);
    return v_flex().id("bots-new").track_focus(this.handle)
      .on_key_down((e, cx) => { if (e.key === "escape") { closeOverlay(props().overlay); cx.stop_propagation(); } })
      .w_full().p(18).bg(t.popover).text_color(t.foreground)
      .child(div().text_size(15).font_semibold().child("New bot"))
      .child(label("Name (a-z 0-9 - _)")).child(field(this.newId))
      .child(label("What it's for (goes into SOUL.md; edit it later)")).child(field(this.newRole))
      .when(this.daemonList.length > 1, (el) => el.child(label("Machine")).child(h_flex().gap(6)
        .children(this.daemonList.map((d, n) => choice("newDaemon", n, machineLabel(d))))))
      .child(label("Agent")).child(h_flex().gap(6).child(choice("newAgent", "claude", "Claude Code")).child(choice("newAgent", "codex", "Codex")))
      .child(label("Permission")).child(h_flex().gap(6)
        .child(choice("newPermission", "full-access", "Full access"))
        .child(choice("newPermission", "accept-edits", "Accept edits"))
        .child(choice("newPermission", "ask", "Ask")))
      .when(this.newPermission === "full-access", (el) => el.child(div().pt(6).text_size(11).text_color(t.dim)
        .child("Runs tools without asking. GitHub/Linear work only comes from people you allow.")))
      .when(Boolean(this.error), (el) => el.child(div().pt(10).text_size(11.5).text_color(t.blocked).child(this.error)))
      .child(h_flex().pt(18).gap(8).justify_end()
        .child(this.button(t, "new-cancel", "Cancel", () => closeOverlay(props().overlay)))
        .child(this.button(t, "new-create", this.busy ? "Creating…" : "Create", (cx) => this.create(cx), valid)));
  }

  // A bot's name, with its machine when bots from several daemons share the list.
  label(b) {
    return this.daemonList?.length > 1 ? `${b.name} · ${machineLabel(b.daemon)}` : b.name;
  }

  dot(t, state) {
    if (state === "working") return div().w(14).text_color(t.working).frames(SPINNER, 90);
    const color = { needs_you: t.blocked, planned: t.muted, idle: t.idle, paused: t.dim }[state] ?? t.unknown;
    return div().w(14).text_color(color).child(state === "paused" ? "❙❙" : "●");
  }

  // Chat: oldest first, newest just above the message box. Panel: open items first.
  renderItems(t, chatOrder) {
    let items = this.snap.items.filter((i) => i.bot === this.selected);
    items = chatOrder ? [...items].sort((a, b) => a.num - b.num).slice(-20) : items.slice(0, 20);
    const pending = chatOrder && this.pending?.bot === this.selected ? this.pending : null;
    if (!items.length && !pending) return div().p(12).text_color(t.dim).child("Nothing yet. Say something below.");
    return v_flex().children(items.map((i) => this.renderItem(t, i, chatOrder)))
      .when(Boolean(pending), (el) => el.child(v_flex().id("item-pending").flex_shrink_0().px(12).py(8).gap(4)
        .border_b_1().border_color(t.border)
        .child(h_flex().gap(6).items_center()
          .child(div().text_size(11).text_color(t.dim).child(pending.answering ? `#${pending.answering}` : "new"))
          .child(div().text_size(11).text_color(t.muted).frames(SPINNER, 90))
          .child(div().text_size(11).text_color(t.muted).child("sending…")))
        .child(div().flex_shrink_0().font_medium().whitespace_normal().text_color(t.muted).child(pending.text))));
  }

  toggleSource(key, reply) {
    const open = this.selecting.get(key);
    if (open) { open.release(); this.selecting.delete(key); return; }
    this.selecting.set(key, EditorState.new({ value: reply, readonly: true, soft_wrap: true, line_numbers: false, folding: false }));
  }

  canCloseAsDone(i) {
    if (i.status === "needs_you") return true;
    if (i.status !== "working") return false;
    return this.snap.workers.some((w) => w.bot === i.bot && w.item_id === i.num && w.status === "running");
  }

  // In the chat a reply shows in full: the chat area scrolls. The all-bots window caps it.
  renderItem(t, i, fullReply = false) {
    const color = { needs_you: t.blocked, working: t.working, planned: t.muted, done: t.done, canceled: t.dim }[i.status];
    const answering = this.answering === i.key;
    const reply = (i.reply ?? "").trim();
    // flex_shrink_0: inside the scroll area, rows keep their height instead of squeezing.
    return v_flex().id(`item-${i.key}`).flex_shrink_0().px(12).py(8).gap(4).border_b_1().border_color(t.border)
      .when(answering, (el) => el.bg(t.raised))
      .child(h_flex().gap(6).items_center()
        .child(div().text_size(11).text_color(t.dim).child(`#${i.num}`))
        .child(div().text_size(11).text_color(color).child(i.status.replace("_", " ")))
        .child(div().text_size(11).text_color(t.dim).child(i.source))
        .child(div().flex_1())
        // Done closes an item by hand: for a question you settled yourself, or a turn that is
        // over with only its workers still out. While the bot is on its turn neither Done nor
        // Cancel stops it (Pause does), so Done stays hidden then and Cancel says what it does.
        .when(this.canCloseAsDone(i), (el) => el
          .child(this.button(t, `done-${i.key}`, "Done", (cx) => this.run(cx, i.daemon, ["done", String(i.num)]))
            .tooltip("Close it as done")))
        .when(!["done", "canceled"].includes(i.status), (el) => el
          .child(this.button(t, `cancel-${i.key}`, "Cancel", (cx) => this.run(cx, i.daemon, ["cancel", String(i.num)]))
            .tooltip(i.status === "working" ? "Stop waiting for it. The bot's current turn keeps going; Pause stops it."
              : i.status === "planned" ? "Drop it before it runs" : "Close it without an answer"))))
      .child(div().flex_shrink_0().font_medium().whitespace_normal().child(i.title))
      .when(Boolean(i.question), (el) => el.child(
        h_flex().id(`ask-${i.key}`).gap(6).p(6).rounded(6).bg(t.background).cursor_pointer()
          .on_click((_e, cx) => { this.answering = i.key; this.input.focus(); cx.notify(); })
          .child(div().flex_none().text_color(t.blocked).child("?"))
          // min_w(0): without it the text keeps its one-line width and pushes past the panel.
          .child(div().flex_1().min_w(0).whitespace_normal().child(i.question))
          .child(div().flex_none().text_size(11).text_color(t.accent).child(answering ? "answering" : "Answer"))))
      .when(Boolean(reply), (el) => el.child(div().flex_shrink_0().min_w(0).text_size(12)
        .when(!fullReply, (d) => d.max_h(240).overflow_y_scrollbar())
        .child(TextView.markdown(`reply-${i.key}`, !fullReply && reply.length > 4000 ? `${reply.slice(0, 4000)}…` : reply))))
      .when(Boolean(i.url), (el) => el.child(div().text_size(11).text_color(t.dim).truncate().child(i.url)));
  }

  renderWorkers(t) {
    const workers = this.snap.workers.filter((w) => w.bot === this.selected).slice(0, 8);
    if (!workers.length) return div();
    return v_flex().px(12).py(8).gap(4).flex_shrink_0()
      .child(div().text_size(11).text_color(t.dim).child("WORKERS"))
      .children(workers.map((w) => h_flex().gap(6).items_center()
        .child(this.dot(t, w.status === "running" ? "working" : w.status === "done" ? "idle" : "needs_you"))
        .child(div().text_size(11.5).child(w.name))
        .child(div().flex_1().min_w(0).text_size(11).text_color(t.muted).truncate().child(w.pr_url ?? w.branch))));
  }

  renderInput(t) {
    const answering = this.snap?.items.find((i) => i.key === this.answering);
    const bot = this.snap?.bots.find((b) => b.id === this.selected);
    const label = answering ? `Reply to ${answering.botName}'s question (#${answering.num})` : bot ? `Message ${this.label(bot)}` : "No bot selected";
    return h_flex().p(8).gap(6).flex_none().border_t_1().border_color(t.border).items_end()
      .when(Boolean(this.answering), (el) => el.child(this.button(t, "unanswer", "×", (cx) => { this.answering = null; cx.notify(); })))
      .child(v_flex().flex_1().gap(2)
        .child(div().text_size(10.5).text_color(t.dim).child(label))
        .child(div().on_key_down(textKeys(this.input))
          .child(Input.new(this.input).h(32).px(8).rounded(8).border_1().border_color(t.input).bg(t.background).text_size(12.5))))
      .child(this.button(t, "send", "Send", (cx) => this.send(cx), true));
  }

  button(t, id, text, action, primary = false) {
    return div().id(id).px(8).py(3).rounded(6).text_size(11).cursor_pointer().flex_none()
      .bg(primary ? t.solid : t.raised).text_color(primary ? t.solid_foreground : t.foreground)
      .hover((s) => s.opacity(0.85))
      .on_mouse_down("left", (_e, cx) => cx.stop_propagation())
      .on_click((_e, cx) => { cx.stop_propagation(); action(cx); })
      .child(text);
  }
}
