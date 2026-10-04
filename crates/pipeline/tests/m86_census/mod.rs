#![allow(dead_code)]
//! M8.6 — the RPC reduction census model (`docs/v0.1/M8.6 Coding.md`).
//!
//! # What this file is
//!
//! M8.6 asks one question of every RPC the production hot path actually makes (§1): is there a
//! reduction opportunity that leaves the system's semantics untouched. This module holds the
//! *judgement* half of that answer — what each recorded call site means, what decision consumes
//! it, whether an earlier answer could serve it, and what would have to exist first — and the pure
//! arithmetic that turns the committed records into the eleven evidence tables. It holds no
//! measurement of its own: every count is folded out of `data/evidence/m8/cross-stage/runs/*/`
//! by [`group_rows`], every duplicate shape out of `reuse-candidates.json`, and every owner,
//! authority, freshness and invalidation label out of M8.4.3's ownership matrix by inheritance
//! rather than retyping.
//!
//! # Why it lives under `tests/`
//!
//! §30 prefers zero production changes over a diagnostic module in `crates/*/src`, and this
//! milestone changes no behaviour, so the model is test-only shared code, opened by
//! `mod m86_census;` from both `rpc_reduction_evidence.rs` (assembly) and
//! `rpc_reduction_recompute.rs` (recomputation and negative controls). The precedent is
//! `crates/opportunity/tests/support/mod.rs`.
//!
//! # The three layers stay three layers (§2.2)
//!
//! A pair of asks being one ask ([`crate::m86_census`'s duplicate classes, inherited from
//! M8.4.2](#vocabulary)) is `physical duplicate`. A candidate's answer possibly serving another
//! consumer is `reusable_in_principle`. Nothing is `safe_to_reuse_now` unless this file can name
//! the proof, and [`candidate_invariant_errors`] refuses any table that credits a saving it cannot
//! trace to that proof. The two functions that do the separating are [`safe_saving_of`] (which
//! returns 0 unless every gate condition is met) and [`priority_of`] (which follows §14's five
//! rules as a decision list, so no label is ever typed by hand).

use std::collections::BTreeMap;

use serde_json::{json, Value};

use evm_pipeline::canonicalization::{read_category, RpcReadKey};

/// The second half of the model: turning this file's judgements and the committed records into the
/// eleven evidence files. It is a submodule rather than a section of this file because the assembly
/// is the only part that touches the filesystem, and §30's "no production module" preference is
/// easiest to honour when the writing is in one place a gate can point at.
pub mod tables;

// ---------------------------------------------------------------------------
// paths
// ---------------------------------------------------------------------------

/// §34's output tree. `rpc-census.json` is the first table §18 asks for; §19's matrix, §20's
/// rejections, §21's queue, §10's information flow, §7's five saving measures, §25's negative
/// controls, §4's surface and §32's verdict follow.
pub const EVIDENCE_DIR: &str = "data/evidence/m8/m8.6";

/// The ten JSON tables, in the order §18–§32 introduce them. `README.md` is written beside them
/// and is named separately, as in every other milestone of this tree.
pub const CENSUS_TABLES: [&str; 10] = [
    "rpc_surface.json",
    "rpc-census.json",
    "rpc-reduction-candidates.json",
    "reduction-matrix.json",
    "rejected-opportunities.json",
    "priority-queue.json",
    "information_flow.json",
    "saving-kinds.json",
    "negative-controls.json",
    "final-verdict.json",
];

pub const README_FILE: &str = "README.md";

/// §31/§32's corpus: the same three live BuildOnly route runs M8.4.2 and M8.5.1 measured. Opening
/// a socket would be the fastest way to fill these tables and the one way to break §2.4, so the
/// census reads these records and nothing else.
pub const RUNS_DIR: &str = "data/evidence/m8/cross-stage/runs";
pub const CALLS_FILE: &str = "pipeline-calls.json";
/// The per-run summary, which is where this corpus publishes a run's own identity: chain id,
/// pinned height, endpoint digest, execution mode and git revision. The call rows carry none of
/// those, so a census that keyed a run by itself would have to invent them.
pub const SUMMARY_FILE: &str = "pipeline-summary.json";
pub const BOTTLENECK_FILE: &str = "bottleneck-classification.json";
pub const CANDIDATES_RECORD: &str = "data/evidence/m8/cross-stage/reuse-candidates.json";
pub const SUMMARY_RECORD: &str = "data/evidence/m8/cross-stage/duplicate-summary.json";
pub const OWNERSHIP_RECORD: &str = "data/evidence/m8/state-ownership/ownership-matrix.json";
pub const PROPAGATION_RECORD: &str = "data/evidence/m8/m8.4.4/summary.json";
pub const ETH_CALL_RECORD: &str = "data/evidence/m8/m8.5.1/reuse-verdicts.json";

/// The three files that make this tree: the model you are reading, the assembly, and the
/// recomputation gate. The census is test-only by §30's preference, so the model's own path names
/// `tests/`, unlike M8.5.1's `eth_call_semantics.rs`, which is a production module.
pub const MODEL_FILE: &str = "crates/pipeline/tests/m86_census/mod.rs";
pub const ASSEMBLY_FILE: &str = "crates/pipeline/tests/rpc_reduction_evidence.rs";
pub const RECOMPUTE_FILE: &str = "crates/pipeline/tests/rpc_reduction_recompute.rs";

/// Naming a directory would invite a hand-edited table. Its presence is the instruction to copy a
/// fresh assembly over the committed evidence, which is the only way these ten files change.
pub const REFRESH_ENV: &str = "M86_CENSUS_REFRESH";

pub const MILESTONE: &str = "M8.6";
pub const GENERATED_BY: &str = ASSEMBLY_FILE;

// ---------------------------------------------------------------------------
// vocabulary: §5's semantic classes
// ---------------------------------------------------------------------------

/// §5. A read of canonical state at a named height.
pub const CLASS_CANONICAL_STATE_READ: &str = "A_CANONICAL_STATE_READ";
/// §5. The header: which block is canonical, and what it says about fees and time.
pub const CLASS_BLOCK_CONTEXT: &str = "B_CANONICAL_BLOCK_CONTEXT";
/// §5. A protocol oracle answering a priced question about a transaction.
pub const CLASS_PROTOCOL_ORACLE: &str = "C_PROTOCOL_ORACLE";
/// §5. A read whose purpose is to check something another stage claimed.
pub const CLASS_EXECUTION_VERIFICATION: &str = "D_EXECUTION_VERIFICATION";
/// §5. A read that prepares a transaction: nonce, fee, balance, asset.
pub const CLASS_EXECUTION_PREPARATION: &str = "E_EXECUTION_PREPARATION";
/// §5. Which chain, which connection, which endpoint.
pub const CLASS_IDENTITY_CONNECTION: &str = "F_IDENTITY_CONNECTION";
/// §5. A read made to describe what happened rather than to decide anything.
pub const CLASS_OBSERVATION: &str = "G_OBSERVATION";
/// §5. A settlement read. Named for completeness: no row of this census is one (§4).
pub const CLASS_RECEIPT_SETTLEMENT: &str = "H_RECEIPT_SETTLEMENT";
/// §5. The case §5's eight other classes do not describe.
pub const CLASS_UNKNOWN: &str = "I_UNKNOWN";

pub const SEMANTIC_CLASSES: [&str; 9] = [
    CLASS_CANONICAL_STATE_READ,
    CLASS_BLOCK_CONTEXT,
    CLASS_PROTOCOL_ORACLE,
    CLASS_EXECUTION_VERIFICATION,
    CLASS_EXECUTION_PREPARATION,
    CLASS_IDENTITY_CONNECTION,
    CLASS_OBSERVATION,
    CLASS_RECEIPT_SETTLEMENT,
    CLASS_UNKNOWN,
];

/// §5's closing rule: the class follows the consumer's role, and the method name alone is not
/// evidence. Each census row carries the sentence saying which consumer put it in its class.
pub const CLASS_RULE: &str =
    "a site's class is decided by the decision its answer feeds, not by its method name";

/// M8.4.2's own six categories map onto §5's nine, and the mapping is published rather than left
/// implicit: two vocabularies describing one row must be reconcilable or the census is noise.
pub fn class_from_m842_category(
    category: &str,
    method: &str,
    consumer_is_check: bool,
) -> &'static str {
    match category {
        "state_read" => CLASS_CANONICAL_STATE_READ,
        "block_read" if method == "eth_blockNumber" => CLASS_BLOCK_CONTEXT,
        "block_read" if consumer_is_check => CLASS_BLOCK_CONTEXT,
        "block_read" => CLASS_BLOCK_CONTEXT,
        "chain_identity" => CLASS_IDENTITY_CONNECTION,
        "simulation_call" if consumer_is_check => CLASS_EXECUTION_VERIFICATION,
        "simulation_call" => CLASS_CANONICAL_STATE_READ,
        "transaction_preparation" => CLASS_EXECUTION_PREPARATION,
        _ => CLASS_UNKNOWN,
    }
}

// ---------------------------------------------------------------------------
// vocabulary: §11's verification roles, §12's alternatives, §14's priorities, §7's measures
// ---------------------------------------------------------------------------

pub const ROLE_NONE: &str = "NONE";
pub const ROLE_INFORMATION_ONLY: &str = "INFORMATION_ONLY";
pub const ROLE_SAFETY_GATE: &str = "SAFETY_GATE";
pub const ROLE_INDEPENDENT_RECHECK: &str = "INDEPENDENT_RECHECK";
pub const ROLE_FINAL_EXECUTION_GUARD: &str = "FINAL_EXECUTION_GUARD";
pub const ROLE_SETTLEMENT_PROOF: &str = "SETTLEMENT_PROOF";

pub const VERIFICATION_ROLES: [&str; 6] = [
    ROLE_NONE,
    ROLE_INFORMATION_ONLY,
    ROLE_SAFETY_GATE,
    ROLE_INDEPENDENT_RECHECK,
    ROLE_FINAL_EXECUTION_GUARD,
    ROLE_SETTLEMENT_PROOF,
];

/// §11's default: a read that *is* a check cannot be served from another stage's copy of the
/// answer without deleting the check. These are the three roles that carry that default.
pub const CHECK_ROLES: [&str; 3] = [
    ROLE_SAFETY_GATE,
    ROLE_INDEPENDENT_RECHECK,
    ROLE_FINAL_EXECUTION_GUARD,
];

pub fn is_check_role(role: &str) -> bool {
    CHECK_ROLES.contains(&role)
}

/// §12. Another check, already in the build, would still answer the question.
pub const ALT_EXISTING_OTHER_CHECK: &str = "EXISTING_OTHER_CHECK";
/// §12. The reduction is only possible alongside a mechanism that does not exist yet.
pub const ALT_NEW_CHECK_REQUIRED: &str = "NEW_CHECK_REQUIRED";
/// §12. Nothing else would answer the question, so §12 blocks the reduction.
pub const ALT_NO_ALTERNATIVE: &str = "NO_ALTERNATIVE";
/// §12. The read carries no verification, so §12 asks nothing of it.
pub const ALT_NOT_APPLICABLE: &str = "NOT_APPLICABLE";

pub const ALTERNATIVES: [&str; 4] = [
    ALT_EXISTING_OTHER_CHECK,
    ALT_NEW_CHECK_REQUIRED,
    ALT_NO_ALTERNATIVE,
    ALT_NOT_APPLICABLE,
];

/// §14's five labels. `P0` needs five conditions at once; `REJECT` is §14's own list including
/// "independent verification" and "block-sensitive without safe freshness".
pub const PRIORITY_P0: &str = "P0";
pub const PRIORITY_P1: &str = "P1";
pub const PRIORITY_P2: &str = "P2";
pub const PRIORITY_P3: &str = "P3";
pub const PRIORITY_REJECT: &str = "REJECT";

pub const PRIORITIES: [&str; 5] = [
    PRIORITY_P0,
    PRIORITY_P1,
    PRIORITY_P2,
    PRIORITY_P3,
    PRIORITY_REJECT,
];

/// §18 asks the census table — one row per *call site*, 44 of them — for a `priority` column, and
/// §14's five labels rank *candidates*, of which this corpus has 18. The remaining 26 sites are
/// named by no candidate: nothing proposes removing them, so §14 has made no judgement about them
/// and a label borrowed from the candidate ranking would be a judgement the census invented. The
/// row says which candidate set it was ranked over instead, and [`census_invariant_errors`] accepts
/// exactly these six strings.
pub const PRIORITY_NOT_A_CANDIDATE: &str = "no_candidate_names_this_site";

pub const CENSUS_PRIORITIES: [&str; 6] = [
    PRIORITY_P0,
    PRIORITY_P1,
    PRIORITY_P2,
    PRIORITY_P3,
    PRIORITY_REJECT,
    PRIORITY_NOT_A_CANDIDATE,
];

/// §7's five measures, kept as five fields for the whole milestone. Naming them together is the
/// point: M8.3.3's wall-time result and M8.4.2's duplicate count are not the same quantity, and a
/// table that folds them is how a parallelism result gets reported as an RPC saving.
pub const SAVING_KINDS: [&str; 5] = [
    "physical_duplicate_saving",
    "semantic_reuse_saving",
    "verification_removal_saving",
    "parallel_wall_time_saving",
    "total_safe_rpc_saving",
];

/// How far a candidate's own evidence reaches. §24's gates use this to keep the priority derived:
/// `proven` is a milestone's published verdict, `code_derived` is a decision line in this
/// repository's source, `record_only` is a figure with no code behind it, `absent` is a question
/// no committed record answers.
pub const EVIDENCE_PROVEN: &str = "proven";
pub const EVIDENCE_CODE_DERIVED: &str = "code_derived";
pub const EVIDENCE_RECORD_ONLY: &str = "record_only";
pub const EVIDENCE_ABSENT: &str = "absent";

pub const EVIDENCE_STRENGTHS: [&str; 4] = [
    EVIDENCE_PROVEN,
    EVIDENCE_CODE_DERIVED,
    EVIDENCE_RECORD_ONLY,
    EVIDENCE_ABSENT,
];

/// §8's five patterns plus §9/§10's sixth, which the task book describes in prose rather than as a
/// letter: a read whose answer is computable from information the run already holds.
pub const PATTERN_A: &str = "A_physical_duplicate";
pub const PATTERN_B: &str = "B_cross_stage_canonical_read";
pub const PATTERN_C: &str = "C_block_context_propagation";
pub const PATTERN_D: &str = "D_protocol_oracle";
pub const PATTERN_E: &str = "E_preparation_read";
pub const PATTERN_F: &str = "F_information_already_held";

pub const PATTERNS: [&str; 6] = [
    PATTERN_A, PATTERN_B, PATTERN_C, PATTERN_D, PATTERN_E, PATTERN_F,
];

/// §22's three allowed answers, and §17's two escape hatches for a question this stage may not
/// spend an RPC on.
pub const VERDICT_NONE_FOUND: &str = "NO_SAFE_RPC_REDUCTION_FOUND";
pub const VERDICT_CANDIDATE_FOUND: &str = "SAFE_RPC_REDUCTION_CANDIDATE_FOUND";
pub const VERDICT_NOT_ENOUGH: &str = "NOT_ENOUGH_EVIDENCE";
pub const NEXT_STEP_EXPERIMENT: &str = "EXPERIMENT_REQUIRED";
pub const NEXT_STEP_NONE: &str = "none";

/// §24's sixth figure: the only kinds of proof that can put a number in `safe_saving`. The list
/// exists so that `safe_saving` cannot be raised by typing: [`PROOF_NONE`] is what every candidate
/// in this census carries, and [`candidate_invariant_errors`] refuses a non-zero saving that
/// claims anything else. A future milestone that earns a saving adds one of the two real kinds and
/// points it at the record rows that carry it.
pub const PROOF_NONE: &str = "none";
/// M8.4.2's five reuse conditions, all of them resolved `checked_met` on every pair of the
/// candidate. Nothing in the committed corpus reaches this state — the measured count is 0 of 78.
pub const PROOF_ALL_CONDITIONS_MET: &str = "every_reuse_condition_met_in_the_record";
/// The record's own `safe_to_reuse` field set true on every pair of the candidate, which is the
/// field M8.4.2 published and which reads false on all 78 rows.
pub const PROOF_RECORD_MARKS_SAFE: &str = "the_record_itself_marks_the_pair_safe_to_reuse";

pub const PROOFS: [&str; 3] = [
    PROOF_NONE,
    PROOF_ALL_CONDITIONS_MET,
    PROOF_RECORD_MARKS_SAFE,
];

/// The proofs that credit a saving: [`PROOF_NONE`] is the absence of one, so it is deliberately
/// missing from this list even though it is a legal value of the field.
pub const CREDITING_PROOFS: [&str; 2] = [PROOF_ALL_CONDITIONS_MET, PROOF_RECORD_MARKS_SAFE];

// ---------------------------------------------------------------------------
// §9/§10's freshness verdicts
// ---------------------------------------------------------------------------

/// Serving the consumer from the producer's answer keeps what the consumer requires fresh.
pub const FRESHNESS_PRESERVED: &str = "preserved";
/// The producer's answer is known to be stale for the consumer — a tag, or a different block.
pub const FRESHNESS_NOT_PRESERVED: &str = "not_preserved";
/// Nothing measured here says either way, which §9 reads as a block, not as a pass.
pub const FRESHNESS_UNPROVEN: &str = "unproven";

pub const FRESHNESS_VERDICTS: [&str; 3] = [
    FRESHNESS_PRESERVED,
    FRESHNESS_NOT_PRESERVED,
    FRESHNESS_UNPROVEN,
];

// ---------------------------------------------------------------------------
// state kinds, inherited from M8.4.3 rather than renamed
// ---------------------------------------------------------------------------

/// M8.4.3's eleven categories, spelled exactly as `ownership-matrix.json` spells them, so the
/// census's `owner`, `authority`, `freshness` and `invalidation` columns are looked up (§24's
/// "可回算") instead of being typed a second time in a second vocabulary.
pub const STATE_POOL_RESERVES: &str = "pool_reserves";
pub const STATE_CONTRACT_CODE: &str = "contract_code";
pub const STATE_STORAGE_SLOT: &str = "storage_slot";
pub const STATE_NATIVE_BALANCE: &str = "native_balance";
pub const STATE_NONCE: &str = "nonce";
pub const STATE_FEE_PARAMETERS: &str = "fee_parameters";
pub const STATE_BLOCK_HEADER: &str = "block_header";
pub const STATE_CHAIN_IDENTITY: &str = "chain_identity";
pub const STATE_ETH_CALL_RESULT: &str = "eth_call_result";

// ---------------------------------------------------------------------------
// carriers: §10's "does anything already hold this across a stage boundary"
// ---------------------------------------------------------------------------

/// Nothing in the build holds the answer past the call that read it: each later consumer asks the
/// node again. This is the default, and §19 counts how many candidates need one of these first.
pub const CARRIER_NONE: &str = "no_carrier_in_code";
/// `HttpChainAdapter`'s own field, written at connect and read by every decode afterwards.
pub const CARRIER_ADAPTER_FIELD: &str = "chain_adapter_field";
/// M8.3.1's cache: real, measured, and scoped to one simulation by its own documentation.
pub const CARRIER_SIMULATION_CACHE: &str = "state_read_cache_within_one_simulation";
/// The pin: a height and a hash that travel from the observation stage into the run's plan.
pub const CARRIER_BLOCK_PIN: &str = "block_pin_height_and_hash";
/// M8.4.4's contract: produced only by a verified read, consumed only by a consumer that verifies.
pub const CARRIER_VERIFIED_BLOCK_CONTEXT: &str = "verified_block_context_preflight_to_build";
/// One stage's own fact-gathering, which is a scope, not a carrier across a boundary.
pub const CARRIER_STAGE_FACTS: &str = "preflight_facts_within_one_stage";
/// The transaction being built holds its own fee fields; that is the intent, not a shared cache.
pub const CARRIER_INTENT: &str = "execution_intent_being_built";
/// The report written after the sequence: an audit trail, never a decision input.
pub const CARRIER_REPORT: &str = "execution_sequence_report_after_the_fact";

pub const CARRIERS: [&str; 8] = [
    CARRIER_NONE,
    CARRIER_ADAPTER_FIELD,
    CARRIER_SIMULATION_CACHE,
    CARRIER_BLOCK_PIN,
    CARRIER_VERIFIED_BLOCK_CONTEXT,
    CARRIER_STAGE_FACTS,
    CARRIER_INTENT,
    CARRIER_REPORT,
];

// ---------------------------------------------------------------------------
// §4's rule that an uncalled method is not a hot-path RPC
// ---------------------------------------------------------------------------

/// Methods a classification rule in this repository mentions and the production hot path never
/// calls. They are listed in the surface table under `not_in_the_records` so that a later agent
/// cannot "find" them as savings: §4 forbids writing an RPC with no actual call site as a
/// hot-path RPC, and §5's examples name `eth_gasPrice` and `eth_estimateGas` as if they were.
pub const CLASSIFIED_BUT_UNCALLED_METHODS: [&str; 2] = ["eth_gasPrice", "eth_estimateGas"];

/// Methods this repository can call and these three runs never did — send, receipt, logs, block by
/// hash. Listed the same way: known to the code, absent from the census, so the census's totals
/// cannot quietly include a submission or a receipt read.
pub const CODED_BUT_UNTRACED_METHODS: [&str; 6] = [
    "eth_getBlockByHash",
    "eth_getBlockReceipts",
    "eth_getLogs",
    "eth_getTransactionReceipt",
    "eth_sendRawTransaction",
    "eth_subscribe",
];

// ---------------------------------------------------------------------------
// §4's caller families: how a free-text label becomes a census key
// ---------------------------------------------------------------------------

/// The label a row carries when nothing was stamped at the throat of the call.
pub const UNSTAMPED_CALLER: &str = "<unstamped>";
/// A byte-mask, not a merge: the record's addresses differ per call, the site does not.
pub const MASKED_ADDRESS: &str = "<addr>";
/// A byte-mask for the step index and the nonce a step carried, both of which count the same site.
pub const MASKED_INDEX: &str = "N";
/// The shortest hex run `caller_family` treats as an address. Under 8, a short hex tail (`0dfe1681`
/// in a selector) is data the label keeps, because it names which call the step made.
pub const MIN_MASKED_HEX: usize = 8;

fn leading_digits_len(text: &str) -> usize {
    text.find(|c: char| !c.is_ascii_digit())
        .unwrap_or(text.len())
}

fn leading_hex_len(text: &str) -> usize {
    text.find(|c: char| !c.is_ascii_hexdigit())
        .unwrap_or(text.len())
}

/// The census key is `(method, stage, caller_family)`, and §4's `caller` column has to survive
/// being counted across three runs, which the literal label does not: a REVM step's label carries
/// the step number, the nonce it sent, the three addresses the EVM touched, and a four-byte
/// selector. So the family masks exactly three things — `step <digits>`, `nonce <digits>`, and a
/// `0x` literal of at least [`MIN_MASKED_HEX`] hex digits — and keeps everything else, including
/// the selector text, because `views: token0()` and `views: token1()` are two sites and the census
/// must not merge them into one.
///
/// This is a *presentation* key. It is never an identity: §8's identity terms come from the
/// recorded `dedup_key` through [`RpcReadKey`], and a row whose label this function cannot fold
/// stays its own group and fails the surface gate rather than joining a neighbour.
pub fn caller_family(caller: Option<&str>) -> String {
    let Some(text) = caller else {
        return UNSTAMPED_CALLER.to_string();
    };
    let mut out = String::with_capacity(text.len());
    let mut index = 0usize;
    while index < text.len() {
        let tail = &text[index..];
        let mut masked = false;
        for (prefix, replacement) in [("step ", MASKED_INDEX), ("nonce ", MASKED_INDEX)] {
            if let Some(after) = tail.strip_prefix(prefix) {
                let digits = leading_digits_len(after);
                if digits > 0 {
                    out.push_str(prefix);
                    out.push_str(replacement);
                    index += prefix.len() + digits;
                    masked = true;
                    break;
                }
            }
        }
        if masked {
            continue;
        }
        if let Some(after) = tail.strip_prefix("0x") {
            let hex = leading_hex_len(after);
            if hex >= MIN_MASKED_HEX {
                out.push_str(MASKED_ADDRESS);
                index += 2 + hex;
                continue;
            }
        }
        if let Some(ch) = tail.chars().next() {
            out.push(ch);
            index += ch.len_utf8();
        }
    }
    out
}

/// §4's row key as the model sees it: the triple a census line is identified by. A stage of `None`
/// is the connect-time read, which has no stamp to name it.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct SiteKey {
    pub method: String,
    pub stage: Option<String>,
    pub caller_family: String,
}

impl SiteKey {
    pub fn of_row(row: &Value) -> Self {
        Self {
            method: row["method"].as_str().unwrap_or_default().to_string(),
            stage: row["stage"].as_str().map(str::to_owned),
            caller_family: caller_family(row["caller"].as_str()),
        }
    }

    pub fn label(&self) -> String {
        format!(
            "{}|{}|{}",
            self.method,
            self.stage
                .clone()
                .unwrap_or_else(|| "<unstamped>".to_string()),
            self.caller_family
        )
    }
}

// ---------------------------------------------------------------------------
// the site table: what each recorded call means and who consumes it
// ---------------------------------------------------------------------------

/// One production call site as the census sees it. Three fields are *measured* (they are folded
/// out of the records by [`group_rows`]); the rest are this milestone's judgement, and each
/// judgement field carries the code line it was read from so §24's "不允许手工把 candidate 改绿"
/// has something to check against.
pub struct Site {
    /// A stable slug, unique within [`SITES`], used by [`CANDIDATES`] to point at a consumer.
    pub id: &'static str,
    pub method: &'static str,
    pub stage: Option<&'static str>,
    pub caller_family: &'static str,
    /// M8.4.3's category, so `owner`/`authority`/`freshness`/`invalidation` resolve by lookup.
    pub state_kind: &'static str,
    /// §5.
    pub semantic_class: &'static str,
    /// §5's closing rule, answered in one sentence: which consumer put this site in its class.
    pub class_basis: &'static str,
    /// §11.
    pub verification_role: &'static str,
    /// The decision line that made it one, as `file:line` prose with the code's own words.
    pub role_evidence: &'static str,
    /// §12.
    pub alternative_verification: &'static str,
    pub alternative_evidence: &'static str,
    /// §10.
    pub carrier: &'static str,
    pub carrier_evidence: &'static str,
    /// What the record says about height: a number, a tag, or no block term at all.
    pub block_semantics: &'static str,
    /// §4's `consumer` column in one sentence: what the code does with the answer.
    pub consumer: &'static str,
    /// Where the caller label is written, so the surface table can publish a real line number.
    pub stamp: (&'static str, &'static str),
    /// The first decision the answer reaches, likewise.
    pub decision: (&'static str, &'static str),
}

impl Site {
    pub fn key(&self) -> SiteKey {
        SiteKey {
            method: self.method.to_string(),
            stage: self.stage.map(str::to_owned),
            caller_family: self.caller_family.to_string(),
        }
    }

    const BASE: Site = Site {
        id: "",
        method: "",
        stage: None,
        caller_family: "",
        state_kind: "",
        semantic_class: CLASS_UNKNOWN,
        class_basis: "",
        verification_role: ROLE_NONE,
        role_evidence: "",
        alternative_verification: ALT_NOT_APPLICABLE,
        alternative_evidence: "",
        carrier: CARRIER_NONE,
        carrier_evidence: "",
        block_semantics: "",
        consumer: "",
        stamp: ("", ""),
        decision: ("", ""),
    };
}

const BLOCK_NUMBER_HEAD: &str = "head and header that fixed the pin";
const CALLER_UNSTAMPED: &str = UNSTAMPED_CALLER;

/// Every site the three committed runs recorded, in the `(method, stage, caller_family)` order the
/// census table publishes them in. 44 rows, matching [`group_rows`]' 44 groups exactly: the
/// assembly refuses an unassigned group and refuses an unused site, so this table cannot drift
/// behind the records without failing.
pub const SITES: [Site; 44] = [
    // -- the height and the header ------------------------------------------------------
    Site {
        id: "observation.eth_blockNumber.pin",
        method: "eth_blockNumber",
        stage: Some("observation"),
        caller_family: BLOCK_NUMBER_HEAD,
        state_kind: STATE_BLOCK_HEADER,
        semantic_class: CLASS_BLOCK_CONTEXT,
        class_basis: "its consumer is the pin: the answer becomes the height every later read in \
            the run must name, which is §5's block context by role rather than by method",
        verification_role: ROLE_INFORMATION_ONLY,
        role_evidence: "crates/pipeline/src/arbitrage.rs: the answer is `head`, and the first \
            comparison it meets is `BlockPin::new(head, header.hash)` downstream at the \
            simulation's `check_pin`, which verifies the hash this call never returned",
        alternative_verification: ALT_NOT_APPLICABLE,
        carrier: CARRIER_BLOCK_PIN,
        carrier_evidence: "the height it learns travels as the run's pin; the hash leg comes from \
            the header read that follows it",
        block_semantics: "names no height: the ask is the head at the instant of asking",
        consumer: "fixes the pin every later read has to name",
        stamp: ("crates/pipeline/src/arbitrage.rs", "head and header that fixed the pin"),
        decision: ("crates/pipeline/src/arbitrage.rs", "BlockPin::new(head"),
        ..Site::BASE
    },
    Site {
        id: "observation.eth_getBlockByNumber.pin",
        method: "eth_getBlockByNumber",
        stage: Some("observation"),
        caller_family: BLOCK_NUMBER_HEAD,
        state_kind: STATE_BLOCK_HEADER,
        semantic_class: CLASS_BLOCK_CONTEXT,
        class_basis: "the header is the canonical block, and this is the read that says which one",
        verification_role: ROLE_SAFETY_GATE,
        role_evidence: "crates/pipeline/src/arbitrage.rs: `header.hash` is the pin's identity leg; \
            `check_pin` refuses a simulation whose own header read disagrees, so the hash is a \
            check's operand and not a value passed along",
        alternative_verification: ALT_NO_ALTERNATIVE,
        alternative_evidence: "this is the read that says WHICH block the run pinned: the gate's \
            `check_pin` compares a consumer's own header read against this one's hash, so no \
            committed check binds height to hash if it goes",
        carrier: CARRIER_BLOCK_PIN,
        carrier_evidence: "height and hash are read out of this one header, which is why the \
            producer of a block context never mixes two calls",
        block_semantics: "numbered height, resolved from the height the run just fixed",
        consumer: "the header that fixes the pin, and the base fee and env the run prices from",
        stamp: ("crates/pipeline/src/arbitrage.rs", "head and header that fixed the pin"),
        decision: ("crates/simulation/src/engine.rs", "claimed != found"),
    },
    // -- opportunity detection's pricing reads ------------------------------------------
    Site {
        id: "opportunity_detection.eth_call.reserves",
        method: "eth_call",
        stage: Some("opportunity_detection"),
        caller_family: "reserves of both venues, at the pin",
        state_kind: STATE_POOL_RESERVES,
        semantic_class: CLASS_CANONICAL_STATE_READ,
        class_basis: "detection prices a route from reserves: it reads state to build a claim, and \
            the checks come later and elsewhere (§5's note that the same method splits by caller)",
        verification_role: ROLE_INFORMATION_ONLY,
        role_evidence: "crates/pipeline/src/arbitrage.rs: the reserves become `Venue` quotes and a \
            `RouteLeg`, and the run's first refusal is a zero mid, not a comparison against an \
            earlier answer — this site is the *claimed side* that two later reads check",
        carrier: CARRIER_REPORT,
        carrier_evidence: "the claim travels inside the opportunity and is compared against, never \
            served from: `Repricing::expected_output` and the simulation's `check_reserves` both \
            re-read what this site read",
        block_semantics: "numbered height at the pin",
        consumer: "quotes both venues and writes the reserves the run later re-reads",
        stamp: ("crates/pipeline/src/arbitrage.rs", "reserves of both venues, at the pin"),
        decision: ("crates/simulation/src/engine.rs", "claimed != found"),
        ..Site::BASE
    },
    // -- preflight's fact gathering -----------------------------------------------------
    Site {
        id: "preflight.eth_call.l1_fee",
        method: "eth_call",
        stage: Some("preflight"),
        caller_family: "cost ceiling per step",
        state_kind: STATE_ETH_CALL_RESULT,
        semantic_class: CLASS_PROTOCOL_ORACLE,
        class_basis: "the sequencer's own fee oracle answering a question about a transaction, \
            which is §5's protocol oracle by name",
        verification_role: ROLE_SAFETY_GATE,
        role_evidence: "crates/execution/src/preflight.rs: `fn l1_line` turns an unreadable oracle \
            into a failed `L1FeeEstimate` line rather than a smaller bill, and §35 forbids reading \
            a bill without the L1 leg as the whole bill",
        alternative_verification: ALT_NEW_CHECK_REQUIRED,
        alternative_evidence: "nothing committed bears this duty today: the receipt's own `l1Fee` \
            settles the figure only after the transaction exists, so a reduction here would need a \
            NEW check (a derivation the census cannot yet prove) before the read could go — which is \
            §17's Candidate 1 as `EXPERIMENT_REQUIRED`, not a blocked opportunity",
        carrier: CARRIER_STAGE_FACTS,
        block_semantics: "numbered height at the head, per step",
        consumer: "the L1 half of the sequence ceiling the balance and profit lines spend",
        stamp: ("crates/execution/src/giwa/preflight_facts.rs", "cost ceiling per step"),
        decision: ("crates/execution/src/preflight.rs", "fn l1_line"),
        ..Site::BASE
    },
    Site {
        id: "preflight.eth_call.leg0",
        method: "eth_call",
        stage: Some("preflight"),
        caller_family: "reserves of leg 0",
        state_kind: STATE_POOL_RESERVES,
        semantic_class: CLASS_EXECUTION_VERIFICATION,
        class_basis: "the same method as detection's pricing read, and a different site: this \
            answer's consumer is the difference between two readings",
        verification_role: ROLE_INDEPENDENT_RECHECK,
        role_evidence: "crates/execution/src/preflight.rs: `expected_output` refuses when the \
            head's reserves are missing (`not re-read at the head`) rather than falling back to \
            the finding's numbers, so this read is the second opinion of the detection claim",
        alternative_verification: ALT_NO_ALTERNATIVE,
        alternative_evidence: "the reserve line is a difference between the priced route and the \
            head; feeding it the priced side's own value leaves the gate comparing a number with a \
            copy of itself",
        block_semantics: "numbered height at the head",
        consumer: "the head's own first-leg reserves, re-priced against the finding",
        stamp: ("crates/execution/src/giwa/preflight_facts.rs", "reserves of leg 0"),
        decision: ("crates/execution/src/preflight.rs", "expected_output(&facts.reserves"),
        ..Site::BASE
    },
    Site {
        id: "preflight.eth_call.leg1",
        method: "eth_call",
        stage: Some("preflight"),
        caller_family: "reserves of leg 1",
        state_kind: STATE_POOL_RESERVES,
        semantic_class: CLASS_EXECUTION_VERIFICATION,
        class_basis: "the second half of the same comparison as leg 0",
        verification_role: ROLE_INDEPENDENT_RECHECK,
        role_evidence: "crates/execution/src/preflight.rs: `expected_output` needs both legs at the \
            head, and §27's verdict cannot be formed from one side",
        alternative_verification: ALT_NO_ALTERNATIVE,
        block_semantics: "numbered height at the head",
        consumer: "the head's own second-leg reserves",
        stamp: ("crates/execution/src/giwa/preflight_facts.rs", "reserves of leg 1"),
        decision: ("crates/execution/src/preflight.rs", "expected_output(&facts.reserves"),
        ..Site::BASE
    },
    Site {
        id: "preflight.eth_getBlockByNumber.head_latest",
        method: "eth_getBlockByNumber",
        stage: Some("preflight"),
        caller_family: "head at latest",
        state_kind: STATE_BLOCK_HEADER,
        semantic_class: CLASS_BLOCK_CONTEXT,
        class_basis: "the head is a block context, and the checks that use it are staleness checks",
        verification_role: ROLE_SAFETY_GATE,
        role_evidence: "crates/execution/src/preflight.rs: a head below the intent's block is a \
            `StaleOpportunity` refusal, and the same answer is the height the fee, balance and \
            binding legs are gathered at",
        alternative_verification: ALT_NO_ALTERNATIVE,
        carrier: CARRIER_STAGE_FACTS,
        carrier_evidence: "number and hash come out of this one header, which is what makes it a \
            `VerifiedBlockContext` producer rather than a height to be filled in",
        block_semantics: "a tag: `latest`, deliberately asked for now",
        consumer: "the head the staleness, fee and chain legs are all measured against",
        stamp: ("crates/execution/src/giwa/preflight_facts.rs", "head at latest"),
        decision: ("crates/execution/src/block_context.rs", "two calls can straddle a block"),
        ..Site::BASE
    },
    Site {
        id: "preflight.eth_getBlockByNumber.fee_pin",
        method: "eth_getBlockByNumber",
        stage: Some("preflight"),
        caller_family: "fee at pinned block",
        state_kind: STATE_FEE_PARAMETERS,
        semantic_class: CLASS_EXECUTION_PREPARATION,
        class_basis: "the base fee this read carries becomes the ceiling the transaction will \
            carry: the consumer is a price, not a check of someone else's price",
        verification_role: ROLE_SAFETY_GATE,
        role_evidence: "crates/execution/src/fee.rs: `max_tip > max_fee` and a missing base fee are \
            refusals, and the reading's `max_fee_per_gas` is what the balance ceiling is computed \
            from",
        alternative_verification: ALT_NO_ALTERNATIVE,
        block_semantics: "numbered height at the pin",
        consumer: "the priced intent's fee fields and the pin half of the pin-versus-head line",
        stamp: ("crates/execution/src/giwa/preflight_facts.rs", "fee at pinned block"),
        decision: ("crates/execution/src/fee.rs", "max_tip > max_fee"),
        ..Site::BASE
    },
    Site {
        id: "preflight.eth_getBlockByNumber.fee_head",
        method: "eth_getBlockByNumber",
        stage: Some("preflight"),
        caller_family: "fee at head",
        state_kind: STATE_FEE_PARAMETERS,
        semantic_class: CLASS_EXECUTION_PREPARATION,
        class_basis: "the head's ceiling is the other half of the fee line's comparison",
        verification_role: ROLE_SAFETY_GATE,
        role_evidence: "crates/execution/src/preflight.rs: `facts.fee.max_fee_per_gas` is the \
            operand of the `FeeEstimate` line that refuses an underpriced run",
        alternative_verification: ALT_NO_ALTERNATIVE,
        block_semantics: "numbered height at the head",
        consumer: "the head half of the fee line, and the height that leg must agree with",
        stamp: ("crates/execution/src/giwa/preflight_facts.rs", "fee at head"),
        decision: ("crates/execution/src/preflight.rs", "facts.fee.max_fee_per_gas.is_zero"),
        ..Site::BASE
    },
    Site {
        id: "preflight.eth_getBlockByNumber.binding_pin",
        method: "eth_getBlockByNumber",
        stage: Some("preflight"),
        caller_family: "block binding at pin",
        state_kind: STATE_BLOCK_HEADER,
        semantic_class: CLASS_BLOCK_CONTEXT,
        class_basis: "the same header as the pin's read, asked again to check the pin still exists",
        verification_role: ROLE_INDEPENDENT_RECHECK,
        role_evidence: "crates/execution/src/chain_read.rs: `hash == pinned` is the whole decision, \
            and a binding that never ran arrives as `Unverified` rather than as a pass",
        alternative_verification: ALT_NO_ALTERNATIVE,
        alternative_evidence: "the verification is that this stage read the hash itself; serving \
            it from another stage's header would make the comparison one value against itself",
        carrier: CARRIER_VERIFIED_BLOCK_CONTEXT,
        block_semantics: "numbered height at the pin, with the hash asked alongside it",
        consumer: "the `BlockBinding` leg of the gate and the block context build later re-checks",
        stamp: ("crates/execution/src/giwa/preflight_facts.rs", "block binding at pin"),
        decision: ("crates/execution/src/chain_read.rs", "hash == pinned"),
        ..Site::BASE
    },
    Site {
        id: "preflight.eth_getBlockByNumber.nonce_pair_head",
        method: "eth_getBlockByNumber",
        stage: Some("preflight"),
        caller_family: "pending and latest nonces",
        state_kind: STATE_BLOCK_HEADER,
        semantic_class: CLASS_BLOCK_CONTEXT,
        class_basis: "the header is read to name the height the confirmed nonce view describes \
            (class by consumer role, §5)",
        verification_role: ROLE_INFORMATION_ONLY,
        role_evidence: "crates/execution/src/nonce.rs: the comparison is `pending` against \
            `confirmed`, and this answer only labels which height the confirmed half describes — \
            no line reads it as a check",
        alternative_verification: ALT_NOT_APPLICABLE,
        block_semantics: "a tag: `latest`, asked as the second read of a pair",
        consumer: "the height label the pending-versus-confirmed pair is stamped with",
        stamp: ("crates/execution/src/giwa/preflight_facts.rs", "pending and latest nonces"),
        decision: ("crates/execution/src/nonce.rs", "pending < self.confirmed"),
        ..Site::BASE
    },
    Site {
        id: "preflight.eth_getTransactionCount.pair",
        method: "eth_getTransactionCount",
        stage: Some("preflight"),
        caller_family: "pending and latest nonces",
        state_kind: STATE_NONCE,
        semantic_class: CLASS_EXECUTION_PREPARATION,
        class_basis: "the pending view becomes the nonce the intent carries, and the confirmed \
            view is its contradiction check",
        verification_role: ROLE_SAFETY_GATE,
        role_evidence: "crates/execution/src/gate.rs: `NonceValid` refuses when the intent's nonce \
            is not the pending view, and `pending < confirmed` is an error in itself",
        alternative_verification: ALT_NO_ALTERNATIVE,
        block_semantics: "a tag for the pending leg, the head's number for the confirmed leg",
        consumer: "the intent's nonce and the `NonceValid` leg of the gate",
        stamp: ("crates/execution/src/giwa/preflight_facts.rs", "pending and latest nonces"),
        decision: ("crates/execution/src/nonce.rs", "pending < self.confirmed"),
        ..Site::BASE
    },
    Site {
        id: "preflight.eth_getBalance.sender",
        method: "eth_getBalance",
        stage: Some("preflight"),
        caller_family: "native balance of the sender",
        state_kind: STATE_NATIVE_BALANCE,
        semantic_class: CLASS_EXECUTION_PREPARATION,
        class_basis: "the answer is spent against the sequence ceiling: §5 puts a read that \
            decides whether the money is there in preparation, not in verification",
        verification_role: ROLE_SAFETY_GATE,
        role_evidence: "crates/execution/src/gate.rs: `BalanceSufficient` refuses an insufficient or \
            unread balance, and preflight's own ceiling line re-reads the same figure against the \
            whole sequence",
        alternative_verification: ALT_NO_ALTERNATIVE,
        block_semantics: "numbered height at the head",
        consumer: "the balance leg of the gate, on the sequence-wide ceiling",
        stamp: ("crates/execution/src/giwa/preflight_facts.rs", "native balance of the sender"),
        decision: ("crates/execution/src/gate.rs", "pub enum BlockBinding"),
        ..Site::BASE
    },
    Site {
        id: "preflight.eth_chainId.endpoint",
        method: "eth_chainId",
        stage: Some("preflight"),
        caller_family: "endpoint chain id",
        state_kind: STATE_CHAIN_IDENTITY,
        semantic_class: CLASS_IDENTITY_CONNECTION,
        class_basis: "the ask is which chain this endpoint answers for, and its consumer is the \
            three-way equality",
        verification_role: ROLE_INDEPENDENT_RECHECK,
        role_evidence: "crates/execution/src/gate.rs: intent, configuration and endpoint must all \
            agree, and the file's own words for the leg are that it is read rather than remembered",
        alternative_verification: ALT_NEW_CHECK_REQUIRED,
        alternative_evidence: "M8.4.3's `chain_identity` row already says what is missing: a \
            per-check field distinguishing a fresh answer from a shared one, and any detection of \
            a chain change after connect. Without them the endpoint's live claim disappears",
        carrier: CARRIER_ADAPTER_FIELD,
        carrier_evidence: "the adapter already holds a chain id from connect, which is exactly why \
            this leg is a re-read rather than a lookup",
        block_semantics: "no block term: the method asks none",
        consumer: "the `ChainMatches` leg and the chain id inside the block context",
        stamp: ("crates/execution/src/giwa/preflight_facts.rs", "endpoint chain id"),
        decision: ("crates/execution/src/gate.rs", "pub enum BlockBinding"),
    },
    Site {
        id: "preflight.eth_maxPriorityFeePerGas.pin",
        method: "eth_maxPriorityFeePerGas",
        stage: Some("preflight"),
        caller_family: "fee at pinned block",
        state_kind: STATE_FEE_PARAMETERS,
        semantic_class: CLASS_EXECUTION_PREPARATION,
        class_basis: "the tip enters the ceiling the intent carries",
        verification_role: ROLE_SAFETY_GATE,
        role_evidence: "crates/execution/src/fee.rs: a headroom policy with no suggestion is \
            refused — \"defaulting a tip to zero would be a decision about the transaction, not a \
            read\" — and a tip above the ceiling is refused too",
        alternative_verification: ALT_NO_ALTERNATIVE,
        block_semantics: "no block term, asked under the pin's stamp",
        consumer: "the pin reading's tip, ceiling and provenance",
        stamp: ("crates/execution/src/giwa/preflight_facts.rs", "fee at pinned block"),
        decision: ("crates/execution/src/fee.rs", "the endpoint gave no suggested priority fee"),
        ..Site::BASE
    },
    Site {
        id: "preflight.eth_maxPriorityFeePerGas.head",
        method: "eth_maxPriorityFeePerGas",
        stage: Some("preflight"),
        caller_family: "fee at head",
        state_kind: STATE_FEE_PARAMETERS,
        semantic_class: CLASS_EXECUTION_PREPARATION,
        class_basis: "the same parameterless method under the head's stamp, and part of the head \
            reading the fee line compares with",
        verification_role: ROLE_SAFETY_GATE,
        role_evidence: "crates/execution/src/preflight.rs: the `FeeEstimate` operand is the head \
            reading's `max_fee_per_gas`, and the headroom policy builds that number out of the base \
            fee *and* this tip, so the read is inside the comparison and not beside it",
        alternative_verification: ALT_NO_ALTERNATIVE,
        alternative_evidence: "measured, not assumed: `GiwaSequencerDirect::fee_reading` asks the \
            tip on every call, so the head reading carries a head tip into the refusal line, and \
            serving it the pin's tip would move an operand of the gate",
        block_semantics: "no block term, asked under the head's stamp",
        consumer: "the head reading's tip, inside the fee line's ceiling",
        stamp: ("crates/execution/src/giwa/preflight_facts.rs", "fee at head"),
        decision: ("crates/execution/src/preflight.rs", "facts.fee.max_fee_per_gas.is_zero"),
        ..Site::BASE
    },
    // -- the simulation's canonical reads -----------------------------------------------
    Site {
        id: "simulation.eth_getBlockByNumber.header",
        method: "eth_getBlockByNumber",
        stage: Some("simulation"),
        caller_family: "header at pin",
        state_kind: STATE_BLOCK_HEADER,
        semantic_class: CLASS_BLOCK_CONTEXT,
        class_basis: "the header is read to check the state being executed is the block the run \
            named",
        verification_role: ROLE_INDEPENDENT_RECHECK,
        role_evidence: "crates/simulation/src/engine.rs: the read answers its own chain id check \
            and then `check_pin`, whose refusal is `StateMismatch` — the state is not the state the \
            opportunity was found on",
        alternative_verification: ALT_NO_ALTERNATIVE,
        carrier: CARRIER_SIMULATION_CACHE,
        block_semantics: "numbered height at the pin",
        consumer: "the pin check, the fee ceiling and REVM's block env",
        stamp: ("crates/simulation/src/engine.rs", "note_read_phase(\"header at pin\")"),
        decision: ("crates/simulation/src/engine.rs", "claimed != found"),
        ..Site::BASE
    },
    Site {
        id: "simulation.eth_getCode.touched",
        method: "eth_getCode",
        stage: Some("simulation"),
        caller_family: "codes: touched_contracts",
        state_kind: STATE_CONTRACT_CODE,
        semantic_class: CLASS_CANONICAL_STATE_READ,
        class_basis: "bytecode is canonical state, and §5's note about consumer role makes the \
            existence check a gate on top of it",
        verification_role: ROLE_SAFETY_GATE,
        role_evidence: "crates/simulation/src/engine.rs: an empty `eth_getCode` is \
            `MissingCode` — a refusal, not a bytecode to be filled in",
        alternative_verification: ALT_NO_ALTERNATIVE,
        carrier: CARRIER_SIMULATION_CACHE,
        carrier_evidence: "M8.3.1's cache is why this site shows no duplicate in the records at \
            all: repeated code reads inside one simulation are answered from it",
        block_semantics: "numbered height at the pin",
        consumer: "REVM's journal for the route's contracts",
        stamp: ("crates/simulation/src/engine.rs", "note_read_phase(\"codes: touched_contracts\")"),
        decision: ("crates/simulation/src/engine.rs", "claimed != found"),
        ..Site::BASE
    },
    Site {
        id: "simulation.eth_getCode.sender",
        method: "eth_getCode",
        stage: Some("simulation"),
        caller_family: "code: sender",
        state_kind: STATE_CONTRACT_CODE,
        semantic_class: CLASS_CANONICAL_STATE_READ,
        class_basis: "an inverted existence check on the same method: the answer's role is to be \
            empty",
        verification_role: ROLE_SAFETY_GATE,
        role_evidence: "crates/simulation/src/engine.rs: bytecode on the sender is a refusal — §58's \
            account signs nothing and runs nothing",
        alternative_verification: ALT_NO_ALTERNATIVE,
        carrier: CARRIER_SIMULATION_CACHE,
        block_semantics: "numbered height at the pin",
        consumer: "a hard refusal if the simulated sender is a contract",
        stamp: ("crates/simulation/src/engine.rs", "note_read_phase(\"code: sender\")"),
        decision: ("crates/simulation/src/engine.rs", "claimed != found"),
        ..Site::BASE
    },
    Site {
        id: "simulation.eth_getBalance.sender_account",
        method: "eth_getBalance",
        stage: Some("simulation"),
        caller_family: "account: sender",
        state_kind: STATE_NATIVE_BALANCE,
        semantic_class: CLASS_CANONICAL_STATE_READ,
        class_basis: "the same method as the gate's balance read, and a different site: this answer \
            seeds a local execution",
        verification_role: ROLE_INFORMATION_ONLY,
        role_evidence: "crates/simulation/src/engine.rs: the triple seeds REVM's journal; no line \
            compares it against another stage's reading",
        carrier: CARRIER_SIMULATION_CACHE,
        block_semantics: "numbered height at the pin",
        consumer: "the sender's account as REVM sees it at the pin",
        stamp: ("crates/simulation/src/engine.rs", "note_read_phase(\"account: sender\")"),
        decision: ("crates/simulation/src/engine.rs", "claimed != found"),
        ..Site::BASE
    },
    Site {
        id: "simulation.eth_getTransactionCount.sender_account",
        method: "eth_getTransactionCount",
        stage: Some("simulation"),
        caller_family: "account: sender",
        state_kind: STATE_NONCE,
        semantic_class: CLASS_CANONICAL_STATE_READ,
        class_basis: "the nonce as of the pin, for a local execution: preparation happens in build, \
            not here",
        verification_role: ROLE_INFORMATION_ONLY,
        role_evidence: "crates/simulation/src/engine.rs: the journal's start nonce comes from this \
            value, and an absent account degrades to 0 rather than refusing",
        carrier: CARRIER_SIMULATION_CACHE,
        block_semantics: "numbered height at the pin",
        consumer: "REVM's starting nonce for the simulated sequence",
        stamp: ("crates/simulation/src/engine.rs", "note_read_phase(\"account: sender\")"),
        decision: ("crates/simulation/src/engine.rs", "claimed != found"),
        ..Site::BASE
    },
    // -- the build lane's own gate reads ------------------------------------------------
    Site {
        id: "build.eth_chainId.gate",
        method: "eth_chainId",
        stage: Some("build"),
        caller_family: "step N: gate — endpoint chain id",
        state_kind: STATE_CHAIN_IDENTITY,
        semantic_class: CLASS_IDENTITY_CONNECTION,
        class_basis: "the lane asks the endpoint which chain it is on, seconds before signing",
        verification_role: ROLE_FINAL_EXECUTION_GUARD,
        role_evidence: "crates/execution/src/sequence.rs: the three-way equality runs on this step's \
            own read and a failed gate halts the step before a signature exists",
        alternative_verification: ALT_NEW_CHECK_REQUIRED,
        alternative_evidence: "same missing mechanism as preflight's leg: a per-check field and a \
            chain-change detector, which M8.4.3 lists as this category's open proof",
        carrier: CARRIER_ADAPTER_FIELD,
        block_semantics: "no block term",
        consumer: "the `ChainMatches` leg and the chain id inside the block identity",
        stamp: ("crates/execution/src/sequence.rs", "step {step_number}: gate — endpoint chain id"),
        decision: ("crates/execution/src/gate.rs", "pub enum BlockBinding"),
        ..Site::BASE
    },
    Site {
        id: "build.eth_getBlockByNumber.binding",
        method: "eth_getBlockByNumber",
        stage: Some("build"),
        caller_family: "step N: gate — block binding at pin",
        state_kind: STATE_BLOCK_HEADER,
        semantic_class: CLASS_BLOCK_CONTEXT,
        class_basis: "the header re-read to check the pin the intent names still resolves to its \
            hash",
        verification_role: ROLE_INDEPENDENT_RECHECK,
        role_evidence: "crates/execution/src/sequence.rs: the step compares its own read against \
            the context preflight verified, and the code says the read replaces no read — nothing \
            is skipped, so no RPC is saved here",
        alternative_verification: ALT_NO_ALTERNATIVE,
        carrier: CARRIER_VERIFIED_BLOCK_CONTEXT,
        carrier_evidence: "M8.4.4's `VerifiedBlockContext` is the carrier and was measured to be \
            safe to propagate with zero net RPC saving; it does not replace this read, it is what \
            this read is checked against",
        block_semantics: "numbered height at the pin, with the hash asked alongside it",
        consumer: "the binding leg, and the cross-stage comparison against preflight's verdict",
        stamp: ("crates/execution/src/sequence.rs", "step {step_number}: gate — block binding at pin"),
        decision: ("crates/execution/src/chain_read.rs", "hash == pinned"),
        ..Site::BASE
    },
    Site {
        id: "build.eth_getBlockByNumber.fee_pin",
        method: "eth_getBlockByNumber",
        stage: Some("build"),
        caller_family: "step N: fee at pinned block",
        state_kind: STATE_FEE_PARAMETERS,
        semantic_class: CLASS_EXECUTION_PREPARATION,
        class_basis: "the header's base fee becomes this step's ceiling",
        verification_role: ROLE_SAFETY_GATE,
        role_evidence: "crates/execution/src/sequence.rs: `fields_for(tx_type)` is a refusal path, \
            and the ceiling this reading yields is what the balance leg is computed against",
        alternative_verification: ALT_NO_ALTERNATIVE,
        block_semantics: "numbered height at the pin",
        consumer: "the intent's fee fields for the step being built",
        stamp: ("crates/execution/src/sequence.rs", "step {step_number}: fee at pinned block"),
        decision: ("crates/execution/src/fee.rs", "max_tip > max_fee"),
        ..Site::BASE
    },
    Site {
        id: "build.eth_getBlockByNumber.nonce_pair_head",
        method: "eth_getBlockByNumber",
        stage: Some("build"),
        caller_family: "step N: pending and latest nonces",
        state_kind: STATE_BLOCK_HEADER,
        semantic_class: CLASS_BLOCK_CONTEXT,
        class_basis: "the header names the height the lane's confirmed nonce view describes",
        verification_role: ROLE_INFORMATION_ONLY,
        role_evidence: "crates/execution/src/nonce.rs: the decisions read `pending` against \
            `confirmed` and the lane's own outstanding count; the height is a label on the pair",
        alternative_verification: ALT_NOT_APPLICABLE,
        block_semantics: "a tag: `latest`, read as the pair's label",
        consumer: "the height label on the nonce pair this step allocates against",
        stamp: ("crates/execution/src/sequence.rs", "step {step_number}: pending and latest nonces"),
        decision: ("crates/execution/src/nonce.rs", "could duplicate a live transaction"),
        ..Site::BASE
    },
    Site {
        id: "build.eth_getTransactionCount.pair",
        method: "eth_getTransactionCount",
        stage: Some("build"),
        caller_family: "step N: pending and latest nonces",
        state_kind: STATE_NONCE,
        semantic_class: CLASS_EXECUTION_PREPARATION,
        class_basis: "the pair that both allocates the lane's nonce and checks the intent against it",
        verification_role: ROLE_FINAL_EXECUTION_GUARD,
        role_evidence: "crates/execution/src/nonce.rs: an in-flight lane refuses — the transaction \
            \"could duplicate a live transaction\" — and the gate's `NonceValid` leg re-affirms the \
            intent against the same reading",
        alternative_verification: ALT_NO_ALTERNATIVE,
        block_semantics: "a tag for the pending leg, the head's number for the confirmed leg",
        consumer: "the lane's allocation and the last nonce check before signing",
        stamp: ("crates/execution/src/sequence.rs", "step {step_number}: pending and latest nonces"),
        decision: ("crates/execution/src/nonce.rs", "could duplicate a live transaction"),
        ..Site::BASE
    },
    Site {
        id: "build.eth_getBalance.gate",
        method: "eth_getBalance",
        stage: Some("build"),
        caller_family: "step N: gate — native balance",
        state_kind: STATE_NATIVE_BALANCE,
        semantic_class: CLASS_EXECUTION_PREPARATION,
        class_basis: "the balance is spent against this built transaction's own ceiling",
        verification_role: ROLE_FINAL_EXECUTION_GUARD,
        role_evidence: "crates/execution/src/sequence.rs: the comparison is this step's \
            `maximum_cost_wei`, a different basis from preflight's estimate, and a failure halts \
            the step",
        alternative_verification: ALT_NO_ALTERNATIVE,
        block_semantics: "numbered height at the pin",
        consumer: "the balance leg of the gate this step signs behind",
        stamp: ("crates/execution/src/sequence.rs", "step {step_number}: gate — native balance"),
        decision: ("crates/execution/src/sequence.rs", "maximum_cost_wei"),
        ..Site::BASE
    },
    Site {
        id: "build.eth_maxPriorityFeePerGas.pin",
        method: "eth_maxPriorityFeePerGas",
        stage: Some("build"),
        caller_family: "step N: fee at pinned block",
        state_kind: STATE_FEE_PARAMETERS,
        semantic_class: CLASS_EXECUTION_PREPARATION,
        class_basis: "the tip that enters this step's ceiling and the transaction's priority field",
        verification_role: ROLE_SAFETY_GATE,
        role_evidence: "crates/execution/src/fee.rs: the policy refusals run on this step's own \
            reading, and the codec refuses a priority above the ceiling a second time",
        alternative_verification: ALT_NO_ALTERNATIVE,
        block_semantics: "no block term, asked under the pin's stamp",
        consumer: "the intent's priority fee and the ceiling's tip half",
        stamp: ("crates/execution/src/sequence.rs", "step {step_number}: fee at pinned block"),
        decision: ("crates/execution/src/fee.rs", "the endpoint gave no suggested priority fee"),
        ..Site::BASE
    },
    Site {
        id: "build.eth_call.before_snapshot",
        method: "eth_call",
        stage: Some("build"),
        caller_family: "before-snapshot: native and input-token balances",
        state_kind: STATE_ETH_CALL_RESULT,
        semantic_class: CLASS_OBSERVATION,
        class_basis: "an audit snapshot of the sender's token balance: §5's observation, because \
            the only comparison it enters happens after the transactions are already sent",
        verification_role: ROLE_INFORMATION_ONLY,
        role_evidence: "crates/execution/src/sequence.rs: the before/after pair becomes a \
            `BalanceDelta` reconciled against the receipt's logs, and a disagreement is recorded as \
            a mismatch rather than stopping anything",
        alternative_verification: ALT_NOT_APPLICABLE,
        carrier: CARRIER_REPORT,
        block_semantics: "numbered height at the pin",
        consumer: "the accounting baseline of the sequence report",
        stamp: ("crates/execution/src/sequence.rs", "before-snapshot: native and input-token balances"),
        decision: ("crates/execution/src/sequence.rs", "maximum_cost_wei"),
        ..Site::BASE
    },
    Site {
        id: "build.eth_getBalance.before_snapshot",
        method: "eth_getBalance",
        stage: Some("build"),
        caller_family: "before-snapshot: native and input-token balances",
        state_kind: STATE_NATIVE_BALANCE,
        semantic_class: CLASS_OBSERVATION,
        class_basis: "the native half of the same accounting baseline",
        verification_role: ROLE_INFORMATION_ONLY,
        role_evidence: "crates/execution/src/sequence.rs: the snapshot's first decision is a binding \
            sanity check on which snapshot it is, not on how much is there",
        alternative_verification: ALT_NOT_APPLICABLE,
        carrier: CARRIER_REPORT,
        block_semantics: "numbered height at the pin",
        consumer: "the native half of the delta the report reconciles against the receipts",
        stamp: ("crates/execution/src/sequence.rs", "before-snapshot: native and input-token balances"),
        decision: ("crates/execution/src/sequence.rs", "maximum_cost_wei"),
        ..Site::BASE
    },
    // -- the connect-time read and the REVM's demand reads -------------------------------
    Site {
        id: "connect.eth_chainId.unstamped",
        method: "eth_chainId",
        stage: None,
        caller_family: CALLER_UNSTAMPED,
        state_kind: STATE_CHAIN_IDENTITY,
        semantic_class: CLASS_IDENTITY_CONNECTION,
        class_basis: "the read that learns which chain the endpoint is on before an adapter exists \
            to be watched",
        verification_role: ROLE_SAFETY_GATE,
        role_evidence: "crates/pipeline/src/arbitrage.rs: the stored answer is compared against the \
            configured chain at the start of every run, and it keys every later decode",
        alternative_verification: ALT_NOT_APPLICABLE,
        carrier: CARRIER_ADAPTER_FIELD,
        carrier_evidence: "`HttpChainAdapter` holds the value it read, which is why the two later \
            legs are re-reads of the same ask rather than first answers",
        block_semantics: "no block term",
        consumer: "the adapter's chain id, the run's first mismatch refusal, the sink's note",
        stamp: ("crates/chain/src/rpc.rs", "request_with(&http, url, \"eth_chainId\""),
        decision: ("crates/pipeline/src/arbitrage.rs", "if chain_id != expected"),
        ..Site::BASE
    },
    Site {
        id: "simulation.eth_getBalance.views_token0",
        method: "eth_getBalance",
        stage: Some("simulation"),
        caller_family: "views: token0()",
        state_kind: STATE_NATIVE_BALANCE,
        semantic_class: CLASS_CANONICAL_STATE_READ,
        class_basis: "an account read the EVM demanded while running a view, so its role is to \
            answer for the call and not for a check",
        verification_role: ROLE_INFORMATION_ONLY,
        role_evidence: "crates/simulation/src/engine.rs: a view's companion reads enter the journal \
            as the call's input; the check is the call's own answer",
        carrier: CARRIER_SIMULATION_CACHE,
        block_semantics: "numbered height at the pin",
        consumer: "the account state behind one read-only call",
        stamp: ("crates/simulation/src/engine.rs", "let signature = call.signature();"),
        decision: ("crates/simulation/src/engine.rs", "claimed != found"),
        ..Site::BASE
    },
    Site {
        id: "simulation.eth_getCode.views_token0",
        method: "eth_getCode",
        stage: Some("simulation"),
        caller_family: "views: token0()",
        state_kind: STATE_CONTRACT_CODE,
        semantic_class: CLASS_CANONICAL_STATE_READ,
        class_basis: "the bytecode the view call needed, fetched by the same mechanism as the §5 \
            batch",
        verification_role: ROLE_INFORMATION_ONLY,
        role_evidence: "crates/simulation/src/engine.rs: this site has no emptiness rule of its \
            own; the `MissingCode` refusal belongs to the batch site",
        carrier: CARRIER_SIMULATION_CACHE,
        block_semantics: "numbered height at the pin",
        consumer: "the code REVM executes for one view call",
        stamp: ("crates/simulation/src/engine.rs", "let signature = call.signature();"),
        decision: ("crates/simulation/src/engine.rs", "claimed != found"),
        ..Site::BASE
    },
    Site {
        id: "simulation.eth_getTransactionCount.views_token0",
        method: "eth_getTransactionCount",
        stage: Some("simulation"),
        caller_family: "views: token0()",
        state_kind: STATE_NONCE,
        semantic_class: CLASS_CANONICAL_STATE_READ,
        class_basis: "the sender's nonce as the view's journal needs it",
        verification_role: ROLE_INFORMATION_ONLY,
        role_evidence: "crates/simulation/src/engine.rs: nothing compares this nonce against \
            another stage's; the signing nonce is read in build",
        carrier: CARRIER_SIMULATION_CACHE,
        block_semantics: "numbered height at the pin",
        consumer: "the journal's account state for one view call",
        stamp: ("crates/simulation/src/engine.rs", "let signature = call.signature();"),
        decision: ("crates/simulation/src/engine.rs", "claimed != found"),
        ..Site::BASE
    },
    Site {
        id: "simulation.eth_getStorageAt.views_getreserves",
        method: "eth_getStorageAt",
        stage: Some("simulation"),
        caller_family: "views: getReserves()",
        state_kind: STATE_STORAGE_SLOT,
        semantic_class: CLASS_CANONICAL_STATE_READ,
        class_basis: "the slots REVM reads to answer the pool's own `getReserves()` — note that the \
            reserve check is a local execution, not an `eth_call`",
        verification_role: ROLE_INFORMATION_ONLY,
        role_evidence: "crates/simulation/src/engine.rs: the executed view's answer is what \
            `check_reserves` compares against the claim, and this site only supplies the state the \
            view ran on",
        carrier: CARRIER_SIMULATION_CACHE,
        block_semantics: "numbered height at the pin",
        consumer: "the reserve slots behind one executed view",
        stamp: ("crates/simulation/src/engine.rs", "let signature = call.signature();"),
        decision: ("crates/simulation/src/engine.rs", "claimed != found"),
        ..Site::BASE
    },
    Site {
        id: "simulation.eth_getStorageAt.views_token0",
        method: "eth_getStorageAt",
        stage: Some("simulation"),
        caller_family: "views: token0()",
        state_kind: STATE_STORAGE_SLOT,
        semantic_class: CLASS_CANONICAL_STATE_READ,
        class_basis: "the slot behind the executed `token0()`",
        verification_role: ROLE_INFORMATION_ONLY,
        carrier: CARRIER_SIMULATION_CACHE,
        block_semantics: "numbered height at the pin",
        consumer: "one storage slot of one pool, for one view",
        stamp: ("crates/simulation/src/engine.rs", "let signature = call.signature();"),
        decision: ("crates/simulation/src/engine.rs", "claimed != found"),
        ..Site::BASE
    },
    Site {
        id: "simulation.eth_getStorageAt.views_token1",
        method: "eth_getStorageAt",
        stage: Some("simulation"),
        caller_family: "views: token1()",
        state_kind: STATE_STORAGE_SLOT,
        semantic_class: CLASS_CANONICAL_STATE_READ,
        class_basis: "the slot behind the executed `token1()`",
        verification_role: ROLE_INFORMATION_ONLY,
        carrier: CARRIER_SIMULATION_CACHE,
        block_semantics: "numbered height at the pin",
        consumer: "one storage slot of one pool, for one view",
        stamp: ("crates/simulation/src/engine.rs", "let signature = call.signature();"),
        decision: ("crates/simulation/src/engine.rs", "claimed != found"),
        ..Site::BASE
    },
    Site {
        id: "simulation.eth_getStorageAt.execute_swap",
        method: "eth_getStorageAt",
        stage: Some("simulation"),
        caller_family:
            "execute: step N (nonce N) from <addr> to <addr>: swap(uint256,uint256,address,bytes), value 0, selector <addr>",
        state_kind: STATE_STORAGE_SLOT,
        semantic_class: CLASS_CANONICAL_STATE_READ,
        class_basis: "the SLOADs one executed swap demanded",
        verification_role: ROLE_INFORMATION_ONLY,
        role_evidence: "crates/simulation/src/engine.rs: journal input — the only decisions made \
            from an executed step are its own success, revert or out-of-gas status",
        carrier: CARRIER_SIMULATION_CACHE,
        block_semantics: "numbered height at the pin",
        consumer: "the state one executed step read",
        stamp: (
            "crates/simulation/src/engine.rs",
            "interpreter_phase(EXECUTE_PHASE_PREFIX, &step.describe())",
        ),
        decision: ("crates/simulation/src/engine.rs", "claimed != found"),
        ..Site::BASE
    },
    Site {
        id: "simulation.eth_getStorageAt.execute_transfer",
        method: "eth_getStorageAt",
        stage: Some("simulation"),
        caller_family:
            "execute: step N (nonce N) from <addr> to <addr>: transfer(address,uint256), value 0, selector <addr>",
        state_kind: STATE_STORAGE_SLOT,
        semantic_class: CLASS_CANONICAL_STATE_READ,
        class_basis: "the SLOADs one executed token transfer demanded",
        verification_role: ROLE_INFORMATION_ONLY,
        carrier: CARRIER_SIMULATION_CACHE,
        block_semantics: "numbered height at the pin",
        consumer: "the state one executed step read",
        stamp: (
            "crates/simulation/src/engine.rs",
            "interpreter_phase(EXECUTE_PHASE_PREFIX, &step.describe())",
        ),
        decision: ("crates/simulation/src/engine.rs", "claimed != found"),
        ..Site::BASE
    },
    Site {
        id: "simulation.eth_getStorageAt.execute_balanceof",
        method: "eth_getStorageAt",
        stage: Some("simulation"),
        caller_family:
            "execute: step N (nonce N) from <addr> to <addr>: balanceOf(address), value 0, selector <addr>",
        state_kind: STATE_STORAGE_SLOT,
        semantic_class: CLASS_CANONICAL_STATE_READ,
        class_basis: "the SLOADs one executed `balanceOf` demanded",
        verification_role: ROLE_INFORMATION_ONLY,
        carrier: CARRIER_SIMULATION_CACHE,
        block_semantics: "numbered height at the pin",
        consumer: "the state one executed step read",
        stamp: (
            "crates/simulation/src/engine.rs",
            "interpreter_phase(EXECUTE_PHASE_PREFIX, &step.describe())",
        ),
        decision: ("crates/simulation/src/engine.rs", "claimed != found"),
        ..Site::BASE
    },
    Site {
        id: "simulation.eth_getBalance.execute_balanceof",
        method: "eth_getBalance",
        stage: Some("simulation"),
        caller_family:
            "execute: step N (nonce N) from <addr> to <addr>: balanceOf(address), value 0, selector <addr>",
        state_kind: STATE_NATIVE_BALANCE,
        semantic_class: CLASS_CANONICAL_STATE_READ,
        class_basis: "an account balance the interpreter demanded mid-step",
        verification_role: ROLE_INFORMATION_ONLY,
        carrier: CARRIER_SIMULATION_CACHE,
        block_semantics: "numbered height at the pin",
        consumer: "the native balance in REVM's journal for one step",
        stamp: (
            "crates/simulation/src/engine.rs",
            "interpreter_phase(EXECUTE_PHASE_PREFIX, &step.describe())",
        ),
        decision: ("crates/simulation/src/engine.rs", "claimed != found"),
        ..Site::BASE
    },
    Site {
        id: "simulation.eth_getBalance.execute_swap",
        method: "eth_getBalance",
        stage: Some("simulation"),
        caller_family:
            "execute: step N (nonce N) from <addr> to <addr>: swap(uint256,uint256,address,bytes), value 0, selector <addr>",
        state_kind: STATE_NATIVE_BALANCE,
        semantic_class: CLASS_CANONICAL_STATE_READ,
        class_basis: "an account balance the interpreter demanded mid-step",
        verification_role: ROLE_INFORMATION_ONLY,
        carrier: CARRIER_SIMULATION_CACHE,
        block_semantics: "numbered height at the pin",
        consumer: "the native balance in REVM's journal for one step",
        stamp: (
            "crates/simulation/src/engine.rs",
            "interpreter_phase(EXECUTE_PHASE_PREFIX, &step.describe())",
        ),
        decision: ("crates/simulation/src/engine.rs", "claimed != found"),
        ..Site::BASE
    },
    Site {
        id: "simulation.eth_getTransactionCount.execute_balanceof",
        method: "eth_getTransactionCount",
        stage: Some("simulation"),
        caller_family:
            "execute: step N (nonce N) from <addr> to <addr>: balanceOf(address), value 0, selector <addr>",
        state_kind: STATE_NONCE,
        semantic_class: CLASS_CANONICAL_STATE_READ,
        class_basis: "an account nonce the interpreter demanded mid-step",
        verification_role: ROLE_INFORMATION_ONLY,
        carrier: CARRIER_SIMULATION_CACHE,
        block_semantics: "numbered height at the pin",
        consumer: "the nonce in REVM's journal for one step",
        stamp: (
            "crates/simulation/src/engine.rs",
            "interpreter_phase(EXECUTE_PHASE_PREFIX, &step.describe())",
        ),
        decision: ("crates/simulation/src/engine.rs", "claimed != found"),
        ..Site::BASE
    },
    Site {
        id: "simulation.eth_getTransactionCount.execute_swap",
        method: "eth_getTransactionCount",
        stage: Some("simulation"),
        caller_family:
            "execute: step N (nonce N) from <addr> to <addr>: swap(uint256,uint256,address,bytes), value 0, selector <addr>",
        state_kind: STATE_NONCE,
        semantic_class: CLASS_CANONICAL_STATE_READ,
        class_basis: "an account nonce the interpreter demanded mid-step",
        verification_role: ROLE_INFORMATION_ONLY,
        carrier: CARRIER_SIMULATION_CACHE,
        block_semantics: "numbered height at the pin",
        consumer: "the nonce in REVM's journal for one step",
        stamp: (
            "crates/simulation/src/engine.rs",
            "interpreter_phase(EXECUTE_PHASE_PREFIX, &step.describe())",
        ),
        decision: ("crates/simulation/src/engine.rs", "claimed != found"),
        ..Site::BASE
    },
];

/// §11's rows that carry a verification, §12's hard block, and §14's REJECT list in one place.
pub const READ_CATEGORY_UNASSIGNED: &str = "caller_family_unassigned";

pub fn site_by_key(key: &SiteKey) -> Option<&'static Site> {
    SITES.iter().find(|site| site.key() == *key)
}

pub fn site_by_id(id: &str) -> Option<&'static Site> {
    SITES.iter().find(|site| site.id == id)
}

// ---------------------------------------------------------------------------
// §4's transport layer: which code path turns a method into bytes on the wire
// ---------------------------------------------------------------------------

/// One wire method, the adapter function that issues it, and whether the census's records can see
/// it. `traced` sites are the ones §4 requires before a method may be called hot-path; the two
/// blind-spot rows are here because the code calls them and no record does (§4's rule cuts both
/// ways: an uncalled method is not a row, and an unrecorded call is not invisible).
pub struct Transport {
    pub method: &'static str,
    pub adapter_file: &'static str,
    pub adapter_token: &'static str,
    pub traced: bool,
    pub note: &'static str,
}

pub const TRANSPORTS: [Transport; 11] = [
    Transport {
        method: "eth_blockNumber",
        adapter_file: "crates/chain/src/rpc.rs",
        adapter_token: "async fn latest_block(&self",
        traced: true,
        note: "one ask per run, at the observation stage, and it is the ask that fixes the pin",
    },
    Transport {
        method: "eth_getBlockByNumber",
        adapter_file: "crates/chain/src/rpc.rs",
        adapter_token: "async fn get_block_context(&self",
        traced: true,
        note: "the largest family in the census; two of its three wire paths are raw requests \
            (`giwa/preflight_facts.rs`'s head read and `giwa/sequencer_direct.rs`'s nonce-pair \
            head), which is why the surface table keys a method to more than one adapter leg",
    },
    Transport {
        method: "eth_call",
        adapter_file: "crates/chain/src/rpc.rs",
        adapter_token: "async fn call(&self",
        traced: true,
        note: "60 asks in the corpus, graded site by site in M8.5.1",
    },
    Transport {
        method: "eth_getBalance",
        adapter_file: "crates/chain/src/rpc.rs",
        adapter_token: "async fn get_balance(&self",
        traced: true,
        note: "read by the gate, by the lane, by the snapshot and by REVM",
    },
    Transport {
        method: "eth_getStorageAt",
        adapter_file: "crates/chain/src/rpc.rs",
        adapter_token: "async fn get_storage_at(&self",
        traced: true,
        note:
            "every ask is a REVM demand inside one simulation, which is the scope M8.3.1's cache \
            already serves",
    },
    Transport {
        method: "eth_getCode",
        adapter_file: "crates/chain/src/rpc.rs",
        adapter_token: "fn get_code(",
        traced: true,
        note: "same as storage: no pair among the 78, because the cache is asked first",
    },
    Transport {
        method: "eth_getTransactionCount",
        adapter_file: "crates/chain/src/rpc.rs",
        adapter_token: "async fn get_nonce(&self",
        traced: true,
        note: "always read as a pair, pending and confirmed",
    },
    Transport {
        method: "eth_chainId",
        adapter_file: "crates/chain/src/rpc.rs",
        adapter_token: "request_with(&http, url, \"eth_chainId\"",
        traced: true,
        note: "three asks per run and no block term on any of them; the connect leg is recorded \
            with `context_not_stamped` because no stage exists yet to name it",
    },
    Transport {
        method: "eth_maxPriorityFeePerGas",
        adapter_file: "crates/execution/src/giwa/sequencer_direct.rs",
        adapter_token: "async fn suggested_tip_raw",
        traced: true,
        note: "asked from inside `fee_reading`, so a pin-and-head fee gather asks it twice",
    },
    Transport {
        method: "eth_gasPrice",
        adapter_file: "",
        adapter_token: "",
        traced: false,
        note: "§5 names it as an example and no code path in this workspace calls it: not a \
            hot-path RPC (§4), so it is absent from the census totals",
    },
    Transport {
        method: "eth_estimateGas",
        adapter_file: "",
        adapter_token: "",
        traced: false,
        note: "the same: a classification rule mentions it, no call site exists",
    },
];

// ---------------------------------------------------------------------------
// the candidate table: §6's rows, with the judgement and nothing else
// ---------------------------------------------------------------------------

/// §6. The *counts* of a candidate row are measured — [`CandidateSpec::measured`] folds them out
/// of the committed pairs — and the *judgement* is this struct. Priority and safe saving are never
/// fields: [`priority_of`] and [`safe_saving_of`] derive them, so §14's rules and §24's "safe
/// saving 可回算" hold by construction and a hand-edit to a label changes nothing.
pub struct CandidateSpec {
    pub id: &'static str,
    /// §8's pattern.
    pub pattern: &'static str,
    /// The site whose answer is already asked. `""` means the candidate is one site asking itself
    /// twice, which is what §8 Pattern A is.
    pub producer: &'static str,
    /// The site that asks again. §6's `consumer` column resolves through this site's judgement.
    pub consumer: &'static str,
    pub blockers: &'static [&'static str],
    /// What would have to exist first. §14's P0 refuses a candidate that needs any of these, which
    /// is why they are fields and not prose.
    pub requires_new_carrier: bool,
    pub requires_new_lifecycle: bool,
    pub requires_new_experiment: bool,
    /// §14's P0 condition "safe reuse already proven", stated separately from the rest so a
    /// candidate cannot reach P0 on the strength of its arithmetic alone.
    pub safe_reuse_proven: bool,
    pub evidence_strength: &'static str,
    pub verdict_note: &'static str,
    pub next_step: &'static str,
}

impl CandidateSpec {
    const BASE: CandidateSpec = CandidateSpec {
        id: "",
        pattern: PATTERN_A,
        producer: "",
        consumer: "",
        blockers: &[],
        requires_new_carrier: true,
        requires_new_lifecycle: true,
        requires_new_experiment: false,
        safe_reuse_proven: false,
        evidence_strength: EVIDENCE_CODE_DERIVED,
        verdict_note: "",
        next_step: NEXT_STEP_NONE,
    };

    pub fn producer_site(&self) -> Option<&'static Site> {
        if self.producer.is_empty() {
            return site_by_id(self.consumer);
        }
        site_by_id(self.producer)
    }

    pub fn consumer_site(&self) -> &'static Site {
        site_by_id(self.consumer).unwrap_or_else(|| {
            panic!(
                "candidate {} names an unknown consumer site {}",
                self.id, self.consumer
            )
        })
    }
}

/// The exact-duplicate shapes M8.4.2 measured, one candidate each, plus the two questions that have
/// no duplicate and still have to be answered: the oracle's (§17's Candidate 1) and the
/// information-already-held one (§9/§10). Every shape in the records appears here and nothing here
/// is absent from the records — [`shape_completeness_errors`] is the gate that says so.
pub const CANDIDATES: [CandidateSpec; 18] = [
    CandidateSpec {
        id: "chainid.connect_to_preflight",
        pattern: PATTERN_A,
        producer: "connect.eth_chainId.unstamped",
        consumer: "preflight.eth_chainId.endpoint",
        blockers: &["endpoint_claim_would_stop_being_live", "no_per_check_freshness_field"],
        requires_new_carrier: false,
        requires_new_lifecycle: true,
        evidence_strength: EVIDENCE_PROVEN,
        verdict_note: "the only family in the census whose ownership is proven, whose scope is \
            proven, and whose answer provably does not depend on the block — and therefore the one \
            where a reduction is a design problem rather than a semantic impossibility",
        ..CandidateSpec::BASE
    },
    CandidateSpec {
        id: "chainid.connect_to_build",
        pattern: PATTERN_A,
        producer: "connect.eth_chainId.unstamped",
        consumer: "build.eth_chainId.gate",
        blockers: &["endpoint_claim_would_stop_being_live", "no_per_check_freshness_field"],
        requires_new_carrier: false,
        requires_new_lifecycle: true,
        evidence_strength: EVIDENCE_PROVEN,
        verdict_note: "the leg runs seconds before a signature, which is the moment an answer four \
            stages stale is least able to carry",
        ..CandidateSpec::BASE
    },
    CandidateSpec {
        id: "balance.preflight_to_build_snapshot",
        pattern: PATTERN_B,
        producer: "preflight.eth_getBalance.sender",
        consumer: "build.eth_getBalance.before_snapshot",
        blockers: &["no_owner_in_code", "no_carrier_across_stages", "head_equals_pin_in_these_runs_only"],
        requires_new_carrier: true,
        requires_new_lifecycle: true,
        evidence_strength: EVIDENCE_RECORD_ONLY,
        verdict_note: "the only exact-duplicate family whose consumer carries no verification, so \
            it is the one candidate the §12 rule does not kill outright — and the reason it still \
            saves nothing is that three runs cannot show whether the head and the pin are the same \
            height in general",
        ..CandidateSpec::BASE
    },
    CandidateSpec {
        id: "balance.simulation_to_build_gate",
        pattern: PATTERN_B,
        producer: "simulation.eth_getBalance.sender_account",
        consumer: "build.eth_getBalance.gate",
        blockers: &["read_is_the_check", "no_owner_in_code"],
        requires_new_carrier: true,
        requires_new_lifecycle: true,
        evidence_strength: EVIDENCE_CODE_DERIVED,
        verdict_note: "M8.4.3's own words for this category: duplicate in the record, different in \
            purpose, and deliberately re-read",
        ..CandidateSpec::BASE
    },
    CandidateSpec {
        id: "tip.preflight_pin_to_preflight_head",
        pattern: PATTERN_A,
        producer: "preflight.eth_maxPriorityFeePerGas.pin",
        consumer: "preflight.eth_maxPriorityFeePerGas.head",
        blockers: &["read_is_the_check", "tip_enters_the_gate_operand"],
        requires_new_carrier: false,
        requires_new_lifecycle: false,
        evidence_strength: EVIDENCE_CODE_DERIVED,
        verdict_note: "the census's most tempting row and its clearest refusal: one parameterless \
            ask twice inside one stage, and the head reading's ceiling — which this tip is \
            arithmetic inside of — is the operand of the fee line",
        ..CandidateSpec::BASE
    },
    CandidateSpec {
        id: "tip.preflight_pin_to_build",
        pattern: PATTERN_E,
        producer: "preflight.eth_maxPriorityFeePerGas.pin",
        consumer: "build.eth_maxPriorityFeePerGas.pin",
        blockers: &["read_is_the_check", "each_step_prices_its_own_transaction"],
        requires_new_carrier: true,
        requires_new_lifecycle: true,
        evidence_strength: EVIDENCE_CODE_DERIVED,
        verdict_note: "a step of a sequence is not a weaker transaction than a lone one, which is \
            the code's stated reason for asking again",
        ..CandidateSpec::BASE
    },
    CandidateSpec {
        id: "nonce.preflight_to_build",
        pattern: PATTERN_E,
        producer: "preflight.eth_getTransactionCount.pair",
        consumer: "build.eth_getTransactionCount.pair",
        blockers: &["moving_target_is_the_point", "lane_allocation_advances_the_nonce"],
        requires_new_carrier: false,
        requires_new_lifecycle: true,
        evidence_strength: EVIDENCE_CODE_DERIVED,
        verdict_note: "the one category the build deliberately owns on a moving target: anything \
            between the two reads — a mined transaction, the lane's own allocation — is a different \
            answer, not a duplicate one",
        ..CandidateSpec::BASE
    },
    CandidateSpec {
        id: "header.observation_to_preflight_binding",
        pattern: PATTERN_C,
        producer: "observation.eth_getBlockByNumber.pin",
        consumer: "preflight.eth_getBlockByNumber.binding_pin",
        blockers: &["read_is_the_check", "safe_propagation_already_measured_no_saving"],
        requires_new_carrier: false,
        requires_new_lifecycle: false,
        evidence_strength: EVIDENCE_PROVEN,
        verdict_note: "§16's inherited row: the context travels, the read stays, and M8.4.4's own \
            aggregate measured the net saving at zero",
        ..CandidateSpec::BASE
    },
    CandidateSpec {
        id: "header.observation_to_build_binding",
        pattern: PATTERN_C,
        producer: "observation.eth_getBlockByNumber.pin",
        consumer: "build.eth_getBlockByNumber.binding",
        blockers: &["read_is_the_check", "safe_propagation_already_measured_no_saving"],
        requires_new_carrier: false,
        requires_new_lifecycle: false,
        evidence_strength: EVIDENCE_PROVEN,
        verdict_note: "the consumer verification M8.4.4 built the contract for, and the code says \
            plainly that the read replaces no read",
        ..CandidateSpec::BASE
    },
    CandidateSpec {
        id: "header.observation_to_simulation_header",
        pattern: PATTERN_C,
        producer: "observation.eth_getBlockByNumber.pin",
        consumer: "simulation.eth_getBlockByNumber.header",
        blockers: &["read_is_the_check", "safe_propagation_already_measured_no_saving"],
        requires_new_carrier: false,
        requires_new_lifecycle: false,
        evidence_strength: EVIDENCE_CODE_DERIVED,
        verdict_note: "the pin check is the reason the simulation's numbers describe the block the \
            opportunity was found on; serving it the observation's header would delete the check",
        ..CandidateSpec::BASE
    },
    CandidateSpec {
        id: "header.observation_to_preflight_fee",
        pattern: PATTERN_C,
        producer: "observation.eth_getBlockByNumber.pin",
        consumer: "preflight.eth_getBlockByNumber.fee_pin",
        blockers: &["read_is_the_check", "base_fee_priced_at_the_moment_of_pricing"],
        requires_new_carrier: false,
        requires_new_lifecycle: true,
        evidence_strength: EVIDENCE_CODE_DERIVED,
        verdict_note: "same height, same answer, and the answer is inside a price the run is \
            responsible for",
        ..CandidateSpec::BASE
    },
    CandidateSpec {
        id: "header.observation_to_build_fee",
        pattern: PATTERN_C,
        producer: "observation.eth_getBlockByNumber.pin",
        consumer: "build.eth_getBlockByNumber.fee_pin",
        blockers: &["read_is_the_check", "base_fee_priced_at_the_moment_of_pricing"],
        requires_new_carrier: false,
        requires_new_lifecycle: true,
        evidence_strength: EVIDENCE_CODE_DERIVED,
        verdict_note: "the build prices the transaction it is about to hand over, on its own \
            adapter, at its own moment",
        ..CandidateSpec::BASE
    },
    CandidateSpec {
        id: "header.preflight_head_to_preflight_nonce_pair",
        pattern: PATTERN_E,
        producer: "preflight.eth_getBlockByNumber.head_latest",
        consumer: "preflight.eth_getBlockByNumber.nonce_pair_head",
        blockers: &["tag_freshness_unproven", "two_calls_can_straddle_a_block"],
        requires_new_carrier: false,
        requires_new_lifecycle: false,
        evidence_strength: EVIDENCE_CODE_DERIVED,
        verdict_note: "both asks name `latest`, and the code's reason for reading number and hash \
            out of one header is that two calls can straddle a block; the record cannot say whether \
            they did (§4.2 forbids guessing what a tag resolved to)",
        ..CandidateSpec::BASE
    },
    CandidateSpec {
        id: "header.preflight_head_to_build_nonce_pair",
        pattern: PATTERN_E,
        producer: "preflight.eth_getBlockByNumber.head_latest",
        consumer: "build.eth_getBlockByNumber.nonce_pair_head",
        blockers: &["tag_freshness_unproven", "two_calls_can_straddle_a_block"],
        requires_new_carrier: false,
        requires_new_lifecycle: false,
        evidence_strength: EVIDENCE_CODE_DERIVED,
        verdict_note: "the same tag pair across a stage boundary, one block-time further apart",
        ..CandidateSpec::BASE
    },
    CandidateSpec {
        id: "eth_call.detection_to_preflight_reserves_leg0",
        pattern: PATTERN_B,
        producer: "opportunity_detection.eth_call.reserves",
        consumer: "preflight.eth_call.leg0",
        blockers: &["read_is_the_check", "block_changes_the_answer", "safe_saving_zero"],
        requires_new_carrier: true,
        requires_new_lifecycle: true,
        evidence_strength: EVIDENCE_PROVEN,
        verdict_note: "§14 names this family explicitly and forbids reading it as a P1: M8.5.1 \
            settled it as an independent recheck whose every blocker is `read_is_the_check`, and \
            the two asks answer for different heights, so it is not even a duplicate of the kind \
            §4.1 counts. Recorded here so no later agent re-asks it",
        ..CandidateSpec::BASE
    },
    CandidateSpec {
        id: "eth_call.detection_to_preflight_reserves_leg1",
        pattern: PATTERN_B,
        producer: "opportunity_detection.eth_call.reserves",
        consumer: "preflight.eth_call.leg1",
        blockers: &["read_is_the_check", "block_changes_the_answer", "safe_saving_zero"],
        requires_new_carrier: true,
        requires_new_lifecycle: true,
        evidence_strength: EVIDENCE_PROVEN,
        verdict_note: "the second leg of the same recheck, with the same verdict for the same \
            reason: the exit pool's reserves are asked to be re-read, not to be remembered",
        ..CandidateSpec::BASE
    },
    CandidateSpec {
        id: "l1fee.preflight_oracle",
        pattern: PATTERN_D,
        producer: "preflight.eth_call.l1_fee",
        consumer: "preflight.eth_call.l1_fee",
        blockers: &["no_duplicate_to_remove", "oracle_block_dependency_unproven"],
        requires_new_carrier: false,
        requires_new_lifecycle: false,
        requires_new_experiment: true,
        evidence_strength: EVIDENCE_CODE_DERIVED,
        verdict_note: "18 asks and 18 different calldata: there is no duplicate to reduce, and the \
            question §17 asks — whether the same calldata at a different height is one answer — is \
            not in any committed record",
        next_step: NEXT_STEP_EXPERIMENT,
        ..CandidateSpec::BASE
    },
    CandidateSpec {
        id: "block_number.observation_redundant_with_header",
        pattern: PATTERN_F,
        producer: "observation.eth_getBlockByNumber.pin",
        consumer: "observation.eth_blockNumber.pin",
        blockers: &["removal_would_replace_a_named_height_with_a_tag", "not_a_measured_duplicate"],
        requires_new_carrier: false,
        requires_new_lifecycle: false,
        evidence_strength: EVIDENCE_CODE_DERIVED,
        verdict_note: "§9's DERIVABLE case: the height the run fixes is also the height the \
            following header read names, so one of the two asks could in principle go — and the one \
            that would go is the ask that converts the moving head into a fixed height for \
            everything downstream, which is a semantic change, not a saving",
        ..CandidateSpec::BASE
    },
];

pub fn candidate_by_id(id: &str) -> Option<&'static CandidateSpec> {
    CANDIDATES.iter().find(|candidate| candidate.id == id)
}

// ---------------------------------------------------------------------------
// folding the records
// ---------------------------------------------------------------------------

/// One measured census group: everything §18's census column needs that is a fact about the
/// records, before any of this milestone's judgement is applied.
#[derive(Clone, Debug)]
pub struct Group {
    pub key: SiteKey,
    pub asks: usize,
    pub duration_ns: u64,
    pub per_run: BTreeMap<String, usize>,
    pub block_tags: BTreeMap<String, usize>,
    pub identity_head: Option<String>,
    pub asks_without_recorded_key: usize,
    pub sinks: BTreeMap<String, usize>,
    pub context_notes: BTreeMap<String, usize>,
}

/// Fold the rows into `(method, stage, caller_family)` groups. Sorted, keyed by the triple, so the
/// census table's row order is a property of the data and never of how a table was assembled —
/// §25's NC5 depends on that.
pub fn group_rows(rows: &[Value]) -> Vec<Group> {
    let mut groups: BTreeMap<SiteKey, Group> = BTreeMap::new();
    for row in rows {
        let key = SiteKey::of_row(row);
        let group = groups.entry(key.clone()).or_insert_with(|| Group {
            key,
            asks: 0,
            duration_ns: 0,
            per_run: BTreeMap::new(),
            block_tags: BTreeMap::new(),
            identity_head: None,
            asks_without_recorded_key: 0,
            sinks: BTreeMap::new(),
            context_notes: BTreeMap::new(),
        });
        group.asks += 1;
        group.duration_ns += row["duration_ns"].as_u64().unwrap_or_default();
        if let Some(run) = row["run"].as_str() {
            *group.per_run.entry(run.to_string()).or_insert(0) += 1;
        }
        let tag = row["block_tag"]
            .as_str()
            .map(str::to_string)
            .unwrap_or_else(|| "<none>".to_string());
        *group.block_tags.entry(tag).or_insert(0) += 1;
        match row["dedup_key"].as_str() {
            Some(key) => {
                if group.identity_head.is_none() {
                    group.identity_head =
                        Some(key.split('|').next().unwrap_or_default().to_string());
                }
            }
            None => group.asks_without_recorded_key += 1,
        }
        if let Some(sink) = row["sink"].as_str() {
            *group.sinks.entry(sink.to_string()).or_insert(0) += 1;
        }
        let note = row["context_note"]
            .as_str()
            .unwrap_or("context_stamped")
            .to_string();
        *group.context_notes.entry(note).or_insert(0) += 1;
    }
    groups.into_values().collect()
}

/// The measured facts about one candidate, folded out of the pairs M8.4.2 published: how many asks
/// the consumer site makes, how many of them repeat the producer's ask, which duplicate classes the
/// pairs land in, and what the pairs say about the block.
#[derive(Clone, Debug)]
pub struct Measured {
    pub consumer_asks: usize,
    pub producer_asks: usize,
    pub pairs: usize,
    pub exact_pairs: usize,
    pub other_class_pairs: usize,
    pub block_relations: BTreeMap<String, usize>,
    pub consumer_duration_ns: u64,
    /// §2.2's second layer, measured rather than asserted: how many of this candidate's pairs have
    /// all five of M8.4.2's reuse conditions resolved `checked_met`.
    pub reusable_in_principle_pairs: usize,
    /// §2.2's third layer: the pairs the record itself marks `safe_to_reuse`.
    pub safe_to_reuse_pairs: usize,
    /// Every condition's outcome tally over this candidate's pairs, so a level can be read back
    /// from the five fields it was computed from (§24's fifth and sixth rules).
    pub condition_outcomes: BTreeMap<String, BTreeMap<String, usize>>,
    pub duplicate_types: BTreeMap<String, usize>,
    /// The published id of each pair folded into this row, so the count is checkable against the
    /// record it came from.
    pub pair_ids: Vec<String>,
}

/// M8.4.2's level-B test on one published pair: every one of its five conditions resolved
/// `checked_met`. An outcome of `not_checkable_from_a_record` is not a pass — §2.4's rule that a
/// missing field is not evidence — and neither is `checkable_not_met`.
pub fn conditions_all_met(pair: &Value) -> bool {
    let conditions = pair["conditions"].as_array().cloned().unwrap_or_default();
    !conditions.is_empty()
        && conditions
            .iter()
            .all(|item| item["outcome"].as_str() == Some("checked_met"))
}

/// M8.4.2's level-C test on one published pair, read from the field the record carries rather than
/// inferred from the conditions.
pub fn pair_marked_safe(pair: &Value) -> bool {
    pair["safe_to_reuse"].as_bool().unwrap_or(false)
}

impl Measured {
    pub fn empty() -> Self {
        Self {
            consumer_asks: 0,
            producer_asks: 0,
            pairs: 0,
            exact_pairs: 0,
            other_class_pairs: 0,
            block_relations: BTreeMap::new(),
            consumer_duration_ns: 0,
            reusable_in_principle_pairs: 0,
            safe_to_reuse_pairs: 0,
            condition_outcomes: BTreeMap::new(),
            duplicate_types: BTreeMap::new(),
            pair_ids: Vec::new(),
        }
    }

    pub fn same_block_all(&self) -> bool {
        self.block_relations
            .keys()
            .all(|relation| relation == "same_block")
    }
}

/// Which side of a published pair is which site. The method comes from the row's canonical
/// `identity.method` field (a paramless method such as `eth_chainId` has no `method=` term to
/// parse out of the identity string), and the stage and caller out of the side object, so the
/// join uses exactly the same key the census uses.
pub fn pair_side_key(pair: &Value, side: &str) -> Option<SiteKey> {
    let method = pair["identity"]["method"].as_str()?.to_string();
    let side_object = &pair[side];
    Some(SiteKey {
        method,
        stage: side_object["stage"].as_str().map(str::to_string),
        caller_family: caller_family(side_object["caller"].as_str()),
    })
}

/// Fold the pairs onto the candidate table. Pair *counts* come only from the records: a candidate
/// that names a shape the records never produced keeps `pairs = 0`, which is how §17's Candidate 1
/// ends up with a zero theoretical saving rather than an invented one.
pub fn measured(spec: &CandidateSpec, groups: &[Group], pairs: &[Value]) -> Measured {
    let producer_key = spec
        .producer_site()
        .unwrap_or_else(|| {
            panic!(
                "candidate {} names an unknown producer site {}",
                spec.id, spec.producer
            )
        })
        .key();
    let consumer_key = spec.consumer_site().key();
    let mut out = Measured::empty();
    for group in groups {
        if group.key == consumer_key {
            out.consumer_asks = group.asks;
            out.consumer_duration_ns = group.duration_ns;
        }
        if group.key == producer_key {
            out.producer_asks = group.asks;
        }
    }
    for pair in pairs {
        let Some(producer) = pair_side_key(pair, "producer") else {
            continue;
        };
        let Some(consumer) = pair_side_key(pair, "consumer") else {
            continue;
        };
        let same_site = spec.producer.is_empty() || spec.producer == spec.consumer;
        let matched = if same_site {
            producer == consumer && consumer == consumer_key
        } else {
            producer == producer_key && consumer == consumer_key
        };
        if !matched {
            continue;
        }
        out.pairs += 1;
        let class = pair["duplicate_type"].as_str().unwrap_or_default();
        if class == "exact_duplicate" {
            out.exact_pairs += 1;
        } else {
            out.other_class_pairs += 1;
        }
        *out.duplicate_types.entry(class.to_string()).or_insert(0) += 1;
        if conditions_all_met(pair) {
            out.reusable_in_principle_pairs += 1;
        }
        if pair_marked_safe(pair) {
            out.safe_to_reuse_pairs += 1;
        }
        for condition in pair["conditions"].as_array().cloned().unwrap_or_default() {
            let name = condition["condition"]
                .as_str()
                .unwrap_or("unrecorded")
                .to_string();
            let outcome = condition["outcome"]
                .as_str()
                .unwrap_or("unrecorded")
                .to_string();
            *out.condition_outcomes
                .entry(name)
                .or_default()
                .entry(outcome)
                .or_insert(0) += 1;
        }
        if let Some(id) = pair["candidate_id"].as_str() {
            out.pair_ids.push(id.to_string());
        }
        *out.block_relations
            .entry(
                pair["block_relation"]
                    .as_str()
                    .unwrap_or("unrecorded")
                    .to_string(),
            )
            .or_insert(0) += 1;
    }
    out
}

/// Every exact-duplicate shape in the records, keyed by its producer and consumer site. §6's
/// candidate table must cover all of them and no more.
pub fn measured_shapes(pairs: &[Value]) -> BTreeMap<(SiteKey, SiteKey), usize> {
    let mut shapes: BTreeMap<(SiteKey, SiteKey), usize> = BTreeMap::new();
    for pair in pairs {
        if pair["duplicate_type"].as_str() != Some("exact_duplicate") {
            continue;
        }
        let (Some(producer), Some(consumer)) = (
            pair_side_key(pair, "producer"),
            pair_side_key(pair, "consumer"),
        ) else {
            continue;
        };
        *shapes.entry((producer, consumer)).or_insert(0) += 1;
    }
    shapes
}

/// Every directed pair shape in the records, whatever its duplicate class. §6's candidate table
/// may adjudicate any of them; [`measured_shapes`] is the narrower set every row of which a
/// candidate *must* cover.
pub fn measured_pair_shapes(pairs: &[Value]) -> BTreeMap<(SiteKey, SiteKey), usize> {
    let mut shapes: BTreeMap<(SiteKey, SiteKey), usize> = BTreeMap::new();
    for pair in pairs {
        let (Some(producer), Some(consumer)) = (
            pair_side_key(pair, "producer"),
            pair_side_key(pair, "consumer"),
        ) else {
            continue;
        };
        *shapes.entry((producer, consumer)).or_insert(0) += 1;
    }
    shapes
}

/// §24's completeness rule in code: no exact-duplicate shape in the records without a candidate,
/// and no candidate naming a pair shape the records never produced — a candidate has to be an
/// answer to something measured, which is what keeps §17's questions (an oracle with no duplicate,
/// a derivable height) from becoming a licence to invent one.
pub fn shape_completeness_errors(pairs: &[Value]) -> Vec<String> {
    let shapes = measured_shapes(pairs);
    let all_shapes = measured_pair_shapes(pairs);
    let mut errors = Vec::new();
    let mut covered: BTreeMap<(SiteKey, SiteKey), &str> = BTreeMap::new();
    for spec in CANDIDATES {
        let producer = spec
            .producer_site()
            .map(|site| site.key())
            .unwrap_or_else(|| spec.consumer_site().key());
        let consumer = spec.consumer_site().key();
        let key = (producer, consumer);
        if let Some(previous) = covered.insert(key.clone(), spec.id) {
            errors.push(format!(
                "two candidates cover one shape: {previous} and {}",
                spec.id
            ));
        }
        let question_not_a_duplicate = spec.pattern == PATTERN_D || spec.pattern == PATTERN_F;
        if !all_shapes.contains_key(&key) && !question_not_a_duplicate {
            errors.push(format!(
                "candidate {} names a shape the records never produced: {} -> {}",
                spec.id,
                key.0.label(),
                key.1.label()
            ));
        }
    }
    for (key, count) in &shapes {
        if !covered.contains_key(key) {
            errors.push(format!(
                "{count} exact-duplicate pairs have no candidate: {} -> {}",
                key.0.label(),
                key.1.label()
            ));
        }
    }
    errors
}

/// §12's rule, applied to a candidate: what happens to the verification if the read goes.
pub fn verification_preserved(spec: &CandidateSpec) -> bool {
    let consumer = spec.consumer_site();
    if !is_check_role(consumer.verification_role) {
        return true;
    }
    consumer.alternative_verification == ALT_EXISTING_OTHER_CHECK
}

/// §9/§10's freshness verdict for a candidate shape: whether serving the consumer from the
/// producer's answer keeps the state the consumer reads as fresh as the code requires. Derived
/// from the inherited ownership row and the block relation the pairs actually carry — never from a
/// guess about what a tag resolved to.
pub fn freshness_of(measured: &Measured, ownership: &Value) -> &'static str {
    let identity = ownership["identity"].as_str().unwrap_or_default();
    let tag_semantics = ownership["block_tag_semantics"]
        .as_str()
        .unwrap_or_default();
    // A read with no block term in its identity cannot go stale between two asks of it, which is
    // M8.4.3's own wording for `chain_identity` and `fee_parameters`.
    if identity == "not_block_scoped" || identity == "no_block_term" || tag_semantics == "absent" {
        return FRESHNESS_PRESERVED;
    }
    // A category whose recorded asks name a tag: §4.2 forbids guessing what the tag resolved to,
    // so no pair of them can be shown to be one answer.
    if tag_semantics == "tag" {
        return FRESHNESS_NOT_PRESERVED;
    }
    if measured.block_relations.is_empty() {
        return FRESHNESS_UNPROVEN;
    }
    let one_answer = measured
        .block_relations
        .keys()
        .all(|relation| relation == "same_block" || relation == "no_block_in_either_request");
    if one_answer {
        return FRESHNESS_PRESERVED;
    }
    FRESHNESS_NOT_PRESERVED
}

/// §13: the theoretical ceiling of a candidate is the asks it could take away if every question
/// about identity, ownership, freshness and authority were answered in its favour. It is computed
/// from the pairs, never typed.
pub fn theoretical_saving_of(measured: &Measured) -> usize {
    measured.exact_pairs
}

/// §13's other half: the saving the census can credit. A candidate earns nothing unless every
/// condition is met, and each condition is one of §2.1's ordering rules or §14's P0 clauses.
pub fn safe_saving_of(
    spec: &CandidateSpec,
    measured: &Measured,
    freshness: &str,
    ownership: &Value,
) -> usize {
    let consumer = spec.consumer_site();
    let theoretical = theoretical_saving_of(measured);
    if theoretical == 0 {
        return 0;
    }
    if !verification_preserved(spec) {
        return 0;
    }
    if freshness != FRESHNESS_PRESERVED {
        return 0;
    }
    if spec.requires_new_carrier || spec.requires_new_lifecycle || spec.requires_new_experiment {
        return 0;
    }
    if !spec.safe_reuse_proven {
        return 0;
    }
    if ownership["reuse_status"]["check_would_stop_existing"]
        .as_bool()
        .unwrap_or(false)
    {
        return 0;
    }
    if is_check_role(consumer.verification_role) {
        return 0;
    }
    theoretical
}

/// §14's five labels as one decision list over measured fields. The order *is* the rule: §2.1's
/// ordering, then §12's hard block, then §14's own REJECT list, and only then P1/P2/P3, where what
/// separates P1 from P2 is how far the evidence reaches, not how much time the calls took.
pub fn priority_of(
    spec: &CandidateSpec,
    measured: &Measured,
    freshness: &str,
    safe_saving: usize,
) -> &'static str {
    let theoretical = theoretical_saving_of(measured);
    let consumer = spec.consumer_site();
    if safe_saving >= 1
        && measured.exact_pairs > 0
        && verification_preserved(spec)
        && !spec.requires_new_carrier
        && !spec.requires_new_lifecycle
        && !spec.requires_new_experiment
        && spec.safe_reuse_proven
    {
        return PRIORITY_P0;
    }
    if consumer.alternative_verification == ALT_NO_ALTERNATIVE {
        return PRIORITY_REJECT;
    }
    if theoretical == 0 && !spec.requires_new_experiment {
        return PRIORITY_REJECT;
    }
    if freshness == FRESHNESS_NOT_PRESERVED {
        return PRIORITY_REJECT;
    }
    if spec.pattern == PATTERN_C {
        return PRIORITY_REJECT;
    }
    if spec.requires_new_carrier || spec.requires_new_lifecycle || spec.requires_new_experiment {
        if spec.evidence_strength == EVIDENCE_PROVEN
            || spec.evidence_strength == EVIDENCE_CODE_DERIVED
        {
            return PRIORITY_P1;
        }
        return PRIORITY_P2;
    }
    PRIORITY_P3
}

/// §21's `implementation_risk` column and §7's fourth measure both need this one: how much new
/// machinery a candidate requires before it can save anything, expressed as words and as the rank
/// the queue sorts by. Derived from the three `requires_*` fields, never typed beside them.
pub fn implementation_risk_of(spec: &CandidateSpec) -> (&'static str, u8) {
    if spec.requires_new_experiment {
        ("needs_a_real_chain_experiment", 4)
    } else if spec.requires_new_carrier && spec.requires_new_lifecycle {
        ("needs_a_new_carrier_and_ownership_rule", 3)
    } else if spec.requires_new_carrier {
        ("needs_a_new_carrier", 2)
    } else if spec.requires_new_lifecycle {
        ("needs_a_new_ownership_rule", 1)
    } else {
        ("none", 0)
    }
}

// ---------------------------------------------------------------------------
// §24's invariants, in one function the gates and the negative controls share
// ---------------------------------------------------------------------------

/// The rules that make a table legal. Every §25 negative control is one of these firing, which is
/// why the controls do not need bespoke assertions: mutating an identity, a verification role, a
/// safe saving or a theoretical/safe split changes a row and this function refuses the row.
pub fn candidate_invariant_errors(candidate: &Value) -> Vec<String> {
    let mut errors = Vec::new();
    let id = candidate["candidate"].as_str().unwrap_or("<unnamed>");
    let theoretical = candidate["theoretical_saving"].as_u64().unwrap_or_default();
    let safe = candidate["safe_saving"].as_u64().unwrap_or_default();
    let role = candidate["verification_role"].as_str().unwrap_or_default();
    let alternative = candidate["alternative_verification"]
        .as_str()
        .unwrap_or_default();
    let freshness = candidate["freshness"].as_str().unwrap_or_default();
    let pattern = candidate["pattern"].as_str().unwrap_or_default();
    let priority = candidate["priority"].as_str().unwrap_or_default();
    let blockers = candidate["blockers"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let proof = &candidate["safe_reuse_proof"];
    let measured_pairs = candidate["duplicate_count_exact"]
        .as_u64()
        .unwrap_or_default();

    if safe > theoretical {
        errors.push(format!(
            "{id}: safe_saving {safe} exceeds the theoretical ceiling {theoretical}, which §13 \
             forbids: a safe saving is a subset of the ceiling, never an addition to it"
        ));
    }
    let proof_kind = proof
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if safe > 0 && !CREDITING_PROOFS.contains(&proof_kind) {
        errors.push(format!(
            "{id}: safe_saving {safe} with proof {proof_kind:?}. §24's sixth figure has to be \
             recomputable, and the only two kinds that credit a saving are {CREDITING_PROOFS:?} — \
             which is what §25's NC3 fires on: raising `safe_saving` from 0 to 1 without adding one \
             of them, and the record rows behind it, changes nothing but the arithmetic's honesty"
        ));
    }
    if !PROOFS.contains(&proof_kind) {
        errors.push(format!(
            "{id}: safe_reuse_proof kind {proof_kind:?} is not one of {PROOFS:?}"
        ));
    }
    if safe > 0 && is_check_role(role) && alternative != ALT_EXISTING_OTHER_CHECK {
        errors.push(format!(
            "{id}: safe_saving {safe} while the consumer's role is {role} and its alternative is \
             {alternative}. §11's default is that this is the check, and §25's NC4 is exactly this \
             row"
        ));
    }
    if role == ROLE_NONE
        && candidate["role_none_reason"]
            .as_str()
            .unwrap_or_default()
            .is_empty()
    {
        errors.push(format!(
            "{id}: role NONE with no reason named. §24's eighth rule is that a verification role \
             cannot be ignored, and the way to answer it for a read that verifies nothing is to say \
             what the read is for instead — which is also why §25's NC2 fails: blanking \
             `read_is_the_check` to NONE leaves the reason empty"
        ));
    }
    if safe > 0 && freshness != FRESHNESS_PRESERVED {
        errors.push(format!(
            "{id}: safe_saving {safe} with freshness {freshness}"
        ));
    }
    if safe == 0 && measured_pairs > 0 && blockers.is_empty() && pattern != PATTERN_F {
        errors.push(format!(
            "{id}: {measured_pairs} exact-duplicate pairs earn nothing and name no blocker. §20's \
             rejected opportunities exist so that no later agent has to re-ask this question, and \
             a rejection without a reason is not an answer"
        ));
    }
    if !PRIORITIES.contains(&priority) {
        errors.push(format!(
            "{id}: priority {priority} is not one of §14's five labels"
        ));
    }
    if !SEMANTIC_CLASSES.contains(&candidate["semantic_class"].as_str().unwrap_or_default()) {
        errors.push(format!(
            "{id}: semantic class {:?} is not one of §5's nine",
            candidate["semantic_class"].as_str().unwrap_or_default()
        ));
    }
    if !VERIFICATION_ROLES.contains(&role) {
        errors.push(format!(
            "{id}: verification role {role:?} is not one of §11's six"
        ));
    }
    if !ALTERNATIVES.contains(&alternative) {
        errors.push(format!(
            "{id}: alternative verification {alternative:?} is not one of §12's four"
        ));
    }
    if is_check_role(role) && alternative == ALT_NOT_APPLICABLE {
        errors.push(format!(
            "{id}: role {role} with alternative NOT_APPLICABLE. §12 asks a check role a question \
             it cannot decline — if this read goes, who bears the duty — and NOT_APPLICABLE is the \
             answer only for a read that checks nothing"
        ));
    }
    let claims_the_read_is_the_check = blockers
        .iter()
        .any(|blocker| blocker.as_str() == Some("read_is_the_check"));
    if claims_the_read_is_the_check && !is_check_role(role) {
        errors.push(format!(
            "{id}: a blocker says the read IS the check while {role} says the read verifies \
             nothing. §25's NC2 is this row: blanking `read_is_the_check` to NONE does not delete \
             the decision line it names"
        ));
    }
    if candidate["duplicate_status"]["identity"]["uses_row_index"]
        .as_bool()
        .unwrap_or(false)
    {
        errors.push(format!(
            "{id}: an identity built from a row index (§24's twelfth rule)"
        ));
    }
    errors
}

/// §24's rules over the census table: one row per measured group, no group unassigned, every
/// judgement column answered, no duration used as an identity or as a sort key.
pub fn census_invariant_errors(rows: &[Value], groups: &[Group]) -> Vec<String> {
    let mut errors = Vec::new();
    let mut seen: BTreeSetString = BTreeSetString::default();
    if rows.len() != groups.len() {
        errors.push(format!(
            "the census has {} rows and the records have {} groups",
            rows.len(),
            groups.len()
        ));
    }
    let mut asks = 0usize;
    for row in rows {
        let key = row["key"].as_str().unwrap_or("<unnamed>").to_string();
        if !seen.insert(key.clone()) {
            errors.push(format!("census key {key} appears twice"));
        }
        asks += row["asks"].as_u64().unwrap_or_default() as usize;
        for field in [
            "semantic_class",
            "verification_role",
            "alternative_verification",
            "owner",
            "authority",
            "freshness",
            "invalidation",
            "carrier",
            "identity",
            "block_semantics",
            "priority",
            "class_basis",
        ] {
            if row[field].as_str().unwrap_or_default().is_empty() {
                errors.push(format!("census row {key} has no {field}"));
            }
        }
        if !SEMANTIC_CLASSES.contains(&row["semantic_class"].as_str().unwrap_or_default()) {
            errors.push(format!("census row {key} has an unknown §5 class"));
        }
        if !VERIFICATION_ROLES.contains(&row["verification_role"].as_str().unwrap_or_default()) {
            errors.push(format!("census row {key} has an unknown §11 role"));
        }
        if !CENSUS_PRIORITIES.contains(&row["priority"].as_str().unwrap_or_default()) {
            errors.push(format!(
                "census row {key} has priority {:?}, which is neither one of §14's five candidate \
                 labels nor {PRIORITY_NOT_A_CANDIDATE}",
                row["priority"].as_str().unwrap_or_default()
            ));
        }
        if row["duration_rank_as_identity"].as_bool().unwrap_or(false) {
            errors.push(format!(
                "census row {key} uses a duration-sorted index as identity"
            ));
        }
    }
    let total: usize = groups.iter().map(|group| group.asks).sum();
    if asks != total {
        errors.push(format!(
            "the census's asks sum to {asks} and the records hold {total}, which §24's first rule \
             forbids: one table, one count"
        ));
    }
    errors
}

/// A tiny set that keeps insertion order out of the picture: the census's row identity is a string
/// and duplicates are an error, so a `BTreeSet` of the keys is all the bookkeeping needed.
#[derive(Default)]
pub struct BTreeSetString(pub std::collections::BTreeSet<String>);

impl BTreeSetString {
    pub fn insert(&mut self, value: String) -> bool {
        self.0.insert(value)
    }
}

/// §14's rule that a priority may not be read off the table's timing. The check is a falsifiable
/// one: find a pair of candidates where the *slower* one carries the *worse* label. If priority
/// were a duration sort, no such pair could exist — so the pairs' presence proves the labels came
/// from somewhere else, and their absence fails the gate rather than passing it quietly.
pub fn priority_duration_inversions(rows: &[Value]) -> Vec<(String, String)> {
    let rank_of = |row: &Value| -> u8 {
        let label = row["priority"].as_str().unwrap_or_default();
        PRIORITIES
            .iter()
            .position(|p| *p == label)
            .unwrap_or(PRIORITIES.len()) as u8
    };
    let mut out = Vec::new();
    for a in rows {
        for b in rows {
            let (slow, fast) = (
                a["total_duration_ms"].as_f64().unwrap_or_default(),
                b["total_duration_ms"].as_f64().unwrap_or_default(),
            );
            if slow > fast && rank_of(a) > rank_of(b) {
                out.push((
                    a["candidate"].as_str().unwrap_or_default().to_string(),
                    b["candidate"].as_str().unwrap_or_default().to_string(),
                ));
            }
        }
    }
    out
}

/// The same rule as a verdict: at least one inversion means the labels are not the clock's order.
pub fn priority_is_not_a_duration_proxy(rows: &[Value]) -> bool {
    !priority_duration_inversions(rows).is_empty()
}

/// §21's ordering as one tuple: the priority band first (a label §14 already derives from saving
/// over risk), then inside a band by saving down, by how much new machinery it needs up, by how
/// far its evidence reaches, and by id so the order is total and reproducible. No duration appears
/// at any position, which is what makes §24's tenth rule hold by construction rather than by
/// assertion.
pub fn queue_sort_key(row: &Value) -> (u8, u64, u8, u8, String) {
    let label = row["priority"].as_str().unwrap_or_default();
    let rank = PRIORITIES
        .iter()
        .position(|p| *p == label)
        .unwrap_or(PRIORITIES.len()) as u8;
    let strength = match row["evidence_strength"].as_str().unwrap_or_default() {
        EVIDENCE_PROVEN => 0,
        EVIDENCE_CODE_DERIVED => 1,
        EVIDENCE_RECORD_ONLY => 2,
        _ => 3,
    };
    (
        rank,
        // Flipped so an ascending sort of the tuple puts the larger saving first.
        u64::MAX - row["safe_saving"].as_u64().unwrap_or_default(),
        row["implementation_risk_rank"].as_u64().unwrap_or_default() as u8,
        strength,
        row["candidate"].as_str().unwrap_or_default().to_string(),
    )
}

/// The M8.4.2 §8 identity of a row, read through the published machinery so the census cannot
/// invent a fifth key grammar. `dedup_key` absent means the method carries no terms and §8's
/// declared rule keys it by method and endpoint digest.
pub fn identity_of_row(row: &Value, chain_id: Option<u64>) -> String {
    let key = RpcReadKey::of_row(row, chain_id);
    if let Some(identity) = key.identity() {
        return identity;
    }
    format!(
        "paramless|{}|{}",
        row["method"].as_str().unwrap_or_default(),
        row["endpoint_id"].as_str().unwrap_or_default()
    )
}

/// M8.4.2's category, so a census row can be reconciled against the cross-stage tables row for row.
pub fn category_of(
    method: &str,
    sink: Option<&str>,
    caller: Option<&str>,
) -> (&'static str, String) {
    read_category(method, sink, caller)
}

// ---------------------------------------------------------------------------
// reading the committed records
// ---------------------------------------------------------------------------

/// The corpus, read once: the asks, the pairs M8.4.2 published over them, and the three inherited
/// verdict tables the census looks its judgement columns up in. §2.4's "existing evidence first" is
/// a type here — nothing in this milestone's code path can ask a node anything, because the only
/// way in is a directory of files that already exist.
///
/// `Clone` exists for §25's NC6: the control re-derives the semantic columns from a copy of the
/// corpus whose clock has been multiplied, and the two derivations have to agree.
#[derive(Clone)]
pub struct Records {
    pub root: std::path::PathBuf,
    pub runs: Vec<String>,
    pub rows: Vec<Value>,
    pub pairs: Vec<Value>,
    pub ownership: BTreeMap<String, Value>,
    pub propagation: Value,
    pub eth_call: Value,
    /// M8.4.2's aggregate pair record, root level: the published totals the census reconciles
    /// against, not only its rows.
    pub candidates: Value,
    pub provenance: Value,
}

pub fn read_json(path: &std::path::Path) -> Value {
    let bytes =
        std::fs::read(path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    serde_json::from_slice(&bytes)
        .unwrap_or_else(|error| panic!("parse {}: {error}", path.display()))
}

fn subdirectories(root: &std::path::Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(root)
        .unwrap_or_else(|error| panic!("{}: {error}", root.display()))
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.path().is_dir())
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .collect();
    names.sort();
    names
}

impl Records {
    /// The corpus is read, never asked. Each run contributes its call rows from
    /// `pipeline-calls.json` and its own identity from `pipeline-summary.json`'s `assembled_from`
    /// entry — that is where the published `chain_id`, `block_number` and `endpoint_id` live; the
    /// call rows carry no chain field of their own, so a census key must not pretend otherwise.
    pub fn load(workspace_root: &std::path::Path) -> Self {
        let runs_dir = workspace_root.join(RUNS_DIR);
        let runs = subdirectories(&runs_dir);
        assert!(!runs.is_empty(), "no runs under {RUNS_DIR}");
        let mut rows = Vec::new();
        let mut provenance = Vec::new();
        for run in &runs {
            let table = read_json(&runs_dir.join(run).join(CALLS_FILE));
            for row in table["rows"].as_array().expect("rows") {
                rows.push(row.clone());
            }
            let summary = read_json(&runs_dir.join(run).join(SUMMARY_FILE));
            let assembled = summary["assembled_from"]
                .as_array()
                .expect("assembled_from")
                .clone();
            let entry = assembled
                .iter()
                .find(|item| item["run"].as_str() == Some(run))
                .unwrap_or_else(|| panic!("{SUMMARY_FILE}: no assembled_from entry for run {run}"));
            assert_eq!(
                entry["calls"].as_u64().unwrap_or_default() as usize,
                table["rows"].as_array().map_or(0, |list| list.len()),
                "run {run}: {SUMMARY_FILE} says {} calls, {} rows are recorded",
                entry["calls"],
                table["rows"].as_array().map_or(0, |list| list.len())
            );
            provenance.push(json!({
                "run": run,
                "chain_id": entry["chain_id"],
                "block_number": entry["block_number"],
                "endpoint_id": entry["endpoint_id"],
                "source": entry["source"],
                "execution_mode": entry["execution_mode"],
                "git_revision": entry["git_revision"],
                "generated_at_unix_ms": entry["generated_at_unix_ms"],
                "rows": table["rows"].as_array().map(|list| list.len()),
                "diagnosis_schema": table["diagnosis_schema"],
                "clock": table["clock"],
            }));
        }
        let candidate_rows = read_json(&workspace_root.join(CANDIDATES_RECORD));
        let pairs = candidate_rows["rows"]
            .as_array()
            .expect("candidate rows")
            .clone();
        let ownership_table = read_json(&workspace_root.join(OWNERSHIP_RECORD));
        let mut ownership = BTreeMap::new();
        for row in ownership_table["rows"].as_array().expect("ownership rows") {
            ownership.insert(
                row["state_kind"].as_str().expect("state_kind").to_string(),
                row.clone(),
            );
        }
        Self {
            root: workspace_root.to_path_buf(),
            runs,
            rows,
            pairs,
            ownership,
            propagation: read_json(&workspace_root.join(PROPAGATION_RECORD)),
            eth_call: read_json(&workspace_root.join(ETH_CALL_RECORD)),
            candidates: candidate_rows,
            provenance: json!(provenance),
        }
    }

    /// The one chain id the corpus publishes. All runs must agree, because a census that blended
    /// chains would blend identities.
    pub fn chain_id(&self) -> u64 {
        let mut seen: Vec<u64> = Vec::new();
        for entry in self.provenance.as_array().expect("provenance") {
            let chain = entry["chain_id"]
                .as_u64()
                .unwrap_or_else(|| panic!("provenance entry has no chain id: {entry}"));
            if !seen.contains(&chain) {
                seen.push(chain);
            }
        }
        assert_eq!(seen.len(), 1, "the corpus spans chains {seen:?}");
        seen[0]
    }

    /// The one endpoint digest the corpus published, same rule as [`Records::chain_id`].
    pub fn endpoint_id(&self) -> String {
        let mut seen: Vec<String> = Vec::new();
        for entry in self.provenance.as_array().expect("provenance") {
            let endpoint = entry["endpoint_id"]
                .as_str()
                .unwrap_or_else(|| panic!("provenance entry has no endpoint id: {entry}"))
                .to_string();
            if !seen.contains(&endpoint) {
                seen.push(endpoint);
            }
        }
        assert_eq!(seen.len(), 1, "the corpus spans endpoints {seen:?}");
        seen[0].clone()
    }

    pub fn runs(&self) -> usize {
        self.runs.len()
    }

    pub fn groups(&self) -> Vec<Group> {
        group_rows(&self.rows)
    }

    pub fn ownership_of(&self, site: &Site) -> Value {
        self.ownership
            .get(site.state_kind)
            .cloned()
            .unwrap_or_else(|| {
                panic!(
                    "site {} names state kind {} and no ownership row carries it",
                    site.id, site.state_kind
                )
            })
    }
}

/// Every measured pair whose consumer is this site, split by duplicate class.
pub fn pairs_into(key: &SiteKey, pairs: &[Value]) -> (usize, usize, usize) {
    let (mut exact, mut other, mut all) = (0usize, 0usize, 0usize);
    for pair in pairs {
        if pair_side_key(pair, "consumer").as_ref() == Some(key) {
            all += 1;
            if pair["duplicate_type"].as_str() == Some("exact_duplicate") {
                exact += 1;
            } else {
                other += 1;
            }
        }
    }
    (all, exact, other)
}

/// The record row whose identity the census publishes for a site. A site can carry several recorded
/// keys — [`recorded_key_variants`] counts them and the table publishes that figure — so this choice
/// has to be a function of the group's key set, not of where in the files its rows happened to sit.
/// The rank is the smallest recorded `dedup_key`, which is what the identity string is built from;
/// rows that carry no key sort last, and among them the smallest `(run, rpc_id)`, which no printed
/// column can turn into drift. Picking the first row read instead would make a printed identity move
/// when the corpus is re-read in another order — §24's twelfth rule and §25's NC5 in miniature.
pub fn representative_rows(rows: &[Value]) -> BTreeMap<SiteKey, Value> {
    let rank = |row: &Value| {
        let recorded = row["dedup_key"].as_str();
        (
            u8::from(recorded.is_none()),
            recorded.unwrap_or_default().to_string(),
            row["run"].as_str().unwrap_or_default().to_string(),
            row["rpc_id"].as_u64().unwrap_or_default(),
        )
    };
    let mut ordered: Vec<&Value> = rows.iter().collect();
    ordered.sort_by_key(|row| rank(row));
    let mut out: BTreeMap<SiteKey, Value> = BTreeMap::new();
    for row in ordered {
        out.entry(SiteKey::of_row(row))
            .or_insert_with(|| (*row).clone());
    }
    out
}

/// Nanoseconds to the two-decimal millisecond every duration column publishes. `pub` because
/// §24's thirteenth rule recomputes those columns inside the gate, and a recompute that rounds
/// differently from the writer would report a difference that is only formatting.
pub fn ms(ns: u64) -> f64 {
    (ns as f64 / 1_000_000f64 * 100f64).round() / 100f64
}
