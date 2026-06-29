#!/usr/bin/env python3
"""Generate an ARRL Field Day Cabrillo 3.0 log for N0JDC from the verified
fd_worked_stations.csv.  All 438 creditable QSOs, one QSO: line each, mode DG.

Field Day note: Cabrillo carries no class/power/bonus fields -- those go on the
fdentry.php web form.  This file serves as the by-band/mode dupe sheet / QSO list
that ARRL accepts in lieu of a paper dupe sheet.  The station class (2B) appears
as the sent exchange on every QSO line.
"""
import csv, os
from datetime import datetime, timezone

HERE = os.path.dirname(os.path.abspath(__file__))
rows = list(csv.DictReader(open(os.path.join(HERE, "fd_worked_stations.csv"))))

MY_CALL, MY_CLASS, MY_SECTION = "N0JDC", "2B", "CO"

# Canonical FT8/FT4 dial frequency (kHz) per band; Cabrillo only needs to convey
# the band.  "both" sub-modes use the FT8 frequency for that band.
FREQ = {
    ("40m", "FT8"): 7074,  ("40m", "FT4"): 7047,
    ("20m", "FT8"): 14074, ("20m", "FT4"): 14080,
    ("15m", "FT8"): 21074, ("15m", "FT4"): 21140,
    ("10m", "FT8"): 28074, ("10m", "FT4"): 28180,
}
def freq_for(band, submodes):
    sm = "FT4" if submodes == "FT4" else "FT8"   # FT8 for FT8-only and FT4+FT8
    return FREQ.get((band, sm), 0)

def parse_t(s):
    return datetime.strptime(s, "%Y-%m-%d %H:%M:%S").replace(tzinfo=timezone.utc)

# Build a unified QSO list: (datetime, freq_kHz, mode_code, call, class, section).
qsos = []
for r in rows:                       # digital, from the verified decode reconstruction
    t = parse_t(r["FirstUTC"])
    qsos.append((t, freq_for(r["Band"], r["Submodes"]), "DG",
                 r["Call"], r["TheirClass"], r["TheirSection"]))

# Manually-logged QSOs not present in the digital decode captures (e.g. voice).
MODE_CODE = {"Phone": "PH", "CW": "CW", "Digital": "DG"}
n_phone = 0
mpath = os.path.join(HERE, "manual_qsos.csv")
if os.path.exists(mpath):
    for m in csv.DictReader(open(mpath)):
        t = datetime.strptime(m["DateUTC"] + " " + m["TimeUTC"],
                              "%Y-%m-%d %H%M").replace(tzinfo=timezone.utc)
        qsos.append((t, int(m["Freq_kHz"]), MODE_CODE.get(m["Mode"], "PH"),
                     m["Call"], m["TheirClass"], m["TheirSection"]))
        if m["Mode"] == "Phone":
            n_phone += 1

qsos.sort(key=lambda q: q[0])        # chronological order, like a real log
total = len(qsos)
n_digital = total - n_phone
# Field Day points: digital 2 pts each, phone 1 pt each; QRP x5 power multiplier.
score = (n_digital * 2 + n_phone * 1) * 5   # QSO points only (bonus added on the form)

out = []
out.append("START-OF-LOG: 3.0")
out.append("CREATED-BY: DM420 fd_report (reconstructed from raw decode captures)")
out.append("CONTEST: ARRL-FD")
out.append("CALLSIGN: N0JDC")
out.append("LOCATION: CO")
out.append("CATEGORY-OPERATOR: MULTI-OP")
out.append("CATEGORY-STATION: PORTABLE")
out.append("CATEGORY-TRANSMITTER: TWO")
out.append("CATEGORY-POWER: QRP")
out.append("CATEGORY-MODE: DIGI")
out.append("CATEGORY-BAND: ALL")
out.append("CLAIMED-SCORE: %d" % score)
out.append("OPERATORS: N0JDC, W4LL")
out.append("NAME: Josh Carter / Joel Odom")
out.append("SOAPBOX: ARRL Field Day 2026 -- N0JDC, Class 2B, Section CO, QRP.")
out.append("SOAPBOX: %d QSOs: %d digital (FT8/FT4) + %d phone." % (total, n_digital, n_phone))
out.append("SOAPBOX: Digital QSOs reconstructed from DM420 raw decode captures and verified")
out.append("SOAPBOX: three ways (381 confirmed + 57 exchange-complete); phone logged manually.")
out.append("SOAPBOX: Class/power/bonus are entered on the Field Day web form, not in Cabrillo.")
out.append("SOAPBOX: Digital frequencies are canonical band frequencies.")

for t, freq, mode, call, cls, sec in qsos:
    out.append(
        "QSO: %5d %s %s %s %-8s %-3s %-4s %-8s %-3s %-4s" % (
            freq, mode, t.strftime("%Y-%m-%d"), t.strftime("%H%M"),
            MY_CALL, MY_CLASS, MY_SECTION, call, cls, sec,
        )
    )
out.append("END-OF-LOG:")

path = os.path.join(HERE, "N0JDC_2026_FD.cbr")
open(path, "w").write("\n".join(out) + "\n")
print("wrote", path)
print("QSO lines: %d (%d digital + %d phone) | CLAIMED-SCORE: %d (QSO pts only, no bonus)"
      % (total, n_digital, n_phone, score))
