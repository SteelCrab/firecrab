//! CLI input never places provider keys in process arguments or normal output.
use crate::api_client::ApiClient;
use clap::Subcommand;
use firecrab_api_types::{
    AiConnection, AiConnectionInput, AiLoginRequest, AiLoginStart, AiLoginStatus, AiModelSelection,
    AiResponse, AiResponseRequest, AiVerifyRequest,
};
use std::{
    io::{self, Read},
    path::PathBuf,
};
use uuid::Uuid;

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Sign in with ChatGPT using the local REST callback. No API key is needed.
    Login {
        id: Uuid,
        #[arg(long)]
        name: String,
        /// Optional inference probe. Omit to connect the account without a model.
        #[arg(long)]
        model: Option<String>,
        /// Return the authorization URL without waiting for browser sign-in.
        #[arg(long)]
        no_wait: bool,
    },
    /// Inspect a login attempt without exposing credentials.
    LoginStatus { login: Uuid },
    /// List connected providers and inference-verified models as JSON.
    List,
    /// Verify and save/replace a connection from private JSON; use '-' for stdin.
    Connect {
        /// Stable client-generated UUID. Reuse it to replace credentials.
        id: Uuid,
        #[arg(long)]
        file: PathBuf,
    },
    /// Verify inference permission for another model on a saved connection.
    Verify { id: Uuid, model: String },
    /// Disconnect and remove dependent project defaults.
    Disconnect { id: Uuid },
    /// Read a project's saved model selection.
    GetDefault { project: Uuid },
    /// Set a project default to an inference-verified model.
    SetDefault {
        project: Uuid,
        connection: Uuid,
        model: String,
    },
    /// Generate text from stdin using a project default or explicit task override.
    Respond {
        project: Uuid,
        #[arg(long, requires = "model")]
        connection: Option<Uuid>,
        #[arg(long, requires = "connection")]
        model: Option<String>,
    },
}

pub fn run(client: &ApiClient, command: Command) -> Result<(), Box<dyn std::error::Error>> {
    let base = "/api/pipe/ai";
    match command {
        Command::Login {
            id,
            name,
            model,
            no_wait,
        } => {
            let start: AiLoginStart = client.post(
                &format!("{base}/connections/{id}/login"),
                &AiLoginRequest { name, model },
            )?;
            if no_wait {
                print(&start)?;
                return Ok(());
            }
            eprintln!(
                "Continue with ChatGPT in your browser: {}",
                start.authorization_url
            );
            let deadline =
                std::time::Instant::now() + std::time::Duration::from_secs(start.expires_in);
            loop {
                let status: AiLoginStatus =
                    client.get(&format!("{base}/logins/{}", start.login_id))?;
                match status.status.as_str() {
                    "connected" => {
                        print(&status)?;
                        break;
                    }
                    "failed" => {
                        return Err(status
                            .message
                            .unwrap_or_else(|| "ChatGPT login failed".into())
                            .into());
                    }
                    _ if std::time::Instant::now() >= deadline => {
                        return Err("ChatGPT login expired".into());
                    }
                    _ => std::thread::sleep(std::time::Duration::from_secs(2)),
                }
            }
        }
        Command::LoginStatus { login } => {
            print(&client.get::<AiLoginStatus>(&format!("{base}/logins/{login}"))?)?
        }
        Command::List => print(&client.get::<Vec<AiConnection>>(&format!("{base}/connections"))?)?,
        Command::Connect { id, file } => {
            let bytes = if file.as_os_str() == "-" {
                read_stdin(65_536)?
            } else {
                let file = std::fs::File::open(file)?;
                read_bounded(file, 65_536)?
            };
            let input: AiConnectionInput = serde_json::from_slice(&bytes)
                .map_err(|_| "Invalid AI connection JSON; expected name, provider and apiKey")?;
            print(&client.put::<_, AiConnection>(&format!("{base}/connections/{id}"), &input)?)?;
        }
        Command::Verify { id, model } => print(&client.post::<_, AiConnection>(
            &format!("{base}/connections/{id}/verify"),
            &AiVerifyRequest { model },
        )?)?,
        Command::Disconnect { id } => {
            print(&client.delete_json::<serde_json::Value>(&format!("{base}/connections/{id}"))?)?;
        }
        Command::GetDefault { project } => print(
            &client.get::<Option<AiModelSelection>>(&format!("{base}/projects/{project}/model"))?,
        )?,
        Command::SetDefault {
            project,
            connection,
            model,
        } => print(&client.put::<_, AiModelSelection>(
            &format!("{base}/projects/{project}/model"),
            &AiModelSelection {
                connection_id: connection,
                model,
            },
        )?)?,
        Command::Respond {
            project,
            connection,
            model,
        } => {
            let input = String::from_utf8(read_stdin(32_768)?)?;
            let selection = connection
                .zip(model)
                .map(|(connection_id, model)| AiModelSelection {
                    connection_id,
                    model,
                });
            print(&client.post::<_, AiResponse>(
                &format!("{base}/responses"),
                &AiResponseRequest {
                    project_id: project,
                    selection,
                    input,
                },
            )?)?;
        }
    }
    Ok(())
}

fn read_stdin(limit: usize) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    read_bounded(io::stdin().lock(), limit)
}

fn read_bounded(reader: impl Read, limit: usize) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let mut bytes = Vec::new();
    reader.take((limit + 1) as u64).read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err("AI input exceeds the size limit".into());
    }
    Ok(bytes)
}

fn print(value: &impl serde::Serialize) -> Result<(), serde_json::Error> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}
