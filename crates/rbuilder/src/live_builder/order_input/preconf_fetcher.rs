use super::{ReplaceableOrderPoolCommand};
use crate::{
    primitives::{Order},
};
use std::time::Duration;
use time::OffsetDateTime;
use tokio::{
    sync::{mpsc, mpsc::error::SendTimeoutError},
    task::JoinHandle,
};
use tokio::sync::mpsc::Sender;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info};
use crate::preconf::{PreconfApiClient, PreconfClient, PreconfConfig, PreconfInfo};

const PRECONF_CAPACITY: usize = 30_000;
const PRECONF_TIMEOUT_PERIOD: Duration = Duration::from_millis(50);


/// Subscribes to EL mempool and pushes new txs as orders in results.
/// This version allows 4844 by subscribing to subscribe_pending_txs to get the hashes and then calling eth_getRawTransactionByHash
/// to get the raw tx that, in case of 4844 tx, may include blobs.
/// In the future we may consider updating reth so we can process blob txs in a different task to avoid slowing down non blob txs.
pub async fn subscribe_to_preconf_pool(
    config: PreconfConfig,
    results: Sender<ReplaceableOrderPoolCommand>,
    global_cancel: CancellationToken,
) -> eyre::Result<(JoinHandle<()>, Sender<PreconfInfo>)> {
    let (preconf_info_sender, mut preconf_info_receiver) = mpsc::channel::<PreconfInfo>(1);
    let (preconf_order_sender, mut preconf_order_receiver) = mpsc::channel::<Order>(PRECONF_CAPACITY);
    match get_preconf_client(config, preconf_order_sender) {
        Ok(mut preconf_client) => {
            debug!("Preconf client: started");
            // job to receive slot info after got header from relay
            tokio::spawn(async move {
                loop {
                    match preconf_info_receiver.recv().await {
                        Some(info) => {
                            debug!("Received preconf info: {:?}", info);
                            if preconf_client.get_fallback_enabled() {
                                if let Some(api_client) = &mut preconf_client.api {
                                    fetch_preconf_orders(api_client, info).await;
                                }
                            }
                        }
                        None => {}
                    }
                }
            });
        }
        Err(e) => {
            debug!("No preconf client found, skipping: {}", e);
        }
    };
    let handle = tokio::spawn(async move {
        info!("Subscribe to preconf pool: started");
        loop {
            match preconf_order_receiver.recv().await {
                Some(order) => {
                    debug!("received preconf order (id={}).", order.id());
                    match results
                        .send_timeout(
                            ReplaceableOrderPoolCommand::Order(order),
                            PRECONF_TIMEOUT_PERIOD,
                        )
                        .await
                    {
                        Ok(()) => {}
                        Err(SendTimeoutError::Timeout(_)) => {
                            error!("Error receiving preconf order in preconf_receiver and failed to send preconf bundle to results channel, timeout");
                            break;
                        }
                        Err(SendTimeoutError::Closed(_)) => {
                            error!("Error receiving preconf order in preconf_receiver and failed to send preconf bundle to results channel, closed");
                            break;
                        }
                    }
                }
                None => {}
            }
        }
        global_cancel.cancel();
        info!("Subscribe to preconf pool: finished");
    });

    Ok((handle, preconf_info_sender))
}

pub async fn fetch_preconf_orders(api_client: &mut PreconfApiClient, info: PreconfInfo) {
    let market_expiry = api_client.get_inclusion_preconf_market_expiry(info.slot).await;
    if let Some(expiry) = market_expiry {
        let sleep_duration = expiry - OffsetDateTime::now_utc();
        if sleep_duration.is_positive() {
            debug!("preconf fetcher sleep for {:?} on slot={}", sleep_duration, info.slot);
            tokio::time::sleep(sleep_duration.try_into().unwrap()).await;
        }
        debug!("preconf fetcher fetches slot={}", info.slot);
        api_client.fetch_inclusion_preconfs(info.slot, info.block_number, info.timestamp.unwrap()).await;
    } else {
        debug!("preconf fetcher did not get market info from slot={}, skip.", info.slot);
    }
}


pub fn get_preconf_client(config: PreconfConfig, preconf_sender: Sender<Order>) -> eyre::Result<PreconfClient> {
    if config.preconf_ws_url.is_none() && config.preconf_api_url.is_none() {
        return Err(eyre::eyre!(
            "kindly include preconf_api_url or preconf_ws_url in the configuration"
        ));
    }
    Ok(PreconfClient::new(config.preconf_ws_url, config.preconf_api_url, preconf_sender))
}