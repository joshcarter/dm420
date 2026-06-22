#!/usr/bin/env python3
"""qso_grammar.py — learn the on-air FT8/FT4 grammar from a dm420 decode archive.

The `archive` crate appends one JSON object per line to `decodes.jsonl`: every
message we *heard* (a `Decode`) and every message we *sent* (a `TxLogEntry`).
This tool reads that archive and does three things, to support tuning the `qso`
auto-sequencer:

  1. LEARN THE GRAMMAR.  Every raw message string is abstracted into a *token
     template* — `K1ABC W9XYZ R-09` becomes `CALL CALL R-RPT`, `CQ POTA W5DOC
     EL09` becomes `CQ MOD CALL GRID`.  Templates are tallied and tagged
     PARSED / UNPARSED according to whether dm420's own parser
     (`core::parse::parse_message`, mirrored in the archived `structured` field)
     produced a real `ParsedMessage` variant or fell back to `Free`/`Raw`.  The
     UNPARSED templates are exactly the grammar the sequencer is blind to.

  2. RECONSTRUCT QSOs.  Directed messages (`TO FROM ...`) are grouped into
     conversations keyed by the unordered callsign pair and segmented by slot
     gaps.  Each QSO's message-type sequence and how it *ended* (RR73 / RRR /
     bare-73 / unterminated) is recorded — the spread of real endings is what a
     robust sequencer has to tolerate.

  3. DETECT STALLS.  Focused on our own station (`--me`, default W4LL): runs of
     an identical non-CQ over re-sent with no progress (the "repeats forever"
     failure), and openers addressed to us that we did not answer (the "doesn't
     reply" failure).

It is pure stdlib and read-only.  Nothing here is on the live path; it is an
offline lens on the archive.

    python3 qso_grammar.py                         # ~/.dm420/decodes.jsonl
    python3 qso_grammar.py /path/to/decodes.jsonl  # explicit file(s)
    python3 qso_grammar.py --me W4LL --top 50      # tuning knobs

Sections can be selected with --grammar / --qsos / --stalls (default: all).
"""
from __future__ import annotations

import argparse
import collections
import glob
import json
import os
import re
import sys

# --------------------------------------------------------------------------- IO

DEFAULT_PATH = os.path.expanduser("~/.dm420/decodes.jsonl")


class Event:
    """One unified message, whether heard or sent."""

    __slots__ = ("t", "slot", "dir", "offset", "dial", "mode", "text",
                 "structured", "snr", "outcome")

    def __init__(self, t, slot, direction, offset, dial, mode, text,
                 structured, snr, outcome):
        self.t = t
        self.slot = slot
        self.dir = direction          # "heard" | "sent"
        self.offset = offset
        self.dial = dial
        self.mode = mode
        self.text = text              # raw on-air text, e.g. "K1ABC W9XYZ -07"
        self.structured = structured  # dm420 ParsedMessage as JSON (or None)
        self.snr = snr
        self.outcome = outcome        # "Sent"/... for TX, else None


def iter_events(paths):
    """Yield Events from one or more decodes.jsonl files, tolerating both the
    `Decode` (heard) and `TxLogEntry` (sent) row shapes."""
    for path in paths:
        with open(path, "r", errors="replace") as fh:
            for line in fh:
                line = line.strip()
                if not line:
                    continue
                try:
                    row = json.loads(line)
                except json.JSONDecodeError:
                    continue
                ev = row.get("event") or {}
                direction = row.get("direction")
                dial = row.get("dial_hz")
                mode = ev.get("mode")

                if "content" in ev:
                    # Decode (heard): event.content.Slotted = {slot, raw, message}
                    slotted = (ev.get("content") or {}).get("Slotted")
                    if not slotted:
                        continue  # streaming row — not slot-sequenced
                    yield Event(
                        t=ev.get("t"), slot=slotted.get("slot"),
                        direction=direction or "heard", offset=ev.get("offset"),
                        dial=dial, mode=mode, text=slotted.get("raw"),
                        structured=slotted.get("message"), snr=ev.get("snr_db"),
                        outcome=None,
                    )
                elif "message" in ev:
                    # TxLogEntry (sent): event.message = {text, structured}
                    msg = ev.get("message") or {}
                    yield Event(
                        t=ev.get("t"), slot=ev.get("slot"),
                        direction=direction or "sent", offset=ev.get("offset"),
                        dial=dial, mode=mode, text=msg.get("text"),
                        structured=msg.get("structured"), snr=None,
                        outcome=ev.get("outcome"),
                    )


# ------------------------------------------------------------------ structured

def variant_key(structured):
    """A compact label for a dm420 ParsedMessage, e.g. 'Exchange/RogerReport'."""
    if not isinstance(structured, dict) or not structured:
        return "None"
    k = next(iter(structured))
    if k == "Exchange":
        pl = structured["Exchange"].get("payload")
        if isinstance(pl, dict) and pl:
            return "Exchange/" + next(iter(pl))
        return "Exchange/" + str(pl)
    if k == "Signoff":
        return "Signoff/" + str(structured["Signoff"].get("kind"))
    return k  # Cq | Free | Raw


def parsed_ok(structured):
    """True when dm420 produced a *structured* variant (not Free/Raw/None)."""
    if not isinstance(structured, dict) or not structured:
        return False
    return next(iter(structured)) not in ("Free", "Raw")


def addressed(structured):
    """`(to, from)` for a directed message, else None."""
    if not isinstance(structured, dict):
        return None
    k = next(iter(structured))
    if k in ("Exchange", "Signoff"):
        d = structured[k]
        return d.get("to"), d.get("from")
    return None


# --------------------------------------------------------------- token grammar

# Directed-CQ / activity modifiers seen on the bands (not exhaustive; anything
# unrecognized that is short + alpha falls through to WORD, which is fine — the
# template still shows the *shape*).
CONTEST_MODS = {
    "DX", "TEST", "FD", "RU", "NA", "SA", "EU", "AS", "AF", "OC", "WW", "POTA",
    "SOTA", "PARK", "QRP", "WCUP", "WC", "GL", "YLP", "ASIA", "WAS", "DIG",
}

_RPT = re.compile(r"[+-]\d{1,2}")
_RRPT = re.compile(r"R[+-]\d{1,2}")
_GRID = re.compile(r"[A-R]{2}\d{2}")
_GRID6 = re.compile(r"[A-R]{2}\d{2}[A-X]{2}")
_CLASS = re.compile(r"\d{1,2}[A-F]")
_CALL = re.compile(r"[A-Z0-9/]+")


def classify(tok: str) -> str:
    """Map one on-air token to its grammar class."""
    if tok == "CQ":
        return "CQ"
    if tok in ("DE", "QRZ"):
        return tok
    if tok.startswith("<"):           # <CALL> or <...> — a hashed callsign
        return "<HASH>"
    if tok in ("RR73", "RRR", "73"):
        return tok
    if tok == "R":
        return "R"
    if _RRPT.fullmatch(tok):
        return "R-RPT"
    if _RPT.fullmatch(tok):
        return "RPT"
    if _GRID6.fullmatch(tok):
        return "GRID6"
    if _GRID.fullmatch(tok):
        return "GRID"
    if _CLASS.fullmatch(tok):
        return "CLASS"
    if tok in CONTEST_MODS:
        return "MOD"
    if re.search(r"\d", tok) and re.search(r"[A-Z]", tok) and _CALL.fullmatch(tok):
        return "CALL"                 # standard or compound callsign
    if re.fullmatch(r"[A-Z]{1,4}", tok):
        return "WORD"                 # section, unknown modifier, free word
    return "?"


def template(text: str) -> str:
    if not text or not text.strip():
        return "<empty>"
    return " ".join(classify(t) for t in text.split())


# ----------------------------------------------------------------- QSO rebuild

SLOT_GAP = 6   # > this many slots of silence between a pair starts a new QSO

ENDERS = {"Signoff/Rr73": "RR73", "Signoff/Rrr": "RRR", "Signoff/Seven3": "73"}


class Qso:
    def __init__(self, a, b):
        self.calls = (a, b)
        self.msgs = []        # list of (event, vkey)

    def end_kind(self):
        for ev, vk in reversed(self.msgs):
            if vk in ENDERS:
                return ENDERS[vk]
        return "—"            # never reached a sign-off

    def involves(self, call):
        return call in self.calls


def reconstruct(events):
    """Group directed messages into per-pair, slot-adjacent QSOs."""
    by_pair = collections.defaultdict(list)
    for ev in events:
        ad = addressed(ev.structured)
        if not ad:
            continue
        to, frm = ad
        if not to or not frm:
            continue
        key = frozenset((to, frm))
        by_pair[key].append(ev)

    qsos = []
    for key, evs in by_pair.items():
        evs.sort(key=lambda e: (e.slot if e.slot is not None else 0, e.t or 0))
        calls = tuple(key)
        a = calls[0]
        b = calls[1] if len(calls) == 2 else calls[0]
        cur = None
        last_slot = None
        for ev in evs:
            s = ev.slot if ev.slot is not None else 0
            if cur is None or (last_slot is not None and s - last_slot > SLOT_GAP):
                cur = Qso(a, b)
                qsos.append(cur)
            cur.msgs.append((ev, variant_key(ev.structured)))
            last_slot = s
    return qsos


# --------------------------------------------------------------------- reports

def hr(title):
    print("\n" + "=" * 78)
    print(title)
    print("=" * 78)


def report_grammar(events, top):
    hr("1. LEARNED GRAMMAR")

    variants = collections.Counter()
    templates = collections.Counter()
    tmpl_parsed = {}                 # template -> bool (dm420 parsed it)
    tmpl_example = {}                # template -> a raw example
    tmpl_variant = collections.defaultdict(collections.Counter)
    n_heard = n_sent = 0

    for ev in events:
        if ev.dir == "sent":
            n_sent += 1
        else:
            n_heard += 1
        vk = variant_key(ev.structured)
        variants[vk] += 1
        tm = template(ev.text)
        templates[tm] += 1
        ok = parsed_ok(ev.structured)
        # A template is "UNPARSED" if *any* instance fell back to Free/Raw.
        tmpl_parsed[tm] = tmpl_parsed.get(tm, True) and ok
        tmpl_variant[tm][vk] += 1
        if tm not in tmpl_example and ev.text and ev.text.strip():
            tmpl_example[tm] = ev.text

    total = n_heard + n_sent
    print(f"\n{total} messages   ({n_heard} heard, {n_sent} sent)\n")

    print("dm420 ParsedMessage variants (what the sequencer actually sees):")
    for vk, c in variants.most_common():
        print(f"  {c:7d}  {c/total*100:5.1f}%  {vk}")

    unparsed = sum(c for vk, c in variants.items() if vk in ("Free", "Raw"))
    print(f"\n  -> {unparsed} ({unparsed/total*100:.2f}%) fell through to "
          f"Free/Raw — invisible to the auto-sequencer.")

    print(f"\nToken templates (top {top} of {len(templates)}):")
    print(f"  {'count':>7}  {'parse':<8} template")
    for tm, c in templates.most_common(top):
        tag = "PARSED" if tmpl_parsed.get(tm) else "UNPARSED"
        ex = tmpl_example.get(tm, "")
        print(f"  {c:7d}  {tag:<8} {tm:<28} e.g. {ex!r}")

    print("\nUNPARSED templates only (the grammar gaps), by frequency:")
    gaps = [(tm, c) for tm, c in templates.most_common() if not tmpl_parsed.get(tm)]
    if not gaps:
        print("  (none)")
    for tm, c in gaps[:top]:
        ex = tmpl_example.get(tm, "")
        vs = ", ".join(f"{v}x{n}" for v, n in tmpl_variant[tm].most_common())
        print(f"  {c:7d}  {tm:<26} e.g. {ex!r:32}  [{vs}]")
    if len(gaps) > top:
        print(f"  ... and {len(gaps) - top} rarer gap templates")


def report_qsos(qsos, me):
    hr("2. RECONSTRUCTED QSOs")
    complete = [q for q in qsos if len(q.msgs) >= 2]
    print(f"\n{len(qsos)} candidate conversations, "
          f"{len(complete)} with >=2 messages.\n")

    endings = collections.Counter(q.end_kind() for q in complete)
    print("How real QSOs ended (a robust sequencer must accept every one):")
    for end, c in endings.most_common():
        label = {"RR73": "RR73", "RRR": "RRR", "73": "bare 73",
                 "—": "no sign-off seen"}.get(end, end)
        print(f"  {c:6d}  {c/len(complete)*100:5.1f}%  {label}")

    mine = [q for q in complete if q.involves(me)]
    print(f"\nQSOs involving {me}: {len(mine)}")
    for q in mine[:40]:
        other = [c for c in q.calls if c != me]
        other = other[0] if other else "?"
        seq = " -> ".join(vk.replace("Exchange/", "").replace("Signoff/", "")
                          for _, vk in q.msgs)
        slots = [e.slot for e, _ in q.msgs if e.slot is not None]
        span = (max(slots) - min(slots)) if slots else 0
        print(f"  {me}<->{other:<9} end={q.end_kind():<4} slots={span:<3} {seq}")


def report_stalls(events, me, run_min):
    hr("3. STALL DETECTION  (our station = %s)" % me)

    ev_sorted = sorted(events, key=lambda e: (e.t or 0))
    sent = [e for e in ev_sorted if e.dir == "sent"]

    def is_cq(e):
        return isinstance(e.structured, dict) and "Cq" in e.structured

    # --- A. repeated identical non-CQ over (the "repeats forever" failure) ----
    print("\nA. Repeated overs (same non-CQ message re-sent with no progress):")
    runs = []
    i = 0
    while i < len(sent):
        j = i
        while (j + 1 < len(sent) and sent[j + 1].text == sent[i].text):
            j += 1
        run = sent[i:j + 1]
        if len(run) >= run_min and not is_cq(run[0]):
            runs.append(run)
        i = j + 1
    if not runs:
        print("  (no non-CQ over repeated >=%d times)" % run_min)
    for run in runs:
        slots = [e.slot for e in run if e.slot is not None]
        span = (max(slots) - min(slots)) if slots else 0
        kind = variant_key(run[0].structured)
        flag = ""
        if run[0].text and run[0].text.rstrip().endswith("RR73"):
            flag = "  <-- terminal RR73 never released (CQ-side never finishes)"
        print(f"  {len(run):3d}x  '{run[0].text}'  [{kind}] over ~{span} slots{flag}")

    # --- A'. longest CQ run (informational: CQ repetition is expected) --------
    cq_runs = []
    i = 0
    while i < len(sent):
        j = i
        while j + 1 < len(sent) and is_cq(sent[j + 1]) and is_cq(sent[i]):
            j += 1
        if is_cq(sent[i]) and j > i:
            cq_runs.append(sent[i:j + 1])
        i = j + 1
    if cq_runs:
        longest = max(cq_runs, key=len)
        print(f"\n  (info) longest unanswered CQ run: {len(longest)}x "
              f"'{longest[0].text}'  — CQ repetition is expected, not a stall")

    # --- B. openers addressed to us that we did not answer --------------------
    print("\nB. Openers addressed to %s that we did not answer:" % me)
    OPENERS = {"Exchange/Grid", "Exchange/Report", "Exchange/RogerReport",
               "Exchange/FieldDay"}
    sent_by_slot = collections.defaultdict(list)
    for e in sent:
        if e.slot is not None:
            sent_by_slot[e.slot].append(e)
    hits = 0
    for e in ev_sorted:
        if e.dir != "heard":
            continue
        ad = addressed(e.structured)
        if not ad or ad[0] != me:
            continue
        vk = variant_key(e.structured)
        is_open = vk in OPENERS
        is_unparsed_to_us = vk in ("Free", "Raw") and e.text and me in e.text
        if not (is_open or is_unparsed_to_us):
            continue
        frm = ad[1]
        # did we, within the next few slots, send a directed reply to `frm`?
        answered = False
        for ds in range(1, 5):
            for s in sent_by_slot.get((e.slot or 0) + ds, []):
                a2 = addressed(s.structured)
                if a2 and a2[1] == me and a2[0] == frm:
                    answered = True
        if not answered:
            hits += 1
            if hits <= 25:
                tail = "report-opener (Now-#11 gap)" if vk == "Exchange/Report" \
                    else ("unparsed grammar" if vk in ("Free", "Raw") else vk)
                print(f"  slot {e.slot}  heard '{e.text}'  [{tail}] — no reply sent")
    if hits == 0:
        print("  (none detected)")
    else:
        print(f"  -> {hits} opener(s) to {me} went unanswered.")


def main(argv=None):
    ap = argparse.ArgumentParser(
        description=__doc__,
        formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("paths", nargs="*", default=[DEFAULT_PATH],
                    help="decodes.jsonl file(s) (default: ~/.dm420/decodes.jsonl)")
    ap.add_argument("--me", default="W4LL", help="our station callsign")
    ap.add_argument("--top", type=int, default=40, help="rows per histogram")
    ap.add_argument("--run-min", type=int, default=3,
                    help="repeated-over run length that counts as a stall")
    ap.add_argument("--grammar", action="store_true")
    ap.add_argument("--qsos", action="store_true")
    ap.add_argument("--stalls", action="store_true")
    args = ap.parse_args(argv)

    # expand globs / fall back to default
    paths = []
    for p in (args.paths or [DEFAULT_PATH]):
        paths.extend(glob.glob(p) or [p])
    paths = [p for p in paths if os.path.exists(p)]
    if not paths:
        sys.exit(f"no archive found (looked for {args.paths or [DEFAULT_PATH]})")

    events = list(iter_events(paths))
    if not events:
        sys.exit("no events parsed from archive")

    all_sections = not (args.grammar or args.qsos or args.stalls)
    print(f"dm420 decode-archive grammar analysis — {len(events)} events "
          f"from {', '.join(paths)}")

    if all_sections or args.grammar:
        report_grammar(events, args.top)
    if all_sections or args.qsos:
        report_qsos(reconstruct(events), args.me)
    if all_sections or args.stalls:
        report_stalls(events, args.me, args.run_min)


if __name__ == "__main__":
    main()
