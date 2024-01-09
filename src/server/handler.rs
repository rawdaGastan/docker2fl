use crate::config::Config;
use axum::{debug_handler, extract::State, http::StatusCode, response::IntoResponse, Json};
use bollard::auth::DockerCredentials;
use serde::{Deserialize, Serialize};

#[allow(non_snake_case)]
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct Flist {
    pub image_name: String,
    pub username: Option<String>,
    pub password: Option<String>,
    pub auth: Option<String>,
    pub email: Option<String>,
    pub server_address: Option<String>,
    pub identity_token: Option<String>,
    pub registry_token: Option<String>,
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
    State(config): State<Config>,
    Json(body): Json<Flist>,
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

    match docker2fl::convert(&config.store_url, &body.image_name, credentials).await {
        Ok(_) => (
            StatusCode::CREATED,
            Json(serde_json::json!({
                "status": "success",
                "url": format!(
                    "{}:{}/{}/{}",
                    config.host, config.port, config.flist_dir, "flist_name"
                ),
            })),
        ),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "status": "failed",
                "message": format!("Failed to create flist: {}", e),
            })),
        ),
    }
}
