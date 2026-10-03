//! M8.4.2 §8's cross-pipeline read identity, §4's duplicate classes, and §6's three
//! reuse verdicts — computed from the records M8.4.1 already publishes.
//!
//! ## What this module is for
//!
//! M8.4.2 asks one question (§1): do two pipeline stages ask a node for the same
//! thing? Answering it needs one thing the record did not have: a way to say *which
//! asks are the same ask*, across two stages that each wrote their own row. That is
//! this module's whole job — §8's `RpcReadKey`, §4's duplicate classes, §6's reuse
//! verdicts, and nothing else.
//!
//! It is deliberately a *reader* of [`crate::diagnosis`]'s rows and of
//! [`evm_chain::describe_call`]'s key, not a second recorder. The reason is §3's
//! instruction — 「优先复用 M8.4.1 已经建立的 evidence/schema，不要重新建立第二套 RPC
//! tracing 系统」 — plus §2's ban on changing any behaviour: a key minted here cannot
//! move a request, and a key minted at the choke point would have meant editing the
//! trace line, which would have moved every number M8.4.1 committed. So:
//!
//! ```text
//! describe_call (crates/chain/src/rpc_trace.rs)   the one normalizer, at record time
//!         ↓  recorded as `dedup_key`
//! pipeline_call_row (diagnosis.rs)                one row per call, both sinks
//!         ↓  read here, immutably
//! RpcReadKey (this module)                        §8's terms, §9's category
//!         ↓
//! classify_pair / reuse verdict                   §4's classes, §6's three levels
//! ```
//!
//! ## The one rule this module adds, and why it needed adding
//!
//! §9 names six read categories and requires `eth_chainId` to *not* be called a state
//! read. M8.4.1's key list (§12) covers seven methods and refuses every other one, so
//! `eth_chainId`, `eth_blockNumber` and `eth_maxPriorityFeePerGas` arrive here with no
//! `dedup_key` at all — 11 of the 235 rows M8.4.1 committed. Those asks are exactly the
//! ones §19 asks about (chain id, fee), so a key rule that stops at the seven would
//! answer §19 by shrugging. This module gives a parameterless method an identity of its
//! own (§8's field list, with the absent fields absent) and states the cost: the answer
//! to `eth_chainId` is a property of the endpoint rather than of a block, so §6's
//! block-identity condition can never be *met* by such a pair, only waived, and this
//! build does not waive it.
//!
//! ## What may not be read out of these keys
//!
//! A key that matches is §4's *duplicate*, nothing more. §6's `safe-to-reuse` needs four
//! conditions and this module can only ever see two of them (block identity and the
//! recorded terms); the other two — whether the state means the same thing to both
//! callers, and whether anything in the lifecycle is entitled to hold the first answer —
//! are claims about code that a row does not carry. Every verdict here therefore names
//! which conditions it checked and which it could not, and `safe_to_reuse` is computed
//! from that list rather than asserted.
//!
//! ```text
//! what this can prove        two asks are the same ask, or differ in a named way
//! what this cannot prove     that the second ask could have been served by the first
//! what this never does       changes a request, a cache, a schedule, or a decision (§2)
//! ```

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{json, Value};

/// The version of the identity and classification rules in this file.
///
/// Published beside every table that uses them, so a reader of `duplicate-matrix.json`
/// knows which §4 rules produced a class. A later milestone that changes a rule changes
/// this number; the committed tables then stay readable as what the old rule said.
pub const CANONICALIZATION_SCHEMA: u64 = 1;

// ---------------------------------------------------------------------------
// §8: the canonical identity
// ---------------------------------------------------------------------------

/// Where a key's identity term came from, one of three.
///
/// The third is not a failure of the caller: a method M8.4.1's §12 list does not cover,
/// or params that did not arrive in the shape that rule assumes, has no identity to
/// compare — and §12's own note is carried through rather than replaced by a guess.
pub const IDENTITY_FROM_RECORDED_KEY: &str = "identity_from_the_recorded_dedup_key";
pub const IDENTITY_FROM_PARAMLESS_REQUEST: &str = "identity_from_a_parameterless_request";
pub const IDENTITY_UNAVAILABLE: &str = "identity_unavailable";

/// How a key's block term reads, which decides what §23 can say about freshness.
///
/// A number names one block. A tag (`latest`, `pending`, `safe`, `finalized`) names
/// "whatever the head is when the node answers", which is a different kind of thing: two
/// asks that both spell `latest` are not two asks for the same state, and this build has
/// no record of what either was answered with. `absent` is a method that carries no block
/// at all — §9's `chain_identity` and `transaction_preparation` reads mostly live there.
pub const BLOCK_FORM_NUMBER: &str = "number";
pub const BLOCK_FORM_TAG: &str = "tag";
pub const BLOCK_FORM_ABSENT: &str = "absent";
pub const BLOCK_FORMS: [&str; 3] = [BLOCK_FORM_NUMBER, BLOCK_FORM_TAG, BLOCK_FORM_ABSENT];

/// Whether a recorded block term names one height, as opposed to naming a *way* to pick
/// one — which is what every tag does.
///
/// Decided by shape rather than by a list of tag names: a list would silently file a tag it
/// does not know as a number, which is the one misclassification §23 cannot recover from,
/// and the §18/§19 ban on asking the node for the moving head is held by a scan that reads
/// production source as text, where a vocabulary table would look like a call site.
fn term_names_a_block_height(term: &str) -> bool {
    let digits = term
        .strip_prefix("0x")
        .or_else(|| term.strip_prefix("0X"))
        .unwrap_or(term);
    !digits.is_empty() && digits.chars().all(|c| c.is_ascii_hexdigit())
}

/// §8's canonical identity of one ask.
///
/// The fields are §8's list, in §8's order, with two of them merged: §8 asks for
/// `block_number` and `block_tag` separately, and this build cannot fill both — the
/// record stores one block term, which is either a height or a tag ([`RpcReadKey::block_form`]
/// says which). Splitting one stored term into two fields would have invented a value for
/// the half that is absent, so there is one term and its form.
///
/// `chain_id` comes from the run's own published field rather than from the key: the key
/// embeds a chain string M8.4.1 wrote, and the run's `chain_id` is what this build knows
/// the endpoint answered for. A pair whose chains differ is not a duplicate, and the rule
/// that enforces that reads this field.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RpcReadKey {
    pub method: String,
    pub category: &'static str,
    /// The rule that produced [`RpcReadKey::category`], in the words this module prints,
    /// so a category in a table is traceable to a line of §9 rather than to a guess.
    pub category_rule: String,
    pub chain_id: Option<u64>,
    /// The block term as recorded: a decimal height, or the tag that was sent.
    pub block: Option<String>,
    pub block_form: &'static str,
    /// The account, contract or hash the ask names, lowercased and `0x`-prefixed by
    /// [`evm_chain::normalize_address`] — the same hygiene the published row carries.
    pub address: Option<String>,
    /// A storage word, zero-padded to 64 hex digits, for `eth_getStorageAt` only.
    pub slot: Option<String>,
    /// The terms that make two asks the same ask, keyed by term name. Empty when
    /// [`RpcReadKey::identity_source`] is [`IDENTITY_UNAVAILABLE`].
    pub terms: BTreeMap<String, String>,
    pub identity_source: &'static str,
    /// M8.4.1's own reason when there is no recorded key
    /// ([`evm_chain::DEDUP_KEY_UNAVAILABLE_FOR_METHOD`] or
    /// [`evm_chain::DEDUP_KEY_PARAMS_UNREADABLE`]), carried through unchanged.
    pub identity_note: Option<String>,
    /// Whether the row's own `block_tag`, `target` and `slot` name the terms its recorded
    /// key names. `true` for a parameterless ask, which has no such term to match: this is
    /// an integrity question about the record, asked of keyed rows and answered for all.
    pub row_matches_key: bool,
    /// The endpoint this ask went to, as a digest. Not part of [`RpcReadKey::identity`]
    /// — §8's field list does not carry a provider — but required by §6's state-semantics
    /// condition, because an answer from a different node is not the same answer.
    pub endpoint_id: Option<String>,
    /// Which sink recorded the call, in M8.4.1's two words. Part of the record, not of the
    /// identity: the simulation's provider and the lifecycle's provider are two observers
    /// of one run, and §13's pairs are drawn across them.
    pub sink: Option<String>,
    pub stage: Option<String>,
    pub caller: Option<String>,
    pub run: Option<String>,
    pub rpc_id: Option<u64>,
    pub logical_request_id: Option<String>,
    /// The recorded start stamp, on its own run's monotonic clock. §5's direction
    /// (producer first, consumer second) is read off this and nothing else.
    pub started_ns: Option<u64>,
    pub finished_ns: Option<u64>,
    pub duration_ns: Option<u64>,
    /// How many HTTP tries this one logical ask took — `attempt` of the row, which is one
    /// row per *logical* request with its tries listed beside it. §25's rule — a retry is
    /// not a duplicate — is enforced by that: pairs are formed between rows, so a call that
    /// tried three times is one ask here and can never contribute three duplicates.
    pub physical_attempts: Option<u64>,
    /// Whether the row carried its tries at all. `Some(false)` is a record that predates
    /// per-attempt stamps, which is said rather than read as one unrecorded try.
    pub attempts_recorded: Option<bool>,
    pub success: Option<bool>,
}

impl RpcReadKey {
    /// §8's identity as one comparable string: every term this key has, sorted by term
    /// name, joined. Two keys with the same text asked for the same thing.
    ///
    /// `None` when there is no identity to compare, which is reported rather than
    /// defaulted: a pair of asks with no comparable terms is not a pair of distinct asks,
    /// it is a pair this build cannot judge.
    pub fn identity(&self) -> Option<String> {
        self.identity_of(true)
    }

    /// The identity with the block term removed — the term §4.3 and §4.4 are about.
    ///
    /// This is what makes 「same target, different block」 a *measured* class instead of a
    /// sentence: two keys are equal here and differ there, and the difference has a name.
    pub fn identity_without_block(&self) -> Option<String> {
        self.identity_of(false)
    }

    fn identity_of(&self, with_block: bool) -> Option<String> {
        if self.terms.is_empty() {
            return None;
        }
        let filtered: Vec<String> = self
            .terms
            .iter()
            .filter(|(term, _)| with_block || term.as_str() != "block")
            .map(|(term, value)| format!("{term}={value}"))
            .collect();
        if !with_block && !self.terms.contains_key("block") {
            // No block term to remove means this identity says nothing about a block, so
            // asking for the block-free version of it cannot distinguish anything: refuse
            // rather than hand back the same string and let the caller read "equal" twice.
            return None;
        }
        Some(format!("method={}|{}", self.method, filtered.join("|")))
    }

    /// §23's `same_block`, as three answers rather than two: `true` and `false` require
    /// the record to name two heights and for them to differ or not, and everything else
    /// — a tag on either side, a method with no block — is that this build cannot say.
    pub fn same_block(a: &RpcReadKey, b: &RpcReadKey) -> SameBlock {
        match (a.block_form, b.block_form) {
            (BLOCK_FORM_NUMBER, BLOCK_FORM_NUMBER) => {
                if a.block == b.block {
                    SameBlock::Yes
                } else {
                    SameBlock::No
                }
            }
            (BLOCK_FORM_ABSENT, BLOCK_FORM_ABSENT) => SameBlock::NoBlockInEitherRequest,
            (BLOCK_FORM_TAG, BLOCK_FORM_TAG) => {
                if a.block == b.block {
                    SameBlock::BothAskTheSameTag
                } else {
                    SameBlock::DifferentTags
                }
            }
            _ => SameBlock::TagAgainstHeight,
        }
    }

    /// §26 and §29's cache-isolation guard in one place: an identity can only ever come
    /// from a row that *is* a recorded request. There is no constructor here that accepts
    /// a cache hit, a REVM lookup, or a logical read that cost no request, because those
    /// have no row — and §18 forbids counting one as a physical duplicate.
    pub fn is_physical_request(&self) -> bool {
        self.logical_request_id.is_some()
    }

    /// One published row of `pipeline-calls.json` as a key.
    ///
    /// A row whose method M8.4.1 could key keeps that key's terms (§12's rules, which
    /// this module does not re-derive); a row whose method it could not key and that takes
    /// no parameters gets the parameterless identity; anything else keeps M8.4.1's refusal
    /// as its own [`RpcReadKey::identity_note`]. The parse of a recorded key is total and
    /// refusing — see [`terms_of_recorded_key`] — because it is reading another module's
    /// string, which is the one thing this file's existence was argued against.
    ///
    /// The terms come from the recorded key *only*, and the row's own `block_tag`, `target`
    /// and `slot` are then compared against them rather than merged into them
    /// ([`RpcReadKey::row_matches_key`]). Two sources for one term is how a table ends up
    /// reporting a figure that neither field supports; here a disagreement is a measured
    /// fact about the record, and it is counted instead of being resolved by whichever
    /// field happened to be written last.
    pub fn of_row(row: &Value, chain_id: Option<u64>) -> Self {
        let method = row["method"].as_str().unwrap_or_default().to_string();
        let recorded_key = row["dedup_key"].as_str().map(str::to_owned);
        let key_note = row["key_note"].as_str();
        let block = row["block_tag"].as_str().map(str::to_owned);
        let block_form = match (block.as_deref(), method_names_a_block(&method)) {
            (None, _) => BLOCK_FORM_ABSENT,
            (Some(_), false) => BLOCK_FORM_ABSENT,
            (Some(term), true) if term_names_a_block_height(term) => BLOCK_FORM_NUMBER,
            (Some(_), true) => BLOCK_FORM_TAG,
        };
        let address = row["target"]
            .as_str()
            .filter(|_| ACCOUNT_NAMING_METHODS.contains(&method.as_str()))
            .map(evm_chain::normalize_address);
        let slot = row["slot"].as_str().map(str::to_owned);

        let mut terms: BTreeMap<String, String> = BTreeMap::new();
        let (identity_source, identity_note) = match (recorded_key.as_deref(), key_note) {
            (Some(key), _) => match terms_of_recorded_key(&method, key) {
                Some(parsed) => {
                    terms = parsed;
                    (IDENTITY_FROM_RECORDED_KEY, None)
                }
                // A key this build cannot take apart is not a key that matched nothing:
                // the shape is reported and the ask drops out of every tally.
                None => (
                    IDENTITY_UNAVAILABLE,
                    Some(RECORDED_KEY_SHAPE_UNPARSEABLE.to_string()),
                ),
            },
            (None, Some(_)) if PARAMETERLESS_METHODS.contains(&method.as_str()) => {
                terms.insert("method_kind".to_string(), method.clone());
                terms.insert(
                    "endpoint".to_string(),
                    row["endpoint_id"].as_str().unwrap_or("unknown").to_string(),
                );
                (IDENTITY_FROM_PARAMLESS_REQUEST, None)
            }
            (None, note) => (IDENTITY_UNAVAILABLE, note.map(str::to_owned)),
        };

        // The row's own three fields against the key they are supposed to agree with. Only
        // meaningful for a keyed row — the parameterless identity has no such term to match —
        // and a keyed row whose fields and key disagree is reported, not averaged.
        let row_matches_key = identity_source != IDENTITY_FROM_RECORDED_KEY
            || terms_are_disagreement_free(
                &terms,
                block.as_deref(),
                address.as_deref(),
                slot.as_deref(),
            );

        let (category, category_rule) =
            read_category(&method, row["sink"].as_str(), row["caller"].as_str());

        RpcReadKey {
            method,
            category,
            category_rule,
            chain_id,
            block: block.clone(),
            block_form,
            address,
            slot,
            terms,
            identity_source,
            identity_note,
            row_matches_key,
            endpoint_id: row["endpoint_id"].as_str().map(str::to_owned),
            sink: row["sink"].as_str().map(str::to_owned),
            stage: row["stage"].as_str().map(str::to_owned),
            caller: row["caller"].as_str().map(str::to_owned),
            run: row["run"].as_str().map(str::to_owned),
            rpc_id: row["rpc_id"].as_u64(),
            logical_request_id: row["logical_request_id"].as_str().map(str::to_owned),
            started_ns: row["started_ns"].as_u64(),
            finished_ns: row["finished_ns"].as_u64(),
            duration_ns: row["duration_ns"].as_u64(),
            // `attempt` is how many HTTP tries this one logical ask took — see
            // `pipeline_call_row`: one row is one logical request, and `attempts` lists its
            // tries. §25's rule is enforced by the row being one ask at all, and this field
            // is what says so out loud.
            physical_attempts: row["attempt"].as_u64(),
            attempts_recorded: row["attempts_recorded"].as_bool(),
            success: row["success"].as_bool(),
        }
    }
}

/// Whether a keyed row's three literal fields name the same terms its recorded key names.
///
/// Absent fields count as agreeing — a method with no address has no address to disagree
/// about. A present field against a term of a different name is a disagreement, which is a
/// statement about the record rather than about the ask, so it is surfaced as
/// [`RpcReadKey::row_matches_key`] = `false` and counted by the gate that reads it.
///
/// The names a field may match are the names §12 gives that term, which differ by method:
/// an `eth_call`'s `target` field is the key's `to`, and a block-by-hash read's is its
/// `hash`. Reading `target` as `address` for those two would call 76 of this corpus's 235
/// rows a disagreement about a field that agrees with its key perfectly.
fn terms_are_disagreement_free(
    terms: &BTreeMap<String, String>,
    block: Option<&str>,
    address: Option<&str>,
    slot: Option<&str>,
) -> bool {
    let same = |names: &[&str], field: Option<&str>| match field {
        None => true,
        Some(value) => names
            .iter()
            .any(|name| terms.get(*name).is_some_and(|term| term == value)),
    };
    same(&["block", "hash"], block) && same(&["address", "to"], address) && same(&["slot"], slot)
}

/// §23's three-way answer about block identity, with the two refusals named.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SameBlock {
    /// Both asks name a height and the heights are equal.
    Yes,
    /// Both asks name a height and they differ — §4.3, and §23's own
    /// 「safe_to_reuse = false, reason = different block identity」.
    No,
    /// Both asks carry no block term at all, so there is no identity of this kind to
    /// agree on: §6's first condition is *not met*, which is not the same as violated.
    NoBlockInEitherRequest,
    /// Both asks carry a tag, and the same tag: two asks that both say `latest` at
    /// different instants are not two asks for one state (§23's own example, one block
    /// earlier than the case it names).
    BothAskTheSameTag,
    /// Two different tags — `latest` against `pending` — which §4.4 says is not a
    /// duplicate even when the account is the same.
    DifferentTags,
    /// One side names a height, the other a tag: the record has no answer for what the
    /// tag resolved to, so neither `same` nor `different` is provable.
    TagAgainstHeight,
}

impl SameBlock {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Yes => "same_block",
            Self::No => "different_block",
            Self::NoBlockInEitherRequest => "no_block_in_either_request",
            Self::BothAskTheSameTag => "both_ask_a_tag",
            Self::DifferentTags => "different_tags",
            Self::TagAgainstHeight => "tag_against_height",
        }
    }

    /// §5's `same_block` figure: `true` and `false` only where the record proves them, and
    /// `None` for the four cases where it does not. §23's rule is stated in terms of this:
    /// only [`SameBlock::Yes`] satisfies it, and only [`SameBlock::No`] ends the question in
    /// the negative.
    pub fn figure(self) -> Option<bool> {
        match self {
            Self::Yes => Some(true),
            Self::No => Some(false),
            _ => None,
        }
    }
}

/// The note this module adds when a recorded key exists but is not one of §12's seven
/// shapes. M8.4.1's two notes are about a key that was never built; this one is about a
/// key this reader could not take apart, which is a different fact and must not be
/// reported as either of those.
pub const RECORDED_KEY_SHAPE_UNPARSEABLE: &str = "recorded_key_shape_unparseable";

/// The five methods whose recorded `target` names an account or a contract, so §8's
/// `address` field is filled from it. `eth_call`'s target is its `to`, which is the contract
/// being asked, and §15's identity block names it; `eth_getBlockByNumber` and
/// `eth_getBlockByHash` are absent because their target is a height or a hash, not a holder
/// of state — the field that §6's state-semantics condition is about.
const ACCOUNT_NAMING_METHODS: [&str; 5] = [
    "eth_getBalance",
    "eth_getCode",
    "eth_getTransactionCount",
    "eth_getStorageAt",
    "eth_call",
];

/// The methods §12 does not key and that take no parameters, so their whole ask is their
/// name plus the endpoint that answers it. `eth_getLogs` is deliberately absent: it takes
/// a filter, and a parameterless identity for it would key two unrelated log queries as
/// one ask.
pub const PARAMETERLESS_METHODS: [&str; 3] =
    ["eth_chainId", "eth_blockNumber", "eth_maxPriorityFeePerGas"];

/// Whether a method's recorded block term names a block. `eth_getBlockByHash` names a hash
/// where §8's `block_number` would sit, and §23's rule is about heights, so a hash read gets
/// the absent form and the pair is judged on its `hash` term rather than on a block identity
/// this build cannot name.
fn method_names_a_block(method: &str) -> bool {
    matches!(
        method,
        "eth_getStorageAt"
            | "eth_getBalance"
            | "eth_getCode"
            | "eth_getTransactionCount"
            | "eth_call"
            | "eth_getBlockByNumber"
    )
}

/// Take a recorded §12 key apart into named terms.
///
/// §12's seven shapes all put `kind|chain|block|rest…` first, so the tail after the block
/// is the remaining terms — `to` and calldata for an `eth_call`, `hydrated` for a block
/// read, nothing for an account read. Reading it back is a dependency on another module's
/// string format, which is why this refuses rather than guesses: a key whose prefix does
/// not match its method's known shape yields `None`, the ask drops out of the tallies, and
/// [`RECORDED_KEY_SHAPE_UNPARSEABLE`] says which.
fn terms_of_recorded_key(method: &str, key: &str) -> Option<BTreeMap<String, String>> {
    let fields: Vec<&str> = key.split('|').collect();
    let kind = *fields.first()?;
    let expected = match method {
        "eth_getStorageAt" => "storage",
        "eth_getBalance" => "balance",
        "eth_getCode" => "code",
        "eth_getTransactionCount" => "nonce",
        "eth_call" => "call",
        "eth_getBlockByNumber" => "block",
        "eth_getBlockByHash" => "blockhash",
        _ => return None,
    };
    if kind != expected || fields.len() < 3 {
        return None;
    }
    let mut terms = BTreeMap::new();
    terms.insert("chain".to_string(), fields[1].to_string());
    // Position 2 is the block term for every keyed method; for `eth_getBlockByHash` it is
    // a hash, so it is named for what it is rather than called a height.
    terms.insert(
        if method == "eth_getBlockByHash" {
            "hash"
        } else {
            "block"
        }
        .to_string(),
        fields[2].to_string(),
    );
    match method {
        "eth_getStorageAt" => {
            // storage|chain|block|address|slot
            let address = fields.get(3)?;
            let slot = fields.get(4)?;
            terms.insert("address".to_string(), address.to_string());
            terms.insert("slot".to_string(), slot.to_string());
        }
        "eth_getBalance" | "eth_getCode" | "eth_getTransactionCount" => {
            terms.insert("address".to_string(), fields.get(3)?.to_string());
        }
        "eth_call" => {
            // call|chain|block|to|data[|value|value]
            terms.insert("to".to_string(), fields.get(3)?.to_string());
            terms.insert("data".to_string(), fields.get(4)?.to_string());
            if let (Some(label), Some(amount)) = (fields.get(5), fields.get(6)) {
                if *label != "value" {
                    return None;
                }
                terms.insert("value".to_string(), amount.to_string());
            } else if fields.len() > 5 {
                return None;
            }
        }
        "eth_getBlockByNumber" | "eth_getBlockByHash" => {
            // …|block|hydrated|<bool>
            if *fields.get(3)? != "hydrated" {
                return None;
            }
            terms.insert("hydrated".to_string(), fields.get(4)?.to_string());
        }
        _ => return None,
    }
    Some(terms)
}

// ---------------------------------------------------------------------------
// §9: read categories
// ---------------------------------------------------------------------------

/// §9's six words, verbatim. The rule that assigns one is below and is a total function:
/// every method this build issues lands in exactly one category, and a method this module
/// does not know lands in `other` with the reason in its rule text rather than in a
/// invented class.
pub const READ_CATEGORY_STATE_READ: &str = "state_read";
pub const READ_CATEGORY_BLOCK_READ: &str = "block_read";
pub const READ_CATEGORY_CHAIN_IDENTITY: &str = "chain_identity";
pub const READ_CATEGORY_SIMULATION_CALL: &str = "simulation_call";
pub const READ_CATEGORY_TRANSACTION_PREPARATION: &str = "transaction_preparation";
pub const READ_CATEGORY_OTHER: &str = "other";
pub const READ_CATEGORIES: [&str; 6] = [
    READ_CATEGORY_STATE_READ,
    READ_CATEGORY_BLOCK_READ,
    READ_CATEGORY_CHAIN_IDENTITY,
    READ_CATEGORY_SIMULATION_CALL,
    READ_CATEGORY_TRANSACTION_PREPARATION,
    READ_CATEGORY_OTHER,
];

/// §9's classification of one ask.
///
/// The task book's own two examples fix the shape of the rule: `eth_chainId` is not a
/// state read, and `eth_getBalance` is. Its third — 「`eth_call` 必须根据实际语义判断」 —
/// cannot be answered from the method name, because this build issues `eth_call` for four
/// different jobs: pricing a market (detection's and preflight's reserves), costing a step
/// (preflight's ceiling per step), taking a balance snapshot for the audit (build's
/// before-snapshot), and answering the EVM's own external call (the simulation sink). One
/// word for all four would be the §9 mistake in reverse, so the rule reads the method plus
/// the *caller* the issuing code stamped — M8.4.1's §4 field, in the code's own words.
///
/// That choice has a cost and it is published: a caller label that changes its wording
/// moves a category. `diagnosis.rs`'s cross-stage gate fails loudly when an observed
/// caller family matches none of the rules, so the drift shows up as a refused assembly
/// rather than as a silent `other`.
pub fn read_category(
    method: &str,
    sink: Option<&str>,
    caller: Option<&str>,
) -> (&'static str, String) {
    let caller_text = caller.unwrap_or("<unstamped>");
    match method {
        "eth_chainId" => (
            READ_CATEGORY_CHAIN_IDENTITY,
            "§9 names eth_chainId as not a state read; it asks which chain an endpoint \
             answers for and carries no block term"
                .to_string(),
        ),
        "eth_blockNumber" => (
            READ_CATEGORY_BLOCK_READ,
            "no account is named and no state is read: the answer is a height, which is a \
             property of the head at the instant of asking, so it is a block read with no \
             block term of its own"
                .to_string(),
        ),
        "eth_getBalance" | "eth_getCode" | "eth_getStorageAt" => (
            READ_CATEGORY_STATE_READ,
            "§9's own example: an account's balance, code, or a named storage word at a \
             block is state"
                .to_string(),
        ),
        "eth_getTransactionCount" => {
            if sink == Some("simulation") {
                (
                    READ_CATEGORY_STATE_READ,
                    "the simulation's provider asks for an account's nonce as part of \
                     warming the state the EVM runs against, which is the same kind of read \
                     as its balance and code reads"
                        .to_string(),
                )
            } else {
                (
                    READ_CATEGORY_TRANSACTION_PREPARATION,
                    "the nonce a transaction needs to be addressed with: §9's \
                     transaction_preparation, not a read of market state"
                        .to_string(),
                )
            }
        }
        "eth_getBlockByNumber" | "eth_getBlockByHash" => (
            READ_CATEGORY_BLOCK_READ,
            "a header (or a header with its transactions) at a named height or hash: §9's \
             block_read"
                .to_string(),
        ),
        "eth_maxPriorityFeePerGas" | "eth_gasPrice" | "eth_estimateGas" => (
            READ_CATEGORY_TRANSACTION_PREPARATION,
            "§19's `fee` and `gas`: what the next transaction will have to pay, asked of \
             the head rather than of a pinned block"
                .to_string(),
        ),
        "eth_call" => {
            if sink == Some("simulation") {
                (
                    READ_CATEGORY_SIMULATION_CALL,
                    "the EVM's own external call, answered by this build's state provider: \
                     §9's simulation_call"
                        .to_string(),
                )
            } else if caller_text.starts_with("cost ceiling") {
                (
                    READ_CATEGORY_TRANSACTION_PREPARATION,
                    "preflight's per-step cost ceiling: the call is asked to price a step, \
                     not to read a market"
                        .to_string(),
                )
            } else if caller_text.starts_with("before-snapshot")
                || caller_text.starts_with("after-snapshot")
            {
                (
                    READ_CATEGORY_TRANSACTION_PREPARATION,
                    "the execution lane's own audit snapshot: a balance read taken to \
                     describe what a transaction will do, not to decide whether to do it"
                        .to_string(),
                )
            } else if caller_text.starts_with("reserves") {
                (
                    READ_CATEGORY_STATE_READ,
                    "a pool's reserves, read at a block in order to price the pair: §9's \
                     state_read by the call's actual semantics, which is what §9 asks the \
                     eth_call rule to do"
                        .to_string(),
                )
            } else if caller_text.starts_with("fee at") {
                (
                    READ_CATEGORY_TRANSACTION_PREPARATION,
                    "a fee read dressed as a call: what the next transaction pays".to_string(),
                )
            } else {
                (
                    READ_CATEGORY_OTHER,
                    format!(
                        "no §9 rule covers an eth_call whose caller reads {caller_text:?}; \
                         reported as other rather than folded into the nearest category"
                    ),
                )
            }
        }
        _ => (
            READ_CATEGORY_OTHER,
            format!("no §9 rule names {method}; reported as other"),
        ),
    }
}

/// The caller families this module's `eth_call` rules read, published so a gate can ask
/// whether an assembly saw one it cannot classify. A family is the prefix the code's own
/// label starts with.
pub const ETH_CALL_CALLER_FAMILIES: [&str; 5] = [
    "cost ceiling",
    "before-snapshot",
    "after-snapshot",
    "reserves",
    "fee at",
];

// ---------------------------------------------------------------------------
// §4: the duplicate classes
// ---------------------------------------------------------------------------

/// §4.1: same method, same chain, same block term, same target, same slot, same
/// normalized parameters — every term of §8's key equal. The strongest evidence this
/// build can produce, and the only one it can produce for a pair of rows.
pub const DUPLICATE_EXACT: &str = "exact_duplicate";
/// §4.2: two asks whose *literal* spellings differ but which name one state.
///
/// This build measures zero of these and says why instead of quietly merging the class
/// into [`DUPLICATE_EXACT`]: the normalization happens at record time, in
/// [`evm_chain::describe_call`], and `pipeline_call_row` lowercases the target again, so
/// the row layer a duplicate tally is built from holds one spelling per ask by the time
/// this module sees it. See `duplicate-summary.json`'s `semantic_duplicate` entry, which
/// carries the mechanism, the one place a literal *is* still stored (the simulation
/// sink's own trace lines), and the measured count of identity groups there whose members
/// spell a target two ways.
pub const DUPLICATE_SEMANTIC: &str = "semantic_duplicate";
/// §4.3: one target, two heights. Named, counted, and never a reuse candidate.
pub const SAME_TARGET_DIFFERENT_BLOCK: &str = "same_target_different_block";
/// §4.4: one method and one account, two different semantics — the task book's example is
/// `getTransactionCount(A, latest)` against `(A, pending)`, which is what this class
/// measures when both sides carry a tag and the tags differ.
pub const SAME_METHOD_DIFFERENT_SEMANTICS: &str = "same_method_different_semantics";
/// The case §4 does not name and the data has: one side names a height, the other a tag.
/// §4.2 forbids guessing what a tag resolved to and §4.3 requires two different heights,
/// so this build can prove neither. Declared here as this milestone's addition, with the
/// reason, rather than rounded into one of the four.
pub const SAME_TARGET_BLOCK_UNDETERMINED: &str = "same_target_block_undetermined";
/// Nothing this module can call a duplicate: a term other than the block differs, the
/// methods differ, or one side has no identity at all.
pub const NOT_A_DUPLICATE: &str = "not_a_duplicate";

/// §4's classes plus the two additions this build declares, in the order a table lists
/// them. A pair always lands in exactly one of these, and the first three are duplicates.
pub const DUPLICATE_CLASSES: [&str; 6] = [
    DUPLICATE_EXACT,
    DUPLICATE_SEMANTIC,
    SAME_TARGET_DIFFERENT_BLOCK,
    SAME_METHOD_DIFFERENT_SEMANTICS,
    SAME_TARGET_BLOCK_UNDETERMINED,
    NOT_A_DUPLICATE,
];

/// Which of §4's classes one pair of asks is.
///
/// A total function on two keys, and the whole of the duplicate rule:
///
/// ```text
/// different method                    -> not_a_duplicate   (§4.1 requires the same method)
/// either side has no identity         -> not_a_duplicate   (judged, with the note beside it)
/// every term equal                    -> exact_duplicate
/// only the block term differs:
///     two heights, unequal            -> same_target_different_block
///     two tags, unequal               -> same_method_different_semantics
///     height against tag            -> same_target_block_undetermined
/// anything else                     -> not_a_duplicate
/// ```
///
/// `semantic_duplicate` is never returned, because the record this reads has already
/// collapsed the spellings; the class exists so the summary can report a measured zero
/// against a named rule rather than against an absent one.
pub fn classify_pair(a: &RpcReadKey, b: &RpcReadKey) -> &'static str {
    if a.method != b.method {
        return NOT_A_DUPLICATE;
    }
    if a.identity().is_none() || b.identity().is_none() {
        return NOT_A_DUPLICATE;
    }
    if a.chain_id.is_some() && b.chain_id.is_some() && a.chain_id != b.chain_id {
        return NOT_A_DUPLICATE;
    }
    if a.identity() == b.identity() {
        return DUPLICATE_EXACT;
    }
    let (without_a, without_b) = (a.identity_without_block(), b.identity_without_block());
    if without_a.is_none() || without_a != without_b {
        return NOT_A_DUPLICATE;
    }
    match RpcReadKey::same_block(a, b) {
        SameBlock::No => SAME_TARGET_DIFFERENT_BLOCK,
        SameBlock::DifferentTags => SAME_METHOD_DIFFERENT_SEMANTICS,
        SameBlock::TagAgainstHeight => SAME_TARGET_BLOCK_UNDETERMINED,
        // Equal identities were handled above, so a `Yes` here cannot happen; the two
        // refusals cannot either, because both sides would then carry no block term and so
        // would have had no block-free difference to find. Reachable only if a future rule
        // lets a key vary without a block term, and reported as not-a-duplicate if it does.
        _ => NOT_A_DUPLICATE,
    }
}

/// §4's class for a pair whose identity matches but whose *endpoints* differ.
///
/// §8's key does not carry a provider, so two asks of two nodes are equal here. They are
/// still the same *ask*; they are not the same *answer*, which is §6's second condition
/// and is handled in the reuse verdict rather than by hiding the pair from the matrix.
pub fn same_endpoint(a: &RpcReadKey, b: &RpcReadKey) -> bool {
    match (a.endpoint_id.as_deref(), b.endpoint_id.as_deref()) {
        (Some(x), Some(y)) => x == y,
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// §17: intra-stage versus cross-stage
// ---------------------------------------------------------------------------

/// Two asks from the same stage: §17's `intra_stage`, which this milestone must not sell
/// as the cross-stage finding it is not.
pub const SCOPE_INTRA_STAGE: &str = "intra_stage";
/// Two asks from different stages, in whichever order the stages appear in the ladder.
pub const SCOPE_CROSS_STAGE: &str = "cross_stage";
/// Two asks from one call. Not a pair: §25's retries are one logical request, so the
/// second HTTP try of an ask never becomes a second ask.
pub const SCOPE_SAME_LOGICAL_REQUEST: &str = "same_logical_request";
pub const SCOPES: [&str; 3] = [
    SCOPE_CROSS_STAGE,
    SCOPE_INTRA_STAGE,
    SCOPE_SAME_LOGICAL_REQUEST,
];

/// §17's split, with §25's exclusion folded into it.
///
/// Two rows are a *pair* only when they are two different logical requests: a call that
/// retried is one ask with two attempts, and the row list carries it once
/// (`attempt` counts the tries). A stage that stamped nothing shares an absent name with
/// itself, so a pair of unstamped asks is `intra_stage` under the label `unstamped`, which
/// is M8.4.1's word for the absence ([`crate::diagnosis`]'s `PIPELINE_UNSTAMPED`) and is
/// kept, not renamed.
pub fn pair_scope(a: &RpcReadKey, b: &RpcReadKey) -> &'static str {
    if a.logical_request_id.is_some() && a.logical_request_id == b.logical_request_id {
        return SCOPE_SAME_LOGICAL_REQUEST;
    }
    if a.stage == b.stage {
        SCOPE_INTRA_STAGE
    } else {
        SCOPE_CROSS_STAGE
    }
}

// ---------------------------------------------------------------------------
// §6 and §23: the three verdicts
// ---------------------------------------------------------------------------

/// §6's condition that this build can *meet*: two heights named by the record, equal.
pub const CONDITION_BLOCK_IDENTITY: &str = "block_identity";
/// §6's second condition: the state means the same thing to both callers. Provable from
/// a record only when the two asks are the same method and the same terms on the same
/// endpoint, which is what this checks and no more.
pub const CONDITION_STATE_SEMANTICS: &str = "state_semantics";
/// §6's third: the lifecycle allows one stage to hold another's answer. Nothing in a row
/// says who owns an answer, so this build reports it as unchecked for every pair and
/// names the module that would have to be read to close it.
pub const CONDITION_LIFECYCLE_OWNERSHIP: &str = "lifecycle_ownership";
/// §6's fourth: the consumer has no freshness requirement. A consumer that asks a tag, or
/// asks a category whose answer *is* the head, has stated one; a consumer that asks the
/// same height as the producer has not stated the absence of one, which is a different
/// thing and is reported as this.
pub const CONDITION_NO_FRESHNESS_REQUIREMENT: &str = "no_freshness_requirement";
/// §23's rule as a condition in its own right, so a pair §23 refuses cannot be reported
/// as merely unproven.
pub const CONDITION_SAME_BLOCK_REQUIRED: &str = "same_block";

/// §6's four conditions, in §6's order, plus §23's.
pub const REUSE_CONDITIONS: [&str; 5] = [
    CONDITION_BLOCK_IDENTITY,
    CONDITION_STATE_SEMANTICS,
    CONDITION_LIFECYCLE_OWNERSHIP,
    CONDITION_NO_FRESHNESS_REQUIREMENT,
    CONDITION_SAME_BLOCK_REQUIRED,
];

/// What one condition came to.
pub const CHECKED_MET: &str = "checked_met";
pub const CHECKED_VIOLATED: &str = "checked_violated";
pub const CHECKABLE_NOT_MET: &str = "checkable_not_met";
pub const NOT_CHECKABLE_FROM_A_RECORD: &str = "not_checkable_from_a_record";

/// §6's three levels, as the verdict on one pair.
///
/// ```text
/// duplicate               §4's first three classes: the two asks are the same ask
/// reusable                only when the conditions this build can check are met, and
///                         the ones it cannot are named as unmet — never as passed
/// safe_to_reuse = true    every condition in REUSE_CONDITIONS resolved to met
/// ```
///
/// The middle level is the one §35's result C lives in, and it is expected to be where
/// most of this milestone's pairs land: a pair of asks can be provably the same ask at
/// provably the same height on the provably same endpoint while nothing in the record says
/// whether the second caller is allowed to be served by the first. That is a finding about
/// the evidence, not a shortfall of the rule.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReuseVerdict {
    /// Not even a duplicate: there is nothing to reuse.
    NotACandidate,
    /// A duplicate whose §23 block condition is *violated* — two named, unequal heights,
    /// which is §4.3's class. §23 settles this one in the negative rather than leaving it
    /// open, and the tables count it apart from the candidates.
    RefusedByBlockIdentity,
    /// A duplicate at one block, with at least one of §6's conditions left unmet or
    /// uncheckable. This is `reusable = "unknown"` in §5's own JSON.
    CandidateReuseUnknown,
    /// Every §6 condition resolved to met. `safe_reuse = true` and nothing else says so.
    SafeToReuse,
}

impl ReuseVerdict {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotACandidate => "not_a_candidate",
            Self::RefusedByBlockIdentity => "refused_different_block_identity",
            Self::CandidateReuseUnknown => "reuse_candidate_unknown",
            Self::SafeToReuse => "safe_to_reuse",
        }
    }

    /// §16's two tallies: what counts as a candidate and what counts as a safe one.
    ///
    /// A pair §23 settled in the negative is not a candidate: it is §4.3's own class, and the
    /// tables tally that class separately. Counting it here too would let one pair inflate both
    /// the duplicate figures and the reuse figures.
    pub fn counts_as_candidate(self) -> bool {
        matches!(self, Self::CandidateReuseUnknown | Self::SafeToReuse)
    }

    pub fn counts_as_safe(self) -> bool {
        matches!(self, Self::SafeToReuse)
    }
}

/// One §6 condition's outcome plus the reason, as one published row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConditionOutcome {
    pub condition: &'static str,
    pub outcome: &'static str,
    pub reason: String,
}

impl ConditionOutcome {
    pub fn to_json(&self) -> Value {
        Value::Object(serde_json::Map::from_iter([
            (
                "condition".to_string(),
                Value::String(self.condition.to_string()),
            ),
            (
                "outcome".to_string(),
                Value::String(self.outcome.to_string()),
            ),
            ("reason".to_string(), Value::String(self.reason.clone())),
        ]))
    }
}

/// §6's four conditions and §23's rule, applied to one pair of asks.
///
/// Every condition gets an outcome, including the ones this build cannot check — an
/// absent condition would read as a passed one. The order is
/// [`REUSE_CONDITIONS`]' so two runs of this function on the same pair write the same
/// list, which is what makes the byte gate over `reuse-candidates.json` mean anything.
pub fn reuse_conditions(
    a: &RpcReadKey,
    b: &RpcReadKey,
    duplicate_class: &'static str,
    same_block: SameBlock,
) -> Vec<ConditionOutcome> {
    let mut out: Vec<ConditionOutcome> = Vec::new();
    let mut push = |condition: &'static str, outcome: &'static str, reason: String| {
        out.push(ConditionOutcome {
            condition,
            outcome,
            reason,
        })
    };

    // §23 first, because it is the one condition the task book states as a rule with a
    // conclusion attached: different block identity ends the question.
    match same_block {
        SameBlock::Yes => push(
            CONDITION_SAME_BLOCK_REQUIRED,
            CHECKED_MET,
            "both asks name the same height".to_string(),
        ),
        SameBlock::No => push(
            CONDITION_SAME_BLOCK_REQUIRED,
            CHECKED_VIOLATED,
            "§23: two named heights differ, so safe_to_reuse is false and the reason is \
             \"different block identity\""
                .to_string(),
        ),
        SameBlock::BothAskTheSameTag => push(
            CONDITION_SAME_BLOCK_REQUIRED,
            CHECKABLE_NOT_MET,
            "§23's own failure case: both sides ask a tag, which names the head at answer \
             time and not a block, so the condition is not met even though the two asks \
             read alike"
                .to_string(),
        ),
        SameBlock::DifferentTags => push(
            CONDITION_SAME_BLOCK_REQUIRED,
            CHECKABLE_NOT_MET,
            "two different tags are two different semantics (§4.4), not two heights".to_string(),
        ),
        SameBlock::TagAgainstHeight => push(
            CONDITION_SAME_BLOCK_REQUIRED,
            CHECKABLE_NOT_MET,
            "one side names a height and the other a tag: the record has no value for what \
             the tag resolved to, so the condition is neither met nor violated"
                .to_string(),
        ),
        SameBlock::NoBlockInEitherRequest => push(
            CONDITION_SAME_BLOCK_REQUIRED,
            CHECKABLE_NOT_MET,
            "neither ask carries a block term, so §6's block-identity condition cannot be \
             met by it; this is an absent identity, not a matching one"
                .to_string(),
        ),
    }

    if duplicate_class == NOT_A_DUPLICATE {
        push(
            CONDITION_BLOCK_IDENTITY,
            CHECKABLE_NOT_MET,
            "the pair is not a duplicate, so §6 was never reached".to_string(),
        );
        push(
            CONDITION_STATE_SEMANTICS,
            CHECKABLE_NOT_MET,
            "the pair is not a duplicate, so §6 was never reached".to_string(),
        );
        push(
            CONDITION_LIFECYCLE_OWNERSHIP,
            NOT_CHECKABLE_FROM_A_RECORD,
            "not applicable to a pair that asks for different things".to_string(),
        );
        push(
            CONDITION_NO_FRESHNESS_REQUIREMENT,
            NOT_CHECKABLE_FROM_A_RECORD,
            "not applicable to a pair that asks for different things".to_string(),
        );
        return out;
    }

    match same_block {
        SameBlock::Yes => push(
            CONDITION_BLOCK_IDENTITY,
            CHECKED_MET,
            "one height names one block, and both asks name it".to_string(),
        ),
        other => push(
            CONDITION_BLOCK_IDENTITY,
            CHECKABLE_NOT_MET,
            format!(
                "the record's block terms do not give one identity ({})",
                other.as_str()
            ),
        ),
    }

    if same_endpoint(a, b) {
        push(
            CONDITION_STATE_SEMANTICS,
            CHECKED_MET,
            "same method, same terms, same endpoint digest: what the node was asked is one \
             ask. This does not say the two callers mean the same thing by the answer \
             — §21's StateStore-versus-REVM question — only that the ask is one ask"
                .to_string(),
        );
    } else {
        push(
            CONDITION_STATE_SEMANTICS,
            CHECKABLE_NOT_MET,
            format!(
                "the two asks went to {} and {}, which are two providers and therefore two \
                 answers to compare",
                a.endpoint_id.as_deref().unwrap_or("<absent>"),
                b.endpoint_id.as_deref().unwrap_or("<absent>")
            ),
        );
    }

    push(
        CONDITION_LIFECYCLE_OWNERSHIP,
        NOT_CHECKABLE_FROM_A_RECORD,
        "no field of a recorded call says who holds an answer after it arrives, and M8.3.1's \
         reuse boundary is documented as one simulation's own; whether a later stage may be \
         served from an earlier stage's memory is a property of code this milestone is \
         forbidden to change (§2, §22)"
            .to_string(),
    );

    match (b.block_form, b.category) {
        (BLOCK_FORM_TAG, _) => push(
            CONDITION_NO_FRESHNESS_REQUIREMENT,
            CHECKABLE_NOT_MET,
            "the consumer asks a tag, which is a request for the newest answer the node \
             holds: a stated freshness requirement, and §7's example of the case where \
             matching data is not permission"
                .to_string(),
        ),
        (_, READ_CATEGORY_TRANSACTION_PREPARATION) => push(
            CONDITION_NO_FRESHNESS_REQUIREMENT,
            CHECKABLE_NOT_MET,
            "the consumer's §9 category is a read whose answer is a property of the head \
             (a fee, a gas ceiling, a pending nonce), so its purpose is to be fresh"
                .to_string(),
        ),
        (BLOCK_FORM_NUMBER, _) => push(
            CONDITION_NO_FRESHNESS_REQUIREMENT,
            NOT_CHECKABLE_FROM_A_RECORD,
            "the consumer names a height rather than a tag, which is weaker than a stated \
             freshness requirement and is not its absence: no recorded field says why the \
             code asked again"
                .to_string(),
        ),
        _ => push(
            CONDITION_NO_FRESHNESS_REQUIREMENT,
            NOT_CHECKABLE_FROM_A_RECORD,
            "the consumer carries no block term, so nothing here states or refuses a \
             freshness requirement"
                .to_string(),
        ),
    }
    out
}

/// §6's verdict for one pair, read off the duplicate class and the conditions' outcomes.
///
/// `safe_to_reuse` is *computed* — it is true when every condition resolved to
/// [`CHECKED_MET`] — which is what lets the tables be honest about a rule rather than
/// about an expectation. A pair whose block identity §23 settles in the negative is
/// refused here rather than left unknown, because §23 says so in terms.
///
/// The class is asked first, because §23's refusal is a statement about a pair that *was*
/// the same ask at two heights: a pair that differs in its slot, its method, or anything
/// else is not refused reuse, it has nothing to reuse.
pub fn reuse_verdict(
    duplicate_class: &'static str,
    same_block: SameBlock,
    conditions: &[ConditionOutcome],
) -> ReuseVerdict {
    if !is_duplicate_class(duplicate_class) {
        return ReuseVerdict::NotACandidate;
    }
    if same_block == SameBlock::No {
        return ReuseVerdict::RefusedByBlockIdentity;
    }
    if conditions.iter().all(|item| item.outcome == CHECKED_MET) {
        ReuseVerdict::SafeToReuse
    } else {
        ReuseVerdict::CandidateReuseUnknown
    }
}

/// §6's level A: which of §4's classes is a duplicate at all.
///
/// §4.4's two tags are two different semantics asking for two different things, and the
/// undetermined case is §4's own refusal to guess between them — neither is a repeat.
pub fn is_duplicate_class(duplicate_class: &'static str) -> bool {
    matches!(
        duplicate_class,
        DUPLICATE_EXACT | DUPLICATE_SEMANTIC | SAME_TARGET_DIFFERENT_BLOCK
    )
}

// ---------------------------------------------------------------------------
// §5, §13, §15, §16: the pairs one run forms, and the tables they fill
// ---------------------------------------------------------------------------

/// The four §14 names, and §12's per-run file. The rows in them are recomputable from the
/// runs' own `pipeline-calls.json`, which is the only source any function below reads.
pub const DUPLICATE_MATRIX_FILE: &str = "duplicate-matrix.json";
pub const DUPLICATE_SUMMARY_FILE: &str = "duplicate-summary.json";
pub const REUSE_CANDIDATES_FILE: &str = "reuse-candidates.json";
pub const STAGE_PAIRS_FILE: &str = "stage-pairs.json";
pub const CROSS_STAGE_RUN_FILE: &str = "cross-stage-duplicates.json";

/// The whole of §5's direction rule, in the one sentence a reader needs to re-derive any
/// count below. It is published in every table that uses it rather than only in a README,
/// because a matrix cell without its pair-formation rule is a number with no definition.
pub const PAIR_FORMATION_RULE: &str = "one ask is examined once, against the earliest earlier \
     ask that asked the same thing. If an earlier ask with the identical §8 identity exists, \
     the pair is §4.1's exact duplicate and its producer is the earliest ask of that identity. \
     If not, but an earlier ask of the same target exists — the same identity with the block \
     term removed — the producer is that earliest ask of the target, and §4's class is decided \
     by the two block terms. An ask with neither is the first ask for its target and forms no \
     pair. Pairs are formed between rows, and a row is one logical request with its HTTP tries \
     listed beside it, so §25's rule holds by construction: a call that tried three times is \
     one ask here and cannot contribute three duplicates.";

/// §5's one directed pair, with §4's class, §23's block answer, §6's five conditions and §6's
/// verdict all read off the two rows rather than asserted about them.
#[derive(Clone, Debug)]
pub struct DuplicatePair {
    pub producer: RpcReadKey,
    pub consumer: RpcReadKey,
    pub duplicate_class: &'static str,
    pub scope: &'static str,
    pub same_block: SameBlock,
    pub conditions: Vec<ConditionOutcome>,
    pub verdict: ReuseVerdict,
}

impl DuplicatePair {
    /// One pair, classified and judged by the four rules this module owns.
    fn between(producer: RpcReadKey, consumer: RpcReadKey) -> Self {
        let duplicate_class = classify_pair(&producer, &consumer);
        let scope = pair_scope(&producer, &consumer);
        let same_block = RpcReadKey::same_block(&producer, &consumer);
        let conditions = reuse_conditions(&producer, &consumer, duplicate_class, same_block);
        let verdict = reuse_verdict(duplicate_class, same_block, &conditions);
        Self {
            producer,
            consumer,
            duplicate_class,
            scope,
            same_block,
            conditions,
            verdict,
        }
    }

    /// §6's level B as §5's own word. `no` is a measurement — §23 settled it, or there was
    /// nothing to reuse — and `unknown` is an unmet or uncheckable condition. The two are not
    /// one finding and are never written as one value.
    pub fn reusable(&self) -> &'static str {
        match self.verdict {
            ReuseVerdict::SafeToReuse => "yes",
            ReuseVerdict::CandidateReuseUnknown => "unknown",
            ReuseVerdict::RefusedByBlockIdentity | ReuseVerdict::NotACandidate => "no",
        }
    }

    /// Whether §6's level A holds: one of §4's three duplicate classes.
    pub fn is_duplicate(&self) -> bool {
        is_duplicate_class(self.duplicate_class)
    }

    /// §23's `same_block` as three answers. Only two named, equal heights prove `true` and
    /// only two named, unequal heights prove `false`; a tag against anything is `null`, which
    /// §5 forbids writing as either of the other two.
    pub fn same_block_figure(&self) -> Option<bool> {
        self.same_block.figure()
    }

    /// The first condition that did not resolve to met, in [`REUSE_CONDITIONS`]' order — the
    /// reason §15 asks a candidate to carry.
    pub fn first_unmet_condition(&self) -> Option<&ConditionOutcome> {
        self.conditions
            .iter()
            .find(|condition| condition.outcome != CHECKED_MET)
    }

    pub fn to_json(&self) -> Value {
        let reason = match self.first_unmet_condition() {
            Some(condition) => format!(
                "{}: {} — {}",
                condition.condition, condition.outcome, condition.reason
            ),
            None => "every §6 condition resolved to met".to_string(),
        };
        json!({
            "run": self.consumer.run,
            "candidate_id": format!(
                "{}:rpc{}",
                self.consumer.run.as_deref().unwrap_or_default(),
                self.consumer.rpc_id.unwrap_or_default()
            ),
            "scope": self.scope,
            "producer": side_json(&self.producer, "producer"),
            "consumer": side_json(&self.consumer, "consumer"),
            "identity": identity_json(&self.consumer),
            "duplicate": self.is_duplicate(),
            "duplicate_type": self.duplicate_class,
            "producer_block": self.producer.block,
            "consumer_block": self.consumer.block,
            "producer_block_form": self.producer.block_form,
            "consumer_block_form": self.consumer.block_form,
            "same_block": self.same_block_figure(),
            "block_relation": self.same_block.as_str(),
            "reusable": self.reusable(),
            "safe_to_reuse": self.verdict.counts_as_safe(),
            "verdict": self.verdict.as_str(),
            "conditions": self
                .conditions
                .iter()
                .map(ConditionOutcome::to_json)
                .collect::<Vec<Value>>(),
            "reason": reason,
        })
    }
}

/// One side of a pair: §15's `stage` and `rpc_id`, plus the fields that let a reader find the
/// row again in the run's own `pipeline-calls.json` and in the trace line behind it.
fn side_json(key: &RpcReadKey, role: &str) -> Value {
    json!({
        "role": role,
        "stage": key.stage,
        "stage_label": key.stage.clone().unwrap_or_else(unstamped_label),
        "caller": key.caller,
        "sink": key.sink,
        "rpc_id": key.rpc_id,
        "logical_request_id": key.logical_request_id,
        "category": key.category,
        "started_ns": key.started_ns,
        "finished_ns": key.finished_ns,
        "duration_ns": key.duration_ns,
        "physical_attempts": key.physical_attempts,
        "success": key.success,
    })
}

/// §8's identity as one ask named it: §8's own fields, with the block filled into whichever
/// of its two forms the record actually carries, and the refusal note beside the asks that have
/// no identity to publish.
fn identity_json(key: &RpcReadKey) -> Value {
    json!({
        "method": key.method,
        "chain_id": key.chain_id,
        "block_number": match key.block_form {
            BLOCK_FORM_NUMBER => key.block.clone().into(),
            _ => Value::Null,
        },
        "block_tag": match key.block_form {
            BLOCK_FORM_TAG => key.block.clone().into(),
            _ => Value::Null,
        },
        "address": key.address,
        "slot": key.slot,
        "endpoint_id": key.endpoint_id,
        "identity": key.identity(),
        "identity_without_block": key.identity_without_block(),
        "identity_source": key.identity_source,
        "identity_note": key.identity_note,
        "terms": key
            .terms
            .iter()
            .map(|(term, value)| json!([term, value]))
            .collect::<Vec<Value>>(),
    })
}

fn unstamped_label() -> String {
    crate::diagnosis::PIPELINE_UNSTAMPED.to_string()
}

/// One run's rows, classified. Every figure the four tables print is a fold over this, so a
/// question about a pooled number can be answered by naming the run it came from.
#[derive(Clone, Debug)]
pub struct RunAnalysis {
    pub provenance: Value,
    pub asks: usize,
    pub asks_with_identity: usize,
    pub asks_without_identity: usize,
    pub identity_groups: usize,
    pub target_groups: usize,
    pub first_asks: usize,
    pub rows_disagreeing_with_key: usize,
    pub rows_with_more_than_one_try: usize,
    pub categories: BTreeMap<&'static str, usize>,
    /// Every ask this run stamped, counted by the stage label its row carries — including pairs'
    /// producers and consumers, and including a stage that issued one call. §10's rule that a
    /// stage with no RPC is reported as a measured zero needs the counts of the stages that did
    /// issue calls to be stated with the same granularity.
    pub asks_by_stage: BTreeMap<String, usize>,
    pub pairs: Vec<DuplicatePair>,
}

impl RunAnalysis {
    /// §5's pairs over one run's published rows, in that run's own clock order.
    pub fn of(provenance: Value, rows: &[Value]) -> Self {
        let chain_id = provenance["chain_id"].as_u64();
        let keys: Vec<RpcReadKey> = rows
            .iter()
            .map(|row| RpcReadKey::of_row(row, chain_id))
            .collect();
        let mut order: Vec<usize> = (0..keys.len()).collect();
        order.sort_by_key(|index| {
            (
                keys[*index].started_ns.unwrap_or(u64::MAX),
                keys[*index].sink.clone().unwrap_or_default(),
                keys[*index].rpc_id.unwrap_or(u64::MAX),
            )
        });

        let mut targets: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        let mut identities: BTreeSet<String> = BTreeSet::new();
        let mut categories: BTreeMap<&'static str, usize> = BTreeMap::new();
        let mut asks_without_identity = 0_usize;
        let mut rows_disagreeing_with_key = 0_usize;
        let mut rows_with_more_than_one_try = 0_usize;
        let mut asks_by_stage: BTreeMap<String, usize> = BTreeMap::new();
        for index in &order {
            let key = &keys[*index];
            *categories.entry(key.category).or_default() += 1;
            *asks_by_stage
                .entry(key.stage.clone().unwrap_or_else(unstamped_label))
                .or_default() += 1;
            if !key.row_matches_key {
                rows_disagreeing_with_key += 1;
            }
            if key.physical_attempts.unwrap_or(1) > 1 {
                rows_with_more_than_one_try += 1;
            }
            let Some(target) = key.identity_without_block().or_else(|| key.identity()) else {
                asks_without_identity += 1;
                continue;
            };
            if let Some(identity) = key.identity() {
                identities.insert(identity);
            }
            targets.entry(target).or_default().push(*index);
        }

        // One pass per target group rather than one per ask: the group's earliest ask is the
        // producer of every later ask that has no identical predecessor, and a later ask's
        // identical predecessor is the first earlier member whose full identity matches.
        let mut pairs: Vec<DuplicatePair> = Vec::new();
        let mut first_asks = 0_usize;
        for members in targets.values() {
            first_asks += 1;
            for position in 1..members.len() {
                let consumer_index = members[position];
                let producer_index = members[..position]
                    .iter()
                    .find(|earlier| keys[**earlier].identity() == keys[consumer_index].identity())
                    .copied()
                    .unwrap_or(members[0]);
                let pair = DuplicatePair::between(
                    keys[producer_index].clone(),
                    keys[consumer_index].clone(),
                );
                // Two asks of one target whose identities differ in a term other than the
                // block cannot happen inside a target group, so a refusal here is a rule that
                // changed under this loop: it is dropped from the pairs and counted nowhere,
                // which the class tally's own total makes visible if it ever does.
                if pair.duplicate_class != NOT_A_DUPLICATE {
                    pairs.push(pair);
                }
            }
        }
        pairs.sort_by(|a, b| {
            (
                a.consumer.started_ns.unwrap_or(u64::MAX),
                a.consumer.sink.clone(),
                a.consumer.rpc_id,
            )
                .cmp(&(
                    b.consumer.started_ns.unwrap_or(u64::MAX),
                    b.consumer.sink.clone(),
                    b.consumer.rpc_id,
                ))
        });

        Self {
            provenance,
            asks: rows.len(),
            asks_with_identity: rows.len() - asks_without_identity,
            asks_without_identity,
            identity_groups: identities.len(),
            target_groups: targets.len(),
            first_asks,
            rows_disagreeing_with_key,
            rows_with_more_than_one_try,
            categories,
            asks_by_stage,
            pairs,
        }
    }

    /// §17's cross tally: pairs per duplicate class per scope, keyed so the order a table
    /// lists them in is [`DUPLICATE_CLASSES`]' and [`SCOPES`]' rather than a hash map's.
    pub fn counts(&self) -> BTreeMap<(&'static str, &'static str), usize> {
        let mut out: BTreeMap<(&'static str, &'static str), usize> = BTreeMap::new();
        for pair in &self.pairs {
            *out.entry((pair.duplicate_class, pair.scope)).or_default() += 1;
        }
        out
    }

    /// The run's own §12 record: its asks, its groups, and every pair it formed, so a pooled
    /// figure can be checked against the run that produced it without a second derivation.
    pub fn to_json(&self) -> Value {
        json!({
            "question": "§5's directed duplicate pairs of this run: which ask repeated which, \
                         what §4 class each pair is, and what §6's three levels came to",
            "unit": "pairs and asks",
            "canonicalization_schema": CANONICALIZATION_SCHEMA,
            "pair_formation_rule": PAIR_FORMATION_RULE,
            "duplicate_classes": DUPLICATE_CLASSES,
            "scopes": SCOPES,
            "reuse_conditions": REUSE_CONDITIONS,
            "read_categories": READ_CATEGORIES,
            "run_provenance": self.provenance,
            "asks": self.asks,
            "asks_with_identity": self.asks_with_identity,
            "asks_without_identity": self.asks_without_identity,
            "identity_groups": self.identity_groups,
            "target_groups": self.target_groups,
            "first_asks_for_their_target": self.first_asks,
            "pairs": self.pairs.len(),
            "asks_by_stage": self
                .asks_by_stage
                .iter()
                .map(|(stage, count)| json!({ "stage": stage, "asks": count }))
                .collect::<Vec<Value>>(),
            "pairs_by_class_and_scope": self
                .counts()
                .iter()
                .map(|((class, scope), count)| {
                    json!({ "duplicate_type": class, "scope": scope, "pairs": count })
                })
                .collect::<Vec<Value>>(),
            "asks_by_read_category": self
                .categories
                .iter()
                .map(|(category, count)| json!({ "category": category, "asks": count }))
                .collect::<Vec<Value>>(),
            "integrity": {
                "rows_whose_fields_disagree_with_their_key": self.rows_disagreeing_with_key,
                "rows_with_more_than_one_physical_attempt": self.rows_with_more_than_one_try,
                "note": "§25's retry rule is enforced by one row being one logical request, so \
                         a run whose rows are all single-try has no retry that could be \
                         mis-counted — reported as a measurement, not as a pass",
            },
            "pairs_rows": self.pairs.iter().map(DuplicatePair::to_json).collect::<Vec<Value>>(),
            "recompute": "every row of `pairs_rows` is two rows of the run's own \
                          `pipeline-calls.json` read through `RpcReadKey::of_row`; the class, \
                          the block relation and the five conditions are functions of those two \
                          rows and of nothing else",
        })
    }

    /// The run's name, as the evidence tree spells its directory.
    fn run_name(&self) -> &str {
        self.provenance["run"].as_str().unwrap_or_default()
    }

    /// The one-row-per-run entry §16's `per_run` column carries: the same figures the headline
    /// reports, folded over this run alone, and naming the file the full pair list lives in.
    pub fn summary_json(&self) -> Value {
        json!({
            "run": self.provenance["run"],
            "detail_file": format!("{}/{}", self.run_name(), CROSS_STAGE_RUN_FILE),
            "asks": self.asks,
            "asks_with_identity": self.asks_with_identity,
            "identity_groups": self.identity_groups,
            "target_groups": self.target_groups,
            "pairs": self.pairs.len(),
            "pairs_by_class_and_scope": self
                .counts()
                .iter()
                .map(|((class, scope), count)| {
                    json!({ "duplicate_type": class, "scope": scope, "pairs": count })
                })
                .collect::<Vec<Value>>(),
            "reuse_candidates": self
                .pairs
                .iter()
                .filter(|p| p.verdict.counts_as_candidate())
                .count(),
            "safe_to_reuse": self
                .pairs
                .iter()
                .filter(|p| p.verdict.counts_as_safe())
                .count(),
            "refused_by_block_identity": self
                .pairs
                .iter()
                .filter(|p| p.verdict == ReuseVerdict::RefusedByBlockIdentity)
                .count(),
        })
    }
}

/// §13's named pairs, as the task book words them against the stage names this build's trace
/// actually stamps.
///
/// A pair whose producer or consumer side names no stage this build stamps is published as
/// [`PAIR_STAGE_ABSENT`] with the reason, which is §13's own instruction not to invent data for
/// a path the pipeline does not have. A pair whose sides both stamp calls but whose cell is
/// empty is a *measurement* of zero, and says so differently.
pub struct NamedStagePair {
    pub label: &'static str,
    pub producer_stages: &'static [&'static str],
    pub consumer_stages: &'static [&'static str],
    pub note: &'static str,
}

/// The cell has an answer: how many pairs, of which classes.
pub const PAIR_MEASURED: &str = "measured";
/// Both sides stamp calls, and no pair of them is a duplicate of any class.
pub const PAIR_NO_DUPLICATES: &str = "measured_no_duplicates";
/// One side stamps calls and the other side's stage ran without issuing one — which is a
/// measured zero on that side, not a missing path.
pub const PAIR_NO_ASKS_OBSERVED: &str = "measured_one_side_issued_no_calls";
/// §13's `not_applicable`: this build stamps no stage by that name, so the pair is not a path
/// the pipeline has.
pub const PAIR_STAGE_ABSENT: &str = "not_applicable_no_such_stage";

/// §13's eleven pairs, in §13's order.
pub const NAMED_STAGE_PAIRS: [NamedStagePair; 11] = [
    NamedStagePair {
        label: "Detection -> Preflight",
        producer_stages: &["opportunity_detection"],
        consumer_stages: &["preflight"],
        note: "detection prices the pair at the pin; preflight re-prices it before the build",
    },
    NamedStagePair {
        label: "Detection -> Simulation",
        producer_stages: &["opportunity_detection"],
        consumer_stages: &["simulation"],
        note: "§20's pair: the same reserves read by the opportunity finder and by the EVM's \
               state provider",
    },
    NamedStagePair {
        label: "Detection -> Build",
        producer_stages: &["opportunity_detection"],
        consumer_stages: &["build"],
        note: "detection's answers against the state the built transaction is described from",
    },
    NamedStagePair {
        label: "State Update -> Preflight",
        producer_stages: &["state_update", "graph_update"],
        consumer_stages: &["preflight"],
        note: "§21's pair, if the state update ever asks a node: this build's graph is rebuilt \
               from recorded events, so these stages may issue no calls at all",
    },
    NamedStagePair {
        label: "State Update -> Simulation",
        producer_stages: &["state_update", "graph_update"],
        consumer_stages: &["simulation"],
        note: "§21's own question — StateStore versus REVM canonical state — which is why this \
               pair is reported even when its cell is empty",
    },
    NamedStagePair {
        label: "Opportunity -> Preflight",
        producer_stages: &[],
        consumer_stages: &["preflight"],
        note: "this build has no stage named `opportunity`: the candidate list is assembled \
               inside `opportunity_detection`, which issues the calls §13's Detection rows \
               already count, so there is no second producer to compare with",
    },
    NamedStagePair {
        label: "Opportunity -> Simulation",
        producer_stages: &[],
        consumer_stages: &["simulation"],
        note: "the same absence as Opportunity -> Preflight",
    },
    NamedStagePair {
        label: "Preflight -> Simulation",
        producer_stages: &["preflight"],
        consumer_stages: &["simulation"],
        note: "the gate's facts against the state the simulation ran on",
    },
    NamedStagePair {
        label: "Preflight -> Build",
        producer_stages: &["preflight"],
        consumer_stages: &["build"],
        note: "§19's pair: nonce, balance, fee, block, chain id, gas",
    },
    NamedStagePair {
        label: "Simulation -> Build",
        producer_stages: &["simulation"],
        consumer_stages: &["build"],
        note: "§13's pair, and the one M8.3.1's cache cannot answer: the cache is one \
               simulation's own, so a build that reads what a simulation read is outside its \
               scope (§18)",
    },
    NamedStagePair {
        label: "Simulation -> Execution Preparation",
        producer_stages: &["simulation"],
        consumer_stages: &[],
        note: "this build stamps no stage named execution preparation: the gate and the \
               before-snapshot reads carry `build`, so this pair's numbers are Simulation -> \
               Build's, and M8.4.1 §13 already records the absence",
    },
];

/// The §13 view: one row per named pair, plus the pairs the runs produced that §13 does not
/// name, so the named list cannot hide an observed redundancy.
pub fn stage_pairs(runs: &[RunAnalysis]) -> Value {
    let pairs: Vec<&DuplicatePair> = runs.iter().flat_map(|run| run.pairs.iter()).collect();
    let asks_of = |stages: &[&str]| -> usize {
        runs.iter()
            .map(|run| {
                run.asks_by_stage
                    .iter()
                    .filter(|(stage, _)| stages.contains(&stage.as_str()))
                    .map(|(_, count)| *count)
                    .sum::<usize>()
            })
            .sum()
    };

    let named = NAMED_STAGE_PAIRS
        .iter()
        .map(|pair| {
            let forward = pairs
                .iter()
                .filter(|p| {
                    stage_is(p.producer.stage.as_deref(), pair.producer_stages)
                        && stage_is(p.consumer.stage.as_deref(), pair.consumer_stages)
                })
                .copied()
                .collect::<Vec<&DuplicatePair>>();
            let reverse = pairs
                .iter()
                .filter(|p| {
                    stage_is(p.producer.stage.as_deref(), pair.consumer_stages)
                        && stage_is(p.consumer.stage.as_deref(), pair.producer_stages)
                })
                .count();
            let producer_asks = asks_of(pair.producer_stages);
            let consumer_asks = asks_of(pair.consumer_stages);
            let status = if pair.producer_stages.is_empty() || pair.consumer_stages.is_empty() {
                PAIR_STAGE_ABSENT
            } else if producer_asks == 0 || consumer_asks == 0 {
                PAIR_NO_ASKS_OBSERVED
            } else if forward.is_empty() {
                PAIR_NO_DUPLICATES
            } else {
                PAIR_MEASURED
            };
            json!({
                "pair": pair.label,
                "producer_stages": pair.producer_stages,
                "consumer_stages": pair.consumer_stages,
                "status": status,
                "why": pair.note,
                "producer_asks": producer_asks,
                "consumer_asks": consumer_asks,
                "pairs": forward.len(),
                "pairs_by_class": class_counts(&forward),
                "pairs_by_scope": scope_counts(&forward),
                "pairs_in_the_other_direction": reverse,
                "candidates": forward
                    .iter()
                    .filter(|p| p.verdict.counts_as_candidate())
                    .count(),
                "safe_to_reuse": forward.iter().filter(|p| p.verdict.counts_as_safe()).count(),
                "identities": forward
                    .iter()
                    .filter_map(|p| p.consumer.identity())
                    .collect::<BTreeSet<String>>()
                    .into_iter()
                    .collect::<Vec<String>>(),
            })
        })
        .collect::<Vec<Value>>();

    // Every observed producer/consumer label that no §13 row covers, named by the labels the
    // record carries rather than folded into the nearest named pair.
    let covered = NAMED_STAGE_PAIRS
        .iter()
        .fold(BTreeSet::new(), |mut acc, pair| {
            for p in pairs.iter().filter(|p| {
                stage_is(p.producer.stage.as_deref(), pair.producer_stages)
                    && stage_is(p.consumer.stage.as_deref(), pair.consumer_stages)
            }) {
                acc.insert(cell_label(p));
            }
            acc
        });
    let mut extra: BTreeMap<String, Vec<&DuplicatePair>> = BTreeMap::new();
    for pair in &pairs {
        let label = cell_label(pair);
        if !covered.contains(&label) {
            extra.entry(label).or_default().push(pair);
        }
    }
    let unnamed = extra
        .iter()
        .map(|(label, members)| {
            json!({
                "pair": label,
                "status": PAIR_MEASURED,
                "why": "a producer/consumer combination the runs produced that §13's eleven \
                       named pairs do not list",
                "pairs": members.len(),
                "pairs_by_class": class_counts(members),
                "pairs_by_scope": scope_counts(members),
                "identities": members
                    .iter()
                    .filter_map(|p| p.consumer.identity())
                    .collect::<BTreeSet<String>>()
                    .into_iter()
                    .collect::<Vec<String>>(),
            })
        })
        .collect::<Vec<Value>>();

    json!({
        "question": "§13's stage pairs, one row each, with the duplicates each cell holds and \
                     the reason a cell has none",
        "unit": "pairs and asks",
        "canonicalization_schema": CANONICALIZATION_SCHEMA,
        "pair_formation_rule": PAIR_FORMATION_RULE,
        "statuses": [PAIR_MEASURED, PAIR_NO_DUPLICATES, PAIR_NO_ASKS_OBSERVED, PAIR_STAGE_ABSENT],
        "named_pairs_are_never_omitted": "a pair this build cannot measure is reported with the \
                                         reason, because a missing row would read as a zero",
        "rows": named,
        "pairs_observed_but_not_named": unnamed,
        "recompute": "each row is a filter of `reuse-candidates.json`'s rows on the producer \
                      and consumer stage labels those rows carry",
    })
}

/// The cell a pair belongs to, in the same words the §13 table uses for a named pair.
fn cell_label(pair: &DuplicatePair) -> String {
    format!(
        "{} -> {}",
        pair.producer.stage.clone().unwrap_or_else(unstamped_label),
        pair.consumer.stage.clone().unwrap_or_else(unstamped_label)
    )
}

fn stage_is(stage: Option<&str>, stages: &[&str]) -> bool {
    stage.is_some_and(|name| stages.contains(&name))
}

fn class_counts(pairs: &[&DuplicatePair]) -> BTreeMap<&'static str, usize> {
    let mut out: BTreeMap<&'static str, usize> = DUPLICATE_CLASSES
        .iter()
        .map(|class| (*class, 0_usize))
        .collect();
    for pair in pairs {
        *out.entry(pair.duplicate_class).or_default() += 1;
    }
    out.retain(|_, count| *count > 0);
    out
}

fn scope_counts(pairs: &[&DuplicatePair]) -> BTreeMap<&'static str, usize> {
    let mut out: BTreeMap<&'static str, usize> =
        SCOPES.iter().map(|scope| (*scope, 0_usize)).collect();
    for pair in pairs {
        *out.entry(pair.scope).or_default() += 1;
    }
    out.retain(|_, count| *count > 0);
    out
}

/// §14's matrix: one row per producer stage × consumer stage × duplicate class, with the
/// asks that produced it named.
///
/// The per-run columns are part of the row rather than a second table, because §12's question
/// is whether the relation reproduces: a reader has to see the same cell three times to answer
/// it, and a pooled number alone cannot show whether it came from three runs or from one.
pub fn duplicate_matrix(runs: &[RunAnalysis]) -> Value {
    let pairs: Vec<&DuplicatePair> = runs.iter().flat_map(|run| run.pairs.iter()).collect();
    let mut cells: BTreeMap<(String, String, &'static str, &'static str), Vec<&DuplicatePair>> =
        BTreeMap::new();
    for pair in &pairs {
        cells
            .entry((
                pair.producer.stage.clone().unwrap_or_else(unstamped_label),
                pair.consumer.stage.clone().unwrap_or_else(unstamped_label),
                pair.duplicate_class,
                pair.scope,
            ))
            .or_default()
            .push(pair);
    }

    let rows = cells
        .iter()
        .map(|((producer, consumer, class, scope), members)| {
            let per_run = runs.iter().map(|run| {
                json!({
                    "run": run.provenance["run"],
                    "pairs": run
                        .pairs
                        .iter()
                        .filter(|p| {
                            stage_matches(p.producer.stage.as_deref(), producer)
                                && stage_matches(p.consumer.stage.as_deref(), consumer)
                                && p.duplicate_class == *class
                        })
                        .count(),
                })
            });
            json!({
                "producer_stage": producer,
                "consumer_stage": consumer,
                "duplicate_type": class,
                "scope": scope,
                "pairs": members.len(),
                "pairs_per_run": per_run.collect::<Vec<Value>>(),
                "methods": members
                    .iter()
                    .map(|p| p.consumer.method.clone())
                    .collect::<BTreeSet<String>>()
                    .into_iter()
                    .collect::<Vec<String>>(),
                "producer_categories": members
                    .iter()
                    .map(|p| p.producer.category.to_string())
                    .collect::<BTreeSet<String>>()
                    .into_iter()
                    .collect::<Vec<String>>(),
                "consumer_categories": members
                    .iter()
                    .map(|p| p.consumer.category.to_string())
                    .collect::<BTreeSet<String>>()
                    .into_iter()
                    .collect::<Vec<String>>(),
                "identities": members
                    .iter()
                    .filter_map(|p| p.consumer.identity())
                    .collect::<BTreeSet<String>>()
                    .into_iter()
                    .collect::<Vec<String>>(),
                "block_relations": members
                    .iter()
                    .map(|p| p.same_block.as_str().to_string())
                    .collect::<BTreeSet<String>>()
                    .into_iter()
                    .collect::<Vec<String>>(),
                "safe_to_reuse": members.iter().filter(|p| p.verdict.counts_as_safe()).count(),
            })
        })
        .collect::<Vec<Value>>();

    json!({
        "question": "which stage's answer is the same ask as which later stage's, per §4's \
                     duplicate class, per run",
        "unit": "pairs",
        "canonicalization_schema": CANONICALIZATION_SCHEMA,
        "pair_formation_rule": PAIR_FORMATION_RULE,
        "duplicate_classes": DUPLICATE_CLASSES,
        "scopes": SCOPES,
        "runs": runs.len(),
        "rows": rows,
        "totals": {
            "pairs": pairs.len(),
            "by_class": class_counts(&pairs),
            "by_scope": scope_counts(&pairs),
        },
        "recompute": "one row per (producer stage, consumer stage, class, scope) over every \
                      pair of every run in `runs`; the identity list is the §8 identity of \
                      each pair's consumer, so a cell can be traced to the rows it came from",
    })
}

/// Whether a row's stage label is the cell's, with the unstamped label matching an absent
/// stage rather than nothing.
fn stage_matches(stage: Option<&str>, label: &str) -> bool {
    stage.map(str::to_owned).unwrap_or_else(unstamped_label) == label
}

/// §15's file: every pair this build found, as one directed row, with §6's three levels and
/// the condition list behind each.
pub fn reuse_candidates(runs: &[RunAnalysis]) -> Value {
    let rows = runs
        .iter()
        .flat_map(|run| run.pairs.iter())
        .map(DuplicatePair::to_json)
        .collect::<Vec<Value>>();
    let pairs: Vec<&DuplicatePair> = runs.iter().flat_map(|run| run.pairs.iter()).collect();
    json!({
        "question": "§15's reuse candidates: one row per directed pair, with duplicate, \
                     reusable and safe_to_reuse as three separate verdicts and the condition \
                     list each verdict came from",
        "unit": "pairs",
        "canonicalization_schema": CANONICALIZATION_SCHEMA,
        "pair_formation_rule": PAIR_FORMATION_RULE,
        "levels": {
            "duplicate": "§4's first three classes — the two asks are the same ask",
            "reusable": "§6's level B: yes only when every condition resolved to met, no when \
                         a condition was measured in the negative, unknown when one is unmet \
                         or not checkable from a record",
            "safe_to_reuse": "§6's level C, computed from the five conditions listed on the \
                              row rather than asserted by it",
        },
        "reuse_conditions": REUSE_CONDITIONS,
        "conditions_are_never_omitted": "every row carries every condition in the order above, \
                                         including the ones this build cannot check",
        "runs": runs.len(),
        "pairs": rows.len(),
        "candidates": pairs.iter().filter(|p| p.verdict.counts_as_candidate()).count(),
        "safe_to_reuse": pairs.iter().filter(|p| p.verdict.counts_as_safe()).count(),
        "refused_by_block_identity": pairs
            .iter()
            .filter(|p| p.verdict == ReuseVerdict::RefusedByBlockIdentity)
            .count(),
        "rows": rows,
        "recompute": "each row names its two rows by run, sink and rpc_id; the identity is \
                      those rows' §8 terms and the verdict is `reuse_verdict` over the five \
                      conditions printed beside it",
    })
}

/// §16's file: the cross-run figures, each broken out by method, producer stage and consumer
/// stage, with every declaration a reader needs to re-rank the same integers.
pub fn duplicate_summary(runs: &[RunAnalysis]) -> Value {
    let pairs: Vec<&DuplicatePair> = runs.iter().flat_map(|run| run.pairs.iter()).collect();
    let count =
        |predicate: fn(&&DuplicatePair) -> bool| pairs.iter().filter(|p| predicate(p)).count();
    let by = |dimension: &str| -> Vec<Value> {
        let mut cells: BTreeMap<String, Vec<&DuplicatePair>> = BTreeMap::new();
        for pair in &pairs {
            let key = match dimension {
                "method" => pair.consumer.method.clone(),
                "producer_stage" => pair.producer.stage.clone().unwrap_or_else(unstamped_label),
                _ => pair.consumer.stage.clone().unwrap_or_else(unstamped_label),
            };
            cells.entry(key).or_default().push(pair);
        }
        cells
            .iter()
            .map(|(key, members)| {
                json!({
                    dimension: key,
                    "pairs": members.len(),
                    "exact_duplicate": members.iter().filter(|p| p.duplicate_class == DUPLICATE_EXACT).count(),
                    "semantic_duplicate": members.iter().filter(|p| p.duplicate_class == DUPLICATE_SEMANTIC).count(),
                    "same_target_different_block": members.iter().filter(|p| p.duplicate_class == SAME_TARGET_DIFFERENT_BLOCK).count(),
                    "same_method_different_semantics": members.iter().filter(|p| p.duplicate_class == SAME_METHOD_DIFFERENT_SEMANTICS).count(),
                    "same_target_block_undetermined": members.iter().filter(|p| p.duplicate_class == SAME_TARGET_BLOCK_UNDETERMINED).count(),
                    "cross_stage": members.iter().filter(|p| p.scope == SCOPE_CROSS_STAGE).count(),
                    "intra_stage": members.iter().filter(|p| p.scope == SCOPE_INTRA_STAGE).count(),
                    "reuse_candidates": members.iter().filter(|p| p.verdict.counts_as_candidate()).count(),
                    "safe_to_reuse": members.iter().filter(|p| p.verdict.counts_as_safe()).count(),
                })
            })
            .collect()
    };

    json!({
        "question": "§16's cross-run figures: how many asks the runs made, how many of them \
                     repeated an earlier ask, and how many of those repeats this build can \
                     call safe to serve from the earlier answer",
        "unit": "counts, no ratios without their two terms",
        "canonicalization_schema": CANONICALIZATION_SCHEMA,
        "pair_formation_rule": PAIR_FORMATION_RULE,
        "duplicate_classes_are_mutually_exclusive": "every pair lands in exactly one of \
                                                     DUPLICATE_CLASSES, and the class counts \
                                                     below sum to the pair count",
        "runs": runs.len(),
        "run_names": runs.iter().map(|run| run.provenance["run"].clone()).collect::<Vec<Value>>(),
        "total_asks": runs.iter().map(|run| run.asks).sum::<usize>(),
        "asks_with_identity": runs.iter().map(|run| run.asks_with_identity).sum::<usize>(),
        "asks_without_identity": runs.iter().map(|run| run.asks_without_identity).sum::<usize>(),
        "identity_groups": runs.iter().map(|run| run.identity_groups).sum::<usize>(),
        "target_groups": runs.iter().map(|run| run.target_groups).sum::<usize>(),
        "first_asks_for_their_target": runs.iter().map(|run| run.first_asks).sum::<usize>(),
        "duplicate_pairs": pairs.len(),
        "exact_duplicate_pairs": count(|p| p.duplicate_class == DUPLICATE_EXACT),
        "semantic_duplicate_pairs": count(|p| p.duplicate_class == DUPLICATE_SEMANTIC),
        "same_target_different_block_pairs": count(|p| p.duplicate_class == SAME_TARGET_DIFFERENT_BLOCK),
        "same_method_different_semantics_pairs": count(|p| p.duplicate_class == SAME_METHOD_DIFFERENT_SEMANTICS),
        "same_target_block_undetermined_pairs": count(|p| p.duplicate_class == SAME_TARGET_BLOCK_UNDETERMINED),
        "cross_stage_pairs": count(|p| p.scope == SCOPE_CROSS_STAGE),
        "intra_stage_pairs": count(|p| p.scope == SCOPE_INTRA_STAGE),
        "same_logical_request_pairs": count(|p| p.scope == SCOPE_SAME_LOGICAL_REQUEST),
        "reuse_candidates": count(|p| p.verdict.counts_as_candidate()),
        "unknown_reuse_candidates": count(|p| p.verdict == ReuseVerdict::CandidateReuseUnknown),
        "safe_reuse_candidates": count(|p| p.verdict.counts_as_safe()),
        "refused_by_block_identity": count(|p| p.verdict == ReuseVerdict::RefusedByBlockIdentity),
        "semantic_duplicate": {
            "figure": 0,
            "status": "not_measurable_from_this_record",
            "mechanism": "§4.2's two spellings of one ask cannot be told apart in this \
                         corpus's rows because the collapse happens at record time: \
                         `describe_call` normalizes the block parameter, the address, the \
                         storage word and the calldata before any of them is stored, and \
                         `pipeline_call_row` lowercases the target again, so a row holds one \
                         spelling of each term by the time this module reads it",
            "consequence": "an exact duplicate here is exact *after* that normalization, which \
                           is §4.1's param-semantics condition rather than §4.2's literal \
                           difference — so the exact count is not an upper bound that excludes \
                           semantic duplicates, it is a count that already absorbed them",
            "measured": "identity groups whose members spell one term two ways, counted over \
                         the runs' published rows",
        },
        "integrity": {
            "rows_whose_fields_disagree_with_their_key": runs
                .iter()
                .map(|run| run.rows_disagreeing_with_key)
                .sum::<usize>(),
            "rows_with_more_than_one_physical_attempt": runs
                .iter()
                .map(|run| run.rows_with_more_than_one_try)
                .sum::<usize>(),
            "note": "§25's retry rule needs a retry to be tested against, and these runs \
                     recorded none; the rule is enforced by one row being one logical request, \
                     and canonicalization's own tests cover the case a live run did not",
        },
        "by_method": by("method"),
        "by_producer_stage": by("producer_stage"),
        "by_consumer_stage": by("consumer_stage"),
        "per_run": runs.iter().map(RunAnalysis::summary_json).collect::<Vec<Value>>(),
        "declarations": {
            "identity_rules": "crates/chain/src/rpc_trace.rs's §12 key list, read back by \
                               canonicalization.rs, plus one addition this milestone declares: \
                               a method §12 refuses that takes no parameters is identified by \
                               its method name and its endpoint digest",
            "class_rules": DUPLICATE_CLASSES.to_vec(),
            "reuse_rule": "safe_to_reuse is computed as every condition of REUSE_CONDITIONS \
                           resolving to checked_met; lifecycle ownership is not checkable from \
                           a recorded call, so a pair can be a duplicate at one height on one \
                           endpoint and still be unknown",
            "block_rule": "§23: two named, unequal heights end the question in the negative; a \
                           tag against a height, or a tag against a tag, is not a height \
                           comparison and is reported as not met rather than as violated",
            "freshness_rule": "a consumer that asks a tag, or asks a §9 category whose answer is \
                               a property of the head, has stated a freshness requirement; a \
                               consumer that names a height has not stated its absence",
        },
        "recompute": "every figure is a fold over `reuse-candidates.json`'s rows, which are \
                      pairs of rows of the runs' own `pipeline-calls.json`",
    })
}

/// The provenance one [`RunAnalysis`] publishes about its run: the fields a reader needs to
/// place the run (which build, which mode, which chain, which height, which endpoint) and
/// nothing else. Taken from the run record M8.4.1's own assembly publishes, so this module
/// adds no field to a run and re-derives none.
pub fn run_provenance(run: &Value) -> Value {
    json!({
        "run": run["run"],
        "source": run["source"],
        "chain_id": run["chain_id"],
        "block_number": run["block_number"],
        "endpoint_id": run["endpoint_id"],
        "execution_mode": run["execution_mode"],
        "git_revision": run["git_revision"],
        "generated_at_unix_ms": run["generated_at_unix_ms"],
        "calls": run["calls"].as_array().map_or(0, Vec::len),
    })
}

/// One run's rows, classified. The public form of [`RunAnalysis::of`] for callers that hold
/// M8.4.1's run records rather than a row slice.
pub fn analyze_run(run: &Value) -> RunAnalysis {
    let rows = run["calls"]
        .as_array()
        .map_or_else(Vec::new, |rows| rows.clone());
    RunAnalysis::of(run_provenance(run), &rows)
}

/// Every run's analysis, in the order the runs were given.
pub fn analyze_runs(runs: &[Value]) -> Vec<RunAnalysis> {
    runs.iter().map(analyze_run).collect()
}

/// §14's four root tables, keyed by their file names — the shape
/// [`crate::diagnosis::dependency_tables_of`] uses, so the evidence writer treats this
/// milestone's tables like that milestone's and a reader has one convention, not two.
///
/// This is the whole of M8.4.2's output surface: four tables over the runs' published rows,
/// with no new tracing, no new schema field and no second record of any call.
pub fn cross_stage_tables(runs: &[Value]) -> Value {
    let analyses = analyze_runs(runs);
    json!({
        DUPLICATE_MATRIX_FILE: duplicate_matrix(&analyses),
        DUPLICATE_SUMMARY_FILE: duplicate_summary(&analyses),
        REUSE_CANDIDATES_FILE: reuse_candidates(&analyses),
        STAGE_PAIRS_FILE: stage_pairs(&analyses),
    })
}

/// §12's per-run file: one run's own pairs, un-pooled, so a figure in a root table can be
/// traced to the single run that carried it.
pub fn cross_stage_run_table(run: &Value) -> Value {
    analyze_run(run).to_json()
}

#[cfg(test)]
mod tests;
