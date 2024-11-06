use rbuilder::{
    live_builder::{cli, config::Config},
    utils::build_info::print_version_info,
};

#[tokio::main]
async fn main() -> eyre::Result<()> {
    cli::run::<Config>(print_version_info, None).await
}
