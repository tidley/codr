use herdr_white_noise_bridge::activity::{Submission, UserInput};
use serde_json::json;

#[test]
fn native_receipt_never_accepts_an_equal_text_with_a_different_id() {
    let submission = Submission::new("phone", "hello", Some(&[]), 1);
    let mut saved = serde_json::to_value(submission).unwrap();
    saved["native_message_id"] = json!("msg_codr_expected");
    let mut submissions = vec![serde_json::from_value::<Submission>(saved).unwrap()];
    Submission::observe(
        &mut submissions,
        &[UserInput::new("opencode:msg_local".into(), "hello")],
    );
    assert_eq!(
        submissions.len(),
        1,
        "A local echo must not confirm an API receipt"
    );
}

#[test]
fn native_id_survives_restart_and_confirms_without_a_terminal_baseline() {
    let submission = Submission::new("phone", "hello", None, 1);
    let mut saved = serde_json::to_value(submission).unwrap();
    saved["native_message_id"] = json!("msg_codr_expected");
    let mut submissions = vec![serde_json::from_value::<Submission>(saved).unwrap()];
    Submission::observe(
        &mut submissions,
        &[UserInput::new("opencode:msg_codr_expected".into(), "hello")],
    );
    assert!(submissions.is_empty());
}

#[test]
fn a_native_api_echo_cannot_also_confirm_an_old_terminal_submission() {
    let terminal = Submission::new("old-terminal", "hello", Some(&[]), 1);
    let mut native = Submission::new("phone", "hello", None, 2);
    native.native_message_id = Some("msg_codr_expected".into());
    let mut submissions = vec![terminal, native];
    Submission::observe(
        &mut submissions,
        &[UserInput::new("opencode:msg_codr_expected".into(), "hello")],
    );
    assert_eq!(submissions.len(), 1);
    assert_eq!(submissions[0].receipt, "old-terminal");
}

#[test]
fn a_native_id_with_different_text_is_not_accepted() {
    let mut submission = Submission::new("phone", "hello", None, 1);
    submission.native_message_id = Some("msg_codr_expected".into());
    let mut submissions = vec![submission];
    Submission::observe(
        &mut submissions,
        &[UserInput::new(
            "opencode:msg_codr_expected".into(),
            "different",
        )],
    );
    assert_eq!(submissions.len(), 1);
}
