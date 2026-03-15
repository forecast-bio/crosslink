#!/usr/bin/env python3
"""
Generate a Forecast-styled diagram for the design document workflow.

Shows: Explore → Interview → Draft → Validate → Iterate loop → Implementation outputs

Usage:
    python3 scripts/generate-diagram-design.py -o docs_src/assets/img/design-flow.svg
"""

import argparse
import math
import random
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
from brand import (P, svg_header, svg_footer, ellipse, circle, rrect, text,
                   arrow_curved, arrow_straight, confetti, pill, card, container,
                   write_svg)

WIDTH = 700
HEIGHT = 580
SEED = 31


def generate():
    rng = random.Random(SEED)
    svg = svg_header(WIDTH, HEIGHT)

    cx = WIDTH / 2

    # ── Title ─────────────────────────────────────────────────────────────
    svg += text(cx, 36, "Design document workflow", cls="heading", size=22, fill=P["black"])
    svg += text(cx, 56, "from idea to validated, codebase-grounded spec",
                cls="subheading", size=14, fill=P["muted"])

    # ── Main flow (horizontal, top row) ───────────────────────────────────
    flow_y = 155
    nodes = [
        (100,  P["blue"],   "Explore",   "search code +\nknowledge pages"),
        (260,  P["green"],  "Interview", "3-5 grounded\nquestions"),
        (420,  P["yellow"], "Draft",     ".design/&lt;slug&gt;.md"),
        (580,  P["red"],    "Validate",  "reqs, ACs,\nreal file refs"),
    ]

    for nx, color, label, desc in nodes:
        svg += ellipse(nx, flow_y, 65, 45, color, opacity=0.12)
        svg += ellipse(nx, flow_y, 55, 37, P["white"], opacity=0.85)
        svg += text(nx, flow_y - 10, label, cls="heading", size=15, fill=color)

        lines = desc.split("\n")
        for i, line in enumerate(lines):
            is_code = line.startswith(".")
            svg += text(nx, flow_y + 10 + i * 16, line,
                        cls="mono" if is_code else "body", size=11, fill=P["muted"])

    # Straight arrows between nodes (solid, black)
    for i in range(len(nodes) - 1):
        x1 = nodes[i][0] + 65
        x2 = nodes[i + 1][0] - 65
        svg += arrow_straight(x1, flow_y, x2, flow_y, P["text"], stroke_width=2)

    # ── Iterate loop: Validate → back to Interview ────────────────────────
    # Solid red curve, tighter arc
    loop_drop = 70
    svg += (f'  <path d="M 580 {flow_y + 48} Q 580 {flow_y + loop_drop} 420 {flow_y + loop_drop} '
            f'Q 260 {flow_y + loop_drop} 260 {flow_y + 48}" '
            f'fill="none" stroke="{P["red"]}" stroke-width="2" '
            f'stroke-linecap="round" opacity="0.7"/>\n')
    # Arrowhead — compute angle from the final bezier approach
    # Path ends at (260, flow_y+48), approaching from below (260, flow_y+loop_drop)
    arr_angle = math.atan2((flow_y + 48) - (flow_y + loop_drop), 0)  # straight up
    hl = 8
    ax1 = 260 - hl * math.cos(arr_angle - 0.4)
    ay1 = (flow_y + 48) - hl * math.sin(arr_angle - 0.4)
    ax2 = 260 - hl * math.cos(arr_angle + 0.4)
    ay2 = (flow_y + 48) - hl * math.sin(arr_angle + 0.4)
    svg += (f'  <polygon points="260,{flow_y + 48} {ax1:.1f},{ay1:.1f} {ax2:.1f},{ay2:.1f}" '
            f'fill="{P["red"]}" opacity="0.7"/>\n')
    # Label — positioned below the arc, moved up to avoid overlap
    svg += text(420, flow_y + loop_drop - 6, "/design --continue &lt;slug&gt;", cls="mono",
                size=12, fill=P["red"])

    # ── Validation status callout ─────────────────────────────────────────
    vy = flow_y + 120
    svg += rrect(180, vy, 340, 65, P["gray"], rx=16)
    checks = [
        ("[PASS] requirements: 3",       P["green"]),
        ("[PASS] acceptance criteria: 4", P["green"]),
        ("[OPEN] 1 unresolved question",  P["yellow"]),
    ]
    for i, (check, color) in enumerate(checks):
        svg += text(cx, vy + 20 + i * 18, check, cls="mono", size=11, fill=color)

    # ── Output cards (bottom row) ─────────────────────────────────────────
    out_y = vy + 90

    svg += text(cx, out_y - 5, "From design to implementation", cls="heading",
                size=16, fill=P["black"])

    # Arrow from validate down to output row (solid)
    svg += arrow_straight(cx, vy + 65, cx, out_y + 10, P["muted"], stroke_width=1.5)

    card_y = out_y + 15
    cards = [
        (30,  160, P["green"],  "Knowledge",
         [("stored via git", False), ("tagged: design-doc", False), ("searchable", False)]),
        (200, 160, P["blue"],   "Gap analysis",
         [("kickoff plan", False), ("files to modify", False), ("tests needed", False)]),
        (370, 160, P["yellow"], "Single agent",
         [("kickoff run --doc", True), ("validates ACs", False), ("reports result", False)]),
        (540, 145, P["red"],    "Swarm build",
         [("swarm init --doc", True), ("phased execution", False), ("budget-aware", False)]),
    ]
    for card_x, card_w, color, title, items in cards:
        svg += rrect(card_x, card_y, card_w, 115, color, rx=18, opacity=0.08)
        svg += text(card_x + card_w / 2, card_y + 20, title,
                    cls="subheading", size=15, fill=color, weight="bold")
        for j, (item, is_cmd) in enumerate(items):
            iy = card_y + 45 + j * 24
            svg += circle(card_x + 18, iy - 3, 4, color, opacity=0.5)
            svg += text(card_x + 32, iy, item,
                        cls="mono" if is_cmd else "body",
                        size=11 if is_cmd else 13,
                        fill=P["text"], anchor="start")

    # ── Confetti ──────────────────────────────────────────────────────────
    svg += confetti(rng, 10, 80, 60, 80, 5)
    svg += confetti(rng, 630, 80, 60, 80, 5)

    svg += svg_footer()
    return svg


def main():
    parser = argparse.ArgumentParser(description="Generate design workflow diagram SVG")
    parser.add_argument("-o", "--output", help="Output file (default: stdout)")
    args = parser.parse_args()
    write_svg(generate(), args)


if __name__ == "__main__":
    main()
