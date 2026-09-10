//! A recording fake Bloom host, used only by this crate's tests.
//!
//! The `host` seam dispatches here under `cfg(test)`, so a test can drive a
//! whole write flow (detail fetch, quotes, allowance, pre-flight, stage) and
//! then assert on the exact host calls the route made and the exact durable
//! record it left behind. Nothing here is compiled into a route component.
//!
//! HTTP replies are keyed by URL. Chain replies are matched by
//! `(method, to, calldata prefix)`; the longest matching prefix wins, replies
//! are served in order and the last one repeats.

use alloy_primitives::{Address, U256};
use petal::{
    EvmTransaction, HostStatus, HttpRequest, HttpResponse, OutboxInspection, SdkError,
    StagedTransaction,
};
use serde_json::Value;
use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};

#[derive(Clone, Debug)]
pub struct ChainCall {
    pub method: String,
    pub params: Value,
}

impl ChainCall {
    pub fn to(&self) -> Option<String> {
        self.params[0]["to"].as_str().map(str::to_owned)
    }
    pub fn from(&self) -> Option<String> {
        self.params[0]["from"].as_str().map(str::to_owned)
    }
    pub fn data(&self) -> Option<String> {
        self.params[0]["data"].as_str().map(str::to_owned)
    }
}

struct ChainRule {
    method: String,
    to: Option<String>,
    data_prefix: String,
    replies: Vec<Result<String, SdkError>>,
    served: usize,
}

#[derive(Default)]
pub struct FakeHost {
    pub now_ms: u64,
    pub http_calls: Vec<HttpRequest>,
    http_replies: BTreeMap<String, Vec<(u16, Vec<u8>)>>,
    http_served: BTreeMap<String, usize>,
    pub chain_calls: Vec<ChainCall>,
    chain_rules: Vec<ChainRule>,
    state: BTreeMap<String, Vec<u8>>,
    pub staged: Vec<EvmTransaction>,
    /// Every `tx_inspect` the routes made, by outbox id.
    pub inspect_calls: Vec<String>,
    stage_failures: VecDeque<SdkError>,
    outbox: BTreeMap<String, OutboxInspection>,
    vfs: BTreeMap<String, Vec<u8>>,
    settings: BTreeMap<String, String>,
    pub random_fill: u8,
    puts: usize,
    /// Make every store write after this many successful writes fail, to model
    /// a process that dies between a host effect and its durable record.
    pub fail_store_after: Option<usize>,
}

impl FakeHost {
    pub fn new(now_ms: u64) -> Self {
        Self {
            now_ms,
            random_fill: 0x11,
            ..Self::default()
        }
    }

    // ---- scripting ----

    pub fn reply_http(&mut self, url: &str, status: u16, body: &Value) -> &mut Self {
        self.http_replies
            .entry(url.to_owned())
            .or_default()
            .push((status, serde_json::to_vec(body).expect("reply serializes")));
        self
    }

    pub fn reply_http_bytes(&mut self, url: &str, status: u16, body: &[u8]) -> &mut Self {
        self.http_replies
            .entry(url.to_owned())
            .or_default()
            .push((status, body.to_vec()));
        self
    }

    pub fn reply_chain(
        &mut self,
        method: &str,
        to: Option<Address>,
        data_prefix: &str,
        reply: Result<String, SdkError>,
    ) -> &mut Self {
        let to = to.map(|a| format!("{a:?}"));
        let prefix = data_prefix
            .strip_prefix("0x")
            .unwrap_or(data_prefix)
            .to_ascii_lowercase();
        if let Some(rule) = self
            .chain_rules
            .iter_mut()
            .find(|r| r.method == method && r.to == to && r.data_prefix == prefix)
        {
            rule.replies.push(reply);
        } else {
            self.chain_rules.push(ChainRule {
                method: method.to_owned(),
                to,
                data_prefix: prefix,
                replies: vec![reply],
                served: 0,
            });
        }
        self
    }

    /// Replace every queued reply for a `(method, to, prefix)` rule.
    pub fn replace_chain(
        &mut self,
        method: &str,
        to: Option<Address>,
        data_prefix: &str,
        reply: Result<String, SdkError>,
    ) -> &mut Self {
        let to_key = to.map(|a| format!("{a:?}"));
        let prefix = data_prefix
            .strip_prefix("0x")
            .unwrap_or(data_prefix)
            .to_ascii_lowercase();
        self.chain_rules
            .retain(|r| !(r.method == method && r.to == to_key && r.data_prefix == prefix));
        self.reply_chain(method, to, data_prefix, reply)
    }

    /// Script an `eth_call` result of one 32-byte word.
    pub fn call_u256(&mut self, to: Address, data_prefix: &str, value: U256) -> &mut Self {
        let hex = format!("0x{}", hex::encode(value.to_be_bytes::<32>()));
        self.reply_chain(
            "eth_call",
            Some(to),
            data_prefix,
            Ok(serde_json::to_string(&hex).unwrap()),
        )
    }

    /// Script an `eth_call` result of raw return bytes.
    pub fn call_bytes(&mut self, to: Address, data_prefix: &str, bytes: &[u8]) -> &mut Self {
        let hex = format!("0x{}", hex::encode(bytes));
        self.reply_chain(
            "eth_call",
            Some(to),
            data_prefix,
            Ok(serde_json::to_string(&hex).unwrap()),
        )
    }

    /// Script an `eth_call` failure (a revert, as the daemon reports it).
    pub fn call_error(&mut self, to: Address, data_prefix: &str, message: &str) -> &mut Self {
        self.reply_chain(
            "eth_call",
            Some(to),
            data_prefix,
            Err(SdkError::Message(message.to_owned())),
        )
    }

    /// Script `eth_getBalance` for an address.
    pub fn balance(&mut self, address: Address, wei: U256) -> &mut Self {
        let hex = format!("{wei:#x}");
        self.chain_rules.push(ChainRule {
            method: "eth_getBalance".into(),
            to: Some(format!("{address:?}")),
            data_prefix: String::new(),
            replies: vec![Ok(serde_json::to_string(&hex).unwrap())],
            served: 0,
        });
        self
    }

    pub fn seed_state(&mut self, key: &str, value: &impl serde::Serialize) -> &mut Self {
        self.state.insert(
            key.to_owned(),
            serde_json::to_vec(value).expect("seed serializes"),
        );
        self
    }

    pub fn state_json(&self, key: &str) -> Option<Value> {
        self.state
            .get(key)
            .map(|bytes| serde_json::from_slice(bytes).expect("stored state is JSON"))
    }

    pub fn state_keys(&self) -> Vec<String> {
        self.state.keys().cloned().collect()
    }

    pub fn fail_next_stage(&mut self, error: SdkError) -> &mut Self {
        self.stage_failures.push_back(error);
        self
    }

    pub fn set_outbox(
        &mut self,
        id: &str,
        state: &str,
        tx_hash: Option<&str>,
        receipt: Option<&Value>,
    ) -> &mut Self {
        self.outbox.insert(
            id.to_owned(),
            OutboxInspection {
                outbox_id: id.to_owned(),
                state: state.to_owned(),
                tx_hash: tx_hash.map(str::to_owned),
                receipt_json: receipt.map(|r| r.to_string()),
            },
        );
        self
    }

    pub fn remove_outbox(&mut self, id: &str) -> &mut Self {
        self.outbox.remove(id);
        self
    }

    pub fn seed_vfs(&mut self, path: &str, bytes: &[u8]) -> &mut Self {
        self.vfs.insert(path.to_owned(), bytes.to_vec());
        self
    }

    pub fn set_setting(&mut self, key: &str, value: &str) -> &mut Self {
        self.settings.insert(key.to_owned(), value.to_owned());
        self
    }

    /// Successful store writes so far (`put`, `put_new`, `del`).
    pub fn store_writes(&self) -> usize {
        self.puts
    }

    pub fn eth_calls_to(&self, to: Address) -> Vec<&ChainCall> {
        let to = format!("{to:?}");
        self.chain_calls
            .iter()
            .filter(|c| c.method == "eth_call" && c.to().as_deref() == Some(to.as_str()))
            .collect()
    }

    pub fn chain_methods(&self) -> Vec<&str> {
        self.chain_calls.iter().map(|c| c.method.as_str()).collect()
    }

    /// Critique B1: every chain read this host saw used an allowlisted method
    /// (`eth_call`/`eth_getBalance`) at the `latest` block.
    pub fn assert_chain_calls_allowlisted(&self) {
        assert!(!self.chain_calls.is_empty(), "no chain call recorded");
        for call in &self.chain_calls {
            assert!(
                matches!(call.method.as_str(), "eth_call" | "eth_getBalance"),
                "method {} is not allowlisted",
                call.method
            );
            assert_eq!(
                call.params[1].as_str(),
                Some("latest"),
                "{} must read the latest block: {}",
                call.method,
                call.params
            );
        }
    }

    /// Consume one scripted HTTP reply for `url` (tests that script a
    /// second, different reply for the same URL).
    pub fn fetch_for_test(&mut self, url: &str) -> Result<HttpResponse, SdkError> {
        let request = HttpRequest {
            method: "GET".into(),
            url: url.to_owned(),
            headers: Vec::new(),
            body: Vec::new(),
        };
        let response = self.fetch(&request);
        self.http_calls.pop();
        response
    }

    // ---- behaviour ----

    fn writable(&mut self) -> Result<(), SdkError> {
        if self
            .fail_store_after
            .is_some_and(|limit| self.puts >= limit)
        {
            return Err(SdkError::Host(HostStatus::Backend));
        }
        self.puts += 1;
        Ok(())
    }

    fn fetch(&mut self, request: &HttpRequest) -> Result<HttpResponse, SdkError> {
        self.http_calls.push(request.clone());
        let Some(replies) = self.http_replies.get(&request.url) else {
            return Err(SdkError::Message(format!(
                "fake host: no reply scripted for {}",
                request.url
            )));
        };
        let served = self.http_served.entry(request.url.clone()).or_default();
        let index = (*served).min(replies.len() - 1);
        *served += 1;
        let (status, body) = replies[index].clone();
        Ok(HttpResponse {
            status,
            headers: vec![("content-type".into(), "application/json".into())],
            body,
        })
    }

    /// Serve one chain call (exposed so a test can consume a scripted reply).
    pub fn chain(&mut self, method: &str, params_json: &str) -> Result<String, SdkError> {
        let params: Value = serde_json::from_str(params_json).expect("chain params are JSON");
        let call = ChainCall {
            method: method.to_owned(),
            params: params.clone(),
        };
        let (to, data) = match method {
            "eth_getBalance" => (params[0].as_str().map(str::to_owned), String::new()),
            _ => (
                call.to(),
                call.data()
                    .map(|d| d.trim_start_matches("0x").to_ascii_lowercase())
                    .unwrap_or_default(),
            ),
        };
        self.chain_calls.push(call);
        let mut best: Option<usize> = None;
        for (index, rule) in self.chain_rules.iter().enumerate() {
            if rule.method != method
                || (rule.to.is_some() && rule.to != to)
                || !data.starts_with(&rule.data_prefix)
            {
                continue;
            }
            if best.is_none_or(|b| rule.data_prefix.len() > self.chain_rules[b].data_prefix.len()) {
                best = Some(index);
            }
        }
        let Some(index) = best else {
            return Err(SdkError::Message(format!(
                "fake host: no chain reply scripted for {method} to={to:?} data={}",
                &data[..data.len().min(10)]
            )));
        };
        let rule = &mut self.chain_rules[index];
        let reply = rule.replies[rule.served.min(rule.replies.len() - 1)].clone();
        rule.served += 1;
        reply
    }
}

thread_local! {
    static HOST: RefCell<Option<FakeHost>> = const { RefCell::new(None) };
}

/// Install a fake host for the current test thread.
pub fn install(host: FakeHost) {
    HOST.with(|slot| *slot.borrow_mut() = Some(host));
}

/// Borrow the installed fake host.
pub fn with<T>(body: impl FnOnce(&mut FakeHost) -> T) -> T {
    HOST.with(|slot| {
        let mut slot = slot.borrow_mut();
        body(
            slot.as_mut()
                .expect("install a FakeHost before exercising a route"),
        )
    })
}

pub fn http_fetch(request: &HttpRequest, max_bytes: usize) -> Result<HttpResponse, SdkError> {
    with(|host| {
        let response = host.fetch(request)?;
        if response.body.len() > max_bytes {
            return Err(SdkError::Host(HostStatus::BufferTooSmall {
                needed: response.body.len(),
            }));
        }
        Ok(response)
    })
}

pub fn chain_read(chain: &str, method: &str, params_json: &str) -> Result<String, SdkError> {
    assert_eq!(
        chain,
        crate::constants::CHAIN,
        "every chain read names the arc chain"
    );
    with(|host| host.chain(method, params_json))
}

pub fn tx_stage(request: &EvmTransaction) -> Result<StagedTransaction, SdkError> {
    petal::validate_wallet_id(&request.wallet).map_err(SdkError::Message)?;
    with(|host| {
        if let Some(error) = host.stage_failures.pop_front() {
            return Err(error);
        }
        host.staged.push(request.clone());
        let id = format!("ob-{}", host.staged.len());
        host.outbox.insert(
            id.clone(),
            OutboxInspection {
                outbox_id: id.clone(),
                state: "pending".into(),
                tx_hash: None,
                receipt_json: None,
            },
        );
        Ok(StagedTransaction {
            outbox_id: id.clone(),
            plan_md: format!(
                "# Staged transaction {id}\n\nto: {}\nvalue: {} wei\n",
                request.to, request.value_wei
            ),
            approval: None,
        })
    })
}

pub fn tx_inspect(
    wallet: &str,
    chain: &str,
    outbox_id: &str,
) -> Result<OutboxInspection, SdkError> {
    petal::validate_wallet_id(wallet).map_err(SdkError::Message)?;
    assert_eq!(chain, crate::constants::CHAIN);
    with(|host| {
        host.inspect_calls.push(outbox_id.to_owned());
        host.outbox
            .get(outbox_id)
            .cloned()
            .ok_or(SdkError::Host(HostStatus::NotFound))
    })
}

pub fn store_get(key: &str, max_bytes: usize) -> Result<Vec<u8>, SdkError> {
    with(|host| {
        let bytes = host
            .state
            .get(key)
            .cloned()
            .ok_or(SdkError::Host(HostStatus::NotFound))?;
        if bytes.len() > max_bytes {
            return Err(SdkError::Host(HostStatus::BufferTooSmall {
                needed: bytes.len(),
            }));
        }
        Ok(bytes)
    })
}

pub fn store_put(key: &str, value: &[u8]) -> Result<(), SdkError> {
    with(|host| {
        host.writable()?;
        host.state.insert(key.to_owned(), value.to_vec());
        Ok(())
    })
}

pub fn store_put_new(key: &str, value: &[u8]) -> Result<(), SdkError> {
    with(|host| {
        host.writable()?;
        if host.state.contains_key(key) {
            // The daemon's wording (`vm.rs` component_store_put_new test):
            // the SDK surfaces it as a plain message, not a status.
            return Err(SdkError::Message(format!(
                "store put_new: key {key} already exists"
            )));
        }
        host.state.insert(key.to_owned(), value.to_vec());
        Ok(())
    })
}

pub fn store_del(key: &str) -> Result<(), SdkError> {
    with(|host| {
        host.writable()?;
        host.state
            .remove(key)
            .map(|_| ())
            .ok_or(SdkError::Host(HostStatus::NotFound))
    })
}

pub fn store_list(prefix: &str, max_bytes: usize) -> Result<Vec<String>, SdkError> {
    with(|host| {
        let keys: Vec<String> = host
            .state
            .keys()
            .filter(|key| key.starts_with(prefix))
            .cloned()
            .collect();
        let size: usize = keys.iter().map(String::len).sum();
        if size > max_bytes {
            return Err(SdkError::Host(HostStatus::BufferTooSmall { needed: size }));
        }
        Ok(keys)
    })
}

pub fn vfs_read(path: &str, max_bytes: usize) -> Result<Vec<u8>, SdkError> {
    with(|host| {
        let bytes = host
            .vfs
            .get(path)
            .cloned()
            .ok_or(SdkError::Host(HostStatus::NotFound))?;
        if bytes.len() > max_bytes {
            return Err(SdkError::Host(HostStatus::BufferTooSmall {
                needed: bytes.len(),
            }));
        }
        Ok(bytes)
    })
}

pub fn now_ms() -> u64 {
    with(|host| host.now_ms)
}

pub fn random_bytes(len: usize) -> Result<Vec<u8>, SdkError> {
    with(|host| Ok(vec![host.random_fill; len]))
}

pub fn runtime_setting(key: &str) -> Result<Option<String>, SdkError> {
    with(|host| Ok(host.settings.get(key).cloned()))
}
