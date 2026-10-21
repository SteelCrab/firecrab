// Exercise the guest descriptor contract on native macOS/Windows runners too.
// The full guest API uses Linux syscalls and cannot build on those hosts.
#[path = "../../firecrab-api/src/host_platform.rs"]
mod guest_host_platform;
