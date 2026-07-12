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
use serde::{de::DeserializeOwned, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
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
        let mut command_blockers = Vec::new();
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
                            command_blockers.push(format!("Command timed out: {command}"));
                        } else {
                            command_blockers.push(format!("Command failed: {command}"));
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
        let terminal_completed = metadata
            .as_ref()
            .is_some_and(|state| state.status == RunStatus::Completed)
            || events.iter().any(|event| event.kind == "run.completed");
        if !terminal_completed {
            blockers.extend(command_blockers);
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

mod models;
pub use models::*;

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
    if repo_evidence_text_is_secret_like(value) {
        return Err(StoreError::RepoEvidenceSecretLikeText);
    }
    Ok(())
}

pub fn redact_repo_evidence_payload(value: Value) -> Value {
    match value {
        Value::Object(object) => Value::Object(
            object
                .into_iter()
                .map(|(key, value)| (key, redact_repo_evidence_payload(value)))
                .collect(),
        ),
        Value::Array(items) => Value::Array(
            items
                .into_iter()
                .map(redact_repo_evidence_payload)
                .collect(),
        ),
        Value::String(text) if repo_evidence_text_is_secret_like(&text) => {
            Value::String("[REDACTED]".to_owned())
        }
        other => other,
    }
}

fn repo_evidence_text_is_secret_like(value: &str) -> bool {
    let lowered = value.to_ascii_lowercase();
    REPO_EVIDENCE_SECRET_MARKERS
        .iter()
        .any(|marker| lowered.contains(marker))
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
mod tests;
