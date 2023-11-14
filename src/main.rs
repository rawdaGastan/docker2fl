
use bollard::Docker;
use bollard::image::{RemoveImageOptions, CreateImageOptions};
use bollard::container::{RemoveContainerOptions, InspectContainerOptions, CreateContainerOptions, Config};

use futures_util::stream::StreamExt;
use anyhow::{Context, Result};
use regex::Regex;
use std::default::Default;
use std::process::Command;
use std::collections::HashMap;
use std::fs::File;
use std::path::PathBuf;
use serde_json::json;

use rfs::fungi;
use rfs::store::{self, Router};

use uuid::Uuid;

#[tokio::main]
async fn main() -> Result<()> {
  let debug = 1;
  simple_logger::SimpleLogger::new()
    .with_utc_timestamps()
    .with_level({
        match debug {
            0 => log::LevelFilter::Info,
            1 => log::LevelFilter::Debug,
            _ => log::LevelFilter::Trace,
        }
    })
    .with_module_level("sqlx", log::Level::Error.to_level_filter())
    .init()?;


  log::debug!("Start!");
  let image_name = String::from("redis");
  let store_url = vec!["dir:///tmp/store0".to_string()];
  let docker_directory = String::from("docker_temp");

  let converter = DockerConverter::new(store_url, docker_directory, image_name);
	converter.convert().await?;
	Ok(())

  // let rt = tokio::runtime::Runtime::new()?;
  // rt.block_on(async move {
  //   converter.convert().await?;
  //   Ok(())
  // })
}

#[derive(Debug)]
pub struct DockerConverter {
  store_url: Vec<String>,
  docker_directory: String,
  image_name: String,
}

impl DockerConverter {
  pub fn new(store_url: Vec<String>, docker_directory: String, image_name: String) -> Self {
    Self {
      store_url,
      docker_directory,
      image_name,
    }
  }

  pub async fn convert(&self) -> Result<()> {
    log::debug!("store: {:#?}", self.store_url);
    log::debug!("image name: {}", self.image_name);
    log::debug!("directory name: {}", self.docker_directory);
    log::debug!("flist name: {}.fl", self.image_name);

    // #[cfg(unix)]
    // let docker = Docker::connect_with_socket_defaults().context("failed to create docker")?;

    // let mut docker_image = self.image_name.to_string();
    // if !docker_image.contains(':') {
    //   docker_image.push_str(":latest");
    // }

    // let container_name = Uuid::new_v4().to_string();
    // log::debug!("Starting temporary container {}", &container_name);

    // self.extract_image(&docker, &docker_image, &container_name).await
    // .context("failed to extract docker image to a directory")?;

    self.convert_to_fl().await
    .context("failed to convert docker image to flist")?;

    // self.clean(&docker, &docker_image, &container_name).await
    // .context("failed to clean docker image and container")?;

    Ok(())
  }

	async fn convert_to_fl(&self) -> Result<()> {
    log::debug!("using rfs to pack {} to an flist", &self.image_name);

		let target = &self.docker_directory;
    let store = parse_router(self.store_url.as_slice()).await.context("failed to parse store urls")?;
    let meta = fungi::Writer::new(format!("{}.fl", self.image_name)).await.context("failed to format flist metdata")?;
    rfs::pack(meta, store, target, true).await.context("failed to pack flist")?;

    Ok(())
	}

	// TODO: from string to str
  async fn extract_image(&self, docker: &Docker, docker_image: &String, container_name: &String) -> Result<()> {
    self.pull_image(docker, docker_image).await?;
    self.export_container(docker, docker_image, container_name).await?;
    Ok(())
	}

  async fn container_boot(&self, docker: &Docker, container_name: &String, docker_tmp_dir: &String) -> Result<()> {
    log::debug!("Inspecting docker container {}", &container_name);

    let options = Some(InspectContainerOptions{
      size: false,
    });

    let container = docker.inspect_container(container_name, options).await.context("failed to inpect docker container")?;
    let container_config = container.config.context("failed to get docker container configs")?;

    let command;
    let args;
    let mut env: HashMap<String, String>  = HashMap::new();
    let mut cwd = String::from("/");

    let cmd = container_config.cmd.unwrap();

    if container_config.entrypoint.is_some() {
      let entrypoint = container_config.entrypoint.unwrap();
      command = (entrypoint.first().unwrap()).to_string();

      if entrypoint.len() > 1 {
        let (_, entries) = entrypoint.split_first().unwrap();
        args = entries.to_vec();
      } 
      else {
        args = cmd;
      }
    }
    else{
      command = (cmd.first().unwrap()).to_string();
      let (_, entries) = cmd.split_first().unwrap();
      args = entries.to_vec();
    }

    if container_config.env.is_some() {
      for entry in container_config.env.unwrap().iter() {
        let mut split = entry.split('=');
        env.insert(split.next().unwrap().to_string(), split.next().unwrap().to_string());
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

    let file_path = PathBuf::from(&docker_tmp_dir).join(".startup.toml");
    serde_json::to_writer(&File::create(file_path)?, &metadata)?;

    Ok(())
  }

  async fn export_container(&self, docker: &Docker, docker_image: &String, container_name: &String) -> Result<()> {
    log::debug!("Inspecting docker image {}", &docker_image);

    let image = docker.inspect_image(docker_image).await.context("failed to inpect docker image")?;
    let image_config = image.config.context("failed to get docker image configs")?;

    let mut command = String::new();
    if image_config.cmd.is_none() && image_config.entrypoint.is_none() {
        command = String::from("/bin/sh");
    }

    let options = Some(CreateContainerOptions{
      name: container_name.clone(),
      platform: None,
    });
    
    let config = Config {
        image: Some(docker_image.to_string()),
        hostname: Some(container_name.clone()),
        cmd: Some(vec![command]),
        ..Default::default()
    };
    
    docker.create_container(options, config).await.context("failed to create docker temporary container")?;

    let tmp_dir = tempdir::TempDir::new_in(&self.docker_directory, container_name)?;
    let tmp_dir_path = tmp_dir.path().display();

    Command::new("sh")
      .arg("-c")
      .arg(format!("docker export {} | tar -xpf - -C {}", &container_name, &tmp_dir_path))
      .output()
      .expect("failed to execute export docker container");

    self.container_boot(docker, container_name, &tmp_dir_path.to_string()).await?;

    Ok(())
  }

  async fn pull_image(&self, docker: &Docker, docker_image: &String) -> Result<()> {
    log::debug!("pulling docker image {}", &docker_image);

    let options = Some(CreateImageOptions{
      from_image: docker_image.to_string(),
      ..Default::default()
    });
    
    let mut image_pull_stream = docker.create_image(options, None, None);
    while let Some(msg) = image_pull_stream.next().await {
      log::debug!("Pull message: {:?}", msg);
      msg.context("failed to pull docker image")?;
    }

    Ok(())
	}

  async fn clean(&self, docker: &Docker, docker_image: &str, container_name: &str) -> Result<()> {
    log::debug!("cleaning docker image and container");

    let options = Some(RemoveContainerOptions{
      force: true,
      ..Default::default()
    });
    
    docker.remove_container(container_name, options).await?;

    let remove_options = Some(RemoveImageOptions{
      force: true,
      ..Default::default()
    });

    docker.remove_image(docker_image, remove_options, None).await?;

    Ok(())
	}
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
