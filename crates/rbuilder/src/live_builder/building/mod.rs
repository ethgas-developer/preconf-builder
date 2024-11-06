pub mod relay_submit;

use std::{sync::Arc, time::Duration};
pub use relay_submit::SubmissionConfig;

use crate::{
    building::{
        builders::{BlockBuildingAlgorithm, BlockBuildingAlgorithmInput, BuilderSinkFactory},
        BlockBuildingContext,
    },
    live_builder::{payload_events::MevBoostSlotData, simulation::SlotOrderSimResults},
    utils::ProviderFactoryReopener,
};
use reth_db::database::Database;
use time::OffsetDateTime;
use tokio::time::sleep;
use tokio::sync::{broadcast, mpsc};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, trace};
use crate::live_builder::order_input::order_replacement_manager::OrderReplacementManager;
use crate::preconf::PreconfInfo;
use super::{
    bidding::BiddingService,
    order_input::{
        self, orderpool::OrdersForBlock,
    },
    payload_events,
    simulation::OrderSimulationPool,
};

#[derive(Debug)]
pub struct BlockBuildingPool<DB, BuilderSinkFactoryType: BuilderSinkFactory> {
    provider_factory: ProviderFactoryReopener<DB>,
    builders: Vec<Arc<dyn BlockBuildingAlgorithm<DB, BuilderSinkFactoryType::SinkType>>>,
    sink_factory: BuilderSinkFactoryType,
    bidding_service: Box<dyn BiddingService>,
    orderpool_subscriber: order_input::OrderPoolSubscriber,
    order_simulation_pool: OrderSimulationPool<DB>,
}

impl<DB: Database + Clone + 'static, BuilderSinkFactoryType: BuilderSinkFactory>
    BlockBuildingPool<DB, BuilderSinkFactoryType>
where
    <BuilderSinkFactoryType as BuilderSinkFactory>::SinkType: 'static,
{
    pub fn new(
        provider_factory: ProviderFactoryReopener<DB>,
        builders: Vec<Arc<dyn BlockBuildingAlgorithm<DB, BuilderSinkFactoryType::SinkType>>>,
        sink_factory: BuilderSinkFactoryType,
        bidding_service: Box<dyn BiddingService>,
        orderpool_subscriber: order_input::OrderPoolSubscriber,
        order_simulation_pool: OrderSimulationPool<DB>,
    ) -> Self {
        BlockBuildingPool {
            provider_factory,
            builders,
            sink_factory,
            bidding_service,
            orderpool_subscriber,
            order_simulation_pool,
        }
    }

    /// Connects OrdersForBlock->OrderReplacementManager->Simulations and calls start_building_job
    pub fn start_block_building(
        &mut self,
        payload: payload_events::MevBoostSlotData,
        block_ctx: BlockBuildingContext,
        global_cancellation: CancellationToken,
        max_time_to_build: Duration,
        preconf_info_sender: mpsc::Sender<PreconfInfo>,
    ) {
        let block_cancellation = global_cancellation.child_token();

        let cancel = block_cancellation.clone();
        tokio::spawn(async move {
            tokio::time::sleep(max_time_to_build).await;
            cancel.cancel();
        });

        let (orders_for_block, sink) = OrdersForBlock::new_with_sink();
        // add OrderReplacementManager to manage replacements and cancellations
        let order_replacement_manager = OrderReplacementManager::new(Box::new(sink));
        // sink removal is automatic via OrderSink::is_alive false
        let _block_sub = self.orderpool_subscriber.add_sink(
            block_ctx.block_env.number.to(),
            Box::new(order_replacement_manager),
        );

        let simulations_for_block = self.order_simulation_pool.spawn_simulation_job(
            block_ctx.clone(),
            orders_for_block,
            block_cancellation.clone(),
        );
        self.start_building_job(
            block_ctx,
            payload,
            simulations_for_block,
            block_cancellation,
            preconf_info_sender,
        );
    }

    /// Per each BlockBuildingAlgorithm creates BlockBuildingAlgorithmInput and Sinks and spawn a task to run it
    fn start_building_job(
        &mut self,
        ctx: BlockBuildingContext,
        slot_data: MevBoostSlotData,
        input: SlotOrderSimResults,
        cancel: CancellationToken,
        preconf_info_sender: mpsc::Sender<PreconfInfo>,
    ) {
        let slot = slot_data.slot();
        let block = slot_data.block();
        let slot_end_timestamp = slot_data.timestamp();

        // @Todo keep handles
        let slot_bidder = self.bidding_service.create_slot_bidder(
            block,
            slot,
            slot_end_timestamp.unix_timestamp() as u64,
        );

        let builder_sink =
            self.sink_factory
                .create_builder_sink(slot_data, slot_bidder.clone(), cancel.clone());
        let (broadcast_input, _) = broadcast::channel(10_000);

        let block_number = ctx.block_env.number.to::<u64>();
        let provider_factory = match self
            .provider_factory
            .check_consistency_and_reopen_if_needed(block_number)
        {
            Ok(provider_factory) => provider_factory,
            Err(err) => {
                error!(?err, "Error while reopening provider factory");
                return;
            }
        };

        tokio::spawn(async move {
            if let Some(delta) = ctx.slot_delta_to_start_block_build_ms {
                let submit_start_time = slot_end_timestamp + delta;
                let sleep_duration = submit_start_time - OffsetDateTime::now_utc();
                if sleep_duration.is_positive() {
                    debug!(?sleep_duration, "sleep til block start time");
                    sleep(sleep_duration.try_into().unwrap()).await;
                }
            }
            preconf_info_sender.send(PreconfInfo {
                block_number,
                slot,
                timestamp: Some(slot_end_timestamp.unix_timestamp() as u64),
            }).await.expect("preconf info sender cannot send preconf info");
        });

        debug!("start building jobs");
        for builder in self.builders.iter() {
            let builder_name = builder.name();
            debug!(block = block_number, builder_name, "Spawning builder job");
            let input = BlockBuildingAlgorithmInput::<DB, BuilderSinkFactoryType::SinkType> {
                provider_factory: provider_factory.clone(),
                ctx: ctx.clone(),
                input: broadcast_input.subscribe(),
                sink: builder_sink.clone(),
                slot_bidder: slot_bidder.clone(),
                cancel: cancel.clone(),
            };
            let builder = builder.clone();
            tokio::task::spawn_blocking(move || {
                builder.build_blocks(input);
                debug!(block = block_number, builder_name, "Stopped builder job");
            });
        }

        tokio::spawn(multiplex_job(input.orders, broadcast_input));
    }
}

async fn multiplex_job<T>(mut input: mpsc::Receiver<T>, sender: broadcast::Sender<T>) {
    // we don't worry about waiting for input forever because it will be closed by producer job
    while let Some(input) = input.recv().await {
        // we don't create new subscribers to the broadcast so here we can be sure that err means end of receivers
        if sender.send(input).is_err() {
            return;
        }
    }
    trace!("Cancelling multiplex job");
}
