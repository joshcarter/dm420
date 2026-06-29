#!/usr/bin/env python3
"""
fd_report.py  --  Build ARRL Field Day 2026 submission data for N0JDC from the
raw DM420 decode captures (decodes-joel.json + decodes-josh.jsonl).

Station: N0JDC, Class 2B, Section CO.  Two transmitters were on the air at once
(Joel + Josh), so creditable QSOs are counted for the STATION, deduped once per
(partner, band) -- FT8 and FT4 are both "Digital" and share one dupe bucket per
ARRL rules, so a station worked on 20m FT8 and 20m FT4 is a single credit.

A QSO is creditable when the Field Day exchange (class+section) demonstrably went
BOTH ways during the FD window.  Two confidence tiers (see qso_flow discussion):

  CONFIRMED  - we copied their class+section AND have explicit proof they copied
               ours: either they sent us RR73/RRR/73, or they sent "N0JDC <CALL>
               R <cls> <sec>" (the 'R' rogers our report).
  EXCHANGED  - we copied their class+section AND we transmitted our rogered
               exchange ("<CALL> N0JDC R 2B CO") to them, but never decoded their
               final RR73 (the lost-RR73 case).  Counted for Field Day because we
               have good reason to believe it completed (FD is unadjudicated).

Both tiers are creditable; the split is reported so it can be reviewed.

Because the two transmitters overhear each other, ANY message from=N0JDC->partner
(whether 'sent' by this op or 'heard' from the other op) counts as "we sent", and
any partner->N0JDC message counts as "we received".  This is robust to gaps in a
single transmitter's own sent-log.

Outputs (written next to this script in fd-results/):
  fd_submission_report.txt   - human-readable summary to enter at fdentry.php
  fd_worked_stations.csv      - the by-band/mode list of stations worked
"""

import json, csv, collections, re, os
from datetime import datetime, timezone

HERE = os.path.dirname(os.path.abspath(__file__))
FILES = [os.path.join(HERE, "decodes-joel.json"),
         os.path.join(HERE, "decodes-josh.jsonl")]

OUR_CALLS = {"N0JDC", "W4LL"}   # N0JDC = FD call; W4LL = Joel's own call (pre-FD tests)
SUB_CALL  = "N0JDC"             # the call we submit under

# ARRL Field Day 2026: 1800 UTC Sat Jun 27 -> 2059 UTC Sun Jun 28.
WIN_START = datetime(2026, 6, 27, 18, 0, 0, tzinfo=timezone.utc).timestamp() * 1000
WIN_END   = datetime(2026, 6, 28, 21, 0, 0, tzinfo=timezone.utc).timestamp() * 1000

# Field-Day-legal HF bands (no 30/17/12m WARC).
BAND_RANGES = [
    (1.8, 2.0, "160m", True), (3.5, 4.0, "80m", True), (5.3, 5.45, "60m", False),
    (7.0, 7.3, "40m", True), (10.1, 10.15, "30m", False), (14.0, 14.35, "20m", True),
    (18.0, 18.2, "17m", False), (21.0, 21.45, "15m", True), (24.8, 25.0, "12m", False),
    (28.0, 29.7, "10m", True),
]
CALL_RE = re.compile(r"^[A-Z0-9]{0,3}[0-9][A-Z0-9]{0,3}[A-Z](?:/[A-Z0-9]+)?$")


def band_of(dial_hz):
    if dial_hz is None:
        return None, False
    mhz = dial_hz / 1e6
    for lo, hi, name, legal in BAND_RANGES:
        if lo <= mhz <= hi:
            return name, legal
    return None, False


def iter_records(path):
    with open(path) as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            try:
                yield json.loads(line)
            except json.JSONDecodeError:
                pass


def record_time_ms(r):
    ev = r.get("event", {}) or {}
    t = ev.get("t")
    if isinstance(t, (int, float)):
        return t
    cap = r.get("captured_at")
    if cap:
        try:
            return datetime.fromisoformat(cap.replace("Z", "+00:00")).timestamp() * 1000
        except ValueError:
            return None
    return None


def get_message(r):
    """Return (kind, body, raw) for both 'heard' and 'sent' record schemas."""
    ev = r.get("event", {}) or {}
    if r.get("direction") == "sent":
        m = ev.get("message", {}) or {}
        st = m.get("structured", {}) or {}
        raw = m.get("text")
        for k, b in st.items():
            return k, b, raw
        return None, None, raw
    sl = (ev.get("content", {}) or {}).get("Slotted") or {}
    msg = sl.get("message", {})
    raw = sl.get("raw")
    if isinstance(msg, dict):
        for k, b in msg.items():
            return k, b, raw
    return None, None, raw


def blank_qso():
    return dict(
        HE=0, HER=0, HS=0, SE=0, SER=0, SS=0,
        their_class=None, their_section=None,
        submodes=set(), files=set(), first_t=None, last_t=None,
        best_snr=None,
    )


def main():
    qsos = collections.defaultdict(blank_qso)       # (partner, band) -> evidence
    bad_calls = collections.Counter()
    outside_window_complete = 0
    null_band_involved = 0
    stats = collections.Counter()

    for path in FILES:
        fname = os.path.basename(path)
        for r in iter_records(path):
            kind, body, raw = get_message(r)
            if kind not in ("Exchange", "Signoff") or not isinstance(body, dict):
                continue
            t = record_time_ms(r)
            mode = (r.get("event", {}) or {}).get("mode")     # Ft8 / Ft4
            band, legal = band_of(r.get("dial_hz"))

            to = body.get("to")
            frm = body.get("from")

            # Identify the (us, partner) directionality.
            partner = direction = None
            if frm == SUB_CALL and to and to not in OUR_CALLS:
                partner, direction = to, "out"          # us -> partner
            elif to == SUB_CALL and frm and frm not in OUR_CALLS:
                partner, direction = frm, "in"           # partner -> us
            else:
                continue

            # Field Day exchange payload (class+section) -- only FD exchanges matter.
            fd = None
            if kind == "Exchange":
                pl = body.get("payload", {})
                if isinstance(pl, dict) and "FieldDay" in pl:
                    fd = pl["FieldDay"]
                else:
                    continue     # non-FD exchange (grid/report) -- ignore for FD credit
            # (Signoff has no payload; it still contributes confirmation.)

            stats["fd_relevant_msgs"] += 1

            if t is None or not (WIN_START <= t < WIN_END):
                # Track exchanges that would have completed but fall outside the window.
                if direction == "in" and fd is not None:
                    outside_window_complete += 1
                continue
            if band is None:
                null_band_involved += 1
                continue
            if not legal:
                continue  # WARC / non-FD band -- not creditable

            if not CALL_RE.match(partner):
                bad_calls[partner] += 1
                # keep going but these are flagged; do not credit obviously bad calls
                continue

            q = qsos[(partner, band)]
            q["files"].add(fname)
            if mode:
                q["submodes"].add(mode)
            if t is not None:
                q["first_t"] = t if q["first_t"] is None else min(q["first_t"], t)
                q["last_t"] = t if q["last_t"] is None else max(q["last_t"], t)
            snr = (r.get("event", {}) or {}).get("snr_db")
            if direction == "in" and isinstance(snr, (int, float)):
                q["best_snr"] = snr if q["best_snr"] is None else max(q["best_snr"], snr)

            rogered = bool(fd.get("rogered")) if fd else False
            if direction == "out":
                if kind == "Signoff":
                    q["SS"] += 1
                else:
                    q["SE"] += 1
                    if rogered:
                        q["SER"] += 1
            else:  # in
                if kind == "Signoff":
                    q["HS"] += 1
                else:
                    q["HE"] += 1
                    if rogered:
                        q["HER"] += 1
                    if fd:
                        q["their_class"] = fd.get("class") or q["their_class"]
                        q["their_section"] = fd.get("section") or q["their_section"]

    # ---- classify ----
    confirmed, exchanged, incomplete = [], [], []
    for (partner, band), q in qsos.items():
        HE, HER, HS = q["HE"], q["HER"], q["HS"]
        SE, SER, SS = q["SE"], q["SER"], q["SS"]
        tierA = bool(HE and (HER or (HS and (SE or SS))))
        tierB = bool((not tierA) and HE and SER)
        rec = dict(call=partner, band=band, **q)
        if tierA:
            rec["tier"] = "CONFIRMED"; confirmed.append(rec)
        elif tierB:
            rec["tier"] = "EXCHANGED"; exchanged.append(rec)
        else:
            # reason for not crediting
            if HE and not SE and not SER:
                rec["why"] = "heard their exchange; we never sent ours"
            elif (SE or SER) and not HE:
                rec["why"] = "we sent ours; never copied their exchange"
            else:
                rec["why"] = "partial/one-sided"
            rec["tier"] = "INCOMPLETE"; incomplete.append(rec)

    creditable = confirmed + exchanged
    creditable.sort(key=lambda r: (r["band"], r["call"]))

    # ---- QSO totals by band (all Digital) ----
    band_order = ["160m", "80m", "40m", "20m", "15m", "10m"]
    by_band = collections.Counter(r["band"] for r in creditable)
    by_band_conf = collections.Counter(r["band"] for r in confirmed)
    by_band_exch = collections.Counter(r["band"] for r in exchanged)
    total = len(creditable)

    # ---- write worked-stations CSV ----
    csv_path = os.path.join(HERE, "fd_worked_stations.csv")
    with open(csv_path, "w", newline="") as f:
        w = csv.writer(f)
        w.writerow(["Call", "Band", "Mode", "Submodes", "TheirClass",
                    "TheirSection", "Tier", "FirstUTC", "BestSNR", "SeenIn"])
        for r in creditable:
            ts = (datetime.fromtimestamp(r["first_t"] / 1000, timezone.utc)
                  .strftime("%Y-%m-%d %H:%M:%S") if r["first_t"] else "")
            w.writerow([r["call"], r["band"], "Digital",
                        "+".join(sorted(s.upper() for s in r["submodes"])),
                        r["their_class"] or "", r["their_section"] or "",
                        r["tier"], ts,
                        r["best_snr"] if r["best_snr"] is not None else "",
                        ",".join(sorted(x.replace("decodes-", "").replace(".jsonl", "").replace(".json", "")
                                        for x in r["files"]))])

    # ---- write report ----
    lines = []
    P = lines.append
    P("=" * 72)
    P("ARRL FIELD DAY 2026  --  N0JDC submission data (derived from DM420 decodes)")
    P("=" * 72)
    P("")
    P("Station call .......... N0JDC")
    P("Entry class ........... 2B  (2 transmitters, Class B portable)")
    P("ARRL/RAC section ...... CO  (Colorado)")
    P("FD period ............. 2026-06-27 1800Z  ->  2026-06-28 2100Z")
    P("                        (setup began 1800Z Sat -> full 24h+ period allowed)")
    P("Source files .......... decodes-joel.json, decodes-josh.jsonl (merged, deduped)")
    P("")
    P("-" * 72)
    P("QSO TOTALS BY BAND AND MODE  (all contacts are Digital / FT8+FT4)")
    P("-" * 72)
    P(f"{'Band':<6}{'Digital QSOs':>14}{'  (Confirmed':>14}{' + Exchanged)':>14}")
    for b in band_order:
        if by_band.get(b):
            P(f"{b:<6}{by_band[b]:>14}{by_band_conf.get(b,0):>14}{by_band_exch.get(b,0):>14}")
    P(f"{'TOTAL':<6}{total:>14}{len(confirmed):>14}{len(exchanged):>14}")
    P("")
    P("Mode breakdown: Digital QSOs = %d (Phone = 0, CW = 0)" % total)
    P("Digital QSO points = QSOs x 2 = %d  (before power multiplier)" % (total * 2))
    P("Power multiplier: x5 if all QSOs made at <=5 W from a non-commercial power")
    P("  source (QRP); otherwise x2 for <=150 W. -> claimed QSO points =")
    P("  %d (x5 QRP)  or  %d (x2)." % (total * 2 * 5, total * 2 * 2))
    P("")
    P("Confidence split:")
    P("  CONFIRMED  (their RR73/RRR/73 or their 'R' copied) ... %d" % len(confirmed))
    P("  EXCHANGED  (we sent R+exchange, their final RR73 not")
    P("             decoded; counted per FD unadjudicated policy) %d" % len(exchanged))
    P("")
    P("-" * 72)
    P("DATA NOT DERIVABLE FROM DECODES -- fill in by hand at fdentry.php:")
    P("-" * 72)
    P("  * Number of participants / list of operators (Joel W4LL, Josh ...)")
    P("  * GOTA station call + GOTA QSO totals (if any)")
    P("  * Claimed bonus points (100% emergency power, public location, media")
    P("    publicity, NTS messages, alternate power, etc.) + documentation")
    P("  * Power source description for the power multiplier above")
    P("")
    P("-" * 72)
    P("DIAGNOSTICS")
    P("-" * 72)
    P("  Field-Day exchange messages involving N0JDC ......... %d" % stats["fd_relevant_msgs"])
    P("  Distinct (partner,band) pairs with any involvement ... %d" % len(qsos))
    P("  Creditable QSOs (Confirmed+Exchanged) ............... %d" % total)
    P("  Incomplete / not credited ........................... %d" % len(incomplete))
    P("  In-band exchanges OUTSIDE the FD window (excluded) .. %d" % outside_window_complete)
    P("  Records dropped for null/unknown band ............... %d" % null_band_involved)
    if bad_calls:
        P("  Implausible partner callsigns skipped (decode busts): %d" %
          sum(bad_calls.values()))
        P("    e.g. " + ", ".join(list(bad_calls)[:10]))
    P("")
    P("Incomplete breakdown (top reasons):")
    why = collections.Counter(r["why"] for r in incomplete)
    for reason, n in why.most_common():
        P("  %4d  %s" % (n, reason))
    P("")
    P("Worked-stations list written to: fd_worked_stations.csv (%d rows)" % total)
    P("=" * 72)

    report = "\n".join(lines)
    print(report)
    with open(os.path.join(HERE, "fd_submission_report.txt"), "w") as f:
        f.write(report + "\n")


if __name__ == "__main__":
    main()
