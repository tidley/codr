use herdr_white_noise_bridge::output::decision::{self, Decision};
use serde_json::{Value, json};

fn data(status: &str) -> Value {
    json!({"messages":[
        {"info":{"id":"user","role":"user"},"parts":[]},
        {"info":{"id":"assistant","parentID":"user","role":"assistant"},"parts":[
            {"type":"text","text":"PRIVATE REASONING"},
            {"type":"tool","id":"part-a12345","tool":"question","state":{"status":status,"input":{"questions":[{
                "header":"Scope","question":"Which next step?","options":[
                    {"label":"Proceed","description":"Continue with the plan"},
                    {"label":"Review","description":"Review the alternatives"}
                ]
            }]},"output":"PRIVATE TOOL OUTPUT"}}
        ]}
    ]})
}

fn pending() -> Decision {
    decision::from_export(&data("running")).unwrap().unwrap()
}

fn forms() -> Value {
    json!({"data":[{
        "sessionID":"ses_fixture", "metadata":{"kind":"question","tool":{"messageID":"message","id":"tool"}},
        "fields":[{"key":"q0","type":"string","title":"Scope","description":"Which next step?","custom":true,
            "options":[{"value":"Proceed","label":"Proceed","description":"Continue with the plan"},
                       {"value":"Review","label":"Review","description":"Review the alternatives"}]}]
    }]})
}

#[test]
fn phone_question_cards_show_the_question_and_choices_without_internal_headings() {
    let mut value = forms();
    value["data"][0]["id"] = json!("frm_amber");
    let mut decision = decision::from_forms(&value, "ses_fixture")
        .unwrap()
        .unwrap();
    let question = "Which next step?\n\n1. Proceed\nContinue with the plan\n\n2. Review\nReview the alternatives";
    assert_eq!(decision.text(), question);
    decision.requires_token = true;
    assert!(decision.text().starts_with(question));
    assert!(decision.text().contains(&format!(
        "The question changed. Reply with {}",
        decision.token()
    )));
    decision.requires_token = false;
    decision.questions.push(decision.questions[0].clone());
    assert_eq!(
        decision.text(),
        format!(
            "{question}\n\n{question}\n\nAnswer this multi-question or multi-select dialog in the terminal."
        )
    );
}

#[test]
fn live_forms_keep_question_identity_and_reject_ambiguous_or_foreign_requests() {
    let decision = decision::from_forms(&forms(), "ses_fixture")
        .unwrap()
        .unwrap();
    assert_eq!(decision.id, "message:tool");
    assert_eq!(decision.questions, pending().questions);
    assert!(decision::from_forms(&forms(), "ses_other").is_err());
    for path in ["messageID", "id"] {
        let mut value = forms();
        value["data"][0]["metadata"]["tool"][path] = json!("");
        assert!(decision::from_forms(&value, "ses_fixture").is_err());
    }
    let mut value = forms();
    let duplicate = value["data"][0].clone();
    value["data"].as_array_mut().unwrap().push(duplicate);
    assert!(decision::from_forms(&value, "ses_fixture").is_err());
    assert!(
        decision::from_forms(&json!({"data":[]}), "ses_fixture")
            .unwrap()
            .is_none()
    );
    assert!(decision::from_forms(&json!({}), "ses_fixture").is_err());
}

#[test]
fn live_forms_only_expose_supported_question_fields_and_keep_multi_select_terminal_only() {
    for (key, replacement) in [
        ("type", json!("external")),
        ("key", json!("q1")),
        ("options", json!([])),
        ("description", json!("x".repeat(4001))),
    ] {
        let mut value = forms();
        value["data"][0]["fields"][0][key] = replacement;
        assert!(
            decision::from_forms(&value, "ses_fixture").is_err(),
            "{key}"
        );
    }
    let mut value = forms();
    value["data"][0]["fields"][0]["options"][0]["value"] = json!("different");
    assert!(decision::from_forms(&value, "ses_fixture").is_err());
    let mut value = forms();
    value["data"][0]["metadata"]["kind"] = json!("other");
    assert!(
        decision::from_forms(&value, "ses_fixture")
            .unwrap()
            .is_none()
    );
    let mut value = forms();
    value["data"][0]["fields"][0]["type"] = json!("multiselect");
    let decision = decision::from_forms(&value, "ses_fixture")
        .unwrap()
        .unwrap();
    assert!(!decision.supported());
    assert!(decision.choice("1").is_err());
}

#[test]
fn only_running_questions_in_the_current_turn_are_selected() {
    let decision = pending();
    assert!(decision.supported());
    assert!(decision.text().contains("1. Proceed"));
    assert!(!decision.text().contains("PRIVATE"));
    assert!(decision::from_export(&data("completed")).unwrap().is_none());
    let mut value = data("running");
    value["messages"]
        .as_array_mut()
        .unwrap()
        .push(json!({"info":{"id":"new-user","role":"user"},"parts":[]}));
    assert!(decision::from_export(&value).unwrap().is_none());
    let mut value = data("running");
    let duplicate = value["messages"][1]["parts"][1].clone();
    value["messages"][1]["parts"]
        .as_array_mut()
        .unwrap()
        .push(duplicate);
    assert!(decision::from_export(&value).is_err());
}

#[test]
fn a_choice_is_a_listed_number_or_exact_label_not_arbitrary_keys() {
    let decision = pending();
    assert_eq!(decision.choice("1").unwrap(), 1);
    assert_eq!(decision.choice("review").unwrap(), 2);
    assert_eq!(decision.choice("/answer 2").unwrap(), 2);
    for invalid in ["0", "3", "9", "1\nenter", "ctrl+c", "something else", ""] {
        assert!(decision.choice(invalid).is_err(), "{invalid}");
    }
    let mut duplicate = decision.clone();
    duplicate.questions[0].options[1].label = "Proceed".into();
    assert!(duplicate.choice("Proceed").is_err());
    assert_eq!(duplicate.choice("2").unwrap(), 2);
}

#[test]
fn changed_questions_require_their_code() {
    let mut decision = pending();
    decision.requires_token = true;
    assert!(decision.choice("1").is_err());
    assert_eq!(
        decision.choice(&format!("{} 1", decision.token())).unwrap(),
        1
    );
    assert!(decision.choice("qwrong 1").is_err());
    assert!(
        decision.looks_like_answer(" Qold001 1"),
        "An expired question code is still an answer attempt, not a new prompt"
    );
    assert!(!decision.looks_like_answer("éabcde 1"));
}

#[test]
fn native_custom_answers_keep_codes_limits_and_literal_text_separate_from_choices() {
    use decision::Answer;
    let mut value = forms();
    value["data"][0]["id"] = json!("frm_amber");
    let mut decision = decision::from_forms(&value, "ses_fixture")
        .unwrap()
        .unwrap();
    assert!(decision.looks_like_answer("Retry ngit"));
    assert!(!decision.looks_like_explicit_answer("Retry ngit"));
    for explicit in [
        "1",
        "ReView",
        "/answer Retry ngit",
        " Qold001 Retry ngit",
        &format!("{} Retry ngit", decision.token().to_uppercase()),
    ] {
        assert!(decision.looks_like_explicit_answer(explicit), "{explicit}");
    }
    assert_eq!(
        decision.answer("v6.6.6").unwrap(),
        Answer::Custom("v6.6.6".into())
    );
    assert_eq!(
        decision.answer("1 v6.6.6").unwrap(),
        Answer::Custom("1 v6.6.6".into())
    );
    assert_eq!(decision.answer("1").unwrap(), Answer::Choice(1));
    assert_eq!(decision.answer("review").unwrap(), Answer::Choice(2));
    assert_eq!(
        decision.answer("/answer /path/to/version").unwrap(),
        Answer::Custom("/path/to/version".into())
    );
    assert!(
        !decision
            .text()
            .contains("Use the terminal for a custom answer")
    );
    for invalid in [
        "",
        "  ",
        "/unknown-command",
        "qold001 v6.6.6",
        &"x".repeat(4001),
    ] {
        assert!(
            decision.answer(invalid).is_err(),
            "Unexpected custom answer: {invalid}"
        );
    }
    decision.requires_token = true;
    assert!(decision.answer("v6.6.6").is_err());
    assert_eq!(
        decision
            .answer(&format!("{} v6.6.6", decision.token()))
            .unwrap(),
        Answer::Custom("v6.6.6".into())
    );
    let mut replaced = decision.clone();
    replaced.form_id = Some("frm_replaced".into());
    assert!(!decision.same_request(&replaced));
    assert!(
        pending().answer("v6.6.6").is_err(),
        "Legacy questions have no request-bound custom API"
    );
    decision.questions[0].custom = false;
    assert!(
        decision
            .answer(&format!("{} v6.6.6", decision.token()))
            .is_err()
    );
}

#[test]
fn malformed_form_identifiers_cannot_become_native_reply_paths() {
    for id in [
        "",
        "frm_a/other",
        "frm_a?reply=true",
        "frm_a\npost",
        "not-a-form",
    ] {
        let mut value = forms();
        value["data"][0]["id"] = json!(id);
        assert!(
            decision::from_forms(&value, "ses_fixture").is_err(),
            "Unsafe form ID: {id}"
        );
    }
}

#[test]
fn native_question_codes_change_with_form_identity_and_question_content() {
    let mut value = forms();
    value["data"][0]["id"] = json!("frm_amber");
    let decision = decision::from_forms(&value, "ses_fixture")
        .unwrap()
        .unwrap();
    let mut replaced = decision.clone();
    replaced.form_id = Some("frm_replaced".into());
    assert_ne!(
        decision.token(),
        replaced.token(),
        "Old form answers must not authorize a replacement form"
    );
    replaced = decision.clone();
    replaced.questions[0].question = "A changed question".into();
    assert_ne!(decision.token(), replaced.token());
}

#[test]
fn only_the_exact_visible_single_choice_dialog_can_receive_a_shortcut() {
    let decision = pending();
    let visible = "Unrelated old output\n│ Which next step?\n│ 1. Proceed\n│ Continue with the plan\n│ 2. Review\n│ Review the alternatives\n│ 3. Type your own answer\n↑↓ select enter submit esc dismiss\n";
    assert!(decision.matches_visible(visible));
    assert!(
        !decision.matches_visible(&visible.replace("Which next step?", "A different question?"))
    );
    assert!(!decision.matches_visible(&visible.replace(
        "↑↓ select enter submit esc dismiss",
        "Allow permission? enter confirm"
    )));
    assert!(
        !decision
            .matches_visible(&visible.replace("↑↓ select", "Custom text being edited\n↑↓ select"))
    );
    let mut multiple = decision;
    multiple.questions[0].multiple = true;
    assert!(!multiple.supported());
    assert!(multiple.choice("1").is_err());
    assert!(!multiple.matches_visible(visible));
}

#[test]
fn a_wide_opencode_sidebar_does_not_hide_the_active_question() {
    let decision = pending();
    let lines = [
        "Unrelated output",
        "Which next step?",
        "1. Proceed",
        "Continue with the plan",
        "2. Review",
        "Review the alternatives",
        "3. Type your own answer",
        "↑↓ select enter submit esc dismiss",
    ];
    let visible = lines
        .iter()
        .enumerate()
        .map(|(index, line)| {
            format!(
                "{line:<80}  {}",
                if index == lines.len() - 1 {
                    "• OpenCode 1.18.35"
                } else {
                    "sidebar content"
                }
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(decision.matches_visible(&visible));
    assert!(
        !decision.matches_visible(&visible.replace("Which next step?", "A different question?"))
    );
}

#[test]
fn a_v2_repository_footer_does_not_make_the_current_question_look_stale() {
    let decision = pending();
    let lines = [
        "  ┃ Questions",
        "  ┃",
        "  ┃ Which next step?",
        "  ┃ 1. Proceed",
        "  ┃ Continue with the plan",
        "  ┃ 2. Review",
        "  ┃ Review the alternatives",
        "  ┃ 3. Type your own answer",
        "  ┃",
        "  ┃ ↑↓ select enter submit esc dismiss",
        "  ┃",
    ];
    for directory in ["~/code/example:main", "/srv/projects/example:dev"] {
        let visible = lines
            .iter()
            .enumerate()
            .map(|(index, line)| {
                format!(
                    "{line:<80}  {}",
                    if index == lines.len() - 1 {
                        directory
                    } else {
                        "sidebar content"
                    }
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            decision.matches_visible(&visible),
            "A sidebar repository footer is not part of the dialog"
        );
        assert!(
            !decision.matches_visible(&visible.replace("Which next step?", "Another question?"))
        );
        assert!(!decision.matches_visible(
            &visible.replace("Continue with the plan", "A different description")
        ));
        assert!(!decision.matches_visible(&visible.replace(
            "↑↓ select enter submit esc dismiss",
            "Allow permission? enter confirm"
        )));
        assert!(
            !decision.matches_visible(
                &visible.replace("  ┃ ↑↓", "  ┃ Custom text being edited\n  ┃ ↑↓")
            )
        );
    }
}

#[test]
fn joined_emoji_keep_their_display_width_when_the_sidebar_is_removed() {
    let mut decision = pending();
    decision.questions[0].question = format!("{}?", "👩‍💻".repeat(20));
    let lines = [
        decision.questions[0].question.as_str(),
        "1. Proceed",
        "Continue with the plan",
        "2. Review",
        "Review the alternatives",
        "3. Type your own answer",
        "↑↓ select enter submit esc dismiss",
    ];
    let visible = lines
        .iter()
        .enumerate()
        .map(|(index, line)| {
            format!(
                "{}{}  {}",
                line,
                " ".repeat(80 - unicode_width::UnicodeWidthStr::width(*line)),
                if index == lines.len() - 1 {
                    "• OpenCode 1.18.35"
                } else {
                    "sidebar"
                }
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(decision.matches_visible(&visible));
}
