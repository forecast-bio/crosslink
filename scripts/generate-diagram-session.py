#!/usr/bin/env python3
"""
Generate a Forecast-styled vertical diagram for the crosslink session lifecycle.

Usage:
    python3 scripts/generate-diagram-session.py -o docs_src/assets/img/session-flow.svg
"""

import argparse
import random
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
from brand import (P, svg_header, svg_footer, ellipse, circle, rrect, text,
                   arrow_curved, arrow_straight, confetti, container, write_svg)

WIDTH = 520
HEIGHT = 620
SEED = 11


def generate():
    rng = random.Random(SEED)
    svg = svg_header(WIDTH, HEIGHT)

    cx = WIDTH / 2

    # ── Title ─────────────────────────────────────────────────────────────
    svg += text(cx, 36, "Session lifecycle", cls="heading", size=22, fill=P["black"])
    svg += text(cx, 56, "persistent memory across conversations", cls="subheading", size=14, fill=P["muted"])

    # ── Vertical timeline backbone ────────────────────────────────────────
    svg += (f'  <line x1="{cx}" y1="80" x2="{cx}" y2="510" '
            f'stroke="{P["gray"]}" stroke-width="3" stroke-linecap="round"/>\n')

    # ── Phase nodes along vertical timeline (compressed spacing) ─────────
    phases = [
        (110, P["green"],  "session start",     "Reads handoff notes from previous session"),
        (210, P["blue"],   "session work &lt;id&gt;", "Marks focus issue, starts timer"),
        (310, P["yellow"], "session action",    "Records breadcrumbs that survive compression"),
        (410, P["red"],    "/commit",           "Commits changes, records result on issue"),
        (510, P["green"],  "session end",       "Writes handoff notes for next session"),
    ]

    for py, color, label, desc in phases:
        # Node on timeline
        svg += circle(cx, py, 16, color, opacity=0.2)
        svg += circle(cx, py, 10, P["white"])
        svg += circle(cx, py, 6, color)

        # Label and description to the right (capped to avoid clipping)
        is_code = label.startswith("session") or label.startswith("/")
        label_cls = "mono" if is_code else "body"
        svg += text(cx + 30, py - 4, label, cls=label_cls, size=14, fill=color,
                    anchor="start", weight="bold")
        svg += text(cx + 30, py + 16, desc, cls="body", size=11, fill=P["muted"], anchor="start")

    # ── Wrap-around arrow: end → next start ───────────────────────────────
    # Tighter arc (x=90 instead of x=60), solid line, proper arrowhead
    arc_x = 160
    # Cubic bezier: leaves bottom horizontally, curves left, arrives at top horizontally
    svg += (f'  <path d="M {cx - 20} 526 C {arc_x} 526 {arc_x} 94 {cx - 20} 94" '
            f'fill="none" stroke="{P["green"]}" stroke-width="2" '
            f'stroke-linecap="round" opacity="0.7"/>\n')
    # Arrowhead pointing right (tangent at endpoint is horizontal)
    tip_x = cx - 20
    tip_y = 94
    svg += (f'  <polygon points="{tip_x},{tip_y} {tip_x - 10},{tip_y - 5} {tip_x - 10},{tip_y + 5}" '
            f'fill="{P["green"]}" opacity="0.7"/>\n')
    # Labels beside the arrow
    svg += text(arc_x - 12, 300, "next conversation", cls="subheading", size=12, fill=P["green"],
                anchor="end")
    svg += text(arc_x - 12, 317, "picks up here", cls="subheading", size=12, fill=P["green"],
                anchor="end")

    # ── Bottom summary ────────────────────────────────────────────────────
    svg += rrect(80, 550, WIDTH - 160, 50, P["gray"], rx=18)
    svg += text(cx, 580, "Every breadcrumb and handoff note persists across restarts",
                cls="body", size=13, fill=P["text"])

    # ── Confetti ──────────────────────────────────────────────────────────
    svg += confetti(rng, 400, 80, 100, 120, 6)
    svg += confetti(rng, 400, 420, 100, 100, 5)

    svg += svg_footer()
    return svg


def main():
    parser = argparse.ArgumentParser(description="Generate session lifecycle diagram SVG")
    parser.add_argument("-o", "--output", help="Output file (default: stdout)")
    args = parser.parse_args()
    write_svg(generate(), args)


if __name__ == "__main__":
    main()
