---
name: summarize
description: Condense the current conversation, a document, or any provided content into a brief, scannable briefing — emoji-headed bullets, plus rich colored status boxes (green "done" / amber "needs you" / red blockers) via the visualize widget, with an interactive, tickable action checklist you can add items to. Use when the user types "/summarize", or says "summarize this", "tl;dr", "give me the brief/short version", "condense this", "boil this down", "make this scannable", or wants findings/a discussion distilled. Plain markdown body, never HTML or PDF files.
---

# Summarize

Produce a briefing the reader grasps in seconds: a tight scannable text summary, capped by colored status boxes — including an interactive checklist of what the human still needs to do.

## What to summarize

- `/summarize` with no target → summarize **this conversation's** findings, decisions, and open items.
- `/summarize <topic / pasted text / file>` → summarize that specific thing.

## Output structure (two parts)

**Part 1 — the scannable summary (markdown response text):**
- **Emoji section headers** — one *relevant* emoji per group (📦 🐛 ⚡ ✅ ⚠️ 🔧 🗓️ 🚀 …). Match the content; never decorative.
- **Short bullets, no paragraphs.** Past ~2 lines → split or cut.
- **Bold the key term** at the start of each bullet so the eye scans labels.
- **Group under 3–7 headers**, ordered by importance. Lead with the conclusion / root cause.
- Use `→` for cause→effect. Keep specifics: file:line, names, numbers, IDs.
- Cut hedging, transitions, restated context.

**Part 1 — the status boxes (a `visualize` widget, rendered after the text):**
Render a single `mcp__visualize__show_widget` with vertically stacked role-colored boxes:
- 🟢 **Done / wins** (`--bg-success`) — completed or confirmed working. Include only if there are real wins.
- 🔴 **Blockers / risks** (`--bg-danger`) — only if something is broken or blocking. Omit otherwise.
- 🔵 **Key facts** (`--bg-accent`) — optional, the one or two things to remember.
- 🟡 **Needs you** (`--bg-warning`) — **always last, always present if there's any human action.** Render as an **interactive checklist** (below). The payoff box.

### The "Needs you" interactive checklist
- Each action item is a `<label>` with a native `<input type="checkbox">`. On toggle: strike through + dim the item, and update a live `N / M done` readout and progress bar.
- An **add-item row** (text input + ➕ button, also fires on Enter) appends new action items live; they join the count and the save payload.
- A **"Save progress ↗" button** calls `sendPrompt(...)` with the current done/remaining items (including any added) + an instruction to re-render. This is how state persists across the chat.
- When you (the model) receive a "Update my /summarize checklist…" message from that button, re-render the widget with done items checked / moved to the 🟢 Done box, and keep any newly added items. You are the source of truth for persisted state.

### Widget mechanics (follow exactly)
- Call `mcp__visualize__read_me` (modules `["mockup"]`) **once per session before the first** `show_widget`. Don't narrate that call.
- **Inside the widget: no emoji** — use Tabler outline icons (`ti ti-circle-check`, `ti ti-alert-triangle`, `ti ti-flag`, `ti ti-info-circle`, `ti ti-plus`). Emoji stay in Part-1 text only. Icon-only buttons get `aria-label`.
- Use **CSS variables** for all color: box `background: var(--bg-{role})`, `border: 0.5px solid var(--border-{role})`, `border-radius: 12px`, `padding: 1rem 1.25rem`; text/icon `color: var(--text-{role})`.
- Start with a visually-hidden `<h2 class="sr-only">` one-line summary. Sentence case. No paragraphs/titles inside the widget. `<script>` goes last. Query checkboxes **fresh** (use delegation on `#list`) so added items are included.
- Checklist skeleton:
  ```html
  <div style="background:var(--bg-warning); border:0.5px solid var(--border-warning); border-radius:12px; padding:1rem 1.25rem;">
    <div style="display:flex; align-items:center; justify-content:space-between; margin-bottom:10px;">
      <div style="display:flex; align-items:center; gap:8px;">
        <i class="ti ti-flag" style="font-size:20px; color:var(--text-warning);" aria-hidden="true"></i>
        <span style="font-weight:500; color:var(--text-warning);">Needs you</span>
      </div>
      <span id="prog" style="font-size:13px; color:var(--text-warning);">0 / N done</span>
    </div>
    <div style="height:4px; background:var(--border-warning); border-radius:2px; overflow:hidden; margin-bottom:12px;">
      <div id="bar" style="height:100%; width:0%; background:var(--text-warning); transition:width .2s;"></div>
    </div>
    <div id="list" style="display:flex; flex-direction:column; gap:10px;">
      <label style="display:flex; align-items:flex-start; gap:10px; cursor:pointer; color:var(--text-warning); line-height:1.5;">
        <input type="checkbox" style="margin-top:3px; accent-color:var(--text-warning);"><span>…</span>
      </label>
    </div>
    <div style="display:flex; gap:8px; margin-top:12px;">
      <input id="newitem" type="text" placeholder="Add an action…" style="flex:1;">
      <button id="add" aria-label="Add item"><i class="ti ti-plus" aria-hidden="true"></i></button>
    </div>
    <button id="save" style="margin-top:8px;">Save progress ↗</button>
  </div>
  <script>
  (function(){
    var list=document.getElementById('list'), prog=document.getElementById('prog'), bar=document.getElementById('bar');
    function boxes(){ return Array.prototype.slice.call(list.querySelectorAll('input[type=checkbox]')); }
    function upd(){ var bs=boxes(), d=0; bs.forEach(function(b){ var s=b.nextElementSibling;
        if(b.checked){d++; s.style.textDecoration='line-through'; s.style.opacity='0.55';}
        else {s.style.textDecoration='none'; s.style.opacity='1';} });
      prog.textContent=d+' / '+bs.length+' done'; bar.style.width=(bs.length?Math.round(d/bs.length*100):0)+'%'; }
    list.addEventListener('change', upd);
    function addItem(t){ t=(t||'').trim(); if(!t) return;
      var l=document.createElement('label'); l.style.cssText='display:flex; align-items:flex-start; gap:10px; cursor:pointer; color:var(--text-warning); line-height:1.5;';
      var c=document.createElement('input'); c.type='checkbox'; c.style.cssText='margin-top:3px; accent-color:var(--text-warning);';
      var s=document.createElement('span'); s.textContent=t; l.appendChild(c); l.appendChild(s); list.appendChild(l); upd(); }
    var ni=document.getElementById('newitem');
    document.getElementById('add').addEventListener('click', function(){ addItem(ni.value); ni.value=''; ni.focus(); });
    ni.addEventListener('keydown', function(e){ if(e.key==='Enter'){ addItem(ni.value); ni.value=''; } });
    document.getElementById('save').addEventListener('click', function(){
      var done=[], todo=[]; boxes().forEach(function(b){ (b.checked?done:todo).push(b.nextElementSibling.textContent.trim()); });
      if(window.sendPrompt) sendPrompt('Update my /summarize checklist. Done: '+(done.join('; ')||'none')+'. Remaining: '+(todo.join('; ')||'none')+'. Re-render the status boxes with the done items checked and moved to the Done box.');
    });
    upd();
  })();
  </script>
  ```

### Persistence reality (state the limit honestly if asked)
Live ticks and added items hold only while the rendered widget is on screen and reset on a fresh summary; the widget can't report state back on its own. Durable tracking comes only from the **Save progress** button (sendPrompt → you re-render) — you hold the state.

### Fallback — client can't render widgets (plain terminal CLI)
Skip `show_widget`; emit the boxes as markdown, action items as a **task list**:
- `> ✅ **Done** — …`
- `> 🚫 **Blockers** — …` (only if any)
- Action items as `- [ ]` / `- [x]` under a `> ⚠️ **Needs you**` heading; the user adds/updates by telling you.

## Anti-patterns
- ❌ Rivers of text — multi-sentence bullets that re-explain context
- ❌ Emoji inside the widget (use Tabler icons there)
- ❌ Empty boxes — omit a role box with no items (but keep "Needs you" whenever there's an action)
- ❌ Dropping paths / numbers / IDs for the sake of brevity
- ❌ Generating an HTML page or PDF file (the widget is inline, not a file)
- ❌ Promising checks persist on their own — only the Save-progress round-trip persists
