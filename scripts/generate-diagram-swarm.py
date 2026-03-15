#!/usr/bin/env python3
"""
Generate a Forecast-styled diagram for swarm orchestration.

Shows: Design doc → Planner → Phases (with agents + gates) → Checkpoint

Usage:
    python3 scripts/generate-diagram-swarm.py -o docs_src/assets/img/swarm-flow.svg
"""

import argparse
import random
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
from brand import (P, svg_header, svg_footer, ellipse, circle, rrect, text,
                   arrow_curved, arrow_straight, confetti, pill, container,
                   write_svg)

WIDTH = 780
HEIGHT = 540
SEED = 23


def generate():
    rng = random.Random(SEED)
    svg = svg_header(WIDTH, HEIGHT)

    cx = WIDTH / 2

    # ── Title ─────────────────────────────────────────────────────────────
    svg += text(cx, 36, "Swarm orchestration", cls="heading", size=22, fill=P["black"])
    svg += text(cx, 56, "multi-agent phased builds from a design document",
                cls="subheading", size=14, fill=P["muted"])

    # ── Design doc input (left) ───────────────────────────────────────────
    dx, dy = 70, 170
    svg += rrect(dx - 45, dy - 50, 90, 100, P["pink"], rx=18, opacity=0.3)
    svg += rrect(dx - 41, dy - 46, 82, 92, P["white"], rx=16, opacity=0.9)
    svg += text(dx, dy - 14, "DESIGN.md", cls="mono", size=11, fill=P["red"], weight="bold")

    # Mini lines to suggest doc content
    for i in range(4):
        lw = 35 + rng.random() * 25
        svg += rrect(dx - 28, dy + 8 + i * 12, lw, 3, P["gray"], rx=1.5, opacity=0.6)

    # ── Arrow: doc → planner ──────────────────────────────────────────────
    svg += arrow_curved(120, 170, 170, 170, P["red"], stroke_width=2)

    # ── Planner ───────────────────────────────────────────────────────────
    svg += container(180, 125, 150, 90, P["green"], "Swarm planner")
    svg += pill(195, 160, 120, 24, P["green"], "budget-window", rx=12, label_cls="body")

    # ── Arrow: planner → phase 1 (enters top) ────────────────────────────
    svg += arrow_straight(330, 155, 370, 105, P["green"], stroke_width=2)

    # ── Phases (stacked vertically, shifted up) ───────────────────────────
    phase_x = 380
    phase_data = [
        ("Phase 1: core types",   P["blue"],   ["a1", "a2"],          "pass"),
        ("Phase 2: API layer",    P["red"],    ["a3", "a4", "a5"],    "pass"),
        ("Phase 3: integration",  P["yellow"], ["a6"],                "pending"),
    ]

    for i, (label, color, agents, gate) in enumerate(phase_data):
        py = 80 + i * 130
        pw, ph = 220, 100

        svg += rrect(phase_x, py, pw, ph, color, rx=18, opacity=0.1)
        svg += rrect(phase_x + 3, py + 3, pw - 6, ph - 6, P["white"], rx=16, opacity=0.85)
        svg += text(phase_x + pw / 2, py + 22, label, cls="subheading", size=13, fill=color)

        # Agent circles
        spacing = pw / (len(agents) + 1)
        for j, agent in enumerate(agents):
            acx = phase_x + spacing * (j + 1)
            acy = py + 50
            svg += circle(acx, acy, 14, color, opacity=0.15)
            svg += circle(acx, acy, 9, color, opacity=0.3)
            svg += text(acx, acy + 26, agent, cls="body", size=10, fill=P["muted"])

        # Gate at bottom
        gate_color = P["green"] if gate == "pass" else P["yellow"]
        svg += rrect(phase_x + 40, py + ph - 6, pw - 80, 5, gate_color, rx=2.5)

        # Arrow between phases (from gate)
        if i < 2:
            svg += arrow_straight(phase_x + pw / 2, py + ph, phase_x + pw / 2, py + ph + 24,
                                  P["muted"], stroke_width=1.5)
            svg += text(phase_x + pw / 2 + 16, py + ph + 16, "gate", cls="body",
                        size=10, fill=gate_color, anchor="start")

    # ── Checkpoint (below phases) ──────────────────────────────────────────
    last_phase_bottom = 80 + 2 * 130 + 100  # y=440
    chx, chy = phase_x + 110, last_phase_bottom + 20  # centered under phases
    svg += arrow_straight(phase_x + 110, last_phase_bottom - 1, chx, chy - 30,
                          P["muted"], stroke_width=1.5)

    # ── Complete badge (right side, vertically centered) ──────────────────
    bx, by = 680, 260
    svg += ellipse(bx, by, 60, 50, P["green"], opacity=0.1)
    svg += ellipse(bx, by, 50, 40, P["white"], opacity=0.85)
    svg += text(bx, by - 10, "Complete", cls="heading", size=15, fill=P["green"], weight="bold")
    svg += text(bx, by + 8, "all gates", cls="body", size=11, fill=P["muted"])
    svg += text(bx, by + 22, "passed", cls="body", size=11, fill=P["muted"])

    # Arrow from last gate to complete badge
    svg += arrow_curved(phase_x + 220, 80 + 2 * 130 + 50, bx - 55, by, P["green"], stroke_width=2)

    # ── Budget bar (bottom) ───────────────────────────────────────────────
    bar_y = 470
    svg += rrect(60, bar_y, 660, 45, P["gray"], rx=14, opacity=0.12)
    svg += rrect(63, bar_y + 3, 654, 39, P["white"], rx=12, opacity=0.85)
    svg += text(cx, bar_y - 10, "Budget-aware scheduling", cls="subheading",
                size=14, fill=P["muted"])

    # Segments
    svg += rrect(68, bar_y + 6, 200, 33, P["blue"], rx=10, opacity=0.2)
    svg += text(168, bar_y + 27, "phase 1: 42 min", cls="body", size=11, fill=P["blue"])
    svg += rrect(276, bar_y + 6, 240, 33, P["red"], rx=10, opacity=0.2)
    svg += text(396, bar_y + 27, "phase 2: 68 min", cls="body", size=11, fill=P["red"])
    svg += rrect(524, bar_y + 6, 140, 33, P["yellow"], rx=10, opacity=0.2)
    svg += text(594, bar_y + 27, "phase 3: ...", cls="body", size=11, fill=P["yellow"])

    # ── Confetti ──────────────────────────────────────────────────────────
    svg += confetti(rng, 640, 80, 60, 80, 5)
    svg += confetti(rng, 10, 400, 40, 80, 4)

    svg += svg_footer()
    return svg


def main():
    parser = argparse.ArgumentParser(description="Generate swarm orchestration diagram SVG")
    parser.add_argument("-o", "--output", help="Output file (default: stdout)")
    args = parser.parse_args()
    write_svg(generate(), args)


if __name__ == "__main__":
    main()
