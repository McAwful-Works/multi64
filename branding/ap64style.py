#!/usr/bin/env python3
"""AP64 mark: the Archipelago ring of six circles, built as low-poly spheres and
flat-shaded the way the N64 would have drawn them.

Uses n64style's solids, BSP painter and SVG writer unchanged. That engine's
camera sits at a fixed 45 deg azimuth, so the logo is laid in the xy plane with
logo-right along (1,-1)/sqrt2 and logo-up along (1,1)/sqrt2; the projection then
maps logo (u, v) straight to screen (X, -Y), and `elev` is how far the camera
looks down onto the plane (90 deg = face-on).

    python branding/ap64style.py [outdir]   # contact sheet of variants
"""
import math
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import n64style as n  # noqa: E402

# Archipelago's own colours, sampled from upstream data/icon.png.
RED = "#C97682"
YELLOW = "#EEE391"
GREEN = "#75C275"
BLUE = "#767EBD"
PINK = "#CA94C2"
ORANGE = "#D9A07D"

# Circle centres in units of the circle radius (99 px in the 512 px upstream
# icon), relative to the ring's centre, logo-up positive; and the stacking
# level each one sits at (the upstream icon's overlap order: orange covers
# blue and pink, which cover yellow and green, which cover red).
RING = [
    (RED, 0.0, 1.55, 0),
    (GREEN, 1.47, 0.72, 1),
    (PINK, 1.47, -0.71, 2),
    (ORANGE, 0.0, -1.57, 3),
    (BLUE, -1.47, -0.71, 2),
    (YELLOW, -1.47, 0.72, 1),
]

SQ2 = math.sqrt(2)


def to_world(u, v, z):
    return ((u + v) / SQ2, (v - u) / SQ2, z)


def light_dir(u=-0.45, v=0.55, z=1.0):
    """Light from the viewer's upper left, in logo coordinates."""
    return n.norm(to_world(u, v, z))


def flat_shader(ambient=0.62, diffuse=0.48, levels=None, light=None):
    """One colour per face from its normal: the N64's flat-shaded look. A face
    pointing straight at the viewer keeps the base colour."""
    L = light or light_dir()
    top = ambient + diffuse * n.dot((0.0, 0.0, 1.0), L)

    def color_fn(nrm, base):
        f = (ambient + diffuse * max(0.0, n.dot(nrm, L))) / top
        if levels:
            f = round(f * levels) / levels
        return n.shade(base, f)
    return color_fn


def sphere(cu, cv, r, z0, base, color_fn, sides=12, bands=6, twist=0.0, upright=True):
    """A low-poly UV sphere: `sides` around, `bands` of latitude, a triangle fan
    at each pole. Rings share azimuths, so every band face is a planar
    trapezoid and the solid stays convex. upright puts the poles along logo-up,
    like a globe; otherwise they point at the viewer."""
    def pt(du, dv, dz):  # (du, dv, dz) with the pole along dz
        if upright:
            du, dv, dz = du, dz, -dv
        return to_world(cu + du, cv + dv, z0 + dz)

    lats = [-math.pi / 2 + math.pi * k / bands for k in range(1, bands)]
    az = [2 * math.pi * (i + twist) / sides for i in range(sides)]
    ring_pts = [[pt(r * math.cos(t) * math.cos(a), r * math.cos(t) * math.sin(a),
                    r * math.sin(t)) for a in az] for t in lats]
    south, north = pt(0, 0, -r), pt(0, 0, r)
    faces = []
    for i in range(sides):
        j = (i + 1) % sides
        faces.append([south, ring_pts[0][j], ring_pts[0][i]])
        faces.append([north, ring_pts[-1][i], ring_pts[-1][j]])
        for lo, hi in zip(ring_pts, ring_pts[1:]):
            faces.append([lo[i], lo[j], hi[j], hi[i]])
    return n.convex_solid(faces, color_fn, base)


# The shipped master: a 25 deg tilt off face-on, strong light.
TILT = 25


def scene_ap(step=0.12, sides=12, bands=6, twist=0.0, color_fn=None, spread=1.0, upright=True):
    """step: rise per stacking level, in circle radii. spread scales the ring
    out from its centre; at 1.0 the spheres overlap as the upstream circles
    do, and intersect where they meet."""
    cf = color_fn or flat_shader(ambient=0.45, diffuse=0.75)
    polys = []
    for base, u, v, lvl in RING:
        polys += sphere(u * spread, v * spread, 1.0, lvl * step, base, cf,
                        sides=sides, bands=bands, twist=twist, upright=upright)
    return polys


def elev_for_tilt(tilt_deg):
    return math.radians(90 - tilt_deg)


if __name__ == "__main__":
    out = sys.argv[1] if len(sys.argv) > 1 else os.path.join(HERE, "out")
    os.makedirs(out, exist_ok=True)
    variants = [
        ("%d x %d (%d faces per sphere)%s" % (sd, bd, sd * bd, " - master" if sd == 12 else ""),
         dict(sides=sd, bands=bd), TILT)
        for sd, bd in ((6, 3), (8, 4), (10, 5), (12, 6), (16, 8), (20, 10))
    ]
    # svg_sheet takes one elev for the whole sheet, so render each cell alone
    # and tile them.
    cell, cols = 360, 3
    rows = (len(variants) + cols - 1) // cols
    W, H = cell * cols, (cell + 40) * rows
    parts = ['<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 %d %d" width="%d" height="%d">'
             % (W, H, W, H), '<rect width="%d" height="%d" fill="#ffffff"/>' % (W, H)]
    for i, (label, kw, tilt) in enumerate(variants):
        r, c = divmod(i, cols)
        elev = elev_for_tilt(tilt)
        polys = scene_ap(**kw)
        sub = n.svg_single(polys, size=cell, pad=0.07, elev=elev, stroke=0.5)
        body = sub.split("\n", 1)[1].rsplit("</svg>", 1)[0]
        parts.append('<g transform="translate(%d,%d)">%s</g>' % (c * cell, r * (cell + 40), body))
        parts.append('<text x="%d" y="%d" font-family="Segoe UI, Arial, sans-serif" '
                     'font-size="15" text-anchor="middle" fill="#333">%s</text>'
                     % (c * cell + cell / 2, r * (cell + 40) + cell + 22, label))
        with open(os.path.join(out, "ap64_%d.svg" % i), "w", newline="\n") as f:
            f.write(n.svg_single(polys, elev=elev, stroke=1.5))
    parts.append("</svg>")
    path = os.path.join(out, "ap64_sheet.svg")
    with open(path, "w", newline="\n") as f:
        f.write("\n".join(parts) + "\n")
    print("wrote", path)
