use std::{
    collections::{BTreeSet, VecDeque},
    fs::{self, OpenOptions},
    io::{BufRead, BufReader, Read, Seek, SeekFrom, Write},
    path::{Component, Path, PathBuf},
};

use coder_core::{FinalReport, ReportStatus, RunId, RunState, RunStatus};
use coder_events::{
    redact_payload, redact_secret_text, CoderEvent, LargePayloadRef,
    DEFAULT_LARGE_PAYLOAD_PREVIEW_LIMIT,
};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use thiserror::Error;
use time::OffsetDateTime;

const MAX_REPO_EVIDENCE_STRING_CHARS: usize = 16_000;
const MAX_REPO_EVIDENCE_LIST_ITEMS: usize = 300;
const MAX_REPO_EVIDENCE_JSON_CHARS: usize = 256_000;
const MAX_DURABLE_READ_BYTES: u64 = 50 * 1024 * 1024;
const REPO_EVIDENCE_SECRET_MARKERS: &[&str] = &[
    "deepseek_api_key",
    "llm_api_key",
    "api_key",
    "password",
    "begin rsa",
    "secret_key",
    "private_key",
];
const LOCAL_STORE_DIRS: &[&str] = &[
    "sessions",
    "runs",
    "background-tasks",
    "timeline",
    "blobs",
    "artifacts",
    "settings",
    "checkpoints",
    "changesets",
    "repo-index",
    "plugin-cache",
    "skill-cache",
    "logs",
    "tmp",
];
const DISPOSABLE_CACHE_DIRS: &[&str] = &["repo-index", "plugin-cache", "skill-cache", "tmp"];
const COMPACTION_STATE_DIR: &str = "checkpoints/compaction";
const GOAL_STATE_DIR: &str = "checkpoints/goals";
pub const MAX_DURABLE_JSONL_PAGE_LIMIT: usize = 1000;
pub const MAX_CACHE_USAGE_SCAN_ENTRIES: usize = MAX_DURABLE_JSONL_PAGE_LIMIT;

#[derive(Debug, Clone)]
pub struct RunStore {
    root: PathBuf,
}

impl RunStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn ensure_local_layout(&self) -> Result<LocalStoreLayout, StoreError> {
        fs::create_dir_all(&self.root)?;
        for dir in LOCAL_STORE_DIRS {
            fs::create_dir_all(self.root.join(dir))?;
        }
        Ok(LocalStoreLayout::new(&self.root))
    }

    pub fn write_metadata(&self, state: &RunState) -> Result<(), StoreError> {
        write_json(
            self.safe_run_dir(&state.run_id)?.join("metadata.json"),
            state,
        )
    }

    pub fn read_metadata(&self, run_id: &RunId) -> Result<Option<RunState>, StoreError> {
        read_json_optional(self.safe_run_dir(run_id)?.join("metadata.json"))
    }

    pub fn write_run_config_snapshot<T: Serialize>(
        &self,
        run_id: &RunId,
        value: &T,
    ) -> Result<String, StoreError> {
        let path = self
            .safe_run_dir(run_id)?
            .join("project-config.snapshot.json");
        write_json(&path, value)?;
        Ok(format!(
            "run-config://runs/{}/project-config.snapshot.json",
            run_id.as_str()
        ))
    }

    pub fn read_run_config_snapshot_json(
        &self,
        run_id: &RunId,
    ) -> Result<Option<Value>, StoreError> {
        read_json_optional(
            self.safe_run_dir(run_id)?
                .join("project-config.snapshot.json"),
        )
    }

    pub fn write_permission_settings<T: Serialize>(
        &self,
        destination: &str,
        value: &T,
    ) -> Result<String, StoreError> {
        reject_session_record_secret_like_json(&serde_json::to_value(value)?)?;
        let safe_destination = safe_file_name(destination)?;
        let path = self.permission_settings_path(&safe_destination);
        write_json(&path, value)?;
        Ok(format!("settings://permissions/{safe_destination}.json"))
    }

    pub fn read_permission_settings<T: DeserializeOwned>(
        &self,
        destination: &str,
    ) -> Result<Option<T>, StoreError> {
        let safe_destination = safe_file_name(destination)?;
        read_json_optional(self.permission_settings_path(&safe_destination))
    }

    pub fn list_run_summaries(&self) -> Result<Vec<StoredRunSummary>, StoreError> {
        let runs_dir = self.root.join("runs");
        if !runs_dir.exists() {
            return Ok(Vec::new());
        }

        let mut summaries = Vec::new();
        for entry in fs::read_dir(runs_dir)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let Some(run_name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if safe_store_segment(&run_name, "run_id").is_err() {
                continue;
            }

            let run_id = RunId::from_string(run_name.clone());
            let metadata = self.read_metadata(&run_id)?;
            let event_count = self.event_count(&run_id)?;
            let has_report = self.read_report(&run_id)?.is_some();
            let repo_evidence_count = self.repo_evidence_count(&run_id)?;
            summaries.push(StoredRunSummary {
                run_id: run_name,
                metadata,
                event_count,
                has_report,
                repo_evidence_count,
            });
        }
        summaries.sort_by(|left, right| left.run_id.cmp(&right.run_id));
        Ok(summaries)
    }

    pub fn append_event(&self, run_id: &RunId, event: &CoderEvent) -> Result<(), StoreError> {
        let path = self.safe_run_dir(run_id)?.join("events.jsonl");
        ensure_parent(&path)?;
        let mut event = event.clone();
        event.payload = redact_payload(event.payload);
        let mut file = OpenOptions::new().create(true).append(true).open(path)?;
        file.write_all(event.to_jsonl()?.as_bytes())?;
        Ok(())
    }

    pub fn read_events(&self, run_id: &RunId) -> Result<Vec<CoderEvent>, StoreError> {
        let path = self.safe_run_dir(run_id)?.join("events.jsonl");
        read_jsonl_records(path)
    }

    pub fn read_events_page(
        &self,
        run_id: &RunId,
        options: DurableJsonlPageOptions,
    ) -> Result<DurableJsonlPage<CoderEvent>, StoreError> {
        let path = self.safe_run_dir(run_id)?.join("events.jsonl");
        read_jsonl_page(path, options, |event: &CoderEvent| event.sequence)
    }

    pub fn event_count(&self, run_id: &RunId) -> Result<usize, StoreError> {
        let path = self.safe_run_dir(run_id)?.join("events.jsonl");
        count_jsonl_records(path)
    }

    pub fn append_session_record(
        &self,
        session_id: &str,
        sequence: u64,
        kind: impl Into<String>,
        payload: Value,
    ) -> Result<(), StoreError> {
        let session_id = safe_file_name(session_id)?;
        let kind = kind.into();
        reject_session_record_secret_like_text(&kind)?;
        reject_session_record_secret_like_json(&payload)?;
        let record = SessionJsonlRecord {
            session_id: session_id.clone(),
            sequence,
            kind,
            created_at: OffsetDateTime::now_utc(),
            payload,
        };
        let path = self
            .root
            .join("sessions")
            .join(format!("{session_id}.jsonl"));
        ensure_parent(&path)?;
        let mut file = OpenOptions::new().create(true).append(true).open(path)?;
        writeln!(file, "{}", serde_json::to_string(&record)?)?;
        Ok(())
    }

    pub fn append_session_record_next(
        &self,
        session_id: &str,
        kind: impl Into<String>,
        payload: Value,
    ) -> Result<u64, StoreError> {
        let session_id = safe_file_name(session_id)?;
        let kind = kind.into();
        reject_session_record_secret_like_text(&kind)?;
        reject_session_record_secret_like_json(&payload)?;
        let sequence = self.next_session_sequence(&session_id)?;
        self.append_session_record(&session_id, sequence, kind, payload)?;
        Ok(sequence)
    }

    pub fn read_session_records(
        &self,
        session_id: &str,
    ) -> Result<Vec<SessionJsonlRecord>, StoreError> {
        let session_id = safe_file_name(session_id)?;
        let path = self
            .root
            .join("sessions")
            .join(format!("{session_id}.jsonl"));
        read_jsonl_records(path)
    }

    pub fn read_session_records_page(
        &self,
        session_id: &str,
        options: DurableJsonlPageOptions,
    ) -> Result<DurableJsonlPage<SessionJsonlRecord>, StoreError> {
        let session_id = safe_file_name(session_id)?;
        let path = self
            .root
            .join("sessions")
            .join(format!("{session_id}.jsonl"));
        read_jsonl_page(path, options, |record: &SessionJsonlRecord| record.sequence)
    }

    pub fn append_subagent_transcript_record_next(
        &self,
        run_id: &RunId,
        agent_id: &str,
        parent_sequence: Option<u64>,
        kind: impl Into<String>,
        payload: Value,
    ) -> Result<u64, StoreError> {
        let safe_agent_id = safe_file_name(agent_id)?;
        let kind = kind.into();
        reject_session_record_secret_like_text(&kind)?;
        reject_session_record_secret_like_json(&payload)?;
        let sequence = self.next_subagent_sequence(run_id, &safe_agent_id)?;
        let record = SubagentTranscriptRecord {
            run_id: run_id.as_str().to_owned(),
            agent_id: safe_agent_id.clone(),
            sequence,
            parent_sequence,
            kind,
            created_at: OffsetDateTime::now_utc(),
            payload,
        };
        let path = self.subagent_transcript_path(run_id, &safe_agent_id)?;
        ensure_parent(&path)?;
        let mut file = OpenOptions::new().create(true).append(true).open(path)?;
        writeln!(file, "{}", serde_json::to_string(&record)?)?;
        Ok(sequence)
    }

    pub fn read_subagent_transcript_records(
        &self,
        run_id: &RunId,
        agent_id: &str,
    ) -> Result<Vec<SubagentTranscriptRecord>, StoreError> {
        let safe_agent_id = safe_file_name(agent_id)?;
        let path = self.subagent_transcript_path(run_id, &safe_agent_id)?;
        read_jsonl_records(path)
    }

    pub fn read_subagent_transcript_records_page(
        &self,
        run_id: &RunId,
        agent_id: &str,
        options: DurableJsonlPageOptions,
    ) -> Result<DurableJsonlPage<SubagentTranscriptRecord>, StoreError> {
        let safe_agent_id = safe_file_name(agent_id)?;
        let path = self.subagent_transcript_path(run_id, &safe_agent_id)?;
        read_jsonl_page(path, options, |record: &SubagentTranscriptRecord| {
            record.sequence
        })
    }

    pub fn append_run_content_replacement_record_next(
        &self,
        run_id: &RunId,
        replacements: Vec<ContentReplacementRecord>,
    ) -> Result<u64, StoreError> {
        reject_session_record_secret_like_json(&serde_json::to_value(&replacements)?)?;
        let sequence = self.next_run_content_replacement_sequence(run_id)?;
        let record = RunContentReplacementEntry {
            run_id: run_id.as_str().to_owned(),
            sequence,
            kind: "content-replacement".to_owned(),
            created_at: OffsetDateTime::now_utc(),
            replacements,
        };
        let path = self.run_content_replacements_path(run_id)?;
        ensure_parent(&path)?;
        let mut file = OpenOptions::new().create(true).append(true).open(path)?;
        writeln!(file, "{}", serde_json::to_string(&record)?)?;
        Ok(sequence)
    }

    pub fn read_run_content_replacement_records(
        &self,
        run_id: &RunId,
    ) -> Result<Vec<RunContentReplacementEntry>, StoreError> {
        read_jsonl_records(self.run_content_replacements_path(run_id)?)
    }

    pub fn read_run_content_replacement_records_page(
        &self,
        run_id: &RunId,
        options: DurableJsonlPageOptions,
    ) -> Result<DurableJsonlPage<RunContentReplacementEntry>, StoreError> {
        read_jsonl_page(
            self.run_content_replacements_path(run_id)?,
            options,
            |record: &RunContentReplacementEntry| record.sequence,
        )
    }

    pub fn write_subagent_metadata(
        &self,
        run_id: &RunId,
        agent_id: &str,
        metadata: &SubagentMetadata,
    ) -> Result<String, StoreError> {
        let safe_agent_id = safe_file_name(agent_id)?;
        reject_session_record_secret_like_json(&serde_json::to_value(metadata)?)?;
        let path = self.subagent_metadata_path(run_id, &safe_agent_id)?;
        write_json(&path, metadata)?;
        Ok(format!(
            "subagent://runs/{}/subagents/agent-{safe_agent_id}.meta.json",
            run_id.as_str()
        ))
    }

    pub fn read_subagent_metadata(
        &self,
        run_id: &RunId,
        agent_id: &str,
    ) -> Result<Option<SubagentMetadata>, StoreError> {
        let safe_agent_id = safe_file_name(agent_id)?;
        read_json_optional(self.subagent_metadata_path(run_id, &safe_agent_id)?)
    }

    pub fn write_subagent_background_task_record(
        &self,
        record: &SubagentBackgroundTaskRecord,
    ) -> Result<String, StoreError> {
        let safe_task_id = safe_file_name(&record.task_id)?;
        reject_session_record_secret_like_json(&serde_json::to_value(record)?)?;
        let path = self.subagent_background_task_record_path(&safe_task_id);
        write_json(&path, record)?;
        Ok(format!("background-task://subagents/{safe_task_id}.json"))
    }

    pub fn read_subagent_background_task_record(
        &self,
        task_id: &str,
    ) -> Result<Option<SubagentBackgroundTaskRecord>, StoreError> {
        let safe_task_id = safe_file_name(task_id)?;
        read_json_optional(self.subagent_background_task_record_path(&safe_task_id))
    }

    pub fn write_command_background_task_record(
        &self,
        record: &CommandBackgroundTaskRecord,
    ) -> Result<String, StoreError> {
        let safe_task_id = safe_file_name(&record.task_id)?;
        reject_session_record_secret_like_json(&serde_json::to_value(record)?)?;
        let path = self.command_background_task_record_path(&safe_task_id);
        write_json(&path, record)?;
        Ok(format!("background-task://commands/{safe_task_id}.json"))
    }

    pub fn read_command_background_task_record(
        &self,
        task_id: &str,
    ) -> Result<Option<CommandBackgroundTaskRecord>, StoreError> {
        let safe_task_id = safe_file_name(task_id)?;
        read_json_optional(self.command_background_task_record_path(&safe_task_id))
    }

    pub fn write_command_background_output_tail(
        &self,
        task_id: &str,
        output: &[u8],
    ) -> Result<String, StoreError> {
        let safe_task_id = safe_file_name(task_id)?;
        if output.len() as u64 > MAX_DURABLE_READ_BYTES {
            return Err(StoreError::DurableReadLimitExceeded {
                path: self
                    .command_background_output_path(&safe_task_id)
                    .display()
                    .to_string(),
                bytes: output.len() as u64,
                max_bytes: MAX_DURABLE_READ_BYTES,
            });
        }
        let path = self.command_background_output_path(&safe_task_id);
        ensure_parent(&path)?;
        fs::write(&path, output)?;
        Ok(format!(
            "background-task-output://commands/{safe_task_id}.output"
        ))
    }

    pub fn read_command_background_output_tail(
        &self,
        task_id: &str,
        max_bytes: usize,
    ) -> Result<CommandBackgroundOutputTail, StoreError> {
        let safe_task_id = safe_file_name(task_id)?;
        let path = self.command_background_output_path(&safe_task_id);
        let (bytes, total_bytes, truncated) = read_file_tail_bytes(&path, max_bytes)?;
        Ok(CommandBackgroundOutputTail {
            output: String::from_utf8_lossy(&bytes).to_string(),
            bytes: total_bytes,
            truncated,
        })
    }

    fn next_session_sequence(&self, session_id: &str) -> Result<u64, StoreError> {
        let path = self.root.join("sessions").join(format!("{session_id}.seq"));
        ensure_parent(&path)?;
        let next = match fs::read_to_string(&path) {
            Ok(text) => text.trim().parse::<u64>().ok().unwrap_or(0) + 1,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => 1,
            Err(error) => return Err(StoreError::Io(error)),
        };
        write_json(&path, &next)?;
        Ok(next)
    }

    fn next_subagent_sequence(
        &self,
        run_id: &RunId,
        safe_agent_id: &str,
    ) -> Result<u64, StoreError> {
        let path = self
            .subagent_dir(run_id)?
            .join(format!("agent-{safe_agent_id}.seq"));
        ensure_parent(&path)?;
        let next = match fs::read_to_string(&path) {
            Ok(text) => text.trim().parse::<u64>().ok().unwrap_or(0) + 1,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => 1,
            Err(error) => return Err(StoreError::Io(error)),
        };
        write_json(&path, &next)?;
        Ok(next)
    }

    fn next_run_content_replacement_sequence(&self, run_id: &RunId) -> Result<u64, StoreError> {
        let path = self.safe_run_dir(run_id)?.join("content-replacements.seq");
        ensure_parent(&path)?;
        let next = match fs::read_to_string(&path) {
            Ok(text) => text.trim().parse::<u64>().ok().unwrap_or(0) + 1,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => 1,
            Err(error) => return Err(StoreError::Io(error)),
        };
        write_json(&path, &next)?;
        Ok(next)
    }

    pub fn write_report(&self, run_id: &RunId, report: &FinalReport) -> Result<String, StoreError> {
        let mut report = report.clone();
        redact_final_report(&mut report);
        self.write_artifact(run_id, "final-report.json", &report)
    }

    pub fn build_evidence_report(&self, run_id: &RunId) -> Result<FinalReport, StoreError> {
        let metadata = self.read_metadata(run_id)?;
        let events = self.read_events(run_id)?;
        let repo_evidence = self.list_repo_evidence(run_id)?;
        if metadata.is_none() && events.is_empty() && repo_evidence.is_empty() {
            return Err(StoreError::RunNotFound(run_id.as_str().to_owned()));
        }

        let mut checks = Vec::new();
        let mut blockers = Vec::new();
        let mut verification_blockers = Vec::new();
        let mut changed_file_seen = BTreeSet::new();
        let mut patch_ref_seen = BTreeSet::new();
        let mut evidence_ref_seen = BTreeSet::new();
        let mut evidence_refs = Vec::new();
        let mut plan_context = None;
        let mut requested = None;
        let mut completed = Vec::new();
        if !events.is_empty() {
            evidence_ref_seen.insert((
                "event_log".to_owned(),
                format!("eventlog://runs/{}", run_id.as_str()),
            ));
        }

        for event in &events {
            for reference in &event.refs {
                let key = (reference.label.clone(), reference.uri.clone());
                evidence_ref_seen.insert(key);
            }

            match event.kind.as_str() {
                "run.started" => {
                    requested = payload_string(&event.payload, "task").or(requested);
                    if let Some(value) = event
                        .payload
                        .get("plan_context")
                        .filter(|value| !value.is_null())
                    {
                        plan_context = Some(value.clone());
                    }
                }
                "approval.requested" => {
                    let approval_type = payload_string(&event.payload, "approval_type")
                        .unwrap_or_else(|| "approval".to_owned());
                    if approval_type == "command" {
                        let command = payload_string(&event.payload, "command")
                            .unwrap_or_else(|| "command".to_owned());
                        blockers.push(format!("Command requires approval: {command}"));
                    } else if approval_type == "patch_apply" {
                        let patch_file = payload_string(&event.payload, "patch_file")
                            .unwrap_or_else(|| "patch".to_owned());
                        blockers.push(format!("Patch apply requires approval: {patch_file}"));
                        collect_patch_files(&event.payload, &mut changed_file_seen);
                        collect_patch_ref(&event.payload, &mut patch_ref_seen);
                    }
                }
                "command.completed" | "command.failed" => {
                    let command = payload_string(&event.payload, "command")
                        .unwrap_or_else(|| "command".to_owned());
                    let status = payload_string(&event.payload, "status")
                        .unwrap_or_else(|| event.kind.trim_start_matches("command.").to_owned());
                    completed.push(format!("Command {status}: {command}"));
                    let returncode = event
                        .payload
                        .get("returncode")
                        .and_then(|value| value.as_i64())
                        .map(|code| format!(" exit {code}"))
                        .unwrap_or_default();
                    checks.push(format!("{command}: {status}{returncode}"));
                    let passed = event
                        .payload
                        .get("passed")
                        .and_then(|value| value.as_bool())
                        .unwrap_or(event.kind == "command.completed");
                    let cancelled = status == "cancelled";
                    if !passed {
                        if cancelled {
                            continue;
                        } else if event
                            .payload
                            .get("timed_out")
                            .and_then(|value| value.as_bool())
                            .unwrap_or(false)
                        {
                            blockers.push(format!("Command timed out: {command}"));
                        } else {
                            blockers.push(format!("Command failed: {command}"));
                        }
                    }
                }
                "patch.previewed" | "patch.applied" | "patch.failed" => {
                    completed.push(format!(
                        "Patch {}",
                        event.kind.trim_start_matches("patch.").replace('_', " ")
                    ));
                    collect_patch_files(&event.payload, &mut changed_file_seen);
                    collect_patch_ref(&event.payload, &mut patch_ref_seen);
                    for reference in &event.refs {
                        if reference.label.contains("patch") {
                            patch_ref_seen.insert(reference.uri.clone());
                        }
                    }
                    if event.kind == "patch.failed" {
                        let patch_file = payload_string(&event.payload, "patch_file")
                            .unwrap_or_else(|| "patch".to_owned());
                        blockers.push(format!("Patch failed: {patch_file}"));
                    }
                }
                "verification.started" => {
                    completed.push("Verification started".to_owned());
                }
                "verification.completed" => {
                    verification_blockers.clear();
                    let summary = verification_summary(&event.payload)
                        .unwrap_or_else(|| "completed".to_owned());
                    completed.push(format!("Verification {summary}"));
                    checks.push(format!("verification: {summary}"));
                    for check in verification_check_summaries(&event.payload) {
                        checks.push(format!("verification: {check}"));
                    }
                }
                "verification.failed" => {
                    let reason = payload_string(&event.payload, "reason")
                        .or_else(|| verification_summary(&event.payload))
                        .unwrap_or_else(|| "verification failed".to_owned());
                    checks.push(format!("verification: failed - {reason}"));
                    if reason.contains("requires evidence") {
                        verification_blockers.push(format!("Verification blocked: {reason}"));
                    } else {
                        verification_blockers.push(format!("Verification failed: {reason}"));
                    }
                }
                _ => {}
            }
        }
        blockers.extend(verification_blockers);

        for reference in repo_evidence {
            let ref_id = reference.ref_id;
            completed.push(format!("Recorded repo evidence: {}", reference.summary));
            evidence_ref_seen.insert(("repo_evidence".to_owned(), ref_id.clone()));
            if reference.kind == RepoEvidenceKind::RepoDiff {
                let payload = self.read_repo_evidence(&ref_id)?;
                match payload_string(&payload, "operation").as_deref() {
                    Some("patch_preview") => {
                        patch_ref_seen.insert(repo_evidence_uri(&ref_id));
                        collect_patch_files(&payload, &mut changed_file_seen);
                    }
                    Some("patch_apply") => {
                        patch_ref_seen.insert(repo_evidence_uri(&ref_id));
                        collect_patch_files(&payload, &mut changed_file_seen);
                        if let Some(result) = payload.get("result") {
                            let patch_file = payload_string(result, "patch_file")
                                .unwrap_or_else(|| "patch".to_owned());
                            let status = payload_string(result, "status").unwrap_or_default();
                            let requires_approval = result
                                .get("requires_approval")
                                .and_then(|value| value.as_bool())
                                .unwrap_or(false);
                            if requires_approval {
                                blockers
                                    .push(format!("Patch apply requires approval: {patch_file}"));
                            } else if status == "failed" {
                                blockers.push(format!("Patch failed: {patch_file}"));
                            }
                        }
                    }
                    Some("file_write") => {
                        collect_patch_files(&payload, &mut changed_file_seen);
                    }
                    _ => {}
                }
            }
        }
        if requested.is_none() {
            requested = plan_context_summary(plan_context.as_ref());
        }
        if let Some(summary) = plan_context_summary(plan_context.as_ref()) {
            checks.push(format!("plan_context: {summary}"));
        }
        for (kind, reference) in evidence_ref_seen {
            evidence_refs.push(coder_core::EvidenceRef { kind, reference });
        }

        let cancelled = metadata
            .as_ref()
            .map(|state| state.status == RunStatus::Cancelled)
            .unwrap_or(false)
            || events.iter().any(|event| event.kind == "run.cancelled");
        let status = if cancelled {
            ReportStatus::Cancelled
        } else if blockers.iter().any(|blocker| {
            blocker.contains("requires approval:") || blocker.contains("Verification blocked:")
        }) {
            ReportStatus::Blocked
        } else if !blockers.is_empty() {
            ReportStatus::Failed
        } else {
            ReportStatus::Completed
        };
        let mut report = FinalReport::with_status(status, "");
        report.changed_files = changed_file_seen.into_iter().collect();
        report.checks = checks;
        report.patch_refs = patch_ref_seen.into_iter().collect();
        report.blockers = blockers;
        report.evidence_refs = evidence_refs;
        report.refresh_planner_style_summary(requested.as_deref(), &completed);
        redact_final_report(&mut report);
        Ok(report)
    }

    pub fn write_repo_evidence(
        &self,
        run_id: &RunId,
        kind: RepoEvidenceKind,
        repo_root: impl Into<String>,
        scope_paths: Vec<String>,
        summary: impl Into<String>,
        payload: Value,
    ) -> Result<RepoEvidenceRef, StoreError> {
        let safe_run_id = safe_store_segment(run_id.as_str(), "run_id")?;
        let evidence_dir = self
            .root
            .join("runs")
            .join(&safe_run_id)
            .join("repo_evidence");
        fs::create_dir_all(&evidence_dir)?;

        let prefix = kind.prefix();
        let suffix = uuid::Uuid::new_v4().simple().to_string();
        let ref_id = format!("{prefix}:{suffix}");
        let payload_path = evidence_dir.join(format!("{prefix}-{suffix}.json"));
        let sanitized = sanitize_repo_evidence_payload(payload)?;
        let payload_text = serde_json::to_string_pretty(&sanitized)?;
        if payload_text.chars().count() > MAX_REPO_EVIDENCE_JSON_CHARS {
            return Err(StoreError::RepoEvidencePayloadTooLarge {
                max_chars: MAX_REPO_EVIDENCE_JSON_CHARS,
            });
        }

        fs::write(&payload_path, format!("{payload_text}\n"))?;
        let reference = RepoEvidenceRef {
            ref_id,
            kind,
            repo_root: repo_root.into(),
            scope_paths,
            summary: compact_string(&summary.into(), 500),
            payload_path: payload_path.display().to_string(),
            created_at: OffsetDateTime::now_utc(),
            token_estimate: token_estimate(&payload_text),
        };
        let index_path = evidence_dir.join("index.jsonl");
        let mut index = OpenOptions::new()
            .create(true)
            .append(true)
            .open(index_path)?;
        index.write_all(serde_json::to_string(&reference)?.as_bytes())?;
        index.write_all(b"\n")?;
        Ok(reference)
    }

    pub fn read_repo_evidence(&self, ref_id: &str) -> Result<Value, StoreError> {
        let safe_ref_id = safe_store_segment(ref_id, "ref_id")?;
        let runs_dir = self.root.join("runs");
        if !runs_dir.exists() {
            return Err(StoreError::RepoEvidenceNotFound(safe_ref_id));
        }
        for run_entry in fs::read_dir(runs_dir)? {
            let run_entry = run_entry?;
            let evidence_dir = run_entry.path().join("repo_evidence");
            let index_path = evidence_dir.join("index.jsonl");
            if !index_path.exists() {
                continue;
            }
            reject_file_over_read_limit(&index_path, MAX_DURABLE_READ_BYTES)?;
            let file = fs::File::open(&index_path)?;
            let reader = BufReader::new(file);
            for line in reader.lines() {
                let line = line?;
                if line.trim().is_empty() {
                    continue;
                }
                let record: RepoEvidenceRef = serde_json::from_str(&line)?;
                if record.ref_id != safe_ref_id {
                    continue;
                }
                let payload_path = PathBuf::from(&record.payload_path);
                ensure_path_under(&payload_path, &evidence_dir)?;
                let payload_text = read_text_with_limit(&payload_path, MAX_DURABLE_READ_BYTES)?;
                return Ok(serde_json::from_str(&payload_text)?);
            }
        }
        Err(StoreError::RepoEvidenceNotFound(safe_ref_id))
    }

    pub fn list_repo_evidence(&self, run_id: &RunId) -> Result<Vec<RepoEvidenceRef>, StoreError> {
        let evidence_dir = self.safe_run_dir(run_id)?.join("repo_evidence");
        let index_path = evidence_dir.join("index.jsonl");
        if !index_path.exists() {
            return Ok(Vec::new());
        }

        reject_file_over_read_limit(&index_path, MAX_DURABLE_READ_BYTES)?;
        let file = fs::File::open(index_path)?;
        let reader = BufReader::new(file);
        let mut records = Vec::new();
        for line in reader.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            let record: RepoEvidenceRef = serde_json::from_str(&line)?;
            ensure_path_under(&PathBuf::from(&record.payload_path), &evidence_dir)?;
            records.push(record);
        }
        Ok(records)
    }

    pub fn read_report(&self, run_id: &RunId) -> Result<Option<FinalReport>, StoreError> {
        read_json_optional(
            self.safe_run_dir(run_id)?
                .join("artifacts")
                .join("final-report.json"),
        )
    }

    pub fn write_artifact<T: Serialize>(
        &self,
        run_id: &RunId,
        name: &str,
        value: &T,
    ) -> Result<String, StoreError> {
        let safe_name = safe_file_name(name)?;
        let path = self
            .safe_run_dir(run_id)?
            .join("artifacts")
            .join(&safe_name);
        write_json(&path, value)?;
        Ok(format!(
            "artifact://runs/{}/artifacts/{safe_name}",
            run_id.as_str()
        ))
    }

    pub fn read_artifact_json(&self, run_id: &RunId, name: &str) -> Result<Value, StoreError> {
        let safe_name = safe_file_name(name)?;
        let path = self
            .safe_run_dir(run_id)?
            .join("artifacts")
            .join(&safe_name);
        if !path.exists() {
            return Err(StoreError::ArtifactNotFound {
                run_id: run_id.as_str().to_owned(),
                name: safe_name,
            });
        }
        let text = read_text_with_limit(&path, MAX_DURABLE_READ_BYTES)?;
        Ok(serde_json::from_str(&text)?)
    }

    pub fn write_checkpoint<T: Serialize>(
        &self,
        run_id: &RunId,
        name: &str,
        value: &T,
    ) -> Result<String, StoreError> {
        let safe_name = safe_file_name(name)?;
        let path = self
            .safe_run_dir(run_id)?
            .join("checkpoints")
            .join(&safe_name);
        write_json(&path, value)?;
        Ok(format!(
            "checkpoint://runs/{}/checkpoints/{safe_name}",
            run_id.as_str()
        ))
    }

    pub fn read_checkpoint_json(&self, run_id: &RunId, name: &str) -> Result<Value, StoreError> {
        let safe_name = safe_file_name(name)?;
        let path = self
            .safe_run_dir(run_id)?
            .join("checkpoints")
            .join(&safe_name);
        if !path.exists() {
            return Err(StoreError::CheckpointNotFound {
                run_id: run_id.as_str().to_owned(),
                name: safe_name,
            });
        }
        let text = read_text_with_limit(&path, MAX_DURABLE_READ_BYTES)?;
        Ok(serde_json::from_str(&text)?)
    }

    pub fn list_checkpoints(&self, run_id: &RunId) -> Result<Vec<RunCheckpointRef>, StoreError> {
        let checkpoints_dir = self.safe_run_dir(run_id)?.join("checkpoints");
        if !checkpoints_dir.exists() {
            return Ok(Vec::new());
        }
        let mut checkpoints = Vec::new();
        for entry in fs::read_dir(checkpoints_dir)? {
            let entry = entry?;
            if !entry.file_type()?.is_file() {
                continue;
            }
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if safe_file_name(&name).is_err() {
                continue;
            }
            checkpoints.push(RunCheckpointRef {
                name: name.clone(),
                checkpoint_ref: format!("checkpoint://runs/{}/checkpoints/{name}", run_id.as_str()),
            });
        }
        checkpoints.sort_by(|left, right| left.name.cmp(&right.name));
        Ok(checkpoints)
    }

    pub fn write_blob(&self, content: &[u8]) -> Result<String, StoreError> {
        let digest = Sha256::digest(content);
        let hex = format!("{digest:x}");
        let path = self.root.join("blobs").join(&hex[..2]).join(&hex);
        ensure_parent(&path)?;
        if !path.exists() {
            fs::write(path, content)?;
        }
        Ok(format!("blob://sha256/{hex}"))
    }

    pub fn read_blob_sha256(&self, digest: &str) -> Result<Vec<u8>, StoreError> {
        let safe_digest = safe_sha256_digest(digest)?;
        let path = self
            .root
            .join("blobs")
            .join(&safe_digest[..2])
            .join(&safe_digest);
        if !path.exists() {
            return Err(StoreError::BlobNotFound(safe_digest));
        }
        reject_file_over_read_limit(&path, MAX_DURABLE_READ_BYTES)?;
        Ok(fs::read(path)?)
    }

    pub fn write_large_text_ref(&self, content: &str) -> Result<LargePayloadRef, StoreError> {
        self.write_large_text_ref_with_limit(content, DEFAULT_LARGE_PAYLOAD_PREVIEW_LIMIT)
    }

    pub fn write_large_text_ref_with_limit(
        &self,
        content: &str,
        preview_limit: usize,
    ) -> Result<LargePayloadRef, StoreError> {
        let blob_ref = self.write_blob(content.as_bytes())?;
        Ok(LargePayloadRef::from_text(content, blob_ref, preview_limit))
    }

    pub fn run_dir(&self, run_id: &RunId) -> PathBuf {
        self.root.join("runs").join(run_id.as_str())
    }

    pub fn cache_bucket_usage(
        &self,
        relative_dir: impl AsRef<Path>,
    ) -> Result<CacheBucketUsage, StoreError> {
        let relative_dir = relative_dir.as_ref();
        ensure_safe_store_relative_path(relative_dir)?;
        cache_bucket_usage_at(&self.root.join(relative_dir))
    }

    pub fn clear_disposable_caches(&self) -> Result<CacheCleanupSummary, StoreError> {
        self.ensure_local_layout()?;
        let mut summary = CacheCleanupSummary::default();
        for relative_dir in DISPOSABLE_CACHE_DIRS {
            let relative_path = Path::new(relative_dir);
            ensure_safe_store_relative_path(relative_path)?;
            let path = self.root.join(relative_path);
            let usage = cache_bucket_usage_at(&path)?;
            remove_path_if_exists(&path)?;
            fs::create_dir_all(&path)?;
            summary.directories.push((*relative_dir).to_owned());
            summary.entries += usage.entries;
            summary.bytes += usage.bytes;
            summary.entry_scan_limit = summary.entry_scan_limit.max(usage.entry_scan_limit);
            summary.truncated |= usage.truncated;
        }
        Ok(summary)
    }

    pub fn read_compaction_circuit_state(
        &self,
        scope_id: &str,
    ) -> Result<Option<CompactionCircuitState>, StoreError> {
        let safe_scope_id = safe_file_name(scope_id)?;
        read_json_optional(
            self.root
                .join(COMPACTION_STATE_DIR)
                .join(format!("{safe_scope_id}.json")),
        )
    }

    pub fn record_compaction_circuit_outcome(
        &self,
        scope_id: &str,
        max_consecutive_failures: u8,
        succeeded: bool,
    ) -> Result<CompactionCircuitState, StoreError> {
        let safe_scope_id = safe_file_name(scope_id)?;
        let path = self
            .root
            .join(COMPACTION_STATE_DIR)
            .join(format!("{safe_scope_id}.json"));
        let previous = read_json_optional::<CompactionCircuitState>(&path)?;
        let previous_failures = previous
            .as_ref()
            .map(|state| state.consecutive_failures)
            .unwrap_or(0);
        let consecutive_failures = if succeeded {
            0
        } else {
            previous_failures.saturating_add(1)
        };
        let state = CompactionCircuitState {
            scope_id: safe_scope_id,
            max_consecutive_failures,
            consecutive_failures,
            circuit_breaker_open: max_consecutive_failures > 0
                && consecutive_failures >= max_consecutive_failures,
            updated_at: OffsetDateTime::now_utc(),
        };
        write_json(&path, &state)?;
        Ok(state)
    }

    pub fn read_goal_state_json(&self, session_id: &str) -> Result<Option<Value>, StoreError> {
        let safe_session_id = safe_file_name(session_id)?;
        read_json_optional(
            self.root
                .join(GOAL_STATE_DIR)
                .join(format!("{safe_session_id}.json")),
        )
    }

    pub fn write_goal_state_json(
        &self,
        session_id: &str,
        value: &Value,
    ) -> Result<String, StoreError> {
        reject_session_record_secret_like_json(value)?;
        let safe_session_id = safe_file_name(session_id)?;
        let path = self
            .root
            .join(GOAL_STATE_DIR)
            .join(format!("{safe_session_id}.json"));
        write_json(&path, value)?;
        Ok(format!("goal://sessions/{safe_session_id}.json"))
    }

    pub fn delete_goal_state(&self, session_id: &str) -> Result<bool, StoreError> {
        let safe_session_id = safe_file_name(session_id)?;
        let path = self
            .root
            .join(GOAL_STATE_DIR)
            .join(format!("{safe_session_id}.json"));
        if !path.exists() {
            return Ok(false);
        }
        fs::remove_file(path)?;
        Ok(true)
    }

    fn safe_run_dir(&self, run_id: &RunId) -> Result<PathBuf, StoreError> {
        let safe_run_id = safe_store_segment(run_id.as_str(), "run_id")?;
        Ok(self.root.join("runs").join(safe_run_id))
    }

    fn run_content_replacements_path(&self, run_id: &RunId) -> Result<PathBuf, StoreError> {
        Ok(self
            .safe_run_dir(run_id)?
            .join("content-replacements.jsonl"))
    }

    fn subagent_dir(&self, run_id: &RunId) -> Result<PathBuf, StoreError> {
        Ok(self.safe_run_dir(run_id)?.join("subagents"))
    }

    fn subagent_transcript_path(
        &self,
        run_id: &RunId,
        safe_agent_id: &str,
    ) -> Result<PathBuf, StoreError> {
        Ok(self
            .subagent_dir(run_id)?
            .join(format!("agent-{safe_agent_id}.jsonl")))
    }

    fn subagent_metadata_path(
        &self,
        run_id: &RunId,
        safe_agent_id: &str,
    ) -> Result<PathBuf, StoreError> {
        Ok(self
            .subagent_dir(run_id)?
            .join(format!("agent-{safe_agent_id}.meta.json")))
    }

    fn subagent_background_task_record_path(&self, safe_task_id: &str) -> PathBuf {
        self.root
            .join("background-tasks")
            .join("subagents")
            .join(format!("{safe_task_id}.json"))
    }

    fn command_background_task_record_path(&self, safe_task_id: &str) -> PathBuf {
        self.root
            .join("background-tasks")
            .join("commands")
            .join(format!("{safe_task_id}.json"))
    }

    fn command_background_output_path(&self, safe_task_id: &str) -> PathBuf {
        self.root
            .join("background-tasks")
            .join("commands")
            .join(format!("{safe_task_id}.output"))
    }

    fn permission_settings_path(&self, safe_destination: &str) -> PathBuf {
        self.root
            .join("settings")
            .join("permissions")
            .join(format!("{safe_destination}.json"))
    }

    pub fn repo_evidence_count(&self, run_id: &RunId) -> Result<usize, StoreError> {
        Ok(self.list_repo_evidence(run_id)?.len())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalStoreLayout {
    pub root: PathBuf,
    pub sessions: PathBuf,
    pub runs: PathBuf,
    pub background_tasks: PathBuf,
    pub timeline: PathBuf,
    pub blobs: PathBuf,
    pub artifacts: PathBuf,
    pub settings: PathBuf,
    pub checkpoints: PathBuf,
    pub changesets: PathBuf,
    pub repo_index: PathBuf,
    pub plugin_cache: PathBuf,
    pub skill_cache: PathBuf,
    pub logs: PathBuf,
    pub tmp: PathBuf,
}

impl LocalStoreLayout {
    fn new(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
            sessions: root.join("sessions"),
            runs: root.join("runs"),
            background_tasks: root.join("background-tasks"),
            timeline: root.join("timeline"),
            blobs: root.join("blobs"),
            artifacts: root.join("artifacts"),
            settings: root.join("settings"),
            checkpoints: root.join("checkpoints"),
            changesets: root.join("changesets"),
            repo_index: root.join("repo-index"),
            plugin_cache: root.join("plugin-cache"),
            skill_cache: root.join("skill-cache"),
            logs: root.join("logs"),
            tmp: root.join("tmp"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionJsonlRecord {
    pub session_id: String,
    pub sequence: u64,
    pub kind: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(default)]
    pub payload: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SubagentTranscriptRecord {
    pub run_id: String,
    pub agent_id: String,
    pub sequence: u64,
    #[serde(default)]
    pub parent_sequence: Option<u64>,
    pub kind: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(default)]
    pub payload: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContentReplacementRecord {
    pub kind: String,
    #[serde(rename = "toolUseId")]
    pub tool_use_id: String,
    pub replacement: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RunContentReplacementEntry {
    pub run_id: String,
    pub sequence: u64,
    pub kind: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(default)]
    pub replacements: Vec<ContentReplacementRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SubagentMetadata {
    pub agent_type: String,
    pub parent_agent_id: String,
    pub parent_harness_id: String,
    pub invocation_kind: String,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub terminal_record_kind: Option<String>,
    #[serde(default)]
    pub last_sequence: Option<u64>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub worktree_path: Option<String>,
    #[serde(default)]
    pub transcript_ref: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubagentBackgroundTaskRecord {
    pub task_id: String,
    pub run_id: String,
    pub agent_id: String,
    pub status: String,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
    pub metadata_ref: String,
    pub transcript_ref: String,
    #[serde(default)]
    pub report: Option<FinalReport>,
    #[serde(default)]
    pub event_count: usize,
    #[serde(default)]
    pub events_truncated: bool,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandBackgroundTaskRecord {
    pub task_id: String,
    #[serde(default)]
    pub run_id: Option<String>,
    pub repo_root: String,
    pub cwd: String,
    pub argv: Vec<String>,
    pub command: String,
    pub approval_key: String,
    pub policy: Value,
    pub status: String,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
    pub output_ref: String,
    #[serde(default)]
    pub output_bytes: u64,
    #[serde(default)]
    pub output_truncated: bool,
    pub max_output_bytes: usize,
    #[serde(default)]
    pub result: Option<Value>,
    #[serde(default)]
    pub evidence_ref: Option<RepoEvidenceRef>,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandBackgroundOutputTail {
    pub output: String,
    pub bytes: u64,
    pub truncated: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DurableJsonlPageOptions {
    pub after_sequence: Option<u64>,
    pub limit: usize,
    pub tail: bool,
}

impl DurableJsonlPageOptions {
    pub fn new(limit: usize) -> Result<Self, StoreError> {
        Self::with_after_sequence(None, limit)
    }

    pub fn with_after_sequence(
        after_sequence: Option<u64>,
        limit: usize,
    ) -> Result<Self, StoreError> {
        if limit == 0 || limit > MAX_DURABLE_JSONL_PAGE_LIMIT {
            return Err(StoreError::DurableJsonlPageLimitOutOfRange {
                limit,
                max: MAX_DURABLE_JSONL_PAGE_LIMIT,
            });
        }
        Ok(Self {
            after_sequence,
            limit,
            tail: false,
        })
    }

    pub fn tail(limit: usize) -> Result<Self, StoreError> {
        if limit == 0 || limit > MAX_DURABLE_JSONL_PAGE_LIMIT {
            return Err(StoreError::DurableJsonlPageLimitOutOfRange {
                limit,
                max: MAX_DURABLE_JSONL_PAGE_LIMIT,
            });
        }
        Ok(Self {
            after_sequence: None,
            limit,
            tail: true,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DurableJsonlPage<T> {
    pub records: Vec<T>,
    pub total_records: usize,
    pub matching_records: usize,
    pub returned_records: usize,
    pub truncated: bool,
    pub next_after_sequence: Option<u64>,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct CacheBucketUsage {
    pub entries: usize,
    pub bytes: u64,
    pub scanned_entries: usize,
    pub entry_scan_limit: usize,
    pub truncated: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct CacheCleanupSummary {
    pub directories: Vec<String>,
    pub entries: usize,
    pub bytes: u64,
    pub entry_scan_limit: usize,
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CompactionCircuitState {
    pub scope_id: String,
    pub max_consecutive_failures: u8,
    pub consecutive_failures: u8,
    pub circuit_breaker_open: bool,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepoEvidenceKind {
    RepoFileList,
    RepoTextSearch,
    RepoRead,
    RepoTest,
    RepoDiff,
}

impl RepoEvidenceKind {
    fn prefix(self) -> &'static str {
        match self {
            Self::RepoFileList => "repo-file-list",
            Self::RepoTextSearch => "repo-text-search",
            Self::RepoRead => "repo-read",
            Self::RepoTest => "repo-test",
            Self::RepoDiff => "repo-diff",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoEvidenceRef {
    pub ref_id: String,
    pub kind: RepoEvidenceKind,
    pub repo_root: String,
    #[serde(default)]
    pub scope_paths: Vec<String>,
    pub summary: String,
    pub payload_path: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    pub token_estimate: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredRunSummary {
    pub run_id: String,
    pub metadata: Option<RunState>,
    pub event_count: usize,
    pub has_report: bool,
    pub repo_evidence_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunCheckpointRef {
    pub name: String,
    pub checkpoint_ref: String,
}

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("run not found: {0}")]
    RunNotFound(String),
    #[error("invalid file name: {0}")]
    InvalidFileName(String),
    #[error("invalid store segment for {label}: {value}")]
    InvalidStoreSegment { label: String, value: String },
    #[error("repo evidence payload contains secret-like text")]
    RepoEvidenceSecretLikeText,
    #[error("session JSONL record contains secret-like text")]
    SessionRecordSecretLikeText,
    #[error("repo evidence payload is over limit {max_chars} chars")]
    RepoEvidencePayloadTooLarge { max_chars: usize },
    #[error("durable read over limit: {path} is {bytes} bytes, max {max_bytes} bytes")]
    DurableReadLimitExceeded {
        path: String,
        bytes: u64,
        max_bytes: u64,
    },
    #[error("durable JSONL page limit {limit} is out of range, max {max}")]
    DurableJsonlPageLimitOutOfRange { limit: usize, max: usize },
    #[error("repo evidence not found: {0}")]
    RepoEvidenceNotFound(String),
    #[error("repo evidence payload path escaped repo_evidence directory: {0}")]
    RepoEvidencePathEscape(String),
    #[error("artifact not found: runs/{run_id}/artifacts/{name}")]
    ArtifactNotFound { run_id: String, name: String },
    #[error("checkpoint not found: runs/{run_id}/checkpoints/{name}")]
    CheckpointNotFound { run_id: String, name: String },
    #[error("invalid blob sha256 digest: {0}")]
    InvalidBlobDigest(String),
    #[error("blob not found: sha256:{0}")]
    BlobNotFound(String),
}

fn write_json(path: impl AsRef<Path>, value: &impl Serialize) -> Result<(), StoreError> {
    let path = path.as_ref();
    ensure_parent(path)?;
    fs::write(path, serde_json::to_string_pretty(value)?)?;
    Ok(())
}

fn reject_file_over_read_limit(path: &Path, max_bytes: u64) -> Result<(), StoreError> {
    let bytes = fs::metadata(path)?.len();
    if bytes > max_bytes {
        return Err(StoreError::DurableReadLimitExceeded {
            path: path.display().to_string(),
            bytes,
            max_bytes,
        });
    }
    Ok(())
}

fn read_text_with_limit(path: &Path, max_bytes: u64) -> Result<String, StoreError> {
    reject_file_over_read_limit(path, max_bytes)?;
    Ok(fs::read_to_string(path)?)
}

fn read_jsonl_records<T: DeserializeOwned>(path: impl AsRef<Path>) -> Result<Vec<T>, StoreError> {
    let path = path.as_ref();
    if !path.exists() {
        return Ok(Vec::new());
    }
    reject_file_over_read_limit(path, MAX_DURABLE_READ_BYTES)?;
    let file = fs::File::open(path)?;
    let reader = BufReader::new(file);
    let mut records = Vec::new();
    for line in reader.lines() {
        let line = line?;
        if !line.trim().is_empty() {
            records.push(serde_json::from_str(&line)?);
        }
    }
    Ok(records)
}

fn read_jsonl_page<T, F>(
    path: impl AsRef<Path>,
    options: DurableJsonlPageOptions,
    sequence_of: F,
) -> Result<DurableJsonlPage<T>, StoreError>
where
    T: DeserializeOwned,
    F: Fn(&T) -> u64,
{
    let path = path.as_ref();
    if !path.exists() {
        return Ok(DurableJsonlPage {
            records: Vec::new(),
            total_records: 0,
            matching_records: 0,
            returned_records: 0,
            truncated: false,
            next_after_sequence: None,
        });
    }
    reject_file_over_read_limit(path, MAX_DURABLE_READ_BYTES)?;
    let file = fs::File::open(path)?;
    let reader = BufReader::new(file);
    let mut records = Vec::new();
    let mut tail_records = VecDeque::new();
    let mut total_records = 0;
    let mut matching_records = 0;

    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        total_records += 1;
        let record = serde_json::from_str::<T>(&line)?;
        if options
            .after_sequence
            .map(|after| sequence_of(&record) <= after)
            .unwrap_or(false)
        {
            continue;
        }
        matching_records += 1;
        if options.tail {
            if tail_records.len() == options.limit {
                tail_records.pop_front();
            }
            tail_records.push_back(record);
        } else if records.len() < options.limit {
            records.push(record);
        }
    }

    if options.tail {
        records = tail_records.into_iter().collect();
    }
    let returned_records = records.len();
    let next_after_sequence = records.last().map(sequence_of);
    Ok(DurableJsonlPage {
        records,
        total_records,
        matching_records,
        returned_records,
        truncated: matching_records > returned_records,
        next_after_sequence,
    })
}

fn count_jsonl_records(path: impl AsRef<Path>) -> Result<usize, StoreError> {
    let path = path.as_ref();
    if !path.exists() {
        return Ok(0);
    }
    reject_file_over_read_limit(path, MAX_DURABLE_READ_BYTES)?;
    let file = fs::File::open(path)?;
    let reader = BufReader::new(file);
    let mut count = 0;
    for line in reader.lines() {
        if !line?.trim().is_empty() {
            count += 1;
        }
    }
    Ok(count)
}

fn read_file_tail_bytes(path: &Path, max_bytes: usize) -> Result<(Vec<u8>, u64, bool), StoreError> {
    if !path.exists() {
        return Ok((Vec::new(), 0, false));
    }
    let max_bytes = max_bytes.clamp(1, MAX_DURABLE_READ_BYTES as usize);
    let metadata = fs::metadata(path)?;
    let total_bytes = metadata.len();
    let offset = total_bytes.saturating_sub(max_bytes as u64);
    let mut file = fs::File::open(path)?;
    file.seek(SeekFrom::Start(offset))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    Ok((bytes, total_bytes, offset > 0))
}

fn redact_final_report(report: &mut FinalReport) {
    report.summary = redact_secret_text(&report.summary);
    redact_strings(&mut report.changed_files);
    redact_strings(&mut report.checks);
    redact_strings(&mut report.patch_refs);
    redact_strings(&mut report.artifact_refs);
    redact_strings(&mut report.blockers);
    redact_strings(&mut report.next_steps);
    for evidence in &mut report.evidence_refs {
        evidence.kind = redact_secret_text(&evidence.kind);
        evidence.reference = redact_secret_text(&evidence.reference);
    }
}

fn redact_strings(items: &mut [String]) {
    for item in items {
        *item = redact_secret_text(item);
    }
}

fn payload_string(payload: &Value, key: &str) -> Option<String> {
    payload
        .get(key)
        .and_then(|value| value.as_str())
        .map(str::to_owned)
}

fn verification_summary(payload: &Value) -> Option<String> {
    if let Some(summary) = payload_string(payload, "summary") {
        return Some(summary);
    }
    let status = payload_string(payload, "status").filter(|value| !value.trim().is_empty())?;
    let total_refs = payload
        .pointer("/evidence/total_refs")
        .and_then(|value| value.as_u64());
    Some(match total_refs {
        Some(total_refs) => format!("{status} with {total_refs} evidence ref(s)"),
        None => status,
    })
}

fn verification_check_summaries(payload: &Value) -> Vec<String> {
    payload
        .get("checks")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|check| {
            let name = payload_string(check, "name")?;
            let status = payload_string(check, "status").unwrap_or_else(|| "completed".to_owned());
            let detail = payload_string(check, "detail").unwrap_or_default();
            if detail.trim().is_empty() {
                Some(format!("{name} {status}"))
            } else {
                Some(format!("{name} {status} - {detail}"))
            }
        })
        .collect()
}

fn collect_patch_files(payload: &Value, files: &mut BTreeSet<String>) {
    if let Some(items) = payload
        .get("files")
        .or_else(|| payload.pointer("/preview/files"))
        .or_else(|| payload.pointer("/result/preview/files"))
        .and_then(|value| value.as_array())
    {
        for item in items {
            let path = payload_string(item, "new_path")
                .or_else(|| payload_string(item, "old_path"))
                .or_else(|| payload_string(item, "path"));
            if let Some(path) = path.filter(|path| !path.trim().is_empty()) {
                files.insert(path);
            }
        }
    }
}

fn collect_patch_ref(payload: &Value, refs: &mut BTreeSet<String>) {
    if let Some(reference) = payload_string(payload, "evidence_ref") {
        refs.insert(repo_evidence_uri(&reference));
    }
}

fn repo_evidence_uri(ref_id: &str) -> String {
    if ref_id.contains("://") {
        ref_id.to_owned()
    } else {
        format!("repo-evidence://{ref_id}")
    }
}

fn plan_context_summary(plan_context: Option<&Value>) -> Option<String> {
    let plan_context = plan_context?;
    let summary = plan_context
        .get("plan_draft")
        .and_then(|plan| plan.get("goal"))
        .and_then(Value::as_str)
        .or_else(|| {
            plan_context
                .get("original_user_request")
                .and_then(Value::as_str)
        })
        .or_else(|| {
            plan_context
                .get("planner_conversation_summary")
                .and_then(Value::as_str)
        })?
        .trim();
    if summary.is_empty() {
        None
    } else {
        Some(summary.chars().take(240).collect())
    }
}

fn read_json_optional<T: DeserializeOwned>(
    path: impl AsRef<Path>,
) -> Result<Option<T>, StoreError> {
    let path = path.as_ref();
    if !path.exists() {
        return Ok(None);
    }
    let text = read_text_with_limit(path, MAX_DURABLE_READ_BYTES)?;
    Ok(Some(serde_json::from_str(&text)?))
}

fn ensure_parent(path: &Path) -> Result<(), StoreError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    Ok(())
}

fn remove_path_if_exists(path: &Path) -> Result<(), StoreError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(StoreError::Io(error)),
    };
    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        fs::remove_dir_all(path)?;
    } else {
        fs::remove_file(path)?;
    }
    Ok(())
}

fn safe_file_name(value: &str) -> Result<String, StoreError> {
    if value.is_empty()
        || value.contains('/')
        || value.contains('\\')
        || value == "."
        || value == ".."
        || !value
            .chars()
            .all(|item| item.is_ascii_alphanumeric() || matches!(item, '_' | '.' | '-'))
    {
        return Err(StoreError::InvalidFileName(value.to_owned()));
    }
    Ok(value.to_owned())
}

fn safe_store_segment(value: &str, label: &str) -> Result<String, StoreError> {
    if value.is_empty()
        || value == "."
        || value == ".."
        || value.contains('/')
        || value.contains('\\')
        || !value
            .chars()
            .all(|item| item.is_ascii_alphanumeric() || matches!(item, '_' | '.' | ':' | '-'))
    {
        return Err(StoreError::InvalidStoreSegment {
            label: label.to_owned(),
            value: value.to_owned(),
        });
    }
    Ok(value.to_owned())
}

fn ensure_safe_store_relative_path(value: &Path) -> Result<(), StoreError> {
    if value.as_os_str().is_empty() || value.is_absolute() {
        return Err(StoreError::InvalidStoreSegment {
            label: "relative_dir".to_owned(),
            value: value.display().to_string(),
        });
    }
    for component in value.components() {
        match component {
            Component::Normal(segment) => {
                let Some(segment) = segment.to_str() else {
                    return Err(StoreError::InvalidStoreSegment {
                        label: "relative_dir".to_owned(),
                        value: value.display().to_string(),
                    });
                };
                safe_file_name(segment)?;
            }
            _ => {
                return Err(StoreError::InvalidStoreSegment {
                    label: "relative_dir".to_owned(),
                    value: value.display().to_string(),
                });
            }
        }
    }
    Ok(())
}

fn safe_sha256_digest(value: &str) -> Result<String, StoreError> {
    if value.len() != 64 || !value.chars().all(|item| item.is_ascii_hexdigit()) {
        return Err(StoreError::InvalidBlobDigest(value.to_owned()));
    }
    Ok(value.to_ascii_lowercase())
}

fn sanitize_repo_evidence_payload(value: Value) -> Result<Value, StoreError> {
    match value {
        Value::Object(object) => object
            .into_iter()
            .map(|(key, value)| Ok((key, sanitize_repo_evidence_payload(value)?)))
            .collect::<Result<Map<String, Value>, StoreError>>()
            .map(Value::Object),
        Value::Array(items) => {
            let omitted_items = items.len().saturating_sub(MAX_REPO_EVIDENCE_LIST_ITEMS);
            let mut sanitized = items
                .into_iter()
                .take(MAX_REPO_EVIDENCE_LIST_ITEMS)
                .map(sanitize_repo_evidence_payload)
                .collect::<Result<Vec<_>, _>>()?;
            if omitted_items > 0 {
                sanitized.push(serde_json::json!({
                    "truncated": true,
                    "omitted_items": omitted_items
                }));
            }
            Ok(Value::Array(sanitized))
        }
        Value::String(text) => {
            reject_secret_like_text(&text)?;
            Ok(Value::String(compact_string(
                &text,
                MAX_REPO_EVIDENCE_STRING_CHARS,
            )))
        }
        other => Ok(other),
    }
}

fn reject_secret_like_text(value: &str) -> Result<(), StoreError> {
    let lowered = value.to_ascii_lowercase();
    if REPO_EVIDENCE_SECRET_MARKERS
        .iter()
        .any(|marker| lowered.contains(marker))
    {
        return Err(StoreError::RepoEvidenceSecretLikeText);
    }
    Ok(())
}

fn reject_session_record_secret_like_text(value: &str) -> Result<(), StoreError> {
    let lowered = value.to_ascii_lowercase();
    if REPO_EVIDENCE_SECRET_MARKERS
        .iter()
        .any(|marker| lowered.contains(marker))
    {
        return Err(StoreError::SessionRecordSecretLikeText);
    }
    Ok(())
}

fn reject_session_record_secret_like_json(value: &Value) -> Result<(), StoreError> {
    match value {
        Value::Object(object) => {
            for (key, value) in object {
                reject_session_record_secret_like_text(key)?;
                reject_session_record_secret_like_json(value)?;
            }
            Ok(())
        }
        Value::Array(items) => {
            for item in items {
                reject_session_record_secret_like_json(item)?;
            }
            Ok(())
        }
        Value::String(text) => reject_session_record_secret_like_text(text),
        _ => Ok(()),
    }
}

fn cache_bucket_usage_at(path: &Path) -> Result<CacheBucketUsage, StoreError> {
    cache_bucket_usage_at_with_limit(path, MAX_CACHE_USAGE_SCAN_ENTRIES)
}

fn cache_bucket_usage_at_with_limit(
    path: &Path,
    entry_scan_limit: usize,
) -> Result<CacheBucketUsage, StoreError> {
    let mut usage = CacheBucketUsage {
        entry_scan_limit: entry_scan_limit.max(1),
        ..CacheBucketUsage::default()
    };
    accumulate_cache_bucket_usage(path, &mut usage, false)?;
    Ok(usage)
}

fn accumulate_cache_bucket_usage(
    path: &Path,
    usage: &mut CacheBucketUsage,
    count_path: bool,
) -> Result<(), StoreError> {
    if usage.truncated {
        return Ok(());
    }
    if count_path && !record_cache_usage_scan_entry(usage) {
        return Ok(());
    }
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(StoreError::Io(error)),
    };
    let file_type = metadata.file_type();
    if file_type.is_file() {
        usage.entries += 1;
        usage.bytes += metadata.len();
    } else if file_type.is_dir() {
        for entry in fs::read_dir(path)? {
            accumulate_cache_bucket_usage(&entry?.path(), usage, true)?;
            if usage.truncated {
                break;
            }
        }
    } else if file_type.is_symlink() {
        usage.entries += 1;
        usage.bytes += metadata.len();
    }
    Ok(())
}

fn record_cache_usage_scan_entry(usage: &mut CacheBucketUsage) -> bool {
    if usage.scanned_entries >= usage.entry_scan_limit {
        usage.truncated = true;
        return false;
    }
    usage.scanned_entries += 1;
    true
}

fn compact_string(value: &str, limit: usize) -> String {
    let mut chars = value.chars();
    let mut compacted = chars.by_ref().take(limit).collect::<String>();
    if chars.next().is_some() {
        compacted.truncate(compacted.trim_end().len());
        compacted.push_str("...");
    }
    compacted
}

fn token_estimate(text: &str) -> usize {
    text.chars().count().div_ceil(4).max(1)
}

fn ensure_path_under(path: &Path, root: &Path) -> Result<(), StoreError> {
    let canonical_path = path.canonicalize()?;
    let canonical_root = root.canonicalize()?;
    if !canonical_path.starts_with(&canonical_root) {
        return Err(StoreError::RepoEvidencePathEscape(
            path.display().to_string(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use coder_events::CoderEvent;
    use serde_json::json;

    use super::*;

    #[test]
    fn event_log_roundtrips() {
        let root = temp_root();
        let store = RunStore::new(&root);
        let run_id = RunId::from_string("run_test");
        let event = CoderEvent::new(
            run_id.clone(),
            1,
            "run.started",
            json!({"workflow_id": "wf"}),
        );

        store.append_event(&run_id, &event).unwrap();
        let events = store.read_events(&run_id).unwrap();

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, "run.started");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn event_log_page_supports_after_sequence_and_tail_without_full_vec() {
        let root = temp_root();
        let store = RunStore::new(&root);
        let run_id = RunId::from_string("run_page");
        for sequence in 1..=5 {
            store
                .append_event(
                    &run_id,
                    &CoderEvent::new(
                        run_id.clone(),
                        sequence,
                        format!("event.{sequence}"),
                        json!({"sequence": sequence}),
                    ),
                )
                .unwrap();
        }

        let page = store
            .read_events_page(
                &run_id,
                DurableJsonlPageOptions::with_after_sequence(Some(2), 2).unwrap(),
            )
            .unwrap();
        assert_eq!(page.total_records, 5);
        assert_eq!(page.matching_records, 3);
        assert_eq!(page.returned_records, 2);
        assert_eq!(page.next_after_sequence, Some(4));
        assert_eq!(
            page.records
                .iter()
                .map(|event| event.sequence)
                .collect::<Vec<_>>(),
            vec![3, 4]
        );
        assert!(page.truncated);

        let tail = store
            .read_events_page(&run_id, DurableJsonlPageOptions::tail(2).unwrap())
            .unwrap();
        assert_eq!(
            tail.records
                .iter()
                .map(|event| event.sequence)
                .collect::<Vec<_>>(),
            vec![4, 5]
        );
        assert_eq!(tail.next_after_sequence, Some(5));
        assert!(tail.truncated);
        assert_eq!(store.event_count(&run_id).unwrap(), 5);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn durable_jsonl_page_limit_is_bounded() {
        assert!(matches!(
            DurableJsonlPageOptions::new(0).unwrap_err(),
            StoreError::DurableJsonlPageLimitOutOfRange { .. }
        ));
        assert!(matches!(
            DurableJsonlPageOptions::tail(MAX_DURABLE_JSONL_PAGE_LIMIT + 1).unwrap_err(),
            StoreError::DurableJsonlPageLimitOutOfRange { .. }
        ));
    }

    #[test]
    fn append_event_redacts_payload_before_jsonl_write() {
        let root = temp_root();
        let store = RunStore::new(&root);
        let run_id = RunId::from_string("run_test");
        let event = CoderEvent {
            event_id: "evt_manual".to_owned(),
            run_id: run_id.clone(),
            sequence: 1,
            timestamp: OffsetDateTime::now_utc(),
            kind: "run.started".to_owned(),
            payload: json!({"task": "Use sk-live-1234567890"}),
            refs: Vec::new(),
        };

        store.append_event(&run_id, &event).unwrap();

        let events = store.read_events(&run_id).unwrap();
        assert_eq!(events[0].payload["task"], "[REDACTED]");
        let text =
            fs::read_to_string(root.join("runs").join("run_test").join("events.jsonl")).unwrap();
        assert!(!text.contains("sk-live-1234567890"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn local_layout_creates_required_directories() {
        let root = temp_root();
        let store = RunStore::new(&root);

        let layout = store.ensure_local_layout().unwrap();

        assert_eq!(layout.root, root);
        for dir in LOCAL_STORE_DIRS {
            assert!(root.join(dir).is_dir(), "{dir} should exist");
        }
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn session_records_append_jsonl_and_reject_secret_like_payloads() {
        let root = temp_root();
        let store = RunStore::new(&root);

        store
            .append_session_record(
                "session_1",
                1,
                "session.created",
                json!({"workflow_id": "planner-led", "mode": "discuss"}),
            )
            .unwrap();
        store
            .append_session_record(
                "session_1",
                2,
                "session.turn.completed",
                json!({"turn_count": 2, "ready": false}),
            )
            .unwrap();

        let records = store.read_session_records("session_1").unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].sequence, 1);
        assert_eq!(records[1].kind, "session.turn.completed");
        let text = fs::read_to_string(root.join("sessions").join("session_1.jsonl")).unwrap();
        assert_eq!(text.lines().count(), 2);

        let error = store
            .append_session_record(
                "session_1",
                3,
                "session.turn.completed",
                json!({"api_key": "redacted"}),
            )
            .unwrap_err();
        assert!(matches!(error, StoreError::SessionRecordSecretLikeText));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn session_record_next_sequence_uses_persistent_sidecar() {
        let root = temp_root();
        let store = RunStore::new(&root);

        let first = store
            .append_session_record_next(
                "session_1",
                "session.created",
                json!({"workflow_id": "planner-led"}),
            )
            .unwrap();
        let second = store
            .append_session_record_next(
                "session_1",
                "session.turn.completed",
                json!({"turn_count": 2}),
            )
            .unwrap();

        assert_eq!(first, 1);
        assert_eq!(second, 2);
        let records = store.read_session_records("session_1").unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].sequence, 1);
        assert_eq!(records[1].sequence, 2);
        let sequence_text =
            fs::read_to_string(root.join("sessions").join("session_1.seq")).unwrap();
        assert_eq!(sequence_text.trim(), "2");
        let error = store
            .append_session_record_next(
                "session_1",
                "session.turn.completed",
                json!({"api_key": "redacted"}),
            )
            .unwrap_err();
        assert!(matches!(error, StoreError::SessionRecordSecretLikeText));
        let sequence_text =
            fs::read_to_string(root.join("sessions").join("session_1.seq")).unwrap();
        assert_eq!(sequence_text.trim(), "2");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn session_records_page_reads_incremental_records() {
        let root = temp_root();
        let store = RunStore::new(&root);
        for index in 1..=4 {
            store
                .append_session_record(
                    "session_page",
                    index,
                    "session.turn.completed",
                    json!({"index": index}),
                )
                .unwrap();
        }

        let page = store
            .read_session_records_page(
                "session_page",
                DurableJsonlPageOptions::with_after_sequence(Some(1), 2).unwrap(),
            )
            .unwrap();

        assert_eq!(page.total_records, 4);
        assert_eq!(page.matching_records, 3);
        assert_eq!(page.returned_records, 2);
        assert_eq!(page.next_after_sequence, Some(3));
        assert_eq!(
            page.records
                .iter()
                .map(|record| record.sequence)
                .collect::<Vec<_>>(),
            vec![2, 3]
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn subagent_transcript_and_metadata_use_run_scoped_sidecars() {
        let root = temp_root();
        let store = RunStore::new(&root);
        let run_id = RunId::from_string("run_subagents");

        let first = store
            .append_subagent_transcript_record_next(
                &run_id,
                "agent-1",
                None,
                "subagent.started",
                json!({"prompt": "review files"}),
            )
            .unwrap();
        let second = store
            .append_subagent_transcript_record_next(
                &run_id,
                "agent-1",
                Some(first),
                "subagent.message",
                json!({"summary": "looked at src/lib.rs"}),
            )
            .unwrap();
        let metadata = SubagentMetadata {
            agent_type: "code-reviewer".to_owned(),
            parent_agent_id: "executor".to_owned(),
            parent_harness_id: "native-code-edit".to_owned(),
            invocation_kind: "spawn".to_owned(),
            status: Some("completed".to_owned()),
            terminal_record_kind: Some("subagent.completed".to_owned()),
            last_sequence: Some(second),
            error: None,
            description: Some("review implementation".to_owned()),
            worktree_path: None,
            transcript_ref: Some(
                "subagent://runs/run_subagents/subagents/agent-agent-1.jsonl".to_owned(),
            ),
        };
        let metadata_ref = store
            .write_subagent_metadata(&run_id, "agent-1", &metadata)
            .unwrap();

        assert_eq!(first, 1);
        assert_eq!(second, 2);
        assert_eq!(
            metadata_ref,
            "subagent://runs/run_subagents/subagents/agent-agent-1.meta.json"
        );
        let records = store
            .read_subagent_transcript_records(&run_id, "agent-1")
            .unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].sequence, 1);
        assert_eq!(records[1].parent_sequence, Some(1));
        assert_eq!(records[1].payload["summary"], "looked at src/lib.rs");
        let loaded = store
            .read_subagent_metadata(&run_id, "agent-1")
            .unwrap()
            .unwrap();
        assert_eq!(loaded, metadata);
        let sequence_text = fs::read_to_string(
            root.join("runs")
                .join("run_subagents")
                .join("subagents")
                .join("agent-agent-1.seq"),
        )
        .unwrap();
        assert_eq!(sequence_text.trim(), "2");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn subagent_background_task_record_roundtrips() {
        let root = temp_root();
        let store = RunStore::new(&root);
        let record = SubagentBackgroundTaskRecord {
            task_id: "task-123".to_owned(),
            run_id: "run_subagents".to_owned(),
            agent_id: "agent-1".to_owned(),
            status: "running".to_owned(),
            created_at_ms: 1000,
            updated_at_ms: 1000,
            metadata_ref: "subagent://runs/run_subagents/subagents/agent-agent-1.meta.json"
                .to_owned(),
            transcript_ref: "subagent://runs/run_subagents/subagents/agent-agent-1.jsonl"
                .to_owned(),
            report: None,
            event_count: 0,
            events_truncated: false,
            error: None,
        };

        let task_ref = store
            .write_subagent_background_task_record(&record)
            .unwrap();
        assert_eq!(task_ref, "background-task://subagents/task-123.json");
        let loaded = store
            .read_subagent_background_task_record("task-123")
            .unwrap()
            .unwrap();
        assert_eq!(loaded.task_id, record.task_id);
        assert_eq!(loaded.run_id, record.run_id);
        assert_eq!(loaded.agent_id, record.agent_id);
        assert_eq!(loaded.status, "running");
        assert_eq!(loaded.event_count, 0);
        assert!(root
            .join("background-tasks")
            .join("subagents")
            .join("task-123.json")
            .exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn command_background_task_record_and_output_tail_roundtrips() {
        let root = temp_root();
        let store = RunStore::new(&root);
        let record = CommandBackgroundTaskRecord {
            task_id: "cmd-123".to_owned(),
            run_id: Some("run_cmd".to_owned()),
            repo_root: ".".to_owned(),
            cwd: ".".to_owned(),
            argv: vec!["echo".to_owned(), "hello".to_owned()],
            command: "echo hello".to_owned(),
            approval_key: "approval-1".to_owned(),
            policy: json!({"allowed": true}),
            status: "running".to_owned(),
            created_at_ms: 1000,
            updated_at_ms: 1000,
            output_ref: "background-task-output://commands/cmd-123.output".to_owned(),
            output_bytes: 0,
            output_truncated: false,
            max_output_bytes: 16,
            result: None,
            evidence_ref: None,
            error: None,
        };

        let task_ref = store.write_command_background_task_record(&record).unwrap();
        let output_ref = store
            .write_command_background_output_tail("cmd-123", b"0123456789abcdef")
            .unwrap();

        assert_eq!(task_ref, "background-task://commands/cmd-123.json");
        assert_eq!(
            output_ref,
            "background-task-output://commands/cmd-123.output"
        );
        let loaded = store
            .read_command_background_task_record("cmd-123")
            .unwrap()
            .unwrap();
        assert_eq!(loaded.task_id, record.task_id);
        assert_eq!(loaded.command, "echo hello");
        let full = store
            .read_command_background_output_tail("cmd-123", 64)
            .unwrap();
        assert_eq!(full.output, "0123456789abcdef");
        assert_eq!(full.bytes, 16);
        assert!(!full.truncated);
        let tail = store
            .read_command_background_output_tail("cmd-123", 6)
            .unwrap();
        assert_eq!(tail.output, "abcdef");
        assert_eq!(tail.bytes, 16);
        assert!(tail.truncated);
        assert!(root
            .join("background-tasks")
            .join("commands")
            .join("cmd-123.json")
            .exists());
        assert!(root
            .join("background-tasks")
            .join("commands")
            .join("cmd-123.output")
            .exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn subagent_transcript_page_reads_tail_records() {
        let root = temp_root();
        let store = RunStore::new(&root);
        let run_id = RunId::from_string("run_subagent_page");
        for index in 1..=4 {
            store
                .append_subagent_transcript_record_next(
                    &run_id,
                    "agent-1",
                    None,
                    "subagent.message",
                    json!({"index": index}),
                )
                .unwrap();
        }

        let page = store
            .read_subagent_transcript_records_page(
                &run_id,
                "agent-1",
                DurableJsonlPageOptions::tail(2).unwrap(),
            )
            .unwrap();

        assert_eq!(page.total_records, 4);
        assert_eq!(page.matching_records, 4);
        assert_eq!(
            page.records
                .iter()
                .map(|record| record.sequence)
                .collect::<Vec<_>>(),
            vec![3, 4]
        );
        assert!(page.truncated);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn run_content_replacement_records_roundtrip_and_reject_secrets() {
        let root = temp_root();
        let store = RunStore::new(&root);
        let run_id = RunId::from_string("run_replacements");

        let first = store
            .append_run_content_replacement_record_next(
                &run_id,
                vec![ContentReplacementRecord {
                    kind: "tool-result".to_owned(),
                    tool_use_id: "toolu-1".to_owned(),
                    replacement: "<persisted-output>preview</persisted-output>".to_owned(),
                }],
            )
            .unwrap();
        let second = store
            .append_run_content_replacement_record_next(
                &run_id,
                vec![ContentReplacementRecord {
                    kind: "tool-result".to_owned(),
                    tool_use_id: "toolu-2".to_owned(),
                    replacement: "<persisted-output>preview 2</persisted-output>".to_owned(),
                }],
            )
            .unwrap();

        assert_eq!(first, 1);
        assert_eq!(second, 2);
        let records = store.read_run_content_replacement_records(&run_id).unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].kind, "content-replacement");
        assert_eq!(records[0].replacements[0].tool_use_id, "toolu-1");
        let page = store
            .read_run_content_replacement_records_page(
                &run_id,
                DurableJsonlPageOptions::tail(1).unwrap(),
            )
            .unwrap();
        assert_eq!(page.records[0].sequence, 2);
        let sequence_text = fs::read_to_string(
            root.join("runs")
                .join("run_replacements")
                .join("content-replacements.seq"),
        )
        .unwrap();
        assert_eq!(sequence_text.trim(), "2");

        let error = store
            .append_run_content_replacement_record_next(
                &run_id,
                vec![ContentReplacementRecord {
                    kind: "tool-result".to_owned(),
                    tool_use_id: "toolu-secret".to_owned(),
                    replacement: "api_key should not persist".to_owned(),
                }],
            )
            .unwrap_err();
        assert!(matches!(error, StoreError::SessionRecordSecretLikeText));
        let sequence_text = fs::read_to_string(
            root.join("runs")
                .join("run_replacements")
                .join("content-replacements.seq"),
        )
        .unwrap();
        assert_eq!(sequence_text.trim(), "2");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn subagent_sidecars_reject_unsafe_agent_ids_and_secret_like_payloads() {
        let root = temp_root();
        let store = RunStore::new(&root);
        let run_id = RunId::from_string("run_subagents");

        let path_error = store
            .append_subagent_transcript_record_next(
                &run_id,
                "../escape",
                None,
                "subagent.started",
                json!({}),
            )
            .unwrap_err();
        assert!(matches!(path_error, StoreError::InvalidFileName(_)));

        let secret_error = store
            .append_subagent_transcript_record_next(
                &run_id,
                "agent-1",
                None,
                "subagent.message",
                json!({"api_key": "redacted"}),
            )
            .unwrap_err();
        assert!(matches!(
            secret_error,
            StoreError::SessionRecordSecretLikeText
        ));

        let metadata = SubagentMetadata {
            agent_type: "reviewer".to_owned(),
            parent_agent_id: "executor".to_owned(),
            parent_harness_id: "native".to_owned(),
            invocation_kind: "spawn".to_owned(),
            status: Some("running".to_owned()),
            terminal_record_kind: None,
            last_sequence: None,
            error: None,
            description: Some("contains password marker".to_owned()),
            worktree_path: None,
            transcript_ref: None,
        };
        let metadata_error = store
            .write_subagent_metadata(&run_id, "agent-1", &metadata)
            .unwrap_err();
        assert!(matches!(
            metadata_error,
            StoreError::SessionRecordSecretLikeText
        ));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn cache_bucket_usage_counts_real_files_and_bytes() {
        let root = temp_root();
        let store = RunStore::new(&root);
        store.ensure_local_layout().unwrap();

        store.write_blob(b"abc").unwrap();
        fs::write(root.join("repo-index").join("index.jsonl"), b"abcd").unwrap();

        let blob_usage = store.cache_bucket_usage("blobs").unwrap();
        let repo_index_usage = store.cache_bucket_usage("repo-index").unwrap();
        let missing_usage = store.cache_bucket_usage("logs").unwrap();

        assert_eq!(blob_usage.entries, 1);
        assert_eq!(blob_usage.bytes, 3);
        assert_eq!(repo_index_usage.entries, 1);
        assert_eq!(repo_index_usage.bytes, 4);
        assert_eq!(missing_usage.entries, 0);
        assert_eq!(missing_usage.bytes, 0);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn cache_bucket_usage_stops_at_scan_limit() {
        let root = temp_root();
        let cache_dir = root.join("repo-index");
        fs::create_dir_all(&cache_dir).unwrap();
        fs::write(cache_dir.join("first.txt"), b"1234").unwrap();
        fs::create_dir_all(cache_dir.join("empty-dir")).unwrap();
        fs::write(cache_dir.join("second.txt"), b"5678").unwrap();

        let usage = cache_bucket_usage_at_with_limit(&cache_dir, 2).unwrap();

        assert_eq!(usage.scanned_entries, 2);
        assert_eq!(usage.entry_scan_limit, 2);
        assert!(usage.truncated);
        assert!(usage.entries <= 2);
        assert!(usage.bytes <= 8);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn clear_disposable_caches_preserves_durable_store_data() {
        let root = temp_root();
        let store = RunStore::new(&root);
        store.ensure_local_layout().unwrap();

        fs::write(root.join("repo-index").join("index.jsonl"), b"repo").unwrap();
        fs::write(root.join("tmp").join("scratch.txt"), b"tmp").unwrap();
        fs::write(root.join("logs").join("server.log"), b"log").unwrap();
        store.write_blob(b"blob").unwrap();
        store
            .append_session_record("session-1", 1, "session.created", json!({}))
            .unwrap();

        let summary = store.clear_disposable_caches().unwrap();

        assert_eq!(
            summary.directories,
            vec!["repo-index", "plugin-cache", "skill-cache", "tmp"]
        );
        assert_eq!(summary.entries, 2);
        assert_eq!(summary.bytes, 7);
        assert!(root.join("repo-index").exists());
        assert!(root.join("tmp").exists());
        assert!(!root.join("repo-index").join("index.jsonl").exists());
        assert!(!root.join("tmp").join("scratch.txt").exists());
        assert!(root.join("logs").join("server.log").exists());
        assert!(root.join("sessions").join("session-1.jsonl").exists());
        assert_eq!(store.cache_bucket_usage("blobs").unwrap().entries, 1);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn blob_refs_are_content_addressed() {
        let root = temp_root();
        let store = RunStore::new(&root);

        let first = store.write_blob(b"same content").unwrap();
        let second = store.write_blob(b"same content").unwrap();

        assert_eq!(first, second);
        assert!(first.starts_with("blob://sha256/"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn large_text_refs_store_full_content_outside_event_payload() {
        let root = temp_root();
        let store = RunStore::new(&root);

        let payload = store
            .write_large_text_ref_with_limit("0123456789", 4)
            .unwrap();

        assert_eq!(payload.preview, "0123");
        assert!(payload.truncated);
        assert!(payload.blob_ref.starts_with("blob://sha256/"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn artifact_names_reject_path_traversal() {
        let root = temp_root();
        let store = RunStore::new(&root);
        let run_id = RunId::from_string("run_test");

        let error = store
            .write_artifact(&run_id, "../escape.json", &json!({"bad": true}))
            .unwrap_err();

        assert!(matches!(error, StoreError::InvalidFileName(_)));
        let wildcard_error = store
            .write_artifact(&run_id, "bad*name.json", &json!({"bad": true}))
            .unwrap_err();
        assert!(matches!(wildcard_error, StoreError::InvalidFileName(_)));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn compaction_circuit_state_persists_failures_and_resets_on_success() {
        let root = temp_root();
        let store = RunStore::new(&root);

        let first = store
            .record_compaction_circuit_outcome("run-compact", 3, false)
            .unwrap();
        let second = store
            .record_compaction_circuit_outcome("run-compact", 3, false)
            .unwrap();
        let third = store
            .record_compaction_circuit_outcome("run-compact", 3, false)
            .unwrap();

        assert_eq!(first.consecutive_failures, 1);
        assert_eq!(second.consecutive_failures, 2);
        assert_eq!(third.consecutive_failures, 3);
        assert!(third.circuit_breaker_open);
        let persisted = store
            .read_compaction_circuit_state("run-compact")
            .unwrap()
            .unwrap();
        assert_eq!(persisted.consecutive_failures, 3);
        assert!(persisted.circuit_breaker_open);

        let reset = store
            .record_compaction_circuit_outcome("run-compact", 3, true)
            .unwrap();
        assert_eq!(reset.consecutive_failures, 0);
        assert!(!reset.circuit_breaker_open);
        let invalid = store
            .record_compaction_circuit_outcome("../escape", 3, false)
            .unwrap_err();
        assert!(matches!(invalid, StoreError::InvalidFileName(_)));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn goal_state_json_roundtrips_deletes_and_rejects_unsafe_or_secret_like_state() {
        let root = temp_root();
        let store = RunStore::new(&root);
        let state = json!({
            "session_id": "session-1",
            "objective": "Ship the goal runtime",
            "status": "active",
            "token_budget": 100,
            "tokens_used": 5
        });

        assert!(store.read_goal_state_json("session-1").unwrap().is_none());
        let reference = store.write_goal_state_json("session-1", &state).unwrap();
        assert_eq!(reference, "goal://sessions/session-1.json");
        assert_eq!(
            store.read_goal_state_json("session-1").unwrap().unwrap(),
            state
        );
        assert!(store.delete_goal_state("session-1").unwrap());
        assert!(store.read_goal_state_json("session-1").unwrap().is_none());
        assert!(!store.delete_goal_state("session-1").unwrap());

        let invalid_write = store
            .write_goal_state_json("../escape", &json!({"objective": "safe"}))
            .unwrap_err();
        let invalid_read = store.read_goal_state_json("../escape").unwrap_err();
        let secret_write = store
            .write_goal_state_json("session-secret", &json!({"objective": "api_key=abc"}))
            .unwrap_err();

        assert!(matches!(invalid_write, StoreError::InvalidFileName(_)));
        assert!(matches!(invalid_read, StoreError::InvalidFileName(_)));
        assert!(matches!(
            secret_write,
            StoreError::SessionRecordSecretLikeText
        ));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn artifact_json_roundtrips_and_reports_missing() {
        let root = temp_root();
        let store = RunStore::new(&root);
        let run_id = RunId::from_string("run_test");

        let reference = store
            .write_artifact(&run_id, "summary.json", &json!({"status": "ok"}))
            .unwrap();
        let payload = store.read_artifact_json(&run_id, "summary.json").unwrap();
        let missing = store
            .read_artifact_json(&run_id, "missing.json")
            .unwrap_err();

        assert_eq!(reference, "artifact://runs/run_test/artifacts/summary.json");
        assert_eq!(payload["status"], "ok");
        assert!(matches!(missing, StoreError::ArtifactNotFound { .. }));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn checkpoint_json_roundtrips_lists_and_reports_missing() {
        let root = temp_root();
        let store = RunStore::new(&root);
        let run_id = RunId::from_string("run_test");

        let reference = store
            .write_checkpoint(&run_id, "resume.json", &json!({"step": 2}))
            .unwrap();
        let payload = store.read_checkpoint_json(&run_id, "resume.json").unwrap();
        let checkpoints = store.list_checkpoints(&run_id).unwrap();
        let missing = store
            .read_checkpoint_json(&run_id, "missing.json")
            .unwrap_err();

        assert_eq!(
            reference,
            "checkpoint://runs/run_test/checkpoints/resume.json"
        );
        assert_eq!(payload["step"], 2);
        assert_eq!(checkpoints[0].name, "resume.json");
        assert_eq!(checkpoints[0].checkpoint_ref, reference);
        assert!(matches!(missing, StoreError::CheckpointNotFound { .. }));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn checkpoint_names_reject_path_traversal() {
        let root = temp_root();
        let store = RunStore::new(&root);
        let run_id = RunId::from_string("run_test");

        let error = store
            .write_checkpoint(&run_id, "../escape.json", &json!({"bad": true}))
            .unwrap_err();

        assert!(matches!(error, StoreError::InvalidFileName(_)));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn blob_reads_by_sha256_digest() {
        let root = temp_root();
        let store = RunStore::new(&root);

        let reference = store.write_blob(b"same content").unwrap();
        let digest = reference.strip_prefix("blob://sha256/").unwrap();
        let loaded = store.read_blob_sha256(digest).unwrap();
        let missing = store
            .read_blob_sha256("0000000000000000000000000000000000000000000000000000000000000000")
            .unwrap_err();
        let invalid = store.read_blob_sha256("../escape").unwrap_err();

        assert_eq!(loaded, b"same content");
        assert!(matches!(missing, StoreError::BlobNotFound(_)));
        assert!(matches!(invalid, StoreError::InvalidBlobDigest(_)));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn durable_read_limit_rejects_oversized_files() {
        let root = temp_root();
        fs::create_dir_all(&root).unwrap();
        let path = root.join("oversized.jsonl");
        fs::write(&path, "12345").unwrap();

        let error = reject_file_over_read_limit(&path, 4).unwrap_err();

        assert!(matches!(
            error,
            StoreError::DurableReadLimitExceeded {
                bytes: 5,
                max_bytes: 4,
                ..
            }
        ));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn durable_json_sidecar_reads_reject_oversized_files() {
        let root = temp_root();
        let store = RunStore::new(&root);
        let run_id = RunId::from_string("run_oversized_sidecars");
        let mut state = RunState::new(run_id.clone(), coder_core::WorkflowId::new("workflow"));
        state.status = coder_core::RunStatus::Completed;
        store.write_metadata(&state).unwrap();
        store
            .write_artifact(&run_id, "summary.json", &json!({"status": "ok"}))
            .unwrap();
        store
            .write_checkpoint(&run_id, "resume.json", &json!({"step": 1}))
            .unwrap();
        store
            .write_subagent_metadata(
                &run_id,
                "agent-1",
                &SubagentMetadata {
                    agent_type: "reviewer".to_owned(),
                    parent_agent_id: "executor".to_owned(),
                    parent_harness_id: "native".to_owned(),
                    invocation_kind: "spawn".to_owned(),
                    status: Some("running".to_owned()),
                    terminal_record_kind: None,
                    last_sequence: None,
                    error: None,
                    description: None,
                    worktree_path: None,
                    transcript_ref: None,
                },
            )
            .unwrap();

        let oversized_paths = [
            root.join("runs")
                .join("run_oversized_sidecars")
                .join("metadata.json"),
            root.join("runs")
                .join("run_oversized_sidecars")
                .join("artifacts")
                .join("summary.json"),
            root.join("runs")
                .join("run_oversized_sidecars")
                .join("checkpoints")
                .join("resume.json"),
            root.join("runs")
                .join("run_oversized_sidecars")
                .join("subagents")
                .join("agent-agent-1.meta.json"),
        ];
        for path in &oversized_paths {
            fs::OpenOptions::new()
                .write(true)
                .open(path)
                .unwrap()
                .set_len(MAX_DURABLE_READ_BYTES + 1)
                .unwrap();
        }

        assert!(matches!(
            store.read_metadata(&run_id).unwrap_err(),
            StoreError::DurableReadLimitExceeded { .. }
        ));
        assert!(matches!(
            store
                .read_artifact_json(&run_id, "summary.json")
                .unwrap_err(),
            StoreError::DurableReadLimitExceeded { .. }
        ));
        assert!(matches!(
            store
                .read_checkpoint_json(&run_id, "resume.json")
                .unwrap_err(),
            StoreError::DurableReadLimitExceeded { .. }
        ));
        assert!(matches!(
            store
                .read_subagent_metadata(&run_id, "agent-1")
                .unwrap_err(),
            StoreError::DurableReadLimitExceeded { .. }
        ));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn run_store_operations_reject_unsafe_run_segments() {
        let root = temp_root();
        let store = RunStore::new(&root);
        let run_id = RunId::from_string("../escape");
        let mut state = RunState::new(run_id.clone(), coder_core::WorkflowId::new("workflow"));
        state.status = coder_core::RunStatus::Completed;

        let metadata_error = store.write_metadata(&state).unwrap_err();
        let event_error = store
            .append_event(
                &run_id,
                &CoderEvent::new(run_id.clone(), 1, "run.started", json!({})),
            )
            .unwrap_err();
        let artifact_error = store
            .write_artifact(&run_id, "summary.json", &json!({"bad": true}))
            .unwrap_err();

        assert!(matches!(
            metadata_error,
            StoreError::InvalidStoreSegment { .. }
        ));
        assert!(matches!(
            event_error,
            StoreError::InvalidStoreSegment { .. }
        ));
        assert!(matches!(
            artifact_error,
            StoreError::InvalidStoreSegment { .. }
        ));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn metadata_and_report_roundtrip() {
        let root = temp_root();
        let store = RunStore::new(&root);
        let run_id = RunId::from_string("run_test");
        let mut state = RunState::new(run_id.clone(), coder_core::WorkflowId::new("workflow"));
        state.status = coder_core::RunStatus::Completed;
        let report = FinalReport::completed("done").with_evidence("event_log", "eventlog://run");

        store.write_metadata(&state).unwrap();
        store.write_report(&run_id, &report).unwrap();

        let loaded_state = store.read_metadata(&run_id).unwrap().unwrap();
        let loaded_report = store.read_report(&run_id).unwrap().unwrap();
        assert_eq!(loaded_state.status, coder_core::RunStatus::Completed);
        assert_eq!(loaded_report.summary, "done");
        assert_eq!(loaded_report.evidence_refs[0].kind, "event_log");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn report_artifact_redacts_key_like_strings() {
        let root = temp_root();
        let store = RunStore::new(&root);
        let run_id = RunId::from_string("run_test");
        let mut report = FinalReport::completed("Used sk-live-1234567890");
        report
            .checks
            .push("cargo test sk-live-1234567890".to_owned());
        report
            .blockers
            .push("blocked by sk-live-1234567890".to_owned());

        store.write_report(&run_id, &report).unwrap();

        let text = fs::read_to_string(
            root.join("runs")
                .join("run_test")
                .join("artifacts")
                .join("final-report.json"),
        )
        .unwrap();
        assert!(!text.contains("sk-live-1234567890"));
        assert!(text.contains("[REDACTED]"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn evidence_report_blocks_on_command_approval_request() {
        let root = temp_root();
        let store = RunStore::new(&root);
        let run_id = RunId::from_string("run-1");
        store
            .append_event(
                &run_id,
                &CoderEvent::new(
                    run_id.clone(),
                    1,
                    "approval.requested",
                    json!({
                        "approval_type": "command",
                        "command": "cargo test",
                        "approval_key": "cmd:abc"
                    }),
                ),
            )
            .unwrap();

        let report = store.build_evidence_report(&run_id).unwrap();

        assert_eq!(report.status, ReportStatus::Blocked);
        assert!(report
            .blockers
            .iter()
            .any(|item| item.contains("cargo test")));
        assert!(report
            .evidence_refs
            .iter()
            .any(|reference| reference.kind == "event_log"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn evidence_report_fails_on_failed_command_event() {
        let root = temp_root();
        let store = RunStore::new(&root);
        let run_id = RunId::from_string("run-1");
        store
            .append_event(
                &run_id,
                &CoderEvent::new(
                    run_id.clone(),
                    1,
                    "command.failed",
                    json!({
                        "command": "cargo test",
                        "status": "failed",
                        "passed": false,
                        "returncode": 101
                    }),
                ),
            )
            .unwrap();

        let report = store.build_evidence_report(&run_id).unwrap();

        assert_eq!(report.status, ReportStatus::Failed);
        assert!(report.checks[0].contains("cargo test"));
        assert!(report.blockers[0].contains("Command failed"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn evidence_report_does_not_fail_on_cancelled_command_event() {
        let root = temp_root();
        let store = RunStore::new(&root);
        let run_id = RunId::from_string("run-1");
        store
            .append_event(
                &run_id,
                &CoderEvent::new(
                    run_id.clone(),
                    1,
                    "command.failed",
                    json!({
                        "command": "powershell Start-Sleep -Seconds 30",
                        "status": "cancelled",
                        "passed": false,
                        "returncode": 1,
                        "timed_out": false
                    }),
                ),
            )
            .unwrap();

        let report = store.build_evidence_report(&run_id).unwrap();

        assert_eq!(report.status, ReportStatus::Completed);
        assert!(report
            .checks
            .iter()
            .any(|check| check.contains("cancelled exit 1")));
        assert!(report.blockers.is_empty());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn evidence_report_includes_plan_context_from_run_started() {
        let root = temp_root();
        let store = RunStore::new(&root);
        let run_id = RunId::from_string("run-1");
        store
            .append_event(
                &run_id,
                &CoderEvent::new(
                    run_id.clone(),
                    1,
                    "run.started",
                    json!({
                        "plan_context": {
                            "original_user_request": "Update Planner loop",
                            "plan_draft": {
                                "goal": "Update Planner loop",
                                "acceptance_criteria": ["final report cites plan context"]
                            }
                        }
                    }),
                ),
            )
            .unwrap();

        let report = store.build_evidence_report(&run_id).unwrap();

        assert!(report
            .checks
            .iter()
            .any(|check| check == "plan_context: Update Planner loop"));
        assert!(!report
            .checks
            .iter()
            .any(|check| check == "acceptance: final report cites plan context"));
        assert!(report.summary.contains("Requested: Update Planner loop"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn evidence_report_summary_covers_request_work_evidence_risks_and_next_steps() {
        let root = temp_root();
        let store = RunStore::new(&root);
        let run_id = RunId::from_string("run-1");
        store
            .append_event(
                &run_id,
                &CoderEvent::new(
                    run_id.clone(),
                    1,
                    "run.started",
                    json!({"task": "Update README.md"}),
                ),
            )
            .unwrap();
        store
            .append_event(
                &run_id,
                &CoderEvent::new(
                    run_id.clone(),
                    2,
                    "command.completed",
                    json!({
                        "command": "cargo test",
                        "status": "completed",
                        "passed": true,
                        "returncode": 0
                    }),
                )
                .with_ref("command_evidence", "repo-evidence://repo-test:abc"),
            )
            .unwrap();
        store
            .append_event(
                &run_id,
                &CoderEvent::new(
                    run_id.clone(),
                    3,
                    "patch.applied",
                    json!({
                        "evidence_ref": "repo-diff:def",
                        "files": [{"new_path": "README.md", "status": "modified"}]
                    }),
                )
                .with_ref("patch_evidence", "repo-evidence://repo-diff:def"),
            )
            .unwrap();

        let report = store.build_evidence_report(&run_id).unwrap();

        assert_eq!(report.status, ReportStatus::Completed);
        assert!(report.summary.contains("Status: completed"));
        assert!(report.summary.contains("Requested: Update README.md"));
        assert!(report
            .summary
            .contains("Done: Command completed: cargo test"));
        assert!(report.summary.contains("Patch applied"));
        assert!(report.summary.contains("Changed files: README.md"));
        assert!(report
            .summary
            .contains("Verification: cargo test: completed exit 0"));
        assert!(report
            .summary
            .contains("Evidence: 3 evidence ref(s) recorded"));
        assert!(report
            .summary
            .contains("Remaining risks: No remaining blocker or risk was recorded."));
        assert!(report
            .summary
            .contains("Next steps: No next step was recorded."));
        assert!(!report.summary.contains("repo-evidence://"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn evidence_report_includes_completed_verification_events() {
        let root = temp_root();
        let store = RunStore::new(&root);
        let run_id = RunId::from_string("run-1");
        store
            .append_event(
                &run_id,
                &CoderEvent::new(
                    run_id.clone(),
                    1,
                    "verification.completed",
                    json!({
                        "status": "completed",
                        "summary": "browser gameplay passed",
                        "evidence": {"total_refs": 1}
                    }),
                )
                .with_ref("browser_validation", "blob://sha256/browser-proof"),
            )
            .unwrap();

        let report = store.build_evidence_report(&run_id).unwrap();

        assert_eq!(report.status, ReportStatus::Completed);
        assert!(report
            .checks
            .iter()
            .any(|check| check == "verification: browser gameplay passed"));
        assert!(report
            .summary
            .contains("Verification: verification: browser gameplay passed"));
        assert!(report.evidence_refs.iter().any(|reference| {
            reference.kind == "browser_validation"
                && reference.reference == "blob://sha256/browser-proof"
        }));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn evidence_report_expands_verification_check_payloads() {
        let root = temp_root();
        let store = RunStore::new(&root);
        let run_id = RunId::from_string("run-1");
        store
            .append_event(
                &run_id,
                &CoderEvent::new(
                    run_id.clone(),
                    1,
                    "verification.completed",
                    json!({
                        "status": "completed",
                        "summary": "browser verification passed",
                        "checks": [
                            {
                                "name": "snake_gameplay_browser.restart_score",
                                "status": "passed",
                                "detail": "restart reset visible score"
                            }
                        ]
                    }),
                ),
            )
            .unwrap();

        let report = store.build_evidence_report(&run_id).unwrap();

        assert!(report.checks.iter().any(|check| {
            check
                == "verification: snake_gameplay_browser.restart_score passed - restart reset visible score"
        }));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn evidence_report_blocks_on_missing_required_verification_evidence() {
        let root = temp_root();
        let store = RunStore::new(&root);
        let run_id = RunId::from_string("run-1");
        store
            .append_event(
                &run_id,
                &CoderEvent::new(
                    run_id.clone(),
                    1,
                    "verification.failed",
                    json!({
                        "status": "failed",
                        "reason": "verification requires evidence refs before completion, but the backend returned none",
                        "evidence": {"total_refs": 0}
                    }),
                ),
            )
            .unwrap();

        let report = store.build_evidence_report(&run_id).unwrap();

        assert_eq!(report.status, ReportStatus::Blocked);
        assert!(report
            .checks
            .iter()
            .any(|check| check.contains("verification: failed")));
        assert!(report
            .blockers
            .iter()
            .any(|blocker| blocker.contains("Verification blocked")));
        assert!(report
            .summary
            .contains("Remaining risks: Verification blocked"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn evidence_report_clears_repaired_verification_failure() {
        let root = temp_root();
        let store = RunStore::new(&root);
        let run_id = RunId::from_string("run-1");
        store
            .append_event(
                &run_id,
                &CoderEvent::new(
                    run_id.clone(),
                    1,
                    "verification.failed",
                    json!({
                        "status": "blocked",
                        "reason": "browser_dynamic.playwright missing"
                    }),
                ),
            )
            .unwrap();
        store
            .append_event(
                &run_id,
                &CoderEvent::new(
                    run_id.clone(),
                    2,
                    "verification.completed",
                    json!({
                        "status": "completed",
                        "summary": "browser verification passed"
                    }),
                ),
            )
            .unwrap();

        let report = store.build_evidence_report(&run_id).unwrap();

        assert_eq!(report.status, ReportStatus::Completed);
        assert!(report
            .checks
            .iter()
            .any(|check| check.contains("browser_dynamic.playwright missing")));
        assert!(report.blockers.is_empty());
        assert!(report
            .summary
            .contains("Remaining risks: No remaining blocker or risk was recorded."));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn evidence_report_cancels_on_cancelled_run_state() {
        let root = temp_root();
        let store = RunStore::new(&root);
        let run_id = RunId::from_string("run-1");
        let mut state = RunState::new(run_id.clone(), coder_core::WorkflowId::new("workflow"));
        state.status = RunStatus::Cancelled;
        store.write_metadata(&state).unwrap();
        store
            .append_event(
                &run_id,
                &CoderEvent::new(
                    run_id.clone(),
                    1,
                    "run.cancelled",
                    json!({"reason": "user_cancelled"}),
                ),
            )
            .unwrap();

        let report = store.build_evidence_report(&run_id).unwrap();

        assert_eq!(report.status, ReportStatus::Cancelled);
        assert!(report.summary.contains("cancelled"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn evidence_report_includes_repo_evidence_only_runs() {
        let root = temp_root();
        let store = RunStore::new(&root);
        let run_id = RunId::from_string("run-1");
        let reference = store
            .write_repo_evidence(
                &run_id,
                RepoEvidenceKind::RepoRead,
                "repo",
                Vec::new(),
                "read",
                json!({"snippet": "safe"}),
            )
            .unwrap();

        let report = store.build_evidence_report(&run_id).unwrap();

        assert_eq!(report.status, ReportStatus::Completed);
        assert!(report
            .evidence_refs
            .iter()
            .any(|item| item.reference == reference.ref_id));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn evidence_report_includes_patch_event_files_and_refs() {
        let root = temp_root();
        let store = RunStore::new(&root);
        let run_id = RunId::from_string("run-1");
        store
            .append_event(
                &run_id,
                &CoderEvent::new(
                    run_id.clone(),
                    1,
                    "patch.previewed",
                    json!({
                        "evidence_ref": "repo-diff:abc",
                        "files": [
                            {
                                "old_path": "src/old.py",
                                "new_path": "src/app.py",
                                "status": "modified"
                            }
                        ]
                    }),
                )
                .with_ref("patch_evidence", "repo-evidence://repo-diff:abc"),
            )
            .unwrap();

        let report = store.build_evidence_report(&run_id).unwrap();

        assert_eq!(report.changed_files, vec!["src/app.py"]);
        assert_eq!(report.patch_refs, vec!["repo-evidence://repo-diff:abc"]);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn evidence_report_blocks_on_patch_apply_approval_request() {
        let root = temp_root();
        let store = RunStore::new(&root);
        let run_id = RunId::from_string("run-1");
        store
            .append_event(
                &run_id,
                &CoderEvent::new(
                    run_id.clone(),
                    1,
                    "approval.requested",
                    json!({
                        "approval_type": "patch_apply",
                        "patch_file": "change.patch",
                        "evidence_ref": "repo-diff:abc",
                        "files": [
                            {
                                "old_path": "src/app.py",
                                "new_path": "src/app.py",
                                "status": "modified"
                            }
                        ]
                    }),
                )
                .with_ref("patch_evidence", "repo-evidence://repo-diff:abc"),
            )
            .unwrap();

        let report = store.build_evidence_report(&run_id).unwrap();

        assert_eq!(report.status, ReportStatus::Blocked);
        assert_eq!(report.changed_files, vec!["src/app.py"]);
        assert_eq!(report.patch_refs, vec!["repo-evidence://repo-diff:abc"]);
        assert!(report.blockers[0].contains("change.patch"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn evidence_report_tracks_applied_and_failed_patch_events() {
        let root = temp_root();
        let store = RunStore::new(&root);
        let run_id = RunId::from_string("run-1");
        store
            .append_event(
                &run_id,
                &CoderEvent::new(
                    run_id.clone(),
                    1,
                    "patch.applied",
                    json!({
                        "patch_file": "good.patch",
                        "evidence_ref": "repo-diff:good",
                        "files": [
                            {
                                "old_path": "src/app.py",
                                "new_path": "src/app.py",
                                "status": "modified"
                            }
                        ]
                    }),
                )
                .with_ref("patch_evidence", "repo-evidence://repo-diff:good"),
            )
            .unwrap();
        store
            .append_event(
                &run_id,
                &CoderEvent::new(
                    run_id.clone(),
                    2,
                    "patch.failed",
                    json!({
                        "patch_file": "bad.patch",
                        "evidence_ref": "repo-diff:bad",
                        "files": [
                            {
                                "old_path": "src/bad.py",
                                "new_path": "src/bad.py",
                                "status": "modified"
                            }
                        ]
                    }),
                )
                .with_ref("patch_evidence", "repo-evidence://repo-diff:bad"),
            )
            .unwrap();

        let report = store.build_evidence_report(&run_id).unwrap();

        assert_eq!(report.status, ReportStatus::Failed);
        assert_eq!(report.changed_files, vec!["src/app.py", "src/bad.py"]);
        assert_eq!(
            report.patch_refs,
            vec![
                "repo-evidence://repo-diff:bad",
                "repo-evidence://repo-diff:good"
            ]
        );
        assert!(report.blockers[0].contains("bad.patch"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn evidence_report_includes_repo_patch_preview_files_and_refs() {
        let root = temp_root();
        let store = RunStore::new(&root);
        let run_id = RunId::from_string("run-1");
        let reference = store
            .write_repo_evidence(
                &run_id,
                RepoEvidenceKind::RepoDiff,
                "repo",
                Vec::new(),
                "Previewed patch touching 1 file.",
                json!({
                    "operation": "patch_preview",
                    "preview": {
                        "files": [
                            {
                                "old_path": null,
                                "new_path": "src/new.py",
                                "status": "added"
                            }
                        ]
                    }
                }),
            )
            .unwrap();

        let report = store.build_evidence_report(&run_id).unwrap();

        assert_eq!(report.changed_files, vec!["src/new.py"]);
        assert_eq!(
            report.patch_refs,
            vec![format!("repo-evidence://{}", reference.ref_id)]
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn list_run_summaries_reports_counts_and_skips_unsafe_dirs() {
        let root = temp_root();
        let store = RunStore::new(&root);
        let run_id = RunId::from_string("run_test");
        let mut state = RunState::new(run_id.clone(), coder_core::WorkflowId::new("workflow"));
        state.status = coder_core::RunStatus::Completed;
        let report = FinalReport::completed("done");

        fs::create_dir_all(root.join("runs").join("bad run")).unwrap();
        store.write_metadata(&state).unwrap();
        store
            .append_event(
                &run_id,
                &CoderEvent::new(run_id.clone(), 1, "run.started", json!({})),
            )
            .unwrap();
        store.write_report(&run_id, &report).unwrap();
        store
            .write_repo_evidence(
                &run_id,
                RepoEvidenceKind::RepoRead,
                "repo",
                Vec::new(),
                "read",
                json!({"snippet": "safe"}),
            )
            .unwrap();

        let summaries = store.list_run_summaries().unwrap();

        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].run_id, "run_test");
        assert_eq!(summaries[0].metadata.as_ref().unwrap().status, state.status);
        assert_eq!(summaries[0].event_count, 1);
        assert!(summaries[0].has_report);
        assert_eq!(summaries[0].repo_evidence_count, 1);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn list_run_summaries_is_empty_without_runs_dir() {
        let root = temp_root();
        let store = RunStore::new(&root);

        let summaries = store.list_run_summaries().unwrap();

        assert!(summaries.is_empty());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn repo_evidence_roundtrips_with_index_ref() {
        let root = temp_root();
        let store = RunStore::new(&root);
        let run_id = RunId::from_string("run-1");

        let reference = store
            .write_repo_evidence(
                &run_id,
                RepoEvidenceKind::RepoTextSearch,
                "F:/repo",
                vec!["src".to_owned()],
                "Found one hit.",
                json!({"evidence_kind": "repo_evidence", "hits": [{"path": "src/app.py", "line": 1}]}),
            )
            .unwrap();
        let payload = store.read_repo_evidence(&reference.ref_id).unwrap();

        assert!(reference.ref_id.starts_with("repo-text-search:"));
        assert_eq!(reference.kind, RepoEvidenceKind::RepoTextSearch);
        assert!(PathBuf::from(&reference.payload_path)
            .starts_with(root.join("runs").join("run-1").join("repo_evidence")));
        assert_eq!(payload["hits"][0]["path"], "src/app.py");
        let records = store.list_repo_evidence(&run_id).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].ref_id, reference.ref_id);
        assert_eq!(records[0].summary, "Found one hit.");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn repo_evidence_rejects_unsafe_segments() {
        let root = temp_root();
        let store = RunStore::new(&root);

        let run_error = store
            .write_repo_evidence(
                &RunId::from_string("../escape"),
                RepoEvidenceKind::RepoRead,
                "repo",
                Vec::new(),
                "bad",
                json!({"text": "safe"}),
            )
            .unwrap_err();
        let ref_error = store.read_repo_evidence("../escape").unwrap_err();

        assert!(matches!(run_error, StoreError::InvalidStoreSegment { .. }));
        assert!(matches!(ref_error, StoreError::InvalidStoreSegment { .. }));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn repo_evidence_compacts_large_strings_and_lists() {
        let root = temp_root();
        let store = RunStore::new(&root);
        let run_id = RunId::from_string("run-1");
        let items = (0..350)
            .map(|index| json!({"path": format!("src/{index}.rs")}))
            .collect::<Vec<_>>();

        let reference = store
            .write_repo_evidence(
                &run_id,
                RepoEvidenceKind::RepoFileList,
                "repo",
                Vec::new(),
                "large",
                json!({"snippet": "x".repeat(20_000), "items": items}),
            )
            .unwrap();
        let payload = store.read_repo_evidence(&reference.ref_id).unwrap();

        assert!(payload["snippet"].as_str().unwrap().len() < 20_000);
        assert!(payload["snippet"].as_str().unwrap().ends_with("..."));
        assert_eq!(payload["items"].as_array().unwrap().len(), 301);
        assert_eq!(payload["items"][300]["truncated"], true);
        assert_eq!(payload["items"][300]["omitted_items"], 50);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn repo_evidence_rejects_secret_like_text() {
        let root = temp_root();
        let store = RunStore::new(&root);
        let run_id = RunId::from_string("run-1");

        let error = store
            .write_repo_evidence(
                &run_id,
                RepoEvidenceKind::RepoRead,
                "repo",
                Vec::new(),
                "secret",
                json!({"snippet": "api_key=abc"}),
            )
            .unwrap_err();

        assert!(matches!(error, StoreError::RepoEvidenceSecretLikeText));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn repo_evidence_rejects_payload_path_escape() {
        let root = temp_root();
        let store = RunStore::new(&root);
        let run_id = RunId::from_string("run-1");
        let reference = store
            .write_repo_evidence(
                &run_id,
                RepoEvidenceKind::RepoRead,
                "repo",
                Vec::new(),
                "read",
                json!({"snippet": "safe"}),
            )
            .unwrap();
        let outside = root.join("outside.json");
        fs::write(&outside, "{}").unwrap();
        let mut escaped = reference;
        escaped.payload_path = outside.display().to_string();
        let index_path = root
            .join("runs")
            .join("run-1")
            .join("repo_evidence")
            .join("index.jsonl");
        fs::write(
            index_path,
            format!("{}\n", serde_json::to_string(&escaped).unwrap()),
        )
        .unwrap();

        let error = store.read_repo_evidence(&escaped.ref_id).unwrap_err();

        assert!(matches!(error, StoreError::RepoEvidencePathEscape(_)));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn repo_evidence_reads_reject_oversized_index_and_payload() {
        let root = temp_root();
        let store = RunStore::new(&root);
        let run_id = RunId::from_string("run-1");
        let reference = store
            .write_repo_evidence(
                &run_id,
                RepoEvidenceKind::RepoRead,
                "repo",
                Vec::new(),
                "read",
                json!({"snippet": "safe"}),
            )
            .unwrap();
        let payload_path = PathBuf::from(&reference.payload_path);
        fs::OpenOptions::new()
            .write(true)
            .open(&payload_path)
            .unwrap()
            .set_len(MAX_DURABLE_READ_BYTES + 1)
            .unwrap();

        let payload_error = store.read_repo_evidence(&reference.ref_id).unwrap_err();
        assert!(matches!(
            payload_error,
            StoreError::DurableReadLimitExceeded { .. }
        ));

        fs::write(&payload_path, "{}").unwrap();
        let index_path = root
            .join("runs")
            .join("run-1")
            .join("repo_evidence")
            .join("index.jsonl");
        fs::OpenOptions::new()
            .write(true)
            .open(&index_path)
            .unwrap()
            .set_len(MAX_DURABLE_READ_BYTES + 1)
            .unwrap();

        let read_index_error = store.read_repo_evidence(&reference.ref_id).unwrap_err();
        assert!(matches!(
            read_index_error,
            StoreError::DurableReadLimitExceeded { .. }
        ));
        let list_index_error = store.list_repo_evidence(&run_id).unwrap_err();
        assert!(matches!(
            list_index_error,
            StoreError::DurableReadLimitExceeded { .. }
        ));
        let _ = fs::remove_dir_all(root);
    }

    fn temp_root() -> PathBuf {
        static NEXT_TEMP_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let id = NEXT_TEMP_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        test_tmp_root().join(format!("coder-store-{}-{}", std::process::id(), id))
    }

    fn test_tmp_root() -> PathBuf {
        std::env::var_os("CODER_TEST_TMPDIR")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
    }
}
