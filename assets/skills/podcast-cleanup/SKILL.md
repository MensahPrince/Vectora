---
name: Podcast cleanup
description: Clean up a recorded conversation - denoise voices, level volumes, duck music, and add gentle fades.
---

# Podcast cleanup

Goal: a talk recording that sounds clean and even, without changing its
content.

1. Call `describe_project`. Identify voice clips (clips on audio lanes, or
   media clips whose audio carries the conversation) and any music clips.
2. Tag roles so later steps and the UI know what is what:
   `set_audio_role` with `voiceover` on each voice clip and `music` on
   music clips.
3. Denoise every voice clip: `set_denoise` with `denoise: true`. Do not
   denoise music — it smears.
4. Level the conversation: voice clips should sit at volume 1.0; if one
   speaker's clip is noticeably hot or quiet the user will say so — adjust
   only named clips with `set_clip_audio` (e.g. volume 0.8), never guess.
5. Duck music under speech: on each music clip that overlaps voice, set
   `set_clip_audio` volume to ~0.25 and add 0.5 s fades in and out.
6. Trim dead air at the ends: if the first or last clip starts or ends
   with obvious silence the user mentioned, `trim_clip` it tight, then add
   a 0.3 s fade in on the first audible clip and a 0.5 s fade out on the
   last (`set_clip_audio`).
7. Report what changed per clip in one short list.
