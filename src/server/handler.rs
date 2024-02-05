use std::collections::HashMap;

use axum::{
    debug_handler, extract::Path, extract::State, http::StatusCode, response::IntoResponse,
    Extension, Json,
};
use bollard::auth::DockerCredentials;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use rfs::fungi::Writer;
use rfs::store::Router;
use uuid::Uuid;

use crate::config::{self, JobID};

#[allow(non_snake_case)]
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct FlistInputs {
    pub image_name: String,
    pub username: Option<String>,
    pub password: Option<String>,
    pub auth: Option<String>,
    pub email: Option<String>,
    pub server_address: Option<String>,
    pub identity_token: Option<String>,
    pub registry_token: Option<String>,
}

pub struct ConvertFlistRequirements {
    job_id: JobID,
    writer: Writer,
    store: Router,
    docker_image: String,
    fl_path: String,
    docker_credentials: Option<DockerCredentials>,
}

#[derive(Debug, Clone, Serialize)]

pub enum FlistState {
    Accepted,
    Started,
    Created,
    Failed,
    NotExists,
}

pub async fn health_checker_handler() -> impl IntoResponse {
    let json_response = serde_json::json!({
        "status": "success",
        "message": "flist health checker"
    });

    (StatusCode::OK, Json(json_response))
}

#[debug_handler]
pub async fn create_flist_handler(
    Extension(sender): Extension<mpsc::Sender<ConvertFlistRequirements>>,
    State(mut app): State<config::State>,
    Json(body): Json<FlistInputs>,
) -> impl IntoResponse {
    let credentials = Some(DockerCredentials {
        username: body.username,
        password: body.password,
        auth: body.auth,
        email: body.email,
        serveraddress: body.server_address,
        identitytoken: body.identity_token,
        registrytoken: body.registry_token,
    });

    let mut docker_image = body.image_name.to_string();
    if !docker_image.contains(':') {
        docker_image.push_str(":latest");
    }

    let fl_name = docker_image.replace([':', '/'], "-") + ".fl";
    // TODO: username
    let fl_path = format!("{}/{}/{}", app.config.flist_dir, "username", fl_name);

    let meta = match Writer::new(&fl_path).await {
        Ok(writer) => writer,
        Err(err) => {
            log::error!(
                "failed to create a new writer for flist `{}` with error {}",
                fl_path,
                err
            );
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "status": "failed",
                    "message": "internal server error",
                })),
            );
        }
    };

    let store = match docker2fl::parse_router(&app.config.store_url).await {
        Ok(s) => s,
        Err(err) => {
            log::error!("failed to parse router for store with error {}", err);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "status": "failed",
                    "message": "internal server error",
                })),
            );
        }
    };

    // Create a new job id for the flist request
    let job_id = JobID(Uuid::new_v4().to_string());

    let requirements = ConvertFlistRequirements {
        job_id: job_id.clone(),
        writer: meta,
        store,
        docker_image,
        fl_path,
        docker_credentials: credentials,
    };

    // Send our request to the processing task
    let res = sender.send(requirements).await;
    if res.is_err() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "status": "failed",
                "message": format!("Failed to send flist inputs: {:?}", res.err()),
            })),
        );
    }

    app.jobs_state.insert(job_id.clone(), FlistState::Accepted);
    (
        StatusCode::ACCEPTED,
        Json(serde_json::json!({
            "status": "success",
            "job_id": job_id,
        })),
    )
}

pub async fn get_flist_state_handler(
    Path(flist_job_id): Path<String>,
    State(app): State<config::State>,
) -> impl IntoResponse {
    if !app.jobs_state.contains_key(&JobID(flist_job_id.clone())) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "status": "failed",
                "message": FlistState::NotExists,
            })),
        );
    }

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "status": "success",
            "job_state": app.jobs_state.get(&JobID(flist_job_id.clone())),
        })),
    )
}

// pub fn get_flist_handler(
//     Path(flist_name): Path<String>,
//     State(app): State<config::State>s,
// ) {
//     // TODO: username
//     ServeFile::new(format!("/{}/{}/{}", config.flist_dir, "username", flist_name));
// }

pub async fn process_flist(
    mut receiver: mpsc::Receiver<ConvertFlistRequirements>,
    mut app: config::State,
) {
    while let Some(requirements) = receiver.recv().await {
        app.jobs_state
            .insert(requirements.job_id.clone(), FlistState::Started);

        let res = docker2fl::convert(
            requirements.writer,
            requirements.store,
            &requirements.docker_image,
            requirements.docker_credentials,
        )
        .await;

        // remove the file created with the writer if fl creation failed
        if res.is_err() {
            let _ = tokio::fs::remove_file(requirements.fl_path).await;
            app.jobs_state
                .insert(requirements.job_id.clone(), FlistState::Failed);
            continue;
        }

        app.jobs_state
            .insert(requirements.job_id, FlistState::Created);
    }
}
