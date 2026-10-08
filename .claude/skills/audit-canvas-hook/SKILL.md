---
name: audit-canvas-hook
description: Audit real Claude Code transcripts for failures of Canvas's automatic posting — false negatives (a post was needed and didn't happen, or the user had to ask) and false positives (a card nobody needed) — and turn them into edits to the posting instructions and post reminders. Use when tuning `plugin/instructions.md`, `plugin/reminders.txt` or `cli/src/stop.rs`, or to check how well the hook is doing.
---

# Audit the Canvas hook

Canvas posts in two ways that this skill judges. The SessionStart instructions
(`plugin/instructions.md`) tells the agent when to post. The UserPromptSubmit hook
(`cli/src/stop.rs`, configured by `plugin/reminders.txt`) adds a reminder to the next
prompt when a finished turn hit a trigger and no `canvas post` ran. A failure of either
shows up in the transcripts and nowhere else: the daemon keeps no record of views or
dismissals, and a deleted card is gone from `stream.jsonl`.

## Run the extractor first

```bash
python3 /Users/pierce/Projects/canvas/.claude/skills/audit-canvas-hook/transcript_signals.py --since YYYY-MM-DD[THH:MM]
```

Pass `--since` as the day the instructions, reminders or hook last changed (a UTC date, or a
date and time like `2026-09-30T19:45`). Older turns ran under different rules and read as
noise.

Never pass a `--since` earlier than `2026-09-30T19:45` UTC (3:45pm EDT). Before then the
UserPromptSubmit hook had not run in any session, so every `FN trigger-missed` turn from
that time is a turn with no hook to fire.

The extractor reads this repo's transcripts and its
`~/.worktrees` ones by default; `--path <dir|file.jsonl>` overrides. `--json` gives every
candidate with its user text, reply tail, transcript path and turn uuid.

Its triggers come from `plugin/reminders.txt` at run time, so a trigger edit changes
the next run. It does not model the `report` trigger (a closing report, off in the default) or the hook's
exact parser, so a hook that stayed silent is a candidate, not proof of a hook bug.

## Signals

**False negatives** (should have posted):

| Signal | Meaning |
|---|---|
| `FN asked-for-post` | The user asked for a post, or asked why there wasn't one. The strongest signal: they should never have to ask. |
| `FN reminder-ignored` | The hook fired, the next turn did not post. Either the agent judged it didn't need one (then the trigger is too loose) or it missed the reminder (then the wording is too weak). |
| `FN late-post` | The post came a turn late, because of the reminder. The hook worked; the instructions did not. |
| `FN trigger-missed` | The turn matched a trigger, no post ran, and no reminder arrived. Either the hook was silent (check whether the hook and plugin were installed on that date) or the trigger model here differs from the hook's. |

**False positives** (should not have posted):

| Signal | Meaning |
|---|---|
| `FP user-objected` | The user said a post wasn't needed. Reported on the turn that posted. |
| `FP tiny-post` | A card with under 8 visible lines: chat would have done. `after_reminder` says whether the hook prompted it. |
| `FP repeats-chat` | The card's words overlap the chat reply by over 60%. |

Not measured: whether a card was ever opened, and whether the user's next message used it.

## Judge each candidate, then rank

For each candidate read `user` and `reply_tail`, and for a doubtful one open the turn in the
transcript by uuid (`grep -n <uuid> <path>`). Sort it into one of:

1. **Real failure** — say which trigger or instructions line should have caught it, or which one
   wrongly did.
2. **Not a failure** — a dispatch brief, a one-line answer, a card that earned its place.
   Name why, because the reason becomes a rule the extractor can absorb.
3. **Extractor error** — fix `transcript_signals.py` in the same change.

A Canvas session talks about Canvas all day, so a user message mentioning it is not a request
to post. Only count an ask aimed at the last reply.

## Report

Lead with counts: real failures per signal, out of turns audited and turns that posted.
Then a table by trigger (`file`, `image`, `links`, `long-block`, `phrase`, instructions line)
giving real failures and false alarms for each. For the worst three triggers, propose one
edit to the owning file (`plugin/instructions.md`, `plugin/reminders.txt`, `cli/src/stop.rs`),
with the quoted turns that justify it. File each edit as a ticket through `backlog spec`.
This skill never edits the instructions or reminders itself, and bumps no plugin version.
