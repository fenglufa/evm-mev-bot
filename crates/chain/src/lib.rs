pub mod adapter;
pub mod error;
pub mod head;
pub mod recorded;
pub mod rpc;
pub mod rpc_trace;
pub mod types;
pub mod ws;

pub use adapter::ChainAdapter;
pub use error::{ChainError, Result};
pub use head::HeadReader;
pub use head::WsHeadReader;
pub use recorded::RecordedChainAdapter;
pub use rpc::{chain_block_from_value, chain_log_from_value, HttpChainAdapter};
pub use rpc_trace::{
    describe_call, normalize_address, RpcAttempt, RpcCallContext, RpcCallDescription, RpcCallEvent,
    RpcCallLabel, RpcTraceSink, RpcTraceSource, TracedCall, BREAKDOWN_UNAVAILABLE,
    CLASS_DECODE_FAILED, CLASS_HTTP_STATUS, CLASS_NODE_REJECTED, CLASS_NON_JSON, CLASS_OK,
    CLASS_SEND_FAILED, CONTEXT_AMBIGUOUS_CONCURRENT_CALLS, CONTEXT_AMBIGUOUS_RESTAMPED_MID_CALL,
    CONTEXT_NOTES, CONTEXT_NOT_STAMPED, DEDUP_KEY_PARAMS_UNREADABLE,
    DEDUP_KEY_UNAVAILABLE_FOR_METHOD, MAX_EVENTS_PER_SINK, RPC_TRACE_SCHEMA,
    RPC_TRACE_SCHEMA_READABLE,
};
pub use types::{
    BlockContext, BlockData, CallRequest, ChainBlock, ChainLog, ChainReceipt, ChainTransaction,
    LogFilter,
};
pub use ws::{ConnStatus, SubscribeOutcome, SubscriptionAttempt, WsOptions, WsRpcClient};
