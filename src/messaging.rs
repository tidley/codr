//! Same-process MDK hosting. No secret extraction and no account recreation.
use crate::BridgeConfig;
use anyhow::{Result, ensure};
use std::{
    os::unix::fs::{DirBuilderExt, PermissionsExt},
    path::PathBuf,
    thread,
    time::Duration,
};
use tokio::sync::oneshot;

pub struct EmbeddedMessaging {
    stop: Option<oneshot::Sender<()>>,
    worker: Option<thread::JoinHandle<Result<()>>>,
}

impl EmbeddedMessaging {
    pub async fn start(config: &BridgeConfig, home: PathBuf) -> Result<Self> {
        ensure!(home.is_absolute(), "Messaging home must be absolute");
        ensure!(
            home.is_dir(),
            "Messaging home must already exist; no account is created automatically"
        );
        match tokio::time::timeout(
            Duration::from_secs(1),
            tokio::net::UnixStream::connect(&config.wn_agent_socket),
        )
        .await
        {
            Ok(Err(error))
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
                ) => {}
            _ => anyhow::bail!(
                "Messaging socket is already owned or unavailable. Stop the external connector before handover"
            ),
        }
        let spool = config.state_path.with_extension("outbound-media");
        match std::fs::DirBuilder::new().mode(0o700).create(&spool) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
        let metadata = std::fs::symlink_metadata(&spool)?;
        ensure!(
            metadata.is_dir()
                && !metadata.file_type().is_symlink()
                && metadata.permissions().mode() & 0o777 == 0o700,
            "Messaging media spool must be a private directory"
        );
        let mut settings = agent_connector::AgentConnectorConfig::new(home);
        settings.socket = config.wn_agent_socket.clone();
        settings.relays = agent_connector::DEFAULT_RELAYS
            .iter()
            .map(|r| r.to_string())
            .collect();
        settings.media_allowed_roots = vec![spool];
        let (stop, stopped) = oneshot::channel();
        // MDK startup and synchronous storage work cannot occupy bridge executor threads.
        let worker = thread::Builder::new()
            .name("codr-messaging".into())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .enable_all()
                    .build()?;
                let result =
                    runtime.block_on(agent_connector::serve_socket_until(settings, async {
                        let _ = stopped.await;
                    }));
                if let Err(error) = result {
                    eprintln!(
                        "Embedded messaging unavailable: {}. Desktop service remains running.",
                        error.privacy_safe_code()
                    );
                    anyhow::bail!("Embedded messaging stopped");
                }
                Ok(())
            })?;
        Ok(Self {
            stop: Some(stop),
            worker: Some(worker),
        })
    }

    pub async fn shutdown(mut self) -> Result<()> {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(worker) = self.worker.take() {
            tokio::task::spawn_blocking(move || {
                worker
                    .join()
                    .map_err(|_| anyhow::anyhow!("Messaging thread stopped unexpectedly"))?
            })
            .await??;
        }
        Ok(())
    }
}

impl Drop for EmbeddedMessaging {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
    }
}
