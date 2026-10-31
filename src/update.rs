//! Owner-confirmed executable updates through pinned ngit release records.
use crate::{
    bridge::{Bridge, now},
    state,
};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Release {
    pub version: String,
    pub event_id: String,
    pub commit: String,
    pub asset: Value,
    pub checksum: Value,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Offer {
    pub release: Option<Release>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub handover: bool,
    pub code: String,
    pub expires_at: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Job {
    pub id: String,
    pub version: String,
    pub group: String,
}

pub(crate) struct Check {
    key: String,
    group: String,
    id: String,
    pub task: tokio::task::JoinHandle<Result<Option<Release>>>,
}

pub fn info() -> Result<Value> {
    let connector: toml::Value = toml::from_str(include_str!("../connector.toml"))?;
    Ok(json!({
        "version":env!("CARGO_PKG_VERSION"), "state_version":1,
        "runtimes":["external-connector", "embedded-mdk"],
        "storage_cohort":connector["storage_cohort"].as_str().ok_or_else(|| anyhow::anyhow!("Missing messaging storage cohort"))?,
        "protocol":crate::wire::PROTOCOL,
        "connector":{"version":connector["version"].as_str(), "revision":connector["revision"].as_str(),
            "patch_sha256":hex::encode(Sha256::digest(include_bytes!("../patches/mdk-codr.patch")))}
    }))
}

fn tools(directory: &Path) -> Result<()> {
    for (name, text) in [
        ("update.py", include_str!("../scripts/update.py")),
        ("install.py", include_str!("../scripts/install.py")),
        ("services.py", include_str!("../scripts/services.py")),
        ("handover.py", include_str!("../scripts/handover.py")),
        ("codr.service", include_str!("../systemd/codr.service")),
    ] {
        use std::{io::Write, os::unix::fs::OpenOptionsExt};
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(directory.join(name))?;
        file.write_all(text.as_bytes())?;
    }
    Ok(())
}

async fn helper(directory: &Path, action: &str, argument: &str) -> Result<Value> {
    let mut command = tokio::process::Command::new("python3");
    command
        .arg(directory.join("update.py"))
        .arg(action)
        .arg(argument)
        .kill_on_drop(true);
    let output = tokio::time::timeout(Duration::from_secs(200), command.output()).await??;
    ensure!(
        output.stdout.len() <= 64 * 1024,
        "Update response is too large"
    );
    let value: Value = serde_json::from_slice(&output.stdout)?;
    ensure!(
        output.status.success() || value["not_started"] == true,
        "{}",
        value["error"]
            .as_str()
            .unwrap_or("The update helper could not complete this request")
    );
    Ok(value)
}

fn reply(
    bridge: &mut Bridge,
    key: &str,
    group: &str,
    id: &str,
    suffix: &str,
    message: &str,
) -> Result<()> {
    let before = bridge.state.clone();
    let mapping = bridge.state.mappings.get_mut(key).unwrap();
    if !mapping.seen_messages.iter().any(|seen| seen == id) {
        mapping.seen_messages.push(id.into());
    }
    bridge
        .state
        .enqueue_manual(group, message, &format!("update:{id}:{suffix}"));
    if let Err(error) = bridge.save() {
        bridge.state = before;
        return Err(error);
    }
    Ok(())
}

pub fn store_offer(
    bridge: &mut Bridge,
    key: &str,
    group: &str,
    id: &str,
    release: Release,
) -> Result<()> {
    ensure!(
        bridge.state.mappings[key].group_id_hex.as_deref() == Some(group),
        "Update chat changed"
    );
    let before = bridge.state.clone();
    let code = uuid::Uuid::new_v4().simple().to_string()[..10].to_string();
    let scope = if bridge.update_messaging_home.is_some() {
        "The complete combined bridge and messaging executable will update together. Accounts, chats and accepted work stay in place; coding agents are not restarted."
    } else {
        "Only the bridge worker will update. Accounts, chats and accepted work stay in place; coding agents and the external connector are not restarted."
    };
    let message = format!(
        "codr {} → {}\nSource: the maintainer's ngit release, downloaded from Blossom.\n{scope}\n\nConfirm within 10 minutes:\n/update confirm\n\nOr /update cancel. A storage-cohort or service-layout change requires local setup.",
        env!("CARGO_PKG_VERSION"),
        release.version
    );
    bridge.state.mappings.get_mut(key).unwrap().update_offer = Some(Offer {
        release: Some(release),
        handover: false,
        code,
        expires_at: now().saturating_add(600),
    });
    if let Err(error) = reply(bridge, key, group, id, "offer", &message) {
        bridge.state = before;
        return Err(error);
    }
    Ok(())
}

fn store_handover_offer(bridge: &mut Bridge, key: &str, group: &str, id: &str) -> Result<()> {
    ensure!(
        bridge.state.mappings[key].group_id_hex.as_deref() == Some(group),
        "Update chat changed"
    );
    let before = bridge.state.clone();
    bridge.state.mappings.get_mut(key).unwrap().update_offer = Some(Offer {
        release: None,
        handover: true,
        code: uuid::Uuid::new_v4().simple().to_string()[..10].to_string(),
        expires_at: now().saturating_add(600),
    });
    if let Err(error) = reply(
        bridge,
        key,
        group,
        id,
        "offer",
        "Finish the combined codr upgrade using the already installed executable. No release download is needed.\n\nThis will stop the old bridge and messaging services, verify a private cold backup of account credentials, chat state and service files, then start one combined service. Coding agents are not restarted. Unsupported layouts or storage changes are refused before stopping anything.\n\nConfirm within 10 minutes:\n/update confirm\n\nOr /update cancel.",
    ) {
        bridge.state = before;
        return Err(error);
    }
    Ok(())
}

fn directory(bridge: &Bridge, job: &Job) -> Result<PathBuf> {
    ensure!(
        !job.id.is_empty()
            && job.id.len() <= 64
            && job
                .id
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-'),
        "Invalid update job identity"
    );
    Ok(bridge
        .config
        .state_path
        .with_extension("updates")
        .join(&job.id))
}

/// Persist the confirmation and one global installation intent before scheduling.
pub fn prepare(
    bridge: &mut Bridge,
    key: &str,
    group: &str,
    id: &str,
    code: &str,
) -> Result<PathBuf> {
    ensure!(
        bridge.state.update_job.is_none(),
        "An update is already pending; no second update was started"
    );
    ensure!(
        bridge.state.mappings[key].group_id_hex.as_deref() == Some(group),
        "Update chat changed"
    );
    let offer = bridge.state.mappings[key]
        .update_offer
        .clone()
        .ok_or_else(|| anyhow::anyhow!("No update offer in this chat. Send /update first"))?;
    ensure!(
        offer.code == code,
        "Use this chat's current update confirmation code"
    );
    ensure!(
        now() < offer.expires_at,
        "The update offer expired. Send /update to check again"
    );
    let numeric = |version: &str| -> Result<Vec<u64>> {
        let parts = version
            .split('.')
            .map(str::parse)
            .collect::<std::result::Result<Vec<u64>, _>>()?;
        ensure!(parts.len() == 3, "Invalid update version");
        Ok(parts)
    };
    let version = if offer.handover {
        ensure!(
            offer.release.is_none() && bridge.update_messaging_home.is_none(),
            "This handover is no longer applicable"
        );
        env!("CARGO_PKG_VERSION").to_string()
    } else {
        let release = offer
            .release
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Missing update release"))?;
        ensure!(
            numeric(&release.version)? > numeric(env!("CARGO_PKG_VERSION"))?,
            "This release is no longer newer than the running bridge. Send /update again"
        );
        release.version.clone()
    };
    let config = bridge
        .update_config_path
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("Automatic updates require the managed bridge service"))?;
    let root = bridge.config.state_path.with_extension("updates");
    std::fs::create_dir_all(&root)?;
    let temporary = tempfile::Builder::new().prefix("job-").tempdir_in(&root)?;
    tools(temporary.path())?;
    let job = Job {
        id: temporary
            .path()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned(),
        version,
        group: group.into(),
    };
    let binary = std::env::current_exe()?;
    state::save_json(
        &temporary.path().join("request.json"),
        &json!({
        "job_id":job.id, "release":offer.release, "info":info()?, "pid":std::process::id(),
            "action":if offer.handover {"handover"} else {"install"},
            "installed_path":binary, "base_sha256":hex::encode(Sha256::digest(std::fs::read(&binary)?)),
            "config_path":config, "state_path":bridge.config.state_path,
            "service":if bridge.update_messaging_home.is_some() {
                std::env::var("CODR_SERVICE_NAME").unwrap_or_else(|_| "codr".into())
            } else {"herdr-white-noise-bridge".into()},
            "messaging_home":bridge.update_messaging_home,
        }),
    )?;
    let before = bridge.state.clone();
    bridge.state.mappings.get_mut(key).unwrap().update_offer = None;
    bridge.state.update_job = Some(job);
    if let Err(error) = reply(
        bridge,
        key,
        group,
        id,
        "confirmed",
        if offer.handover {
            "Combined handover confirmed. Checking the existing services and account before a private cold backup and service handover. No automatic retry will run."
        } else {
            "Update confirmed. Verifying the ngit release and Blossom downloads before replacing the bridge. No automatic installation retry will run."
        },
    ) {
        bridge.state = before;
        return Err(error);
    }
    Ok(temporary.keep())
}

pub async fn handle(
    bridge: &mut Bridge,
    key: &str,
    group: &str,
    id: &str,
    text: &str,
    plain: bool,
    recorded_at: Option<u64>,
) -> Result<()> {
    if !plain {
        return reply(
            bridge,
            key,
            group,
            id,
            "invalid",
            "Send /update as complete standalone text without attachments or a voice transcript.",
        );
    }
    if let Some(job) = &bridge.state.update_job {
        return reply(
            bridge,
            key,
            group,
            id,
            "pending",
            &format!(
                "An update to codr {} is pending. No second installation or automatic retry will start. Wait for its result; if it does not arrive, inspect the local codr update job before trying again.",
                job.version
            ),
        );
    }
    let arguments: Vec<_> = text.split_whitespace().collect();
    if arguments.len() == 2 && arguments[1].eq_ignore_ascii_case("cancel") {
        let before = bridge.state.clone();
        bridge.state.mappings.get_mut(key).unwrap().update_offer = None;
        if let Err(error) = reply(
            bridge,
            key,
            group,
            id,
            "cancelled",
            "Update offer cancelled. Nothing was installed.",
        ) {
            bridge.state = before;
            return Err(error);
        }
        if bridge
            .update_check
            .as_ref()
            .is_some_and(|check| check.key == key)
        {
            bridge.update_check.take().unwrap().task.abort();
        }
        return Ok(());
    }
    if matches!(arguments.len(), 2 | 3) && arguments[1].eq_ignore_ascii_case("confirm") {
        if arguments.len() == 2 {
            let fresh = bridge.update_check.is_none()
                && bridge.state.mappings[key]
                    .update_offer
                    .as_ref()
                    .is_some_and(|offer| {
                        recorded_at.is_some_and(|at| {
                            at > offer.expires_at.saturating_sub(600)
                                && at <= now().saturating_add(5)
                        })
                    });
            if !fresh {
                return reply(
                    bridge,
                    key,
                    group,
                    id,
                    "invalid",
                    "No fresh, verifiable update confirmation for this chat. Send /update, wait for the offer, then send /update confirm as a new message.",
                );
            }
        }
        let confirmation = arguments
            .get(2)
            .map(|value| value.to_string())
            .unwrap_or_else(|| {
                bridge.state.mappings[key]
                    .update_offer
                    .as_ref()
                    .map(|offer| offer.code.clone())
                    .unwrap_or_default()
            });
        let path = match prepare(bridge, key, group, id, &confirmation) {
            Ok(path) => path,
            Err(error) => return reply(bridge, key, group, id, "invalid", &error.to_string()),
        };
        let launched = helper(
            &path,
            "launch",
            path.to_str()
                .ok_or_else(|| anyhow::anyhow!("Update path is not UTF-8"))?,
        )
        .await;
        return match launched {
            Ok(value) if value["not_started"] == true => {
                let job = bridge.state.update_job.as_ref().unwrap();
                state::save_json(
                    &path.join("result.json"),
                    &json!({
                        "job_id":job.id,"version":job.version,"status":"failed",
                        "message":value["error"].as_str().unwrap_or("The updater did not start; nothing was installed."),
                    }),
                )?;
                poll(bridge).await
            }
            Ok(value) if value["started"] == true => Ok(()),
            _ => reply(
                bridge,
                key,
                group,
                id,
                "uncertain",
                "The updater launch was not confirmed. Inspect the local codr update job. No automatic retry will run.",
            ),
        };
    }
    if arguments.len() != 1 {
        return reply(
            bridge,
            key,
            group,
            id,
            "usage",
            "Use /update, /update confirm, or /update cancel.",
        );
    }
    if bridge.update_check.is_some() {
        return reply(
            bridge,
            key,
            group,
            id,
            "checking",
            "A read-only ngit release check is already running. Nothing has been installed.",
        );
    }
    if bridge.update_messaging_home.is_none() {
        return store_handover_offer(bridge, key, group, id);
    }
    let before = bridge.state.clone();
    bridge.state.mappings.get_mut(key).unwrap().update_offer = None;
    if let Err(error) = reply(
        bridge,
        key,
        group,
        id,
        "checking",
        "Checking the maintainer's stable ngit releases. Nothing will be installed without your confirmation.",
    ) {
        bridge.state = before;
        return Err(error);
    }
    let parent = bridge.config.state_path.parent().unwrap().to_path_buf();
    bridge.update_check = Some(Check {
        key: key.into(),
        group: group.into(),
        id: id.into(),
        task: tokio::spawn(async move {
            let temporary = tempfile::Builder::new()
                .prefix("update-check-")
                .tempdir_in(parent)?;
            tools(temporary.path())?;
            let response = helper(temporary.path(), "check", env!("CARGO_PKG_VERSION")).await?;
            Ok(serde_json::from_value(response["release"].clone())?)
        }),
    });
    Ok(())
}

/// Called only on worker startup, after its configuration and state opened.
pub fn ready(bridge: &Bridge) -> Result<()> {
    if let Some(job) = &bridge.state.update_job {
        state::save_json(
            &directory(bridge, job)?.join("ready.json"),
            &json!({
                "job_id":job.id,"version":env!("CARGO_PKG_VERSION"),"pid":std::process::id(),
            }),
        )?;
    }
    Ok(())
}

/// A combined replacement is ready only when its configured local account works.
/// This is not a relay/phone receipt check; offline relays must not force rollback.
pub async fn ready_when_healthy(bridge: &Bridge) -> Result<()> {
    if bridge.state.update_job.is_none() {
        return Ok(());
    }
    if bridge.update_messaging_home.is_some() {
        let accounts = tokio::time::timeout(
            Duration::from_secs(1),
            bridge.marmot.marmot(json!({"type":"account_list"})),
        )
        .await
        .map_err(|_| anyhow::anyhow!("Messaging startup is not ready"))??;
        ensure!(
            accounts["type"] == "account_list"
                && accounts["accounts"]
                    .as_array()
                    .is_some_and(|accounts| accounts.iter().any(
                        |account| account["account_id_hex"] == bridge.config.account_id_hex
                            && account["local_signing"] == true
                    )),
            "The configured messaging account is not ready"
        );
    }
    ready(bridge)
}

pub async fn poll(bridge: &mut Bridge) -> Result<()> {
    if bridge
        .update_check
        .as_ref()
        .is_some_and(|check| check.task.is_finished())
    {
        let check = bridge.update_check.take().unwrap();
        match check.task.await {
            Ok(Ok(Some(release))) if bridge.state.update_job.is_none() => {
                store_offer(bridge, &check.key, &check.group, &check.id, release)?
            }
            Ok(Ok(None)) => reply(
                bridge,
                &check.key,
                &check.group,
                &check.id,
                "current",
                &format!(
                    "Installed codr {}. No newer stable release is available from the pinned ngit maintainer for this check.",
                    env!("CARGO_PKG_VERSION")
                ),
            )?,
            _ => reply(
                bridge,
                &check.key,
                &check.group,
                &check.id,
                "check-failed",
                "The ngit release check could not complete. Check ngit v3 and relay access locally, then send /update again. Nothing was installed; GitHub was not used as a fallback.",
            )?,
        }
    }
    let Some(job) = bridge.state.update_job.clone() else {
        return Ok(());
    };
    let path = directory(bridge, &job)?.join("result.json");
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    ensure!(bytes.len() <= 64 * 1024, "Update receipt is too large");
    let result: Value = serde_json::from_slice(&bytes)?;
    ensure!(
        result["job_id"] == job.id && result["version"] == job.version,
        "Update receipt identity changed"
    );
    ensure!(
        matches!(
            result["status"].as_str(),
            Some("installed" | "rolled_back" | "failed")
        ),
        "Invalid update result"
    );
    let mut message = result["message"]
        .as_str()
        .filter(|text| text.len() <= 2048)
        .ok_or_else(|| anyhow::anyhow!("Missing update result message"))?
        .to_string();
    if result["status"] == "installed" && bridge.update_messaging_home.is_none() {
        message.push_str("\nMessaging still uses the external connector. Send /update, then /update confirm to finish the combined upgrade with a private cold backup. No terminal commands are needed for a supported managed installation.");
    }
    let before = bridge.state.clone();
    bridge
        .state
        .enqueue_manual(&job.group, &message, &format!("update-result:{}", job.id));
    bridge.state.update_job = None;
    if let Err(error) = bridge.save() {
        bridge.state = before;
        return Err(error);
    }
    Ok(())
}
