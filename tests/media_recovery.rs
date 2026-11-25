use herdr_white_noise_bridge::{
    BridgeConfig, media,
    wire::{Socket, read_line},
};
use serde_json::json;
use tokio::{io::AsyncWriteExt, net::UnixListener};

#[tokio::test]
async fn timeline_summaries_are_resolved_to_complete_refs_before_download() {
    for (kind, message_kind, parent) in [
        ("image/png", 9, None),
        ("application/zip", 9, None),
        ("image/png", 1111, Some("parent")),
    ] {
        let root = tempfile::tempdir().unwrap();
        let image = root.path().join("download.png");
        std::fs::write(&image, b"image fixture").unwrap();
        let path = root.path().join("mdk.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let config: BridgeConfig = serde_json::from_value(json!({"phone_npub":"unused", "account_id_hex":"aa".repeat(32),
        "herdr_socket":root.path().join("herdr.sock"),"wn_agent_socket":path,"state_path":root.path().join("state.json")})).unwrap();
        let reference = json!({"media_type":kind,"file_name":"test.png","ciphertext_sha256":"ab".repeat(32),
        "plaintext_sha256":"cd".repeat(32),"nonce_hex":"00".repeat(12),"version":"encrypted-media-v1",
        "source_epoch":7,"locators":[{"kind":"blossom-v1","value":"https://example.com/blob"}]});
        let server = tokio::spawn(async move {
            for index in 0..2 {
                let (stream, _) = listener.accept().await.unwrap();
                let mut reader = tokio::io::BufReader::new(stream);
                let request = read_line(&mut reader).await.unwrap();
                let mut reply = if index == 0 {
                    assert_eq!(request["type"], "timeline_message_get");
                    assert_eq!(request["include_media"], true);
                    json!({"type":"timeline_message","account_id_hex":"aa".repeat(32),"group_id_hex":"bb","message_id_hex":"one",
                    "message":{"message_id_hex":"one","availability":"available","kind":message_kind,"reply_to_message_id_hex":parent,"text":"Look at this.",
                         "sender":{"account_id_hex":"11".repeat(32)},"attachments":[{"media_type":kind,"file_name":"test.png"}]},
                    "media":[reference]})
                } else {
                    assert_eq!(request["type"], "download_media");
                    assert_eq!(request["media"], reference);
                    json!({"type":"media_downloaded","path":image,"media_type":kind,"size_bytes":std::fs::metadata(&image).unwrap().len()})
                };
                reply["id"] = request["id"].clone();
                reply["marmot_agent_control"] = json!("marmot.agent-control.v2");
                reader
                    .get_mut()
                    .write_all((reply.to_string() + "\n").as_bytes())
                    .await
                    .unwrap();
            }
        });
        let prompt = media::prepare(&config, &Socket{path}, "bb", &json!({"message_id_hex":"one","text":"Look at this.",
         "sender":{"account_id_hex":"11".repeat(32)},"media":[{"media_type":kind,"file_name":"test.png"}]})).await.unwrap();
        assert!(prompt.prompt.starts_with(if kind == "image/png" {
            "Look at this.\n\nAttached image:"
        } else {
            "Look at this.\n\nAttached file:"
        }));
        server.await.unwrap();
    }
}
