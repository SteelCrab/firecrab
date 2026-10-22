use clap::{Args, Subcommand};
use firecrab_api_types::{
    ScheduleAction, ScheduleResponse, ScheduleRun, ScheduleSpec, ScheduleTiming,
    SetScheduleEnabledRequest,
};
use uuid::Uuid;

use crate::api_client::{ApiClient, ApiError};

#[derive(Debug, Subcommand)]
pub enum Command {
    /// List saved schedules and their next execution times.
    List {
        #[arg(long)]
        json: bool,
    },
    /// Read a schedule, including its latest execution result.
    Get { id: Uuid },
    /// Create a one-time or recurring schedule.
    Create {
        #[command(flatten)]
        spec: SpecArgs,
    },
    /// Replace a schedule's complete configuration.
    Update {
        id: Uuid,
        #[command(flatten)]
        spec: SpecArgs,
    },
    /// Enable a schedule, starting with its next future occurrence.
    Enable { id: Uuid },
    /// Disable future runs; an active operation continues to completion.
    Disable { id: Uuid },
    /// Show the latest 100 executions, newest first.
    Runs { id: Uuid },
    /// Delete an idle schedule and its execution history.
    Delete { id: Uuid },
}

#[derive(Debug, Args)]
pub struct SpecArgs {
    #[arg(long)]
    name: String,
    /// Target MicroVM UUID.
    #[arg(long)]
    vm: Uuid,
    #[arg(long, value_parser = ["start", "stop"])]
    action: String,
    /// One RFC 3339 instant, including Z or an explicit offset.
    #[arg(long, required_unless_present = "cron", conflicts_with = "cron")]
    at: Option<String>,
    /// Five fields: minute hour day month weekday; use named weekdays.
    #[arg(long, required_unless_present = "at", requires = "time_zone")]
    cron: Option<String>,
    /// IANA zone, such as Asia/Seoul. Required with --cron.
    #[arg(long, requires = "cron")]
    time_zone: Option<String>,
    /// Save the schedule without enabling it.
    #[arg(long)]
    disabled: bool,
}

impl SpecArgs {
    fn into_spec(self) -> ScheduleSpec {
        let timing = match self.at {
            Some(at) => ScheduleTiming::Once { at },
            None => ScheduleTiming::Cron {
                expression: self.cron.expect("clap requires --at or --cron"),
                time_zone: self
                    .time_zone
                    .expect("clap requires --time-zone with --cron"),
            },
        };
        ScheduleSpec {
            name: self.name,
            vm_id: self.vm,
            action: if self.action == "start" {
                ScheduleAction::Start
            } else {
                ScheduleAction::Stop
            },
            timing,
            enabled: !self.disabled,
        }
    }
}

pub fn run(client: &ApiClient, command: Command) -> Result<(), ApiError> {
    match command {
        Command::List { json } => {
            let schedules: Vec<ScheduleResponse> = client.get("/api/schedules")?;
            if json {
                print_json(&schedules);
            } else {
                println!("ID\tNAME\tVM\tACTION\tENABLED\tNEXT_RUN_UTC\tLAST_STATUS");
                for schedule in schedules {
                    println!(
                        "{}\t{}\t{}\t{:?}\t{}\t{}\t{}",
                        schedule.id,
                        schedule.spec.name,
                        schedule.spec.vm_id,
                        schedule.spec.action,
                        schedule.spec.enabled,
                        schedule
                            .next_run_at_ms
                            .and_then(chrono::DateTime::<chrono::Utc>::from_timestamp_millis)
                            .map(|at| at.to_rfc3339())
                            .unwrap_or_else(|| "-".to_owned()),
                        schedule
                            .last_run
                            .map(|run| format!("{:?}", run.status))
                            .unwrap_or_else(|| "-".to_owned())
                    );
                }
            }
        }
        Command::Get { id } => {
            print_json(&client.get::<ScheduleResponse>(&format!("/api/schedules/{id}"))?);
        }
        Command::Create { spec } => {
            print_json(&client.post::<_, ScheduleResponse>("/api/schedules", &spec.into_spec())?);
        }
        Command::Update { id, spec } => {
            print_json(
                &client.put::<_, ScheduleResponse>(
                    &format!("/api/schedules/{id}"),
                    &spec.into_spec(),
                )?,
            );
        }
        Command::Enable { id } => {
            set_enabled(client, id, true)?;
        }
        Command::Disable { id } => {
            set_enabled(client, id, false)?;
        }
        Command::Runs { id } => {
            print_json(&client.get::<Vec<ScheduleRun>>(&format!("/api/schedules/{id}/runs"))?);
        }
        Command::Delete { id } => {
            client.delete(&format!("/api/schedules/{id}"))?;
            print_json(&serde_json::json!({"deleted":id}));
        }
    }
    Ok(())
}

fn set_enabled(client: &ApiClient, id: Uuid, enabled: bool) -> Result<(), ApiError> {
    print_json(&client.put::<_, ScheduleResponse>(
        &format!("/api/schedules/{id}/enabled"),
        &SetScheduleEnabledRequest { enabled },
    )?);
    Ok(())
}

fn print_json(value: &impl serde::Serialize) {
    println!(
        "{}",
        serde_json::to_string_pretty(value).expect("API response serializes")
    );
}

#[cfg(test)]
mod tests {
    use crate::Cli;
    use clap::Parser;

    #[test]
    fn requires_one_timing_and_an_explicit_cron_zone() {
        let base = [
            "firecrab",
            "schedule",
            "create",
            "--name",
            "morning",
            "--vm",
            "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
            "--action",
            "start",
        ];
        for tail in [
            vec![],
            vec!["--cron", "0 9 * * Mon-Fri"],
            vec![
                "--at",
                "2030-01-01T09:00:00+09:00",
                "--cron",
                "0 9 * * *",
                "--time-zone",
                "UTC",
            ],
        ] {
            assert!(Cli::try_parse_from(base.into_iter().chain(tail)).is_err());
        }
        assert!(
            Cli::try_parse_from(
                base.into_iter()
                    .chain(["--at", "2030-01-01T09:00:00+09:00"])
            )
            .is_ok()
        );
        assert!(
            Cli::try_parse_from(base.into_iter().chain([
                "--cron",
                "0 9 * * Mon-Fri",
                "--time-zone",
                "Asia/Seoul"
            ]))
            .is_ok()
        );
    }
}
