# ADR-0004: Settings has one level of tabs

Each thing a person configures in Canvas's Settings window is a top-level tab: General,
Appearance, Instructions, Post reminders, Daemon, Integrations. A tab never holds another set
of tabs, or a switch that swaps the tab's content for a different setting, because a setting
found only by first picking the right inner switch is a setting people miss. Two controls are
not tabs and stay allowed: choosing an item from a list to edit beside it (a layer, a project),
and switching how the same content is shown (Form or Text for the same directives). A new
setting that doesn't fit an existing tab gets its own tab.
