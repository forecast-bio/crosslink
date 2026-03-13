#!/usr/bin/env python3
"""
Generate a Forecast-styled diagram for the design document workflow.

Shows: Explore → Interview → Draft → Validate → Iterate loop → Implementation outputs

Usage:
    python3 scripts/generate-diagram-design.py -o docs_src/assets/img/design-flow.svg
"""

import argparse
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
        svg += text(nx, flow_y - 8, label, cls="heading", size=15, fill=color)

        lines = desc.split("\n")
        for i, line in enumerate(lines):
            is_code = line.startswith(".")
            svg += text(nx, flow_y + 14 + i * 16, line,
                        cls="mono" if is_code else "body", size=11, fill=P["muted"])

    # Arrows between nodes
    for i in range(len(nodes) - 1):
        x1 = nodes[i][0] + 65
        x2 = nodes[i + 1][0] - 65
        svg += arrow_curved(x1, flow_y, x2, flow_y, P["text"], stroke_width=2)

    # ── Iterate loop: Validate → back to Interview ────────────────────────
    svg += (f'  <path d="M 580 {flow_y + 50} Q 580 {flow_y + 90} 420 {flow_y + 90} '
            f'Q 260 {flow_y + 90} 260 {flow_y + 50}" '
            f'fill="none" stroke="{P["red"]}" stroke-width="2" stroke-dasharray="6 4" '
            f'stroke-linecap="round" opacity="0.6"/>\n')
    svg += (f'  <polygon points="260,{flow_y + 50} 255,{flow_y + 60} 265,{flow_y + 60}" '
            f'fill="{P["red"]}" opacity="0.6"/>\n')
    svg += text(420, flow_y + 86, "/design --continue &lt;slug&gt;", cls="mono",
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

    card_y = out_y + 15
    svg += card(30,  card_y, 155, 130, P["green"],  "Knowledge",
                ["stored via git", "tagged: design-doc", "searchable"])
    svg += card(200, card_y, 155, 130, P["blue"],   "Gap analysis",
                ["kickoff plan", "files to modify", "tests needed"])
    svg += card(370, card_y, 155, 130, P["yellow"], "Single agent",
                ["kickoff run --doc", "validates ACs", "reports result"])
    svg += card(540, card_y, 140, 130, P["red"],    "Swarm build",
                ["swarm init --doc", "phased execution", "budget-aware"])

    # Arrow from validate down to output row
    svg += arrow_straight(cx, vy + 65, cx, out_y + 10, P["muted"], stroke_width=1.5, dashed=True)

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
