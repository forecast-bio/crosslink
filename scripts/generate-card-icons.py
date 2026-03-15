#!/usr/bin/env python3
"""
Generate small layered-shape SVG icons for each feature card on the home page.

Each icon is 2-4 overlapping shapes with multiply blending, inspired by
Forecast brand layered shapes. Output: one SVG per card in docs_src/assets/img/cards/.

Usage:
    python3 scripts/generate-card-icons.py -o docs_src/assets/img/cards
"""

import argparse
import os
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
from brand import P, ellipse, circle, rrect

MUL = 'style="mix-blend-mode: multiply"'

def _m(shape_svg):
    return f'  <g {MUL}>\n  {shape_svg}  </g>\n'


def icon_svg(width, height, shapes):
    svg = f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {width} {height}" width="{width}" height="{height}">\n'
    for s in shapes:
        svg += _m(s)
    svg += '</svg>\n'
    return svg


# ── Icon definitions ─────────────────────────────────────────────────────────
# Each is a (slug, width, height, [shapes]) tuple.
# Shapes are composed to suggest the feature's concept.

ICONS = [
    # 1. Session Memory — two overlapping loops suggesting continuity/handoff
    #    A chain of linked ellipses, like sessions passing state forward
    ("session-memory", 150, 100, [
        ellipse(45, 50, 38, 30, P["pink"]),
        ellipse(85, 50, 38, 30, P["blue"]),
        circle(115, 32, 12, P["green"]),   # handoff dot
    ]),

    # 2. Local-First Tracking — stacked rounded rects like a database/file
    #    Solid grounded block with layers suggesting data persistence
    ("local-first", 150, 100, [
        rrect(25, 18, 80, 22, P["green"], rx=11),     # top layer
        rrect(25, 38, 80, 22, P["green"], rx=11),     # mid layer
        rrect(25, 58, 80, 22, P["green"], rx=11),     # bottom layer
        circle(115, 70, 14, P["yellow"]),              # accent
    ]),

    # 3. Multi-Agent Coordination — three agent circles in a triangle formation
    #    with overlapping edges suggesting coordination/connection
    ("multi-agent", 150, 100, [
        circle(50, 60, 30, P["blue"]),
        circle(100, 60, 30, P["green"]),
        circle(75, 30, 30, P["red"]),
    ]),

    # 4. Behavioral Hooks — tall rrect (rail/guardrail) with a small shape
    #    caught/attached to it, like something being guided along a track
    ("hooks", 140, 100, [
        rrect(30, 8, 28, 82, P["green"], rx=14),       # tall rail
        ellipse(85, 50, 35, 28, P["yellow"]),           # guided shape
        circle(50, 78, 10, P["red"]),                   # catch point
    ]),

    # 5. Swarm Orchestration — cluster of circles in a honeycomb-like pattern
    #    Many small agents working together as a unit
    ("swarm", 150, 100, [
        circle(50, 38, 22, P["blue"]),
        circle(90, 38, 22, P["red"]),
        circle(70, 65, 22, P["green"]),
        circle(110, 65, 16, P["yellow"]),
    ]),

    # 6. Knowledge Management — stacked pages/sheets fanning out
    #    Overlapping rounded rects like documents with a search dot
    ("knowledge", 150, 100, [
        rrect(18, 25, 70, 55, P["blue"], rx=12, rotate=-6),    # back page
        rrect(35, 20, 70, 55, P["blue"], rx=12, rotate=3),     # front page
        ellipse(120, 45, 22, 18, P["yellow"]),                  # search lens
        circle(118, 70, 8, P["red"]),                           # accent
    ]),

    # 7. Terminal Dashboard — wide screen shape with cursor/prompt inside
    #    Rounded rect as terminal with small blocks suggesting content
    ("tui", 150, 100, [
        rrect(12, 12, 110, 75, P["blue"], rx=16),              # terminal bg
        rrect(24, 30, 40, 10, P["green"], rx=5),               # line 1
        rrect(24, 48, 60, 10, P["yellow"], rx=5),              # line 2
        circle(130, 20, 10, P["red"]),                          # status dot
    ]),

    # 8. Web Dashboard — browser-like shape with chart/grid inside
    #    Rounded rect with colored blocks suggesting panels/widgets
    ("web-dashboard", 150, 100, [
        rrect(10, 10, 115, 78, P["pink"], rx=18),              # browser frame
        rrect(22, 35, 35, 40, P["blue"], rx=8),                # left panel
        rrect(65, 35, 48, 40, P["green"], rx=8),               # right panel
        circle(28, 20, 5, P["red"]),                            # window dot
    ]),

    # 9. Container Agents — box nested inside a larger box
    #    Outer container with inner agent, suggesting isolation
    ("containers", 150, 100, [
        rrect(12, 12, 100, 75, P["yellow"], rx=22),            # outer container
        rrect(35, 28, 55, 45, P["red"], rx=14),                # inner agent
        circle(120, 25, 12, P["green"]),                        # status indicator
    ]),

    # 10. Smart Workflow — arrow-like diagonal flow of shapes
    #     Overlapping rrects angled to suggest forward motion/automation
    ("workflow", 150, 100, [
        rrect(8, 40, 55, 35, P["yellow"], rx=17, rotate=-10),  # start
        rrect(52, 28, 55, 35, P["green"], rx=17),              # middle
        rrect(95, 38, 45, 30, P["red"], rx=15, rotate=8),      # end
    ]),

    # 11. Maintenance — interlocking circular shapes like gears
    #     Two overlapping ellipses with a small accent, gear-like
    ("maintenance", 150, 100, [
        ellipse(50, 50, 35, 35, P["red"]),
        ellipse(90, 50, 30, 30, P["yellow"]),
        circle(70, 50, 14, P["green"]),                         # overlap accent
    ]),

    # 12. Works Everywhere — shapes spread wide like platforms/devices
    #     Dispersed shapes suggesting breadth and universality
    ("everywhere", 160, 100, [
        circle(35, 50, 28, P["blue"]),
        rrect(68, 28, 50, 44, P["pink"], rx=16),
        circle(135, 45, 22, P["green"]),
        circle(100, 75, 10, P["yellow"]),
    ]),
]


def main():
    parser = argparse.ArgumentParser(description="Generate feature card icon SVGs")
    parser.add_argument("-o", "--output-dir", default="docs_src/assets/img/cards",
                        help="Output directory")
    args = parser.parse_args()

    os.makedirs(args.output_dir, exist_ok=True)

    for slug, w, h, shapes in ICONS:
        svg = icon_svg(w, h, shapes)
        path = os.path.join(args.output_dir, f"{slug}.svg")
        with open(path, "w") as f:
            f.write(svg)
        print(f"  Written: {path}", file=sys.stderr)


if __name__ == "__main__":
    main()
