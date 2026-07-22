use super::Turn;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use ts_rs::TS;

/// Apply a patch to an existing thread's workspace through the native
/// file-change approval chain.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct CodexApplyPatchParams {
    /// Thread the patch is associated with. The thread must exist; the
    /// file-change approval request references this thread.
    pub thread_id: String,
    /// Patch payload in the Codex `apply_patch` grammar
    /// (`*** Begin Patch` ... `*** End Patch`).
    pub patch: String,
    /// Working directory used to resolve relative patch paths. Defaults to the
    /// thread's configured cwd.
    #[ts(optional = nullable)]
    pub cwd: Option<String>,
    /// Optional explanatory reason surfaced in the approval request.
    #[ts(optional = nullable)]
    pub reason: Option<String>,
}

/// Result of `codex/applyPatch`.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct CodexApplyPatchResponse {
    /// Whether the patch was approved and written to disk. `false` when the
    /// file-change approval request was declined or cancelled.
    pub applied: bool,
    /// Native paths of the files touched by the patch.
    pub file_changes: Vec<String>,
}

/// Read a thread's history in either structured or projected plain-text form.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct CodexGetHistoryParams {
    /// Thread to read.
    pub thread_id: String,
    /// `"full"` returns structured turns; `"compact"` returns a plain-text
    /// projection of the same projected history. Defaults to `"compact"`.
    #[ts(optional = nullable)]
    pub mode: Option<String>,
}

/// Result of `codex/getHistory`, tagged by the requested mode.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(tag = "mode", rename_all = "camelCase")]
#[ts(tag = "mode")]
#[ts(export_to = "v2/")]
pub enum CodexGetHistoryResponse {
    #[serde(rename = "full")]
    #[ts(rename = "full")]
    Full { turns: Vec<Turn> },
    #[serde(rename = "compact")]
    #[ts(rename = "compact")]
    Compact { text: String },
}

/// Fork a child thread from an existing parent thread and start its first
/// turn with the supplied prompt.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct CodexSpawnAgentParams {
    /// Parent thread to fork from.
    pub parent_thread_id: String,
    /// Initial task given to the child thread.
    pub prompt: String,
    /// Optional role recorded in the child thread's metadata.
    #[ts(optional = nullable)]
    pub agent_role: Option<String>,
    /// Optional model override for the child thread.
    #[ts(optional = nullable)]
    pub model: Option<String>,
}

/// Result of `codex/spawnAgent`.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct CodexSpawnAgentResponse {
    /// Id of the forked child thread.
    pub child_thread_id: String,
}
