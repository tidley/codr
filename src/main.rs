use anyhow::Result;
use clap::{Parser, Subcommand, ValueEnum};
use herdr_white_noise_bridge::{BridgeConfig, BridgeState, bridge::Bridge, forwarding, state};
use std::path::PathBuf;

#[derive(Parser)]
#[command(version, about = "White Noise remote for local herdr agent sessions")]
struct Args {
    #[arg(long)]
    config: Option<PathBuf>,
    #[command(subcommand)]
    command: Option<Action>,
}

#[derive(Subcommand)]
enum Action {
    /// Print the worker's version and update compatibility cohort without opening state.
    UpdateInfo {
        /// Include combined-runtime capabilities for 0.10 and newer helpers.
        #[arg(long)]
        full: bool,
    },
    /// Run in the foreground. herdr starts this worker through the plugin manifest.
    Run {
        /// Enable the optional same-user portr desktop endpoint for this run.
        #[arg(long = "portr", alias = "porter", conflicts_with = "no_porter")]
        porter: bool,
        /// Disable the portr endpoint, including when enabled in configuration.
        #[arg(long = "no-portr", alias = "no-porter")]
        no_porter: bool,
        /// Host MDK messaging inside this process using an existing account home.
        /// Stop the external connector and back up its home before handover.
        #[arg(long)]
        messaging_home: Option<PathBuf>,
    },
    /// Forward desktop JSON requests to the running plugin.
    Porter {
        /// Keep stdin/stdout open for multiplexed requests and change notifications.
        #[arg(long)]
        stream: bool,
    },
    /// Print public routing state without message contents.
    Status,
    /// Validate live session history readers without changing bridge state.
    CheckReaders,
    /// Approve a paused replacement after inspection. Stop only the worker first.
    ConfirmSessionChange { key: String },
    /// Control global White Noise forwarding without stopping the worker.
    Forwarding {
        #[arg(value_enum, default_value = "status")]
        mode: ForwardingMode,
    },
    /// Resolve an uncertain creation. Inspect MDK groups before using --retry.
    ResolveCreate {
        key: String,
        #[arg(long, conflicts_with = "retry")]
        group: Option<String>,
        #[arg(long)]
        retry: bool,
    },
    /// Send a text notice to all mapped groups.
    Notify {
        text: String,
        #[arg(long)]
        group: Option<String>,
    },
    /// Clear an uncertain prompt after inspecting the live agent. Does not resubmit it.
    ClearPrompt { key: String },
    /// Update mapped group names and workspace/agent descriptions.
    RenameGroups,
}

#[derive(Clone, Copy, ValueEnum)]
enum ForwardingMode {
    Status,
    Desk,
    Away,
    Toggle,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    if let Some(Action::UpdateInfo { full }) = &args.command {
        let mut info = herdr_white_noise_bridge::update::info()?;
        if !full {
            info.as_object_mut().unwrap().remove("runtimes");
            info.as_object_mut().unwrap().remove("storage_cohort");
        }
        println!("{info}");
        return Ok(());
    }
    let config_path = args.config.unwrap_or_else(|| {
        let root = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config")
            });
        root.join("herdr-white-noise-bridge/config.toml")
    });
    let config = BridgeConfig::load(&config_path)?;
    if let Some(Action::Porter { stream }) = &args.command {
        return if *stream {
            herdr_white_noise_bridge::porter::forward_stream(&config.state_path).await
        } else {
            herdr_white_noise_bridge::porter::forward(&config.state_path).await
        };
    }
    if matches!(args.command, Some(Action::CheckReaders)) {
        let checked = check_readers(config).await?;
        println!("{}", serde_json::json!({"checked_sessions":checked}));
        return Ok(());
    }
    if let Some(Action::Forwarding { mode }) = args.command {
        let control = match mode {
            ForwardingMode::Status => forwarding::load(&config.state_path)?,
            ForwardingMode::Desk => forwarding::set(&config.state_path, false)?,
            ForwardingMode::Away => forwarding::set(&config.state_path, true)?,
            ForwardingMode::Toggle => forwarding::toggle(&config.state_path)?,
        };
        let state = BridgeState::load(&config.state_path)?;
        let resume_pending = control.enabled
            && (state.forwarding_resume_pending
                || state.forwarding_revision.as_deref().unwrap_or("initial") != control.revision);
        println!(
            "White Noise forwarding: {}",
            if resume_pending {
                "Away requested (waiting for the worker to checkpoint current history)"
            } else if control.enabled {
                "Away (on for all sessions)"
            } else {
                "At desk (muted; a new phone message wakes all sessions)"
            }
        );
        return Ok(());
    }
    if matches!(args.command, Some(Action::Status)) {
        let s = BridgeState::load(&config.state_path)?;
        let control = forwarding::load(&config.state_path)?;
        let resume_pending = control.enabled
            && (s.forwarding_resume_pending
                || s.forwarding_revision.as_deref().unwrap_or("initial") != control.revision);
        let forwarding_enabled = control.enabled && s.forwarding_enabled && !resume_pending;
        for (key, m) in s.mappings {
            println!(
                "{}",
                serde_json::json!({"session":key,"group":m.group_id_hex,"control":m.control,"creating_uncertain":m.creating,"active":m.active,"state":m.last_agent_status,"session_change_from":m.session_change_from,"pending_prompt":m.pending_prompt,"queued_prompts":m.prompt_queue.len(),"queue_submission_uncertain":m.queue_flight.as_ref().is_some_and(|flight| !flight.confirmed),"unresolved_progress":s.outbox.iter().filter(|item| Some(&item.group)==m.group_id_hex.as_ref() && item.response.as_ref().is_some_and(|delivery| delivery.finished && delivery.target.is_none() && delivery.initial_text.is_some() && !delivery.durable)).count(),"forwarding_enabled":forwarding_enabled,"forwarding_resume_pending":resume_pending})
            );
        }
        return Ok(());
    }
    let _lock = state::lock(&config.state_path)?;
    let mut bridge = Bridge::open(config)?;
    bridge.update_config_path = Some(std::fs::canonicalize(config_path)?);
    match args.command.unwrap_or(Action::Run {
        porter: false,
        no_porter: false,
        messaging_home: None,
    }) {
        Action::Run {
            porter,
            no_porter,
            messaging_home,
        } => {
            let messaging_home = messaging_home
                .map(|home| -> Result<PathBuf> {
                    anyhow::ensure!(home.is_absolute(), "Messaging home must be absolute");
                    Ok(std::fs::canonicalize(home)?)
                })
                .transpose()?;
            bridge.update_messaging_home = messaging_home.clone();
            let messaging = match messaging_home {
                Some(home) => Some(
                    herdr_white_noise_bridge::messaging::EmbeddedMessaging::start(
                        &bridge.config,
                        home,
                    )
                    .await?,
                ),
                None => None,
            };
            let enabled = !no_porter && (porter || bridge.config.portr_enabled);
            let result = tokio::select! {
                result = bridge.run(enabled) => result,
                result = terminate_signal() => {
                    result?;
                    bridge.save()
                }
            };
            if let Some(messaging) = messaging {
                messaging.shutdown().await?;
            }
            result
        }
        Action::Porter { .. } | Action::UpdateInfo { .. } => unreachable!(),
        Action::Notify { text, group } => {
            anyhow::ensure!(
                bridge.state.forwarding_enabled,
                "Forwarding is At desk; switch to Away before sending a notice"
            );
            let groups: Vec<_> = bridge
                .state
                .mappings
                .values()
                .filter(|m| !m.control)
                .filter_map(|m| m.group_id_hex.clone())
                .filter(|id| group.as_ref().is_none_or(|g| g == id))
                .collect();
            anyhow::ensure!(!groups.is_empty(), "No mapped groups exist yet");
            for group in groups {
                bridge.state.enqueue(&group, &text);
            }
            bridge.save()?;
            bridge.flush().await
        }
        Action::ResolveCreate { key, group, retry } => {
            let m = bridge
                .state
                .mappings
                .get_mut(&key)
                .ok_or_else(|| anyhow::anyhow!("Unknown session key"))?;
            anyhow::ensure!(
                m.creating && m.group_id_hex.is_none(),
                "This mapping has no uncertain creation"
            );
            anyhow::ensure!(
                group.is_some() || retry,
                "Choose --group or --retry after inspecting MDK groups"
            );
            if let Some(group) = group {
                anyhow::ensure!(
                    !group.is_empty() && group.bytes().all(|b| b.is_ascii_hexdigit()),
                    "Invalid group ID"
                );
                m.group_id_hex = Some(group);
            }
            m.creating = false;
            bridge.save()
        }
        Action::ClearPrompt { key } => {
            let mapping = bridge
                .state
                .mappings
                .get_mut(&key)
                .ok_or_else(|| anyhow::anyhow!("Unknown session key"))?;
            mapping.pending_prompt = false;
            mapping.completion_pending = false;
            mapping.queue_flight = None;
            if let Some(decision) = mapping.decision.as_mut() {
                decision.submitted = false;
                decision.announced = false;
                mapping.decision_refresh_pending = true;
            }
            let group = mapping
                .group_id_hex
                .clone()
                .filter(|_| mapping.response_id.is_some());
            if let Some(group) = group {
                bridge.state.update_response(
                    &key,
                    &group,
                    "Response cancelled locally. No prompt was resubmitted.",
                    true,
                );
            }
            bridge.save()
        }
        Action::ConfirmSessionChange { key } => bridge.confirm_session_change(&key).await,
        Action::RenameGroups => bridge.rename_groups().await,
        Action::Status => unreachable!(),
        Action::CheckReaders => unreachable!(),
        Action::Forwarding { .. } => unreachable!(),
    }
}

async fn terminate_signal() -> Result<()> {
    let mut signal = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    signal.recv().await;
    Ok(())
}

async fn check_readers(config: BridgeConfig) -> Result<usize> {
    use herdr_white_noise_bridge::{herdr, output, wire::Socket};
    let agents = herdr::snapshot(&Socket {
        path: config.herdr_socket.clone(),
    })
    .await
    .map_err(|_| {
        anyhow::anyhow!("Session history check failed: herdr snapshot is unavailable or invalid")
    })?;
    let sessions: std::collections::BTreeMap<_, _> = agents
        .into_iter()
        .filter(|agent| {
            agent
                .agent_session
                .as_ref()
                .is_some_and(|session| output::supported(&session.agent))
        })
        .map(|agent| (agent.key(), agent.agent_session.unwrap()))
        .collect();
    let checked = sessions.len();
    let limit = std::sync::Arc::new(tokio::sync::Semaphore::new(4));
    let mut readers = tokio::task::JoinSet::new();
    for session in sessions.into_values() {
        let config = config.clone();
        let limit = limit.clone();
        readers.spawn(async move {
            let _permit = limit.acquire_owned().await?;
            output::read_snapshot(&config, &session)
                .await
                .map_err(|error| {
                    anyhow::anyhow!(
                        "Session history check failed: {}",
                        output::read_error_message(&error)
                    )
                })?;
            Ok::<_, anyhow::Error>(())
        });
    }
    let mut failure = None;
    while let Some(result) = readers.join_next().await {
        if let Err(error) = result
            .map_err(|_| anyhow::anyhow!("Session history check failed: reader task failed"))
            .and_then(|result| result)
        {
            failure = Some(error);
        }
    }
    if let Some(error) = failure {
        return Err(error);
    }
    Ok(checked)
}
