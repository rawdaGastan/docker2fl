mod config;
mod handler;

use std::collections::HashMap;

use anyhow::{Context, Result};
use axum::{
    routing::{get, post},
    Extension, Router,
};
use clap::{ArgAction, Parser};
use tokio::sync::mpsc;
use tower_http::cors::CorsLayer;
use tower_http::services::ServeDir;

use crate::{
    config::{parse_config, State},
    handler::{
        create_flist_handler, get_flist_state_handler, health_checker_handler, process_flist,
        ConvertFlistRequirements,
    },
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

    let app_state = State {
        config: config.clone(),
        jobs_state: HashMap::new(),
    };

    // Create a channel to send requests to the processing task
    //TODO:
    let (sender, receiver) = mpsc::channel::<ConvertFlistRequirements>(100);

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

    // TODO: add pagination
    let app = Router::new()
        .route(
            &format!("/{}/api/fl", config.version),
            post(create_flist_handler),
        )
        .route(
            &format!("/{}/api/fl:job_id", config.version),
            get(get_flist_state_handler),
        )
        .route(
            &format!("/{}/api", config.version),
            get(health_checker_handler),
        )
        // TODO: add username to the flist path
        // .nest_service(
        //     &format!("/{}/api/fl/:name", config.version),
        //     get(get_flist_handler),
        // )
        .nest_service(
            &format!("/{}/username", config.flist_dir),
            ServeDir::new(&config.flist_dir),
        )
        // .with_state(config)
        .with_state(app_state.clone())
        .layer(Extension(sender))
        // .layer(Extension(config))
        .layer(cors);

    // Spawn a task to do all the processing. Since this is a single
    // task, all processing will be done sequentially.
    // tokio::spawn(async move {
    //     process_flist(receiver, app_state.clone()).await;
    // });

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
        // .serve(app.into_make_service())
        .await
        .context("failed to serve listener")?;

    Ok(())
}
