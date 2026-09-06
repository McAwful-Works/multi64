#!/usr/bin/env python3
"""N64-style 3D letter logos, rendered programmatically to flat-colour SVG.

Scene: four square posts on the corners of a square footprint. Each side of the
square has a slab (one post-width deep, flush with the outer face) that holds
the letter's diagonal strokes, so every side reads as the letter.

Camera: orthographic, azimuth 45 deg (looking across the front corner),
elevation ELEV (measured from the official N64 logo SVG: sin(e) = 0.385).

Hidden surfaces: back-face culling + BSP-tree painter's algorithm, so the SVG
is exact (no depth-sort artefacts from interpenetrating bars).

No third-party dependencies.
"""
import math
import sys

EPS = 1e-7
SQ2 = math.sqrt(2.0)

# --- official N64 logo palette -------------------------------------------
GREEN = "#069330"
BLUE = "#0222A9"
YELLOW = "#FFC001"
RED = "#FE2015"
HIDDEN = None  # faces that can never be seen from this camera

ELEV = math.asin(0.385)  # ~22.6 deg


# --- tiny vector kit ------------------------------------------------------
def sub(a, b): return (a[0] - b[0], a[1] - b[1], a[2] - b[2])
def add(a, b): return (a[0] + b[0], a[1] + b[1], a[2] + b[2])
def mul(a, k): return (a[0] * k, a[1] * k, a[2] * k)
def dot(a, b): return a[0] * b[0] + a[1] * b[1] + a[2] * b[2]


def norm(a):
    l = math.sqrt(dot(a, a))
    return mul(a, 1.0 / l) if l > 0 else a


def newell(pts):
    nx = ny = nz = 0.0
    for i, p in enumerate(pts):
        q = pts[(i + 1) % len(pts)]
        nx += (p[1] - q[1]) * (p[2] + q[2])
        ny += (p[2] - q[2]) * (p[0] + q[0])
        nz += (p[0] - q[0]) * (p[1] + q[1])
    return (nx, ny, nz)


class Poly:
    __slots__ = ("pts", "color", "n", "d")

    def __init__(self, pts, color, n=None):
        self.pts = list(pts)
        self.color = color
        self.n = norm(newell(self.pts)) if n is None else n
        self.d = dot(self.n, self.pts[0])

    def area(self):
        nn = newell(self.pts)
        return math.sqrt(dot(nn, nn)) / 2


# --- solids ---------------------------------------------------------------
def convex_solid(faces, color_fn, sloped):
    """faces: list of vertex lists. Orientation is fixed so normals point
    away from the centroid (solid must be convex)."""
    allp = [p for f in faces for p in f]
    c = mul((sum(p[0] for p in allp), sum(p[1] for p in allp), sum(p[2] for p in allp)),
            1.0 / len(allp))
    out = []
    for f in faces:
        n = norm(newell(f))
        if dot(n, sub(f[0], c)) < 0:
            f = f[::-1]
            n = mul(n, -1)
        col = color_fn(n, sloped)
        if col is not HIDDEN:
            out.append(Poly(f, col, n))
    return out


def box(x0, x1, y0, y1, z0, z1, color_fn, sloped=RED):
    faces = [
        [(x0, y0, z0), (x1, y0, z0), (x1, y1, z0), (x0, y1, z0)],
        [(x0, y0, z1), (x1, y0, z1), (x1, y1, z1), (x0, y1, z1)],
        [(x0, y0, z0), (x1, y0, z0), (x1, y0, z1), (x0, y0, z1)],
        [(x0, y1, z0), (x1, y1, z0), (x1, y1, z1), (x0, y1, z1)],
        [(x0, y0, z0), (x0, y1, z0), (x0, y1, z1), (x0, y0, z1)],
        [(x1, y0, z0), (x1, y1, z0), (x1, y1, z1), (x1, y0, z1)],
    ]
    return convex_solid(faces, color_fn, sloped)


def side_map(k, a):
    """Side k of the footprint square. Returns f(u, v, z) -> world, where u runs
    along the side from the left post to the right post (as seen from OUTSIDE
    that side), v is depth into the shape (0 = outer face), z is up."""
    if k == 0:   # x = 0 face, left-front as seen by the camera
        return lambda u, v, z: (v, a - u, z)
    if k == 1:   # y = 0 face, right-front
        return lambda u, v, z: (u, v, z)
    if k == 2:   # x = a face, back-right
        return lambda u, v, z: (a - v, u, z)
    return lambda u, v, z: (a - u, a - v, z)  # y = a face, back-left


def prism(profile_uz, k, a, depth, color_fn, sloped):
    """Convex (u,z) profile extruded over v in [0, depth] on side k."""
    m = side_map(k, a)
    near = [m(u, 0.0, z) for (u, z) in profile_uz]
    far = [m(u, depth, z) for (u, z) in profile_uz]
    faces = [near, far]
    n = len(profile_uz)
    for i in range(n):
        j = (i + 1) % n
        faces.append([near[i], near[j], far[j], far[i]])
    return convex_solid(faces, color_fn, sloped)


# --- colour rule ----------------------------------------------------------
def n64_colors(n, sloped):
    nx, ny, nz = n
    if abs(nz) < 1e-6:                  # vertical face
        if nx < -0.5: return GREEN      # faces the camera's left
        if ny < -0.5: return BLUE       # faces the camera's right
        return HIDDEN
    if nz > 1 - 1e-6: return YELLOW     # flat top
    if nz < -(1 - 1e-6): return HIDDEN  # flat bottom
    return sloped                       # sloped face of a diagonal


# --- camera ---------------------------------------------------------------
def project(p, elev=ELEV):
    ce, se = math.cos(elev), math.sin(elev)
    X = (p[0] - p[1]) / SQ2
    Y = -p[2] * ce - (p[0] + p[1]) / SQ2 * se
    return (X, Y)


def eye_dir(elev=ELEV):
    ce, se = math.cos(elev), math.sin(elev)
    return (-ce / SQ2, -ce / SQ2, se)


# --- BSP painter ----------------------------------------------------------
def split(poly, n, d):
    front, back = [], []
    pts = poly.pts
    L = len(pts)
    dist = [dot(n, p) - d for p in pts]
    for i in range(L):
        p, dp = pts[i], dist[i]
        q, dq = pts[(i + 1) % L], dist[(i + 1) % L]
        if dp >= -EPS:
            front.append(p)
        if dp <= EPS:
            back.append(p)
        if (dp > EPS and dq < -EPS) or (dp < -EPS and dq > EPS):
            t = dp / (dp - dq)
            mpt = add(p, mul(sub(q, p), t))
            front.append(mpt)
            back.append(mpt)
    return front, back


class Node:
    def __init__(self, polys):
        # pick the splitter that cuts the fewest other polygons (sampled)
        best, best_cost = None, None
        for cand in polys[:12]:
            cost = 0
            for p in polys:
                if p is cand:
                    continue
                ds = [dot(cand.n, q) - cand.d for q in p.pts]
                if any(x > EPS for x in ds) and any(x < -EPS for x in ds):
                    cost += 1
            if best_cost is None or cost < best_cost:
                best, best_cost = cand, cost
        self.n, self.d = best.n, best.d
        self.coplanar = [best]
        front, back = [], []
        for p in polys:
            if p is best:
                continue
            ds = [dot(self.n, q) - self.d for q in p.pts]
            if all(abs(x) <= EPS for x in ds):
                self.coplanar.append(p)
            elif all(x >= -EPS for x in ds):
                front.append(p)
            elif all(x <= EPS for x in ds):
                back.append(p)
            else:
                f, b = split(p, self.n, self.d)
                if len(f) >= 3:
                    fp = Poly(f, p.color, p.n)
                    if fp.area() > 1e-9:
                        front.append(fp)
                if len(b) >= 3:
                    bp = Poly(b, p.color, p.n)
                    if bp.area() > 1e-9:
                        back.append(bp)
        self.front = Node(front) if front else None
        self.back = Node(back) if back else None

    def paint(self, eye, out):
        """Back-to-front order for an orthographic eye direction."""
        if dot(self.n, eye) > 0:
            if self.back:
                self.back.paint(eye, out)
            out.extend(self.coplanar)
            if self.front:
                self.front.paint(eye, out)
        else:
            if self.front:
                self.front.paint(eye, out)
            out.extend(self.coplanar)
            if self.back:
                self.back.paint(eye, out)


def render_order(polys, elev=ELEV):
    eye = eye_dir(elev)
    visible = [p for p in polys if dot(p.n, eye) > 1e-9]
    out = []
    Node(visible).paint(eye, out)
    return out


# --- scenes ---------------------------------------------------------------
def posts(a, s, h, cf):
    return (box(0, s, 0, s, 0, h, cf) + box(a - s, a, 0, s, 0, h, cf)
            + box(a - s, a, a - s, a, 0, h, cf) + box(0, s, a - s, a, 0, h, cf))


def scene_n64(a=3.53, s=1.0, h=3.25, t=None, sloped=(RED, RED, BLUE, GREEN)):
    """Replica of the Nintendo 64 mark. sloped = colour of each side's
    diagonal's sloped faces, in side order 0..3."""
    t = h / 2 if t is None else t
    cf = n64_colors
    polys = posts(a, s, h, cf)
    for k in range(4):
        prof = [(s, h), (s, h - t), (a - s, 0.0), (a - s, t)]
        polys += prism(prof, k, a, s, cf, sloped[k])
    return polys


def scene_m(a=3.53, s=1.0, h=3.25, t=None, zb=0.0, sloped=(RED, RED, BLUE, GREEN),
            equal_stroke=True, sides=(0, 1, 2, 3)):
    """The M: two posts per side joined by a V that hangs from the top.
    zb = height of the V's bottom point above the ground.
    t  = vertical thickness of each arm (None -> equal_stroke rule).
    sides = which sides carry a V (0,1 are the two the camera sees)."""
    cf = n64_colors
    run = a / 2 - s
    if t is None:
        if equal_stroke:
            # perpendicular stroke width == post width s; solve t = s / cos(theta)
            t = h / 2
            for _ in range(50):
                theta = math.atan2(h - (zb + t), run)
                t = s / math.cos(theta)
        else:
            t = h / 2
    polys = posts(a, s, h, cf)
    for k in sides:
        left = [(s, h), (s, h - t), (a / 2, zb), (a / 2, zb + t)]
        right = [(a - u, z) for (u, z) in left]
        polys += prism(left, k, a, s, cf, sloped[k])
        polys += prism(right, k, a, s, cf, sloped[k])
    return polys


# --- SVG ------------------------------------------------------------------
def svg_paths(polys, elev, ox, oy, scale):
    out = []
    for p in polys:
        P2 = [project(q, elev) for q in p.pts]
        a2 = abs(sum(P2[i][0] * P2[(i + 1) % len(P2)][1] - P2[(i + 1) % len(P2)][0] * P2[i][1]
                     for i in range(len(P2)))) / 2
        if a2 < 1e-4:
            continue
        pts = " ".join("%.4f,%.4f" % (ox + X * scale, oy + Y * scale)
                       for (X, Y) in (project(q, elev) for q in p.pts))
        out.append('<polygon fill="%s" stroke="%s" stroke-width="0.6" '
                   'stroke-linejoin="round" points="%s"/>' % (p.color, p.color, pts))
    return "\n".join(out)


def bbox(polys, elev):
    xs, ys = [], []
    for p in polys:
        for q in p.pts:
            X, Y = project(q, elev)
            xs.append(X)
            ys.append(Y)
    return min(xs), min(ys), max(xs), max(ys)


def svg_single(polys, size=1024, pad=0.06, elev=ELEV, bg=None):
    order = render_order(polys, elev)
    x0, y0, x1, y1 = bbox(polys, elev)
    w, hh = x1 - x0, y1 - y0
    scale = size * (1 - 2 * pad) / max(w, hh)
    ox = size / 2 - (x0 + x1) / 2 * scale
    oy = size / 2 - (y0 + y1) / 2 * scale
    body = svg_paths(order, elev, ox, oy, scale)
    bgrect = '<rect width="%d" height="%d" fill="%s"/>\n' % (size, size, bg) if bg else ""
    return ('<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 %d %d" width="%d" height="%d">\n'
            '%s%s\n</svg>\n' % (size, size, size, size, bgrect, body))


def svg_sheet(items, cell=360, cols=3, elev=ELEV):
    """items: list of (label, polys). Contact sheet with labels."""
    rows = (len(items) + cols - 1) // cols
    W, H = cell * cols, (cell + 40) * rows
    parts = ['<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 %d %d" width="%d" height="%d">'
             % (W, H, W, H),
             '<rect width="%d" height="%d" fill="#ffffff"/>' % (W, H)]
    for i, (label, polys) in enumerate(items):
        r, c = divmod(i, cols)
        cx, cy = c * cell, r * (cell + 40)
        order = render_order(polys, elev)
        x0, y0, x1, y1 = bbox(polys, elev)
        w, hh = x1 - x0, y1 - y0
        scale = cell * 0.86 / max(w, hh)
        ox = cx + cell / 2 - (x0 + x1) / 2 * scale
        oy = cy + cell / 2 - (y0 + y1) / 2 * scale
        parts.append(svg_paths(order, elev, ox, oy, scale))
        parts.append('<text x="%d" y="%d" font-family="Segoe UI, Arial, sans-serif" '
                     'font-size="15" text-anchor="middle" fill="#333">%s</text>'
                     % (cx + cell / 2, cy + cell + 22,
                        label.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;")))
    parts.append('</svg>')
    return "\n".join(parts) + "\n"


if __name__ == "__main__":
    # Exploration contact sheet of variants; the shipped masters come from generate.py.
    import os
    out = sys.argv[1] if len(sys.argv) > 1 else "out"
    os.makedirs(out, exist_ok=True)
    open(out + "/n64_replica.svg", "w").write(svg_single(scene_n64()))
    items = [
        ("N64 replica (calibration)", scene_n64()),
        ("M: same box, V to ground, equal stroke", scene_m()),
        ("M: same box, V to ground, t = h/2", scene_m(equal_stroke=False)),
        ("M: wider box a=4.5, V to ground", scene_m(a=4.5)),
        ("M: same box, V stops at h/3", scene_m(zb=3.25 / 3)),
        ("M: wider a=4.5, V stops at h/4", scene_m(a=4.5, zb=3.25 / 4)),
    ]
    open(out + "/sheet1.svg", "w").write(svg_sheet(items))
    print("wrote", out + "/n64_replica.svg", out + "/sheet1.svg")


def scene_x(a=4.2, s=1.0, h=3.6, t=None, sloped=(RED, RED, BLUE, GREEN), equal_stroke=True):
    """The X: two posts per side joined by two crossing diagonals."""
    cf = n64_colors
    run = a - 2 * s
    if t is None:
        if equal_stroke:
            t = h / 2
            for _ in range(50):
                theta = math.atan2(h - t, run)
                t = s / math.cos(theta)
        else:
            t = h / 2
    polys = posts(a, s, h, cf)
    for k in range(4):
        down = [(s, h), (s, h - t), (a - s, 0.0), (a - s, t)]
        up = [(a - u, z) for (u, z) in down]
        polys += prism(down, k, a, s, cf, sloped[k])
        polys += prism(up, k, a, s, cf, sloped[k])
    return polys


def scene_x2(a=4.2, s=1.0, h=3.6, w=None, sloped=(RED, RED, BLUE, GREEN)):
    """The X, letterform-correct: no posts. Each side carries two bars whose
    terminals are horizontal cuts w wide, so the two bars meeting at a corner
    each give it a flat square top. w=None -> w = s (square corner tops)."""
    w = s if w is None else w
    polys = []
    for k in range(4):
        down = [(0.0, h), (w, h), (a, 0.0), (a - w, 0.0)]
        up = [(a - u, z) for (u, z) in down]
        polys += prism(down, k, a, s, n64_colors, sloped[k])
        polys += prism(up, k, a, s, n64_colors, sloped[k])
    return polys


# --- colour schemes -------------------------------------------------------
def shade(hexcol, f):
    """Scale an #rrggbb colour's brightness by f."""
    r, g, b = int(hexcol[1:3], 16), int(hexcol[3:5], 16), int(hexcol[5:7], 16)
    return "#%02X%02X%02X" % (min(255, int(r * f)), min(255, int(g * f)), min(255, int(b * f)))


def recolor(polys, recessed=1.0, underside=1.0, back_sloped=1.0, recessed_color=None, tops=1.0):
    """Post-process the N64 colouring.
    recessed:   brightness factor for vertical green/blue faces that are NOT on the
                two outer planes the camera sees (x=0 for green, y=0 for blue).
    underside:  factor for downward-facing sloped faces (nz < 0).
    back_sloped: factor for sloped faces coloured green/blue (i.e. on the hidden sides).
    recessed_color: if set, recessed vertical faces take this colour instead.
    tops: factor for yellow tops that are recessed (not one of the four corner tops)."""
    out = []
    for p in polys:
        nx, ny, nz = p.n
        col = p.color
        if abs(nz) < 1e-6:
            onplane = (all(abs(q[0]) < 1e-6 for q in p.pts) if nx < -0.5
                       else all(abs(q[1]) < 1e-6 for q in p.pts))
            if not onplane:
                col = recessed_color if recessed_color else shade(col, recessed)
        elif nz > 1 - 1e-6:
            pass
        else:
            if col in (GREEN, BLUE):
                col = shade(col, back_sloped)
            elif nz < 0:
                col = shade(col, underside)
        out.append(Poly(p.pts, col, p.n))
    return out


def remap(polys, recessed=None, underside=None, back_sloped=None):
    """Palette-only recolouring. recessed: {GREEN: c, BLUE: c} for vertical faces
    off the two outer planes; underside: colour for downward sloped faces;
    back_sloped: colour for sloped faces on the hidden sides (currently green/blue)."""
    out = []
    for p in polys:
        nx, ny, nz = p.n
        col = p.color
        if abs(nz) < 1e-6:
            onplane = (all(abs(q[0]) < 1e-6 for q in p.pts) if nx < -0.5
                       else all(abs(q[1]) < 1e-6 for q in p.pts))
            if not onplane and recessed and col in recessed:
                col = recessed[col]
        elif nz < 1 - 1e-6:
            if col in (GREEN, BLUE):
                if back_sloped:
                    col = back_sloped
            elif nz < 0 and underside:
                col = underside
        out.append(Poly(p.pts, col, p.n))
    return out


def scene_x2_twotone(a=3.53, s=1.0, h=3.6, w=1.0, second=None, sloped=(RED, RED, BLUE, GREEN)):
    """X whose second ('/') bar's vertical outer faces use `second` = {GREEN: c, BLUE: c}."""
    second = second or {GREEN: BLUE, BLUE: GREEN}
    def cf2(nrm, sl):
        c = n64_colors(nrm, sl)
        if c in second and abs(nrm[2]) < 1e-6:
            return second[c]
        return c
    polys = []
    for k in range(4):
        down = [(0.0, h), (w, h), (a, 0.0), (a - w, 0.0)]
        up = [(a - u, z) for (u, z) in down]
        polys += prism(down, k, a, s, n64_colors, sloped[k])
        polys += prism(up, k, a, s, cf2, sloped[k])
    return polys
