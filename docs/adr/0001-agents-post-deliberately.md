# ADR-0001: Canvas shows only what agents post

A card exists only because an agent ran `canvas post`; nothing scrapes links, paths or
images out of a finished turn, and there is no Stop hook. The SessionStart hook injects
guidance on when to post. Local paths in a post resolve through a list `canvas post`
records at post time, so a script inside a post can open only the files that post shows.
