#!/usr/bin/env python3
"""
Generate a Forecast-styled diagram for the crosslink kickoff flow.

Shows: Human → /kickoff → [branch, worktree, agent] → Agent works → Results

Usage:
    python3 scripts/generate-diagram-kickoff.py -o docs_src/assets/img/kickoff-flow.svg
"""

import argparse
import random
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
from brand import (P, svg_header, svg_footer, ellipse, circle, rrect, text,
                   arrow_curved, arrow_straight, confetti, pill, card, container,
                   write_svg)

WIDTH = 900
HEIGHT = 580
SEED = 7


def generate():
    rng = random.Random(SEED)
    svg = svg_header(WIDTH, HEIGHT)

    # ── Title ─────────────────────────────────────────────────────────────
    svg += text(WIDTH / 2, 38, "Kickoff agent lifecycle", cls="heading", size=22, fill=P["black"])
    svg += text(WIDTH / 2, 58, "from instruction to autonomous implementation",
                cls="subheading", size=14, fill=P["muted"])

    # ── Phase 1: Human (left) ─────────────────────────────────────────────
    hx, hy = 100, 170
    svg += ellipse(hx, hy, 80, 60, P["pink"], opacity=0.4)
    svg += ellipse(hx, hy, 68, 48, P["pink"], opacity=0.25)
    svg += text(hx, hy - 14, "Human", cls="subheading", size=18, fill=P["black"])
    svg += text(hx, hy + 8, '"fix the auth bug"', cls="body", size=12, fill=P["muted"])
    svg += text(hx, hy + 26, "high-level instruction", cls="body", size=12, fill=P["muted"])

    # ── Arrow: Human → Kickoff ────────────────────────────────────────────
    svg += arrow_curved(185, 170, 250, 170, P["red"], stroke_width=2.5)
    svg += text(218, 158, "/kickoff", cls="mono", size=13, fill=P["red"], weight="bold")

    # ── Phase 2: Crosslink orchestration (center) ─────────────────────────
    ox, oy, ow, oh = 260, 100, 220, 145
    svg += rrect(ox, oy, ow, oh, P["green"], rx=28, opacity=0.12)
    svg += rrect(ox + 4, oy + 4, ow - 8, oh - 8, P["white"], rx=26, opacity=0.85)
    svg += text(ox + ow / 2, oy + 30, "Crosslink orchestrates", cls="subheading",
                size=16, fill=P["black"])

    # Sub-step pills — 2x2 grid, properly centered
    pill_w, pill_h = 95, 26
    col1 = ox + 14
    col2 = ox + ow / 2 + 3
    row1 = oy + 46
    row2 = oy + 80
    svg += pill(col1, row1, pill_w, pill_h, P["blue"],   "git branch", rx=13)
    svg += pill(col1, row2, pill_w, pill_h, P["yellow"], "git worktree", rx=13)
    svg += pill(col2, row1, pill_w, pill_h, P["red"],    "agent init", rx=13)
    svg += pill(col2, row2, pill_w, pill_h, P["green"],  "issue + session", rx=13, label_cls="body")

    # ── Arrow: Orchestration → Agent ──────────────────────────────────────
    svg += arrow_curved(485, 170, 545, 170, P["green"], stroke_width=2.5)
    svg += text(515, 158, "launch", cls="body", size=13, fill=P["green"])

    # ── Phase 3: Autonomous agent (right) ─────────────────────────────────
    ax, ay = 690, 170
    svg += ellipse(ax, ay, 140, 85, P["blue"], opacity=0.1)
    svg += ellipse(ax, ay, 128, 73, P["white"], opacity=0.8)
    svg += text(ax, ay - 38, "Autonomous agent", cls="subheading", size=16, fill=P["blue"])

    for i, (act, is_code) in enumerate([
        ("explore codebase", False), ("implement feature", False),
        ("run tests + lint", False), ("/commit", True), ("self-review", False),
    ]):
        yy = ay - 16 + i * 18
        svg += circle(ax - 70, yy - 4, 4, P["blue"])
        cls = "mono" if is_code else "body"
        svg += text(ax - 58, yy, act, cls=cls, size=12, fill=P["text"], anchor="start")

    # Loop arrow (solid)
    svg += (f'  <path d="M {ax + 90} {ay - 35} A 45 65 0 1 1 {ax + 90} {ay + 48}" '
            f'fill="none" stroke="{P["blue"]}" stroke-width="1.5" opacity="0.5"/>\n')
    svg += text(ax + 126, ay + 10, "iterate", cls="body", size=11, fill=P["blue"])

    # ── Phase 4: Results (bottom) ─────────────────────────────────────────
    ry = 330
    svg += rrect(50, ry, 800, 220, P["gray"], rx=28, opacity=0.12)
    svg += rrect(53, ry + 3, 794, 214, P["white"], rx=26, opacity=0.85)
    svg += text(WIDTH / 2, ry + 30, "Outputs", cls="heading", size=18, fill=P["black"])

    cards = [
        (70,  175, P["green"],  "Feature branch",
         [("committed code", False), ("tests passing", False), ("clean lint", False)]),
        (265, 175, P["yellow"], "Crosslink trail",
         [("issue comments", False), ("breadcrumbs", False), ("handoff notes", False)]),
        (460, 175, P["blue"],   "Kickoff report",
         [("spec validation", False), ("phase timings", False), ("verdict", False)]),
        (655, 175, P["red"],    "Ready for review",
         [("draft PR", False), ("self-review done", False), ("status: DONE", True)]),
    ]
    card_top = ry + 46
    for cx_, cw, color, title, items in cards:
        svg += rrect(cx_, card_top, cw, 140, color, rx=18, opacity=0.08)
        svg += text(cx_ + cw / 2, card_top + 20, title,
                    cls="subheading", size=15, fill=color, weight="bold")
        for j, (item, is_cmd) in enumerate(items):
            iy = card_top + 45 + j * 24
            svg += circle(cx_ + 18, iy - 3, 4, color, opacity=0.5)
            svg += text(cx_ + 32, iy, item,
                        cls="mono" if is_cmd else "body",
                        size=11 if is_cmd else 13,
                        fill=P["text"], anchor="start")

    # Arrows from agent/orchestration → results
    svg += arrow_straight(690, 255, 690, ry + 40, P["blue"], stroke_width=2)
    svg += arrow_straight(370, 248, 370, ry + 40, P["green"], stroke_width=2)

    # ── Confetti ──────────────────────────────────────────────────────────
    svg += confetti(rng, 15, 80, 60, 80, 6)
    svg += confetti(rng, 820, 80, 60, 80, 6)

    svg += svg_footer()
    return svg


def main():
    parser = argparse.ArgumentParser(description="Generate kickoff flow diagram SVG")
    parser.add_argument("-o", "--output", help="Output file (default: stdout)")
    args = parser.parse_args()
    write_svg(generate(), args)


if __name__ == "__main__":
    main()
