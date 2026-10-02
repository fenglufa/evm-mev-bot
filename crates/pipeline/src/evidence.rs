//! §41's evidence files, written as the run proceeds.
//!
//! The point of a session record is that it answers §42's twelve questions
//! without re-running anything, so the files are organised by the stage that
//! produced them and each line carries the identity of the record above it:
//! a state update names its block, transaction hash, transaction index and log
//! index; a finding names the state version it was priced against; a simulation
//! result names the finding and the block it read. M6 extends the same chain one
//! link further — an execution row names the finding, the simulation and the risk
//! decision it came from (§37), and the signed and submission rows name the
//! execution they belong to (§52, §53).
//!
//! §49's other half lives in [`EvidenceWriter::write_whole`]: the summary files
//! are written to a temporary name and renamed into place, so a process that
//! dies mid-write leaves the previous complete file rather than half of a new
//! one.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::error::{PipelineError, Result};

/// The §41 file set. Names follow the task's list so a reader of the report and
/// a reader of the directory use the same words.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EvidenceFile {
    Blocks,
    Events,
    StateUpdates,
    Opportunities,
    SimulationResults,
    RiskDecisions,
    /// `pending` observations: candidates, never state (§27).
    Candidates,
    /// Findings that were not simulated, with the reason (§51).
    Declines,
    /// Reconnects, subscription answers, provider errors.
    Status,
    /// M6 §37: one row per attempt that reached the execution lane, with the four
    /// ids and the rung it ended at.
    Executions,
    /// M6 §52: the signed envelope — chain, nonce, type, `to`, value, gas, fee,
    /// the calldata's hash and the recovered sender. Never the key.
    SignedTransactions,
    /// M6 §53: what the endpoint answered, including the run that answered
    /// `BLOCKED` because it accepts no submission at all.
    Submissions,
}

impl EvidenceFile {
    pub const fn file_name(self) -> &'static str {
        match self {
            Self::Blocks => "blocks.jsonl",
            Self::Events => "events.jsonl",
            Self::StateUpdates => "state-updates.jsonl",
            Self::Opportunities => "opportunities.jsonl",
            Self::SimulationResults => "simulation-results.jsonl",
            Self::RiskDecisions => "risk-decisions.jsonl",
            Self::Candidates => "candidates.jsonl",
            Self::Declines => "declines.jsonl",
            Self::Status => "status.jsonl",
            Self::Executions => "executions.jsonl",
            Self::SignedTransactions => "signed-transactions.jsonl",
            Self::Submissions => "submissions.jsonl",
        }
    }

    /// Whether this file exists only because a run had somewhere to send a
    /// transaction to.
    pub const fn is_execution_lane(self) -> bool {
        matches!(
            self,
            Self::Executions | Self::SignedTransactions | Self::Submissions
        )
    }
}

/// One session's directory, with one open file per stage.
pub struct EvidenceWriter {
    dir: PathBuf,
    session_id: String,
    files: Vec<(&'static str, File)>,
    lines: u64,
}

impl EvidenceWriter {
    /// Creates the directory if needed and opens every §41 file for appending.
    ///
    /// The directory is one session's own — the caller names it after the session —
    /// so a rerun never lands on a previous run's lines. Inside it, the files
    /// append rather than truncate, and an existing directory is reused rather
    /// than cleared: §43's real runs are evidence, and a write that has to restart
    /// must not destroy what it already recorded.
    ///
    /// `execution_lane` decides whether M6's three files are part of this
    /// session's shape at all. A run without a lane gets no `executions.jsonl`
    /// rather than an empty one, because "there were no attempts" and "this run
    /// could not make an attempt" are different answers to a reader holding one
    /// file and no session record.
    pub fn open(dir: &Path, session_id: &str, execution_lane: bool) -> Result<Self> {
        let mut kinds: Vec<EvidenceFile> = vec![
            EvidenceFile::Blocks,
            EvidenceFile::Events,
            EvidenceFile::StateUpdates,
            EvidenceFile::Opportunities,
            EvidenceFile::SimulationResults,
            EvidenceFile::RiskDecisions,
            EvidenceFile::Candidates,
            EvidenceFile::Declines,
            EvidenceFile::Status,
        ];
        if execution_lane {
            kinds.extend([
                EvidenceFile::Executions,
                EvidenceFile::SignedTransactions,
                EvidenceFile::Submissions,
            ]);
        }
        Self::open_kinds(dir, session_id, &kinds)
    }

    /// A session directory holding only M6's three files: the run that produced
    /// these rows moved no market data at all, and an empty `blocks.jsonl` beside
    /// them would read as a stream that saw nothing rather than as a run that
    /// never had one. This is §35's validation transaction's writer.
    pub fn execution_only(dir: &Path, session_id: &str) -> Result<Self> {
        Self::open_kinds(
            dir,
            session_id,
            &[
                EvidenceFile::Executions,
                EvidenceFile::SignedTransactions,
                EvidenceFile::Submissions,
            ],
        )
    }

    /// A session directory holding M7's route-run files: the three rows a finding walks
    /// through on its way to a decision, plus the execution lane's three. No
    /// `blocks.jsonl` and no `state-updates.jsonl`, because a route run pins one block and
    /// drives no state engine — an empty stream file would read as a stream that saw nothing
    /// rather than as a run that never had one.
    pub fn route_run(dir: &Path, session_id: &str) -> Result<Self> {
        Self::open_kinds(
            dir,
            session_id,
            &[
                EvidenceFile::Opportunities,
                EvidenceFile::SimulationResults,
                EvidenceFile::RiskDecisions,
                EvidenceFile::Executions,
                EvidenceFile::SignedTransactions,
                EvidenceFile::Submissions,
            ],
        )
    }

    fn open_kinds(dir: &Path, session_id: &str, kinds: &[EvidenceFile]) -> Result<Self> {
        std::fs::create_dir_all(dir).map_err(|error| PipelineError::Evidence {
            path: dir.to_path_buf(),
            detail: format!("could not create the session directory: {error}"),
        })?;
        let mut files = Vec::new();
        for kind in kinds {
            let path = dir.join(kind.file_name());
            let opened = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .map_err(|error| PipelineError::Evidence {
                    path: path.clone(),
                    detail: format!("could not be opened for appending: {error}"),
                })?;
            files.push((kind.file_name(), opened));
        }
        Ok(Self {
            dir: dir.to_path_buf(),
            session_id: session_id.to_string(),
            files,
            lines: 0,
        })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub const fn lines(&self) -> u64 {
        self.lines
    }

    /// Append one record. Every line carries the session id, so a file copied out
    /// of the directory still says which run wrote it.
    pub fn line(&mut self, kind: EvidenceFile, record: &Value) -> Result<()> {
        let mut payload = match record.as_object().cloned() {
            Some(map) => Value::Object(map),
            None => serde_json::json!({ "value": record }),
        };
        payload.as_object_mut().expect("a JSON object").insert(
            "session_id".to_string(),
            Value::String(self.session_id.clone()),
        );
        let text = format!("{payload}\n");
        let name = kind.file_name();
        let file = self
            .files
            .iter_mut()
            .find(|(opened, _)| *opened == name)
            .map(|(_, file)| file)
            .ok_or_else(|| PipelineError::Evidence {
                path: self.dir.join(name),
                detail: "this file was not opened by the session".to_string(),
            })?;
        file.write_all(text.as_bytes())
            .and_then(|_| file.flush())
            .map_err(|error| PipelineError::Evidence {
                path: self.dir.join(name),
                detail: format!("{error}"),
            })?;
        self.lines += 1;
        Ok(())
    }

    /// §49: the summary files are written whole, through a temporary name.
    ///
    /// A `metrics.json` that stops mid-object is worse than no file, because it
    /// reads like evidence about a completed run.
    pub fn write_whole(&mut self, name: &str, value: &Value) -> Result<()> {
        let path = self.dir.join(name);
        let temp = self.dir.join(format!("{name}.partial"));
        let text = serde_json::to_string_pretty(value).unwrap_or_else(|_| format!("{value}\n"));
        let write = (|| -> std::io::Result<()> {
            let mut file = File::create(&temp)?;
            file.write_all(text.as_bytes())?;
            file.write_all(b"\n")?;
            file.flush()?;
            file.sync_all()?;
            drop(file);
            std::fs::rename(&temp, &path)
        })();
        if let Err(error) = write {
            // A failed rename leaves no half-written target behind; the temp file
            // is removed so the directory does not accumulate debris.
            let _ = std::fs::remove_file(&temp);
            return Err(PipelineError::Evidence {
                path,
                detail: format!("{error}"),
            });
        }
        Ok(())
    }
}

/// §48's session record: the fields the task names, present even when the run has
/// nothing to say for them.
///
/// The order a reader asks for them in is not reproducible here — this workspace
/// serializes maps in key order, not insertion order — and that is the better
/// property: two runs of one input write the same bytes, which is what §33's
/// determinism claim is checked against. So this function guarantees *presence*
/// (a missing `end_block` reads as `null`, not as a schema that changed), and the
/// rest of the record — capability tables, queue policy, ledger counters — travels
/// under its own name rather than being dropped.
pub fn session_record(record: &Value) -> Value {
    let mut session = serde_json::Map::new();
    for key in [
        "session_id",
        "milestone",
        "chain_id",
        "source",
        "start_block",
        "end_block",
        "started_at_unix_ms",
        "ended_at_unix_ms",
        "ended_by",
        "blocks",
        "state_changes",
        "opportunities",
        "simulations",
        "risk_decisions",
    ] {
        session.insert(
            key.to_string(),
            record.get(key).cloned().unwrap_or(Value::Null),
        );
    }
    // Everything else — the capability tables, the queue policy, the ledger's own
    // counters — travels under a name that says it is context, not a missing field.
    if let Some(extra) = record.as_object() {
        for (key, value) in extra {
            if !session.contains_key(key) {
                session.insert(key.clone(), value.clone());
            }
        }
    }
    Value::Object(session)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("evm-m5-evidence-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn lines_append_and_carry_the_session_id() {
        let dir = temp_dir("append");
        {
            let mut writer = EvidenceWriter::open(&dir, "s1", false).expect("open");
            writer
                .line(EvidenceFile::Blocks, &json!({"number": 10}))
                .expect("line");
            writer
                .line(EvidenceFile::Blocks, &json!({"number": 11}))
                .expect("line");
            assert_eq!(writer.lines(), 2);
        }
        // Reopening the same session directory appends instead of truncating.
        {
            let mut writer = EvidenceWriter::open(&dir, "s2", false).expect("reopen");
            writer
                .line(EvidenceFile::Blocks, &json!({"number": 12}))
                .expect("line");
        }
        let text = std::fs::read_to_string(dir.join("blocks.jsonl")).expect("read");
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 3, "{text}");
        let first: Value = serde_json::from_str(lines[0]).expect("json");
        assert_eq!(first["session_id"], "s1");
        assert_eq!(first["number"], 10);
        let last: Value = serde_json::from_str(lines[2]).expect("json");
        assert_eq!(last["session_id"], "s2");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_whole_file_rename_leaves_no_partial_behind() {
        let dir = temp_dir("whole");
        let writer = EvidenceWriter::open(&dir, "s3", false).expect("open");
        drop(writer);
        // `write_whole` needs a live handle, so reopen mutably.
        let mut writer = EvidenceWriter::open(&dir, "s3", false).expect("reopen");
        writer
            .write_whole("metrics.json", &json!({"blocks": 4}))
            .expect("write");
        assert_eq!(
            std::fs::read_to_string(dir.join("metrics.json"))
                .expect("read")
                .trim()
                .lines()
                .count(),
            3,
            "pretty-printed object plus the closing line"
        );
        assert!(!dir.join("metrics.json.partial").exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_execution_files_belong_only_to_a_run_that_has_a_lane() {
        let without = temp_dir("lane-off");
        {
            let mut writer = EvidenceWriter::open(&without, "s5", false).expect("open");
            // Asking for a row the session cannot produce is the failure this
            // split exists to make loud.
            let error = writer
                .line(EvidenceFile::Executions, &json!({"execution_id": "x"}))
                .expect_err("no executions file was opened");
            assert!(error.to_string().contains("not opened"), "{error}");
            assert!(!without.join("executions.jsonl").exists());
            assert!(without.join("blocks.jsonl").exists());
        }
        let with = temp_dir("lane-on");
        {
            let mut writer = EvidenceWriter::open(&with, "s6", true).expect("open");
            writer
                .line(EvidenceFile::Executions, &json!({"execution_id": "x"}))
                .expect("line");
        }
        assert!(with.join("executions.jsonl").exists());
        std::fs::remove_dir_all(&without).ok();
        std::fs::remove_dir_all(&with).ok();
    }

    #[test]
    fn the_session_record_states_every_field_a_reader_asks_for() {
        let record = session_record(&json!({
            "session_id": "s4",
            "milestone": "M6",
            "chain_id": 91342,
            "source": "websocket",
            "end_block": 12,
            "capability": {"eth_subscribe": "BLOCKED"},
        }));
        // §48's list, present even where this run had nothing to say.
        for key in [
            "session_id",
            "chain_id",
            "start_block",
            "end_block",
            "started_at_unix_ms",
            "ended_at_unix_ms",
            "source",
            "milestone",
        ] {
            assert!(
                record.as_object().expect("object").contains_key(key),
                "{key} is missing from the session record"
            );
        }
        assert_eq!(record["end_block"], 12);
        assert_eq!(record["source"], "websocket");
        assert_eq!(
            record["milestone"], "M6",
            "which milestone a run belongs to is a fact about the run, so it travels from \
             the record the runner wrote rather than from this file's prose"
        );
        assert_eq!(
            record["start_block"],
            Value::Null,
            "a field with no value is stated as null, not left for the reader to \
             wonder whether the schema changed"
        );
        assert_eq!(record["capability"]["eth_subscribe"], "BLOCKED");
        assert_eq!(record["blocks"], Value::Null, "an unrun block");
        // The keys come out in sorted order, which is what lets two runs of one
        // input be compared as bytes rather than as intentions.
        let keys: Vec<&String> = record.as_object().expect("object").keys().collect();
        let mut sorted = keys.clone();
        sorted.sort();
        assert_eq!(keys, sorted);
    }
}
