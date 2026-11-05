use herdr_white_noise_bridge::activity::{Activity, UserInput};

fn input(id: &str, text: &str) -> UserInput {
    UserInput::new(id.into(), text)
}

#[test]
fn staggered_incomplete_native_echoes_match_repeated_submissions_one_to_one() {
    use herdr_white_noise_bridge::activity::Submission;
    let mut pending = vec![Submission::new("one", "continue", Some(&[]), 100)];
    let partial = [UserInput::pending("first".into())];
    Submission::observe(&mut pending, &partial);
    pending.push(Submission::new("two", "continue", Some(&partial), 101));
    let mut pending: Vec<Submission> =
        serde_json::from_str(&serde_json::to_string(&pending).unwrap()).unwrap();
    Submission::observe(
        &mut pending,
        &[input("first", "continue"), input("second", "continue")],
    );
    assert!(
        pending.is_empty(),
        "Each receipt has a separate eligible native message"
    );
}

#[test]
fn native_acceptance_requires_new_complete_unambiguous_inputs() {
    use herdr_white_noise_bridge::activity::Submission;
    let baseline = [input("old", "hello")];
    let submission = Submission::new("receipt", "hello", Some(&baseline), 100);
    assert!(!submission.overdue(159));
    assert!(submission.overdue(160));
    let mut pending = vec![submission];
    Submission::observe(&mut pending, &baseline);
    assert_eq!(pending.len(), 1, "Historical text is not native acceptance");
    Submission::observe(
        &mut pending,
        &[input("old", "hello"), UserInput::pending("new".into())],
    );
    assert_eq!(pending.len(), 1);
    pending[0].warned = true;
    let mut pending: Vec<Submission> =
        serde_json::from_str(&serde_json::to_string(&pending).unwrap()).unwrap();
    Submission::observe(
        &mut pending,
        &[input("old", "hello"), input("new", "hello")],
    );
    assert!(
        pending.is_empty(),
        "Late native acceptance resolves the receipt after restart"
    );
    for same_baseline in [true, false] {
        let mut pending = vec![
            Submission::new("one", "hello", Some(&[]), 100),
            Submission::new(
                "two",
                "hello",
                Some(if same_baseline { &[] } else { &baseline }),
                101,
            ),
        ];
        Submission::observe(&mut pending, &[input("new", "hello")]);
        assert_eq!(
            pending.len(),
            2,
            "A single native input cannot confirm two submissions"
        );
        Submission::observe(
            &mut pending,
            &[input("new", "hello"), input("newer", "hello")],
        );
        assert!(pending.is_empty());
    }
    let mut unknown = vec![Submission::new("unknown", "hello", None, 100)];
    Submission::observe(&mut unknown, &[input("new", "hello")]);
    assert_eq!(
        unknown.len(),
        1,
        "Without a baseline, acceptance cannot be proved"
    );
}

#[test]
fn staggered_echoes_of_multiple_phone_followups_keep_the_later_receipt() {
    let mut state = Activity::default();
    state.start_phone("one", "first", &[]);
    state.confirm_phone("one");
    state.start_phone("two", "second", &[]);
    state.confirm_phone("two");
    assert!(!state.observe(&[input("first-id", "first")]));
    let mut state: Activity =
        serde_json::from_str(&serde_json::to_string(&state).unwrap()).unwrap();
    assert!(!state.observe(&[input("first-id", "first"), input("second-id", "second")]));
    assert!(state.observe(&[
        input("first-id", "first"),
        input("second-id", "second"),
        input("local", "something else")
    ]));
}

#[test]
fn first_history_is_a_baseline_and_new_ids_are_local_even_with_old_text() {
    let mut state = Activity::default();
    assert!(!state.observe(&[input("old", "hello")]));
    assert!(state.observe(&[input("old", "hello"), input("new", "hello")]));
    assert!(!state.observe(&[input("old", "hello"), input("new", "hello")]));
}

#[test]
fn only_one_confirmed_unambiguous_phone_echo_is_exempted() {
    let mut state = Activity::default();
    state.start_phone("receipt", "hello\r\nworld", &[]);
    state.confirm_phone("receipt");
    let serialized = serde_json::to_string(&state).unwrap();
    assert!(!serialized.contains("hello"));
    let mut state: Activity = serde_json::from_str(&serialized).unwrap();
    assert!(!state.observe(&[input("phone", "hello\nworld")]));
    assert!(state.observe(&[
        input("phone", "hello\nworld"),
        input("local", "hello\nworld")
    ]));
}

#[test]
fn uncertain_cancelled_and_duplicate_submissions_cannot_hide_local_activity() {
    for mode in ["uncertain", "cancelled", "duplicate"] {
        let mut state = Activity::default();
        state.start_phone("receipt", "hello", &[]);
        match mode {
            "cancelled" => state.cancel_phone("receipt"),
            "duplicate" => {
                state.confirm_phone("receipt");
                state.start_phone("other", "hello", &[]);
                state.confirm_phone("other");
            }
            _ => {}
        }
        assert!(state.observe(&[input("local", "hello")]), "{mode}");
    }
}

#[test]
fn simultaneous_same_text_messages_and_media_without_text_mute() {
    let mut state = Activity::default();
    state.start_phone("receipt", "hello", &[]);
    state.confirm_phone("receipt");
    assert!(state.observe(&[input("phone", "hello"), input("local", "hello")]));
    assert!(state.observe(&[input("image", "")]));
}

#[test]
fn historical_same_text_does_not_make_a_phone_echo_ambiguous() {
    let mut state = Activity::default();
    state.start_phone("receipt", "hello", &[input("old", "hello")]);
    state.confirm_phone("receipt");
    assert!(!state.observe(&[input("old", "hello"), input("phone", "hello")]));
}

#[test]
fn incomplete_phone_echo_waits_for_text_without_consuming_attribution() {
    let mut state = Activity::default();
    state.start_phone("receipt", "hello", &[]);
    state.confirm_phone("receipt");
    assert!(!state.observe(&[UserInput::pending("phone".into())]));
    assert!(state.unresolved());
    let mut state: Activity =
        serde_json::from_str(&serde_json::to_string(&state).unwrap()).unwrap();
    assert!(!state.observe(&[input("phone", "hello")]));
    assert!(!state.unresolved());
}

#[test]
fn concatenated_phone_text_is_not_an_exact_phone_echo_after_restart() {
    let phone = "This is just a test return with hello.";
    for combined in [
        format!("An unsent desktop draft. {phone}"),
        format!("{phone} An unsent desktop draft."),
    ] {
        let mut state = Activity::default();
        state.start_phone("receipt", phone, &[]);
        state.confirm_phone("receipt");
        let mut state: Activity =
            serde_json::from_str(&serde_json::to_string(&state).unwrap()).unwrap();
        assert!(state.observe(&[input("combined", &combined)]));
        // The mismatch consumes the receipt; a later exact match is not exempt.
        assert!(state.observe(&[input("combined", &combined), input("later", phone)]));
    }
}

#[test]
fn phone_echo_allows_line_endings_but_not_changes_inside_the_message() {
    for (echo, local) in [
        ("  first line\nsecond line\n", false),
        ("first line second line", true),
        ("first line\n\nsecond line", true),
        ("first line\nchanged second line", true),
    ] {
        let mut state = Activity::default();
        state.start_phone("receipt", "first line\r\nsecond line", &[]);
        state.confirm_phone("receipt");
        assert_eq!(state.observe(&[input("echo", echo)]), local, "{echo:?}");
    }
}

#[test]
fn matching_duplicate_phone_sends_are_attributed_one_to_one() {
    let mut state = Activity::default();
    state.start_phone("one", "hello", &[]);
    state.confirm_phone("one");
    state.start_phone("two", "hello", &[]);
    state.confirm_phone("two");
    assert!(!state.observe(&[input("first", "hello"), input("second", "hello")]));
    assert!(state.observe(&[
        input("first", "hello"),
        input("second", "hello"),
        input("local", "hello"),
    ]));
}

#[test]
fn duplicate_phone_text_does_not_poison_other_phone_attribution() {
    let mut state = Activity::default();
    for (receipt, text) in [("status", "status?"), ("one", "hello"), ("two", "hello")] {
        state.start_phone(receipt, text, &[]);
        state.confirm_phone(receipt);
    }
    assert!(!state.observe(&[
        input("status-echo", "status?"),
        input("first", "hello"),
        input("second", "hello"),
    ]));
}
