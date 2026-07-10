---
name: Social media cut
description: Turn the current edit into a short vertical 9:16 cut with a hook, captions-style title, and tight pacing.
---

# Social media cut

Goal: a short (15 to 60 s) vertical clip that works muted and hooks in the
first 2 seconds.

1. Call `describe_project` and note the main-track clips, total duration,
   and any music lanes.
2. Set the canvas vertical: `set_canvas` with aspect `9:16`. Keep the
   background dark unless the user asked otherwise.
3. Pick the strongest moment (the user's selection if any, otherwise the
   start of the longest main-track clip) and cut the timeline down around
   it: `trim_clip` / `split_clip` + `remove_clip` until the total is
   inside the target length. Prefer removing whole weak sections over
   shaving every clip.
4. Reframe for vertical: for each kept main-track clip, `set_clip_transform`
   with a scale that fills the 9:16 frame (media is usually 16:9, so scale
   up about 1.8 to 2.4 and keep position centered unless the subject is
   off-center).
5. Add a hook title in the first 2 seconds: `add_generated` with a short,
   punchy text (ask the user if no obvious hook exists) on a text track,
   duration about 2.5 s, then `set_clip_animation` with a pop or bounce `in`
   animation.
6. Tighten the pacing: if any kept clip runs longer than about 6 s without a
   cut, split it and either remove the slack or speed the middle up with
   `set_clip_speed` (1.5 to 2x).
7. If there is music, keep it under speech: `set_clip_audio` with volume
   about 0.4 on the music clip, fades of 0.3 s in and out.
8. Finish with `describe_project` and report the final duration and what
   was cut.
