// Bots: the herdr-bots daemon's bots, their work items and workers.
//
// One mod, three shapes (chosen by props):
//   { sidebar: true }  a compact list for the left sidebar, above Agents; a click opens the chat
//   { chat: true }     the right-hand panel (beside Files/Changes): the bot in shared "bots.chat"
//   { chat: "<bot>" }  the same conversation as a window, for layouts without that panel
//   { newBot: true }   the "new bot" dialog
//   { deleteBot: id }  the "delete bot?" dialog
//   (none)             everything at once, as a window (`mod.open bots`)
//
// The daemon writes ~/.herdr-bots/state/status.json on every change; this mod watches that
// folder and calls the `herdr-bots` CLI for actions. A bot's live agent opens as a window.

import { View, div } from "gpui-kit";
import { v_flex, h_flex, InputState, Input, TextView } from "gpui-base";
import {
  exec, home, checking, readTextFile, watchFolder, terminals, openOverlay, closeOverlay,
  notify, setBadge, playSound, props, theme, layout, run, shared, setShared,
} from "herdr";
import { BIN } from "./config.js";

const SPINNER = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const stateDir = () => `${home()}/.herdr-bots/state`;
const terminalOf = (bot) => terminals().find((t) => t.name === `bot-${bot}` && t.state === "live");

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

function openNewBot() {
  openOverlay({ mod: "bots", kind: "dialog", width: 420, props: { newBot: true } });
}

export default class Bots extends View {
  init(_props, cx) {
    const p = props() ?? {};
    this.mode = p.titlebar ? "titlebar" : p.newBot ? "new" : p.deleteBot ? "delete" : p.chat ? "chat" : p.sidebar ? "sidebar" : "full";
    this.inPanel = p.chat === true; // follows shared "bots.chat" instead of a fixed bot
    if (this.mode === "titlebar") return; // a terminal window's title bar needs no data
    if (this.mode === "new") return this.initNew(cx);
    if (this.mode === "delete") return this.initDelete(cx);
    this.snap = null;
    this.error = null;
    this.selected = this.inPanel ? (shared("bots.chat") ?? null) : (p.chat ?? null);
    this.answering = null; // the needs_you item the message box answers
    this.seenQuestions = new Set();
    this.firstLoad = true;
    this.input = InputState.new({ placeholder: "Message…" });
    this.input.on("submit", (_e, cx) => this.send(cx));
    if (checking()) return;
    this.reload(cx);
    cx.spawn(async (cx) => {
      for (;;) {
        try { await watchFolder(stateDir()); } catch { await new Promise((r) => cx.timer.after(5000, r)); }
        this.reload(cx);
      }
    });
    // The folder may not exist until the daemon first starts; poll gently as a fallback.
    cx.timer.every(15000, (cx) => this.reload(cx));
    if (this.mode === "chat") cx.timer.after(300, () => this.input.focus());
  }

  reload(cx) {
    cx.spawn(async (cx) => {
      const text = await readTextFile(`${stateDir()}/status.json`).catch(() => null);
      if (!text) { this.snap = null; cx.notify(); return; }
      try { this.snap = JSON.parse(text); } catch { return; } // mid-write; the next change rereads
      if (this.mode === "full" && !this.snap.bots.some((b) => b.id === this.selected)) this.selected = this.snap.bots[0]?.id ?? null;
      // The sidebar copy is always shown, so it alone owns the badge and the alerts.
      if (this.mode === "sidebar") this.announce();
      cx.notify();
    });
  }

  // Dock badge = open questions; a sound and a toast for each new one.
  announce() {
    const asking = this.snap.items.filter((i) => i.status === "needs_you");
    setBadge(asking.length ? String(asking.length) : null);
    for (const item of asking) {
      if (this.seenQuestions.has(item.id)) continue;
      this.seenQuestions.add(item.id);
      if (this.firstLoad) continue;
      playSound("blocked");
      notify({ title: `${item.bot} needs you`, body: item.question ?? item.title, level: "warning" });
    }
    this.firstLoad = false;
  }

  send(cx) {
    const text = this.input.value().trim();
    if (!text || !this.selected) return;
    const args = ["ask", this.selected, text];
    if (this.answering) args.push("--item", String(this.answering));
    this.input.set_value("");
    this.answering = null;
    this.run(cx, args);
  }

  run(cx, args) {
    cx.spawn(async (cx) => {
      const r = await exec(BIN, args).catch((e) => ({ code: 1, stderr: String(e) }));
      this.error = r.code === 0 ? null : (r.stderr || r.stdout || "failed").trim();
      cx.notify();
    });
  }

  openTerminal(bot) {
    const t = terminalOf(bot);
    if (!t) { notify({ title: `${bot} isn't running`, body: "It starts with its first message.", level: "info" }); return; }
    openOverlay({
      kind: "window", key: `bot-${bot}`, width: 760, height: 520,
      layout: { type: "column", children: [
        { type: "block", size: { px: 30 }, content: { mod: "bots", props: { titlebar: { bot } } } },
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
      default: return this.renderFull(t);
    }
  }

  // ---------- sidebar ----------

  renderSidebar(t) {
    const bots = this.snap?.bots ?? [];
    const root = v_flex().id("bots-side").size_full().text_size(13).text_color(t.foreground);
    if (narrow()) {
      return root.pt(6).children(bots.map((b) => h_flex().id(`sb-${b.id}`).h(26).px(8).flex_none().items_center()
        .justify_end().hover((s) => s.bg(t.hover)).on_click(() => openChat(b.id)).tooltip(b.id)
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
      body = div().px(16).text_size(12).text_color(t.dim).child(checking() ? "" : "herdr-bots isn't running. Run herdr-bots install-service.");
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
    return h_flex().id(`sb-${b.id}`).h(34).px(8).gap(8).flex_none().items_center().rounded(6).cursor_pointer()
      .when(open, (el) => el.bg(t.selected))
      .hover((s) => s.bg(open ? t.selected : t.hover))
      .on_click(() => openChat(b.id))
      .child(h_flex().w(14).flex_none().justify_center().child(this.dot(t, b.paused ? "paused" : b.state)))
      .child(v_flex().flex_1().min_w(0)
        .child(div().text_size(13).truncate().child(b.id))
        .child(div().text_size(11).truncate().text_color(question ? t.blocked : t.dim).child(sub)))
      .when(b.needs_you > 0, (el) => el.child(div().flex_none().px(6).rounded_full().bg(t.blocked)
        .text_size(10.5).text_color(t.background).child(String(b.needs_you))));
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
      .child(div().flex_none().font_semibold().child(this.selected ?? "Bots"))
      // The details give way first when the panel is narrow; the name never does.
      .child(div().flex_1().min_w(0).text_size(11).text_color(t.dim).truncate().child(bot ? `${bot.agent} · ${bot.permission}` : ""))
      .when(Boolean(bot), (el) => el
        .child(this.button(t, "chat-pause", bot.paused ? "Resume" : "Pause", (cx) => this.run(cx, [bot.paused ? "resume" : "pause", bot.id])))
        .child(this.button(t, "chat-term", "Terminal", () => this.openTerminal(bot.id)))
        .child(this.button(t, "chat-delete", "Delete", () => openOverlay({ mod: "bots", kind: "dialog", width: 420, props: { deleteBot: bot.id } }))))
      .when(!this.selected, (el) => el.child(this.button(t, "chat-new", "+ New bot", () => openNewBot())))
      .child(this.closeButton(t));
    if (!this.snap) return root.child(header).child(div().p(12).text_color(t.muted).child(checking() ? "" : "herdr-bots isn't running."));
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
      .child(v_flex().flex_1().min_h(0).overflow_y_scrollbar().child(this.renderItems(t, true)).child(this.renderWorkers(t)))
      .when(Boolean(this.error), (el) => el.child(div().px(12).py(6).text_size(11.5).text_color(t.blocked).child(this.error)))
      .child(this.renderInput(t));
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
          .child(h_flex().gap(6).child(div().font_medium().truncate().child(b.id))
            .child(div().text_size(11).text_color(t.dim).child(`${b.agent}${b.paused ? " · paused" : ""}`)))
          .when(Boolean(counts), (el) => el.child(div().text_size(11).text_color(t.muted).truncate().child(counts))))
        .when(selected, (el) => el
          .child(this.button(t, `chat-${b.id}`, "Chat", () => openChat(b.id)))
          .child(this.button(t, `term-${b.id}`, "Terminal", () => this.openTerminal(b.id))));
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
    this.target = props().deleteBot;
    this.force = false;
    this.busy = false;
    this.error = null;
    this.workers = 0;
    this.handle = cx.focus_handle();
    if (checking()) return;
    cx.timer.after(100, () => this.handle.focus());
    cx.spawn(async (cx) => {
      const text = await readTextFile(`${stateDir()}/status.json`).catch(() => null);
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
      const r = await exec(BIN, args).catch((e) => ({ code: 1, stderr: String(e) }));
      this.busy = false;
      if (r.code !== 0) { this.error = (r.stderr || r.stdout || "failed").trim(); cx.notify(); return; }
      if (shared("bots.chat") === this.target) setShared("bots.chat", null);
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
      .child(div().text_size(15).font_semibold().child(`Delete ${this.target}?`))
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

  // ---------- new bot dialog ----------

  initNew(cx) {
    this.newId = InputState.new({ placeholder: "reviewer" });
    this.newRole = InputState.new({ placeholder: "Reviews my PRs and summarizes what to fix" });
    this.newAgent = "claude";
    this.newPermission = "full-access";
    this.error = null;
    this.busy = false;
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
    cx.spawn(async (cx) => {
      const r = await exec(BIN, args).catch((e) => ({ code: 1, stderr: String(e) }));
      this.busy = false;
      if (r.code !== 0) { this.error = (r.stderr || r.stdout || "failed").trim(); cx.notify(); return; }
      closeOverlay(props().overlay);
      openChat(id);
    });
  }

  renderNew(t) {
    const label = (text) => div().pt(14).pb(6).text_size(11.5).text_color(t.dim).child(text);
    const field = (state) => Input.new(state).h(34).px(10).rounded(8).border_1().border_color(t.input).bg(t.background).text_size(12.8);
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

  dot(t, state) {
    if (state === "working") return div().w(14).text_color(t.working).frames(SPINNER, 90);
    const color = { needs_you: t.blocked, planned: t.muted, idle: t.idle, paused: t.dim }[state] ?? t.unknown;
    return div().w(14).text_color(color).child(state === "paused" ? "❙❙" : "●");
  }

  // Chat: oldest first, newest just above the message box. Panel: open items first.
  renderItems(t, chatOrder) {
    let items = this.snap.items.filter((i) => i.bot === this.selected);
    items = chatOrder ? [...items].sort((a, b) => a.id - b.id).slice(-20) : items.slice(0, 20);
    if (!items.length) return div().p(12).text_color(t.dim).child("Nothing yet. Say something below.");
    return v_flex().children(items.map((i) => this.renderItem(t, i)));
  }

  renderItem(t, i) {
    const color = { needs_you: t.blocked, working: t.working, planned: t.muted, done: t.done, canceled: t.dim }[i.status];
    const answering = this.answering === i.id;
    const reply = (i.reply ?? "").trim();
    // flex_shrink_0: inside the scroll area, rows keep their height instead of squeezing.
    return v_flex().id(`item-${i.id}`).flex_shrink_0().px(12).py(8).gap(4).border_b_1().border_color(t.border)
      .when(answering, (el) => el.bg(t.raised))
      .child(h_flex().gap(6).items_center()
        .child(div().text_size(11).text_color(t.dim).child(`#${i.id}`))
        .child(div().text_size(11).text_color(color).child(i.status.replace("_", " ")))
        .child(div().text_size(11).text_color(t.dim).child(i.source))
        .child(div().flex_1())
        .when(!["done", "canceled"].includes(i.status), (el) => el
          .child(this.button(t, `done-${i.id}`, "Done", (cx) => this.run(cx, ["done", String(i.id)])))
          .child(this.button(t, `cancel-${i.id}`, "Cancel", (cx) => this.run(cx, ["cancel", String(i.id)])))))
      .child(div().flex_shrink_0().font_medium().whitespace_normal().child(i.title))
      .when(Boolean(i.question), (el) => el.child(
        h_flex().id(`ask-${i.id}`).gap(6).p(6).rounded(6).bg(t.background).cursor_pointer()
          .on_click((_e, cx) => { this.answering = i.id; this.input.focus(); cx.notify(); })
          .child(div().text_color(t.blocked).child("?"))
          .child(div().flex_1().whitespace_normal().child(i.question))
          .child(div().text_size(11).text_color(t.accent).child(answering ? "answering" : "Answer"))))
      .when(Boolean(reply), (el) => el.child(div().flex_shrink_0().max_h(240).overflow_y_scrollbar().text_size(12)
        .child(TextView.markdown(`reply-${i.id}`, reply.length > 4000 ? `${reply.slice(0, 4000)}…` : reply))))
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
    const label = this.answering ? `Answer #${this.answering}` : this.selected ? `Message ${this.selected}` : "No bot selected";
    return h_flex().p(8).gap(6).flex_none().border_t_1().border_color(t.border).items_end()
      .when(Boolean(this.answering), (el) => el.child(this.button(t, "unanswer", "×", (cx) => { this.answering = null; cx.notify(); })))
      .child(v_flex().flex_1().gap(2)
        .child(div().text_size(10.5).text_color(t.dim).child(label))
        .child(Input.new(this.input).h(32).px(8).rounded(8).border_1().border_color(t.input).bg(t.background).text_size(12.5)))
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
