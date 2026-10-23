//! Native Firecracker execution state; process liveness alone cannot identify a pause.
use std::path::Path;
use std::time::Duration;

use firecrab_api_types::VmState;
use serde::Deserialize;

pub(crate) struct Client(reqwest::Client);

impl Client {
    pub(crate) fn new(socket: &Path) -> Result<Self, String> {
        reqwest::Client::builder()
            .unix_socket(socket.to_owned())
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(5))
            .build()
            .map(Self)
            .map_err(|error| error.to_string())
    }

    pub(crate) async fn state(&self) -> Result<VmState, String> {
        #[derive(Deserialize)]
        struct InstanceInfo {
            state: String,
        }
        let response = self
            .0
            .get("http://localhost/")
            .send()
            .await
            .map_err(|error| error.to_string())?
            .error_for_status()
            .map_err(|error| error.to_string())?;
        let body = response.text().await.map_err(|error| error.to_string())?;
        let info: InstanceInfo = serde_json::from_str(&body).map_err(|error| error.to_string())?;
        match info.state.as_str() {
            "Running" => Ok(VmState::Running),
            "Paused" => Ok(VmState::Paused),
            other => Err(format!("unexpected Firecracker execution state: {other}")),
        }
    }

    pub(crate) async fn set_state(&self, target: VmState) -> Result<(), String> {
        let body = match target {
            VmState::Paused => r#"{"state":"Paused"}"#,
            VmState::Running => r#"{"state":"Resumed"}"#,
            _ => return Err("unsupported execution state".into()),
        };
        self.0
            .patch("http://localhost/vm")
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body)
            .send()
            .await
            .map_err(|error| error.to_string())?
            .error_for_status()
            .map_err(|error| error.to_string())?;
        Ok(())
    }
}
