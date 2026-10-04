# ADR-0002: Interactive pages are artifacts Canvas owns

Canvas holds two kinds of thing. A post is a throwaway document on the Timeline, themed and
sized to its content and kept for 24 hours. An artifact is a folder of files under
`artifacts/<id>/` in `CANVAS_DATA_DIR`, shown on the Artifacts page as a web page in a pane
with its own viewport, never themed by Canvas, and kept until someone deletes it. Every
interactive app is an artifact; a post has no app mode. An artifact can instead link to a
folder the person owns, such as one in a repo, and a linked folder that disappears puts the
artifact in a source-missing state rather than removing it. Artifacts replace pins: an
artifact carries the widget, refresh command and data push, and its widget shows only in its
row on the Artifacts page.
