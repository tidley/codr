use herdr_white_noise_bridge::BridgeState;
use pulldown_cmark::{Event, Options, Parser, Tag};

fn queued(text: &str) -> String {
    let mut state = BridgeState::default();
    state.enqueue_manual("group", text, "request");
    state.outbox.iter().map(|item| item.text.as_str()).collect()
}

#[test]
fn wide_tables_become_labelled_rows_without_changing_surrounding_text() {
    let text = "Before **status**.\n\n| Issue/task | Current status | Agent working | Complete | Blocked |\n| --- | --- | --- | --- | --- |\n| #11 — Select-to-enable | Host blocker documented | None | No | Missing picker-selection hook |\n| #12 — Reference access | Integrated | None | Partial | Exact-file access denied |\n\nAfter.";
    let expected = "Before **status**.\n\n### Issue/task: #11 — Select-to-enable\n- Current status: Host blocker documented\n- Agent working: None\n- Complete: No\n- Blocked: Missing picker-selection hook\n\n### Issue/task: #12 — Reference access\n- Current status: Integrated\n- Agent working: None\n- Complete: Partial\n- Blocked: Exact-file access denied\n\nAfter.";
    assert_eq!(queued(text), expected);
    assert_eq!(queued(expected), expected);
    let mut automatic = BridgeState::default();
    automatic.enqueue("group", text);
    assert_eq!(automatic.outbox[0].text, expected);
}

#[test]
fn small_tables_code_examples_and_nested_tables_stay_byte_identical() {
    let small = "| ID | Status |\n| --- | --- |\n| A | Done |\n";
    let wide = "| A | B | C | D |\n| - | - | - | - |\n| 1 | 2 | 3 | 4 |\n";
    for text in [
        small.to_owned(),
        format!("```markdown\n{wide}```\n"),
        format!("~~~~\n{wide}~~~~\n"),
        wide.lines().map(|line| format!("> {line}\n")).collect(),
        wide.lines().map(|line| format!("    {line}\n")).collect(),
        format!(
            "- Intro\n\n{}",
            wide.lines()
                .map(|line| format!("  {line}\n"))
                .collect::<String>()
        ),
        "| A | B | C | D |\n| - | - | - | - |\n".to_owned(),
        "This | is not | a table.\n".to_owned(),
    ] {
        assert_eq!(queued(&text), text);
    }
}

#[test]
fn labelled_rows_preserve_inline_markdown_links_and_escaped_pipes() {
    let text = "| Item | Status | Docs | Notes |\n| --- | --- | --- | --- |\n| **Build** | *Ready* | [guide][docs] | `a\\|b` and a\\|b |\n\n[docs]: https://example.com/guide?q=1";
    let result = queued(text);
    assert!(result.contains("### Item: **Build**"));
    assert!(result.contains("- Status: *Ready*"));
    assert!(result.contains("- Docs: [guide](<https://example.com/guide?q=1>)"));
    assert!(result.contains("- Notes: `a|b` and a\\|b"));
    assert!(result.ends_with("[docs]: https://example.com/guide?q=1"));
    let links = |source: &str| {
        Parser::new_ext(source, Options::ENABLE_TABLES)
            .filter_map(|event| match event {
                Event::Start(Tag::Link { dest_url, .. }) => Some(dest_url.to_string()),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(links(&result), links(text));
    let code = |source: &str| {
        Parser::new_ext(source, Options::ENABLE_TABLES)
            .filter_map(|event| match event {
                Event::Code(value) => Some(value.to_string()),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(code(&result), code(text));
}

#[test]
fn a_row_title_keeps_trailing_hashes_as_content() {
    for value in ["value ###", "###"] {
        let result = queued(&format!(
            "| Item | B | C | D |\n| - | - | - | - |\n| {value} | two | three | four |\n"
        ));
        let visible: String = Parser::new(&result)
            .filter_map(|event| match event {
                Event::Text(text) => Some(text.to_string()),
                _ => None,
            })
            .collect();
        assert!(visible.contains(&format!("Item: {value}")));
    }
}

#[test]
fn transport_chunks_do_not_split_a_converted_field_link_that_fits() {
    for (heading_length, url_length) in [(29930, 200), (10000, 25000)] {
        let url = format!("https://example.com/{}", "a".repeat(url_length));
        let text = format!(
            "| Item | Docs | C | D |\n| - | - | - | - |\n| {} | [details]({url}) | three | four |\n",
            "x".repeat(heading_length)
        );
        let mut state = BridgeState::default();
        state.enqueue_manual("group", &text, "request");
        let links: Vec<_> = state
            .outbox
            .iter()
            .flat_map(|item| {
                Parser::new(&item.text).filter_map(|event| match event {
                    Event::Start(Tag::Link { dest_url, .. }) => Some(dest_url.to_string()),
                    _ => None,
                })
            })
            .collect();
        assert_eq!(links, [url]);
        assert!(state.outbox.iter().all(|item| item.text.len() <= 30000));
    }
}

#[test]
fn converted_chunks_keep_reference_links_clickable_without_remote_definitions() {
    let rows = "| x | [guide][docs] | c | d |\n".repeat(800);
    let text = format!(
        "| A | B | C | D |\n| - | - | - | - |\n{rows}\n[docs]: https://example.com/guide\n"
    );
    assert!(text.len() < 30000);
    let mut state = BridgeState::default();
    state.enqueue_manual("group", &text, "request");
    assert!(state.outbox.len() > 1);
    let links: Vec<_> = state
        .outbox
        .iter()
        .flat_map(|item| {
            Parser::new(&item.text).filter_map(|event| match event {
                Event::Start(Tag::Link { dest_url, .. }) => Some(dest_url.to_string()),
                _ => None,
            })
        })
        .collect();
    assert_eq!(links.len(), 800);
    assert!(links.iter().all(|link| link == "https://example.com/guide"));
}

#[test]
fn small_visible_tables_ignore_long_link_destinations_and_reference_names() {
    let url = format!("https://example.com/{}", "long".repeat(40));
    let reference = "reference".repeat(30);
    for cell in [format!("[x]({url})"), format!("[x][{reference}]")] {
        let text = format!("| ID | Link |\n| - | - |\n| A | {cell} |\n\n[{reference}]: {url}\n");
        assert_eq!(queued(&text), text);
    }
}

#[test]
fn reference_forms_keep_inline_labels_targets_and_titles() {
    let text = "Before [outside][doc].\n\n| Item | Docs | Compact | Shortcut |\n| - | - | - | - |\n| A | [**guide**][doc] | [compact][] | [shortcut] |\n| B | [code `x]y`][a\\[b] | x | y |\n\n[doc]: <https://example.com/a(b)?x=1&y=2> \"Guide title\"\n[compact]: https://example.com/compact\n[shortcut]: /relative\n[a\\[b]: https://example.com/bracket\n";
    let result = queued(text);
    let metadata = |source: &str| {
        Parser::new_ext(source, Options::ENABLE_TABLES)
            .filter_map(|event| match event {
                Event::Start(Tag::Link {
                    dest_url, title, ..
                }) => Some((dest_url.to_string(), title.to_string())),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(metadata(&result), metadata(text));
    assert!(result.contains("[**guide**](<https://example.com/a(b)?x=1&amp;y=2> \"Guide title\")"));
    assert!(result.contains("[code `x]y`](<https://example.com/bracket>)"));
}

#[test]
fn reference_link_metadata_is_not_entity_decoded_twice() {
    let text = "| A | B | C | D |\n| - | - | - | - |\n| x | [guide][doc] | c | d |\n\n[doc]: <https://example.com/?q=&amp;copy;> \"&amp;copy;\"\n";
    let result = queued(text);
    let metadata = |source: &str| {
        Parser::new_ext(source, Options::ENABLE_TABLES)
            .filter_map(|event| match event {
                Event::Start(Tag::Link {
                    dest_url, title, ..
                }) => Some((dest_url.to_string(), title.to_string())),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(metadata(&result), metadata(text));
}

#[test]
fn long_two_column_tables_are_readable_and_empty_headers_have_labels() {
    let detail = "A sufficiently long description with several words that should not be squeezed into a narrow phone column.";
    let text = format!("| ID | Description |\n| --- | --- |\n| A | {detail} |\n");
    assert_eq!(
        queued(&text),
        format!("### ID: A\n- Description: {detail}\n\n")
    );
    assert_eq!(
        queued("| | B | | D |\n| - | - | - | - |\n| one | two | | four |\n"),
        "### Column 1: one\n- B: two\n- Column 3: \n- D: four\n\n"
    );
}

#[test]
fn converted_payloads_keep_utf8_limits_receipts_and_restart_identity() {
    let rows = (0..600)
        .map(|index| format!("| item-{index} | 🙂🙂 | **ready** | https://example.com/{index} |\n"))
        .collect::<String>();
    let text = format!("| Item | Mood | Status | Link |\n| --- | --- | --- | --- |\n{rows}");
    let mut state = BridgeState::default();
    state.enqueue_tracked("group", &text, Some("request"), vec!["source-key".into()]);
    assert!(state.outbox.len() > 1);
    assert!(state.outbox.iter().all(|item| item.text.len() <= 30000));
    let rendered: String = state.outbox.iter().map(|item| item.text.as_str()).collect();
    assert_eq!(rendered.matches("### Item: item-").count(), 600);
    assert_eq!(rendered.matches("🙂🙂").count(), 600);
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("state.json");
    state.save(&path).unwrap();
    let mut loaded = BridgeState::load(&path).unwrap();
    assert_eq!(loaded.outbox, state.outbox);
    let ids: Vec<_> = loaded.outbox.iter().map(|item| item.id.clone()).collect();
    for (part, id) in ids.iter().enumerate() {
        assert_eq!(id, &format!("request:{part}"));
        loaded.confirm_send(id);
        assert_eq!(
            loaded.delivered_messages.contains_key("group"),
            part + 1 == ids.len()
        );
    }
    assert_eq!(loaded.delivered_messages["group"], ["source-key"]);
}
