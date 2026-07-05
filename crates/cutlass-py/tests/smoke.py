#!/usr/bin/env python3
"""Smoke test for the cutlass v2 API (run after `maturin develop`)."""

from __future__ import annotations

from cutlass import Project, Solid, Text


def main() -> None:
    p = Project("smoke", fps=30, canvas="16:9", background=(10, 10, 20))

    bg = p.add_track("sticker", name="BG")
    titles = p.add_track("text", name="Titles")

    bg.add(Solid((40, 44, 72, 255)), start=0.0, duration=1.5)
    clip = titles.add(
        Text("smoke", size=96, color="#ffffff"),
        start=0.25,
        duration=1.0,
    )
    clip.animate(opacity=[(0.0, 0.0), (0.3, 1.0)])

    assert p.duration >= 1.5
    w, h = p.size
    assert w > 0 and h > 0

    frame = p.get_frame(0.5)
    assert frame.shape == (h, w, 4)
    assert frame.dtype.name == "uint8"
    assert frame.max() > 0

    assert len(p.tracks) == 2
    assert p.track("BG").name == "BG"
    assert len(list(p.track("Titles"))) == 1

    print("smoke ok:", p)


if __name__ == "__main__":
    main()
