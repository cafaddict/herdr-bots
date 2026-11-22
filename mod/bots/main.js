// Bots: the herdr-bots daemon's bots, their work items and workers, and a chat box.
// The daemon writes ~/.herdr-bots/state/status.json on every change; this panel watches that
// folder and calls the `herdr-bots` CLI for actions. A bot's live agent opens as a window.

import { View, div } from "gpui-kit";
import { v_flex, h_flex, InputState, Input, TextView } from "gpui-base";
import {
  exec, home, checking, readTextFile, watchFolder, terminals, openOverlay, closeOverlay,
  notify, setBadge, playSound, props, theme,
} from "herdr";
import { BIN } from "./config.js";

const SPINNER = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const stateDir = () => `${home()}/.herdr-bots/state`;
const terminalOf = (bot) => terminals().find((t) => t.name === `bot-${bot}` && t.state === "live");

export default class Bots extends View {
  init(_props, cx) {
    this.titlebar = props()?.titlebar ?? null;
    if (this.titlebar) return; // the window title bar copy needs no data
    this.snap = null;
    this.error = null;
    this.selected = null;
    this.answering = null; // a needs_you item id the input answers
    this.seenQuestions = new Set();
    this.firstLoad = true;
    this.input = InputState.new({ placeholder: "Message a bot…" });
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
  }

  reload(cx) {
    cx.spawn(async (cx) => {
      const text = await readTextFile(`${stateDir()}/status.json`).catch(() => null);
      if (!text) { this.snap = null; cx.notify(); return; }
      try { this.snap = JSON.parse(text); } catch { return; } // mid-write; the next change rereads
      if (!this.selected || !this.snap.bots.some((b) => b.id === this.selected)) this.selected = this.snap.bots[0]?.id ?? null;
      this.announce();
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

  openBot(bot) {
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
    if (this.titlebar) return this.renderTitlebar(t);
    const root = v_flex().id("bots").size_full().bg(t.chrome).text_color(t.foreground).text_size(12.5);
    if (checking()) return root.child(div().p(12).child("Bots"));
    const header = h_flex().h(36).px(12).items_center().justify_between().border_b_1().border_color(t.border)
      .child(div().font_semibold().child("Bots"))
      .child(h_flex().gap(8).items_center()
        .child(div().text_size(11).text_color(t.dim).child(this.snap ? `v${this.snap.version}` : ""))
        // Opened as a window (`mod.open bots`): it needs its own close button.
        .when(Boolean(props()?.overlay), (el) => el.child(div().id("bots-close").px(6).rounded(6).cursor_pointer()
          .text_color(t.muted).hover((s) => s.bg(t.hover))
          .on_mouse_down("left", (_e, cx) => cx.stop_propagation())
          .on_click(() => closeOverlay(props().overlay)).child("✕"))));
    if (!this.snap) {
      return root.child(header).child(v_flex().p(12).gap(6).text_color(t.muted)
        .child("The herdr-bots daemon isn't running.")
        .child(div().text_color(t.dim).child("Run `herdr-bots install-service`, then `herdr-bots new <id>`.")));
    }
    return root.child(header)
      .child(this.renderBots(t))
      .child(v_flex().flex_1().min_h(0).overflow_y_scrollbar().child(this.renderItems(t)).child(this.renderWorkers(t)))
      .when(Boolean(this.error), (el) => el.child(div().px(12).py(6).text_size(11.5).text_color(t.blocked).child(this.error)))
      .child(this.renderInput(t));
  }

  renderTitlebar(t) {
    const { bot } = this.titlebar;
    return h_flex().id("bots-title").size_full().px(10).items_center().justify_between().bg(t.chrome)
      .border_b_1().border_color(t.border).text_size(12).text_color(t.foreground)
      .child(div().font_semibold().child(`bot · ${bot}`))
      .child(div().id("bots-title-close").px(8).rounded(6).cursor_pointer().text_color(t.muted)
        .hover((s) => s.bg(t.hover))
        .on_mouse_down("left", (_e, cx) => cx.stop_propagation())
        .on_click(() => closeOverlay(props().overlay)).child("✕"));
  }

  dot(t, state) {
    if (state === "working") return div().w(14).text_color(t.working).frames(SPINNER, 90);
    const color = { needs_you: t.blocked, planned: t.muted, idle: t.idle }[state] ?? t.unknown;
    return div().w(14).text_color(color).child("●");
  }

  renderBots(t) {
    const rows = this.snap.bots.map((b) => {
      const selected = b.id === this.selected;
      const counts = [b.needs_you && `${b.needs_you} needs you`, b.working && "working", b.planned && `${b.planned} queued`]
        .filter(Boolean).join(" · ");
      return h_flex().id(`bot-${b.id}`).px(12).py(6).gap(6).items_center().cursor_pointer()
        .bg(selected ? t.selected : t.chrome).hover((s) => s.bg(selected ? t.selected : t.hover))
        .on_click((_e, cx) => { this.selected = b.id; this.answering = null; cx.notify(); })
        .child(this.dot(t, b.paused ? "planned" : b.state))
        .child(v_flex().flex_1().min_w(0)
          .child(h_flex().gap(6).child(div().font_medium().truncate().child(b.id))
            .child(div().text_size(11).text_color(t.dim).child(`${b.agent}${b.paused ? " · paused" : ""}`)))
          .when(Boolean(counts), (el) => el.child(div().text_size(11).text_color(t.muted).truncate().child(counts))))
        .when(selected, (el) => el
          .child(this.button(t, `pause-${b.id}`, b.paused ? "Resume" : "Pause", (cx) => this.run(cx, [b.paused ? "resume" : "pause", b.id])))
          .child(this.button(t, `open-${b.id}`, "Open", () => this.openBot(b.id))));
    });
    const broken = Object.entries(this.snap.bot_errors ?? {}).map(([id, e]) =>
      div().px(12).py(4).text_size(11).text_color(t.blocked).truncate().child(`${id}: ${e}`));
    if (!rows.length && !broken.length) {
      return div().p(12).text_color(t.muted).child("No bots yet. Run `herdr-bots new <id>`.");
    }
    return v_flex().py(4).border_b_1().border_color(t.border).children([...rows, ...broken]);
  }

  renderItems(t) {
    const items = this.snap.items.filter((i) => i.bot === this.selected).slice(0, 20);
    if (!items.length) return div().p(12).text_color(t.dim).child("Nothing yet. Say something below.");
    return v_flex().children(items.map((i) => this.renderItem(t, i)));
  }

  renderItem(t, i) {
    const color = { needs_you: t.blocked, working: t.working, planned: t.muted, done: t.done, canceled: t.dim }[i.status];
    const answering = this.answering === i.id;
    const reply = (i.reply ?? "").trim();
    // flex_shrink_0: inside the scroll area, rows keep their height instead of squeezing.
    return v_flex().id(`item-${i.id}`).flex_shrink_0().px(12).py(8).gap(4).border_b_1().border_color(t.border)
      .bg(answering ? t.raised : t.chrome)
      .child(h_flex().gap(6).items_center()
        .child(div().text_size(11).text_color(t.dim).child(`#${i.id}`))
        .child(div().text_size(11).text_color(color).child(i.status.replace("_", " ")))
        .child(div().text_size(11).text_color(t.dim).child(i.source))
        .child(div().flex_1())
        .when(!["done", "canceled"].includes(i.status), (el) => el
          .child(this.button(t, `done-${i.id}`, "Done", (cx) => this.run(cx, ["done", String(i.id)])))
          .child(this.button(t, `cancel-${i.id}`, "Cancel", (cx) => this.run(cx, ["cancel", String(i.id)])))))
      .child(div().flex_shrink_0().font_medium().truncate().child(i.title))
      .when(Boolean(i.question), (el) => el.child(
        h_flex().id(`ask-${i.id}`).gap(6).p(6).rounded(6).bg(t.background).cursor_pointer()
          .on_click((_e, cx) => { this.answering = i.id; this.input.focus(); cx.notify(); })
          .child(div().text_color(t.blocked).child("?"))
          .child(div().flex_1().whitespace_normal().child(i.question))
          .child(div().text_size(11).text_color(t.accent).child(answering ? "answering" : "Answer"))))
      .when(Boolean(reply), (el) => el.child(div().flex_shrink_0().max_h(160).overflow_y_scrollbar().text_size(12)
        .child(TextView.markdown(`reply-${i.id}`, reply.length > 2000 ? `${reply.slice(0, 2000)}…` : reply))))
      .when(Boolean(i.url), (el) => el.child(div().text_size(11).text_color(t.dim).truncate().child(i.url)));
  }

  renderWorkers(t) {
    const workers = this.snap.workers.filter((w) => w.bot === this.selected).slice(0, 8);
    if (!workers.length) return div();
    return v_flex().px(12).py(8).gap(4)
      .child(div().text_size(11).text_color(t.dim).child("WORKERS"))
      .children(workers.map((w) => h_flex().gap(6).items_center()
        .child(this.dot(t, w.status === "running" ? "working" : w.status === "done" ? "idle" : "needs_you"))
        .child(div().text_size(11.5).child(w.name))
        .child(div().flex_1().min_w(0).text_size(11).text_color(t.muted).truncate().child(w.pr_url ?? w.branch))));
  }

  renderInput(t) {
    const placeholder = this.answering ? `Answer #${this.answering}` : this.selected ? `Message ${this.selected}` : "No bot selected";
    return h_flex().p(8).gap(6).border_t_1().border_color(t.border).items_center()
      .when(Boolean(this.answering), (el) => el.child(this.button(t, "unanswer", "×", (cx) => { this.answering = null; cx.notify(); })))
      .child(v_flex().flex_1().gap(2)
        .child(div().text_size(10.5).text_color(t.dim).child(placeholder))
        .child(Input.new(this.input).h(32).px(8).rounded(8).border_1().border_color(t.input).bg(t.background).text_size(12.5)))
      .child(this.button(t, "send", "Send", (cx) => this.send(cx), true));
  }

  button(t, id, text, action, primary = false) {
    return div().id(id).px(8).py(3).rounded(6).text_size(11).cursor_pointer()
      .bg(primary ? t.solid : t.raised).text_color(primary ? t.solid_foreground : t.foreground)
      .hover((s) => s.opacity(0.85))
      .on_click((_e, cx) => { cx.stop_propagation(); action(cx); })
      .child(text);
  }
}
