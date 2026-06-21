#!/usr/bin/env python3
"""Generate src/geo_data.rs (land + lake basemap) from Natural Earth.

Covers the WHOLE WORLD: keeps every ring (Natural Earth is dateline-split, so no
antimeridian smearing), simplifies (Douglas-Peucker), and pre-triangulates each
ring with mapbox_earcut so the Rust side only projects vertices and draws a
static mesh (robust + cheap, no runtime triangulation). The map auto-fits its
bounds to the plotted spots, so this geometry must span the globe — contacts
anywhere on Earth land on real coastline.

Two knobs trade coastline accuracy against the emitted file size (the geometry
ships as a Rust array literal, so size ≈ compile cost):
  - RES   = Natural Earth resolution: "10m" (most detail), "50m", or "110m".
  - *_TOL = Douglas-Peucker tolerance in degrees (smaller = more faithful coast).
For a genuinely sharper coast use RES=10m with a small tolerance; 50m can't carry
detail finer than its own ~0.05° sampling no matter how small the tolerance is.
MIN_AREA drops islands below that size (deg²) from the *backdrop* — a station
there still gets its own marker. All are overridable via env vars.

Setup (RES defaults to 10m):
    pip install mapbox_earcut numpy
    RES=10m; for k in land lakes; do \
      curl -sL -o /tmp/ne_${RES}_${k}.geojson \
        https://raw.githubusercontent.com/nvkelso/natural-earth-vector/master/geojson/ne_${RES}_${k}.geojson; done
Run:
    python3 tools/gen_geo.py && cp /tmp/geo_out.rs src/geo_data.rs
"""
import json, os, mapbox_earcut as earcut, numpy as np

RES           = os.environ.get("RES", "10m")
LAND_TOL      = float(os.environ.get("LAND_TOL", "0.05"))
LAKE_TOL      = float(os.environ.get("LAKE_TOL", "0.04"))
LAND_MIN_AREA = float(os.environ.get("LAND_MIN_AREA", "0.3"))
LAKE_MIN_AREA = float(os.environ.get("LAKE_MIN_AREA", "2.0"))

def rings_of(geom):
    t=geom["type"]; c=geom["coordinates"]
    if t=="Polygon": return [c[0]]
    if t=="MultiPolygon": return [poly[0] for poly in c]
    return []

def area(poly):
    s=0.0; n=len(poly)
    for i in range(n):
        x1,y1=poly[i]; x2,y2=poly[(i+1)%n]
        s+=x1*y2-x2*y1
    return abs(s)*0.5

def dp_open(pts, tol):
    if len(pts)<3: return pts
    keep=[False]*len(pts); keep[0]=keep[-1]=True
    stack=[(0,len(pts)-1)]
    while stack:
        i,j=stack.pop()
        if j<=i+1: continue
        ax,ay=pts[i]; bx,by=pts[j]; dx,dy=bx-ax,by-ay; L=(dx*dx+dy*dy)**0.5 or 1e-9
        dmax,idx=0.0,-1
        for k in range(i+1,j):
            px,py=pts[k]; d=abs((px-ax)*dy-(py-ay)*dx)/L
            if d>dmax: dmax,idx=d,k
        if dmax>tol:
            keep[idx]=True; stack.append((i,idx)); stack.append((idx,j))
    return [p for p,k in zip(pts,keep) if k]

def dp(pts, tol):
    if pts and pts[0]==pts[-1]: pts=pts[:-1]
    n=len(pts)
    if n<4: return pts
    ax,ay=pts[0]
    far=max(range(1,n), key=lambda k:(pts[k][0]-ax)**2+(pts[k][1]-ay)**2)
    a=dp_open(pts[:far+1],tol); b=dp_open(pts[far:]+[pts[0]],tol)
    return a+b[1:-1]

def collect(path, min_area, tol):
    data=json.load(open(path)); rings=[]
    for feat in data["features"]:
        for r in rings_of(feat["geometry"]):
            if len(r)<4 or area(r)<min_area: continue
            s=dp(r,tol)
            if len(s)>=4: rings.append(s)
    rings.sort(key=area, reverse=True)
    return rings

def build(rings, label):
    # Concatenate ring vertices; record (start,len) per ring; earcut each ring.
    verts=[]; ringspans=[]; tris=[]
    for r in rings:
        start=len(verts)
        ringspans.append((start,len(r)))
        verts.extend(r)
        arr=np.array(r, dtype=np.float64)
        idx=earcut.triangulate_float64(arr, np.array([len(r)]))  # no holes
        tris.extend(int(start+i) for i in idx)
    print(f"// {label}: {len(rings)} rings, {len(verts)} verts, {len(tris)//3} tris")
    return verts, ringspans, tris

def emit(name, verts, ringspans, tris):
    out=[]
    vbody=",".join(f"({p[1]:.2f},{p[0]:.2f})" for p in verts)  # (lat,lon)
    out.append(f"pub const {name}_VERTS: &[(f32, f32)] = &[{vbody}];")
    rbody=",".join(f"({s},{l})" for s,l in ringspans)
    out.append(f"pub const {name}_RINGS: &[(u32, u32)] = &[{rbody}];")
    ibody=",".join(str(i) for i in tris)
    out.append(f"pub const {name}_IDX: &[u32] = &[{ibody}];")
    return "\n".join(out)

# World scale: drop sub-MIN_AREA islands from the backdrop so the emitted source
# stays manageable; DP tolerance is the main accuracy/size lever (see header).
land=collect(f"/tmp/ne_{RES}_land.geojson", LAND_MIN_AREA, LAND_TOL)
lakes=collect(f"/tmp/ne_{RES}_lakes.geojson", LAKE_MIN_AREA, LAKE_TOL)
lv,lr,lt=build(land,"LAND")
kv,kr,kt=build(lakes,"LAKES")
with open("/tmp/geo_out.rs","w") as f:
    f.write(f"// Generated from Natural Earth {RES} (land + lakes), WHOLE WORLD,\n")
    f.write("// pre-triangulated (mapbox_earcut). Regenerate via tools/gen_geo.py.\n")
    f.write("// VERTS are (lat, lon); RINGS are (start, len) spans for outline strokes;\n")
    f.write("// IDX are triangle indices (groups of 3) into VERTS for the fill mesh.\n\n")
    # Generated coordinate data: a coastline longitude near -3.14 is not π.
    f.write("#![allow(clippy::approx_constant)]\n\n")
    f.write(emit("LAND",lv,lr,lt)+"\n\n")
    f.write(emit("LAKES",kv,kr,kt)+"\n")
print("// wrote /tmp/geo_out.rs")
