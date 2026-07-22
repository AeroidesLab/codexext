use crate::error_code::internal_error;
use crate::error_code::invalid_request;
use crate::outgoing_message::ConnectionId;
use crate::outgoing_message::ConnectionRequestId;
use crate::outgoing_message::OutgoingMessageSender;
use crate::request_processors::ThreadRequestProcessor;
use crate::request_processors::TurnRequestProcessor;
use codex_app_server_protocol::CodexApplyPatchParams;
use codex_app_server_protocol::CodexApplyPatchResponse;
use codex_app_server_protocol::CodexGetHistoryParams;
use codex_app_server_protocol::CodexGetHistoryResponse;
use codex_app_server_protocol::CodexSpawnAgentParams;
use codex_app_server_protocol::CodexSpawnAgentResponse;
use codex_app_server_protocol::FileChangeApprovalDecision;
use codex_app_server_protocol::FileChangeRequestApprovalParams;
use codex_app_server_protocol::FileChangeRequestApprovalResponse;
use codex_app_server_protocol::JSONRPCErrorError;
use codex_app_server_protocol::ServerRequestPayload;
use codex_app_server_protocol::ThreadItem;
use codex_app_server_protocol::ThreadReadParams;
use codex_app_server_protocol::Turn;
use codex_app_server_protocol::TurnStartParams;
use codex_app_server_protocol::UserInput as V2UserInput;
use codex_core::NewThread;
use codex_core::StartThreadOptions;
use codex_core::ThreadManager;
use codex_exec_server::EnvironmentManager;
use codex_protocol::ThreadId;
use codex_protocol::error::CodexErr;
use codex_protocol::protocol::InitialHistory;
use codex_protocol::protocol::SessionSource;
use codex_protocol::protocol::SubAgentSource;
use codex_protocol::protocol::ThreadSource;
use codex_utils_path_uri::PathUri;
use std::sync::Arc;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;
use uuid::Uuid;

#[derive(Clone)]
pub(crate) struct CodexRequestProcessor {
    outgoing: Arc<OutgoingMessageSender>,
    thread_manager: Arc<ThreadManager>,
    environment_manager: Arc<EnvironmentManager>,
    thread_processor: ThreadRequestProcessor,
    turn_processor: TurnRequestProcessor,
}

impl CodexRequestProcessor {
    pub(crate) fn new(
        outgoing: Arc<OutgoingMessageSender>,
        thread_manager: Arc<ThreadManager>,
        environment_manager: Arc<EnvironmentManager>,
        thread_processor: ThreadRequestProcessor,
        turn_processor: TurnRequestProcessor,
    ) -> Self {
        Self {
            outgoing,
            thread_manager,
            environment_manager,
            thread_processor,
            turn_processor,
        }
    }

    /// Apply a unified-diff patch against an existing thread's workspace.
    ///
    /// The patch only lands on disk after the client approves the native
    /// `item/fileChange/requestApproval` server request, mirroring the
    /// in-turn apply_patch approval flow.
    pub(crate) async fn apply_patch(
        &self,
        connection_id: ConnectionId,
        params: CodexApplyPatchParams,
    ) -> Result<CodexApplyPatchResponse, JSONRPCErrorError> {
        let CodexApplyPatchParams {
            thread_id,
            patch,
            cwd,
            reason,
        } = params;
        let thread_id = ThreadId::from_string(&thread_id)
            .map_err(|err| invalid_request(format!("invalid thread id: {err}")))?;
        let thread = self
            .thread_manager
            .get_thread(thread_id)
            .await
            .map_err(|_| invalid_request(format!("thread not found: {thread_id}")))?;

        let parsed = codex_apply_patch::parse_patch(&patch)
            .map_err(|err| invalid_request(format!("invalid patch: {err}")))?;
        if parsed.hunks.is_empty() {
            return Err(invalid_request("patch contains no file changes"));
        }
        let cwd_uri = match cwd {
            Some(cwd) => PathUri::from_host_native_path(&cwd)
                .map_err(|err| invalid_request(format!("invalid cwd: {err}")))?,
            None => PathUri::from_abs_path(thread.config_snapshot().await.cwd()),
        };
        let file_changes = parsed
            .hunks
            .iter()
            .map(|hunk| {
                hunk.resolve_path(&cwd_uri)
                    .map(|path| path.inferred_native_path_string())
                    .unwrap_or_else(|_| hunk.path().to_string_lossy().into_owned())
            })
            .collect::<Vec<_>>();

        // Route through the native file-change approval chain: the client
        // receives `item/fileChange/requestApproval` and the patch is written
        // only after an accept decision.
        let (_approval_request_id, rx) = self
            .outgoing
            .send_request_to_connections(
                Some(&[connection_id]),
                ServerRequestPayload::FileChangeRequestApproval(FileChangeRequestApprovalParams {
                    thread_id: thread_id.to_string(),
                    // Standalone request: there is no in-progress turn or
                    // file-change item to reference.
                    turn_id: String::new(),
                    item_id: format!("codex-apply-patch-{}", Uuid::new_v4()),
                    started_at_ms: now_unix_timestamp_ms(),
                    reason,
                    grant_root: None,
                }),
                Some(thread_id),
            )
            .await;

        let decision = match rx.await {
            Ok(Ok(value)) => {
                match serde_json::from_value::<FileChangeRequestApprovalResponse>(value) {
                    Ok(response) => response.decision,
                    Err(err) => {
                        return Err(internal_error(format!(
                            "failed to deserialize FileChangeRequestApprovalResponse: {err}"
                        )));
                    }
                }
            }
            Ok(Err(err)) => return Err(err),
            Err(err) => {
                return Err(internal_error(format!(
                    "file change approval request failed: {err}"
                )));
            }
        };

        let approved = matches!(
            decision,
            FileChangeApprovalDecision::Accept | FileChangeApprovalDecision::AcceptForSession
        );
        if !approved {
            return Ok(CodexApplyPatchResponse {
                applied: false,
                file_changes,
            });
        }

        let fs = self
            .environment_manager
            .try_local_environment()
            .map(|environment| environment.get_filesystem())
            .ok_or_else(|| internal_error("local filesystem is not configured"))?;
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        codex_apply_patch::apply_patch(
            &patch,
            &cwd_uri,
            &mut stdout,
            &mut stderr,
            fs.as_ref(),
            /*sandbox*/ None,
        )
        .await
        .map_err(|failure| {
            let (error, _delta) = failure.into_parts();
            let stderr = String::from_utf8_lossy(&stderr).trim().to_string();
            if stderr.is_empty() {
                invalid_request(format!("failed to apply patch: {error}"))
            } else {
                invalid_request(format!("failed to apply patch: {error}: {stderr}"))
            }
        })?;
        Ok(CodexApplyPatchResponse {
            applied: true,
            file_changes,
        })
    }

    /// Read a thread's history. `full` returns the structured turns produced by
    /// the canonical thread-history projection; `compact` renders the same
    /// projected turns as plain text.
    pub(crate) async fn get_history(
        &self,
        params: CodexGetHistoryParams,
    ) -> Result<CodexGetHistoryResponse, JSONRPCErrorError> {
        let mode = params.mode.as_deref().unwrap_or("compact");
        let response = self
            .thread_processor
            .thread_read_response_inner(ThreadReadParams {
                thread_id: params.thread_id,
                include_turns: true,
            })
            .await?;
        let turns = response.thread.turns;
        match mode {
            "full" => Ok(CodexGetHistoryResponse::Full { turns }),
            "compact" => Ok(CodexGetHistoryResponse::Compact {
                text: compact_history_text(&turns),
            }),
            other => Err(invalid_request(format!(
                "invalid mode: {other} (expected \"full\" or \"compact\")"
            ))),
        }
    }

    /// Fork a child thread off an existing parent thread and start its first
    /// turn with the supplied prompt.
    pub(crate) async fn spawn_agent(
        &self,
        request_id: ConnectionRequestId,
        params: CodexSpawnAgentParams,
        app_server_client_name: Option<String>,
        app_server_client_version: Option<String>,
        supports_openai_form_elicitation: bool,
    ) -> Result<CodexSpawnAgentResponse, JSONRPCErrorError> {
        let CodexSpawnAgentParams {
            parent_thread_id,
            prompt,
            agent_role,
            model,
        } = params;
        let parent_thread_id = ThreadId::from_string(&parent_thread_id)
            .map_err(|err| invalid_request(format!("invalid thread id: {err}")))?;

        // A Gateway sub-agent belongs to the active parent runtime. Use the
        // native subagent fork path so the rollout and thread projection both
        // retain parentThreadId, depth and agentRole. A cold/stale parent is an
        // invalid Gateway session and must not silently fall back to process
        // cwd or an unrelated permission configuration.
        let parent = self
            .thread_manager
            .get_thread(parent_thread_id)
            .await
            .map_err(|_| {
                invalid_request(format!("parent thread is not loaded: {parent_thread_id}"))
            })?;
        let mut config = (*parent.config().await).clone();
        if let Some(model) = model {
            config.model = Some(model);
        }
        let parent_source = parent.config_snapshot().await.session_source;
        let depth = match parent_source {
            SessionSource::SubAgent(SubAgentSource::ThreadSpawn { depth, .. }) => {
                depth.saturating_add(1)
            }
            _ => 1,
        };
        let session_source = SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
            parent_thread_id,
            depth,
            agent_path: None,
            agent_nickname: None,
            agent_role,
        });
        let environments = self
            .thread_manager
            .default_environment_selections(&config.cwd, &config.workspace_roots);

        let parent_trace = self.outgoing.request_trace_context(&request_id).await;
        let NewThread {
            thread_id: child_thread_id,
            ..
        } = self
            .thread_manager
            .spawn_subagent(
                parent_thread_id,
                StartThreadOptions {
                    config,
                    allow_provider_model_fallback: false,
                    initial_history: InitialHistory::New,
                    history_mode: None,
                    session_source: Some(session_source),
                    thread_source: Some(ThreadSource::Subagent),
                    dynamic_tools: Vec::new(),
                    metrics_service_name: None,
                    parent_trace,
                    environments,
                    thread_extension_init: Default::default(),
                    supports_openai_form_elicitation,
                },
            )
            .await
            .map_err(|err| match err {
                CodexErr::InvalidRequest(message) => invalid_request(message),
                err => internal_error(format!("error forking thread: {err}")),
            })?;

        // Subscribe the requesting connection to the child thread so its
        // notifications (and any approval requests it raises) reach the
        // caller, mirroring the thread/fork auto-attach.
        self.thread_processor
            .try_attach_thread_listener(child_thread_id, vec![request_id.connection_id])
            .await;

        // Start the child's first turn. The turn/start response payload is
        // superseded by this RPC's own response, so it is intentionally
        // dropped here.
        self.turn_processor
            .turn_start(
                request_id,
                TurnStartParams {
                    thread_id: child_thread_id.to_string(),
                    input: vec![V2UserInput::Text {
                        text: prompt,
                        text_elements: Vec::new(),
                    }],
                    ..Default::default()
                },
                app_server_client_name,
                app_server_client_version,
                supports_openai_form_elicitation,
            )
            .await?;

        Ok(CodexSpawnAgentResponse {
            child_thread_id: child_thread_id.to_string(),
        })
    }
}

/// Plain-text projection of projected thread history turns.
fn compact_history_text(turns: &[Turn]) -> String {
    let mut text = String::new();
    for turn in turns {
        for item in &turn.items {
            match item {
                ThreadItem::UserMessage { content, .. } => {
                    for input in content {
                        if let V2UserInput::Text {
                            text: input_text, ..
                        } = input
                        {
                            push_section(&mut text, "User", input_text);
                        }
                    }
                }
                ThreadItem::AgentMessage { text: message, .. } => {
                    push_section(&mut text, "Assistant", message);
                }
                ThreadItem::Plan { text: plan, .. } => {
                    push_section(&mut text, "Plan", plan);
                }
                ThreadItem::Reasoning { summary, .. } => {
                    for entry in summary {
                        push_section(&mut text, "Reasoning", entry);
                    }
                }
                ThreadItem::CommandExecution {
                    command,
                    aggregated_output,
                    exit_code,
                    status,
                    ..
                } => {
                    let mut section = format!("$ {command}");
                    if let Some(output) = aggregated_output {
                        section.push_str(&format!("\n{output}"));
                    }
                    section.push_str(&format!("\n[{status:?}"));
                    if let Some(exit_code) = exit_code {
                        section.push_str(&format!(" exit={exit_code}"));
                    }
                    section.push(']');
                    push_section(&mut text, "Command", &section);
                }
                ThreadItem::FileChange {
                    changes, status, ..
                } => {
                    let paths = changes
                        .iter()
                        .map(|change| change.path.clone())
                        .collect::<Vec<_>>()
                        .join(", ");
                    push_section(&mut text, "File change", &format!("{paths} [{status:?}]"));
                }
                _ => {}
            }
        }
    }
    text
}

fn push_section(out: &mut String, role: &str, body: &str) {
    if !out.is_empty() {
        out.push_str("\n\n");
    }
    out.push_str(&format!("{role}: {body}"));
}

fn now_unix_timestamp_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or_default()
}
