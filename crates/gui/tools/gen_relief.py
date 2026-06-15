#!/usr/bin/env python3
"""Bake a shaded-relief texture (assets/relief.png) from a global GEBCO heightmap.

Crops to a North America box, computes a NW-lit hillshade, and concentrates the
shading on steep terrain (ruggedness mask) so plains stay flat. Output is a
grayscale multiplier (255 = unshaded) that the land mesh modulates at runtime.

Setup: pip install pillow scipy numpy ; fetch a global equirectangular gray DEM:
  curl -sLo /tmp/gebco.png https://eoimages.gsfc.nasa.gov/images/imagerecords/73000/73934/gebco_08_rev_elev_21600x10800.png
Run: python3 tools/gen_relief.py
"""
from pathlib import Path
import numpy as np
from PIL import Image
from scipy.ndimage import gaussian_filter

# Relief box (must match RELIEF_* constants in Rust for UV mapping).
LON0, LON1, LAT0, LAT1 = -130.0, -58.0, 8.0, 62.0
OUT_W = 900
STRENGTH = 0.475     # how dark the deepest shadow gets
EXAG = 8.0           # vertical exaggeration of the gradient
EDGE_BAND = 0.08     # outer fraction faded to flat so off-box land doesn't smear

Image.MAX_IMAGE_PIXELS = None
g = Image.open("/tmp/gebco.png").convert("L")
GW, GH = g.size                      # 21600 x 10800, equirectangular
def px(lon, lat):
    return int((lon + 180) / 360 * GW), int((90 - lat) / 180 * GH)
x0, y0 = px(LON0, LAT1)              # top-left (lat max)
x1, y1 = px(LON1, LAT0)             # bottom-right
crop = g.crop((x0, y0, x1, y1))
aspect = (LAT1 - LAT0) / (LON1 - LON0)
out_h = int(OUT_W * aspect)
crop = crop.resize((OUT_W, out_h), Image.BILINEAR)
elev = np.asarray(crop, dtype=np.float32)
elev = gaussian_filter(elev, 1.0)

# Surface normals from gradient (NW light).
gy, gx = np.gradient(elev * EXAG)
nz = np.ones_like(elev)
nlen = np.sqrt(gx*gx + gy*gy + nz*nz)
az, alt = np.deg2rad(315.0), np.deg2rad(45.0)
lx, ly, lz = np.cos(alt)*np.sin(az), np.cos(alt)*np.cos(az), np.sin(alt)
hill = np.clip((-gx*lx + -gy*ly + nz*lz) / nlen, 0, 1)   # 0 shadow .. 1 lit

# Ruggedness: normalized slope magnitude → only mountains get shaded.
slope = np.sqrt(gx*gx + gy*gy)
rug = np.clip(slope / (np.percentile(slope, 97) + 1e-6), 0, 1) ** 0.7

relief = 1.0 - rug * (1.0 - hill) * STRENGTH        # plains ~1.0, mtn shadows darker
relief = np.clip(relief, 0.0, 1.0)

# Feather the shadow deviation to 0 in an outer band: land whose grid cells fall
# outside the relief box samples the (clamped) edge texels, so flat edges prevent
# smearing into northern Canada / Central America while keeping a seamless seam.
def smoothstep(t):
    t = np.clip(t, 0.0, 1.0); return t * t * (3 - 2 * t)
h, w = relief.shape
by, bx = max(int(h * EDGE_BAND), 1), max(int(w * EDGE_BAND), 1)
fy = smoothstep(np.minimum(np.arange(h), h - 1 - np.arange(h)) / by)
fx = smoothstep(np.minimum(np.arange(w), w - 1 - np.arange(w)) / bx)
relief = 1.0 - (1.0 - relief) * np.outer(fy, fx)
img = Image.fromarray((relief * 255).astype(np.uint8), mode="L")
img.save(Path(__file__).resolve().parents[1] / "assets" / "relief.png")
print(f"// relief box: LON {LON0}..{LON1}, LAT {LAT0}..{LAT1}")
print(f"// wrote assets/relief.png ({OUT_W}x{out_h})")
