# ARRL Field Day 2026 — N0JDC

**Station:** N0JDC &nbsp;•&nbsp; **Class:** 2B &nbsp;•&nbsp; **Section:** CO (Colorado) &nbsp;•&nbsp; **Mode:** Digital (FT8 / FT4) &nbsp;•&nbsp; **Power:** QRP (≈5 W)

| | |
|---|---|
| Operating period | 2026-06-27 19:14Z → 2026-06-28 19:18Z |
| Valid QSOs (claimed) | **438** (381 confirmed + 57 exchange-complete) |
| Bands worked | 40 m, 20 m, 15 m, 10 m |
| ARRL/RAC sections worked | 73 |
| Software | DM420 ("Dingus Mangler 420") |

---

## 1. The Event

N0JDC operated **ARRL Field Day 2026** as a **Class 2B** entry from Colorado (section **CO**), running QRP (about 5 watts) on the digital modes **FT8** and **FT4**. The station was a two-person, two-transmitter portable operation:

- **Josh Carter — N0JDC** (the submitted station call)
- **Joel Odom — W4LL**

Both operators ran the project's own software, **DM420**, with the two transmitters active simultaneously on different bands — the multi-transmitter, multi-band style Field Day was built for. Because both stations transmitted under the single call **N0JDC**, the two operators' captures were merged into one station log and de-duplicated per the once-per-band-per-mode rule.

Field Day 2026 ran **1800 UTC Saturday 27 June → 2059 UTC Sunday 28 June**. Setup began at 1800 UTC Saturday, which entitled the station to operate the full period (through ~2100 UTC Sunday). Setup and antenna work filled the first hour or so; the **first contact was logged at 1914Z on 27 June**, and the **last at 1918Z on 28 June**.

Operation was essentially **around the clock**. With FT8/FT4 holding up well on 20 m and 40 m overnight, the station stayed active through every hour of the night rather than going dark — the hourly QSO counts below never fall to zero between the first and last contact.

### Results summary

| Band | Mode | QSOs | Confirmed | Exchange-complete |
|------|------|-----:|----------:|------------------:|
| 40m | Digital | 99 | 86 | 13 |
| 20m | Digital | 221 | 192 | 29 |
| 15m | Digital | 96 | 83 | 13 |
| 10m | Digital | 22 | 20 | 2 |
| **Total** | | **438** | **381** | **57** |

- **Mode split:** 194 QSOs FT8-only, 227 FT4-only, 17 on both sub-modes. All count as a single **Digital** mode for Field Day scoring and duplicate checking.
- **Geographic reach:** **73 distinct ARRL/RAC sections**, including Canadian sections and a handful of DX. Top sections: WWA (20), OH (20), WI (19), MI (15), STX (14), SV (13), ID (13), SCV (13).

**Claimed QSO points.** Digital contacts are worth **2 points** each: 438 × 2 = **876 QSO points** before the power multiplier. Operating QRP at ≤5 W from a non-commercial power source earns a **×5** power multiplier, giving **4380 claimed QSO points** (or 1752 at ×2 if the ≤5 W / power-source conditions are not met). Bonus points (emergency power, public location, publicity, etc.) are **not** included here and are added separately on the entry form.

> **Two figures are given throughout.** *Confirmed* QSOs (381) are those with explicit two-way confirmation. *Exchange-complete* QSOs (57) are ones where the full Field Day exchange went both ways but the final `RR73` was never decoded; they are counted because there is good reason to believe the contact completed (see §2). The conservative claim is **381**; the full claim is **438**.

### Activity by hour (UTC)

```
06-27 19Z    7  ███████
06-27 20Z   18  █████████████████
06-27 21Z    9  ████████
06-27 22Z   12  ███████████
06-27 23Z   14  █████████████
06-28 00Z   19  ██████████████████
06-28 01Z   26  ████████████████████████
06-28 02Z   26  ████████████████████████
06-28 03Z   22  █████████████████████
06-28 04Z   30  ████████████████████████████
06-28 05Z   17  ████████████████
06-28 06Z   21  ████████████████████
06-28 07Z   20  ███████████████████
06-28 08Z   22  █████████████████████
06-28 09Z   23  █████████████████████
06-28 10Z   23  █████████████████████
06-28 11Z   18  █████████████████
06-28 12Z   23  █████████████████████
06-28 13Z    7  ███████
06-28 14Z   14  █████████████
06-28 15Z   17  ████████████████
06-28 16Z   18  █████████████████
06-28 17Z   11  ██████████
06-28 18Z   18  █████████████████
06-28 19Z    3  ███
```

---

## 2. How the Results Were Calculated

DM420's live logger only commits a contact when it **decodes an `RR73`/`RRR`** (or, on the answering side, when it sends its own `RR73`). That is conservative and, under a Field Day pileup, it both **missed** good contacts and could not see contacts whose final acknowledgement was lost. So rather than trust the live log, the results above were rebuilt **from scratch out of the raw decode captures** that DM420 recorded for every cycle it heard or transmitted.

### Source data

- `decodes-joel.json` — every decode and transmission captured by Joel's station.
- `decodes-josh.jsonl` — the same from Josh's station.

Each line is one FT8/FT4 cycle: who/what was heard or sent, the band, the UTC slot time, and the decoded message (CQ, grid, signal report, or the **Field Day class+section exchange**). Both files also contain a week of pre-Field-Day testing, which is excluded.

### What counts as a valid Field Day QSO

A contact is credited only when **all** of the following hold:

1. **Inside the contest window** — the on-air slot time is between 1800 UTC 27 June and 2100 UTC 28 June. (Pre-FD test traffic and the operators' personal-call testing are dropped.)
2. **On a Field-Day-legal band** — 160/80/40/20/15/10 m. WARC bands (30/17/12 m) are not allowed for Field Day and are excluded.
3. **The exchange went both ways** — we received the other station's class+section **and** we sent ours. Field Day requires the class+section exchange, so a bare signal report or an unanswered CQ is not a QSO.

Each contact is then placed in one of two confidence tiers:

- **CONFIRMED** — we copied their class+section **and** have explicit proof they copied ours: either they sent us `RR73`/`RRR`/`73`, or they sent `N0JDC <CALL> R <class> <section>` (the leading `R` rogers our exchange). This is a textbook-complete QSO by the IARU definition (both stations identified, exchanged, and confirmed).
- **EXCHANGE-COMPLETE** — we copied their class+section and we transmitted our **rogered** exchange (`<CALL> N0JDC R 2B CO`) to them, but never decoded their final `RR73`. The exchange demonstrably went both ways; only the closing handshake is missing.

**Why count the exchange-complete tier?** ARRL Field Day is a *communications exercise, not an adjudicated contest* — logs are not cross-checked and there is no "not-in-log" penalty. When we have copied a station's full exchange and sent our own rogered exchange, the most likely reason we didn't see their `RR73` is simple fading on that one transmit cycle — in which case they logged us and the QSO is real. The guardrail is that **our** rogered exchange must have actually gone out; a contact where we only heard them, or only half-completed, stays uncredited.

### Two transmitters, one station

Because both operators transmitted as N0JDC, the two captures were **merged** and contacts de-duplicated to **once per band per mode** — and for Field Day, FT8 and FT4 are the *same* "Digital" mode, so a station worked on 20 m FT8 and again on 20 m FT4 is a single credit. A nice side effect of two co-located receivers is that each often copied the other's QSOs, making the reconstruction robust to gaps in either station's own transmit log.

### Verification (three independent checks)

1. **Independent re-parse.** The QSO set was rebuilt a second time from the literal on-air **text strings** using a completely separate parser, instead of DM420's structured decode fields. The two methods agreed on **every one of the 438 contacts** — exact match, no differences.
2. **Against DM420's own logbook.** The live logbook (which logs only on `RR73`) contains **372** de-duplicated QSOs. **Every one** appears in the reconstruction, with a **matching received class+section** (372/372, zero mismatches). The logbook contradicts the reconstruction nowhere.
3. **Explaining the difference.** The reconstruction has 66 contacts the logbook lacks: **57** are the exchange-complete (lost-`RR73`) tier the logger cannot see by design, and **9** are *fully confirmed* QSOs (with `RR73`/`RRR` in the decodes) that the live logger **dropped under pileup load**. The reconstruction also removes ~33 duplicate entries that the raw logbook had recorded. Net: the decode-based count is **more** complete and **more** accurate than the live log, and the live log corroborates all of it.

### Reproducing this

| Script | Purpose |
|--------|---------|
| `fd_report.py` | Parse both captures → QSO totals + `fd_worked_stations.csv` |
| `fd_crosscheck.py` | Independent raw-text re-parse; confirms the same 438 QSOs |
| `fd_logbook_check.py` | Compare against DM420's logbooks (`fd_logbook_comparison.txt`) |
| `make_fd_writeup.py` | Generate this document from the verified CSV |

---

## 3. Stations Worked — Dupe Sheet / Backing Log

All **438** creditable contacts, grouped by band and listed alphabetically by call for duplicate checking. *Time* is the UTC time of the first exchange of the contact. *Status* is C = confirmed, X = exchange-complete (see §2). The received exchange (their class + section) is shown as logged.

### 40m — Digital (99 QSOs)

| # | Call | Class | Section | Sub-mode | Date/Time (UTC) | Status |
|--:|------|-------|---------|----------|-----------------|:------:|
| 1 | AB7HP | 5A | ID | FT4 | 06-28 05:44 | C |
| 2 | AE5FM | 1D | NTX | FT4 | 06-28 11:26 | C |
| 3 | AG7T | 1E | WWA | FT4 | 06-28 12:35 | C |
| 4 | AI5DE | 1B | NM | FT4+FT8 | 06-28 05:56 | C |
| 5 | AK9D | 2D | KS | FT4 | 06-28 02:02 | C |
| 6 | K0A | 3A | MO | FT4 | 06-28 07:54 | C |
| 7 | K0EG | 2D | MO | FT8 | 06-28 11:13 | X |
| 8 | K0FJ | 1E | KS | FT4 | 06-28 01:57 | C |
| 9 | K0QIK | 2A | MN | FT4 | 06-28 06:07 | C |
| 10 | K0SW | 4A | NE | FT4 | 06-28 13:32 | C |
| 11 | K3NT | 1D | NTX | FT4 | 06-28 08:04 | C |
| 12 | K5CM | 2A | OK | FT8 | 06-28 06:34 | C |
| 13 | K5FD | 2A | WTX | FT8 | 06-28 10:30 | X |
| 14 | K5KDE | 1D | NTX | FT4 | 06-28 06:20 | C |
| 15 | K5N | 2A | STX | FT4 | 06-28 13:41 | C |
| 16 | K6PV | 3A | LAX | FT4 | 06-28 08:46 | C |
| 17 | K6SIS | 2A | SV | FT8 | 06-28 11:10 | C |
| 18 | K7EFA | 3A | MT | FT4 | 06-28 12:29 | C |
| 19 | K7JEP | 4A | ID | FT8 | 06-28 12:10 | C |
| 20 | K7SDX | 3A | ID | FT8 | 06-28 10:32 | C |
| 21 | K7T | 2D | AZ | FT8 | 06-28 06:31 | C |
| 22 | K7UVA | 3A | UT | FT4+FT8 | 06-28 09:08 | C |
| 23 | K7ZVX | 1E | ID | FT8 | 06-28 05:32 | C |
| 24 | K8DAA | 3A | MI | FT4 | 06-28 04:32 | C |
| 25 | K9LRD | 2A | WI | FT4 | 06-28 11:27 | X |
| 26 | K9OM | 1E | WI | FT4 | 06-28 04:14 | C |
| 27 | KB3Z | 1D | EPA | FT8 | 06-28 10:13 | C |
| 28 | KE0FOE | 1E | MO | FT4+FT8 | 06-28 06:48 | C |
| 29 | KE0PDU | 1E | CO | FT4 | 06-28 04:19 | C |
| 30 | KE0YBL | 1D | MN | FT4 | 06-28 07:15 | C |
| 31 | KF7JRD | 1E | UT | FT4 | 06-28 04:13 | C |
| 32 | KI6HFP | 1E | SV | FT8 | 06-28 10:23 | C |
| 33 | KM6FEP | 2D | ID | FT4 | 06-28 08:07 | C |
| 34 | KO6KWT | 1E | SV | FT4 | 06-28 10:46 | C |
| 35 | N0KK | 1D | MN | FT8 | 06-28 11:37 | C |
| 36 | N1SLO | 1D | OR | FT8 | 06-28 10:12 | C |
| 37 | N1VF | 1E | SCV | FT8 | 06-28 09:15 | C |
| 38 | N2WK | 1D | WNY | FT8 | 06-28 09:58 | C |
| 39 | N3CKF | 1B | SV | FT8 | 06-28 12:14 | C |
| 40 | N5SLY | 1D | NTX | FT4 | 06-28 11:33 | C |
| 41 | N6P | 4A | LAX | FT4 | 06-28 13:27 | C |
| 42 | N6YG | 1E | SV | FT8 | 06-28 15:01 | X |
| 43 | N7BOI | 3F | ID | FT4 | 06-28 12:58 | C |
| 44 | N7DNF | 2D | MT | FT4 | 06-28 04:25 | C |
| 45 | N7RO | 1D | WWA | FT4 | 06-28 05:41 | C |
| 46 | N7WAH | 1A | WWA | FT4 | 06-28 08:47 | C |
| 47 | N7XCZ | 1E | NV | FT8 | 06-28 06:38 | C |
| 48 | N9TAE | 1E | IN | FT4 | 06-28 06:44 | C |
| 49 | NA5D | 1D | STX | FT4 | 06-28 12:35 | X |
| 50 | NM5HR | 1A | NM | FT4 | 06-28 05:26 | C |
| 51 | NN0Y | 1B | CO | FT8 | 06-28 14:59 | C |
| 52 | NU0Y | 2D | KS | FT4 | 06-28 06:46 | X |
| 53 | NW5X | 1D | NTX | FT4 | 06-28 04:26 | C |
| 54 | VE3SPR | 3A | ONS | FT4 | 06-28 08:29 | C |
| 55 | VE7SCC | 3A | BC | FT8 | 06-28 13:05 | C |
| 56 | W0BU | 3A | MN | FT8 | 06-28 09:41 | X |
| 57 | W0CXX | 3A | IA | FT8 | 06-28 14:14 | C |
| 58 | W0LV | 1D | NV | FT8 | 06-28 10:19 | C |
| 59 | W0OJY | 3A | SD | FT4 | 06-28 12:27 | C |
| 60 | W0WTN | 9A | SD | FT4 | 06-28 03:37 | C |
| 61 | W0ZA | 1D | NE | FT4 | 06-28 04:29 | C |
| 62 | W1EE | 2A | CT | FT4 | 06-28 06:24 | C |
| 63 | W3AO | 9A | MDC | FT8 | 06-28 10:06 | C |
| 64 | W3SMR | 1A | MDC | FT4 | 06-28 09:00 | X |
| 65 | W4A | 4A | AL | FT8 | 06-28 09:25 | C |
| 66 | W4BFB | 2A | NC | FT8 | 06-28 10:00 | C |
| 67 | W4DW | 5A | NC | FT4 | 06-28 08:34 | X |
| 68 | W4GR | 8A | GA | FT4 | 06-28 08:17 | C |
| 69 | W4JNB | 3A | AL | FT4 | 06-28 05:52 | C |
| 70 | W4JNG | 1D | NC | FT8 | 06-28 09:24 | C |
| 71 | W4PTY | 1D | NTX | FT4 | 06-28 06:13 | C |
| 72 | W5DAH | 1D | OK | FT4 | 06-28 04:17 | C |
| 73 | W5LCR | 2A | STX | FT4 | 06-28 09:04 | C |
| 74 | W6SCF | 2A | SCV | FT8 | 06-28 09:41 | X |
| 75 | W6SJC | 4F | SCV | FT4 | 06-28 07:10 | C |
| 76 | W6WPT | 2A | SJV | FT8 | 06-28 09:21 | C |
| 77 | W7AVM | 4A | WWA | FT8 | 06-28 10:10 | C |
| 78 | W7BAR | 3A | UT | FT4 | 06-28 14:03 | C |
| 79 | W7CT | 1E | UT | FT4 | 06-28 02:01 | C |
| 80 | W7DK | 4A | WWA | FT8 | 06-28 02:07 | C |
| 81 | W7GRA | 4A | OR | FT8 | 06-28 09:12 | C |
| 82 | W7JWT | 2B | EWA | FT4+FT8 | 06-28 04:27 | C |
| 83 | W7OTV | 4A | OR | FT8 | 06-28 10:41 | C |
| 84 | W7PIG | 3A | WWA | FT4 | 06-28 08:41 | C |
| 85 | W7RCH | 1F | UT | FT8 | 06-28 15:01 | X |
| 86 | W7VOI | 2A | ID | FT4 | 06-28 04:18 | C |
| 87 | W7VW | 6A | OR | FT8 | 06-28 10:02 | C |
| 88 | W8BM | 5A | OH | FT4 | 06-28 08:54 | C |
| 89 | W8RP | 5A | MI | FT4 | 06-28 08:48 | C |
| 90 | W8VTD | 8A | OH | FT4 | 06-28 08:34 | X |
| 91 | W8XRN | 3F | OH | FT4 | 06-28 05:49 | C |
| 92 | W8XX | 1A | OH | FT4 | 06-28 10:53 | C |
| 93 | W9TAL | 2A | IL | FT4 | 06-28 11:21 | C |
| 94 | W9UP | 6A | WI | FT4 | 06-28 04:36 | C |
| 95 | WA7DHQ | 1D | AZ | FT8 | 06-28 14:48 | C |
| 96 | WB1BWQ | 1E | WTX | FT8 | 06-28 10:27 | C |
| 97 | WD9HSY | 1E | IL | FT4 | 06-28 10:56 | X |
| 98 | WR5E | 1B | CO | FT8 | 06-28 14:52 | C |
| 99 | WY7HR | 6A | WY | FT4 | 06-28 04:23 | C |

### 20m — Digital (221 QSOs)

| # | Call | Class | Section | Sub-mode | Date/Time (UTC) | Status |
|--:|------|-------|---------|----------|-----------------|:------:|
| 1 | AA5FA | 1D | TN | FT4 | 06-28 19:18 | C |
| 2 | AA7MC | 1E | AZ | FT8 | 06-28 00:22 | X |
| 3 | AC8RC | 5A | MI | FT8 | 06-28 15:18 | C |
| 4 | AC9VM | 1E | IN | FT8 | 06-28 17:57 | X |
| 5 | AE8MM | 1D | OH | FT4 | 06-27 23:09 | C |
| 6 | AG7T | 1E | WWA | FT4 | 06-28 16:49 | C |
| 7 | AI3Q | 1D | EPA | FT4 | 06-28 11:32 | C |
| 8 | AJ7GF | 1A | UT | FT4 | 06-28 15:56 | C |
| 9 | AK4NF | 1B | AL | FT8 | 06-28 14:54 | C |
| 10 | G3UAS | 1D | DX | FT4 | 06-28 07:48 | C |
| 11 | K0FV | 2F | MO | FT4 | 06-28 15:00 | X |
| 12 | K0KKV | 3A | NE | FT4 | 06-28 16:38 | C |
| 13 | K0QIK | 2A | MN | FT8 | 06-27 23:51 | C |
| 14 | K1B | 5E | NH | FT8 | 06-28 09:59 | C |
| 15 | K1NKT | 1D | VT | FT4 | 06-28 09:52 | C |
| 16 | K2DSW | 1D | IA | FT4 | 06-28 18:23 | C |
| 17 | K2IQ | 3A | WNY | FT4 | 06-28 11:49 | X |
| 18 | K3BWA | 4A | MDC | FT8 | 06-28 06:14 | C |
| 19 | K3CAL | 5A | MDC | FT8 | 06-28 10:01 | C |
| 20 | K3KEK | 1E | STX | FT4 | 06-28 03:38 | C |
| 21 | K3Q | 4A | EPA | FT8 | 06-28 09:39 | C |
| 22 | K4B | 5A | GA | FT8 | 06-28 05:56 | C |
| 23 | K4DND | 1D | VA | FT4 | 06-28 12:16 | C |
| 24 | K4FC | 7A | NFL | FT8 | 06-28 06:18 | C |
| 25 | K4LRG | 3A | VA | FT8 | 06-28 01:49 | C |
| 26 | K4OCE | 3D | STX | FT4 | 06-28 03:15 | C |
| 27 | K4PAR | 7F | GA | FT4 | 06-27 23:11 | C |
| 28 | K4RC | 3A | VA | FT8 | 06-28 09:29 | C |
| 29 | K4RFT | 2A | TN | FT8 | 06-28 06:47 | C |
| 30 | K4SEX | 2A | GA | FT8 | 06-28 12:56 | C |
| 31 | K4YT | 1D | VA | FT8 | 06-28 12:10 | C |
| 32 | K5AHL | 1D | STX | FT4 | 06-28 03:44 | X |
| 33 | K5GVL | 5A | NTX | FT8 | 06-28 07:31 | C |
| 34 | K5OWO | 1B | STX | FT4 | 06-28 02:53 | C |
| 35 | K5XU | 3F | MS | FT4 | 06-28 03:11 | C |
| 36 | K6EAG | 3A | EB | FT8 | 06-28 17:42 | C |
| 37 | K6MMM | 3A | SCV | FT4 | 06-28 04:43 | C |
| 38 | K6RO | 1D | ME | FT4 | 06-28 11:19 | C |
| 39 | K6SON | 3A | SF | FT4 | 06-28 03:02 | C |
| 40 | K6T | 4A | SV | FT4+FT8 | 06-28 00:34 | C |
| 41 | K6TQ | 1D | SV | FT4 | 06-28 18:47 | C |
| 42 | K7FD | 1B | OR | FT4 | 06-28 03:29 | C |
| 43 | K7NWF | 1D | AZ | FT8 | 06-28 15:40 | C |
| 44 | K7RE | 1B | NM | FT8 | 06-28 00:30 | X |
| 45 | K7SWI | 4A | ID | FT8 | 06-28 06:10 | X |
| 46 | K8BSR | 10A | OH | FT4+FT8 | 06-28 08:40 | C |
| 47 | K8UU | 3A | OH | FT8 | 06-27 23:54 | C |
| 48 | K9E | 3A | IL | FT8 | 06-28 16:08 | C |
| 49 | K9OM | 1E | WI | FT4 | 06-28 01:17 | X |
| 50 | K9T | 3A | IL | FT8 | 06-28 02:05 | C |
| 51 | KA4J | 6E | TN | FT8 | 06-27 23:56 | C |
| 52 | KA4ZZZ | 2E | GA | FT4 | 06-28 11:54 | C |
| 53 | KA5JTM | 1E | STX | FT4 | 06-28 16:49 | C |
| 54 | KB7RUQ | 1D | UT | FT4 | 06-28 18:57 | C |
| 55 | KB9HGI | 1D | IL | FT4 | 06-28 03:32 | C |
| 56 | KB9LCD | 1D | IL | FT4 | 06-28 03:13 | C |
| 57 | KC3RHQ | 1D | WPA | FT4 | 06-28 05:11 | C |
| 58 | KC4SCO | 2A | NC | FT4 | 06-28 12:32 | C |
| 59 | KC8VC | 3A | MI | FT8 | 06-28 01:42 | C |
| 60 | KC8ZPI | 1D | MI | FT8 | 06-28 02:30 | C |
| 61 | KC9NJZ | 1B | IL | FT8 | 06-28 02:17 | C |
| 62 | KC9UJP | 1D | IN | FT4 | 06-28 01:07 | C |
| 63 | KD0XD | 1D | IA | FT8 | 06-28 16:19 | C |
| 64 | KE0FOE | 1E | MO | FT4 | 06-28 01:12 | X |
| 65 | KE4WLE | 1D | OH | FT4 | 06-28 16:01 | C |
| 66 | KE5HP | 4A | STX | FT8 | 06-28 02:09 | C |
| 67 | KE5ZDZ | 1D | STX | FT8 | 06-28 00:23 | C |
| 68 | KE8RV | 3A | OH | FT4 | 06-28 16:35 | C |
| 69 | KF0NKS | 1D | NE | FT4 | 06-27 20:04 | C |
| 70 | KF7GRP | 1D | NV | FT4 | 06-27 20:06 | C |
| 71 | KF8EFV | 1D | MI | FT4 | 06-27 21:08 | X |
| 72 | KG1AES | 4A | WCF | FT8 | 06-28 09:17 | C |
| 73 | KG4IXS | 1E | VA | FT8 | 06-28 08:32 | C |
| 74 | KH2SR | 1E | SJV | FT4 | 06-28 04:42 | C |
| 75 | KH6COM | 2A | PAC | FT4 | 06-28 09:54 | C |
| 76 | KI5WES | 1B | LA | FT4+FT8 | 06-27 20:14 | C |
| 77 | KI6BTY | 5D | SB | FT8 | 06-28 00:38 | X |
| 78 | KI7OIY | 1D | ID | FT8 | 06-28 00:08 | C |
| 79 | KJ5LNI | 1D | AR | FT8 | 06-28 02:33 | C |
| 80 | KJ6ART | 2B | SJV | FT4 | 06-28 05:03 | X |
| 81 | KJ7MMU | 1D | WWA | FT8 | 06-28 04:10 | C |
| 82 | KK4IRV | 1D | NC | FT8 | 06-28 05:42 | X |
| 83 | KK4RXE | 1E | TN | FT8 | 06-28 17:00 | C |
| 84 | KM4LFT | 3E | GA | FT4 | 06-28 11:52 | C |
| 85 | KM4VIQ | 3A | NC | FT8 | 06-28 12:40 | C |
| 86 | KM7GQZ | 1A | NV | FT4 | 06-27 21:04 | C |
| 87 | KP3RE | 2F | PR | FT4 | 06-28 04:58 | C |
| 88 | KR4CCY | 1E | NC | FT8 | 06-28 08:19 | C |
| 89 | KT5TX | 6A | STX | FT8 | 06-28 01:33 | C |
| 90 | KU4UK | 1D | AL | FT8 | 06-28 12:41 | C |
| 91 | KX4I | 1E | AL | FT4 | 06-27 21:06 | C |
| 92 | KZ8Z | 1E | MI | FT4 | 06-28 18:10 | C |
| 93 | N0GF | 3A | ND | FT8 | 06-28 16:24 | C |
| 94 | N0GJ | 2A | KS | FT8 | 06-28 01:33 | X |
| 95 | N1A | 2A | WMA | FT4 | 06-28 04:41 | C |
| 96 | N1API | 1D | CT | FT8 | 06-28 04:23 | C |
| 97 | N1ERC | 4A | EMA | FT4+FT8 | 06-28 03:07 | C |
| 98 | N1KT | 1D | CT | FT4 | 06-28 10:45 | C |
| 99 | N2A | 2B | SNJ | FT4 | 06-28 11:36 | C |
| 100 | N2OB | 3A | SNJ | FT4 | 06-28 03:01 | C |
| 101 | N2SEC | 2C | WNY | FT4 | 06-28 11:53 | C |
| 102 | N3YPJ | 3A | MDC | FT8 | 06-28 04:35 | X |
| 103 | N4HCA | 1F | WCF | FT8 | 06-28 06:42 | C |
| 104 | N4KGL | 2B | AL | FT4 | 06-28 11:49 | C |
| 105 | N4RNJ | 1B | NFL | FT8 | 06-28 06:51 | C |
| 106 | N4SRC | 4A | WCF | FT8 | 06-28 08:08 | C |
| 107 | N4WMW | 1C | VA | FT4 | 06-28 07:43 | C |
| 108 | N5BL | 4A | NM | FT8 | 06-28 02:27 | C |
| 109 | N5LBJ | 1D | MDC | FT4 | 06-28 07:20 | C |
| 110 | N7M | 4A | AZ | FT8 | 06-28 07:54 | X |
| 111 | N8A | 1E | OH | FT4 | 06-28 18:38 | C |
| 112 | N8NCR | 2E | WPA | FT8 | 06-28 07:35 | C |
| 113 | N8PFK | 1E | MI | FT8 | 06-28 12:53 | C |
| 114 | N8QA | 6A | OH | FT4 | 06-28 12:22 | C |
| 115 | N8XY | 2A | OH | FT4 | 06-28 05:04 | C |
| 116 | N9GMT | 9E | WI | FT4 | 06-27 23:14 | C |
| 117 | N9POL | 1A | IL | FT4+FT8 | 06-28 05:00 | X |
| 118 | NA5D | 1D | STX | FT4 | 06-28 16:45 | X |
| 119 | NA7SS | 4D | WWA | FT4 | 06-28 14:23 | C |
| 120 | NG7X | 1D | AZ | FT4 | 06-28 19:06 | C |
| 121 | NI7C | 3D | AZ | FT8 | 06-28 15:42 | C |
| 122 | NJ8G | 1D | AZ | FT4 | 06-28 15:46 | C |
| 123 | VA3DO | 5A | ONE | FT4 | 06-28 10:43 | C |
| 124 | VA3THP | 1D | ONE | FT8 | 06-28 06:36 | C |
| 125 | VA6CR | 1D | AB | FT4 | 06-28 03:44 | C |
| 126 | VE2CRO | 4A | QC | FT8 | 06-28 06:39 | C |
| 127 | VE2UMS | 2A | QC | FT4 | 06-28 13:29 | C |
| 128 | VE3GCB | 3A | ONS | FT8 | 06-28 12:54 | C |
| 129 | VE5AA | 2A | SK | FT4 | 06-28 10:21 | C |
| 130 | VE6AX | 1D | AB | FT8 | 06-28 04:00 | C |
| 131 | VE7MIS | 3A | BC | FT8 | 06-28 04:26 | C |
| 132 | VE7XD | 7A | BC | FT8 | 06-28 02:22 | C |
| 133 | W0CET | 3A | KS | FT8 | 06-28 07:04 | C |
| 134 | W0CWP | 4A | IA | FT8 | 06-28 09:26 | C |
| 135 | W0DK | 2A | CO | FT8 | 06-28 09:18 | C |
| 136 | W0IND | 3A | IA | FT4 | 06-28 07:20 | C |
| 137 | W0OFK | 4A | NE | FT8 | 06-28 00:19 | X |
| 138 | W0SJE | 1D | CO | FT4 | 06-28 10:18 | C |
| 139 | W0WH | 2B | NE | FT8 | 06-28 17:48 | C |
| 140 | W0ZRT | 2A | ND | FT8 | 06-27 20:28 | C |
| 141 | W1BRS | 6F | CT | FT8 | 06-28 08:13 | C |
| 142 | W1EE | 2A | CT | FT4 | 06-28 09:50 | C |
| 143 | W1M | 3A | ME | FT4 | 06-28 07:10 | C |
| 144 | W1PB | 2A | WCF | FT8 | 06-28 04:38 | X |
| 145 | W1TU | 3A | ME | FT4 | 06-28 10:07 | C |
| 146 | W1WT | 1D | OH | FT4 | 06-28 18:53 | C |
| 147 | W2GSA | 3A | NNJ | FT8 | 06-28 10:05 | C |
| 148 | W2MMD | 9A | SNJ | FT4 | 06-28 07:40 | C |
| 149 | W2RDX | 4A | WNY | FT4 | 06-28 03:26 | C |
| 150 | W2SO | 5A | WNY | FT4 | 06-28 11:03 | X |
| 151 | W2XRX | 3A | WNY | FT4 | 06-28 11:49 | C |
| 152 | W3AO | 9A | MDC | FT8 | 06-28 07:42 | C |
| 153 | W3BN | 2A | EPA | FT8 | 06-28 08:26 | C |
| 154 | W3MIE | 2A | WPA | FT4 | 06-28 14:30 | C |
| 155 | W3RRR | 4A | EPA | FT4 | 06-28 11:06 | C |
| 156 | W3T | 3A | WPA | FT8 | 06-28 06:31 | C |
| 157 | W3VPJ | 3A | EPA | FT8 | 06-28 08:21 | C |
| 158 | W3VPR | 3A | MDC | FT8 | 06-28 07:33 | C |
| 159 | W4BRY | 2D | NC | FT4 | 06-28 18:26 | C |
| 160 | W4CRS | 1D | AL | FT4 | 06-28 12:34 | X |
| 161 | W4DO | 3A | VA | FT8 | 06-28 09:06 | C |
| 162 | W4DW | 5A | NC | FT8 | 06-28 09:08 | C |
| 163 | W4F | 2A | SFL | FT4 | 06-28 18:34 | C |
| 164 | W4GL | 2A | SC | FT4 | 06-28 01:04 | C |
| 165 | W4HOG | 3D | NC | FT4 | 06-28 07:22 | C |
| 166 | W4IY | 9A | VA | FT8 | 06-28 08:42 | X |
| 167 | W4JNG | 1D | NC | FT8 | 06-28 05:18 | C |
| 168 | W4MLB | 4A | SFL | FT8 | 06-28 08:51 | C |
| 169 | W4MOE | 5A | NC | FT8 | 06-28 12:08 | C |
| 170 | W4MT | 2A | VA | FT8 | 06-28 08:34 | C |
| 171 | W4R | 6A | GA | FT8 | 06-28 09:21 | C |
| 172 | W4SHL | 3A | AL | FT8 | 06-28 01:36 | X |
| 173 | W5BMC | 3A | LA | FT8 | 06-28 14:44 | C |
| 174 | W5NAC | 3F | NTX | FT4 | 06-28 00:51 | C |
| 175 | W5SAF | 2A | NM | FT8 | 06-28 00:28 | C |
| 176 | W6ARA | 3A | SCV | FT4 | 06-28 03:39 | X |
| 177 | W6B | 2A | SCV | FT4 | 06-28 02:55 | C |
| 178 | W6DPM | 1D | LAX | FT4 | 06-27 20:44 | C |
| 179 | W6EK | 4A | SV | FT8 | 06-28 07:47 | C |
| 180 | W6MRR | 1A | EB | FT4 | 06-28 03:07 | X |
| 181 | W6PAN | 1D | SCV | FT4 | 06-28 18:06 | C |
| 182 | W6SX | 1D | SJV | FT4 | 06-28 18:11 | C |
| 183 | W6TRW | 5A | SJV | FT8 | 06-28 04:32 | C |
| 184 | W6WU | 2B | MDC | FT8 | 06-28 06:36 | C |
| 185 | W7DSW | 1B | UT | FT4 | 06-28 18:55 | C |
| 186 | W7IME | 2A | WWA | FT4+FT8 | 06-28 03:36 | C |
| 187 | W7JWT | 2B | EWA | FT8 | 06-28 13:07 | X |
| 188 | W7PIG | 3A | WWA | FT4 | 06-28 03:31 | C |
| 189 | W7Q | 4A | OR | FT8 | 06-28 01:54 | C |
| 190 | W7T | 1A | WWA | FT8 | 06-28 04:05 | C |
| 191 | W7VW | 6A | OR | FT8 | 06-28 07:01 | C |
| 192 | W8ACW | 3A | MI | FT4 | 06-28 14:25 | C |
| 193 | W8AL | 2A | OH | FT4 | 06-27 20:13 | C |
| 194 | W8ISS | 1C | MI | FT4 | 06-28 12:16 | C |
| 195 | W8NL | 3A | OH | FT8 | 06-28 14:12 | C |
| 196 | W8PAR | 3A | WV | FT8 | 06-28 05:30 | C |
| 197 | W8QLY | 8A | OH | FT8 | 06-28 15:34 | C |
| 198 | W8RP | 5A | MI | FT4+FT8 | 06-28 07:24 | C |
| 199 | W8SGT | 2F | OH | FT8 | 06-28 12:45 | C |
| 200 | W8TNO | 3A | MI | FT4 | 06-28 15:49 | C |
| 201 | W8VA | 4A | WV | FT8 | 06-28 15:16 | C |
| 202 | W8XRN | 3F | OH | FT4 | 06-28 14:02 | C |
| 203 | W9CQ | 4A | WI | FT8 | 06-28 06:33 | C |
| 204 | W9FEZ | 2D | IN | FT8 | 06-28 12:47 | C |
| 205 | W9TCR | 1B | WCF | FT8 | 06-28 01:57 | X |
| 206 | WA2DQL | 2F | WNY | FT8 | 06-28 12:00 | C |
| 207 | WA7VC | 5F | WWA | FT4 | 06-28 04:55 | C |
| 208 | WB2BIN | 1D | WNY | FT4 | 06-28 10:58 | C |
| 209 | WB5DXG | 1D | MS | FT8 | 06-28 16:13 | C |
| 210 | WB8RMC | 1D | OH | FT4 | 06-28 12:34 | C |
| 211 | WC4NC | 2A | NC | FT4 | 06-28 04:54 | C |
| 212 | WD2K | 3A | ENY | FT8 | 06-28 16:08 | C |
| 213 | WD5TYL | 3A | STX | FT8 | 06-28 04:17 | C |
| 214 | WD9HSY | 1E | IL | FT8 | 06-28 08:53 | C |
| 215 | WJ0D | 1D | CO | FT8 | 06-27 20:53 | C |
| 216 | WM5T | 3A | LA | FT8 | 06-27 23:49 | C |
| 217 | WR2ABB | 3A | ENY | FT8 | 06-28 02:20 | X |
| 218 | WV8HAT | 1A | WV | FT8 | 06-28 14:41 | C |
| 219 | WX4BK | 3A | GA | FT4 | 06-28 13:29 | C |
| 220 | WX4E | 5A | WCF | FT4 | 06-28 07:14 | C |
| 221 | XE2SSB | 1A | DX | FT4 | 06-28 01:01 | C |

### 15m — Digital (96 QSOs)

| # | Call | Class | Section | Sub-mode | Date/Time (UTC) | Status |
|--:|------|-------|---------|----------|-----------------|:------:|
| 1 | AA4JS | 6A | SFL | FT4 | 06-28 02:21 | C |
| 2 | AB6MV | 2A | ORG | FT4 | 06-28 15:38 | C |
| 3 | AB9M | 1D | IL | FT4 | 06-27 22:33 | C |
| 4 | AG7LR | 1D | WWA | FT4 | 06-28 18:33 | C |
| 5 | AG7MI | 4A | OR | FT4 | 06-27 22:32 | C |
| 6 | AG7T | 1E | WWA | FT4 | 06-27 22:48 | C |
| 7 | K0HNY | 6A | MN | FT4 | 06-27 20:51 | C |
| 8 | K0QIK | 2A | MN | FT4 | 06-28 16:45 | C |
| 9 | K1CCN | 3A | WI | FT4 | 06-27 23:47 | C |
| 10 | K1WAS | 3D | CT | FT8 | 06-28 02:26 | C |
| 11 | K1WTF | 1D | CO | FT8 | 06-28 02:35 | C |
| 12 | K2BR | 4F | SNJ | FT4 | 06-28 02:17 | C |
| 13 | K4YT | 1D | VA | FT8 | 06-28 02:31 | X |
| 14 | K6AAR | 3A | SCV | FT8 | 06-27 23:33 | C |
| 15 | K6MI | 1A | SCV | FT4 | 06-27 21:46 | C |
| 16 | K6PV | 3A | LAX | FT4 | 06-28 03:45 | C |
| 17 | K6RO | 1D | ME | FT4 | 06-28 00:49 | C |
| 18 | K7BEL | 2F | WWA | FT4 | 06-28 15:09 | C |
| 19 | K7CA | 1E | NV | FT8 | 06-28 01:46 | C |
| 20 | K7JEP | 4A | ID | FT8 | 06-28 16:09 | X |
| 21 | K7T | 2D | AZ | FT4+FT8 | 06-28 18:06 | C |
| 22 | K7UKE | 1D | CO | FT4 | 06-28 01:13 | C |
| 23 | K8BSR | 10A | OH | FT4 | 06-27 21:47 | C |
| 24 | K8CAD | 3A | MI | FT8 | 06-27 21:09 | C |
| 25 | K8EP | 2A | WV | FT4 | 06-28 04:03 | X |
| 26 | K9LRD | 2A | WI | FT4 | 06-28 16:26 | C |
| 27 | K9OM | 1E | WI | FT4 | 06-27 20:54 | C |
| 28 | KA4J | 6E | TN | FT8 | 06-28 01:33 | C |
| 29 | KB4UF | 3D | NFL | FT8 | 06-28 00:36 | X |
| 30 | KC7CS | 1D | AZ | FT4 | 06-28 17:38 | C |
| 31 | KC8VC | 3A | MI | FT4+FT8 | 06-28 14:32 | C |
| 32 | KE6SHL | 1B | SJV | FT4+FT8 | 06-28 02:44 | C |
| 33 | KI6BTY | 5D | SB | FT8 | 06-28 18:13 | X |
| 34 | KI6HFP | 1E | SV | FT8 | 06-28 18:22 | C |
| 35 | KJ6ART | 2B | SJV | FT4 | 06-27 23:46 | C |
| 36 | KK6LSF | 2B | OR | FT4 | 06-27 22:44 | X |
| 37 | KQ7I | 1D | OR | FT8 | 06-27 21:50 | C |
| 38 | N0GF | 3A | ND | FT8 | 06-28 01:44 | C |
| 39 | N0MB | 1D | IA | FT8 | 06-28 00:09 | X |
| 40 | N2BEF | 1D | ENY | FT4 | 06-28 00:50 | C |
| 41 | N3SRC | 5A | EPA | FT4 | 06-28 03:47 | C |
| 42 | N4EH | 5A | NFL | FT4 | 06-28 02:17 | X |
| 43 | N6DLC | 1D | AZ | FT4 | 06-28 03:53 | X |
| 44 | N6SBC | 5A | SCV | FT4+FT8 | 06-27 21:52 | C |
| 45 | N6YG | 1E | SV | FT8 | 06-27 23:39 | C |
| 46 | N7DNF | 2D | MT | FT4 | 06-28 15:46 | C |
| 47 | N7LE | 3A | OR | FT4 | 06-28 01:28 | C |
| 48 | N7UVH | 1E | ID | FT4 | 06-27 20:51 | C |
| 49 | N8ERL | 1D | MI | FT4 | 06-28 01:08 | C |
| 50 | N9GMT | 9E | WI | FT4 | 06-27 22:13 | C |
| 51 | N9WH | 3A | IL | FT4 | 06-28 15:48 | C |
| 52 | NA7SS | 4D | WWA | FT4 | 06-27 22:29 | C |
| 53 | NY1U | 2E | CT | FT8 | 06-28 00:26 | C |
| 54 | NZ5T | 2A | NTX | FT4 | 06-27 22:40 | C |
| 55 | VE1FO | 3A | NS | FT4 | 06-27 22:45 | C |
| 56 | VE1LD | 3A | NS | FT4 | 06-28 02:41 | C |
| 57 | VE2CDX | 2A | QC | FT4 | 06-28 03:49 | C |
| 58 | W0GKP | 3A | MN | FT8 | 06-28 05:19 | C |
| 59 | W0RRC | 5A | MN | FT8 | 06-27 23:37 | C |
| 60 | W0WTN | 9A | SD | FT8 | 06-28 05:15 | C |
| 61 | W0YL | 3A | IA | FT8 | 06-28 16:18 | C |
| 62 | W1BPT | 2A | CT | FT4 | 06-28 02:16 | X |
| 63 | W1M | 3A | ME | FT4 | 06-28 01:07 | C |
| 64 | W1NPP | 3A | ME | FT4 | 06-28 04:01 | C |
| 65 | W2MMD | 9A | SNJ | FT4 | 06-28 00:49 | C |
| 66 | W2VL | 6A | NLI | FT4 | 06-28 01:21 | C |
| 67 | W3CWC | 3A | MDC | FT4 | 06-28 00:51 | C |
| 68 | W3OC | 4A | WPA | FT4 | 06-27 22:38 | X |
| 69 | W3RRR | 4A | EPA | FT4 | 06-28 01:19 | C |
| 70 | W4FPC | 3F | NFL | FT4 | 06-27 23:58 | C |
| 71 | W4GLO | 5A | WCF | FT8 | 06-28 00:14 | C |
| 72 | W4OVH | 3A | VA | FT8 | 06-28 02:38 | C |
| 73 | W4SPF | 2A | SFL | FT4 | 06-28 18:37 | C |
| 74 | W4TA | 3A | WCF | FT8 | 06-28 00:18 | C |
| 75 | W5FC | 3A | NTX | FT8 | 06-28 01:52 | C |
| 76 | W6ARA | 3A | SCV | FT4 | 06-28 01:06 | C |
| 77 | W6DPM | 1D | LAX | FT4 | 06-28 17:37 | C |
| 78 | W6ERE | 6A | SJV | FT4+FT8 | 06-27 21:19 | X |
| 79 | W6EU | 1D | SV | FT4 | 06-28 00:02 | C |
| 80 | W6F | 5A | SDG | FT4 | 06-28 04:06 | C |
| 81 | W6SJC | 4F | SCV | FT8 | 06-27 23:43 | C |
| 82 | W6SX | 1D | SJV | FT4 | 06-27 22:20 | C |
| 83 | W6YL | 4A | SCV | FT4 | 06-28 18:29 | C |
| 84 | W6ZE | 5A | ORG | FT4 | 06-28 01:15 | C |
| 85 | W7EAT | 3A | WWA | FT8 | 06-28 02:28 | C |
| 86 | W7PIG | 3A | WWA | FT4 | 06-28 03:56 | C |
| 87 | W9MQB | 2A | WI | FT4 | 06-28 15:35 | C |
| 88 | W9OU | 3A | IN | FT4 | 06-27 22:30 | C |
| 89 | W9VRC | 4A | WI | FT8 | 06-28 15:15 | C |
| 90 | WA7PVE | 1D | WWA | FT4 | 06-27 22:54 | C |
| 91 | WB2SIH | 1D | ENY | FT8 | 06-28 02:30 | C |
| 92 | WB7WUQ | 1E | EWA | FT8 | 06-28 19:08 | C |
| 93 | WF3H | 1D | STX | FT8 | 06-28 16:14 | C |
| 94 | WJ0D | 1D | CO | FT8 | 06-28 01:36 | X |
| 95 | WX2DX | 1D | WPA | FT8 | 06-28 02:22 | C |
| 96 | XE2SSB | 1A | DX | FT4 | 06-28 18:27 | C |

### 10m — Digital (22 QSOs)

| # | Call | Class | Section | Sub-mode | Date/Time (UTC) | Status |
|--:|------|-------|---------|----------|-----------------|:------:|
| 1 | K0A | 3A | MO | FT8 | 06-27 19:26 | C |
| 2 | K0K | 2A | KS | FT8 | 06-27 19:46 | C |
| 3 | K0RQ | 3A | ND | FT8 | 06-28 17:06 | C |
| 4 | K1CCN | 3A | WI | FT4 | 06-27 20:16 | C |
| 5 | K6AVP | 4A | SB | FT8 | 06-27 19:44 | C |
| 6 | K6T | 4A | SV | FT4+FT8 | 06-27 19:57 | C |
| 7 | K7JEP | 4A | ID | FT8 | 06-27 20:39 | C |
| 8 | K9BEL | 3A | WI | FT4 | 06-27 20:12 | C |
| 9 | K9LRD | 2A | WI | FT8 | 06-28 17:01 | C |
| 10 | KB9SCT | 1D | WI | FT4 | 06-27 20:17 | X |
| 11 | KD9ZCL | 1E | WI | FT4 | 06-28 16:50 | C |
| 12 | KF0OVL | 1D | CO | FT8 | 06-27 19:14 | C |
| 13 | N0VFJ | 1D | MN | FT8 | 06-27 20:35 | C |
| 14 | N7DB | 1D | OR | FT8 | 06-28 17:11 | C |
| 15 | N7FRO | 2B | ID | FT4 | 06-27 20:20 | C |
| 16 | N9GMT | 9E | WI | FT8 | 06-27 19:34 | C |
| 17 | W0FX | 3A | ND | FT8 | 06-28 16:57 | C |
| 18 | W0MR | 3A | MN | FT8 | 06-28 17:22 | C |
| 19 | W0RHX | 2A | MO | FT8 | 06-27 19:51 | X |
| 20 | W6EU | 1D | SV | FT4 | 06-27 20:09 | C |
| 21 | W9LRC | 4A | WI | FT8 | 06-28 17:04 | C |
| 22 | WJ0D | 1D | CO | FT8 | 06-27 20:00 | C |

---

### Items to complete on the official entry (not derivable from the radio data)

- Number of participants and full operator list
- GOTA station call and GOTA QSO totals, if any
- Power-source description (sets the ×5 vs ×2 multiplier)
- Claimed **bonus points** and their supporting documentation (emergency/alternate power, public location, media publicity, NTS traffic, etc.)

*Generated from the raw DM420 decode captures by `make_fd_writeup.py`. QSO set verified by independent re-parse and against DM420's logbooks. Confirmed 381 / claimed 438.*
