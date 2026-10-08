# ADR-0003: The plugin names only what the repo ships

Canvas is installed by people whose machines hold skills, tools and commands this repo knows
nothing about. So nothing under `plugin/` (the built-in instructions, the canvas skill, the
built-in post reminders) names a skill, tool or command the repo does not ship. When a post
would be better made with a local tool, the person says so in their own instructions, which
Canvas reads after the built-in text. The built-in text may describe a kind of tool ("a
document another skill builds"). It never names one.
