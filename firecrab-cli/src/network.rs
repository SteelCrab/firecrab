use clap::Subcommand;
use firecrab_api_types::{CreateMicroNetworkRequest, MicroNetworkResponse};
use uuid::Uuid;

use crate::api_client::{ApiClient, ApiError};

/// MicroNetwork operations exposed by `firecrab network`.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// List every MicroNetwork on the host.
    List {
        /// Emit the API response as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Create a MicroNetwork.
    Create {
        /// Network name.
        #[arg(long)]
        name: String,
        /// Reserved IPv4 CIDR block, for example 172.31.0.0/24.
        #[arg(long, value_name = "CIDR")]
        subnet_cidr: String,
        /// Emit the created network as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Delete an empty MicroNetwork.
    Delete {
        /// MicroNetwork UUID.
        id: Uuid,
        /// Emit a JSON deletion receipt.
        #[arg(long)]
        json: bool,
    },
}

/// Executes one MicroNetwork command through the host API.
pub fn run(client: &ApiClient, command: Command) -> Result<(), ApiError> {
    match command {
        Command::List { json } => {
            let networks: Vec<MicroNetworkResponse> = client.get("/api/micro-networks")?;
            if json {
                print_json(&networks);
            } else {
                print!("{}", format_list(&networks));
            }
        }
        Command::Create {
            name,
            subnet_cidr,
            json,
        } => {
            let request = CreateMicroNetworkRequest {
                name,
                subnet_cidr,
                internet_enabled: true,
                uplink: None,
                ipv6_cidr: None,
                ipv6_address_mode: None,
            };
            let network: MicroNetworkResponse = client.post("/api/micro-networks", &request)?;
            if json {
                print_json(&network);
            } else {
                println!("{}", format_one("created", &network));
            }
        }
        Command::Delete { id, json } => {
            client.delete(&format!("/api/micro-networks/{id}"))?;
            if json {
                print_json(&serde_json::json!({ "deleted": id }));
            } else {
                println!("deleted network {id}");
            }
        }
    }
    Ok(())
}

fn print_json(value: &impl serde::Serialize) {
    println!(
        "{}",
        serde_json::to_string_pretty(value).expect("API response serializes")
    );
}

fn format_list(networks: &[MicroNetworkResponse]) -> String {
    let mut output = String::from("ID\tNAME\tSUBNET\tGATEWAY\tINTERNET\tUPLINK\n");
    for network in networks {
        output.push_str(&format!(
            "{}\t{}\t{}\t{}\t{}\t{}\n",
            network.id,
            network.name,
            network.subnet_cidr,
            network.gateway,
            if network.internet_enabled {
                "yes"
            } else {
                "no"
            },
            network.uplink.as_deref().unwrap_or("auto")
        ));
    }
    output
}

fn format_one(action: &str, network: &MicroNetworkResponse) -> String {
    format!(
        "{action} network {} ({}, {}, gateway {})",
        network.id, network.name, network.subnet_cidr, network.gateway
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn network() -> MicroNetworkResponse {
        MicroNetworkResponse {
            id: Uuid::parse_str("11111111-1111-4111-8111-111111111111").unwrap(),
            name: "lab".to_owned(),
            subnet_cidr: "172.31.0.0/24".to_owned(),
            gateway: "172.31.0.1".to_owned(),
            internet_enabled: true,
            uplink: None,
            ipv6_cidr: None,
            ipv6_gateway: None,
            ipv6_address_mode: None,
            ipv6_egress: None,
        }
    }

    #[test]
    fn empty_list_still_has_a_header() {
        assert_eq!(
            format_list(&[]),
            "ID\tNAME\tSUBNET\tGATEWAY\tINTERNET\tUPLINK\n"
        );
    }

    #[test]
    fn list_formats_network_fields() {
        let output = format_list(&[network()]);
        assert!(output.contains("11111111-1111-4111-8111-111111111111\tlab"));
        assert!(output.contains("172.31.0.0/24\t172.31.0.1\tyes\tauto"));
    }

    #[test]
    fn create_summary_names_the_network() {
        assert_eq!(
            format_one("created", &network()),
            "created network 11111111-1111-4111-8111-111111111111 (lab, 172.31.0.0/24, gateway 172.31.0.1)"
        );
    }
}
