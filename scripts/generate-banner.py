#!/usr/bin/env python3
"""
Generate the crosslink banner image as an SVG using Forecast brand primitives.

Concept: Interconnected AI agents as layered organic shapes in the Forecast
visual language — overlapping ellipses, circles, crescents, and rounded
rectangles with scattered dot accents suggesting data flow between agents.

Usage:
    python3 scripts/generate-banner.py                    # SVG to stdout
    python3 scripts/generate-banner.py -o images/banner.svg
    python3 scripts/generate-banner.py --png -o images/banner.png  # requires cairosvg
"""

import argparse
import math
import random
import sys

# ── Forecast Brand Palette ──────────────────────────────────────────────────
PALETTE = {
    "red":    "#F95838",
    "green":  "#007C35",
    "blue":   "#00A6DB",
    "yellow": "#FFCE02",
    "pink":   "#FFB6C6",
    "bg":     "#F9F4F5",
    "white":  "#FFFFFF",
    "black":  "#000000",
}

# Extended palette with translucent variants for dot motifs
DOT_COLORS = [
    PALETTE["red"],
    PALETTE["green"],
    PALETTE["blue"],
    PALETTE["yellow"],
    PALETTE["pink"],
    "rgba(0,0,0,0.08)",   # ghost dots
    "rgba(0,0,0,0.05)",
]

WIDTH = 1500
HEIGHT = 500

# Seed for reproducibility
SEED = 42


def svg_header():
    return f"""<?xml version="1.0" encoding="UTF-8"?>
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {WIDTH} {HEIGHT}" width="{WIDTH}" height="{HEIGHT}">
  <defs>
    <!-- Soft clip for clean edges -->
    <clipPath id="banner-clip">
      <rect width="{WIDTH}" height="{HEIGHT}" rx="0"/>
    </clipPath>
  </defs>
  <g clip-path="url(#banner-clip)">
    <!-- Background -->
    <rect width="{WIDTH}" height="{HEIGHT}" fill="{PALETTE['bg']}"/>
"""


def svg_footer():
    return """  </g>
</svg>
"""


def ellipse(cx, cy, rx, ry, fill, opacity=1.0, rotate=0):
    transform = f' transform="rotate({rotate} {cx} {cy})"' if rotate else ""
    op = f' opacity="{opacity}"' if opacity < 1.0 else ""
    return f'    <ellipse cx="{cx}" cy="{cy}" rx="{rx}" ry="{ry}" fill="{fill}"{op}{transform}/>\n'


def circle(cx, cy, r, fill, opacity=1.0):
    op = f' opacity="{opacity}"' if opacity < 1.0 else ""
    return f'    <circle cx="{cx}" cy="{cy}" r="{r}" fill="{fill}"{op}/>\n'


def rounded_rect(x, y, w, h, fill, rx=None, opacity=1.0, rotate=0):
    if rx is None:
        rx = min(w, h) * 0.4
    op = f' opacity="{opacity}"' if opacity < 1.0 else ""
    transform = f' transform="rotate({rotate} {x + w/2} {y + h/2})"' if rotate else ""
    return f'    <rect x="{x}" y="{y}" width="{w}" height="{h}" rx="{rx}" fill="{fill}"{op}{transform}/>\n'


def crescent(cx, cy, r, fill, opacity=1.0, rotate=0):
    """A crescent/C-shape made from two overlapping circles (path difference)."""
    # Outer arc
    outer_r = r
    inner_r = r * 0.7
    offset = r * 0.35

    # Build a crescent using arc paths
    # Start at top of outer circle, arc right, then inner arc back
    svg = f'    <g transform="rotate({rotate} {cx} {cy})"'
    if opacity < 1.0:
        svg += f' opacity="{opacity}"'
    svg += '>\n'

    # Use a mask approach: draw outer circle, cut with offset inner circle
    mask_id = f"crescent-{cx:.0f}-{cy:.0f}-{rotate:.0f}"
    svg += f'      <mask id="{mask_id}">\n'
    svg += f'        <rect x="{cx - outer_r - 10}" y="{cy - outer_r - 10}" '
    svg += f'width="{outer_r * 2 + 20}" height="{outer_r * 2 + 20}" fill="white"/>\n'
    svg += f'        <circle cx="{cx + offset}" cy="{cy}" r="{inner_r}" fill="black"/>\n'
    svg += f'      </mask>\n'
    svg += f'      <circle cx="{cx}" cy="{cy}" r="{outer_r}" fill="{fill}" mask="url(#{mask_id})"/>\n'
    svg += '    </g>\n'
    return svg


def triangle(cx, cy, size, fill, opacity=1.0, rotate=0):
    """An equilateral-ish triangle (like the Forecast brand triangles)."""
    h = size * math.sqrt(3) / 2
    points = [
        (cx, cy - h * 0.6),
        (cx - size / 2, cy + h * 0.4),
        (cx + size / 2, cy + h * 0.4),
    ]
    pts = " ".join(f"{x:.1f},{y:.1f}" for x, y in points)
    op = f' opacity="{opacity}"' if opacity < 1.0 else ""
    transform = f' transform="rotate({rotate} {cx} {cy})"' if rotate else ""
    return f'    <polygon points="{pts}" fill="{fill}"{op}{transform}/>\n'


def scattered_dots(rng, region_x, region_y, region_w, region_h, count, r_min=2, r_max=6):
    """Generate scattered dot motif in a region — the Forecast signature pattern."""
    svg = ""
    for _ in range(count):
        x = region_x + rng.random() * region_w
        y = region_y + rng.random() * region_h
        r = r_min + rng.random() * (r_max - r_min)
        color = rng.choice(DOT_COLORS)
        svg += circle(x, y, r, color)
    return svg


def generate_banner():
    """Compose the banner from Forecast brand primitives.

    Layout concept — three "agent clusters" across the banner connected
    by dot-flow paths, with a large central composition:

    [Agent cluster 1]  ···dots···  [Central hub]  ···dots···  [Agent cluster 2]
         left                        center                       right
    """
    rng = random.Random(SEED)
    svg = svg_header()

    # ── Layer 1: Large background shapes (depth) ────────────────────────

    # Large pink ellipse — center-right, the dominant "hub" shape
    svg += ellipse(820, 260, 260, 200, PALETTE["pink"], opacity=0.7)

    # Golden yellow rounded rect — upper right, tilted
    svg += rounded_rect(1050, 30, 320, 160, PALETTE["yellow"], rx=70, opacity=0.8, rotate=8)

    # Blue ellipse — left side, partially off-canvas
    svg += ellipse(180, 320, 200, 160, PALETTE["blue"], opacity=0.5)

    # ── Layer 2: Medium agent shapes ────────────────────────────────────

    # Agent 1 (left): green circle with red accent dot
    svg += circle(250, 200, 90, PALETTE["green"], opacity=0.85)
    svg += circle(310, 135, 18, PALETTE["red"])

    # Agent 2 (center-left): pink ellipse with blue crescent
    svg += ellipse(530, 280, 100, 75, PALETTE["pink"], opacity=0.9)
    svg += crescent(490, 250, 50, PALETTE["blue"], opacity=0.8, rotate=-30)

    # Central hub: large overlapping composition
    # — green rounded rect as base
    svg += rounded_rect(700, 150, 200, 220, PALETTE["green"], rx=60, opacity=0.75)
    # — yellow circle overlapping
    svg += circle(870, 200, 70, PALETTE["yellow"], opacity=0.85)
    # — small red circle accent
    svg += circle(780, 170, 22, PALETTE["red"])
    # — blue triangle pointing right (suggesting flow/direction)
    svg += triangle(900, 310, 80, PALETTE["blue"], opacity=0.75, rotate=90)

    # Agent 3 (right): yellow ellipse with green triangle
    svg += ellipse(1150, 300, 110, 80, PALETTE["yellow"], opacity=0.8)
    svg += triangle(1200, 260, 55, PALETTE["green"], opacity=0.9, rotate=15)

    # Agent 4 (far right): pink circle cluster
    svg += circle(1350, 180, 75, PALETTE["pink"], opacity=0.8)
    svg += circle(1380, 230, 30, PALETTE["blue"], opacity=0.7)
    svg += circle(1310, 150, 20, PALETTE["red"], opacity=0.9)

    # ── Layer 3: Crescent connectors ────────────────────────────────────
    # Crescents between agent clusters suggesting communication/links

    svg += crescent(400, 180, 45, PALETTE["green"], opacity=0.6, rotate=45)
    svg += crescent(1020, 250, 55, PALETTE["red"], opacity=0.5, rotate=-20)
    svg += crescent(1280, 350, 40, PALETTE["green"], opacity=0.5, rotate=120)

    # ── Layer 4: Scattered dot flows ────────────────────────────────────
    # Dots flowing between agent clusters — the "data/memory" layer

    # Flow: left cluster → center
    svg += scattered_dots(rng, 320, 150, 250, 150, count=25, r_min=2, r_max=5)

    # Flow: center → right cluster
    svg += scattered_dots(rng, 920, 200, 250, 150, count=25, r_min=2, r_max=5)

    # Sparse ambient dots across the full banner
    svg += scattered_dots(rng, 50, 30, 1400, 440, count=40, r_min=1.5, r_max=4)

    # ── Layer 5: Small accent dots on top ───────────────────────────────
    # Bright, opaque accent dots that pop

    accent_positions = [
        (160, 380, 8, PALETTE["yellow"]),
        (450, 120, 6, PALETTE["red"]),
        (650, 400, 10, PALETTE["blue"]),
        (950, 130, 7, PALETTE["yellow"]),
        (1100, 420, 9, PALETTE["green"]),
        (1400, 350, 6, PALETTE["red"]),
        (80, 100, 5, PALETTE["pink"]),
        (1450, 80, 8, PALETTE["green"]),
    ]
    for x, y, r, color in accent_positions:
        svg += circle(x, y, r, color)

    # ── Forecast logo mark (top-left) ───────────────────────────────────
    # The quarter-circle: pink background wedge + green triangle
    svg += f'    <!-- Forecast logo mark -->\n'
    svg += f'    <path d="M 30 30 L 30 65 A 35 35 0 0 0 65 30 Z" fill="{PALETTE["pink"]}" opacity="0.6"/>\n'
    svg += triangle(44, 48, 22, PALETTE["green"], opacity=0.9, rotate=225)

    svg += svg_footer()
    return svg


def main():
    parser = argparse.ArgumentParser(description="Generate crosslink banner SVG")
    parser.add_argument("-o", "--output", help="Output file (default: stdout)")
    parser.add_argument("--png", action="store_true", help="Convert to PNG (requires cairosvg)")
    args = parser.parse_args()

    svg_content = generate_banner()

    if args.png:
        try:
            import cairosvg
        except ImportError:
            print("Error: pip install cairosvg for PNG output", file=sys.stderr)
            sys.exit(1)
        png_data = cairosvg.svg2png(bytestring=svg_content.encode(), output_width=WIDTH * 2)
        if args.output:
            with open(args.output, "wb") as f:
                f.write(png_data)
        else:
            sys.stdout.buffer.write(png_data)
    else:
        if args.output:
            with open(args.output, "w") as f:
                f.write(svg_content)
            print(f"Written: {args.output}", file=sys.stderr)
        else:
            print(svg_content)


if __name__ == "__main__":
    main()
