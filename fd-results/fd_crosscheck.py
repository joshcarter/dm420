#!/usr/bin/env python3
"""
fd_crosscheck.py  --  Independent accuracy check for fd_report.py.

fd_report.py builds the QSO set from DM420's *structured* decode fields
(event...message.{Exchange,Signoff}.payload.FieldDay).  This script ignores
those fields entirely and re-derives every QSO by regex-parsing the raw on-air
TEXT of each transmission ("raw" for heard, "text" for sent) -- a fully separate
parser fed by a different input.  It then compares its creditable (call, band)
set against fd_report.py's fd_worked_stations.csv.

If DM420's structured decoder and the raw text agree, the two independently
produced QSO sets should match.  Any disagreement is printed for inspection.
"""

import json, csv, re, os, collections
from datetime import datetime, timezone

HERE = os.path.dirname(os.path.abspath(__file__))
FILES = [os.path.join(HERE, "decodes-joel.json"),
         os.path.join(HERE, "decodes-josh.jsonl")]
CSV_OUT = os.path.join(HERE, "fd_worked_stations.csv")

OUR_CALLS = {"N0JDC", "W4LL"}
SUB_CALL = "N0JDC"
WIN_START = datetime(2026, 6, 27, 18, 0, 0, tzinfo=timezone.utc).timestamp() * 1000
WIN_END   = datetime(2026, 6, 28, 21, 0, 0, tzinfo=timezone.utc).timestamp() * 1000

BAND_RANGES = [(1.8, 2.0, "160m"), (3.5, 4.0, "80m"), (7.0, 7.3, "40m"),
               (14.0, 14.35, "20m"), (21.0, 21.45, "15m"), (28.0, 29.7, "10m")]
CALL_RE  = re.compile(r"^[A-Z0-9]{0,3}[0-9][A-Z0-9]{0,3}[A-Z](?:/[A-Z0-9]+)?$")
CLASS_RE = re.compile(r"^\d{1,2}[A-F]$")
SECT_RE  = re.compile(r"^[A-Z]{2,4}$")
SIGNOFFS = {"73", "RR73", "RRR"}


def band_of(dial_hz):
    if dial_hz is None:
        return None
    mhz = dial_hz / 1e6
    for lo, hi, name in BAND_RANGES:
        if lo <= mhz <= hi:
            return name
    return None  # WARC / out of FD bands -> not creditable


def iter_records(path):
    with open(path) as f:
        for line in f:
            line = line.strip()
            if line:
                try:
                    yield json.loads(line)
                except json.JSONDecodeError:
                    pass


def rec_time(r):
    t = (r.get("event", {}) or {}).get("t")
    if isinstance(t, (int, float)):
        return t
    cap = r.get("captured_at")
    if cap:
        try:
            return datetime.fromisoformat(cap.replace("Z", "+00:00")).timestamp() * 1000
        except ValueError:
            return None
    return None


def raw_text(r):
    """The literal transmitted message string, for either schema."""
    ev = r.get("event", {}) or {}
    if r.get("direction") == "sent":
        return (ev.get("message", {}) or {}).get("text")
    sl = (ev.get("content", {}) or {}).get("Slotted") or {}
    return sl.get("raw")


def parse_raw(s):
    """Regex-only parse of an FT8/FT4 line. Returns
    (to, frm, kind, rogered, cls, sec) or None. kind in {Exchange, Signoff}."""
    if not s:
        return None
    toks = s.upper().split()
    if len(toks) < 2 or toks[0] in ("CQ", "QRZ"):
        return None
    to, frm, rest = toks[0], toks[1], toks[2:]
    if rest and rest[-1] in SIGNOFFS:
        return (to, frm, "Signoff", False, None, None)
    rogered = False
    if rest and rest[0] == "R":
        rogered, rest = True, rest[1:]
    if len(rest) >= 2 and CLASS_RE.match(rest[0]) and SECT_RE.match(rest[1]):
        return (to, frm, "Exchange", rogered, rest[0], rest[1])
    return None  # grid / signal report / roger-report / anything non-FD


def build_raw_set():
    qsos = collections.defaultdict(lambda: dict(HE=0, HER=0, HS=0, SE=0, SER=0, SS=0))
    for path in FILES:
        for r in iter_records(path):
            parsed = parse_raw(raw_text(r))
            if not parsed:
                continue
            to, frm, kind, rogered, cls, sec = parsed
            if frm == SUB_CALL and to and to not in OUR_CALLS:
                partner, d = to, "out"
            elif to == SUB_CALL and frm and frm not in OUR_CALLS:
                partner, d = frm, "in"
            else:
                continue
            t = rec_time(r)
            if t is None or not (WIN_START <= t < WIN_END):
                continue
            band = band_of(r.get("dial_hz"))
            if band is None or not CALL_RE.match(partner):
                continue
            q = qsos[(partner, band)]
            if d == "out":
                if kind == "Signoff":
                    q["SS"] += 1
                else:
                    q["SE"] += 1
                    q["SER"] += rogered
            else:
                if kind == "Signoff":
                    q["HS"] += 1
                else:
                    q["HE"] += 1
                    q["HER"] += rogered

    credit = set()
    for key, q in qsos.items():
        HE, HER, HS, SE, SER, SS = (q["HE"], q["HER"], q["HS"], q["SE"], q["SER"], q["SS"])
        tierA = bool(HE and (HER or (HS and (SE or SS))))
        tierB = bool((not tierA) and HE and SER)
        if tierA or tierB:
            credit.add(key)
    return credit


def load_structured_set():
    s = set()
    with open(CSV_OUT) as f:
        for row in csv.DictReader(f):
            s.add((row["Call"], row["Band"]))
    return s


def main():
    raw_set = build_raw_set()
    struct_set = load_structured_set()

    print("=" * 64)
    print("CROSS-CHECK: raw-text reconstruction  vs  fd_report.py CSV")
    print("=" * 64)
    print(f"  structured-field method (fd_report) : {len(struct_set):4d} QSOs")
    print(f"  raw-text method (this script)       : {len(raw_set):4d} QSOs")
    print(f"  agree on                            : {len(raw_set & struct_set):4d} QSOs")

    only_struct = struct_set - raw_set
    only_raw = raw_set - struct_set
    print(f"  only in structured (not in raw)     : {len(only_struct):4d}")
    print(f"  only in raw (not in structured)     : {len(only_raw):4d}")

    bands = ["160m", "80m", "40m", "20m", "15m", "10m"]
    sb = collections.Counter(b for _, b in struct_set)
    rb = collections.Counter(b for _, b in raw_set)
    print("\n  per-band   structured / raw:")
    for b in bands:
        if sb.get(b) or rb.get(b):
            flag = "" if sb.get(b, 0) == rb.get(b, 0) else "   <-- differ"
            print(f"    {b:<5} {sb.get(b,0):4d} / {rb.get(b,0):4d}{flag}")

    if only_struct:
        print("\n  In structured but NOT raw (inspect):")
        for k in sorted(only_struct):
            print("    ", k)
    if only_raw:
        print("\n  In raw but NOT structured (inspect):")
        for k in sorted(only_raw):
            print("    ", k)

    agree = len(raw_set & struct_set)
    union = len(raw_set | struct_set)
    pct = 100.0 * agree / union if union else 100.0
    print("\n  Jaccard agreement: %.2f%%" % pct)
    print("  VERDICT:", "EXACT MATCH ✓" if not only_struct and not only_raw
          else "differences above — review")
    print("=" * 64)


if __name__ == "__main__":
    main()
