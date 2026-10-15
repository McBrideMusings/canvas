# ADR-0006: Every machine runs canvasd and shares chosen items upstream

Every machine runs the same `canvas daemon`, and by default it is the primary: it holds
every post and artifact made there and is reached only through its 0600 socket. A
`sharing.toml` in `CANVAS_DATA_DIR` may name one upstream canvasd and rules: glob `include`
and `exclude` lists per kind (`timeline`, `artifacts`), matched against the git root of the
session's working directory, else the working directory, with exclude winning. With no
rule, nothing is shared. Each item lives in exactly one canvasd. A shared item travels
upstream as bytes, never as a path, and the daemon it came from keeps the work that needs
its machine: pid checks, refresh commands, watching a linked folder, opening local paths,
and writing a linked artifact's `canvas-data/`. Other machines and browsers reach canvasd
only over HTTP on the address and port `sharing.toml` names, maybe behind a TLS proxy; it
admits a caller only when its address, or the forwarded client's when the peer is the
`sharing.toml` proxy, is in the `sharing.toml` allow list, and allows only its own and
listed Host and Origin. A daemon with an upstream merges its stream in, showing each item's machine.
