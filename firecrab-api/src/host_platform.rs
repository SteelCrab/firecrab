//! What `GET /api/host` reports about the machine behind the dashboard.
//!
//! On a Linux host that is this system. On macOS and Windows, Firecrab runs in
//! microManager's Linux VM, which cannot see the outer machine, so microManager
//! records it in [`HOST_PLATFORM_DESCRIPTOR_PATH`] and this module reads it.

use std::fs;

use firecrab_api_types::{
    HOST_PLATFORM_DESCRIPTOR_PATH, HostOs, HostPlatformDescriptor, HostPlatformResponse,
};

const OS_RELEASE_PATH: &str = "/etc/os-release";
const KERNEL_RELEASE_PATH: &str = "/proc/sys/kernel/osrelease";

/// Reads the live files and describes the machine behind the dashboard.
pub(crate) fn read_host_platform() -> HostPlatformResponse {
    host_platform(
        fs::read_to_string(HOST_PLATFORM_DESCRIPTOR_PATH)
            .ok()
            .as_deref(),
        fs::read_to_string(OS_RELEASE_PATH).ok().as_deref(),
        fs::read_to_string(KERNEL_RELEASE_PATH).ok().as_deref(),
        std::env::consts::ARCH,
    )
}

/// A descriptor microManager left behind names the outer machine; without
/// one (or with one that does not parse), this Linux system is the host.
fn host_platform(
    descriptor: Option<&str>,
    os_release: Option<&str>,
    kernel: Option<&str>,
    architecture: &str,
) -> HostPlatformResponse {
    let system = os_release
        .and_then(pretty_name)
        .unwrap_or_else(|| "Linux".to_owned());
    let kernel = kernel.map(str::trim).unwrap_or_default().to_owned();
    match descriptor.and_then(|text| serde_json::from_str::<HostPlatformDescriptor>(text).ok()) {
        Some(descriptor) => HostPlatformResponse {
            os: descriptor.os,
            name: descriptor.name,
            version: Some(descriptor.version).filter(|version| !version.is_empty()),
            architecture: descriptor.architecture,
            virtualization: Some(descriptor.virtualization)
                .filter(|virtualization| !virtualization.is_empty()),
            system,
            kernel,
        },
        None => HostPlatformResponse {
            os: HostOs::Linux,
            name: system.clone(),
            version: None,
            architecture: report_architecture(architecture).to_owned(),
            virtualization: None,
            system,
            kernel,
        },
    }
}

/// `PRETTY_NAME` from os-release(5), with its optional quotes removed.
fn pretty_name(os_release: &str) -> Option<String> {
    os_release
        .lines()
        .find_map(|line| line.strip_prefix("PRETTY_NAME="))
        .map(|value| {
            value
                .trim()
                .trim_matches(|c| c == '"' || c == '\'')
                .to_owned()
        })
        .filter(|value| !value.is_empty())
}

/// microManager and the macOS helper report `arm64`, so Linux does too.
fn report_architecture(architecture: &str) -> &str {
    match architecture {
        "aarch64" => "arm64",
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEBIAN: &str =
        "PRETTY_NAME=\"Debian GNU/Linux 13 (trixie)\"\nNAME=\"Debian GNU/Linux\"\n";
    const WINDOWS: &str = r#"{"os":"windows","name":"Windows 11","version":"10.0.26200.9457","architecture":"x86_64","virtualization":"WSL2 2.7.14.0"}"#;

    #[test]
    fn a_linux_host_describes_itself() {
        let platform = host_platform(None, Some(DEBIAN), Some("6.12.48-amd64\n"), "aarch64");
        assert_eq!(platform.os, HostOs::Linux);
        assert_eq!(platform.name, "Debian GNU/Linux 13 (trixie)");
        assert_eq!(platform.system, platform.name);
        assert_eq!(platform.kernel, "6.12.48-amd64");
        assert_eq!(platform.architecture, "arm64");
        assert_eq!(platform.virtualization, None);
        assert_eq!(platform.version, None);
    }

    #[test]
    fn a_micromanager_descriptor_names_the_outer_machine() {
        let platform = host_platform(
            Some(WINDOWS),
            Some(DEBIAN),
            Some("6.18.33.2-microsoft-standard-WSL2"),
            "x86_64",
        );
        assert_eq!(platform.os, HostOs::Windows);
        assert_eq!(platform.name, "Windows 11");
        assert_eq!(platform.version.as_deref(), Some("10.0.26200.9457"));
        assert_eq!(platform.virtualization.as_deref(), Some("WSL2 2.7.14.0"));
        // Firecrab's own system is still the managed Linux VM.
        assert_eq!(platform.system, "Debian GNU/Linux 13 (trixie)");
        assert_eq!(platform.kernel, "6.18.33.2-microsoft-standard-WSL2");
    }

    #[test]
    fn a_broken_descriptor_falls_back_to_the_linux_view() {
        let platform = host_platform(Some("{not json"), Some(DEBIAN), None, "x86_64");
        assert_eq!(platform.os, HostOs::Linux);
        assert_eq!(platform.kernel, "");
    }

    #[test]
    fn empty_descriptor_fields_are_reported_as_absent() {
        let descriptor = r#"{"os":"macos","name":"macOS","version":"","architecture":"arm64","virtualization":""}"#;
        let platform = host_platform(Some(descriptor), None, None, "aarch64");
        assert_eq!(platform.os, HostOs::Macos);
        assert_eq!(platform.version, None);
        assert_eq!(platform.virtualization, None);
        assert_eq!(
            platform.system, "Linux",
            "no os-release falls back to a plain name"
        );
    }

    #[test]
    fn pretty_name_accepts_quoted_and_bare_values() {
        assert_eq!(
            pretty_name("PRETTY_NAME='Alpine Linux v3.21'").as_deref(),
            Some("Alpine Linux v3.21")
        );
        assert_eq!(
            pretty_name("PRETTY_NAME=Arch Linux").as_deref(),
            Some("Arch Linux")
        );
        assert_eq!(pretty_name("NAME=\"x\"\nPRETTY_NAME=\"\""), None);
    }

    #[test]
    fn the_live_reader_always_answers() {
        let platform = read_host_platform();
        assert!(!platform.architecture.is_empty());
        assert!(!platform.system.is_empty());
    }
}
