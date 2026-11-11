use herdr_white_noise_bridge::output::permission::{self, Reply};
use serde_json::{Value, json};

fn request() -> Value {
    json!({"id":"per_fixture","sessionID":"ses_fixture","action":"shell","resources":["git commit -m 'Fix bug'"],
        "source":{"type":"tool","messageID":"msg_fixture","id":"call_fixture"},"metadata":{"command":"git commit -m 'Fix bug'"}})
}

#[test]
fn permission_codes_bind_every_request_field_and_never_offer_persistent_approval() {
    let request = request();
    let permission = permission::from_response(&json!({"data":[request]}), "ses_fixture")
        .unwrap()
        .unwrap();
    assert_eq!(permission.token().len(), 17);
    assert!(!permission.text().contains("Always allow"));
    for (field, value) in [
        ("id", json!("per_other")),
        ("resources", json!(["rm file"])),
        ("metadata", json!({"command":"rm file"})),
        (
            "source",
            json!({"type":"tool","messageID":"msg_other","id":"call_fixture"}),
        ),
        ("save", json!(["git *"])),
    ] {
        let mut changed = request.clone();
        changed[field] = value;
        let changed = permission::from_response(&json!({"data":[changed]}), "ses_fixture")
            .unwrap()
            .unwrap();
        assert_ne!(permission.token(), changed.token(), "{field}");
    }
}

#[test]
fn malformed_foreign_ambiguous_or_misleading_permissions_fail_closed() {
    assert!(
        permission::from_response(&json!({"data":[]}), "ses_fixture")
            .unwrap()
            .is_none()
    );
    assert!(
        permission::from_response(&json!({"data":[request(),request()]}), "ses_fixture").is_err()
    );
    for (field, value) in [
        ("id", json!("per_../other")),
        ("sessionID", json!("ses_other")),
        ("source", Value::Null),
        ("resources", json!([])),
        ("resources", json!(["x".repeat(6001)])),
        ("resources", json!(["echo \u{1b}[2J"])),
        ("resources", json!(["echo \u{202e}hidden"])),
        ("resources", json!(["echo ```hidden```"])),
    ] {
        let mut changed = request();
        changed[field] = value;
        assert!(
            permission::from_response(&json!({"data":[changed]}), "ses_fixture").is_err(),
            "{field}"
        );
    }
}

#[test]
fn permission_card_displays_literal_command_metadata_and_all_resources() {
    let mut request = request();
    request["resources"] = json!(["git commit *"]);
    request["metadata"]["command"] = json!("git commit -m '[safe](actual text)'\nprintf done");
    let permission = permission::from_response(&json!({"data":[request]}), "ses_fixture")
        .unwrap()
        .unwrap();
    let text = permission.text();
    assert!(text.contains("```text\n"));
    assert!(text.contains("git commit *"));
    assert!(text.contains("git commit -m '[safe](actual text)'\nprintf done"));
    assert!(text.contains("Requested access:"));
}

#[test]
fn phone_permission_card_has_clear_actions_without_empty_metadata_or_repeated_ids() {
    let mut request = request();
    let long_command = format!("git diff -- {}", "src/long-directory-name/".repeat(5));
    request["resources"] = json!([long_command, "git remote -v"]);
    request["metadata"] = Value::Null;
    let permission = permission::from_response(&json!({"data":[request]}), "ses_fixture")
        .unwrap()
        .unwrap();
    let text = permission.text();
    assert!(text.starts_with("**Run these commands?**\n\n"));
    assert!(
        text.contains(&long_command),
        "Show the entire command without truncation"
    );
    assert!(text.contains("```text\n"));
    assert!(!text.contains("Metadata"));
    assert!(!text.contains("Resources:"));
    assert!(!text.contains("null"));
    assert!(!text.contains("Permission ["));
    assert!(!text.contains(&permission.token()));
    assert!(text.contains("Send a normal message"));
    assert!(!text.contains("Reply to this message"));
    assert!(text.contains("1. Allow once"));
    assert!(text.contains("2. Deny"));
    assert!(text.contains("Saved permissions stay unchanged."));
}

#[test]
fn phone_permission_card_deduplicates_commands_but_preserves_other_details() {
    let mut request = request();
    request["metadata"]["cwd"] = json!("/workspace/review");
    request["metadata"]["note"] = json!("[keep this literal](not-a-link)");
    let permission = permission::from_response(&json!({"data":[request]}), "ses_fixture")
        .unwrap()
        .unwrap();
    let text = permission.text();
    assert!(text.starts_with("**Run this command?**"));
    assert_eq!(text.matches("git commit -m 'Fix bug'").count(), 1);
    assert!(text.contains("**Details**"));
    assert!(text.contains("/workspace/review"));
    assert!(text.contains("[keep this literal](not-a-link)"));
}

#[test]
fn phone_permission_commands_preserve_quoted_boundary_whitespace() {
    for line in [" middle ", " middle", "middle ", "   "] {
        let mut request = request();
        request["resources"] = json!([format!("printf '%s' 'first\n{line}\nlast'")]);
        request["metadata"] = Value::Null;
        let permission = permission::from_response(&json!({"data":[request]}), "ses_fixture")
            .unwrap()
            .unwrap();
        assert!(
            permission.text().contains(&format!("\n{line}\n")),
            "Whitespace changed for {line:?}"
        );
    }
}

#[test]
fn only_explicit_once_and_reject_with_one_code_are_parsed() {
    assert!(matches!(
        permission::parse_reply(" /ALLOW p123 "),
        Some((Reply::Once, "p123"))
    ));
    assert!(matches!(
        permission::parse_reply("/reject p123"),
        Some((Reply::Reject, "p123"))
    ));
    for text in [
        "/allow",
        "/allow always p123",
        "/allow p123 extra",
        "/allow-always p123",
        "Always allow",
        "Allow once",
        "Reject",
    ] {
        assert!(permission::is_reply(text, false));
        assert!(permission::parse_reply(text).is_none());
    }
    assert!(permission::is_reply("2", true));
    assert!(!permission::is_reply("Do the next task", true));
}

#[test]
fn code_free_card_choices_are_limited_to_once_or_deny() {
    for text in ["1", "Allow once", " /ALLOW "] {
        assert!(matches!(
            permission::parse_card_reply(text),
            Some(Reply::Once)
        ));
    }
    for text in ["2", "Deny", "/reject"] {
        assert!(matches!(
            permission::parse_card_reply(text),
            Some(Reply::Reject)
        ));
    }
    for text in ["3", "Always allow", "/allow-always", "1 and then 2", "yes"] {
        assert!(permission::parse_card_reply(text).is_none());
    }
}
