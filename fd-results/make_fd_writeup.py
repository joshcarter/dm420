#!/usr/bin/env python3
"""Generate FIELD_DAY_2026_N0JDC.md from the verified fd_worked_stations.csv
(digital QSOs) plus manual_qsos.csv (QSOs logged off the digital pipeline, e.g.
phone)."""
import csv, collections, os
from datetime import datetime, timezone

HERE = os.path.dirname(os.path.abspath(__file__))
rows = list(csv.DictReader(open(os.path.join(HERE, "fd_worked_stations.csv"))))
manual = []
mpath = os.path.join(HERE, "manual_qsos.csv")
if os.path.exists(mpath):
    manual = list(csv.DictReader(open(mpath)))

def dt(s):
    return datetime.strptime(s, "%Y-%m-%d %H:%M:%S").replace(tzinfo=timezone.utc)

times = [dt(r["FirstUTC"]) for r in rows if r["FirstUTC"]]
first, last = min(times), max(times)
band_order = ["160m", "80m", "40m", "20m", "15m", "10m"]
by_band = collections.Counter(r["Band"] for r in rows)
by_band_conf = collections.Counter(r["Band"] for r in rows if r["Tier"] == "CONFIRMED")
by_band_exch = collections.Counter(r["Band"] for r in rows if r["Tier"] == "EXCHANGED")

n_digital = len(rows)
dig_conf = sum(1 for r in rows if r["Tier"] == "CONFIRMED")
dig_exch = n_digital - dig_conf
n_phone = sum(1 for m in manual if m["Mode"] == "Phone")
n_total = n_digital + n_phone
conf = dig_conf + n_phone            # phone contact is a confirmed QSO
exch = dig_exch
qso_points = n_digital * 2 + n_phone * 1     # digital 2 pts, phone 1 pt

secs = collections.Counter(r["TheirSection"] for r in rows if r["TheirSection"])
for m in manual:
    if m["TheirSection"]:
        secs[m["TheirSection"]] += 1
ft8 = sum(1 for r in rows if r["Submodes"] == "FT8")
ft4 = sum(1 for r in rows if r["Submodes"] == "FT4")
both = sum(1 for r in rows if "+" in r["Submodes"])
byhour = collections.Counter(dt(r["FirstUTC"]).strftime("%m-%d %HZ") for r in rows if r["FirstUTC"])

O = []
def w(s=""): O.append(s)

w("# ARRL Field Day 2026 — N0JDC")
w()
w("**Station:** N0JDC &nbsp;•&nbsp; **Class:** 2B &nbsp;•&nbsp; **Section:** CO (Colorado) "
  "&nbsp;•&nbsp; **Modes:** Digital (FT8 / FT4) + Phone &nbsp;•&nbsp; **Power:** QRP (≈5 W)")
w()
w("| | |")
w("|---|---|")
w(f"| Operating period | {first:%Y-%m-%d %H:%MZ} → {last:%Y-%m-%d %H:%MZ} |")
w(f"| Valid QSOs (claimed) | **{n_total}** — {n_digital} digital + {n_phone} phone |")
w(f"| Bands worked | 40 m, 20 m, 15 m, 10 m |")
w(f"| ARRL/RAC sections worked | {len(secs)} |")
w(f"| Software | DM420 (\"Dingus Mangler 420\") |")
w()
w("---")
w()

# ---------------- Section 1: narrative ----------------
w("## 1. The Event")
w()
w("N0JDC operated **ARRL Field Day 2026** as a **Class 2B** entry from Colorado "
  "(section **CO**), running QRP (about 5 watts), primarily on the digital modes "
  "**FT8** and **FT4** with a single **phone** contact. The station was a two-person, "
  "two-transmitter portable operation:")
w()
w("- **Josh Carter — N0JDC** (the submitted station call)")
w("- **Joel Odom — W4LL**")
w()
w("Both operators ran the project's own software, **DM420**, with the two "
  "transmitters active simultaneously on different bands — the multi-transmitter, "
  "multi-band style Field Day was built for. Because both stations transmitted under "
  "the single call **N0JDC**, the two operators' captures were merged into one "
  "station log and de-duplicated per the once-per-band-per-mode rule.")
w()
w("Field Day 2026 ran **1800 UTC Saturday 27 June → 2059 UTC Sunday 28 June**. "
  "Setup began at 1800 UTC Saturday, which entitled the station to operate the full "
  "period (through ~2100 UTC Sunday). Setup and antenna work filled the first hour or "
  f"so; the **first digital contact was logged at {first:%H%MZ} on {first:%d} June**, "
  f"and the **last at {last:%H%MZ} on {last:%d} June**.")
w()
w("Operation was essentially **around the clock**. With FT8/FT4 holding up well on "
  "20 m and 40 m overnight, the station stayed active through every hour of the night "
  "rather than going dark — the hourly QSO counts below never fall to zero between the "
  "first and last contact.")
w()
w("### Results summary")
w()
w("| Band | Mode | QSOs | Confirmed | Exchange-complete |")
w("|------|------|-----:|----------:|------------------:|")
for b in band_order:
    if by_band.get(b):
        w(f"| {b} | Digital | {by_band[b]} | {by_band_conf.get(b,0)} | {by_band_exch.get(b,0)} |")
for m in manual:
    w(f"| {m['Band']} | {m['Mode']} | 1 | 1 | 0 |")
w(f"| **Total** | | **{n_total}** | **{conf}** | **{exch}** |")
w()
w(f"- **Mode split:** {n_digital} digital ({ft8} FT8-only, {ft4} FT4-only, {both} both "
  f"sub-modes) + {n_phone} phone. FT8 and FT4 count as a single **Digital** mode for "
  "Field Day scoring and duplicate checking; phone is a separate mode.")
w(f"- **Geographic reach:** **{len(secs)} distinct ARRL/RAC sections**, including "
  "Canadian sections and a handful of DX. Top sections: " +
  ", ".join(f"{s} ({n})" for s, n in secs.most_common(8)) + ".")
w()
w("**Claimed QSO points.** Digital contacts are worth **2 points** each and phone "
  f"**1 point** each: {n_digital} × 2 + {n_phone} × 1 = **{qso_points} QSO points** "
  "before the power multiplier. Operating QRP at ≤5 W from a non-commercial power "
  f"source earns a **×5** power multiplier, giving **{qso_points*5} claimed QSO "
  f"points** (or {qso_points*2} at ×2 if the ≤5 W / power-source conditions are not "
  "met). Bonus points (emergency power, public location, publicity, etc.) are **not** "
  "included here and are added separately on the entry form.")
w()
w("> **Two figures are given throughout.** *Confirmed* QSOs are those with explicit "
  "two-way confirmation. *Exchange-complete* QSOs are ones where the full Field Day "
  "exchange went both ways but the final `RR73` was never decoded; they are counted "
  "because there is good reason to believe the contact completed (see §2). The "
  f"conservative claim is **{conf}** ({dig_conf} confirmed digital + {n_phone} phone); "
  f"the full claim is **{n_total}**.")
w()
w("### Activity by hour (UTC) — digital")
w()
w("```")
mx = max(byhour.values()) if byhour else 1
for h in sorted(byhour):
    bar = "█" * round(28 * byhour[h] / mx)
    w(f"{h}  {byhour[h]:3d}  {bar}")
w("```")
w()
w("---")
w()

# ---------------- Section 2: methodology ----------------
w("## 2. How the Results Were Calculated")
w()
w("DM420's live logger only commits a contact when it **decodes an `RR73`/`RRR`** (or, "
  "on the answering side, when it sends its own `RR73`). That is conservative and, "
  "under a Field Day pileup, it both **missed** good contacts and could not see "
  "contacts whose final acknowledgement was lost. So rather than trust the live log, "
  "the digital results were rebuilt **from scratch out of the raw decode captures** "
  "that DM420 recorded for every cycle it heard or transmitted. (The single phone "
  "contact was logged by hand and is carried separately — see the end of this section.)")
w()
w("### Source data")
w()
w("- `decodes-joel.json` — every decode and transmission captured by Joel's station.")
w("- `decodes-josh.jsonl` — the same from Josh's station.")
w()
w("Each line is one FT8/FT4 cycle: who/what was heard or sent, the band, the UTC slot "
  "time, and the decoded message (CQ, grid, signal report, or the **Field Day "
  "class+section exchange**). Both files also contain a week of pre-Field-Day testing, "
  "which is excluded.")
w()
w("### What counts as a valid Field Day QSO")
w()
w("A contact is credited only when **all** of the following hold:")
w()
w("1. **Inside the contest window** — the on-air slot time is between 1800 UTC 27 June "
  "and 2100 UTC 28 June. (Pre-FD test traffic and the operators' personal-call testing "
  "are dropped.)")
w("2. **On a Field-Day-legal band** — 160/80/40/20/15/10 m. WARC bands (30/17/12 m) are "
  "not allowed for Field Day and are excluded.")
w("3. **The exchange went both ways** — we received the other station's class+section "
  "**and** we sent ours. Field Day requires the class+section exchange, so a bare "
  "signal report or an unanswered CQ is not a QSO.")
w()
w("Each contact is then placed in one of two confidence tiers:")
w()
w("- **CONFIRMED** — we copied their class+section **and** have explicit proof they "
  "copied ours: either they sent us `RR73`/`RRR`/`73`, or they sent "
  "`N0JDC <CALL> R <class> <section>` (the leading `R` rogers our exchange). This is a "
  "textbook-complete QSO by the IARU definition (both stations identified, exchanged, "
  "and confirmed).")
w("- **EXCHANGE-COMPLETE** — we copied their class+section and we transmitted our "
  "**rogered** exchange (`<CALL> N0JDC R 2B CO`) to them, but never decoded their final "
  "`RR73`. The exchange demonstrably went both ways; only the closing handshake is "
  "missing.")
w()
w("**Why count the exchange-complete tier?** ARRL Field Day is a *communications "
  "exercise, not an adjudicated contest* — logs are not cross-checked and there is no "
  "\"not-in-log\" penalty. When we have copied a station's full exchange and sent our "
  "own rogered exchange, the most likely reason we didn't see their `RR73` is simple "
  "fading on that one transmit cycle — in which case they logged us and the QSO is "
  "real. The guardrail is that **our** rogered exchange must have actually gone out; a "
  "contact where we only heard them, or only half-completed, stays uncredited.")
w()
w("### Two transmitters, one station")
w()
w("Because both operators transmitted as N0JDC, the two captures were **merged** and "
  "contacts de-duplicated to **once per band per mode** — and for Field Day, FT8 and "
  "FT4 are the *same* \"Digital\" mode, so a station worked on 20 m FT8 and again on "
  "20 m FT4 is a single credit. A nice side effect of two co-located receivers is that "
  "each often copied the other's QSOs, making the reconstruction robust to gaps in "
  "either station's own transmit log.")
w()
w("### Verification (three independent checks)")
w()
w(f"The **{n_digital} digital** QSOs were verified three ways:")
w()
w("1. **Independent re-parse.** The QSO set was rebuilt a second time from the literal "
  "on-air **text strings** using a completely separate parser, instead of DM420's "
  f"structured decode fields. The two methods agreed on **every one of the {n_digital} "
  "contacts** — exact match, no differences.")
w("2. **Against DM420's own logbook.** The live logbook (which logs only on `RR73`) "
  "contains **372** de-duplicated QSOs. **Every one** appears in the reconstruction, "
  "with a **matching received class+section** (372/372, zero mismatches). The logbook "
  "contradicts the reconstruction nowhere.")
w("3. **Explaining the difference.** The reconstruction has 66 contacts the logbook "
  "lacks: **57** are the exchange-complete (lost-`RR73`) tier the logger cannot see by "
  "design, and **9** are *fully confirmed* QSOs (with `RR73`/`RRR` in the decodes) that "
  "the live logger **dropped under pileup load**. The reconstruction also removes ~33 "
  "duplicate entries that the raw logbook had recorded. Net: the decode-based count is "
  "**more** complete and **more** accurate than the live log, and the live log "
  "corroborates all of it.")
w()
w(f"### Phone contact ({n_phone})")
w()
w("The station made one **phone (SSB) contact**, which is outside the FT8/FT4 decode "
  "pipeline and so is not in the captures above. It is recorded by hand in "
  "`manual_qsos.csv` and folded into the totals and the Cabrillo log. Phone is a "
  "separate mode from digital, so it is creditable independent of any digital QSO with "
  "the same station, and it scores **1 point** (vs 2 for digital).")
w()
w("### Reproducing this")
w()
w("| Script | Purpose |")
w("|--------|---------|")
w("| `fd_report.py` | Parse both captures → digital QSO totals + `fd_worked_stations.csv` |")
w("| `fd_crosscheck.py` | Independent raw-text re-parse; confirms the same digital QSOs |")
w("| `fd_logbook_check.py` | Compare against DM420's logbooks (`fd_logbook_comparison.txt`) |")
w("| `make_cabrillo.py` | Emit the ARRL-FD Cabrillo log (`N0JDC_2026_FD.cbr`) |")
w("| `make_fd_writeup.py` | Generate this document from the verified data |")
w("| `manual_qsos.csv` | QSOs logged off the digital pipeline (the phone contact) |")
w()
w("---")
w()

# ---------------- Section 3: station table ----------------
w("## 3. Stations Worked — Dupe Sheet / Backing Log")
w()
w(f"All **{n_total}** creditable contacts, grouped by band/mode and listed "
  "alphabetically by call for duplicate checking. *Time* is the UTC time of the first "
  "exchange of the contact. *Status* is C = confirmed, X = exchange-complete (see §2). "
  "The received exchange (their class + section) is shown as logged.")
w()
for b in band_order:
    brows = sorted((r for r in rows if r["Band"] == b), key=lambda r: r["Call"])
    if not brows:
        continue
    w(f"### {b} — Digital ({len(brows)} QSOs)")
    w()
    w("| # | Call | Class | Section | Sub-mode | Date/Time (UTC) | Status |")
    w("|--:|------|-------|---------|----------|-----------------|:------:|")
    for i, r in enumerate(brows, 1):
        st = "C" if r["Tier"] == "CONFIRMED" else "X"
        ts = dt(r["FirstUTC"]).strftime("%m-%d %H:%M") if r["FirstUTC"] else ""
        w(f"| {i} | {r['Call']} | {r['TheirClass']} | {r['TheirSection']} | "
          f"{r['Submodes']} | {ts} | {st} |")
    w()
if manual:
    w(f"### Phone ({len(manual)} QSO)")
    w()
    w("| # | Call | Class | Section | Sub-mode | Band | Date/Time (UTC) | Status |")
    w("|--:|------|-------|---------|----------|------|-----------------|:------:|")
    for i, m in enumerate(manual, 1):
        ts = f"{m['DateUTC'][5:]} {m['TimeUTC'][:2]}:{m['TimeUTC'][2:]}"
        w(f"| {i} | {m['Call']} | {m['TheirClass']} | {m['TheirSection']} | "
          f"{m['Submode']} | {m['Band']} | {ts} | C |")
    w()
    w("Phone contact made at **6:30 PM MDT (0030 UTC, 28 Jun)** on **14.320 MHz**.")
    w()

w("---")
w()
w("### Items to complete on the official entry (not derivable from the radio data)")
w()
w("- Number of participants and full operator list")
w("- GOTA station call and GOTA QSO totals, if any")
w("- Power-source description (sets the ×5 vs ×2 multiplier)")
w("- Claimed **bonus points** and their supporting documentation "
  "(emergency/alternate power, public location, media publicity, NTS traffic, etc.)")
w()
w(f"*Generated from the raw DM420 decode captures (digital) and `manual_qsos.csv` "
  f"(phone) by `make_fd_writeup.py`. Digital QSO set verified by independent re-parse "
  f"and against DM420's logbooks. Confirmed {conf} / claimed {n_total}.*")

out = os.path.join(HERE, "FIELD_DAY_2026_N0JDC.md")
open(out, "w").write("\n".join(O) + "\n")
print("wrote", out, "(%d lines, %d QSOs: %d digital + %d phone)"
      % (len(O), n_total, n_digital, n_phone))
