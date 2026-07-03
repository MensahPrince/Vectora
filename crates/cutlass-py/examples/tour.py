#!/usr/bin/env python3
"""
End-to-end tour mirroring api-design.md.

Requires sample media on disk; paths below are placeholders — edit before running.
"""

from __future__ import annotations

from cutlass import Project, Solid, Text

# --- configure media paths ---------------------------------------------------
BEACH = "footage/beach.mp4"
DRONE = "footage/drone.mp4"
MUSIC = "audio/theme.mp3"
OUT = "trailer.mp4"


def main() -> None:
    p = Project("trailer", fps=30, canvas="16:9", background="#101018")

    beach = p.import_media(BEACH)
    drone = p.import_media(DRONE)
    music = p.import_media(MUSIC)

    main_v = p.add_track("video", name="Main")
    titles = p.add_track("text", name="Titles")
    score = p.add_track("audio", name="Music")
    stickers = p.add_track("sticker", name="Badge")

    a = main_v.add(beach.subclip(3.0, 8.0), start=0.0)
    b = main_v.append(drone.subclip(10.0, 14.0))
    a.transition("crossfade", duration=0.8)

    badge = stickers.add(Solid("#202840"), start=0.5, duration=3.0)
    badge.scale = 0.25
    badge.position = (0.35, -0.35)
    badge.animate(opacity=[(0.0, 0.0), (0.4, 1.0)], easing="ease_out")

    titles.add(
        Text("BIG WAVES", size=140, color="#ffffff", bold=True),
        start=1.0,
        duration=3.0,
    )

    bed = score.add(music.subclip(0.0, 9.0), start=0.0)
    bed.volume = 0.6
    bed.fade_out = 1.5

    for track in p.tracks:
        print(track.name, [c.start for c in track])

    _ = p.get_frame(2.0)
    p.export(OUT)
    p.save("trailer.cutlass")
    print("wrote", OUT)


if __name__ == "__main__":
    main()
