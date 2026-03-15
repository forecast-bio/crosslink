#!/usr/bin/env python3
"""
Generate a Forecast-styled diagram for multi-agent coordination.

Usage:
    python3 scripts/generate-diagram-multi-agent.py -o docs_src/assets/img/multi-agent-flow.svg
"""

import argparse
import random
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
from brand import (P, svg_header, svg_footer, ellipse, circle, rrect, text,
                   arrow_straight, confetti, pill, container, write_svg)

WIDTH = 680
HEIGHT = 520
SEED = 19


def agent_node(x, y, name, color, issue):
    """Draw an agent as a colored ellipse with identity and locked issue."""
    svg = ellipse(x, y, 75, 45, color, opacity=0.12)
    svg += ellipse(x, y, 65, 37, P["white"], opacity=0.85)
    svg += text(x, y - 8, name, cls="mono", size=12, fill=color, weight="bold")
    svg += text(x, y + 12, f"working #{issue}", cls="body", size=11, fill=P["muted"])
    return svg


def generate():
    rng = random.Random(SEED)
    svg = svg_header(WIDTH, HEIGHT)

    cx = WIDTH / 2

    # ── Title ─────────────────────────────────────────────────────────────
    svg += text(cx, 36, "Multi-agent coordination", cls="heading", size=22, fill=P["black"])
    svg += text(cx, 56, "distributed locking via crosslink/hub",
                cls="subheading", size=14, fill=P["muted"])

    # ── Three agent nodes (top row) ───────────────────────────────────────
    agents = [
        (130,  130, "agent-frontend", P["blue"],  12),
        (340,  130, "agent-backend",  P["green"], 15),
        (550,  130, "agent-infra",    P["red"],   18),
    ]
    for ax, ay, name, color, issue in agents:
        svg += agent_node(ax, ay, name, color, issue)

    # ── Coordination branch (center hub) ──────────────────────────────────
    hub_y = 270
    # Draw container shell without title, then add black title manually
    svg += rrect(80, hub_y - 40, 520, 120, P["yellow"], rx=30, opacity=0.12)
    svg += rrect(84, hub_y - 36, 512, 112, P["white"], rx=28, opacity=0.85)
    svg += text(340, hub_y - 10, "crosslink/hub branch",
                cls="subheading", size=18, fill=P["black"])

    # Lock pills inside the hub (squeezed to fit without overflow)
    for lx, lw, color, label in [
        (100, 140, P["blue"],  "#12 → frontend"),
        (255, 140, P["green"], "#15 → backend"),
        (410, 140, P["red"],   "#18 → infra"),
    ]:
        svg += pill(lx, hub_y, lw, 28, color, label, rx=14)

    # ── Arrows: agents → hub (solid) ─────────────────────────────────────
    for ax, color in [(130, P["blue"]), (340, P["green"]), (550, P["red"])]:
        target_x = min(max(ax, 150), 530)
        svg += arrow_straight(ax, 175, target_x, hub_y - 45,
                              color, stroke_width=1.5)

    # ── Sync label ────────────────────────────────────────────────────────
    svg += text(cx, hub_y + 55, "sync via git push/pull to coordination branch",
                cls="body", size=12, fill=P["muted"])

    # ── Daemon indicator (sits outside the hub box) ──────────────────────
    svg += rrect(380, hub_y + 90, 220, 28, P["green"], rx=14, opacity=0.12)
    svg += circle(394, hub_y + 104, 4, P["green"])
    svg += text(490, hub_y + 108, "daemon: auto-sync + heartbeat",
                cls="body", size=11, fill=P["green"])

    # ── Bottom: result summary ────────────────────────────────────────────
    svg += rrect(40, 400, WIDTH - 80, 95, P["gray"], rx=20)
    svg += text(cx, 428, "Agents self-coordinate — no manual lock management",
                cls="heading", size=15, fill=P["black"])

    features = [
        ("lock before work",      P["green"]),
        ("heartbeat monitoring",  P["blue"]),
        ("stale lock recovery",   P["yellow"]),
        ("conflict-free merges",  P["red"]),
    ]
    for i, (label, color) in enumerate(features):
        fx = 80 + i * 145
        svg += circle(fx, 455, 4, color)
        svg += text(fx + 12, 459, label, cls="body", size=11, fill=P["text"], anchor="start")

    # Second row of capabilities
    extras = [
        ("machine identity",   P["green"]),
        ("automatic retry",    P["blue"]),
    ]
    for i, (label, color) in enumerate(extras):
        fx = 80 + i * 145
        svg += circle(fx, 477, 4, color)
        svg += text(fx + 12, 481, label, cls="body", size=11, fill=P["text"], anchor="start")

    # ── Confetti ──────────────────────────────────────────────────────────
    svg += confetti(rng, 10, 80, 50, 80, 5)
    svg += confetti(rng, 620, 80, 50, 80, 5)

    svg += svg_footer()
    return svg


def main():
    parser = argparse.ArgumentParser(description="Generate multi-agent coordination diagram SVG")
    parser.add_argument("-o", "--output", help="Output file (default: stdout)")
    args = parser.parse_args()
    write_svg(generate(), args)


if __name__ == "__main__":
    main()
