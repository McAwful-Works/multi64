#!/usr/bin/env python3
"""Regenerate the Multi64 and Xfer64 masters next to this file.

    python branding/generate.py
    (cd crates/multi64 && npx tauri icon ../../branding/multi64.svg)
    (cd crates/xfer64  && npx tauri icon ../../branding/xfer64.svg)
    (cd crates/multi64-test-connector-gui && npx tauri icon ../../branding/multi64.svg)

`tauri icon` also emits android/ and ios/ folders and a 64x64.png that the
apps do not use; only the files already present in each src-tauri/icons/
are committed. No dependencies beyond the Python standard library.
"""
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import n64style as n  # noqa: E402

# Scheme S4: the four N64 hues only, with surfaces recessed behind the two
# camera-facing planes (and downward-facing sloped faces) shaded darker.
S4 = dict(recessed=0.55, underside=0.6, back_sloped=0.55)

# Multi64: four posts on a 4.2-wide footprint, posts 3.6 tall, a V per side.
MULTI64 = n.recolor(n.scene_m(a=4.2, h=3.6), **S4)
# Xfer64: no posts; two crossing bars with flat terminals per side, on the
# original N64 footprint (3.53) at the same height as the M.
XFER64 = n.recolor(n.scene_x2(a=3.53, h=3.6), **S4)

for name, polys in (("multi64.svg", MULTI64), ("xfer64.svg", XFER64)):
    path = os.path.join(HERE, name)
    with open(path, "w", newline="\n") as f:
        f.write(n.svg_single(polys))
    print("wrote", path)
