---
name: Highlights reel
description: Build a fast montage from the timeline's best moments with markers, crossfades, and an end title.
---

# Highlights reel

Goal: a montage of short punchy moments from the material already on the
timeline.

1. Call `describe_project`. If the user marked moments (markers exist),
   those are the highlights; otherwise ask which moments to keep or use
   their selection.
2. For each highlight, isolate a 2 to 5 s segment: `split_clip` at both edges
   of the moment, then `remove_clip` on everything between highlights and
   close gaps track by track with `shift_clips` (or use `ripple_delete`
   on the clips being dropped so later clips close up automatically).
3. Keep the reel moving: no kept segment should exceed about 5 s; split and
   drop the middle of anything longer.
4. Add crossfades between neighboring segments on the main track:
   `add_transition` with a crossfade after each clip except the last,
   then `set_transition` to 0.4 s.
5. Optional energy pass: speed up flat-feeling segments with
   `set_clip_speed` at 1.25 to 1.5x; never speed up speech the viewer must
   follow.
6. End card: `add_generated` text (the user's title, or the project name)
   on a text track over the last 2 s, with a fade `in` animation
   (`set_clip_animation`).
7. Finish with `describe_project` and report the reel's final duration and
   the segments kept.
