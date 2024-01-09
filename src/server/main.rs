mod config;
mod handler;

use anyhow::{Context, Result};

use axum::{
    routing::{get, post},
    Router,
};

use clap::{ArgAction, Parser};
use tower_http::cors::CorsLayer;
use tower_http::services::ServeDir;

use crate::{
    config::parse_config,
    handler::{create_flist_handler, health_checker_handler},
};

#[derive(Parser, Debug)]
#[clap(name ="fl-server", author, version = env!("GIT_VERSION"), about, long_about = None)]
struct Options {
    /// enable debugging logs
    #[clap(short, long, action=ArgAction::Count)]
    debug: u8,

    /// config file path
    #[clap(short, long)]
    config_path: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    let opts = Options::parse();
    let config = parse_config(&opts.config_path).context("failed to parse config file")?;

    simple_logger::SimpleLogger::new()
        .with_utc_timestamps()
        .with_level({
            match opts.debug {
                0 => log::LevelFilter::Info,
                1 => log::LevelFilter::Debug,
                _ => log::LevelFilter::Trace,
            }
        })
        .with_module_level("sqlx", log::Level::Error.to_level_filter())
        .init()?;

    let cors = CorsLayer::new();
    // TODO:
    // .allow_origin("http://localhost:3000".parse::<HeaderValue>().unwrap())
    // .allow_methods([Method::GET, Method::POST])
    // .allow_credentials(true)
    // .allow_headers([AUTHORIZATION, ACCEPT, CONTENT_TYPE]);

    let app = Router::new()
        .route(
            &format!("{}/api/flist", config.version),
            get(health_checker_handler),
        )
        .route(
            &format!("{}/api/flist", config.version),
            post(create_flist_handler),
        )
        .nest_service(
            &format!("/{}", config.flist_dir),
            ServeDir::new(&config.flist_dir),
        )
        .with_state(config.clone())
        .layer(cors);

    let address = format!("{}:{}", config.host, config.port);
    let listener = tokio::net::TcpListener::bind(address)
        .await
        .context("failed to bind address")?;

    log::info!(
        "🚀 Server started successfully at {}:{}",
        config.host,
        config.port
    );
    axum::serve(listener, app)
        .await
        .context("failed to serve listener")?;

    Ok(())
}
