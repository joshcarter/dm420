#!/usr/bin/env python3
"""Generate src/geo_data.rs (land + lake basemap) from Natural Earth 50m.

Clips features to a North America box, simplifies (Douglas-Peucker), and
pre-triangulates each ring with mapbox_earcut so the Rust side only projects
vertices and draws a static mesh (robust + cheap, no runtime triangulation).

Setup:
    pip install mapbox_earcut numpy
    curl -sL -o /tmp/ne_50m_land.geojson  https://raw.githubusercontent.com/nvkelso/natural-earth-vector/master/geojson/ne_50m_land.geojson
    curl -sL -o /tmp/ne_50m_lakes.geojson https://raw.githubusercontent.com/nvkelso/natural-earth-vector/master/geojson/ne_50m_lakes.geojson
Run:
    python3 tools/gen_geo.py && cp /tmp/geo_out.rs src/geo_data.rs
"""
import json, mapbox_earcut as earcut, numpy as np

KX0, KX1, KY0, KY1 = -128.0, -60.0, 12.0, 58.0   # view-intersection box (rings kept whole)

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

def intersects(r): return any(KX0<=p[0]<=KX1 and KY0<=p[1]<=KY1 for p in r)

def collect(path, min_area, tol):
    data=json.load(open(path)); rings=[]
    for feat in data["features"]:
        for r in rings_of(feat["geometry"]):
            if len(r)<4 or not intersects(r) or area(r)<min_area: continue
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

land=collect("/tmp/ne_50m_land.geojson", 0.4, 0.06)
lakes=collect("/tmp/ne_50m_lakes.geojson", 1.0, 0.03)
lv,lr,lt=build(land,"LAND")
kv,kr,kt=build(lakes,"LAKES")
with open("/tmp/geo_out.rs","w") as f:
    f.write("// Generated from Natural Earth 50m (land + lakes), clipped to North America\n")
    f.write("// and pre-triangulated (mapbox_earcut). Regenerate via /tmp/gen_geo.py.\n")
    f.write("// VERTS are (lat, lon); RINGS are (start, len) spans for outline strokes;\n")
    f.write("// IDX are triangle indices (groups of 3) into VERTS for the fill mesh.\n\n")
    f.write(emit("LAND",lv,lr,lt)+"\n\n")
    f.write(emit("LAKES",kv,kr,kt)+"\n")
print("// wrote /tmp/geo_out.rs")
