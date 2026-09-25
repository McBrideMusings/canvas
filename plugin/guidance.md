Canvas (http://127.0.0.1:8229) is the rich-media side of this chat. It isn't a replacement: chat keeps the answer, and Canvas shows what plain Markdown can't render well. Post freely with `canvas post` (HTML, Markdown or text). A local path becomes a clickable file when written as a link (`[plan](/abs/plan.md)`) and an inline image when written as an image (`![](/abs/shot.png)`); a bare path stays plain text. Post when your turn has any of these:
- a file you created or changed that the user may want to open
- a screenshot or other image
- numbers that read better as a chart or table
- a structure (call chain, layout, plan, diff) that reads better as a diagram
- code, config or output longer than about 15 lines
- several links or paths worth clicking

Work handed back to you counts as yours. When a subagent, a delegated session or a worker reports back — a gate, a file list, test output, screenshots, findings — decide whether to post it exactly as if you had produced it; it reaches you as plain text, and Canvas is the only place the user sees it rendered. Subagents never receive this guidance, so what they return is yours to post. If you are the one reporting back to another session, post your report before you hand it over: the session you report to may only relay the text. A task's closing report — files changed, testing steps, anything it drove — is the most common post there is.

Skip it for one-line status updates, and for anything that would just repeat the chat text with no richer form.

Size a card on purpose. The viewer sets the width (846px of content at the default window size, and never wider); you set the height. Aim for about one screen, roughly 600px tall. Arrange the post however conveys it best, and go taller or shorter when the content calls for it, as a choice rather than by accident. Clicking any image opens it full size in a lightbox, so screenshots can sit smaller in the card (`<img src="/abs/shot.png" width="360">`, or side by side for a before/after).
