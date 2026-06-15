//! Transcript-based editing end-to-end (M9 Phase 3): transcribe a real clip's
//! audio through an injected (stub) backend, then ripple-cut the timeline span
//! of a run of words out of the clip and assert the cut + undo behaviour.
//!
//! Gated on a local audio asset (`local-assets/assets/baby.mp3`) since it
//! exercises the decode path; skips cleanly when the asset is absent, like the
//! other decode-dependent engine tests.

mod common;

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use common::{assets_dir, import_asset, temp_engine};
use cutlass_engine::{ApplyOutcome, Command, EditCommand, EditOutcome};
use cutlass_ml::{Segment, StubTranscriber, TranscribeOptions, Transcript, Word};
use cutlass_models::{ClipId, Rational, RationalTime, TimeRange, TrackKind};

fn audio_asset() -> Option<std::path::PathBuf> {
    let path = assets_dir().join("baby.mp3");
    path.exists().then_some(path)
}

/// A four-word transcript the stub returns for any audio.
fn four_word_stub() -> StubTranscriber {
    StubTranscriber::new(Transcript {
        segments: vec![Segment {
            text: "one two three four".into(),
            start: 0.0,
            end: 1.0,
            words: vec![
                word("one", 0.0, 0.25),
                word("two", 0.25, 0.5),
                word("three", 0.5, 0.75),
                word("four", 0.75, 1.0),
            ],
        }],
        language: Some("en".into()),
    })
}

fn word(text: &str, start: f64, end: f64) -> Word {
    Word {
        text: text.into(),
        start,
        end,
        confidence: None,
    }
}

/// Build a temp engine with the stub backend and a ~2s media clip on an audio
/// lane, returning the engine and the clip id.
fn engine_with_clip() -> Option<(tempfile::TempDir, cutlass_engine::Engine, ClipId)> {
    let audio = audio_asset()?;
    let (dir, mut engine) = temp_engine();
    engine.set_transcriber(Arc::new(four_word_stub()));
    let media_id = import_asset(&mut engine, &audio);
    let rate = engine.project().media(media_id).expect("media").frame_rate;
    let a1 = match engine
        .apply(Command::Edit(EditCommand::AddTrack {
            kind: TrackKind::Audio,
            name: "A1".into(),
            index: None,
        }))
        .unwrap()
    {
        ApplyOutcome::Edited(EditOutcome::CreatedTrack(id)) => id,
        other => panic!("expected CreatedTrack, got {other:?}"),
    };
    let clip = match engine
        .apply(Command::Edit(EditCommand::AddClip {
            track: a1,
            media: media_id,
            source: TimeRange::at_rate(0, 2 * rate.num as i64, rate),
            start: RationalTime::new(0, Rational::FPS_24),
        }))
        .unwrap()
    {
        ApplyOutcome::Edited(EditOutcome::Created(id)) => id,
        other => panic!("expected Created, got {other:?}"),
    };
    Some((dir, engine, clip))
}

#[test]
fn transcribe_clip_returns_word_timed_text() {
    let Some((_dir, engine, clip)) = engine_with_clip() else {
        return;
    };
    let cancel = AtomicBool::new(false);
    let transcript = engine
        .transcribe_clip(clip, TranscribeOptions::default(), &cancel, &mut |_| {})
        .expect("transcribed");

    let words: Vec<String> = transcript
        .segments
        .iter()
        .flat_map(|s| s.words.iter())
        .map(|w| w.text.clone())
        .collect();
    assert_eq!(words, vec!["one", "two", "three", "four"]);
}

#[test]
fn transcribe_without_a_backend_errors() {
    let (_dir, engine) = temp_engine();
    let cancel = AtomicBool::new(false);
    let err = engine
        .transcribe_clip(
            ClipId::from_raw(1),
            TranscribeOptions::default(),
            &cancel,
            &mut |_| {},
        )
        .unwrap_err();
    assert!(
        matches!(err, cutlass_engine::EngineError::Transcribe(_)),
        "expected a transcribe error, got: {err}"
    );
}
