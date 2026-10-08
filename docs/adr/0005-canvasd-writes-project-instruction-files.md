# ADR-0005: canvasd writes a project's instruction files

Project instructions live in the repo, at `.canvas/instructions.md` and `.canvas/reminders.txt`
under its git root, so they are versioned and shared with every clone. Settings edits them
through canvasd, which makes canvasd the one Canvas process that writes into a folder the person
owns. It writes only those two names, only under a git root it recorded from a session that
posted, only on a Settings request, through a temp file and a rename, and never through a
symlink. Every other folder the person owns, linked artifacts included (ADR-0002), stays
read-only to canvasd.
