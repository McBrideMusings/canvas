Canvas is the document side of this chat. The chat carries the answer in full, every turn. A card is a rendered page the user keeps open beside the terminal, so it earns a post only when it carries something chat can't:
- a diagram of a structure or flow (call chain, architecture, layout, plan, state machine)
- a chart, or a table of numbers that need scanning
- an image or screenshot, or a before/after side by side
- a rendered diff, or code with annotations
- a control the user answers from (see `canvas wait` in the canvas skill)

Volume is not a reason. A file you changed, code over 15 lines, a list of links, an options list, a closing report or test steps posted as Markdown blocks is the chat text in a second place, so it stays in chat. Before you post, name the figure the card adds; if you can't, answer in chat.

When a card does earn its place, build it as a document, not a note: HTML with panels or sections, headings that tell the whole story when read alone, and the structure drawn inline as SVG or CSS rather than described in prose. Load the canvas skill first; it says how. Markdown is for one thing: a status checklist you keep current with `canvas post --update <card_id>` (the id your first post's JSON output reported), which every open viewer shows in place. A local path becomes a clickable file when written as a link (`[plan](/abs/plan.md)`) and an inline image when written as an image (`![](/abs/shot.png)`); a bare path stays plain text.

When the user says "show me" or asks for a design, a comparison or a structure, that is a request for a card, and it still gets the figure: draw it. Options that read fine as text stay in chat.

A document another skill builds for the user to read (an explainer, a report, a set of mockup screenshots) is already a card: post the file it wrote with `canvas post`, whatever that skill says about opening it, and don't open it as well. A dashboard that changes while you work is one card: post its HTML once, then push new values into the running page with `canvas data <card_id> <file|->` rather than reposting, so it keeps its scroll and state.

Work handed back to you counts as yours. A subagent's or delegated session's findings, screenshots and numbers reach you as plain text, and the same test decides whether to post them. Subagents never receive this guidance.

In HTML, Canvas picks the light or dark theme, not the post, and it forces the page's background and text color to match. Don't set `html` or `body` background or color. Leave colors you don't need to choose unset so they inherit, and where you do paint a background (a callout, a table header) set a `color` beside it. To adapt to both themes, define a light palette on `:root` and override it under `@media (prefers-color-scheme: dark)`; Canvas makes that query match the active theme. Text that ends up unreadable against its background is recolored automatically.

In HTML, a `<pre>` block renders dark with light text; give any background you paint inside one (diff rows, highlights) a dark `color` of its own.

Size a card on purpose. The viewer sets the width (846px of content at the default window size, and never wider); you set the height. A document can run several screens; each panel should still read in about one. Clicking any image opens it full size in a lightbox, so screenshots can sit smaller in the card (`<img src="/abs/shot.png" width="360">`, or side by side for a before/after).
