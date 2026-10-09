//! §41's last arrow, read back off the chain the transaction was sent to.
//!
//! The lane's own tests (`stage_matrix.rs`, `lane_matrix.rs`) drive a scripted endpoint,
//! which is what §40 allows and what makes the ladder's rungs provable one at a time. What
//! they cannot prove is the thing only a node can say: that a transaction this repository
//! built, priced and signed was accepted, mined, and is still there when asked about
//! again. That is the §35 controlled validation transaction, and this file is its receipt
//! — `data/evidence/m6/validation/node-answers-37503978.json`, the endpoint's own answers
//! about transaction 0x8a0ca6… as of the run that wrote them.
//!
//! Two things are pinned here that the live run itself did not settle:
//!
//! * **The binding holds on real data.** [`evm_execution::bind`] is §27's three-part check
//!   run against a receipt the node produced and the §52 row the lane wrote, and the
//!   tracker's `Included` is reached through the same `parse_receipt` the live submitter
//!   uses — so this is the decode path under test, not a paraphrase of it.
//! * **A block the endpoint will not name is not a broken receipt.** The live run of that
//!   transaction ended `failed` for exactly that reason: the node answered
//!   `eth_getTransactionReceipt` with block 37 503 978 a beat before it would answer
//!   `eth_getBlockByNumber` for 37 503 978. `Unbound` was the wrong word for a gap in
//!   knowledge, and the second test below is the regression pin for the fix; the third
//!   keeps `Unbound` meaning what §27 needs it to mean when the block *is* named and is a
//!   different block.
//!
//! M12-B §9's D4 added a third thing to pin. The receipt decoder used to end its
//! provenance line with the endpoint *kind* — the submission-side word for a class of
//! provider — so a run aimed at a local node still labelled every one of its reads
//! "public". `the_read_line_names_the_endpoint_that_answered…` is that fix, and
//! `the_committed_rows_keep_the_labels_the_runs_actually_wrote` is its pair: the M6 and M7
//! artifacts stay exactly as the runs wrote them, including the submission line, where the
//! class is the very fact §53 asks for. Rewriting either is how evidence ends up
//! describing a configuration that never happened.
//!
//! M12-B §9's D4 added a third thing to pin. The receipt decoder used to end its
//! provenance line with the endpoint *kind* — the submission-side word for a class of
//! provider — so a run aimed at a local node still labelled every one of its reads
//! "public". `the_read_line_names_the_endpoint_that_answered…` is that fix, and
//! `the_committed_rows_keep_the_labels_the_runs_actually_wrote` is its pair: the M6/M7
//! artifacts stay exactly as the runs wrote them, including the submission line where the
//! class is the fact §53 asks for. Rewriting either would be how this evidence ends up
//! claiming a configuration that never happened.
//!
//! No number in this file is typed: the expected sender, nonce, target, hash, gas and
//! prices all come out of the two evidence documents, and the assertions are relations
//! between them.

use std::path::{Path, PathBuf};

use alloy_primitives::{Address, B256, U256};
use serde_json::Value;

use evm_execution::{
    bind, parse_receipt, EndpointKind, ExpectedTransaction, Receipt, ReceiptPolicy, ReceiptStatus,
    ReceiptTracker, TrackedReceipt,
};

/// The session §35 asked for, in submit mode.
const SESSION: &str = "data/evidence/m6/validation/validate-91342-1790849090542";

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/execution sits two levels below the workspace root")
        .to_path_buf()
}

fn json_file(relative: &str) -> Value {
    let path = workspace_root().join(relative);
    let text = std::fs::read_to_string(&path).unwrap_or_else(|error| {
        panic!(
            "{}: {error} — this file is the evidence §41 asks the milestone to leave behind, \
             and it is committed rather than regenerated",
            path.display()
        )
    });
    serde_json::from_str(&text).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

/// The one line a session's evidence file holds for this attempt.
fn single_row(file: &str) -> Value {
    let path = workspace_root().join(file);
    let text = std::fs::read_to_string(&path).unwrap_or_else(|error| {
        panic!("{}: {error}", path.display());
    });
    let mut lines = text.lines().filter(|line| !line.trim().is_empty());
    let row = lines
        .next()
        .unwrap_or_else(|| panic!("{file}: the session wrote no row"));
    assert!(
        lines.next().is_none(),
        "{file}: this evidence is about one transaction, so a second row would mean the \
         session ran the ladder twice"
    );
    serde_json::from_str(row).unwrap_or_else(|error| panic!("{file}: {error}"))
}

/// Every row of a committed JSONL evidence file.
fn json_rows(file: &str) -> Vec<Value> {
    let path = workspace_root().join(file);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).unwrap_or_else(|error| panic!("{file}: {error}")))
        .collect()
}

/// A committed evidence file as text, for a search that is about the record rather than
/// about a decoded field.
fn text_file(file: &str) -> String {
    let path = workspace_root().join(file);
    std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

/// The two documents, in the shapes this file reasons about.
struct Evidence {
    /// What the lane wrote when it signed: the hash it computed locally, the sender the
    /// signature recovered to, and the fields the intent carried.
    signed: Value,
    /// What the endpoint answers now, about the same transaction.
    node: Value,
}

fn evidence() -> Evidence {
    Evidence {
        signed: single_row(&format!("{SESSION}/signed-transactions.jsonl")),
        node: json_file("data/evidence/m6/validation/node-answers-37503978.json"),
    }
}

/// The chain id the endpoint named in the same capture.
fn chain_id(ev: &Evidence) -> u64 {
    ev.node["chain_id"]["result"]
        .as_str()
        .and_then(|text| u64::from_str_radix(text.trim_start_matches("0x"), 16).ok())
        .expect("the evidence file records the chain id the endpoint named")
}

/// The endpoint the evidence file says these answers came from. This is the one fact §9's
/// D4 asks a read line to carry: which node answered.
fn endpoint_url(ev: &Evidence) -> &str {
    ev.node["endpoint"]
        .as_str()
        .expect("the evidence file names the endpoint its answers were captured from")
}

/// The receipt, decoded the way the live submitter decodes it.
fn receipt(ev: &Evidence) -> Receipt {
    receipt_read_over(ev, endpoint_url(ev))
}

/// The same node answer decoded as if it had been read over `url`, so a test can ask what
/// the provenance line tracks.
fn receipt_read_over(ev: &Evidence, url: &str) -> Receipt {
    let raw = &ev.node["attempts"]["eth_getTransactionReceipt"]["result"];
    assert!(
        !raw.is_null(),
        "the evidence file has to carry the node's answer, not a note about it"
    );
    parse_receipt(chain_id(ev), url, raw, "the §35 validation transaction")
        .expect("the node's answer decodes through the same path the lane uses")
}

/// §27's expected side, built from the lane's own §52 row rather than restated here.
fn expected(ev: &Evidence) -> ExpectedTransaction {
    ExpectedTransaction {
        transaction_hash: ev.signed["signed_tx_hash"]
            .as_str()
            .and_then(|text| text.parse::<B256>().ok())
            .expect("the signed row carries the hash computed over the bytes"),
        sender: ev.signed["recovered_sender"]
            .as_str()
            .and_then(|text| text.parse::<Address>().ok())
            .expect("the signed row carries the sender the signature recovered to"),
        target: Some(
            ev.signed["to"]
                .as_str()
                .and_then(|text| text.parse::<Address>().ok())
                .expect("the signed row carries the target"),
        ),
        nonce: ev.signed["nonce"]
            .as_u64()
            .expect("the signed row carries a nonce"),
        chain_id: ev.signed["chain_id"].as_u64().expect("and its chain id"),
    }
}

fn tracker() -> ReceiptTracker {
    ReceiptTracker::new(ReceiptPolicy {
        attempts: 3,
        between_attempts: std::time::Duration::from_millis(1),
    })
}

/// Poll the tracker with a block reader built from one answer, so each test differs only
/// in what the chain said about the block the receipt names.
async fn track_with(ev: &Evidence, block_answer: Result<Option<B256>, String>) -> TrackedReceipt {
    let mine = receipt(ev);
    let number = mine.block_number;
    tracker()
        .track(
            &expected(ev),
            || {
                let r = mine.clone();
                async move { Ok(Some(r)) }
            },
            |asked| {
                let answer = block_answer.clone();
                async move {
                    assert_eq!(
                        asked, number,
                        "the tracker asks for the height the receipt named, not for the head"
                    );
                    answer
                }
            },
        )
        .await
}

#[test]
fn the_nodes_receipt_binds_to_the_bytes_this_lane_signed() {
    let ev = evidence();
    let mine = receipt(&ev);
    let want = expected(&ev);

    bind(&mine, &want).expect("the receipt is for the transaction this lane produced");
    assert!(
        mine.success,
        "a plain value transfer with no calldata either pays or says it did not; this one \
         said it did, and the milestone does not get to read a failure as a success (§P): {:?}",
        mine.outcome()
    );
    assert_eq!(
        mine.gas_used,
        ev.signed["gas_limit"].as_u64().expect("gas_limit"),
        "the chain charged exactly the allowance the intent carried — 21 000 is a value \
         transfer's own cost, so a larger figure here would mean the transaction was not \
         the one that was built"
    );
    assert_eq!(
        mine.from, want.sender,
        "§27's third leg on real data: the receipt's sender is the account the signature \
         recovers to"
    );
    assert_eq!(
        mine.to, want.target,
        "and it paid the account the plan named, which for a validation transaction is the \
         sender itself"
    );
    // The block the receipt names is the block the evidence file read back, and the hash
    // the endpoint gave for that number is the hash the receipt carries. That equality is
    // the whole of §27's second leg, and it is why the live run's refusal was about a
    // readable block rather than about a false receipt.
    let header = &ev.node["attempts"]["eth_getBlockByNumber"]["result"];
    assert_eq!(
        mine.block_number,
        header["number"]
            .as_str()
            .and_then(|text| u64::from_str_radix(text.trim_start_matches("0x"), 16).ok())
            .expect("the block read answers for a number"),
        "the receipt and the block read are about the same height"
    );
    assert_eq!(
        format!("{:#x}", mine.block_hash),
        header["hash"].as_str().expect("and a hash"),
        "the endpoint's block at that height is the block the receipt is in"
    );
}

#[tokio::test]
async fn a_transaction_the_chain_names_a_block_for_is_included() {
    // §41's `Included`, reached the only way it can be: the receipt decodes, binds, and
    // the endpoint confirms the block it names.
    let ev = evidence();
    let hash = ev.node["attempts"]["eth_getBlockByNumber"]["result"]["hash"]
        .as_str()
        .and_then(|text| text.parse::<B256>().ok())
        .expect("the evidence file carries the block hash the endpoint gave");
    let outcome = track_with(&ev, Ok(Some(hash))).await;
    assert_eq!(outcome.status(), ReceiptStatus::Included);
    match outcome {
        TrackedReceipt::Included(receipt) => assert_eq!(
            receipt.l2_cost_wei(),
            Some(U256::from(
                ev.node["cost"]["l2_gas_bill_wei"]
                    .as_str()
                    .expect("the cost section is part of the evidence")
                    .parse::<u128>()
                    .expect("a wei figure")
            )),
            "the bill this file reports is `gas_used × effective_gas_price`, computed by the \
             same function the live record used"
        ),
        other => panic!("expected the binding to succeed: {other:?}"),
    }
}

#[tokio::test]
async fn a_block_the_endpoint_cannot_name_yet_times_out_instead_of_failing_the_transaction() {
    // The live run of this transaction answered exactly this way — receipt first, block a
    // beat later — and the ladder reported `failed`. The receipt was never the problem:
    // what was missing was one more poll. So the answer here is `Timeout`, §25's
    // "unknown", and the words say which read came up short.
    let ev = evidence();
    let outcome = track_with(&ev, Ok(None)).await;
    match outcome {
        TrackedReceipt::Pending { attempts, .. } => {
            assert_eq!(attempts, 3, "every attempt asked, none was skipped");
        }
        other => panic!(
            "an unreadable block is absence of knowledge, not a receipt that failed to \
             bind: {other:?}"
        ),
    }
    assert_ne!(
        outcome.status(),
        ReceiptStatus::NotFound,
        "`NotFound` is a claim that the transaction will never land, and this endpoint \
         cannot support it: {:?}",
        outcome.status()
    );
}

#[tokio::test]
async fn a_block_the_endpoint_names_differently_is_still_unbound() {
    // The fix above must not have swallowed §27's real finding. If the endpoint says the
    // block at that height is a *different* block, the receipt is evidence about a fork,
    // and the attempt ends there with both hashes named.
    let ev = evidence();
    let outcome = track_with(&ev, Ok(Some(B256::ZERO))).await;
    match outcome {
        TrackedReceipt::Unbound { reason, .. } => assert!(
            reason.contains("the receipt claims"),
            "the refusal quotes the two hashes it compared: {reason}"
        ),
        other => panic!("a receipt in a block the chain denies must not bind: {other:?}"),
    }
}

#[test]
fn what_the_chain_charged_is_what_the_wallet_lost() {
    // §35's transaction is allowed to cost money, and §58's evidence has to say how much
    // in figures a reader can check against the chain rather than against this file. The
    // identity below is the check: the wallet's balance fell by exactly the L2 gas bill
    // plus the L1 data fee the endpoint charged, and the L2 bill is the ceiling's
    // understudy — the signed maximum was `gas_limit × max_fee`, which the run paid less
    // than because the base fee had room left in it.
    let ev = evidence();
    let cost = &ev.node["cost"];
    let wei = |key: &str| -> U256 {
        U256::from(
            cost[key]
                .as_str()
                .unwrap_or_else(|| panic!("the cost section has no `{key}`"))
                .parse::<u128>()
                .expect("a wei figure"),
        )
    };
    let before = wei("balance_before_wei");
    let after = wei("balance_after_wei");
    let l2 = wei("l2_gas_bill_wei");
    let l1 = wei("l1_data_fee_wei");
    let total = wei("total_wei");

    assert_eq!(before - after, total, "the wallet's own numbers");
    assert_eq!(l2 + l1, total, "and the two bills the endpoint itemised");
    assert_eq!(
        l2,
        receipt(&ev).l2_cost_wei().expect("a bound receipt"),
        "the bill this file reports is the bill the receipt implies"
    );
    let ceiling = receipt(&ev).gas_used as u128
        * ev.signed["fee"]
            .as_str()
            .and_then(|fee| fee.split("maxFeePerGas=").nth(1))
            .and_then(|tail| tail.split(',').next())
            .and_then(|text| text.parse::<u128>().ok())
            .expect("the signed row states the fee ceiling it was built with");
    assert!(
        l2 < U256::from(ceiling),
        "the ceiling is a promise about the worst case, so the bill has to sit under it: \
         paid {l2}, ceiling {ceiling}"
    );
    // §42's boundary, restated where the money is: this is the cost of verifying the
    // pipeline, and it is not a trade. Nothing came back to the wallet.
    assert_eq!(
        ev.signed["value_wei"].as_str(),
        Some("0x0"),
        "a validation transaction transfers nothing, so the whole of the wallet's loss is fee"
    );
}

/// §9's D4: what a receipt read says about its endpoint.
///
/// The decoder used to end the provenance line with the adapter's endpoint *kind* — the
/// submission-side word for a class of provider. One adapter answers both `eth_sendRawTransaction`
/// and `eth_getTransactionReceipt`, so aiming the lane at a self-hosted node left every read
/// in the evidence still claiming "public", which is a label nothing in the read supported.
/// The line now carries the digest of the URL the read went over: it names which endpoint
/// answered and makes no claim about who runs it.
#[test]
fn the_read_line_names_the_endpoint_that_answered_rather_than_a_class_of_endpoint() {
    let ev = evidence();
    // A second URL as a control input rather than as a claim about a node — this repository
    // has never connected to it, and the test only asks that naming it changes the line.
    const ELSEWHERE: &str = "http://127.0.0.1:8545";
    let here = receipt(&ev);
    let there = receipt_read_over(&ev, ELSEWHERE);

    for (url, line) in [
        (endpoint_url(&ev), &here.provenance),
        (ELSEWHERE, &there.provenance),
    ] {
        assert!(
            line.contains(&evm_chain::endpoint_id(url)),
            "the provenance has to carry the digest of the endpoint the read went over: {line}"
        );
        // The three words the submission side uses for a class of provider, plus the one a
        // later overcorrection would reach for. None of them is a fact a read can establish.
        for class in [
            EndpointKind::PublicHttpRpc.name(),
            EndpointKind::FlashblocksHttpRpc.name(),
            EndpointKind::Recorded.name(),
            "local",
        ] {
            assert!(
                !line.contains(class),
                "`{class}` is a claim about who runs the endpoint and what it is for, and a \
                 receipt read may not make it: {line}"
            );
        }
    }

    // A digest that never moved would be the same defect in a new costume.
    assert_ne!(
        here.provenance, there.provenance,
        "two different endpoints have to produce two different read lines"
    );
    // …and the endpoint is the only thing the line tracks: the same node answer decodes to
    // the same receipt whichever URL it is filed under.
    assert_eq!(
        there.block_number, here.block_number,
        "naming a different endpoint cannot change what the node said"
    );
    assert_eq!(
        there.l2_cost_wei(),
        here.l2_cost_wei(),
        "and not the bill either"
    );
}

/// The other half of §9: the change stops at the code that writes lines from now on.
///
/// Nothing already committed is relabelled — the M7 route is kept as both halves of the
/// evidence for that, since it is the run whose file carries a submission class *and* the
/// read wording this milestone retired. A run that wanted its evidence to read "all local"
/// would have to edit one of these two, which is exactly what §9 forbids. It is also the
/// positive control for the class-word loop above: the retired wording is still findable in
/// this repository, so its absence from a fresh read line is the fix rather than a word that
/// went missing everywhere.
#[test]
fn the_committed_rows_keep_the_labels_the_runs_actually_wrote() {
    const RETIRED_READ_LINE: &str = "eth_getTransactionReceipt over the configured GIWA RPC \
                                     URL (public_http_rpc)";
    let route = "data/evidence/m7/route-submit/route-91342-37563264-1790908382146";

    let submissions = json_rows(&format!("{route}/submissions.jsonl"));
    assert!(
        !submissions.is_empty(),
        "the committed route has to still carry its submission rows"
    );
    for row in &submissions {
        assert_eq!(
            row["submission_endpoint_type"].as_str(),
            Some(EndpointKind::PublicHttpRpc.name()),
            "§53 asks the submission line which class of endpoint the bytes went through, and \
             the public one is still the fact: {row}"
        );
    }

    let run = text_file(&format!("{route}/route-run.json"));
    assert!(
        run.contains(RETIRED_READ_LINE),
        "the receipt reads in that file are what the run wrote, and rewriting a committed \
         artifact to satisfy a grep would make the evidence describe a configuration that \
         never happened"
    );
}
