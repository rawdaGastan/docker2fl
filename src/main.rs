use bollard::container::{
    Config, CreateContainerOptions, InspectContainerOptions, RemoveContainerOptions,
};
use bollard::image::{CreateImageOptions, RemoveImageOptions};
use bollard::Docker;

use anyhow::{Context, Result};
use clap::{ArgAction, Parser};
use futures_util::stream::StreamExt;
use regex::Regex;
use serde_json::json;
use std::collections::HashMap;
use std::default::Default;
use std::fs::File;
use std::path::Path;
use std::process::Command;

use rfs::fungi;
use rfs::store::{self, Router};

use uuid::Uuid;

#[derive(Parser, Debug)]
#[clap(name ="docker2fl", author, version = env!("GIT_VERSION"), about, long_about = None)]
struct Options {
    /// enable debugging logs
    #[clap(long, action=ArgAction::Count)]
    debug: u8,

    /// name of the docker image to be converted to flist
    #[clap(short, long)]
    image_name: String,

    /// store url for rfs in the format [xx-xx=]<url>. the range xx-xx is optional and used for
    /// sharding. the URL is per store type, please check docs for more information
    #[clap(short, long, action=ArgAction::Append)]
    store: Vec<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let opts = Options::parse();

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

    let mut docker_image = opts.image_name.to_string();
    if !docker_image.contains(':') {
        docker_image.push_str(":latest");
    }

    convert(&opts.store, &docker_image).await?;
    Ok(())
}

pub async fn convert(store: &[String], image_name: &str) -> Result<()> {
    #[cfg(unix)]
    let docker = Docker::connect_with_socket_defaults().context("failed to create docker")?;

    let container_name = Uuid::new_v4().to_string();

    let docker_tmp_dir = tempdir::TempDir::new(&container_name)?;
    let docker_tmp_dir_path = docker_tmp_dir.path();

    extract_image(&docker, image_name, &container_name, docker_tmp_dir_path)
        .await
        .context("failed to extract docker image to a directory")?;

    let flist_name = image_name.replace([':', '/'], "-") + ".fl";

    convert_to_fl(&flist_name, store, docker_tmp_dir_path)
        .await
        .context("failed to convert docker image to flist")?;

    clean(&docker, image_name, &container_name)
        .await
        .context("failed to clean docker image and container")?;

    log::info!("flist '{}' has been created", flist_name);
    Ok(())
}

async fn convert_to_fl(
    flist_name: &str,
    store_urls: &[String],
    docker_tmp_dir_path: &Path,
) -> Result<()> {
    log::info!("using rfs to pack flist '{}'", flist_name);

    let store = parse_router(store_urls)
        .await
        .context("failed to parse store urls")?;
    let meta = fungi::Writer::new(flist_name)
        .await
        .context("failed to format flist metadata")?;
    rfs::pack(meta, store, docker_tmp_dir_path, true)
        .await
        .context("failed to pack flist")?;

    Ok(())
}

async fn extract_image(
    docker: &Docker,
    image_name: &str,
    container_name: &str,
    docker_tmp_dir_path: &Path,
) -> Result<()> {
    pull_image(docker, image_name).await?;
    create_container(docker, image_name, container_name)
        .await
        .context("failed to create docker container")?;
    export_container(container_name, docker_tmp_dir_path)
        .context("failed to export docker container")?;
    container_boot(docker, container_name, docker_tmp_dir_path)
        .await
        .context("failed to boot docker container")?;
    Ok(())
}

async fn pull_image(docker: &Docker, image_name: &str) -> Result<()> {
    log::info!("pulling docker image {}", image_name);

    let options = Some(CreateImageOptions {
        from_image: image_name,
        ..Default::default()
    });

    let mut image_pull_stream = docker.create_image(options, None, None);
    while let Some(msg) = image_pull_stream.next().await {
        msg.context("failed to pull docker image")?;
    }

    Ok(())
}

async fn create_container(docker: &Docker, image_name: &str, container_name: &str) -> Result<()> {
    log::debug!("Inspecting docker image configurations {}", image_name);

    let image = docker
        .inspect_image(image_name)
        .await
        .context("failed to inspect docker image")?;
    let image_config = image.config.context("failed to get docker image configs")?;

    let mut command = "";
    if image_config.cmd.is_none() && image_config.entrypoint.is_none() {
        command = "/bin/sh";
    }

    log::debug!("Creating a docker container {}", container_name);

    let options = Some(CreateContainerOptions {
        name: container_name,
        platform: None,
    });

    let config = Config {
        image: Some(image_name),
        hostname: Some(container_name),
        cmd: Some(vec![command]),
        ..Default::default()
    };

    docker
        .create_container(options, config)
        .await
        .context("failed to create docker temporary container")?;

    Ok(())
}

fn export_container(container_name: &str, docker_tmp_dir_path: &Path) -> Result<()> {
    log::debug!("Exporting docker container {}", container_name);

    Command::new("sh")
        .arg("-c")
        .arg(format!(
            "docker export {} | tar -xpf - -C {}",
            container_name,
            docker_tmp_dir_path.display()
        ))
        .output()
        .expect("failed to execute export docker container");

    Ok(())
}

async fn container_boot(
    docker: &Docker,
    container_name: &str,
    docker_tmp_dir_path: &Path,
) -> Result<()> {
    log::debug!(
        "Inspecting docker container configurations {}",
        container_name
    );

    let options = Some(InspectContainerOptions { size: false });
    let container = docker
        .inspect_container(container_name, options)
        .await
        .context("failed to inspect docker container")?;

    let container_config = container
        .config
        .context("failed to get docker container configs")?;

    let command;
    let args;
    let mut env: HashMap<String, String> = HashMap::new();
    let mut cwd = String::from("/");

    let cmd = container_config.cmd.unwrap();

    if container_config.entrypoint.is_some() {
        let entrypoint = container_config.entrypoint.unwrap();
        command = (entrypoint.first().unwrap()).to_string();

        if entrypoint.len() > 1 {
            let (_, entries) = entrypoint.split_first().unwrap();
            args = entries.to_vec();
        } else {
            args = cmd;
        }
    } else {
        command = (cmd.first().unwrap()).to_string();
        let (_, entries) = cmd.split_first().unwrap();
        args = entries.to_vec();
    }

    if container_config.env.is_some() {
        for entry in container_config.env.unwrap().iter() {
            let mut split = entry.split('=');
            env.insert(
                split.next().unwrap().to_string(),
                split.next().unwrap().to_string(),
            );
        }
    }

    if container_config.working_dir.is_some() {
        cwd = container_config.working_dir.unwrap();
    }

    let metadata = json!({
        "startup": {
            "entry": {
                "name": "core.system",
                "args": {
                    "name": command,
                    "args": args,
                    "env": env,
                    "dir": cwd,
                }
            }
        }
    });

    log::debug!(
        "Creating '.startup.toml' file from container {}",
        container_name
    );
    serde_json::to_writer(
        &File::create(docker_tmp_dir_path.join(".startup.toml"))?,
        &metadata,
    )
    .context("failed to create '.startup.toml' file")?;

    Ok(())
}

async fn clean(docker: &Docker, image_name: &str, container_name: &str) -> Result<()> {
    log::info!("cleaning docker image and container");

    let options = Some(RemoveContainerOptions {
        force: true,
        ..Default::default()
    });

    docker
        .remove_container(container_name, options)
        .await
        .context("failed to remove docker image")?;

    let remove_options = Some(RemoveImageOptions {
        force: true,
        ..Default::default()
    });

    docker
        .remove_image(image_name, remove_options, None)
        .await
        .context("failed to remove docker container")?;

    Ok(())
}

async fn parse_router(urls: &[String]) -> Result<Router> {
    let mut router = Router::new();
    let pattern = r"^(?P<range>[0-9a-f]{2}-[0-9a-f]{2})=(?P<url>.+)$";
    let re = Regex::new(pattern)?;

    for u in urls {
        let ((start, end), store) = match re.captures(u) {
            None => ((0x00, 0xff), store::make(u).await?),
            Some(captures) => {
                let url = captures.name("url").context("missing url group")?.as_str();
                let rng = captures
                    .name("range")
                    .context("missing range group")?
                    .as_str();

                let store = store::make(url).await?;
                let range = match rng.split_once('-') {
                    None => anyhow::bail!("invalid range format"),
                    Some((low, high)) => (
                        u8::from_str_radix(low, 16)
                            .with_context(|| format!("failed to parse low range '{}'", low))?,
                        u8::from_str_radix(high, 16)
                            .with_context(|| format!("failed to parse high range '{}'", high))?,
                    ),
                };
                (range, store)
            }
        };

        router.add(start, end, store);
    }

    Ok(router)
}
