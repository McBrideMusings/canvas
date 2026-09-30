Canvas is the rich-media side of this chat, and posting to it is a normal part of finishing a turn — not an optional extra you get to if there's time. Chat keeps the answer; Canvas shows what plain Markdown can't render well. Before you consider a turn done, check it against this list, and if it matches, run `canvas post` (HTML, Markdown or text):
- a file you created or changed that the user may want to open
- a screenshot or other image
- numbers that read better as a chart or table
- a structure (call chain, layout, plan, diff) that reads better as a diagram
- code, config or output longer than about 15 lines
- several links or paths worth clicking

If none of those apply, don't post — a one-line status update or anything that would just repeat the chat text stays in chat. But when one does apply, post it; don't decide it's not worth the trouble. A local path becomes a clickable file when written as a link (`[plan](/abs/plan.md)`) and an inline image when written as an image (`![](/abs/shot.png)`); a bare path stays plain text.

For a long task with a status worth watching live — a multi-step plan, a running checklist — post one card and keep it current with `canvas post --update <card_id>` (the id your first post's JSON output reported) instead of posting a new card every time something changes; every open viewer updates that card in place.

Work handed back to you counts as yours. When a subagent, a delegated session or a worker reports back — a gate, a file list, test output, screenshots, findings — decide whether to post it exactly as if you had produced it; it reaches you as plain text, and Canvas is the only place the user sees it rendered. Subagents never receive this guidance, so what they return is yours to post. If you are the one reporting back to another session, post your report before you hand it over: the session you report to may only relay the text. A task's closing report — files changed, testing steps, anything it drove — is the most common post there is, and skipping it is the most common way this gets missed.

In HTML, Canvas picks the light or dark theme, not the post, and it forces the page's background and text color to match. Don't set `html` or `body` background or color. Leave colors you don't need to choose unset so they inherit, and where you do paint a background (a callout, a table header) set a `color` beside it. To adapt to both themes, define a light palette on `:root` and override it under `@media (prefers-color-scheme: dark)`; Canvas makes that query match the active theme. Text that ends up unreadable against its background is recolored automatically.

In HTML, a `<pre>` block renders dark with light text; give any background you paint inside one (diff rows, highlights) a dark `color` of its own.

Size a card on purpose. The viewer sets the width (846px of content at the default window size, and never wider); you set the height. Aim for about one screen, roughly 600px tall. Arrange the post however conveys it best, and go taller or shorter when the content calls for it, as a choice rather than by accident. Clicking any image opens it full size in a lightbox, so screenshots can sit smaller in the card (`<img src="/abs/shot.png" width="360">`, or side by side for a before/after).
