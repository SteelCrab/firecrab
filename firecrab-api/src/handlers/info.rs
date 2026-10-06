//! `GET /api/info`: this build and where it is installed, the same fields
//! `firecrab info` prints, so the dashboard can show them.
//!
//! The values come from this process's own environment, which the API's unit
//! sets from `install.sh`'s, and the defaults live once in
//! [`FirecrabInfo::from_env`].

use axum::Json;
use firecrab_api_types::FirecrabInfo;

fn info_for(bind: Option<String>) -> FirecrabInfo {
    FirecrabInfo::from_env(
        env!("CARGO_PKG_VERSION"),
        &format!("http://{}", crate::server::bind_addr_or_default(bind)),
    )
}

pub async fn get_info() -> Json<FirecrabInfo> {
    Json(info_for(std::env::var("FIRECRAB_BIND_ADDR").ok()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_running_version_and_the_default_address_are_reported() {
        let info = info_for(None);
        assert_eq!(info.version, env!("CARGO_PKG_VERSION"));
        assert_eq!(info.api_base, "http://127.0.0.1:5523");
    }

    #[test]
    fn the_address_the_api_listens_on_is_reported() {
        assert_eq!(
            info_for(Some("0.0.0.0:8443".to_owned())).api_base,
            "http://0.0.0.0:8443"
        );
    }

    #[tokio::test]
    async fn the_handler_answers_with_this_build() {
        let Json(info) = get_info().await;
        assert_eq!(info.version, env!("CARGO_PKG_VERSION"));
        assert!(!info.prefix.is_empty() && !info.datadir.is_empty());
    }
}
