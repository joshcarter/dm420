#!/usr/bin/env python3
"""
fd_logbook_check.py -- Compare DM420's own committed logbooks against the QSO set
reconstructed from the raw decodes by fd_report.py.

DM420 commits a QSO to the logbook only when the contact is confirmed (RR73/RRR
received, or RR73 sent on the answering side).  So the logbook is expected to
line up with fd_report's CONFIRMED tier, and to be MISSING the EXCHANGED tier
(the lost-RR73 QSOs we chose to additionally credit).  This script checks that.
"""

import json, csv, os, collections
from datetime import datetime, timezone

HERE = os.path.dirname(os.path.abspath(__file__))
LOGS = {"joel": os.path.join(HERE, "logbook-joel.json"),
        "josh": os.path.join(HERE, "logbook-josh.json")}
CSV_OUT = os.path.join(HERE, "fd_worked_stations.csv")

WIN_START = datetime(2026, 6, 27, 18, 0, 0, tzinfo=timezone.utc).timestamp() * 1000
WIN_END   = datetime(2026, 6, 28, 21, 0, 0, tzinfo=timezone.utc).timestamp() * 1000


def load_log(path):
    rows = []
    with open(path) as f:
        for line in f:
            line = line.strip()
            if line:
                try:
                    rows.append(json.loads(line))
                except json.JSONDecodeError:
                    pass
    return rows


def band_norm(b):
    # logbook bands look like "B10m", "B20m" ...
    return b[1:] if isinstance(b, str) and b.startswith("B") else b


def in_window(t):
    return isinstance(t, (int, float)) and WIN_START <= t < WIN_END


def qso_key(rec):
    return (rec.get("call"), band_norm(rec.get("band")))


def main():
    # ---- 1. load both logbooks, check they agree with each other ----
    raw = {who: load_log(p) for who, p in LOGS.items()}
    print("=" * 66)
    print("LOGBOOK CROSS-CHECK")
    print("=" * 66)
    for who, rows in raw.items():
        contests = collections.Counter(r.get("contest") for r in rows)
        oow = sum(1 for r in rows if not in_window(r.get("time")))
        print(f"  logbook-{who}: {len(rows)} rows | contest={dict(contests)} | "
              f"out-of-window={oow}")

    # identity comparison between the two logbooks (by full QSO identity id)
    def idset(rows):
        return {json.dumps(r.get("id"), sort_keys=True) for r in rows}
    ij, ih = idset(raw["joel"]), idset(raw["josh"])
    print(f"\n  same QSO-id in both logbooks : {len(ij & ih)}")
    print(f"  only in joel's logbook       : {len(ij - ih)}")
    print(f"  only in josh's logbook       : {len(ih - ij)}")

    # ---- 2. merged, deduped logbook QSO set (FD, in window) ----
    merged = {}                       # (call,band) -> rec
    dupes = collections.Counter()
    fd_inwin = 0
    for who, rows in raw.items():
        for r in rows:
            if r.get("contest") != "ArrlFieldDay" or not in_window(r.get("time")):
                continue
            fd_inwin += 1
            k = qso_key(r)
            if k in merged:
                dupes[k] += 1
            else:
                merged[k] = r
    log_set = set(merged)
    print(f"\n  FD logbook rows in window (both files) ...... {fd_inwin}")
    print(f"  distinct (call,band) logged QSOs ............ {len(log_set)}")
    if dupes:
        print(f"  (call,band) pairs logged more than once ..... {len(dupes)} "
              f"(e.g. {list(dupes)[:5]})")

    # ---- 3. load fd_report's reconstructed set, split by tier ----
    mine_all, mine_conf, mine_exch = set(), set(), set()
    with open(CSV_OUT) as f:
        for row in csv.DictReader(f):
            k = (row["Call"], row["Band"])
            mine_all.add(k)
            (mine_conf if row["Tier"] == "CONFIRMED" else mine_exch).add(k)

    print("\n" + "-" * 66)
    print("LOGBOOK  vs  DECODE RECONSTRUCTION")
    print("-" * 66)
    print(f"  logbook distinct QSOs ............... {len(log_set)}")
    print(f"  reconstruction ALL (conf+exch) ..... {len(mine_all)} "
          f"({len(mine_conf)} confirmed + {len(mine_exch)} exchanged)")
    print(f"  agree (in both) .................... {len(log_set & mine_all)}")

    only_log = log_set - mine_all
    only_mine = mine_all - log_set
    print(f"  in logbook but NOT reconstructed ... {len(only_log)}")
    print(f"  reconstructed but NOT in logbook ... {len(only_mine)}")

    # How does the logbook line up specifically with the CONFIRMED tier?
    print(f"\n  logbook vs CONFIRMED tier:")
    print(f"    confirmed QSOs also in logbook ... {len(mine_conf & log_set)} / {len(mine_conf)}")
    print(f"    confirmed QSOs NOT in logbook .... {len(mine_conf - log_set)}")
    print(f"  logbook vs EXCHANGED tier (lost-RR73):")
    print(f"    exchanged QSOs in logbook ....... {len(mine_exch & log_set)} / {len(mine_exch)}")
    print(f"    exchanged QSOs NOT in logbook ... {len(mine_exch - log_set)}")

    # ---- 4. itemize the disagreements ----
    if only_mine:
        bytier = collections.Counter("CONFIRMED" if k in mine_conf else "EXCHANGED"
                                     for k in only_mine)
        print(f"\n  Reconstructed-but-not-logged breakdown by tier: {dict(bytier)}")
        ex = [k for k in sorted(only_mine) if k in mine_conf]
        if ex:
            print("    CONFIRMED but unlogged (worth a look):")
            for k in ex[:20]:
                print("      ", k)

    if only_log:
        print("\n  Logged but NOT reconstructed from decodes (investigate):")
        for k in sorted(only_log):
            r = merged[k]
            ts = datetime.fromtimestamp(r["time"]/1000, timezone.utc).strftime("%m-%d %H:%M")
            print(f"      {k}  rcvd={r.get('exchange_rcvd')!r} {r.get('mode')} {ts}")

    union = len(log_set | mine_all)
    print("\n  Jaccard(logbook, reconstruction ALL): %.1f%%" %
          (100.0*len(log_set & mine_all)/union if union else 100.0))
    print("  Jaccard(logbook, CONFIRMED tier):     %.1f%%" %
          (100.0*len(log_set & mine_conf)/len(log_set | mine_conf)
           if (log_set | mine_conf) else 100.0))
    print("=" * 66)


if __name__ == "__main__":
    main()
