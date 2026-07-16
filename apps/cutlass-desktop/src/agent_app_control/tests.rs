use std::collections::{BTreeMap, BTreeSet};
use std::fs;

use super::*;

#[test]
fn registry_has_exact_unique_names_tiers_and_object_schemas() {
    let registry = specs();
    let names: Vec<_> = registry.iter().map(|entry| entry.name.as_str()).collect();
    assert_eq!(names, TOOL_NAMES);
    assert_eq!(
        names.iter().copied().collect::<BTreeSet<_>>().len(),
        TOOL_NAMES.len()
    );

    let expected_tiers = BTreeMap::from([
        (APP_STATE, ToolTier::ReadOnly),
        (APP_PLAYBACK, ToolTier::Workspace),
        (APP_SEEK, ToolTier::Workspace),
        (APP_LOOP_RANGE, ToolTier::Workspace),
        (APP_TIMELINE_ZOOM, ToolTier::Workspace),
        (APP_SELECT_CLIP, ToolTier::Workspace),
        (APP_PANEL, ToolTier::Workspace),
        (APP_THEME, ToolTier::Workspace),
        (APP_WINDOW, ToolTier::Workspace),
        (APP_WINDOW_MOVE, ToolTier::Workspace),
        (APP_WINDOW_RESIZE, ToolTier::Workspace),
        (APP_CLOSE, ToolTier::System),
    ]);
    let expected_fields = BTreeMap::from([
        (APP_STATE, BTreeSet::from([])),
        (APP_PLAYBACK, BTreeSet::from(["action"])),
        (APP_SEEK, BTreeSet::from(["seconds"])),
        (
            APP_LOOP_RANGE,
            BTreeSet::from(["action", "end_seconds", "start_seconds"]),
        ),
        (APP_TIMELINE_ZOOM, BTreeSet::from(["action", "value"])),
        (APP_SELECT_CLIP, BTreeSet::from(["action", "clip_id"])),
        (APP_PANEL, BTreeSet::from(["action", "panel"])),
        (APP_THEME, BTreeSet::from(["theme"])),
        (APP_WINDOW, BTreeSet::from(["action"])),
        (APP_WINDOW_MOVE, BTreeSet::from(["x", "y"])),
        (APP_WINDOW_RESIZE, BTreeSet::from(["height", "width"])),
        (APP_CLOSE, BTreeSet::from([])),
    ]);
    let expected_required = BTreeMap::from([
        (APP_STATE, BTreeSet::from([])),
        (APP_PLAYBACK, BTreeSet::from(["action"])),
        (APP_SEEK, BTreeSet::from(["seconds"])),
        (APP_LOOP_RANGE, BTreeSet::from(["action"])),
        (APP_TIMELINE_ZOOM, BTreeSet::from(["action"])),
        (APP_SELECT_CLIP, BTreeSet::from(["action"])),
        (APP_PANEL, BTreeSet::from(["action", "panel"])),
        (APP_THEME, BTreeSet::from(["theme"])),
        (APP_WINDOW, BTreeSet::from(["action"])),
        (APP_WINDOW_MOVE, BTreeSet::from(["x", "y"])),
        (APP_WINDOW_RESIZE, BTreeSet::from(["height", "width"])),
        (APP_CLOSE, BTreeSet::from([])),
    ]);

    for entry in registry {
        assert_eq!(entry.tier, expected_tiers[entry.name.as_str()]);
        assert!(!entry.description.trim().is_empty());
        assert_eq!(entry.parameters["type"], "object");
        assert_eq!(entry.parameters["additionalProperties"], false);
        let fields = entry.parameters["properties"]
            .as_object()
            .expect("properties object")
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        assert_eq!(fields, expected_fields[entry.name.as_str()]);
        let required = entry.parameters["required"]
            .as_array()
            .expect("required array")
            .iter()
            .map(|value| value.as_str().expect("required field name"))
            .collect::<BTreeSet<_>>();
        assert_eq!(required, expected_required[entry.name.as_str()]);
    }
}

#[test]
fn registry_enum_values_and_numeric_bounds_are_exact() {
    let registry = specs();
    let schema = |name: &str| {
        &registry
            .iter()
            .find(|entry| entry.name == name)
            .expect("registered tool")
            .parameters
    };
    assert_eq!(
        schema(APP_PLAYBACK)["properties"]["action"]["enum"],
        json!(["play", "pause", "toggle", "stop"])
    );
    assert_eq!(
        schema(APP_TIMELINE_ZOOM)["properties"]["action"]["enum"],
        json!(["set", "in", "out", "fit"])
    );
    assert_eq!(
        schema(APP_LOOP_RANGE)["properties"]["action"]["enum"],
        json!(["set", "enable", "disable", "clear"])
    );
    assert_eq!(
        schema(APP_SELECT_CLIP)["properties"]["action"]["enum"],
        json!(["select", "toggle", "clear"])
    );
    assert_eq!(
        schema(APP_PANEL)["properties"]["panel"]["enum"],
        json!([
            "library",
            "inspector",
            "assistant",
            "timeline",
            "preview",
            "settings",
            "export",
            "canvas"
        ])
    );
    assert_eq!(
        schema(APP_WINDOW)["properties"]["action"]["enum"],
        json!([
            "minimize",
            "restore",
            "maximize",
            "fullscreen",
            "exit-fullscreen"
        ])
    );
    assert_eq!(
        schema(APP_WINDOW_MOVE)["properties"]["x"]["minimum"],
        MIN_WINDOW_COORD
    );
    assert_eq!(
        schema(APP_WINDOW_RESIZE)["properties"]["width"]["maximum"],
        MAX_WINDOW_WIDTH
    );
}

#[test]
fn parser_accepts_every_tool_and_action_branch() {
    assert_eq!(parse_request(APP_STATE, &json!({})), Ok(Request::State));
    for (key, action) in [
        ("play", PlaybackAction::Play),
        ("pause", PlaybackAction::Pause),
        ("toggle", PlaybackAction::Toggle),
        ("stop", PlaybackAction::Stop),
    ] {
        assert_eq!(
            parse_request(APP_PLAYBACK, &json!({ "action": key })),
            Ok(Request::Playback(action))
        );
    }
    assert_eq!(
        parse_request(APP_SEEK, &json!({ "seconds": 1.25 })),
        Ok(Request::Seek(1.25))
    );
    assert_eq!(
        parse_request(
            APP_LOOP_RANGE,
            &json!({
                "action": "set",
                "start_seconds": 1.0,
                "end_seconds": 2.5
            })
        ),
        Ok(Request::LoopRange(LoopRangeAction::Set {
            start_seconds: 1.0,
            end_seconds: 2.5
        }))
    );
    for (key, action) in [
        ("enable", LoopRangeAction::Enable),
        ("disable", LoopRangeAction::Disable),
        ("clear", LoopRangeAction::Clear),
    ] {
        assert_eq!(
            parse_request(APP_LOOP_RANGE, &json!({ "action": key })),
            Ok(Request::LoopRange(action))
        );
    }
    assert_eq!(
        parse_request(APP_TIMELINE_ZOOM, &json!({ "action": "set", "value": 2.5 })),
        Ok(Request::TimelineZoom(ZoomAction::Set(2.5)))
    );
    for (key, action) in [
        ("in", ZoomAction::In),
        ("out", ZoomAction::Out),
        ("fit", ZoomAction::Fit),
    ] {
        assert_eq!(
            parse_request(APP_TIMELINE_ZOOM, &json!({ "action": key })),
            Ok(Request::TimelineZoom(action))
        );
    }
    assert_eq!(
        parse_request(
            APP_SELECT_CLIP,
            &json!({ "action": "select", "clip_id": "42" })
        ),
        Ok(Request::SelectClip(SelectionAction::Select("42".into())))
    );
    assert_eq!(
        parse_request(
            APP_SELECT_CLIP,
            &json!({ "action": "select", "clip_id": 42 })
        ),
        Ok(Request::SelectClip(SelectionAction::Select("42".into())))
    );
    assert_eq!(
        parse_request(
            APP_SELECT_CLIP,
            &json!({ "action": "toggle", "clip_id": "42" })
        ),
        Ok(Request::SelectClip(SelectionAction::Toggle("42".into())))
    );
    assert_eq!(
        parse_request(APP_SELECT_CLIP, &json!({ "action": "clear" })),
        Ok(Request::SelectClip(SelectionAction::Clear))
    );
    for (panel_key, panel) in [
        ("library", Panel::Library),
        ("inspector", Panel::Inspector),
        ("assistant", Panel::Assistant),
        ("timeline", Panel::Timeline),
        ("preview", Panel::Preview),
        ("settings", Panel::Settings),
        ("export", Panel::Export),
        ("canvas", Panel::Canvas),
    ] {
        for (action_key, action) in [
            ("show", VisibilityAction::Show),
            ("hide", VisibilityAction::Hide),
            ("toggle", VisibilityAction::Toggle),
        ] {
            assert_eq!(
                parse_request(
                    APP_PANEL,
                    &json!({ "panel": panel_key, "action": action_key })
                ),
                Ok(Request::Panel { panel, action })
            );
        }
    }
    for (key, theme) in [
        ("default", cutlass_settings::ThemeChoice::Default),
        ("ember", cutlass_settings::ThemeChoice::Ember),
        ("dark-blue", cutlass_settings::ThemeChoice::DarkBlue),
    ] {
        assert_eq!(
            parse_request(APP_THEME, &json!({ "theme": key })),
            Ok(Request::Theme(theme))
        );
    }
    for (key, action) in [
        ("minimize", WindowAction::Minimize),
        ("restore", WindowAction::Restore),
        ("maximize", WindowAction::Maximize),
        ("fullscreen", WindowAction::Fullscreen),
        ("exit-fullscreen", WindowAction::ExitFullscreen),
    ] {
        assert_eq!(
            parse_request(APP_WINDOW, &json!({ "action": key })),
            Ok(Request::Window(action))
        );
    }
    assert_eq!(
        parse_request(APP_WINDOW_MOVE, &json!({ "x": -50, "y": 90 })),
        Ok(Request::WindowMove { x: -50, y: 90 })
    );
    assert_eq!(
        parse_request(APP_WINDOW_RESIZE, &json!({ "width": 1280, "height": 720 })),
        Ok(Request::WindowResize {
            width: 1280,
            height: 720
        })
    );
    assert_eq!(parse_request(APP_CLOSE, &json!({})), Ok(Request::Close));
}

#[test]
fn parser_requires_objects_known_fields_and_required_fields() {
    assert!(
        parse_request("app_missing", &json!({}))
            .unwrap_err()
            .contains("unknown")
    );
    assert!(
        parse_request(APP_STATE, &Value::Null)
            .unwrap_err()
            .contains("must be an object")
    );
    assert!(
        parse_request(APP_STATE, &json!({ "extra": true }))
            .unwrap_err()
            .contains("unknown argument 'extra'")
    );
    assert!(
        parse_request(APP_PLAYBACK, &json!({}))
            .unwrap_err()
            .contains("missing required argument 'action'")
    );
    assert!(
        parse_request(APP_PLAYBACK, &json!({ "action": 1 }))
            .unwrap_err()
            .contains("must be a string")
    );
    assert!(
        parse_request(APP_PLAYBACK, &json!({ "action": "rewind" }))
            .unwrap_err()
            .contains("unknown value 'rewind'")
    );
    assert!(parse_request(APP_CLOSE, &json!({ "now": true })).is_err());
}

#[test]
fn parser_checks_seek_zoom_and_selection_dependencies() {
    assert_eq!(
        parse_request(APP_SEEK, &json!({ "seconds": 0 })),
        Ok(Request::Seek(0.0))
    );
    assert!(parse_request(APP_SEEK, &json!({ "seconds": -0.01 })).is_err());
    assert!(parse_request(APP_SEEK, &json!({ "seconds": "1" })).is_err());

    assert!(
        parse_request(
            APP_LOOP_RANGE,
            &json!({ "action": "set", "start_seconds": 1.0 })
        )
        .is_err()
    );
    for (start, end) in [(-1.0, 2.0), (2.0, 2.0), (3.0, 2.0)] {
        assert!(
            parse_request(
                APP_LOOP_RANGE,
                &json!({
                    "action": "set",
                    "start_seconds": start,
                    "end_seconds": end
                })
            )
            .is_err()
        );
    }
    assert!(
        parse_request(
            APP_LOOP_RANGE,
            &json!({ "action": "enable", "start_seconds": 1.0 })
        )
        .is_err()
    );
    assert!(parse_request(APP_LOOP_RANGE, &json!({ "action": "repeat" })).is_err());

    for value in [MIN_ZOOM, MAX_ZOOM] {
        assert!(
            parse_request(
                APP_TIMELINE_ZOOM,
                &json!({ "action": "set", "value": value })
            )
            .is_ok()
        );
    }
    for value in [MIN_ZOOM - 0.001, MAX_ZOOM + 0.001, f64::MAX] {
        assert!(
            parse_request(
                APP_TIMELINE_ZOOM,
                &json!({ "action": "set", "value": value })
            )
            .is_err()
        );
    }
    assert!(parse_request(APP_TIMELINE_ZOOM, &json!({ "action": "set" })).is_err());
    assert!(parse_request(APP_TIMELINE_ZOOM, &json!({ "action": "fit", "value": 1 })).is_err());

    assert!(parse_request(APP_SELECT_CLIP, &json!({ "action": "select" })).is_err());
    assert!(
        parse_request(
            APP_SELECT_CLIP,
            &json!({ "action": "toggle", "clip_id": "" })
        )
        .is_err()
    );
    assert!(
        parse_request(
            APP_SELECT_CLIP,
            &json!({ "action": "select", "clip_id": -1 })
        )
        .is_err()
    );
    assert!(
        parse_request(
            APP_SELECT_CLIP,
            &json!({ "action": "clear", "clip_id": "42" })
        )
        .is_err()
    );
}

#[test]
fn parser_checks_integer_types_and_window_edge_bounds() {
    for coordinate in [MIN_WINDOW_COORD, MAX_WINDOW_COORD] {
        assert!(
            parse_request(
                APP_WINDOW_MOVE,
                &json!({ "x": coordinate, "y": coordinate })
            )
            .is_ok()
        );
    }
    for coordinate in [MIN_WINDOW_COORD - 1, MAX_WINDOW_COORD + 1] {
        assert!(parse_request(APP_WINDOW_MOVE, &json!({ "x": coordinate, "y": 0 })).is_err());
    }
    assert!(parse_request(APP_WINDOW_MOVE, &json!({ "x": 1.5, "y": 0 })).is_err());

    for (width, height) in [
        (MIN_WINDOW_WIDTH, MIN_WINDOW_HEIGHT),
        (MAX_WINDOW_WIDTH, MAX_WINDOW_HEIGHT),
    ] {
        assert!(
            parse_request(
                APP_WINDOW_RESIZE,
                &json!({ "width": width, "height": height })
            )
            .is_ok()
        );
    }
    for (width, height) in [
        (MIN_WINDOW_WIDTH - 1, MIN_WINDOW_HEIGHT),
        (MAX_WINDOW_WIDTH + 1, MIN_WINDOW_HEIGHT),
        (MIN_WINDOW_WIDTH, MIN_WINDOW_HEIGHT - 1),
        (MIN_WINDOW_WIDTH, MAX_WINDOW_HEIGHT + 1),
    ] {
        assert!(
            parse_request(
                APP_WINDOW_RESIZE,
                &json!({ "width": width, "height": height })
            )
            .is_err()
        );
    }
}

#[test]
fn theme_keys_map_exactly_and_do_not_accept_aliases() {
    assert_eq!(
        parse_theme_choice("default"),
        Ok(cutlass_settings::ThemeChoice::Default)
    );
    assert_eq!(
        parse_theme_choice("ember"),
        Ok(cutlass_settings::ThemeChoice::Ember)
    );
    assert_eq!(
        parse_theme_choice("dark-blue"),
        Ok(cutlass_settings::ThemeChoice::DarkBlue)
    );
    assert!(parse_theme_choice("dark_blue").is_err());
    assert_eq!(theme_key_from_index(0), "default");
    assert_eq!(theme_key_from_index(1), "ember");
    assert_eq!(theme_key_from_index(2), "dark-blue");
    assert_eq!(theme_key_from_index(99), "dark-blue");
}

#[test]
fn theme_persistence_refuses_to_overwrite_malformed_config() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    let malformed = b"[appearance\ntheme = \"ember\"\n";
    fs::write(&path, malformed).expect("write malformed config");

    let error = persist_theme_at(&path, cutlass_settings::ThemeChoice::Default).unwrap_err();
    assert!(!error.contains(path.to_string_lossy().as_ref()));
    assert_eq!(fs::read(&path).expect("read config"), malformed);
}

#[test]
fn theme_persistence_updates_only_owned_setting() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    fs::write(
        &path,
        "# keep me\n[appearance]\ntheme = \"default\"\n\n[future]\nflag = true\n",
    )
    .expect("write config");

    persist_theme_at(&path, cutlass_settings::ThemeChoice::Ember).expect("save theme");
    let saved = fs::read_to_string(&path).expect("read config");
    assert!(saved.contains("# keep me"));
    assert!(saved.contains("[future]"));
    assert!(saved.contains("flag = true"));
    assert_eq!(
        cutlass_settings::load(&path)
            .expect("load")
            .appearance
            .theme,
        cutlass_settings::ThemeChoice::Ember
    );
}

#[test]
fn visibility_actions_cover_all_transitions() {
    assert!(VisibilityAction::Show.apply(false));
    assert!(VisibilityAction::Show.apply(true));
    assert!(!VisibilityAction::Hide.apply(false));
    assert!(!VisibilityAction::Hide.apply(true));
    assert!(VisibilityAction::Toggle.apply(false));
    assert!(!VisibilityAction::Toggle.apply(true));
}

#[test]
fn mutating_response_envelope_always_carries_result_and_state() {
    let value = state_echo(
        json!({ "status": "requested" }),
        json!({ "transport": { "playing": false } }),
    );
    assert_eq!(value["result"]["status"], "requested");
    assert_eq!(value["state"]["transport"]["playing"], false);
    assert_eq!(value.as_object().expect("echo object").len(), 2);
}

#[test]
fn clip_lookup_reports_track_and_lock_state() {
    let unlocked_clip = crate::Clip {
        id: "unlocked-clip".into(),
        ..Default::default()
    };
    let locked_clip = crate::Clip {
        id: "locked-clip".into(),
        ..Default::default()
    };
    let sequence = crate::Sequence {
        tracks: slint::ModelRc::new(slint::VecModel::from(vec![
            crate::Track {
                id: "track-a".into(),
                clips: slint::ModelRc::new(slint::VecModel::from(vec![unlocked_clip])),
                ..Default::default()
            },
            crate::Track {
                id: "track-b".into(),
                clips: slint::ModelRc::new(slint::VecModel::from(vec![locked_clip])),
                locked: true,
                ..Default::default()
            },
        ])),
        ..Default::default()
    };

    assert_eq!(
        find_clip_track(&sequence, "unlocked-clip"),
        Some(("track-a".into(), false))
    );
    assert_eq!(
        find_clip_track(&sequence, "locked-clip"),
        Some(("track-b".into(), true))
    );
    assert_eq!(find_clip_track(&sequence, "missing"), None);
}

#[test]
fn time_conversion_rounds_and_saturates() {
    let fps = crate::Rational { num: 24, den: 1 };
    assert_eq!(seconds_to_ticks(0.0, &fps), 0);
    assert_eq!(seconds_to_ticks(1.49 / 24.0, &fps), 1);
    assert_eq!(seconds_to_ticks(1.51 / 24.0, &fps), 2);
    assert_eq!(seconds_to_ticks(f64::MAX, &fps), i32::MAX);
    assert!((ticks_to_seconds(36, &fps) - 1.5).abs() < f64::EPSILON);
}

#[test]
fn loop_range_conversion_rejects_collapsed_and_out_of_sequence_ranges() {
    let fps = crate::Rational { num: 24, den: 1 };
    assert_eq!(loop_range_ticks(1.0, 2.5, &fps, 120), Ok((24, 60)));
    assert!(loop_range_ticks(1.0, 1.01, &fps, 120).is_err());
    let error = loop_range_ticks(4.0, 6.0, &fps, 120).unwrap_err();
    assert!(error.contains("beyond the 5.000s sequence"), "{error}");
}

#[test]
fn response_wait_returns_a_queued_result() {
    let (sender, receiver) = bounded(1);
    sender.send(Ok(json!({ "ok": true }))).expect("send");
    let cancel = AtomicBool::new(false);
    let abandoned = AtomicBool::new(false);
    let result = wait_for_ui_response(
        &receiver,
        &cancel,
        &abandoned,
        Duration::from_millis(50),
        Duration::from_millis(2),
    )
    .expect("wait")
    .expect("UI result");
    assert_eq!(result, json!({ "ok": true }));
    assert!(!abandoned.load(Ordering::Acquire));
}

#[test]
fn call_honors_cancellation_before_parsing_or_dispatch() {
    let cancel = AtomicBool::new(true);
    let error = call(slint::Weak::default(), APP_PLAYBACK, &Value::Null, &cancel).unwrap_err();
    assert!(error.contains("cancelled"));
}

#[test]
fn response_wait_is_cancellation_aware() {
    let (_sender, receiver) = bounded::<UiResponse>(1);
    let cancel = AtomicBool::new(true);
    let abandoned = AtomicBool::new(false);
    let started = Instant::now();
    let error = wait_for_ui_response(
        &receiver,
        &cancel,
        &abandoned,
        Duration::from_secs(1),
        Duration::from_millis(5),
    )
    .unwrap_err();
    assert!(error.contains("cancelled"));
    assert!(started.elapsed() < Duration::from_millis(100));
    assert!(abandoned.load(Ordering::Acquire));
}

#[test]
fn response_wait_times_out_and_detects_disconnect() {
    let (_sender, receiver) = bounded::<UiResponse>(1);
    let cancel = AtomicBool::new(false);
    let abandoned = AtomicBool::new(false);
    let started = Instant::now();
    let error = wait_for_ui_response(
        &receiver,
        &cancel,
        &abandoned,
        Duration::from_millis(15),
        Duration::from_millis(3),
    )
    .unwrap_err();
    assert!(error.contains("did not respond"));
    assert!(started.elapsed() < Duration::from_millis(200));
    assert!(abandoned.load(Ordering::Acquire));

    let (sender, receiver) = bounded::<UiResponse>(1);
    drop(sender);
    let abandoned = AtomicBool::new(false);
    let error = wait_for_ui_response(
        &receiver,
        &cancel,
        &abandoned,
        Duration::from_secs(1),
        Duration::from_millis(5),
    )
    .unwrap_err();
    assert!(error.contains("channel closed"));
    assert!(abandoned.load(Ordering::Acquire));
}
