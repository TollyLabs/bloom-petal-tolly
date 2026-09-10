//! This crate's only boundary to the Bloom host.
//!
//! Every host call goes through one of these functions. A release build
//! forwards straight to the pinned SDK; a test build dispatches to the
//! recording fake host in `fake_host`, which lets a test drive a whole write
//! flow (quote, allowance, pre-flight, stage) and assert on the exact requests
//! that left the Petal and the exact durable record it wrote.
//!
//! Only the public `state` store namespace is ever written: this Petal holds
//! no secret. `store_put` therefore has no `secret` parameter at all.

#[cfg(not(test))]
mod real {
    use petal::{
        EvmTransaction, HttpRequest, HttpResponse, OutboxInspection, SdkError, StagedTransaction,
    };

    pub fn http_fetch(request: &HttpRequest, max_bytes: usize) -> Result<HttpResponse, SdkError> {
        petal::sdk::http_fetch(request, max_bytes)
    }
    pub fn chain_read(chain: &str, method: &str, params_json: &str) -> Result<String, SdkError> {
        petal::sdk::chain_read(chain, method, params_json)
    }
    pub fn tx_stage(request: &EvmTransaction) -> Result<StagedTransaction, SdkError> {
        petal::sdk::tx_stage(request)
    }
    pub fn tx_inspect(
        wallet: &str,
        chain: &str,
        outbox_id: &str,
    ) -> Result<OutboxInspection, SdkError> {
        petal::sdk::tx_inspect(wallet, chain, outbox_id)
    }
    pub fn store_get(key: &str, max_bytes: usize) -> Result<Vec<u8>, SdkError> {
        petal::sdk::store_get(key, max_bytes)
    }
    pub fn store_put(key: &str, value: &[u8]) -> Result<(), SdkError> {
        petal::sdk::store_put(key, value, false)
    }
    pub fn store_put_new(key: &str, value: &[u8]) -> Result<(), SdkError> {
        petal::sdk::store_put_new(key, value, false)
    }
    pub fn store_list(prefix: &str, max_bytes: usize) -> Result<Vec<String>, SdkError> {
        petal::sdk::store_list(prefix, max_bytes)
    }
    pub fn vfs_read(path: &str, max_bytes: usize) -> Result<Vec<u8>, SdkError> {
        petal::sdk::vfs_read(path, max_bytes)
    }
    pub fn now_ms() -> u64 {
        petal::sdk::now_ms()
    }
    pub fn random_bytes(len: usize) -> Result<Vec<u8>, SdkError> {
        petal::sdk::random_bytes(len)
    }
    pub fn runtime_setting(key: &str) -> Result<Option<String>, SdkError> {
        petal::sdk::runtime_setting(key)
    }
}

#[cfg(not(test))]
pub use real::{
    chain_read, http_fetch, now_ms, random_bytes, runtime_setting, store_get, store_list,
    store_put, store_put_new, tx_inspect, tx_stage, vfs_read,
};

#[cfg(test)]
pub use crate::fake_host::{
    chain_read, http_fetch, now_ms, random_bytes, runtime_setting, store_get, store_list,
    store_put, store_put_new, tx_inspect, tx_stage, vfs_read,
};
