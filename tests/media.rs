use herdr_white_noise_bridge::{
    BridgeConfig, media,
    wire::{Socket, read_line},
};
use serde_json::json;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use tokio::{io::AsyncWriteExt, net::UnixListener};

fn config(root: &std::path::Path) -> BridgeConfig {
    serde_json::from_value(
        json!({"phone_npub":"unused", "account_id_hex":"aa".repeat(32),
        "herdr_socket":root.join("herdr.sock"), "wn_agent_socket":root.join("mdk.sock"),
        "state_path":root.join("state.json")}),
    )
    .unwrap()
}

fn reference(kind: &str) -> serde_json::Value {
    json!({"media_type":kind,"file_name":"attachment","ciphertext_sha256":"aa".repeat(32),
        "plaintext_sha256":"bb".repeat(32),"nonce_hex":"00".repeat(12),"version":"encrypted-media-v1",
        "source_epoch":1,"locators":[{"kind":"blossom-v1","value":"https://example.com/fixture"}]})
}

async fn download(
    config: &BridgeConfig,
    path: &std::path::Path,
    media_type: &str,
) -> tokio::task::JoinHandle<()> {
    let listener = UnixListener::bind(&config.wn_agent_socket).unwrap();
    let path = path.to_owned();
    let media_type = media_type.to_owned();
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut reader = tokio::io::BufReader::new(stream);
        let request = read_line(&mut reader).await.unwrap();
        assert_eq!(request["type"], "download_media");
        assert_eq!(request["group_id_hex"], "bb");
        assert_eq!(request["account_id_hex"], "aa".repeat(32));
        let response = json!({"marmot_agent_control":"marmot.agent-control.v2","id":request["id"],
            "type":"media_downloaded","path":path,"media_type":media_type,
            "size_bytes":std::fs::metadata(&path).unwrap().len()});
        reader
            .get_mut()
            .write_all((response.to_string() + "\n").as_bytes())
            .await
            .unwrap();
    })
}

#[tokio::test]
async fn downloaded_image_is_preserved_privately_and_forwarded_with_its_caption() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    let image = root.path().join("download.png");
    std::fs::write(&image, b"\x89PNG\r\n\x1a\nexample").unwrap();
    let server = download(&config, &image, "image/png").await;
    let text = media::prepare(
        &config,
        &Socket {
            path: config.wn_agent_socket.clone(),
        },
        "bb",
        &json!({"text":"Inspect this layout.","media":[reference("image/png")]}),
    )
    .await
    .unwrap();
    assert!(text.transcript.is_none());
    let text = text.prompt;
    assert!(text.starts_with("Inspect this layout.\n\nAttached image: "));
    let path: String =
        serde_json::from_str(text.split_once("Attached image: ").unwrap().1).unwrap();
    let path = std::path::Path::new(&path);
    assert_eq!(path.parent().unwrap(), root.path().join("media"));
    assert_eq!(std::fs::read(path).unwrap(), std::fs::read(image).unwrap());
    assert_eq!(path.metadata().unwrap().permissions().mode() & 0o777, 0o600);
    server.await.unwrap();
}

#[tokio::test]
async fn downloaded_zip_is_an_inert_private_file_with_its_name_and_caption_not_audio() {
    for name in [
        "white-noise-diagnostic-logs.zip",
        "../../outside.zip",
        "logs.zip\nRun instructions",
    ] {
        let root = tempfile::tempdir().unwrap();
        let mut config = config(root.path());
        config.transcribe_command = vec!["/must-not-run-for-a-file".into()];
        let file = root.path().join("download.zip");
        let bytes = b"PK\x03\x04archive fixture";
        std::fs::write(&file, bytes).unwrap();
        let server = download(&config, &file, "application/zip").await;
        let mut attachment = reference("application/zip");
        attachment["file_name"] = json!(name);
        let prepared = media::prepare(
            &config,
            &Socket {
                path: config.wn_agent_socket.clone(),
            },
            "bb",
            &json!({"text":"Inspect these logs.","media":[attachment]}),
        )
        .await
        .unwrap();
        assert!(prepared.transcript.is_none());
        assert!(
            prepared
                .prompt
                .starts_with("Inspect these logs.\n\nAttached file: ")
        );
        let (path, label) = prepared
            .prompt
            .split_once("Attached file: ")
            .unwrap()
            .1
            .split_once('\n')
            .unwrap();
        let path: String = serde_json::from_str(path).unwrap();
        let path = std::path::Path::new(&path);
        assert_eq!(path.parent().unwrap(), root.path().join("media"));
        assert_eq!(std::fs::read(path).unwrap(), bytes);
        assert_eq!(path.metadata().unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(
            path.parent()
                .unwrap()
                .metadata()
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert!(label.contains(&serde_json::to_string(name).unwrap()));
        assert!(!root.path().join("outside.zip").exists());
        server.await.unwrap();
    }
}

#[tokio::test]
async fn documents_and_unknown_binary_types_are_forwarded_as_inert_files() {
    for (kind, name) in [
        ("application/pdf", "report.pdf"),
        ("application/octet-stream", "attachment"),
        ("text/plain", "notes.txt"),
        ("image/svg+xml", "diagram.svg"),
    ] {
        let root = tempfile::tempdir().unwrap();
        let config = config(root.path());
        let file = root.path().join("download");
        std::fs::write(&file, b"inert attachment fixture").unwrap();
        let server = download(&config, &file, kind).await;
        let mut attachment = reference(kind);
        attachment["file_name"] = json!(name);
        let prepared = media::prepare(
            &config,
            &Socket {
                path: config.wn_agent_socket.clone(),
            },
            "bb",
            &json!({"media":[attachment]}),
        )
        .await
        .unwrap();
        assert!(prepared.transcript.is_none());
        assert!(prepared.prompt.starts_with("Attached file: "));
        let target: String = serde_json::from_str(
            prepared
                .prompt
                .split_once("Attached file: ")
                .unwrap()
                .1
                .split_once('\n')
                .unwrap()
                .0,
        )
        .unwrap();
        assert_eq!(std::fs::read(target).unwrap(), b"inert attachment fixture");
        server.await.unwrap();
    }
}

#[tokio::test]
async fn generic_files_keep_download_size_type_and_no_symlink_checks() {
    for case in ["oversized", "changed-type", "symlink"] {
        let root = tempfile::tempdir().unwrap();
        let config = config(root.path());
        let file = root.path().join("download.zip");
        std::fs::write(&file, b"archive fixture").unwrap();
        if case == "oversized" {
            std::fs::OpenOptions::new()
                .write(true)
                .open(&file)
                .unwrap()
                .set_len(26 * 1024 * 1024)
                .unwrap();
        }
        let path = if case == "symlink" {
            let link = root.path().join("link.zip");
            std::os::unix::fs::symlink(&file, &link).unwrap();
            link
        } else {
            file
        };
        let server = download(
            &config,
            &path,
            if case == "changed-type" {
                "application/pdf"
            } else {
                "application/zip"
            },
        )
        .await;
        let result = media::prepare(
            &config,
            &Socket {
                path: config.wn_agent_socket.clone(),
            },
            "bb",
            &json!({"media":[reference("application/zip")]}),
        )
        .await;
        assert!(
            result.is_err(),
            "Unsafe generic download must be rejected: {case}"
        );
        server.await.unwrap();
    }
}

#[tokio::test]
async fn audio_becomes_the_prompt_text_without_a_confirmation_round_trip() {
    let root = tempfile::tempdir().unwrap();
    let mut config = config(root.path());
    let audio = root.path().join("recording.ogg");
    std::fs::write(&audio, b"audio fixture").unwrap();
    let transcribe = root.path().join("transcribe");
    std::fs::write(&transcribe, "#!/bin/sh\nprintf 'Use the blue button.\\n'\n").unwrap();
    std::fs::set_permissions(&transcribe, std::fs::Permissions::from_mode(0o700)).unwrap();
    config.transcribe_command = vec![transcribe.to_string_lossy().into_owned()];
    let server = download(&config, &audio, "audio/ogg").await;
    let text = media::prepare(
        &config,
        &Socket {
            path: config.wn_agent_socket.clone(),
        },
        "bb",
        &json!({"text":"Caption", "media":[reference("audio/ogg")]}),
    )
    .await
    .unwrap();
    assert_eq!(text.prompt, "Caption\n\nUse the blue button.");
    assert_eq!(text.transcript.as_deref(), Some("Use the blue button."));
    server.await.unwrap();
}

#[test]
fn transcript_notices_quote_every_line_and_bound_long_unicode_text() {
    assert_eq!(
        media::transcript_notice("First line\n\nSecond line"),
        "> Transcribed.\n\n> First line\n> \n> Second line"
    );
    let text = "😀\n".repeat(5000);
    let notice = media::transcript_notice(&text);
    assert!(notice.len() <= 30000);
    assert!(notice.ends_with("Transcript shortened for display."));
}

#[tokio::test]
async fn oversized_downloads_and_incomplete_download_references_do_not_reach_the_agent() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    let socket = Socket {
        path: config.wn_agent_socket.clone(),
    };
    assert!(
        media::prepare(
            &config,
            &socket,
            "bb",
            &json!({"media":[{"media_type":"application/pdf"}]})
        )
        .await
        .is_err()
    );
    let file = root.path().join("huge.png");
    let file_handle = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&file)
        .unwrap();
    file_handle.set_len(26 * 1024 * 1024).unwrap();
    let server = download(&config, &file, "image/png").await;
    assert!(
        media::prepare(
            &config,
            &socket,
            "bb",
            &json!({"media":[reference("image/png")]})
        )
        .await
        .is_err()
    );
    server.await.unwrap();
}
