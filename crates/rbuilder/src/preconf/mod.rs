use std::collections::{HashMap};
use std::net::TcpStream;
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use alloy_primitives::{B256, U256};
use alloy_rlp::Decodable;
use jsonrpsee::core::Serialize;
use reqwest::StatusCode;
use reth_primitives::{TransactionSigned, TransactionSignedEcRecovered};
use serde::Deserialize;
use thiserror::Error;
use tokio::sync::mpsc::Sender;
use tracing::{error, info, debug};
use url::Url;
use uuid::{Uuid};
use time::{OffsetDateTime};
use tungstenite::{connect, WebSocket};
use tungstenite::stream::MaybeTlsStream;
use crate::live_builder::base_config::BaseConfig;
use crate::primitives::{Bundle, BundleReplacementData, BundleReplacementKey, Metadata, Order, TransactionSignedEcRecoveredWithBlobs};


#[derive(Debug, Clone)]
pub struct PreconfConfig {
    pub preconf_api_url: Option<String>,
    pub preconf_ws_url: Option<String>,
}

impl PreconfConfig {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        preconf_api_url: Option<String>,
        preconf_ws_url: Option<String>,
    ) -> Self {
        Self {
            preconf_api_url,
            preconf_ws_url,
        }
    }
    pub fn from_config(config: &BaseConfig) -> Self {
        PreconfConfig {
            preconf_api_url: config.preconf_api_url.clone(),
            preconf_ws_url: config.preconf_ws_url.clone(),
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct ApiResponse {
    success: bool,
    data: ApiData,
}

impl std::fmt::Display for ApiResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ApiResponse {{ success: {} }}", self.success)
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
enum ApiData {
    PreconfMarkets(PreconfMarkets),
    PreconfBundles(PreconfBundles),
}

#[derive(Debug, Serialize, Deserialize)]
struct PreconfMarkets {
    markets: Vec<PreconfMarket>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PreconfMarket {
    // market_id: u64,
    slot: u64,
    // proposer_account_id: u64,
    // instrument_id: String,
    // name: String,
    // quantity_step: String,
    // min_quantity: String,
    // max_quantity: String,
    // price_step: String,
    // min_price: String,
    // max_price: String,
    // collateral_per_gas: String,
    // best_bid: String,
    // best_ask: String,
    // direction: bool,
    // price: String,
    // mid_price: String,
    // status: u8,
    // maturity_time: u64,
    trx_submit_time: u64,
    // block_time: u64,
    // finality_time: u64,
    // update_date: u64,
    // total_gas: u64,
}

#[derive(Debug, Serialize, Deserialize)]
struct PreconfBundles {
    bundles: Vec<PreconfBundle>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreconfBundle {
    txs: Vec<PreconfTx>,
    uuid: String,
    average_bid_price: f64,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreconfTx {
    tx: String,
    can_revert: bool,
    // create_date: u64,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct PreconfInfo {
    pub block_number: u64,
    pub slot: u64,
    pub timestamp: Option<u64>,
}

#[derive(Error, Debug, Clone, Serialize, Deserialize)]
pub struct PreconfErrorResponse {
    code: Option<u64>,
    message: String,
}

impl std::fmt::Display for PreconfErrorResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Preconf error: (code: {}, message: {})",
            self.code.unwrap_or_default(),
            self.message
        )
    }
}

#[derive(Error, Debug)]
pub enum PreconfError {
    #[error("Request error: {0}")]
    RequestError(#[from] reqwest::Error),
    #[error("Header error")]
    InvalidHeader,
    #[error("Preconf error: {0}")]
    PreconfError(#[from] PreconfErrorResponse),
    #[error("Unknown preconf response, status: {0}, body: {1}")]
    UnknownPreconfError(StatusCode, String),
    #[error("Too many requests")]
    TooManyRequests,
    #[error("Connection error")]
    ConnectionError,
    #[error("Internal error")]
    InternalError,
    #[error("Preconf order conversion error: {0}")]
    PreconfConvertError(String),
    #[error("Sender Error")]
    SenderError,
}

#[derive(Debug)]
pub struct PreconfClient {
    pub ws: Option<PreconfWsClient>,
    pub api: Option<PreconfApiClient>,
    fallback_enabled: bool,
}

impl PreconfClient {
    pub fn new(
        ws_url: Option<String>,
        api_url: Option<String>,
        preconf_sender: Sender<Order>,
    ) -> Self {
        let mut ws = None;
        let mut api = None;
        let mut fallback_enabled = false;
        // Create a shared market_info HashMap
        let order_sender = Arc::new(preconf_sender);
        let market_info = Arc::new(Mutex::new(HashMap::new()));
        if ws_url.is_some() {
            let url = ws_url.unwrap();
            let (socket, response) = connect(url.as_str()).expect("Can't connect preconf ws server");
            if !response.status().is_success() {
                fallback_enabled = true;
            } else {
                ws = Some(PreconfWsClient {
                    ws_url: Url::parse(url.as_str()).unwrap(),
                    socket,
                    preconf_sender: Arc::clone(&order_sender),
                    market_info: Arc::clone(&market_info),
                })
            }
        } else {
            fallback_enabled = true;
        }
        if api_url.is_some() {
            api = Some(PreconfApiClient {
                api_url: Url::parse(api_url.unwrap().as_str()).unwrap(),
                client: reqwest::Client::new(),
                preconf_sender: Arc::clone(&order_sender),
                market_info: Arc::clone(&market_info),
            })
        }
        Self {
            ws,
            api,
            fallback_enabled,
        }
    }

    pub fn set_fallback_enabled(&mut self, enabled: bool) {
        self.fallback_enabled = enabled;
    }

    pub fn get_fallback_enabled(&self) -> bool {
        self.fallback_enabled
    }
}

#[derive(Debug)]
pub struct PreconfWsClient {
    ws_url: Url,
    socket: WebSocket<MaybeTlsStream<TcpStream>>,
    preconf_sender: Arc<Sender<Order>>,
    market_info: Arc<Mutex<HashMap<u64, u64>>>, // slot as key, utc timestamp as value
}

#[derive(Debug)]
pub struct PreconfApiClient {
    api_url: Url,
    client: reqwest::Client,
    preconf_sender: Arc<Sender<Order>>,
    market_info: Arc<Mutex<HashMap<u64, u64>>>, // slot as key, utc timestamp as value
}

impl PreconfApiClient {
    fn clean_market_info(&mut self, slot: u64) {
        if self.market_info.lock().unwrap().len() == 0 {
            return;
        }
        let til = slot - 1;
        for key in 1..=til {
            self.market_info.lock().unwrap().remove(&key);
        }
    }

    pub async fn get_inclusion_preconf_market_expiry(&mut self, slot: u64) -> Option<OffsetDateTime> {
        debug!("get market expiry on slot={}", slot);
        // clean market info
        self.clean_market_info(slot);
        // Try to get the market info from the map or load it if it's not present.
        let market_expiry;
        if !self.market_info.lock().unwrap().contains_key(&slot) {
            market_expiry = self.get_inclusion_preconf_market_info(slot).await;
        } else {
            market_expiry = self.market_info.lock().unwrap().get(&slot).cloned();
        }
        if market_expiry.is_none() {
            return None;
        }
        let timestamp_ns = market_expiry.unwrap();
        let datetime = OffsetDateTime::from_unix_timestamp_nanos(timestamp_ns as i128).unwrap();
        Some(datetime)
    }

    async fn get_inclusion_preconf_market_info(&mut self, slot: u64) -> Option<u64> {
        debug!("get market info from preconf server");
        let url = format!("{}api/p/inclusion_preconf/markets", self.api_url);
        let mut market_expiry: Option<u64> = None;
        match self.client.get(&url).send().await {
            Ok(response) => {
                match response.json::<ApiResponse>().await {
                    Ok(resp) => {
                        if !resp.success {
                            error!("Failed to fetch preconf requests from server: {}", resp);
                            return None;
                        } else {
                            if let ApiData::PreconfMarkets(response) = resp.data {
                                for market in response.markets {
                                    let market_slot = market.slot;
                                    let submission_cutoff_time = market.trx_submit_time;
                                    if !self.market_info.lock().unwrap().contains_key(&market_slot) {
                                        self.market_info.lock().unwrap().insert(market_slot, submission_cutoff_time);
                                    }
                                    if slot == market_slot {
                                        market_expiry = Some(submission_cutoff_time);
                                    }
                                }
                            }
                        }
                    }
                    Err(err) => {
                        error!("Failed to parse JSON response: {}", err);
                    }
                }
            }
            Err(err) => {
                error!("Cannot fetch preconf requests from server: {}", err);
            }
        }
        market_expiry
    }

    pub async fn fetch_inclusion_preconfs(&self, slot: u64, block: u64, timestamp: u64) {
        info!("fetch preconfs on slot={}, block={}", slot, block);
        let url = format!("{}api/p/bundles?slot={}", self.api_url, slot);
        match self.client.get(&url).send().await {
            Ok(response) => {
                match response.json::<ApiResponse>().await {
                    Ok(resp) => {
                        if !resp.success {
                            error!("Failed to fetch inclusion preconf from server: {}", resp);
                        } else {
                            if let ApiData::PreconfBundles(response) = resp.data {
                                for bundle in response.bundles {
                                    if bundle.txs.is_empty() {
                                        debug!("received preconf transactions is empty");
                                        return;
                                    }
                                    match self.generate_order_from_preconf(block, timestamp, bundle) {
                                        Ok(preconf_order) => {
                                            if self.preconf_sender.send(preconf_order).await.is_err() {
                                                error!("receiver closed");
                                            }
                                        }
                                        Err(err) => {
                                            error!("Failed to generate order from preconf: {}", err);
                                        }
                                    }
                                }
                            }
                        }
                    }
                    Err(err) => {
                        error!("Failed to fetch preconf request: {}", err);
                        return; // Exit if JSON parsing fails
                    }
                }
            }
            Err(err) => {
                error!("cannot fetch preconf requests from preconf server, {}", err);
            }
        }
    }

    fn generate_order_from_preconf(&self, block: u64, timestamp: u64, bundle: PreconfBundle) -> Result<Order, PreconfError> {
        let mut reverting_tx_hashes: Vec<B256> = vec![];
        let mut signer = None;
        let bundle_uuid = self.string_to_uuid(bundle.uuid.clone())?;

        let trxs: Vec<TransactionSignedEcRecoveredWithBlobs> = bundle
            .txs
            .iter()
            .filter_map(|preconf_tx| {
                let tx_bytes = preconf_tx.tx.clone().into_bytes();
                let mut slice = tx_bytes.as_slice();
                let trx = TransactionSigned::decode(&mut slice).ok()?;
                if preconf_tx.can_revert {
                    reverting_tx_hashes.push(trx.hash);
                }
                signer = trx.recover_signer();
                TransactionSignedEcRecoveredWithBlobs::new_no_blobs(
                    TransactionSignedEcRecovered::from_signed_transaction(trx, signer.unwrap())
                )
            })
            .collect();

        let replacement_data = BundleReplacementData {
            key: BundleReplacementKey::new(bundle_uuid, signer.unwrap()),
            sequence_number: 0,
        };

        let mut metadata: Metadata = Default::default();
        match self.eth_to_wei(bundle.average_bid_price) {
            Ok(average_bid_price) => {
                metadata.avg_bid_price = Some(average_bid_price);
                Ok(Order::Bundle(Bundle {
                    block,
                    min_timestamp: Some(timestamp),
                    max_timestamp: None,
                    txs: trxs,
                    reverting_tx_hashes,
                    hash: B256::default(),
                    uuid: Uuid::new_v4(),
                    replacement_data: Some(replacement_data),
                    signer,
                    metadata,
                }))
            }
            Err(e) => {
                Err(PreconfError::PreconfConvertError(format!("Cannot generate order from bundle: {}", e)))
            }
        }
    }

    fn string_to_uuid(&self, uuid_str: String) -> Result<Uuid, PreconfError> {
        match Uuid::from_str(uuid_str.as_str()) {
            Ok(bundle_uuid) => {
                Ok(bundle_uuid)
            },
            Err(_) => {
                let err_msg = format!("Failed to parse UUIDv4 from received uuid str={}", uuid_str.as_str());
                Err(PreconfError::PreconfConvertError(err_msg))
            }
        }
    }

    fn eth_to_wei(&self, price_eth: f64) -> Result<U256, PreconfError> {
        if price_eth < 0.0 {
            return Err(PreconfError::PreconfConvertError(String::from("ETH price cannot be negative")));
        }

        // Convert ETH price to Wei
        let wei_value = (price_eth * 1_000_000_000_000_000_000.0).round() as u128; // Multiply by 10^18 and round
        Ok(U256::from(wei_value))
    }
}