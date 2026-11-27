//! Pinned external human identity. No secret import/export or local-key fallback.
use marmot_app::{AccountIdentityProofRequest, AccountIdentityProofSigner};
use nostr::prelude::{Event, PublicKey, UnsignedEvent};
use serde_json::{Value, json};
use std::{
    os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt},
    path::PathBuf,
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::UnixStream,
};
use transport_nostr_peeler::{MarmotNostrSigner, MarmotSignerError, SignerFuture};

#[derive(Clone)]
pub struct ExternalSigner {
    socket: PathBuf,
    public_key: PublicKey,
}
impl std::fmt::Debug for ExternalSigner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ExternalSigner(..)")
    }
}
impl ExternalSigner {
    pub fn new(socket: PathBuf, public_key: PublicKey) -> Self {
        Self { socket, public_key }
    }
    async fn request(&self, method: &str, params: Value) -> Result<Value, MarmotSignerError> {
        // Do not return signer payloads, paths, plaintext or pairing credentials
        // in errors. Cancellation/refusal never retries a signing request.
        tokio::time::timeout(Duration::from_secs(120), async {
            if !self.socket.is_absolute() {
                return Err(());
            }
            let parent =
                std::fs::symlink_metadata(self.socket.parent().ok_or(())?).map_err(|_| ())?;
            let node = std::fs::symlink_metadata(&self.socket).map_err(|_| ())?;
            // The same-user host is the local approval boundary, not room membership.
            let uid = unsafe { libc::geteuid() };
            if !parent.is_dir()
                || parent.file_type().is_symlink()
                || parent.uid() != uid
                || parent.permissions().mode() & 0o077 != 0
                || !node.file_type().is_socket()
                || node.uid() != uid
                || node.permissions().mode() & 0o077 != 0
            {
                return Err(());
            }
            let mut stream = UnixStream::connect(&self.socket).await.map_err(|_| ())?;
            if stream.peer_cred().map_err(|_| ())?.uid() != uid {
                return Err(());
            }
            let id = uuid::Uuid::new_v4().to_string();
            let bytes =
                serde_json::to_vec(&json!({"version":1,"id":id,"method":method,"params":params}))
                    .map_err(|_| ())?;
            if bytes.len() >= agent_control::MAX_AGENT_CONTROL_FRAME_BYTES {
                return Err(());
            }
            stream.write_all(&bytes).await.map_err(|_| ())?;
            stream.write_all(b"\n").await.map_err(|_| ())?;
            let mut response = Vec::new();
            BufReader::new(stream)
                .take(agent_control::MAX_AGENT_CONTROL_FRAME_BYTES as u64 + 1)
                .read_until(b'\n', &mut response)
                .await
                .map_err(|_| ())?;
            if response.len() > agent_control::MAX_AGENT_CONTROL_FRAME_BYTES
                || !response.ends_with(b"\n")
            {
                return Err(());
            }
            let value: Value = serde_json::from_slice(&response).map_err(|_| ())?;
            if value["version"] != 1 || value["id"] != id || value.get("error").is_some() {
                return Err(());
            }
            value.get("result").cloned().ok_or(())
        })
        .await
        .map_err(|_| MarmotSignerError::from("External signer timed out"))?
        .map_err(|_| MarmotSignerError::from("External signer unavailable, refused or invalid"))
    }
    async fn text(
        &self,
        method: &str,
        key: &PublicKey,
        content: &str,
    ) -> Result<String, MarmotSignerError> {
        self.request(method, json!([key.to_hex(), content]))
            .await?
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| MarmotSignerError::from("Invalid external signer response"))
    }
}
impl MarmotNostrSigner for ExternalSigner {
    fn get_public_key(&self) -> SignerFuture<'_, Result<PublicKey, MarmotSignerError>> {
        Box::pin(async move {
            if self.request("get_public_key", json!([])).await?.as_str()
                != Some(self.public_key.to_hex().as_str())
            {
                return Err(MarmotSignerError::from("External signer identity changed"));
            }
            Ok(self.public_key)
        })
    }
    fn sign_event(
        &self,
        unsigned: UnsignedEvent,
    ) -> SignerFuture<'_, Result<Event, MarmotSignerError>> {
        Box::pin(async move {
            if unsigned.pubkey != self.public_key || unsigned.verify_id().is_err() {
                return Err(MarmotSignerError::from("Invalid external signing request"));
            }
            let expected = unsigned.compute_id();
            let event: Event =
                serde_json::from_value(self.request("sign_event", json!([unsigned])).await?)
                    .map_err(|_| MarmotSignerError::from("Invalid external signed event"))?;
            if event.pubkey != self.public_key || event.id != expected || event.verify().is_err() {
                return Err(MarmotSignerError::from(
                    "External signer returned a different or invalid event",
                ));
            }
            Ok(event)
        })
    }
    fn nip04_encrypt<'a>(
        &'a self,
        key: &'a PublicKey,
        content: &'a str,
    ) -> SignerFuture<'a, Result<String, MarmotSignerError>> {
        Box::pin(self.text("nip04_encrypt", key, content))
    }
    fn nip04_decrypt<'a>(
        &'a self,
        key: &'a PublicKey,
        content: &'a str,
    ) -> SignerFuture<'a, Result<String, MarmotSignerError>> {
        Box::pin(self.text("nip04_decrypt", key, content))
    }
    fn nip44_encrypt<'a>(
        &'a self,
        key: &'a PublicKey,
        content: &'a str,
    ) -> SignerFuture<'a, Result<String, MarmotSignerError>> {
        Box::pin(self.text("nip44_encrypt", key, content))
    }
    fn nip44_decrypt<'a>(
        &'a self,
        key: &'a PublicKey,
        content: &'a str,
    ) -> SignerFuture<'a, Result<String, MarmotSignerError>> {
        Box::pin(self.text("nip44_decrypt", key, content))
    }
}
impl AccountIdentityProofSigner for ExternalSigner {
    fn sign_account_identity_proof(
        &self,
        request: &AccountIdentityProofRequest,
    ) -> Result<[u8; 64], String> {
        if request.account_identity.as_slice() != self.public_key.to_bytes() {
            return Err("External signer proof identity mismatch".into());
        }
        let unsigned = request.proof_event()?;
        let signer = self.clone();
        let event = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|_| "Signer runtime unavailable".to_owned())?;
            runtime
                .block_on(signer.sign_event(unsigned))
                .map_err(|_| "External signer proof not confirmed".to_owned())
        })
        .join()
        .map_err(|_| "Signer worker unavailable".to_owned())??;
        request.signature_from_signed_event(event)
    }
}
