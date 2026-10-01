#!/usr/bin/env python3
"""Extract candidate failures of Canvas's automatic posting from Claude Code transcripts.

A turn runs from one human prompt to the next. For each turn this records what the
posting hook could have seen (files changed, images read, the final reply's length and
links) and what happened (a `canvas post` ran, a reminder arrived, the user asked for a
post or objected to one). It prints candidates, not verdicts: the skill's lens judges each.

    transcript_signals.py [--since YYYY-MM-DD[THH:MM]] [--path DIR_OR_JSONL ...] [--json] [--limit N]
"""
import argparse
import glob
import json
import os
import re
import sys
from collections import Counter, defaultdict

PROJECTS = os.path.expanduser("~/.claude/projects")
TRIGGERS_FILE = os.path.join(os.path.dirname(os.path.abspath(__file__)), "../../../plugin/stop-triggers.txt")

# A request to post is a short chat line; a long prompt is a dispatch brief that merely names Canvas.
ASK_MAX_CHARS = 500
IMAGE_EXT = (".png", ".jpg", ".jpeg", ".gif", ".webp")
REMINDER_PREFIX = "Last turn "


ASK_RE = re.compile(
    r"\b(post|put|throw|send|drop|show|render|publish)\b[^.\n]{0,40}\b(on|to|in|onto|into)\s+canvas\b"
    r"|\bcanvas (it|that|this|them)\b|\bcanvas post (it|that|this)\b|\bdid you (post|canvas)\b"
    r"|\bwhy (didn'?t|wasn'?t)[^.\n]{0,30}canvas\b|^\s*/canvas\b",
    re.I,
)
OBJECT_RE = re.compile(
    r"didn'?t need (to be )?(on |a )?(canvas|post|card)|no need (for|to)[^.\n]{0,20}(canvas|post|card)"
    r"|(don'?t|do not|stop|quit) (need to )?(post|posting)|too many (posts|cards)"
    r"|shouldn'?t (have )?(posted|been (posted )?(on|to) canvas)|not everything (needs|belongs)"
    r"|(didn'?t|did not) (need|have) to (post|be posted)|didn'?t warrant (a )?(canvas|card|post)",
    re.I,
)
WORD_RE = re.compile(r"[a-z0-9]{3,}")


def load_config():
    cfg = {"phrases": [], "links": None, "long_block": None, "scratch": [], "image": False, "file": False, "verify": False}
    try:
        for line in open(TRIGGERS_FILE):
            line = line.strip()
            if not line or line.startswith("#"):
                continue
            key, _, rest = line.partition(" ")
            if key in ("image", "file", "verify"):
                cfg[key] = True
            elif key == "off":
                cfg.update(image=False, file=False, verify=False, links=None, long_block=None)
            elif key == "phrase":
                cfg["phrases"].append(rest.lower())
            elif key in ("links", "long-block"):
                cfg["links" if key == "links" else "long_block"] = int(rest)
            elif key == "scratch":
                cfg["scratch"].append(rest)
    except OSError:
        print(f"warning: cannot read {TRIGGERS_FILE}; trigger signals use defaults", file=sys.stderr)
        cfg.update(image=True, file=True, verify=True, links=3, long_block=15, scratch=["/private/tmp/", "/tmp/", "/var/folders/"])
    return cfg


def corpus_files(paths):
    if paths:
        out = []
        for p in paths:
            out += [p] if p.endswith(".jsonl") else sorted(glob.glob(os.path.join(p, "*.jsonl")))
        return out
    out = []
    for d in sorted(os.listdir(PROJECTS) if os.path.isdir(PROJECTS) else []):
        if d == "-Users-pierce-Projects-canvas" or d.startswith("-Users-pierce--worktrees-canvas-"):
            out += sorted(glob.glob(os.path.join(PROJECTS, d, "*.jsonl")))
    return out


def user_text(rec):
    """The human-typed text of a user record, or None for tool results, meta and slash-command noise."""
    if rec.get("isMeta") or (rec.get("origin") or {}).get("kind", "human") != "human":
        return None
    c = rec["message"]["content"]
    if isinstance(c, list):
        has_image = any(b.get("type") == "image" for b in c)
        c = "\n".join(b.get("text", "") for b in c if b.get("type") == "text")
        if has_image and not c.strip():
            c = "[image]"
    c = c.strip()
    if not c or c.startswith(("<local-command", "<command-", "<system-reminder", "[Request interrupted")):
        return None
    return c


PREFIX_WORDS = {"then", "do", "else", "elif", "if", "while", "until", "{", "!", "time", "env", "exec"}
HEREDOC_RE = re.compile(r"<<-?\s*(['\"]?)(\w+)\1")


def segment_is_post(seg):
    words = seg.split()
    while words and (words[0] in PREFIX_WORDS or re.match(r"^\w+=", words[0])):
        words.pop(0)
    return len(words) >= 2 and (words[0] == "canvas" or words[0].endswith("/canvas")) and words[1] == "post"


def is_post_call(cmd):
    """Mirrors `runs_canvas_post` in cli/src/stop.rs: a segment that runs `canvas post`,
    outside quotes and heredoc bodies."""
    stack, start, i, n = [], 0, 0, len(cmd)
    while i < n:
        c = cmd[i]
        top = stack[-1] if stack else None
        if top == "'":
            if c == "'":
                stack.pop()
        elif top == '"':
            if c == "\\":
                i += 1
            elif c == '"':
                stack.pop()
                start = i + 1
            elif c == "$" and cmd[i + 1:i + 2] == "(":
                stack.append("(")
                i += 1
                start = i + 1
        elif c == "\\":
            i += 1
        elif c in "'\"":
            stack.append(c)
        elif c == "$" and cmd[i + 1:i + 2] == "(":
            if segment_is_post(cmd[start:i]):
                return True
            stack.append("(")
            i += 1
            start = i + 1
        elif c in ";&|\n`()":
            if segment_is_post(cmd[start:i]):
                return True
            if c == "(":
                stack.append("(")
            elif c == ")" and stack and stack[-1] == "(":
                stack.pop()
            start = i + 1
            if c == "\n":
                m = HEREDOC_RE.search(cmd, max(0, cmd.rfind("\n", 0, i)), i)
                if m:
                    end = re.compile(r"^\s*" + re.escape(m.group(2)) + r"\s*$", re.M).search(cmd, i + 1)
                    if not end:
                        return False
                    i, start = end.end(), end.end()
        i += 1
    return segment_is_post(cmd[start:])


def post_body(cmd):
    m = re.search(r"<<-?\s*(['\"]?)(\w+)\1[^\n]*\n(.*?)\n\s*\2\s*$", cmd, re.S | re.M)
    return m.group(3) if m else None


def visible_text(body):
    body = re.sub(r"<(script|style)\b.*?</\1>", " ", body, flags=re.S | re.I)
    return re.sub(r"<[^>]+>", "\n", body)


def words(text):
    return set(WORD_RE.findall(text.lower()))


def long_block(text, limit):
    """Mirrors `has_long_block` in cli/src/stop.rs: a fence or table run longer than `limit` lines."""
    inside, fence, table = False, 0, 0
    for ln in text.split("\n"):
        if ln.lstrip().startswith("```"):
            if inside and fence > limit:
                return True
            inside, fence, table = not inside, 0, 0
        elif inside:
            fence += 1
        elif ln.lstrip().startswith("|"):
            table += 1
            if table > limit:
                return True
        else:
            table = 0
    return False


def turn_triggers(turn, cfg):
    hits = []
    if cfg["file"] and any(not p.startswith(tuple(cfg["scratch"])) for p in turn["writes"]):
        hits.append("file")
    if cfg["image"] and turn["images"]:
        hits.append("image")
    text = turn["final"]
    low = text.lower()
    if cfg["verify"] and any(p in low for p in cfg["phrases"]):
        hits.append("phrase")
    if cfg["links"] and text.count("http://") + text.count("https://") + text.count("](/") >= cfg["links"]:
        hits.append("links")
    if cfg["long_block"] and long_block(text, cfg["long_block"]):
        hits.append("long-block")
    return hits


def read_turns(path, since, seen):
    turns, cur, sid = [], None, os.path.basename(path)[:-6]
    for line in open(path):
        try:
            r = json.loads(line)
        except ValueError:
            continue
        ts, uid = r.get("timestamp", ""), r.get("uuid")
        t = r.get("type")
        if uid and t in ("user", "assistant", "attachment"):
            if uid in seen:
                continue
            seen.add(uid)
        if t == "user":
            txt = user_text(r)
            if txt is None:
                continue
            cur = {"session": sid, "uuid": uid, "ts": ts, "user": txt, "final": "", "writes": [],
                   "images": [], "posts": [], "reminder": None, "path": path}
            turns.append(cur)
        elif cur is None:
            continue
        elif t == "attachment":
            a = r.get("attachment", {})
            c = a.get("content") or ""
            if a.get("type") == "hook_success" and a.get("hookEvent") == "UserPromptSubmit" and str(c).startswith(REMINDER_PREFIX) \
                    and not cur["final"] and not cur["posts"]:
                cur["reminder"] = c
        elif t == "assistant":
            content = r["message"]["content"]
            for b in content if isinstance(content, list) else []:
                if b.get("type") == "text" and b["text"].strip():
                    cur["final"] = b["text"]
                elif b.get("type") == "tool_use":
                    inp, name = b.get("input", {}), b["name"]
                    if name in ("Write", "Edit", "NotebookEdit") and inp.get("file_path"):
                        cur["writes"].append(inp["file_path"])
                    elif name == "Read" and str(inp.get("file_path", "")).lower().endswith(IMAGE_EXT):
                        cur["images"].append(inp["file_path"])
                    elif name == "Bash" and is_post_call(inp.get("command", "")):
                        cmd = inp["command"]
                        cur["posts"].append({"update": "--update" in cmd, "body": post_body(cmd)})
    return [t for t in turns if t["ts"] >= since] if since else turns


def clip(s, n):
    s = " ".join(s.split())
    return s if len(s) <= n else s[: n - 1] + "…"


def analyze(turns, cfg):
    by_session = defaultdict(list)
    for t in turns:
        by_session[t["session"]].append(t)
    out = []

    def add(signal, t, **extra):
        out.append({"signal": signal, "session": t["session"], "ts": t["ts"], "uuid": t["uuid"],
                    "user": clip(t["user"], 300), "reply_tail": clip(t["final"][-400:], 400),
                    "posts": len(t["posts"]), "path": t["path"], **extra})

    for ts in by_session.values():
        for i, t in enumerate(ts):
            prev = ts[i - 1] if i else None
            nxt = ts[i + 1] if i + 1 < len(ts) else None
            trig = turn_triggers(t, cfg)
            if len(t["user"]) < ASK_MAX_CHARS and ASK_RE.search(t["user"]):
                add("FN asked-for-post", t, prev_posted=bool(prev and prev["posts"]), triggers=trig)
            if OBJECT_RE.search(t["user"]) and prev and prev["posts"]:
                add("FP user-objected", prev, objection=clip(t["user"], 200))
            if t["reminder"] and prev:
                if t["posts"]:
                    add("FN late-post", prev, reminder=t["reminder"], triggers=turn_triggers(prev, cfg))
                else:
                    add("FN reminder-ignored", prev, reminder=t["reminder"], triggers=turn_triggers(prev, cfg))
            if trig and not t["posts"] and nxt and not nxt["reminder"]:
                add("FN trigger-missed", t, triggers=trig)
            for p in t["posts"]:
                if p["update"] or p["body"] is None:
                    continue
                vis = visible_text(p["body"])
                lines = [ln for ln in vis.split("\n") if ln.strip()]
                union = words(vis) | words(t["final"])
                overlap = len(words(vis) & words(t["final"])) / len(union) if union else 0
                if len(lines) < 8:
                    add("FP tiny-post", t, card_lines=len(lines), after_reminder=bool(t["reminder"]))
                elif overlap > 0.6:
                    add("FP repeats-chat", t, overlap=round(overlap, 2))
    return out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--since")
    ap.add_argument("--path", nargs="*")
    ap.add_argument("--json", action="store_true")
    ap.add_argument("--limit", type=int, default=8, help="examples per signal in text output")
    args = ap.parse_args()
    if args.since and not re.fullmatch(r"\d{4}-\d{2}-\d{2}(T\d{2}:\d{2})?", args.since):
        ap.error("--since must be YYYY-MM-DD or YYYY-MM-DDTHH:MM, in UTC")
    cfg, seen, turns, files = load_config(), set(), [], corpus_files(args.path)
    for f in files:
        turns += read_turns(f, args.since, seen)
    found = analyze(turns, cfg)
    posted = sum(1 for t in turns if t["posts"])
    if args.json:
        json.dump({"sessions": len({t["session"] for t in turns}), "turns": len(turns), "turns_with_post": posted,
                   "candidates": found}, sys.stdout, indent=1)
        return
    print(f"{len(files)} transcripts, {len({t['session'] for t in turns})} sessions, {len(turns)} turns, "
          f"{posted} turns posted, since {args.since or 'the start'}")
    counts = Counter(c["signal"] for c in found)
    for sig, n in sorted(counts.items()):
        print(f"\n## {sig}: {n}")
        if sig in ("FN trigger-missed", "FN reminder-ignored", "FN late-post", "FN asked-for-post"):
            tc = Counter(x for c in found if c["signal"] == sig for x in c.get("triggers", []) or ["(none)"])
            print("   by trigger: " + ", ".join(f"{k} {v}" for k, v in tc.most_common()))
        for c in [c for c in found if c["signal"] == sig][: args.limit]:
            print(f"- {c['ts'][:16]} {c['session'][:8]} {(c['uuid'] or '')[:8]}  user: {c['user'][:140]}")


if __name__ == "__main__":
    main()
