// Read-only console pane (QPlainTextEdit#outputPane, maximumBlockCount 20000).
// Text lives in a list of text nodes so appends are cheap and the line cap trims
// from the front without re-rendering everything.

import { OUTPUT_MAX_LINES, capLines } from "./logic.js";

const countNl = (s) => {
  let n = 0;
  for (let i = s.indexOf("\n"); i !== -1; i = s.indexOf("\n", i + 1)) n++;
  return n;
};

export class OutputPane {
  constructor(el, maxLines = OUTPUT_MAX_LINES) {
    this.el = el;
    this.max = maxLines;
    this.nodes = []; // [{node, nl}]
    this.nl = 0; // newline count = blocks - 1
    this.pending = "";
    this.frame = 0;
    el.addEventListener("keydown", (e) => {
      if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "a") {
        e.preventDefault();
        const r = document.createRange();
        r.selectNodeContents(el);
        const sel = getSelection();
        sel.removeAllRanges();
        sel.addRange(r);
      }
    });
  }

  atBottom() {
    const e = this.el;
    return e.scrollTop + e.clientHeight >= e.scrollHeight - 2;
  }

  /** Replace all text and scroll to the end (`_render_output`). */
  setText(text) {
    clearTimeout(this.frame);
    this.frame = 0;
    this.pending = "";
    this.el.textContent = "";
    this.nodes = [];
    this.nl = 0;
    this._add(capLines(text, this.max));
    this.el.scrollTop = this.el.scrollHeight;
  }

  clear() {
    this.setText("");
  }

  text() {
    return this.pending ? this.el.textContent + this.pending : this.el.textContent;
  }

  /**
   * Append, batched (a timer rather than requestAnimationFrame so a minimised window
   * keeps trimming to the line cap); follows the end only if the view is already there.
   */
  append(text) {
    if (!text) return;
    this.pending += text;
    if (!this.frame) this.frame = setTimeout(() => this.flush(), 16);
  }

  flush() {
    clearTimeout(this.frame);
    this.frame = 0;
    if (!this.pending) return;
    const follow = this.atBottom();
    const t = this.pending;
    this.pending = "";
    this._add(t);
    if (follow) this.el.scrollTop = this.el.scrollHeight;
  }

  _add(text) {
    if (!text) return;
    const node = document.createTextNode(text);
    const nl = countNl(text);
    this.el.appendChild(node);
    this.nodes.push({ node, nl });
    this.nl += nl;
    // blocks = nl + 1 must stay <= max
    while (this.nl + 1 > this.max) {
      const first = this.nodes[0];
      if (first.nl === 0) {
        // a partial line: join it with the next node and continue there
        const next = this.nodes[1];
        next.node.data = first.node.data + next.node.data;
        first.node.remove();
        this.nodes.shift();
        continue;
      }
      const k = Math.min(this.nl + 1 - this.max, first.nl);
      const s = first.node.data;
      if (k === first.nl && s.endsWith("\n") && this.nodes.length > 1) {
        first.node.remove();
        this.nodes.shift();
      } else {
        let cut = 0;
        for (let i = 0; i < k; i++) cut = s.indexOf("\n", cut) + 1;
        first.node.data = s.slice(cut);
        first.nl -= k;
      }
      this.nl -= k;
    }
  }
}
