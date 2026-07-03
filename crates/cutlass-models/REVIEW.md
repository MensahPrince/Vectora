# Review: `cutlass-models` (template system + changes vs. main)

- **Date:** 2026-07-03
- **Branch:** `mobile-support` at `dbcd1de`
- **Scope:** Full diff against `main`, with a code review focused on the new CapCut-style
  template system (`src/template.rs`, `Replaceable`/`SlotMedia` in `src/clip.rs`, new `Project`
  methods in `src/project.rs`, `tests/template.rs`).

## What changed vs. main

Unlike the decoder crate, this crate was evolved, not replaced:

- **`time.rs` reduced from 539 lines to a re-export.** `Rational` / `RationalTime` / `TimeRange`
  now live in `cutlass-core`, shared by decoder, compositor, and models, with a
  `From<TimeError> for ModelError` bridge keeping the model's error type stable. The right call:
  on main the decoder and the model each had their own rational-time universe.
- **New template system:** `Template` / `TemplateMeta` / `Pick` (`src/template.rs`),
  `Replaceable` + `SlotMedia` markers on `Clip`, `Project::{set_clip_media, set_replaceable,
  set_text_editable}`, `TemplateId`, three new error variants, `.cutlasst` file save/load, and a
  308-line integration test.
- Serde hygiene: new `Clip` fields use `skip_serializing_if` defaults, so pre-existing `.cutlass`
  project files remain byte-identical.

## Findings

1. **`Replaceable.max_duration` is dead code** (`src/clip.rs`, field near line 912). Defined,
   documented as CapCut's per-clip duration cap, serialized, and given a builder — but never read
   by `apply` / `slot_source`. Since slot durations are locked, its semantics are also undefined.
   Enforce it or drop it before the file format freezes it.
2. **`ModelError::NotReplaceable` is never constructed** (`src/error.rs:64`). Presumably for a
   fill-by-id API that didn't materialize. Wire it up or remove it.
3. **`set_replaceable` validates nothing, so authoring mistakes surface late with the wrong
   error** (`src/project.rs`, ~line 301). A text-generator or effect clip can be marked as a
   visual slot; it appears in `slots()`, and failure only shows at `apply` time as
   `IncompatibleTrackKind` (pointing at the track, not the authoring mistake). Contrast
   `set_text_editable`, which validates eagerly. Suggested: validate `accepts` against the clip's
   content / track kind at mark time.
4. **`Template::load_from_file` checks the schema in the wrong order and has no migration path**
   (`src/template.rs`, ~line 313). `persist.rs` deliberately validates the version and migrates
   *before* the strict typed parse; the template loader parses first and checks after. A template
   from a future version whose shape changed fails as a confusing `InvalidProjectFile` instead of
   `UnsupportedProjectSchema`, and older template files won't migrate even though the embedded
   `Project` shares `PROJECT_SCHEMA_VERSION`. Align with the project loader before `.cutlasst`
   files exist in the wild.
5. **`slot_source` handles constant `speed` but ignores `speed_curve` and `reversed`**
   (`src/template.rs`, ~line 335). A speed-ramped or reversed slot gets a mis-sized source window
   (`speed_curve_integral` already exists for the correct math). Also `resample` rounds to
   nearest, so a mismatched-rate fill can under-cover the slot by up to half a source frame.
   For v1, rejecting picks into curved/reversed slots beats silently mis-windowing them.

### Minor

- Extra picks beyond the slot count are silently ignored by the `zip` in `apply`. Fine for a UI;
  the AI agent is also a caller, and an error would catch its off-by-one bugs.
- `slot_count()` re-implements `slots()`'s filter instead of reusing it (drift risk; it does skip
  the sort, but this is a cold path).
- Applied projects keep their `replaceable` / `text_editable` markers. The music-swap test relies
  on this and it matches CapCut's re-editable-after-fill behavior — but it means "the result is
  an ordinary `Project`" isn't quite true (saved outputs carry template chrome). If intentional,
  document it on `apply`.
- `Clip::end()` became `Ok(self.timeline.end()?)` — a no-op wrapper existing only to route
  `TimeError` through the bridge; `map_err(Into::into)` would say so more directly.

## Strengths

- The core design — deriving slots / editable texts / music by scanning clip markers instead of
  keeping a separate manifest — eliminates manifest drift as a bug class.
- `apply` clones and re-IDs the project, so templates are reusable and never aliased; unfilled
  slots keep sample media (matches CapCut preview semantics).
- Deterministic slot ordering (order, then clip-ID tie-break), tested with slots authored out of
  order on purpose.
- `set_clip_media` is the single validated primitive under both slot-filling and music swap.
- The integration test proves locked-look preservation with a non-identity transform and a real
  effect, not just structural equality; type-mismatch, too-short-media, in-point windowing,
  file roundtrip, and non-template refusal are all covered.

## Verdict

Well-designed and well-tested. Items 1–4 are cheap to fix now and expensive after `.cutlasst`
files ship; item 5 needs either enforcement or an explicit guard.

## Addendum (2026-07-03): all findings resolved

Same-day follow-up; every fix landed before any `.cutlasst` file exists in the wild.

- **Finding 1 (`max_duration` dead code): dropped.** Slot durations are locked, so a per-fill
  source cap has no defined semantics; the field, its builder, and its serialization are gone
  rather than frozen into the format. (Recoverable later as an additive optional field if a
  real consumer appears.)
- **Finding 2 (`NotReplaceable` never constructed): removed.** The fill-by-id API it
  anticipated never materialized; `apply` fills by order and errors carry the slot id.
- **Finding 3 (`set_replaceable` validated nothing): fixed.** Marking now validates eagerly,
  mirroring `set_text_editable`: the clip must be a media clip (generated content cannot be
  refilled), and `accepts` must match the lane — visual restrictions on video tracks,
  `AudioOnly` on audio tracks — surfacing authoring mistakes as `InvalidParam` at mark time
  instead of `IncompatibleTrackKind` at apply time. Unmarking never fails for known clips.
  Test: `set_replaceable_validates_at_mark_time`.
- **Finding 4 (template loader parse-then-check, no migration): fixed.** `load_from_file` now
  mirrors the project loader's order: read the schema from the raw document, refuse wrong
  kinds and future versions as `UnsupportedProjectSchema` *before* the strict typed parse,
  then run `persist.rs`'s migration chain over the embedded project document (`.cutlasst`
  versions in lockstep with `PROJECT_SCHEMA_VERSION`) and keep the file's version as
  provenance. `read_schema`/`migrate_document` are shared (`pub(crate)`) rather than
  reimplemented. Tests: `load_refuses_future_version_before_parsing` (garbage body, v99 —
  proves the ordering), `load_migrates_older_template_versions`, and the tightened
  `load_rejects_non_template_document`.
- **Finding 5 (`slot_source` vs speed curves / reverse / rounding): fixed.** Speed-ramped or
  reversed slots are refused with the new `SlotRetimeUnsupported` error (v1 policy: reject
  beats mis-windowing; sizing them needs the curve integral). The duration conversion is now
  a single exact i128 rational **ceiling** (was truncate-then-round-nearest), so a
  mismatched-rate fill can never under-cover its slot by the old half-frame. Tests:
  `apply_refuses_reversed_and_speed_ramped_slots`, `cross_rate_fill_rounds_source_window_up`
  (24 fps slot, 30 fps media: 31.25 ticks must become 32, and 31-frame media is refused).

Minor items: extra picks are now refused (`TooManyPicks { given, slots }` — catches agent
off-by-ones; fewer picks still previews with sample media; test `apply_rejects_extra_picks`);
`slot_count()` is defined as `slots().len()` (one definition of "slot"); marker retention
after `apply` is documented on the method (CapCut re-editable-after-fill semantics —
intentional, results carry template chrome); `Clip::end()` uses `map_err(Into::into)`.
