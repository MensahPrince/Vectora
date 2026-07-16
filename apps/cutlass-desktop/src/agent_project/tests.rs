use std::collections::BTreeSet;
use std::path::PathBuf;
use std::time::Duration;

use super::*;

fn draft(
    id: &str,
    name: impl Into<String>,
    modified: SystemTime,
) -> crate::drafts::DraftSummary {
    crate::drafts::DraftSummary {
        name: name.into(),
        project: crate::drafts::project_file(&crate::drafts::root_dir().join(id)),
        modified,
    }
}

fn output_json(output: &ToolOutput) -> Value {
    assert!(output.images.is_empty());
    serde_json::from_str(&output.text).expect("compact JSON project-tool output")
}

fn path_arguments(path: &Path) -> Value {
    json!({"path": path.to_str().expect("UTF-8 test path")})
}

fn validated_media(path: &Path) -> ValidatedImportMedia {
    validated_import_media(&path_arguments(path)).expect("validated media fixture")
}

fn missing_draft_id() -> String {
    let mut time = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("current time after epoch")
        .as_nanos();
    loop {
        // Production ids use epoch milliseconds, so an epoch-nanosecond
        // group is also well outside ids created during this test.
        let id = format!("{time:x}-ffffffffffffffff");
        let project = crate::drafts::project_file(&crate::drafts::root_dir().join(&id));
        if !project.exists() {
            return id;
        }
        time = time.checked_add(1).expect("draft-id search overflow");
    }
}

#[test]
fn specs_have_exact_names_tiers_and_strict_schemas() {
    let registry = specs();
    assert_eq!(
        registry
            .iter()
            .map(|entry| entry.name.as_str())
            .collect::<Vec<_>>(),
        TOOL_NAMES
    );
    assert_eq!(
        registry.iter().map(|entry| entry.tier).collect::<Vec<_>>(),
        [
            ToolTier::ReadOnly,
            ToolTier::Workspace,
            ToolTier::System,
            ToolTier::System,
        ]
    );
    assert_eq!(registry[0].parameters, list_schema());
    assert_eq!(registry[1].parameters, empty_object_schema());
    assert_eq!(registry[2].parameters, open_schema());
    assert_eq!(registry[3].parameters, import_media_schema());
    assert_eq!(registry[0].parameters["properties"]["limit"]["minimum"], 1);
    assert_eq!(
        registry[0].parameters["properties"]["limit"]["maximum"],
        100
    );
    assert_eq!(registry[0].parameters["properties"]["limit"]["default"], 50);
    assert_eq!(
        registry[2].parameters["properties"]["draft_id"]["minLength"],
        MIN_DRAFT_ID_CHARS
    );
    assert_eq!(
        registry[2].parameters["properties"]["draft_id"]["maxLength"],
        MAX_DRAFT_ID_CHARS
    );
    assert_eq!(registry[2].parameters["required"], json!(["draft_id"]));
    assert_eq!(registry[3].parameters["properties"]["path"]["minLength"], 1);
    assert_eq!(
        registry[3].parameters["properties"]["path"]["maxLength"],
        MAX_IMPORT_PATH_CHARS
    );
    assert_eq!(registry[3].parameters["required"], json!(["path"]));
    for entry in registry {
        assert_eq!(entry.parameters["type"], "object");
        assert_eq!(entry.parameters["additionalProperties"], false);
        assert!(!entry.description.is_empty());
    }
}

#[test]
fn parser_rejects_non_objects_unknown_fields_and_non_integer_limits() {
    for arguments in [Value::Null, json!([]), json!(""), json!(false), json!(1)] {
        assert!(parse_request(PROJECT_LIST_DRAFTS, &arguments).is_err());
        assert!(parse_request(PROJECT_SAVE, &arguments).is_err());
        assert!(parse_request(PROJECT_OPEN, &arguments).is_err());
        assert!(parse_request(PROJECT_IMPORT_MEDIA, &arguments).is_err());
    }
    for arguments in [
        json!({"extra": 1}),
        json!({"limit": 1, "extra": 2}),
        json!({"limit": null}),
        json!({"limit": false}),
        json!({"limit": 1.0}),
        json!({"limit": 0}),
        json!({"limit": -1}),
        json!({"limit": 101}),
    ] {
        assert!(
            parse_request(PROJECT_LIST_DRAFTS, &arguments).is_err(),
            "{arguments}"
        );
    }
    assert!(parse_request(PROJECT_SAVE, &json!({"limit": 1})).is_err());
    for arguments in [
        json!({}),
        json!({"extra": "abc-1"}),
        json!({"draft_id": "abc-1", "extra": true}),
        json!({"draft_id": null}),
        json!({"draft_id": false}),
        json!({"draft_id": 1}),
        json!({"draft_id": ""}),
        json!({"draft_id": "a"}),
        json!({"draft_id": "x".repeat(MAX_DRAFT_ID_CHARS + 1)}),
    ] {
        assert!(
            parse_request(PROJECT_OPEN, &arguments).is_err(),
            "{arguments}"
        );
    }
    assert!(parse_request("project_future", &json!({})).is_err());
    assert_eq!(
        parse_request(PROJECT_LIST_DRAFTS, &json!({})),
        Ok(Request::ListDrafts {
            limit: DEFAULT_DRAFT_LIMIT
        })
    );
    assert_eq!(
        parse_request(PROJECT_LIST_DRAFTS, &json!({"limit": 100})),
        Ok(Request::ListDrafts { limit: 100 })
    );
    assert_eq!(parse_request(PROJECT_SAVE, &json!({})), Ok(Request::Save));
    assert_eq!(
        parse_request(PROJECT_OPEN, &json!({"draft_id": "abc-1"})),
        Ok(Request::Open {
            draft_id: "abc-1".into()
        })
    );

    let temp = tempfile::tempdir().expect("tempdir");
    let absolute = temp.path().join("media.mp4");
    let absolute_text = absolute.to_str().expect("UTF-8 temp path");
    for arguments in [
        json!({}),
        json!({"extra": absolute_text}),
        json!({"path": absolute_text, "extra": true}),
        json!({"path": null}),
        json!({"path": false}),
        json!({"path": 1}),
        json!({"path": ""}),
        json!({"path": "relative/media.mp4"}),
        json!({"path": format!("{absolute_text}\0")}),
        json!({"path": "x".repeat(MAX_IMPORT_PATH_CHARS + 1)}),
    ] {
        assert!(
            parse_request(PROJECT_IMPORT_MEDIA, &arguments).is_err(),
            "{arguments}"
        );
    }
    assert_eq!(
        parse_request(PROJECT_IMPORT_MEDIA, &json!({"path": absolute_text})),
        Ok(Request::ImportMedia {
            path: absolute_text.into()
        })
    );
}

#[test]
fn import_preflight_accepts_only_existing_absolute_regular_files() {
    let temp = tempfile::tempdir().expect("tempdir");
    let media = temp.path().join("clip.mp4");
    fs::write(&media, b"media").expect("write media fixture");
    let expected = media.canonicalize().expect("canonical media");
    let arguments = path_arguments(&media);

    let validated = validated_import_media(&arguments).expect("valid media path");
    assert_eq!(validated.canonical_path(), expected);
    validate_request(PROJECT_IMPORT_MEDIA, &arguments).expect("preapproval validation");

    let missing = temp.path().join("agent-secret-missing.mp4");
    let missing_error = validate_request(PROJECT_IMPORT_MEDIA, &path_arguments(&missing))
        .expect_err("missing source must fail");
    assert_eq!(
        missing_error,
        "project_import_media failed: the requested media file does not exist"
    );
    assert!(!missing_error.contains("agent-secret"));
    assert!(!missing_error.contains(temp.path().to_string_lossy().as_ref()));

    let directory_error = validate_request(PROJECT_IMPORT_MEDIA, &path_arguments(temp.path()))
        .expect_err("directory must fail");
    assert_eq!(
        directory_error,
        "project_import_media argument 'path' must name a regular file"
    );
    assert!(!directory_error.contains(temp.path().to_string_lossy().as_ref()));
}

#[test]
fn import_parser_rejects_lexical_dot_components_and_unsafe_text() {
    let temp = tempfile::tempdir().expect("tempdir");
    let media = temp.path().join("clip.mp4");
    fs::write(&media, b"media").expect("write media fixture");
    let child = temp.path().join("child");
    fs::create_dir(&child).expect("create child");

    let dot = temp.path().join(".").join("clip.mp4");
    let parent = child.join("..").join("clip.mp4");
    for path in [&dot, &parent] {
        let error = parse_request(PROJECT_IMPORT_MEDIA, &path_arguments(path))
            .expect_err("lexical traversal component must fail");
        assert!(error.contains("must not contain '.' or '..'"), "{error}");
        assert!(!error.contains(temp.path().to_string_lossy().as_ref()));
    }

    let nul_path = format!("{}\0clip.mp4", temp.path().display());
    let nul = parse_request(PROJECT_IMPORT_MEDIA, &json!({"path": nul_path}))
        .expect_err("NUL must fail");
    assert!(nul.contains("must not contain NUL"), "{nul}");

    let relative = parse_request(PROJECT_IMPORT_MEDIA, &json!({"path": "relative/clip.mp4"}))
        .expect_err("relative path must fail");
    assert_eq!(
        relative,
        "project_import_media argument 'path' must be absolute"
    );

    let oversized = format!(
        "{}{}",
        temp.path().display(),
        "x".repeat(MAX_IMPORT_PATH_CHARS + 1)
    );
    let oversized_error = parse_request(PROJECT_IMPORT_MEDIA, &json!({"path": oversized}))
        .expect_err("oversized path must fail");
    assert!(oversized_error.contains("must contain 1 through"));
    assert!(!oversized_error.contains(temp.path().to_string_lossy().as_ref()));
}

#[cfg(unix)]
#[test]
fn import_preflight_rejects_final_symlinks_but_canonicalizes_intermediate_links() {
    use std::os::unix::fs::symlink;
    use std::os::unix::net::UnixListener;

    let temp = tempfile::tempdir().expect("tempdir");
    let real_dir = temp.path().join("real");
    fs::create_dir(&real_dir).expect("real directory");
    let media = real_dir.join("clip.mp4");
    fs::write(&media, b"media").expect("write media fixture");

    let final_link = temp.path().join("final-link.mp4");
    symlink(&media, &final_link).expect("final symlink");
    let final_error = validate_request(PROJECT_IMPORT_MEDIA, &path_arguments(&final_link))
        .expect_err("final symlink must fail");
    assert!(final_error.contains("symbolic link or reparse point"));
    assert!(!final_error.contains(temp.path().to_string_lossy().as_ref()));

    let socket = temp.path().join("not-media.sock");
    let _listener = UnixListener::bind(&socket).expect("Unix socket");
    let socket_error = validate_request(PROJECT_IMPORT_MEDIA, &path_arguments(&socket))
        .expect_err("non-regular file must fail");
    assert_eq!(
        socket_error,
        "project_import_media argument 'path' must name a regular file"
    );
    assert!(!socket_error.contains(temp.path().to_string_lossy().as_ref()));

    let directory_link = temp.path().join("linked-directory");
    symlink(&real_dir, &directory_link).expect("intermediate directory symlink");
    let through_intermediate = directory_link.join("clip.mp4");
    let validated = validated_import_media(&path_arguments(&through_intermediate))
        .expect("intermediate link is canonicalized");
    assert_eq!(
        validated.canonical_path(),
        media.canonicalize().expect("canonical media")
    );
}

#[test]
fn open_validation_rejects_malformed_and_missing_ids_without_path_leaks() {
    let private_path = "/private/agent-secret/project.cutlass";
    let malformed = validate_request(
        PROJECT_OPEN,
        &json!({
            "draft_id": private_path
        }),
    )
    .expect_err("filesystem paths are not draft ids");
    assert_eq!(
        malformed,
        "project_open argument 'draft_id' must be a canonical app-owned draft ID"
    );
    assert!(!malformed.contains("/private"));
    assert!(!malformed.contains("agent-secret"));

    let missing_id = missing_draft_id();
    let missing = validate_request(PROJECT_OPEN, &json!({"draft_id": missing_id}))
        .expect_err("missing draft must fail read-only preflight");
    assert!(missing.starts_with("project_open failed:"));
    assert!(
        !missing.contains(crate::drafts::root_dir().to_string_lossy().as_ref()),
        "{missing}"
    );
    assert!(!missing.contains("project.cutlass"), "{missing}");
    assert!(missing.len() < 160);
}

#[test]
fn draft_serialization_is_deterministic_bounded_sorted_and_path_free() {
    let before_epoch = UNIX_EPOCH
        .checked_sub(Duration::from_millis(1))
        .expect("pre-epoch time");
    let oversized_name = "é".repeat(MAX_DISPLAY_NAME_CHARS + 20);
    let fixtures = vec![
        draft("abc-1", "old", UNIX_EPOCH + Duration::from_millis(1_000)),
        draft(
            "abc-3",
            oversized_name,
            UNIX_EPOCH + Duration::from_millis(3_000),
        ),
        draft("abc-0", "pre-epoch", before_epoch),
        draft("abc-2", "middle", UNIX_EPOCH + Duration::from_millis(2_000)),
    ];

    let first = list_drafts_output(fixtures.clone(), 2).expect("serialize drafts");
    let second = list_drafts_output(fixtures, 2).expect("serialize drafts again");
    assert_eq!(first, second);
    assert!(!first.text.contains('\n'), "response must be compact JSON");
    assert!(
        !first
            .text
            .contains(crate::drafts::root_dir().to_string_lossy().as_ref())
    );
    assert!(!first.text.contains("project.cutlass"));

    let value = output_json(&first);
    assert_eq!(
        value
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>(),
        BTreeSet::from(["drafts", "status", "total", "truncated"])
    );
    assert_eq!(value["status"], "ok");
    assert_eq!(value["total"], 4);
    assert_eq!(value["truncated"], true);
    let rows = value["drafts"].as_array().expect("draft rows");
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["draft_id"], "abc-3");
    assert_eq!(rows[0]["modified_unix_ms"], 3_000);
    assert_eq!(
        rows[0]["name"].as_str().unwrap().chars().count(),
        MAX_DISPLAY_NAME_CHARS
    );
    assert_eq!(rows[1]["draft_id"], "abc-2");
    assert_eq!(
        rows[0]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>(),
        BTreeSet::from(["draft_id", "modified_unix_ms", "name"])
    );

    let all = output_json(
        &list_drafts_output(
            vec![draft("abc-0", "pre-epoch", before_epoch)],
            MAX_DRAFT_LIMIT,
        )
        .expect("serialize pre-epoch draft"),
    );
    assert!(all["drafts"][0]["modified_unix_ms"].is_null());
    assert_eq!(all["truncated"], false);
}

#[test]
fn open_output_is_bounded_path_free_and_bound_to_the_requested_identity() {
    let draft_id = "abcdef-12";
    let project = crate::drafts::project_file(&crate::drafts::root_dir().join(draft_id));
    let opened = OpenProjectRpcResult {
        path: project.clone(),
        project_name: "é".repeat(MAX_DISPLAY_NAME_CHARS + 20),
        missing_media_count: 7,
    };
    let output = open_output(draft_id, &opened).expect("safe open output");
    assert!(!output.text.contains('\n'), "response must be compact JSON");
    assert!(!output.text.contains(project.to_string_lossy().as_ref()));
    assert!(!output.text.contains("project.cutlass"));

    let value = output_json(&output);
    assert_eq!(
        value
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>(),
        BTreeSet::from(["draft_id", "missing_media_count", "project_name", "status"])
    );
    assert_eq!(value["status"], "ok");
    assert_eq!(value["draft_id"], draft_id);
    assert_eq!(value["missing_media_count"], 7);
    assert_eq!(
        value["project_name"].as_str().unwrap().chars().count(),
        MAX_DISPLAY_NAME_CHARS
    );

    let mismatch =
        open_output("abcdef-13", &opened).expect_err("identity mismatch must fail closed");
    assert!(mismatch.contains("current session was replaced"));
    assert!(!mismatch.contains(project.to_string_lossy().as_ref()));
    assert!(!mismatch.contains("project.cutlass"));

    let outside = OpenProjectRpcResult {
        path: PathBuf::from("/private/agent-secret/project.cutlass"),
        project_name: "outside".into(),
        missing_media_count: 0,
    };
    let outside_error =
        open_output(draft_id, &outside).expect_err("outside path must fail closed");
    assert!(outside_error.contains("current session was replaced"));
    assert!(!outside_error.contains("/private"));
    assert!(!outside_error.contains("agent-secret"));
}

#[test]
fn open_rpc_errors_preserve_not_started_and_unknown_outcomes_without_paths() {
    let cancelled = public_open_error(
        "open project request cancelled before worker claim; not started for \
         /private/agent-secret/project.cutlass",
    );
    assert_eq!(cancelled, "project_open was cancelled before it started");
    assert!(!cancelled.contains("/private"));

    let not_started = public_open_error(
        "open project request failed: preview worker is not running; not started for \
         /private/agent-secret/project.cutlass",
    );
    assert_eq!(
        not_started,
        "project_open failed before the editor started it"
    );
    assert!(!not_started.contains("/private"));

    for internal in [
        "open project request timed out after worker claim; outcome unknown after 30000 ms \
         for /private/agent-secret/project.cutlass",
        "open project outcome uncertain/partially committed: \
         /private/agent-secret/project.cutlass replaced the session",
    ] {
        let unknown = public_open_error(internal);
        assert!(unknown.contains("outcome unknown"), "{unknown}");
        assert!(unknown.contains("project may have opened"), "{unknown}");
        assert!(!unknown.contains("/private"), "{unknown}");
        assert!(!unknown.contains("agent-secret"), "{unknown}");
        assert!(unknown.len() < 160);
    }

    let failed = public_open_error(
        "open project failed for /private/agent-secret/project.cutlass: corrupt data",
    );
    assert_eq!(
        failed,
        "project_open failed: the requested draft could not be opened"
    );
    assert!(!failed.contains("/private"));
    assert!(!failed.contains("agent-secret"));
    assert!(!failed.contains("may have opened"));
}

#[test]
fn import_output_is_bounded_path_free_and_bound_to_the_acknowledged_file() {
    let temp = tempfile::tempdir().expect("tempdir");
    let filename = format!("{}.mp4", "a".repeat(MAX_DISPLAY_NAME_CHARS + 16));
    let media = temp.path().join(&filename);
    fs::write(&media, b"media").expect("write media fixture");
    let expected = validated_media(&media);
    let imported = ImportMediaRpcResult {
        media_id: 41,
        path: expected.canonical_path().to_path_buf(),
    };
    let output = import_media_output(&expected, &imported).expect("safe import output");
    assert!(!output.text.contains('\n'), "response must be compact JSON");
    assert!(
        !output.text.contains(temp.path().to_string_lossy().as_ref()),
        "{}",
        output.text
    );

    let value = output_json(&output);
    assert_eq!(
        value
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>(),
        BTreeSet::from(["filename", "media_id", "status"])
    );
    assert_eq!(value["status"], "ok");
    assert_eq!(value["media_id"], 41);
    assert_eq!(
        value["filename"].as_str().unwrap().chars().count(),
        MAX_DISPLAY_NAME_CHARS
    );
    assert!(output.text.len() < 1_024);

    assert_eq!(
        bounded_safe_display_name("clip\n\u{2028}\u{202e}name.mp4"),
        "clip\u{fffd}\u{fffd}\u{fffd}name.mp4"
    );
}

#[test]
fn import_output_fails_closed_on_identity_mismatch_and_invalid_acknowledgement() {
    let temp = tempfile::tempdir().expect("tempdir");
    let approved = temp.path().join("approved-secret.mp4");
    let different = temp.path().join("different-secret.mp4");
    fs::write(&approved, b"approved").expect("approved fixture");
    fs::write(&different, b"different").expect("different fixture");
    let approved = validated_media(&approved);
    let different = different.canonicalize().expect("canonical different");

    let mismatch = import_media_output(
        &approved,
        &ImportMediaRpcResult {
            media_id: 7,
            path: different,
        },
    )
    .expect_err("different acknowledged file must fail");
    assert!(mismatch.contains("different source file"), "{mismatch}");
    assert!(
        mismatch.contains("media may have been imported"),
        "{mismatch}"
    );
    assert!(!mismatch.contains("approved-secret"), "{mismatch}");
    assert!(!mismatch.contains("different-secret"), "{mismatch}");
    assert!(!mismatch.contains(temp.path().to_string_lossy().as_ref()));

    let zero_id = import_media_output(
        &approved,
        &ImportMediaRpcResult {
            media_id: 0,
            path: approved.canonical_path().to_path_buf(),
        },
    )
    .expect_err("zero media id must fail");
    assert!(zero_id.contains("invalid media identity"), "{zero_id}");
    assert!(
        zero_id.contains("media may have been imported"),
        "{zero_id}"
    );
    assert!(!zero_id.contains("approved-secret"), "{zero_id}");

    let missing_acknowledgement = import_media_output(
        &approved,
        &ImportMediaRpcResult {
            media_id: 8,
            path: temp.path().join("missing-agent-secret.mp4"),
        },
    )
    .expect_err("missing acknowledged path must fail");
    assert!(
        missing_acknowledgement.contains("could not verify"),
        "{missing_acknowledgement}"
    );
    assert!(
        missing_acknowledgement.contains("media may have been imported"),
        "{missing_acknowledgement}"
    );
    assert!(
        !missing_acknowledgement.contains("missing-agent-secret"),
        "{missing_acknowledgement}"
    );
    assert!(
        !missing_acknowledgement.contains(temp.path().to_string_lossy().as_ref()),
        "{missing_acknowledgement}"
    );
}

#[test]
fn import_token_rejects_same_path_replacement_before_dispatch_and_after_ack() {
    let temp = tempfile::tempdir().expect("tempdir");
    let media = temp.path().join("approved-secret.mp4");
    let replacement = temp.path().join("replacement-secret.mp4");
    fs::write(&media, b"approved").expect("approved fixture");
    fs::write(&replacement, b"replacement").expect("replacement fixture");

    let approved = validated_media(&media);
    let replacement_before_move = validated_media(&replacement);
    assert_ne!(
        approved.identity, replacement_before_move.identity,
        "simultaneously existing files must have distinct identities"
    );

    fs::remove_file(&media).expect("remove approved fixture");
    fs::rename(&replacement, &media).expect("move replacement onto approved path");
    let current = validated_media(&media);
    assert_eq!(
        approved.canonical_path(),
        current.canonical_path(),
        "replacement deliberately keeps the approved canonical path"
    );
    assert_eq!(current.identity, replacement_before_move.identity);
    assert!(!approved.identifies_same_file(&current));

    let pre_dispatch = call_with_approved_import(
        None,
        PROJECT_IMPORT_MEDIA,
        &path_arguments(&media),
        Some(&approved),
        &AtomicBool::new(false),
    )
    .expect_err("same-path replacement must stop before worker dispatch");
    assert_eq!(
        pre_dispatch,
        "project_import_media failed: the media file changed after approval; not started"
    );
    assert!(!pre_dispatch.contains("approved-secret"));
    assert!(!pre_dispatch.contains(temp.path().to_string_lossy().as_ref()));

    let acknowledged = import_media_output(
        &approved,
        &ImportMediaRpcResult {
            media_id: 9,
            path: media,
        },
    )
    .expect_err("acknowledged same-path replacement must fail closed");
    assert!(
        acknowledged.contains("different source file"),
        "{acknowledged}"
    );
    assert!(
        acknowledged.contains("media may have been imported"),
        "{acknowledged}"
    );
    assert!(!acknowledged.contains("approved-secret"));
    assert!(!acknowledged.contains("replacement-secret"));
    assert!(
        !acknowledged.contains(temp.path().to_string_lossy().as_ref()),
        "{acknowledged}"
    );
}

#[test]
fn import_rpc_errors_preserve_not_started_and_unknown_outcomes_without_paths() {
    let private = "/private/agent-secret/clip.mp4";
    let cancelled = public_import_error(&format!(
        "import media request cancelled before worker claim; not started for {private}"
    ));
    assert_eq!(
        cancelled,
        "project_import_media was cancelled before it started"
    );
    assert!(!cancelled.contains(private));

    let not_started = public_import_error(&format!(
        "import media request failed: preview worker is not running; not started for {private}"
    ));
    assert_eq!(
        not_started,
        "project_import_media failed before the editor started it"
    );
    assert!(!not_started.contains(private));

    for internal in [
        format!(
            "import media request timed out after worker claim; outcome unknown for {private}"
        ),
        format!("import media outcome uncertain/partially committed while decoding {private}"),
        format!("import succeeded for {private} but its pool record could not be read back"),
    ] {
        let unknown = public_import_error(&internal);
        assert!(unknown.contains("outcome unknown"), "{unknown}");
        assert!(
            unknown.contains("media may have been imported"),
            "{unknown}"
        );
        assert!(!unknown.contains(private), "{unknown}");
        assert!(unknown.len() < 180);
    }

    let decoder = public_import_error(&format!(
        "decoder failed for {private}: stream not started; arbitrary details"
    ));
    assert_eq!(
        decoder,
        "project_import_media failed: the media file could not be imported"
    );
    assert!(!decoder.contains(private));
    assert!(!decoder.contains("decoder"));
    assert!(!decoder.contains("arbitrary"));
}

#[test]
fn save_output_contains_only_safe_identity_and_clean_state() {
    let project = crate::drafts::project_file(&crate::drafts::root_dir().join("abcdef-12"));
    let output = save_output(&project, false).expect("safe save output");
    assert!(!output.text.contains(project.to_string_lossy().as_ref()));
    assert!(!output.text.contains("project.cutlass"));
    let value = output_json(&output);
    assert_eq!(
        value
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>(),
        BTreeSet::from(["dirty", "draft_id", "status"])
    );
    assert_eq!(
        value,
        json!({"status":"ok","draft_id":"abcdef-12","dirty":false})
    );

    let outside = PathBuf::from("/private/agent-secret/project.cutlass");
    let error = save_output(&outside, false).expect_err("outside path must fail");
    assert!(!error.contains("/private"));
    assert!(!error.contains("agent-secret"));

    let dirty_error = save_output(&project, true).expect_err("dirty save must fail");
    assert!(!dirty_error.contains(project.to_string_lossy().as_ref()));

    let internal = "save failed for /private/agent-secret/project.cutlass: disk error";
    let public = public_save_error(internal);
    assert_eq!(
        public,
        "project_save failed: the current draft could not be saved"
    );
    assert!(!public.contains("/private"));
    assert!(!public.contains("agent-secret"));
}

#[test]
fn unavailable_worker_is_an_honest_bounded_error() {
    let error = call(None, PROJECT_SAVE, &json!({}), &AtomicBool::new(false))
        .expect_err("missing worker");
    assert_eq!(
        error,
        "project_save failed: the editor worker is unavailable"
    );
    assert!(error.len() < 128);

    let cancelled = call(
        None,
        PROJECT_OPEN,
        &json!({"draft_id": "abc-1"}),
        &AtomicBool::new(true),
    )
    .expect_err("pre-cancelled open must stop before draft resolution");
    assert_eq!(cancelled, "project_open was cancelled before it started");

    let temp = tempfile::tempdir().expect("tempdir");
    let missing = temp.path().join("missing-before-cancellation.mp4");
    let import_cancelled = call(
        None,
        PROJECT_IMPORT_MEDIA,
        &path_arguments(&missing),
        &AtomicBool::new(true),
    )
    .expect_err("pre-cancelled import must stop before filesystem inspection");
    assert_eq!(
        import_cancelled,
        "project_import_media was cancelled before it started"
    );

    let media = temp.path().join("clip.mp4");
    let other = temp.path().join("other.mp4");
    fs::write(&media, b"media").expect("media fixture");
    fs::write(&other, b"other").expect("other fixture");
    let approved_other = validated_media(&other);
    let changed = call_with_approved_import(
        None,
        PROJECT_IMPORT_MEDIA,
        &path_arguments(&media),
        Some(&approved_other),
        &AtomicBool::new(false),
    )
    .expect_err("post-approval canonical mismatch must stop before queueing");
    assert_eq!(
        changed,
        "project_import_media failed: the media file changed after approval; not started"
    );
    assert!(!changed.contains(temp.path().to_string_lossy().as_ref()));

    let unavailable = call(
        None,
        PROJECT_IMPORT_MEDIA,
        &path_arguments(&media),
        &AtomicBool::new(false),
    )
    .expect_err("valid import without worker");
    assert_eq!(
        unavailable,
        "project_import_media failed: the editor worker is unavailable; not started"
    );
    assert!(!unavailable.contains(temp.path().to_string_lossy().as_ref()));
}

#[test]
fn live_project_mutation_classification_is_explicit() {
    assert!(!mutates_live_project(PROJECT_LIST_DRAFTS));
    assert!(mutates_live_project(PROJECT_SAVE));
    assert!(mutates_live_project(PROJECT_OPEN));
    assert!(mutates_live_project(PROJECT_IMPORT_MEDIA));
    assert!(
        mutates_live_project("project_future"),
        "future project tools must fail closed until explicitly classified as read-only"
    );
    assert!(!mutates_live_project("app_state"));
}
