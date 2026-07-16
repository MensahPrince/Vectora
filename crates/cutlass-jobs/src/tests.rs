use std::sync::mpsc;
use std::time::Duration;

use super::*;

/// A manager plus a channel carrying every subscriber event, so tests
/// rendezvous on real transitions instead of sleeping.
fn manager_with_events() -> (JobManager, mpsc::Receiver<JobSnapshot>) {
    let manager = JobManager::new();
    let (tx, rx) = mpsc::channel();
    manager.subscribe(move |snapshot| {
        let _ = tx.send(snapshot);
    });
    (manager, rx)
}

/// Drain events for `id` (skipping other jobs') until its terminal
/// event; returns the job's full ordered event history.
fn wait_history(rx: &mpsc::Receiver<JobSnapshot>, id: JobId) -> Vec<JobSnapshot> {
    let mut events = Vec::new();
    loop {
        let snapshot = rx
            .recv_timeout(Duration::from_secs(10))
            .expect("timed out waiting for job events");
        if snapshot.id != id {
            continue;
        }
        let terminal = snapshot.state.is_terminal();
        if terminal {
            assert!(
                !snapshot.cancellable,
                "terminal job {id} still accepts cancellation"
            );
        }
        events.push(snapshot);
        if terminal {
            return events;
        }
    }
}

fn assert_one_line_bounded(value: &str, max_bytes: usize) {
    assert!(
        value.len() <= max_bytes,
        "{} bytes exceeds {max_bytes}",
        value.len()
    );
    assert!(std::str::from_utf8(value.as_bytes()).is_ok());
    assert!(
        !value
            .chars()
            .any(|character| character.is_control()
                || matches!(character, '\u{2028}' | '\u{2029}')),
        "unsafe one-line value: {value:?}"
    );
}

fn oversized_text(prefix: &str, cap: usize) -> String {
    let mut value = format!("{prefix}\n\r\t\0\u{2028}\u{2029}");
    while value.len() <= cap + 8 {
        value.push_str("动画🦀");
    }
    value
}

#[test]
fn structured_output_boundaries_are_exact_and_values_are_not_rewritten() {
    let max_name = format!("a{}", "z".repeat(MAX_JOB_OUTPUT_NAME_BYTES - 1));
    let max_ascii_value = "v".repeat(MAX_JOB_OUTPUT_VALUE_BYTES);
    let max_unicode_value = "é".repeat(MAX_JOB_OUTPUT_VALUE_BYTES / 2);
    assert_eq!(max_unicode_value.len(), MAX_JOB_OUTPUT_VALUE_BYTES);

    let empty = JobOutput::new("empty_value", "").expect("empty values are intentional");
    assert_eq!(empty.name(), "empty_value");
    assert_eq!(empty.value(), "");

    let ascii = JobOutput::new(max_name.clone(), max_ascii_value.clone())
        .expect("exact name/value bounds");
    assert_eq!(ascii.name(), max_name);
    assert_eq!(ascii.value(), max_ascii_value);

    let unicode =
        JobOutput::new("unicode", max_unicode_value.clone()).expect("exact UTF-8 byte bound");
    assert_eq!(unicode.value(), max_unicode_value);

    assert_eq!(
        JobOutput::new(format!("{max_name}z"), "").unwrap_err(),
        JobCompletionError::NameTooLong
    );
    assert_eq!(
        JobOutput::new("value", format!("{max_ascii_value}v")).unwrap_err(),
        JobCompletionError::ValueTooLong
    );
    assert_eq!(
        JobOutput::new("value", format!("{max_unicode_value}a")).unwrap_err(),
        JobCompletionError::ValueTooLong
    );
}

#[test]
fn structured_output_rejects_invalid_names_and_multiline_or_control_values() {
    assert_eq!(
        JobOutput::new("", "").unwrap_err(),
        JobCompletionError::EmptyName
    );
    for name in [
        "0key",
        "_key",
        "Content_key",
        "content-key",
        "content.key",
        "content key",
        "contentKey",
        "contenț",
    ] {
        assert_eq!(
            JobOutput::new(name, "").unwrap_err(),
            JobCompletionError::InvalidName,
            "accepted invalid name {name:?}"
        );
    }
    for name in ["a", "a0", "content_key", "analyzer_version", "z_9"] {
        JobOutput::new(name, "").expect("valid output name");
    }

    for value in [
        "line\nbreak",
        "line\rbreak",
        "tab\tvalue",
        "nul\0value",
        "delete\u{7f}",
        "next\u{85}line",
        "separator\u{2028}value",
        "paragraph\u{2029}value",
    ] {
        assert_eq!(
            JobOutput::new("value", value).unwrap_err(),
            JobCompletionError::InvalidValue,
            "accepted invalid value {value:?}"
        );
    }
}

#[test]
fn completion_rejects_duplicate_names_and_output_count_overflow() {
    let mut completion = JobCompletion::new("indexed transcript");
    for index in 0..MAX_JOB_OUTPUTS {
        completion = completion
            .with_output(format!("output_{index}"), index.to_string())
            .expect("output at count bound");
    }
    assert_eq!(completion.outputs().len(), MAX_JOB_OUTPUTS);

    assert_eq!(
        completion
            .clone()
            .with_output("output_0", "replacement")
            .unwrap_err(),
        JobCompletionError::DuplicateName
    );
    assert_eq!(
        completion.with_output("overflow", "8").unwrap_err(),
        JobCompletionError::TooManyOutputs
    );
}

#[test]
fn completion_validation_errors_are_bounded_and_never_echo_input() {
    const HOSTILE: &str = "HOSTILE_INPUT_MUST_NOT_APPEAR";
    for error in [
        JobCompletionError::EmptyName,
        JobCompletionError::NameTooLong,
        JobCompletionError::InvalidName,
        JobCompletionError::ValueTooLong,
        JobCompletionError::InvalidValue,
        JobCompletionError::DuplicateName,
        JobCompletionError::TooManyOutputs,
    ] {
        assert!(
            error.to_string().len() <= 80,
            "unbounded validation error: {error}"
        );
    }

    let errors = [
        JobOutput::new(format!("a{}{HOSTILE}", "x".repeat(64)), "").unwrap_err(),
        JobOutput::new(format!("A{HOSTILE}"), "").unwrap_err(),
        JobOutput::new("value", format!("{HOSTILE}\n")).unwrap_err(),
        JobOutput::new("value", format!("{HOSTILE}{}", "x".repeat(512))).unwrap_err(),
        JobCompletion::new("detail")
            .with_output("same", "first")
            .unwrap()
            .with_output("same", HOSTILE)
            .unwrap_err(),
    ];

    for error in errors {
        let message = error.to_string();
        assert!(message.len() <= 80, "unbounded validation error: {message}");
        assert!(!message.contains(HOSTILE), "error reflected hostile input");
    }
}

#[test]
fn ordinary_spawn_completes_with_empty_outputs() {
    let (manager, rx) = manager_with_events();
    let id = manager.spawn("ordinary", |_| Ok("complete".into()));
    let history = wait_history(&rx, id);

    assert_eq!(history.last().unwrap().state, JobState::Done);
    assert!(history.iter().all(|snapshot| snapshot.outputs.is_empty()));
    assert!(manager.get(id).unwrap().outputs.is_empty());
}

#[test]
fn structured_completion_outputs_appear_only_in_terminal_done_snapshot() {
    let (manager, rx) = manager_with_events();
    let id = manager.spawn_with_completion("structured", |context| {
        context.set_progress(0.5, "indexing");
        let completion = JobCompletion::new("indexed\ntranscript");
        assert_eq!(completion.detail(), "indexed\ntranscript");
        Ok(completion
            .with_output("content_key", "transcript:clip-7")?
            .with_output("segment_count", "12")?)
    });
    let history = wait_history(&rx, id);

    assert_eq!(
        history
            .iter()
            .map(|snapshot| snapshot.state)
            .collect::<Vec<_>>(),
        [
            JobState::Queued,
            JobState::Running,
            JobState::Running,
            JobState::Done,
        ]
    );
    assert!(
        history[..history.len() - 1]
            .iter()
            .all(|snapshot| snapshot.outputs.is_empty())
    );
    let done = history.last().unwrap();
    assert_eq!(done.detail, "indexed transcript");
    assert_eq!(
        done.outputs,
        [
            JobOutput::new("content_key", "transcript:clip-7").unwrap(),
            JobOutput::new("segment_count", "12").unwrap(),
        ]
    );
    assert_eq!(manager.get(id).unwrap().outputs, done.outputs);
}

#[test]
fn accepted_cancellation_discards_returned_completion_outputs() {
    let (manager, rx) = manager_with_events();
    let (ready_tx, ready_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let id = manager.spawn_with_completion("cancel outputs", move |_| {
        ready_tx.send(()).unwrap();
        release_rx.recv().unwrap();
        Ok(JobCompletion::new("prepared").with_output("content_key", "must-not-publish")?)
    });

    ready_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("job did not reach cancellation gate");
    assert!(manager.get(id).unwrap().outputs.is_empty());
    assert!(manager.cancel(id));
    release_tx.send(()).unwrap();

    let history = wait_history(&rx, id);
    let terminal = history.last().unwrap();
    assert_eq!(terminal.state, JobState::Cancelled);
    assert!(terminal.outputs.is_empty());
    assert!(history.iter().all(|snapshot| snapshot.outputs.is_empty()));
    assert!(manager.get(id).unwrap().outputs.is_empty());
}

#[test]
fn commit_boundary_and_rejected_late_cancel_preserve_outputs() {
    let (manager, rx) = manager_with_events();
    let (committing_tx, committing_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let id = manager.spawn_with_completion("commit outputs", move |context| {
        assert!(context.try_begin_commit());
        committing_tx.send(()).unwrap();
        release_rx.recv().unwrap();
        Ok(JobCompletion::new("published").with_output("content_key", "committed")?)
    });

    committing_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("job did not cross commit boundary");
    let committing = manager.get(id).unwrap();
    assert_eq!(committing.state, JobState::Running);
    assert!(!committing.cancellable);
    assert!(committing.outputs.is_empty());
    assert!(!manager.cancel(id));
    release_tx.send(()).unwrap();

    let history = wait_history(&rx, id);
    assert!(
        history[..history.len() - 1]
            .iter()
            .all(|snapshot| snapshot.outputs.is_empty())
    );
    let terminal = history.last().unwrap();
    assert_eq!(terminal.state, JobState::Done);
    assert_eq!(
        terminal.outputs,
        [JobOutput::new("content_key", "committed").unwrap()]
    );
}

#[test]
fn structured_failure_and_panic_publish_no_outputs() {
    let (manager, rx) = manager_with_events();
    let failed_id = manager.spawn_with_completion("structured failure", |context| {
        assert!(context.try_begin_commit());
        Err("publication failed".into())
    });
    let failed = wait_history(&rx, failed_id);
    assert_eq!(failed.last().unwrap().state, JobState::Failed);
    assert!(failed.iter().all(|snapshot| snapshot.outputs.is_empty()));

    let panic_id = manager.spawn_with_completion(
        "structured panic",
        |context| -> Result<JobCompletion, String> {
            assert!(context.try_begin_commit());
            panic!("publication panicked");
        },
    );
    let panicked = wait_history(&rx, panic_id);
    assert_eq!(panicked.last().unwrap().state, JobState::Failed);
    assert!(panicked.iter().all(|snapshot| snapshot.outputs.is_empty()));
}

#[test]
fn retained_output_snapshots_are_immutable_clones() {
    let (manager, rx) = manager_with_events();
    let id = manager.spawn_with_completion("clone outputs", |_| {
        Ok(JobCompletion::new("done").with_output("content_key", "stable")?)
    });
    let terminal = wait_history(&rx, id).pop().unwrap();
    assert_eq!(terminal.outputs.len(), 1);

    let mut fetched = manager.get(id).unwrap();
    fetched.outputs.clear();
    assert!(fetched.outputs.is_empty());
    assert_eq!(manager.get(id).unwrap().outputs, terminal.outputs);

    let mut listed = manager.snapshot();
    listed
        .iter_mut()
        .find(|snapshot| snapshot.id == id)
        .unwrap()
        .outputs
        .clear();
    assert_eq!(manager.get(id).unwrap().outputs, terminal.outputs);
}

#[test]
fn cancellation_token_has_callback_safe_traits() {
    fn assert_traits<T: Clone + Send + Sync + 'static>() {}

    assert_traits::<CancellationToken>();
}

#[test]
fn cancellation_token_moves_into_static_callback_and_observes_cancel() {
    type CancelCallback = Arc<dyn Fn() -> bool + Send + Sync + 'static>;

    let (manager, rx) = manager_with_events();
    let (callback_tx, callback_rx) = mpsc::channel::<CancelCallback>();
    let (resume_tx, resume_rx) = mpsc::channel();
    let id = manager.spawn("callback cancellation", move |ctx| {
        let token = ctx.cancellation_token();
        let callback: CancelCallback = Arc::new(move || token.cancelled());
        if callback_tx.send(Arc::clone(&callback)).is_err() {
            return Err("callback receiver dropped".into());
        }
        resume_rx
            .recv()
            .map_err(|_| "resume sender dropped".to_owned())?;
        assert!(callback());
        Ok("callback observed cancellation".into())
    });

    let callback = callback_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("job did not publish its cancellation callback");
    assert!(!callback());
    assert!(manager.cancel(id));
    assert!(callback());
    resume_tx.send(()).unwrap();

    let terminal = wait_history(&rx, id).pop().unwrap();
    assert_eq!(terminal.state, JobState::Cancelled);
}

#[test]
fn cancellation_token_and_context_agree_before_cancellation() {
    let (manager, rx) = manager_with_events();
    let (state_tx, state_rx) = mpsc::channel();
    let (resume_tx, resume_rx) = mpsc::channel();
    let id = manager.spawn("matching cancellation state", move |ctx| {
        let token = ctx.cancellation_token();
        state_tx
            .send((ctx.cancelled(), token.cancelled()))
            .map_err(|_| "state receiver dropped".to_owned())?;
        resume_rx
            .recv()
            .map_err(|_| "resume sender dropped".to_owned())?;
        Ok("states matched".into())
    });

    let (context_cancelled, token_cancelled) = state_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("job did not publish cancellation state");
    assert_eq!(context_cancelled, token_cancelled);
    assert!(!context_cancelled);
    resume_tx.send(()).unwrap();
    assert_eq!(wait_history(&rx, id).last().unwrap().state, JobState::Done);
}

#[test]
fn cancellation_tokens_remain_read_only_after_terminal_completion() {
    let (manager, rx) = manager_with_events();

    let (uncancelled_tx, uncancelled_rx) = mpsc::channel();
    let uncancelled_id = manager.spawn("uncancelled token", move |ctx| {
        uncancelled_tx
            .send(ctx.cancellation_token())
            .map_err(|_| "token receiver dropped".to_owned())?;
        Ok("finished normally".into())
    });
    let uncancelled = uncancelled_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("uncancelled job did not publish its token");
    assert_eq!(
        wait_history(&rx, uncancelled_id).last().unwrap().state,
        JobState::Done
    );
    assert!(!uncancelled.cancelled());

    let (cancelled_tx, cancelled_rx) = mpsc::channel();
    let (resume_tx, resume_rx) = mpsc::channel();
    let cancelled_id = manager.spawn("cancelled token", move |ctx| {
        cancelled_tx
            .send(ctx.cancellation_token())
            .map_err(|_| "token receiver dropped".to_owned())?;
        resume_rx
            .recv()
            .map_err(|_| "resume sender dropped".to_owned())?;
        Ok("stopped".into())
    });
    let cancelled = cancelled_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("cancelled job did not publish its token");
    assert!(!cancelled.cancelled());
    assert!(manager.cancel(cancelled_id));
    resume_tx.send(()).unwrap();
    assert_eq!(
        wait_history(&rx, cancelled_id).last().unwrap().state,
        JobState::Cancelled
    );
    assert!(cancelled.cancelled());

    // Neither token keeps the context or registry alive, and both retain
    // their final read-only view after those owners are gone.
    drop(manager);
    assert!(!uncancelled.cancelled());
    assert!(cancelled.cancelled());
}

#[test]
fn cancellation_wins_before_commit_boundary_and_prevents_commit() {
    let (manager, rx) = manager_with_events();
    let (ready_tx, ready_rx) = mpsc::channel();
    let (resume_tx, resume_rx) = mpsc::channel();
    let (boundary_tx, boundary_rx) = mpsc::channel();
    let (finish_tx, finish_rx) = mpsc::channel();
    let committed = Arc::new(AtomicBool::new(false));
    let committed_in_job = Arc::clone(&committed);
    let id = manager.spawn("cancel before commit", move |ctx| {
        let token = ctx.cancellation_token();
        ready_tx.send(()).unwrap();
        resume_rx.recv().unwrap();

        let may_commit = ctx.try_begin_commit();
        boundary_tx
            .send((may_commit, ctx.cancelled(), token.cancelled()))
            .unwrap();
        if may_commit {
            committed_in_job.store(true, Ordering::Relaxed);
        }
        finish_rx.recv().unwrap();
        Ok("preparation stopped".into())
    });

    ready_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("job did not reach cancellable preparation");
    let running = manager.get(id).unwrap();
    assert_eq!(running.state, JobState::Running);
    assert!(running.cancellable);
    assert!(!running.cancel_requested);

    assert!(manager.cancel(id));
    let requested = manager.get(id).unwrap();
    assert!(requested.cancellable);
    assert!(requested.cancel_requested);
    resume_tx.send(()).unwrap();

    assert_eq!(
        boundary_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("job did not attempt commit"),
        (false, true, true)
    );
    let still_cancelled = manager.get(id).unwrap();
    assert_eq!(still_cancelled.state, JobState::Running);
    assert!(still_cancelled.cancellable);
    assert!(still_cancelled.cancel_requested);
    assert!(!committed.load(Ordering::Relaxed));
    finish_tx.send(()).unwrap();

    let history = wait_history(&rx, id);
    let states: Vec<JobState> = history.iter().map(|event| event.state).collect();
    assert_eq!(
        states,
        [JobState::Queued, JobState::Running, JobState::Cancelled]
    );
    let terminal = history.last().unwrap();
    assert!(terminal.cancel_requested);
}

#[test]
fn commit_boundary_wins_before_cancel_and_preserves_false_token() {
    let (manager, rx) = manager_with_events();
    let (boundary_tx, boundary_rx) = mpsc::channel();
    let (resume_tx, resume_rx) = mpsc::channel();
    let (observed_tx, observed_rx) = mpsc::channel();
    let committed = Arc::new(AtomicBool::new(false));
    let committed_in_job = Arc::clone(&committed);
    let id = manager.spawn("commit before cancel", move |ctx| {
        let token = ctx.cancellation_token();
        assert!(ctx.try_begin_commit());
        boundary_tx.send(token.clone()).unwrap();
        resume_rx.recv().unwrap();
        observed_tx
            .send((ctx.cancelled(), token.cancelled()))
            .unwrap();
        committed_in_job.store(true, Ordering::Relaxed);
        Ok("published".into())
    });

    let token = boundary_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("job did not cross commit boundary");
    let committing = manager.get(id).unwrap();
    assert_eq!(committing.state, JobState::Running);
    assert!(!committing.cancellable);
    assert!(!committing.cancel_requested);
    assert!(!token.cancelled());

    assert!(!manager.cancel(id));
    assert!(!manager.cancel(id));
    assert!(!token.cancelled());
    let after_rejection = manager.get(id).unwrap();
    assert!(!after_rejection.cancellable);
    assert!(!after_rejection.cancel_requested);

    resume_tx.send(()).unwrap();
    assert_eq!(
        observed_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("job did not report post-boundary cancellation state"),
        (false, false)
    );
    let terminal = wait_history(&rx, id).pop().unwrap();
    assert_eq!(terminal.state, JobState::Done);
    assert!(!terminal.cancel_requested);
    assert!(committed.load(Ordering::Relaxed));
    assert!(!token.cancelled());
}

#[test]
fn cancel_and_commit_boundary_race_has_exactly_one_winner() {
    const ROUNDS: usize = 128;

    let (manager, rx) = manager_with_events();
    for round in 0..ROUNDS {
        let start = Arc::new(std::sync::Barrier::new(3));
        let worker_start = Arc::clone(&start);
        let (ready_tx, ready_rx) = mpsc::channel();
        let (boundary_tx, boundary_rx) = mpsc::channel();
        let id = manager.spawn(format!("commit race {round}"), move |ctx| {
            ready_tx.send(()).unwrap();
            worker_start.wait();
            let boundary_won = ctx.try_begin_commit();
            boundary_tx.send(boundary_won).unwrap();
            Ok(if boundary_won {
                "committed".into()
            } else {
                "cancelled".into()
            })
        });

        ready_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("worker did not reach race gate");
        let cancel_manager = manager.clone();
        let canceller_start = Arc::clone(&start);
        let canceller = thread::spawn(move || {
            canceller_start.wait();
            cancel_manager.cancel(id)
        });

        start.wait();
        let boundary_won = boundary_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("worker did not report race result");
        let cancel_won = canceller.join().expect("canceller panicked");
        assert_eq!(
            boundary_won, !cancel_won,
            "round {round}: cancel={cancel_won}, boundary={boundary_won}"
        );

        let terminal = wait_history(&rx, id).pop().unwrap();
        assert_eq!(
            terminal.state,
            if boundary_won {
                JobState::Done
            } else {
                JobState::Cancelled
            },
            "round {round}"
        );
        assert_eq!(terminal.cancel_requested, cancel_won, "round {round}");
    }
}

#[test]
fn failure_and_panic_after_commit_boundary_remain_failed() {
    let (manager, rx) = manager_with_events();

    let (failure_ready_tx, failure_ready_rx) = mpsc::channel();
    let (fail_tx, fail_rx) = mpsc::channel();
    let failure_id = manager.spawn("post-commit failure", move |ctx| {
        assert!(ctx.try_begin_commit());
        failure_ready_tx.send(ctx.cancellation_token()).unwrap();
        fail_rx.recv().unwrap();
        Err("publication failed".into())
    });
    let failure_token = failure_ready_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("failure job did not cross commit boundary");
    assert!(!manager.cancel(failure_id));
    assert!(!failure_token.cancelled());
    fail_tx.send(()).unwrap();
    let failed = wait_history(&rx, failure_id).pop().unwrap();
    assert_eq!(failed.state, JobState::Failed);
    assert_eq!(failed.detail, "publication failed");
    assert!(!failed.cancel_requested);
    assert!(!failure_token.cancelled());

    let (panic_ready_tx, panic_ready_rx) = mpsc::channel();
    let (panic_tx, panic_rx) = mpsc::channel();
    let panic_id = manager.spawn("post-commit panic", move |ctx| {
        assert!(ctx.try_begin_commit());
        panic_ready_tx.send(ctx.cancellation_token()).unwrap();
        panic_rx.recv().unwrap();
        panic!("post-commit panic");
    });
    let panic_token = panic_ready_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("panic job did not cross commit boundary");
    assert!(!manager.cancel(panic_id));
    assert!(!panic_token.cancelled());
    panic_tx.send(()).unwrap();
    let panicked = wait_history(&rx, panic_id).pop().unwrap();
    assert_eq!(panicked.state, JobState::Failed);
    assert_eq!(panicked.detail, "panicked: post-commit panic");
    assert!(!panicked.cancel_requested);
    assert!(!panic_token.cancelled());
}

#[test]
fn commit_boundary_is_idempotent_and_emits_one_exact_transition() {
    let (manager, rx) = manager_with_events();
    let (calls_tx, calls_rx) = mpsc::channel();
    let id = manager.spawn("idempotent commit", move |ctx| {
        let first = ctx.try_begin_commit();
        let second = ctx.try_begin_commit();
        calls_tx.send((first, second)).unwrap();
        Ok("published".into())
    });

    assert_eq!(
        calls_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("job did not report boundary calls"),
        (true, true)
    );
    let history = wait_history(&rx, id);
    let states: Vec<JobState> = history.iter().map(|event| event.state).collect();
    assert_eq!(
        states,
        [
            JobState::Queued,
            JobState::Running,
            JobState::Running,
            JobState::Done,
        ]
    );
    let cancellable: Vec<bool> = history.iter().map(|event| event.cancellable).collect();
    assert_eq!(cancellable, [true, true, false, false]);
    assert!(history.iter().all(|snapshot| !snapshot.cancel_requested));
    assert_eq!(
        history
            .iter()
            .filter(|snapshot| snapshot.state == JobState::Running && !snapshot.cancellable)
            .count(),
        1
    );
}

#[test]
fn commit_boundary_subscribers_are_reentrant_and_panic_isolated() {
    let manager = JobManager::new();
    manager.subscribe(|snapshot| {
        if snapshot.label == "boundary subscribers"
            && snapshot.state == JobState::Running
            && !snapshot.cancellable
        {
            panic!("boundary subscriber failed");
        }
    });

    let reentrant_manager = manager.clone();
    let (reentrant_tx, reentrant_rx) = mpsc::channel();
    manager.subscribe(move |snapshot| {
        if snapshot.label == "boundary subscribers"
            && snapshot.state == JobState::Running
            && !snapshot.cancellable
        {
            let get_saw_closed = reentrant_manager
                .get(snapshot.id)
                .is_some_and(|stored| !stored.cancellable);
            let list_saw_closed = reentrant_manager
                .snapshot()
                .iter()
                .any(|stored| stored.id == snapshot.id && !stored.cancellable);
            let cancel_was_rejected = !reentrant_manager.cancel(snapshot.id);
            let _ = reentrant_tx.send((get_saw_closed, list_saw_closed, cancel_was_rejected));
        }
    });

    let (healthy_tx, healthy_rx) = mpsc::channel();
    manager.subscribe(move |snapshot| {
        let _ = healthy_tx.send(snapshot);
    });

    let id = manager.spawn("boundary subscribers", |ctx| {
        assert!(ctx.try_begin_commit());
        Ok("published".into())
    });
    assert_eq!(
        reentrant_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("boundary subscriber deadlocked or was suppressed"),
        (true, true, true)
    );

    let history = wait_history(&healthy_rx, id);
    let states: Vec<JobState> = history.iter().map(|event| event.state).collect();
    assert_eq!(
        states,
        [
            JobState::Queued,
            JobState::Running,
            JobState::Running,
            JobState::Done,
        ]
    );
    assert_eq!(manager.get(id).unwrap().state, JobState::Done);
}

#[test]
fn success_path_events_in_order() {
    let (manager, rx) = manager_with_events();
    let id = manager.spawn("Exporting draft.mp4", |ctx| {
        ctx.set_progress(0.25, "frame 900/3600");
        ctx.set_progress(0.75, "frame 2700/3600");
        Ok("wrote draft.mp4".into())
    });

    let events = wait_history(&rx, id);
    let states: Vec<JobState> = events.iter().map(|e| e.state).collect();
    assert_eq!(
        states,
        [
            JobState::Queued,
            JobState::Running,
            JobState::Running,
            JobState::Running,
            JobState::Done,
        ]
    );
    let progress: Vec<Option<f32>> = events.iter().map(|e| e.progress).collect();
    assert_eq!(progress, [None, None, Some(0.25), Some(0.75), Some(1.0)]);
    assert_eq!(events[2].detail, "frame 900/3600");
    assert_eq!(events.iter().filter(|e| e.state.is_terminal()).count(), 1);

    let done = events.last().unwrap();
    assert_eq!(done.detail, "wrote draft.mp4");
    assert!(done.finished.is_some());

    let got = manager.get(id).expect("finished job still known");
    assert_eq!(got.state, JobState::Done);
    assert_eq!(got.detail, "wrote draft.mp4");
    assert_eq!(got.progress, Some(1.0));
}

#[test]
fn failure_keeps_reason_and_last_progress() {
    let (manager, rx) = manager_with_events();
    let id = manager.spawn("Analyzing clip.mov", |ctx| {
        ctx.set_progress(0.4, "scanning");
        Err("disk full".into())
    });

    let failed = wait_history(&rx, id).pop().unwrap();
    assert_eq!(failed.state, JobState::Failed);
    assert_eq!(failed.detail, "disk full");
    // No snap-to-1.0 on failure — the bar stays where the job died.
    assert_eq!(failed.progress, Some(0.4));
    assert!(failed.finished.is_some());
}

#[test]
fn cancel_flag_decides_over_ok_return() {
    let (manager, rx) = manager_with_events();
    let (started_tx, started_rx) = mpsc::channel();
    let (resume_tx, resume_rx) = mpsc::channel();
    let id = manager.spawn("Transcribing interview.mov", move |ctx| {
        started_tx.send(()).unwrap();
        // Deterministic rendezvous: the test cancels while we wait here.
        resume_rx.recv().unwrap();
        assert!(ctx.cancelled());
        Ok("stopped at 00:42".into())
    });

    started_rx.recv().unwrap();
    assert_eq!(manager.get(id).unwrap().state, JobState::Running);
    // Cancel through a clone: all clones share one registry.
    assert!(manager.clone().cancel(id));
    assert!(manager.get(id).unwrap().cancel_requested);
    resume_tx.send(()).unwrap();

    let last = wait_history(&rx, id).pop().unwrap();
    assert_eq!(last.state, JobState::Cancelled);
    assert_eq!(last.detail, "stopped at 00:42");
    assert!(last.cancel_requested);

    // Finished and unknown jobs both refuse.
    assert!(!manager.cancel(id));
    assert!(!manager.cancel(JobId(9999)));
}

#[test]
fn cancel_flag_decides_over_err_return() {
    let (manager, rx) = manager_with_events();
    let (started_tx, started_rx) = mpsc::channel();
    let (resume_tx, resume_rx) = mpsc::channel();
    let id = manager.spawn("Exporting", move |_| {
        started_tx.send(()).unwrap();
        resume_rx.recv().unwrap();
        Err("aborted mid-write".into())
    });

    started_rx.recv().unwrap();
    assert!(manager.cancel(id));
    resume_tx.send(()).unwrap();

    let last = wait_history(&rx, id).pop().unwrap();
    assert_eq!(last.state, JobState::Cancelled);
    assert_eq!(last.detail, "aborted mid-write");
}

#[test]
fn panic_becomes_failed() {
    let (manager, rx) = manager_with_events();

    // Literal panic → &str payload.
    let id = manager.spawn("Rendering", |_| panic!("codec exploded"));
    let failed = wait_history(&rx, id).pop().unwrap();
    assert_eq!(failed.state, JobState::Failed);
    assert_eq!(failed.detail, "panicked: codec exploded");
    assert!(failed.finished.is_some());

    // Formatted panic → String payload.
    let id = manager.spawn("Rendering", |_| panic!("frame {} bad", 7));
    let failed = wait_history(&rx, id).pop().unwrap();
    assert_eq!(failed.detail, "panicked: frame 7 bad");
}

#[test]
fn progress_clamps_and_is_visible_mid_flight() {
    let (manager, rx) = manager_with_events();
    let (ready_tx, ready_rx) = mpsc::channel();
    let (resume_tx, resume_rx) = mpsc::channel();
    let id = manager.spawn("Clamping", move |ctx| {
        ctx.set_progress(1.5, "over");
        ready_tx.send(()).unwrap();
        resume_rx.recv().unwrap();
        ctx.set_progress(-0.5, "under");
        ctx.set_progress(f32::NAN, "nan");
        // Err so the terminal state keeps the last progress unsnapped.
        Err("checked".into())
    });

    ready_rx.recv().unwrap();
    let mid = manager.get(id).unwrap();
    assert_eq!(mid.state, JobState::Running);
    assert_eq!(mid.progress, Some(1.0));
    assert_eq!(mid.detail, "over");
    resume_tx.send(()).unwrap();

    let events = wait_history(&rx, id);
    let progress: Vec<Option<f32>> = events.iter().map(|e| e.progress).collect();
    assert_eq!(
        progress,
        [None, None, Some(1.0), Some(0.0), Some(0.0), Some(0.0)]
    );
}

#[test]
fn snapshot_orders_newest_first() {
    let (manager, rx) = manager_with_events();
    let (gate_a_tx, gate_a_rx) = mpsc::channel::<()>();
    let a = manager.spawn("a", move |_| {
        let _ = gate_a_rx.recv();
        Ok("a done".into())
    });
    let (gate_b_tx, gate_b_rx) = mpsc::channel::<()>();
    let b = manager.spawn("b", move |_| {
        let _ = gate_b_rx.recv();
        Ok("b done".into())
    });
    let c = manager.spawn("c", |_| Ok("c done".into()));
    wait_history(&rx, c);

    // Newest spawned first, regardless of running/finished state.
    let ids: Vec<JobId> = manager.snapshot().iter().map(|s| s.id).collect();
    assert_eq!(ids, [c, b, a]);

    // Release sequentially so each terminal event is awaited in turn.
    drop(gate_a_tx);
    wait_history(&rx, a);
    drop(gate_b_tx);
    wait_history(&rx, b);
}

#[test]
fn prune_keeps_newest_finished_never_running() {
    let (manager, rx) = manager_with_events();

    // Oldest job of all, still running while everything else finishes.
    let (gate_tx, gate_rx) = mpsc::channel::<()>();
    let runner = manager.spawn("long export", move |_| {
        let _ = gate_rx.recv();
        Ok("finally".into())
    });

    // Finish KEEP_FINISHED + 6 quick jobs strictly one after another, so
    // finish order matches spawn order and the prune is deterministic.
    let mut quick = Vec::new();
    for n in 0..KEEP_FINISHED + 6 {
        let id = manager.spawn(format!("quick {n}"), |_| Ok("done".into()));
        wait_history(&rx, id);
        quick.push(id);
    }

    // The six oldest-finished are gone; the running job survives even
    // though it is the oldest job in the registry.
    assert_eq!(manager.snapshot().len(), KEEP_FINISHED + 1);
    for &id in &quick[..6] {
        assert!(manager.get(id).is_none(), "expected {id} pruned");
    }
    for &id in &quick[6..] {
        assert!(manager.get(id).is_some());
    }
    assert_eq!(manager.get(runner).unwrap().state, JobState::Running);

    // The runner finishes last → newest-finished → survives the prune
    // its own completion triggers; the next-oldest quick job goes. Under
    // spawn-order pruning the runner would have been dropped instead.
    drop(gate_tx);
    wait_history(&rx, runner);
    assert_eq!(manager.get(runner).unwrap().state, JobState::Done);
    assert!(manager.get(quick[6]).is_none());
    assert_eq!(manager.snapshot().len(), KEEP_FINISHED);
}

#[test]
fn multiple_subscribers_all_notified() {
    let manager = JobManager::new();
    let (tx1, rx1) = mpsc::channel();
    let (tx2, rx2) = mpsc::channel();
    manager.subscribe(move |s| {
        let _ = tx1.send(s.state);
    });
    manager.subscribe(move |s| {
        let _ = tx2.send(s.state);
    });

    manager.spawn("observed", |_| Ok("ok".into()));
    for rx in [rx1, rx2] {
        loop {
            let state = rx
                .recv_timeout(Duration::from_secs(10))
                .expect("subscriber missed events");
            if state == JobState::Done {
                break;
            }
        }
    }
}

#[test]
fn queued_subscriber_panic_does_not_block_spawn_or_lifecycle() {
    let manager = JobManager::new();
    manager.subscribe(|snapshot| {
        if snapshot.state == JobState::Queued {
            panic!("queued subscriber failed");
        }
    });
    let (healthy_tx, healthy_rx) = mpsc::channel();
    manager.subscribe(move |snapshot| {
        let _ = healthy_tx.send(snapshot);
    });

    // Queued delivery is synchronous, so reaching this assignment proves
    // the first subscriber's panic did not unwind `spawn`.
    let id = manager.spawn("observed", |ctx| {
        ctx.set_progress(0.5, "halfway");
        Ok("complete".into())
    });
    let history = wait_history(&healthy_rx, id);
    let states: Vec<JobState> = history.iter().map(|event| event.state).collect();
    assert_eq!(
        states,
        [
            JobState::Queued,
            JobState::Running,
            JobState::Running,
            JobState::Done,
        ]
    );
    assert_eq!(manager.get(id).unwrap().state, JobState::Done);
}

#[test]
fn panics_on_worker_notifications_do_not_strand_job() {
    let manager = JobManager::new();
    let running_panics = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let terminal_panics = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let running_panics_in_callback = Arc::clone(&running_panics);
    let terminal_panics_in_callback = Arc::clone(&terminal_panics);
    manager.subscribe(move |snapshot| {
        if snapshot.state == JobState::Running {
            running_panics_in_callback.fetch_add(1, Ordering::Relaxed);
            panic!("running/progress subscriber failed");
        }
        if snapshot.state.is_terminal() {
            terminal_panics_in_callback.fetch_add(1, Ordering::Relaxed);
            panic!("terminal subscriber failed");
        }
    });
    let (healthy_tx, healthy_rx) = mpsc::channel();
    manager.subscribe(move |snapshot| {
        let _ = healthy_tx.send(snapshot);
    });

    let id = manager.spawn("observed", |ctx| {
        ctx.set_progress(0.5, "halfway");
        Ok("complete".into())
    });
    let history = wait_history(&healthy_rx, id);
    let states: Vec<JobState> = history.iter().map(|event| event.state).collect();
    assert_eq!(
        states,
        [
            JobState::Queued,
            JobState::Running,
            JobState::Running,
            JobState::Done,
        ]
    );
    assert_eq!(
        history
            .iter()
            .filter(|event| event.state.is_terminal())
            .count(),
        1
    );
    assert!(healthy_rx.try_recv().is_err());
    assert_eq!(running_panics.load(Ordering::Relaxed), 2);
    assert_eq!(terminal_panics.load(Ordering::Relaxed), 1);
    assert_eq!(manager.get(id).unwrap().state, JobState::Done);
}

#[test]
fn subscriber_can_query_cancel_and_spawn_reentrantly() {
    let manager = JobManager::new();
    let (event_tx, event_rx) = mpsc::channel();
    manager.subscribe(move |snapshot| {
        let _ = event_tx.send(snapshot);
    });

    let reentrant_manager = manager.clone();
    let (reentrant_tx, reentrant_rx) = mpsc::channel();
    manager.subscribe(move |snapshot| {
        if snapshot.label == "outer" && snapshot.state == JobState::Queued {
            let get_saw_queued = reentrant_manager
                .get(snapshot.id)
                .is_some_and(|stored| stored.state == JobState::Queued);
            let snapshot_saw_job = reentrant_manager
                .snapshot()
                .iter()
                .any(|stored| stored.id == snapshot.id);
            let cancelled = reentrant_manager.cancel(snapshot.id);
            let inner = reentrant_manager.spawn("inner", |_| Ok("inner done".into()));
            let _ = reentrant_tx.send((get_saw_queued, snapshot_saw_job, cancelled, inner));
        }
    });

    let outer = manager.spawn("outer", |ctx| {
        assert!(ctx.cancelled());
        Ok("outer stopped".into())
    });
    let (get_saw_queued, snapshot_saw_job, cancelled, inner) = reentrant_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("reentrant subscriber did not finish");
    assert!(get_saw_queued);
    assert!(snapshot_saw_job);
    assert!(cancelled);

    let mut terminals = Vec::new();
    while terminals.len() < 2 {
        let snapshot = event_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("timed out waiting for reentrant jobs");
        if snapshot.state.is_terminal() {
            terminals.push((snapshot.id, snapshot.state));
        }
    }
    assert!(terminals.contains(&(inner, JobState::Done)));
    assert!(terminals.contains(&(outer, JobState::Cancelled)));
}

#[test]
fn retained_text_is_safe_bounded_and_deterministic() {
    assert_eq!(
        sanitize_text("a\nb\rc\td\0e\u{2028}f\u{2029}g", MAX_JOB_DETAIL_BYTES),
        "a b c d e f g"
    );

    let label = oversized_text("label:", MAX_JOB_LABEL_BYTES);
    let progress = oversized_text("progress:", MAX_JOB_DETAIL_BYTES);
    let summary = oversized_text("summary:", MAX_JOB_DETAIL_BYTES);
    let expected_label = sanitize_label(&label);
    let expected_progress = sanitize_detail(&progress);
    let expected_summary = sanitize_detail(&summary);
    for (value, cap) in [
        (&expected_label, MAX_JOB_LABEL_BYTES),
        (&expected_progress, MAX_JOB_DETAIL_BYTES),
        (&expected_summary, MAX_JOB_DETAIL_BYTES),
    ] {
        assert_one_line_bounded(value, cap);
        assert!(value.ends_with(ELLIPSIS));
    }
    assert_eq!(sanitize_label(&label), expected_label);
    assert_eq!(sanitize_label(&expected_label), expected_label);
    assert_eq!(sanitize_detail(&progress), expected_progress);
    assert_eq!(sanitize_detail(&expected_progress), expected_progress);
    assert_eq!(sanitize_detail(&summary), expected_summary);
    assert_eq!(sanitize_detail(&expected_summary), expected_summary);

    let (manager, rx) = manager_with_events();
    let (name_tx, name_rx) = mpsc::channel();
    let id = manager.spawn(label, move |ctx| {
        name_tx
            .send(thread::current().name().unwrap_or("").to_owned())
            .unwrap();
        ctx.set_progress(0.5, progress);
        Ok(summary)
    });
    let actual_thread_name = name_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("job did not report its thread name");
    let history = wait_history(&rx, id);
    let states: Vec<JobState> = history.iter().map(|event| event.state).collect();
    assert_eq!(
        states,
        [
            JobState::Queued,
            JobState::Running,
            JobState::Running,
            JobState::Done,
        ]
    );
    assert_eq!(actual_thread_name, thread_name(&expected_label));
    assert!(actual_thread_name.len() <= MAX_THREAD_NAME_BYTES);
    for snapshot in &history {
        assert_eq!(snapshot.label, expected_label);
        assert_one_line_bounded(&snapshot.label, MAX_JOB_LABEL_BYTES);
        assert_one_line_bounded(&snapshot.detail, MAX_JOB_DETAIL_BYTES);
    }
    assert_eq!(history[2].detail, expected_progress);
    assert_eq!(history[3].detail, expected_summary);

    let error = oversized_text("error:", MAX_JOB_DETAIL_BYTES);
    let expected_error = sanitize_detail(&error);
    let error_id = manager.spawn("error job", move |_| Err(error));
    let error_history = wait_history(&rx, error_id);
    let failed = error_history.last().unwrap();
    assert_eq!(failed.state, JobState::Failed);
    assert_eq!(failed.detail, expected_error);
    assert_one_line_bounded(&failed.detail, MAX_JOB_DETAIL_BYTES);
    assert_eq!(sanitize_detail(&failed.detail), failed.detail);

    let panic_payload = oversized_text("panic:", MAX_JOB_DETAIL_BYTES);
    let expected_panic = sanitize_detail(&format!("panicked: {panic_payload}"));
    let panic_id = manager.spawn("panic job", move |_| {
        std::panic::panic_any(panic_payload);
    });
    let panic_history = wait_history(&rx, panic_id);
    let panicked = panic_history.last().unwrap();
    assert_eq!(panicked.state, JobState::Failed);
    assert_eq!(panicked.detail, expected_panic);
    assert_one_line_bounded(&panicked.detail, MAX_JOB_DETAIL_BYTES);
    assert_eq!(sanitize_detail(&panicked.detail), panicked.detail);
}

#[test]
fn thread_names_are_prefixed_and_truncated() {
    assert_eq!(thread_name("Export"), "job: Export");

    let long = thread_name("Transcribing a very long interview recording.mov");
    assert!(long.len() <= 32);
    assert!(long.starts_with("job: Transcribing"));

    // Multi-byte labels truncate on a char boundary, not mid-codepoint.
    let multibyte = thread_name("动画渲染动画渲染动画渲染动画渲染");
    assert!(multibyte.len() <= 32);

    // And the wiring: the job really runs under that name.
    let (manager, rx) = manager_with_events();
    let (name_tx, name_rx) = mpsc::channel();
    let id = manager.spawn("Exporting draft.mp4", move |_| {
        let name = thread::current().name().unwrap_or("").to_string();
        name_tx.send(name).unwrap();
        Ok(String::new())
    });
    assert_eq!(name_rx.recv().unwrap(), "job: Exporting draft.mp4");
    wait_history(&rx, id);
}

#[test]
fn ids_are_unique_and_display_plainly() {
    assert_eq!(JobId::from_raw(7).raw(), 7);
    assert_eq!(JobId::from_raw(7).to_string(), "7");

    // Sequential spawn + drain: `wait_history` discards other jobs'
    // events, so concurrent jobs must not share one drain pass.
    let (manager, rx) = manager_with_events();
    let a = manager.spawn("a", |_| Ok(String::new()));
    wait_history(&rx, a);
    let b = manager.spawn("b", |_| Ok(String::new()));
    wait_history(&rx, b);
    assert_ne!(a, b);
}
