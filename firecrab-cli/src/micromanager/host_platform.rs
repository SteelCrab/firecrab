//! The description of this machine that microManager leaves inside its Linux
//! VM, so the dashboard's Host view can show what Firecrab really runs on.

use firecrab_api_types::{HostOs, HostPlatformDescriptor};

pub use firecrab_api_types::HOST_PLATFORM_DESCRIPTOR_PATH;

/// One JSON line in the shape `GET /api/host` parses back.
pub fn descriptor_json(os: HostOs, name: &str, version: &str, virtualization: &str) -> String {
    let descriptor = HostPlatformDescriptor {
        os,
        name: name.to_owned(),
        version: version.trim().to_owned(),
        architecture: architecture(std::env::consts::ARCH).to_owned(),
        virtualization: virtualization.to_owned(),
    };
    serde_json::to_string(&descriptor).expect("a descriptor of plain strings always serializes")
}

/// Every microManager report spells ARM64 the way macOS does.
fn architecture(rust_architecture: &str) -> &str {
    match rust_architecture {
        "aarch64" => "arm64",
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_descriptor_round_trips_through_the_api_type() {
        let json = descriptor_json(
            HostOs::Windows,
            "Windows 11",
            " 10.0.26200.9457\n",
            "WSL2 2.7.14.0",
        );
        let parsed: HostPlatformDescriptor = serde_json::from_str(&json).expect("valid JSON");
        assert_eq!(parsed.os, HostOs::Windows);
        assert_eq!(parsed.version, "10.0.26200.9457");
        assert_eq!(parsed.virtualization, "WSL2 2.7.14.0");
        assert!(!json.contains('\n'));
    }

    #[test]
    fn arm64_is_spelled_like_macos() {
        assert_eq!(architecture("aarch64"), "arm64");
        assert_eq!(architecture("x86_64"), "x86_64");
    }
}
