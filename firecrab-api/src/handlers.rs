pub mod bootstrap;
pub mod builder_vm;
pub mod console;
pub mod console_sentinel;
pub mod images;
#[cfg(test)]
#[allow(clippy::err_expect)]
mod images_local_tests;
pub mod info;
pub mod kernels;
pub mod micro_networks;
pub mod micro_services;
pub mod micro_storages;
pub mod microregistry;
pub mod network;
pub mod oci;
pub mod registries;
pub mod secrets;
pub mod shells;
pub mod storage;
pub mod update;
pub mod vms;

pub mod cloudflare;
pub mod pipe;
/// Provider authentication endpoints shared by supported Pipe services.
pub mod pipe_auth;
pub mod pipe_databases;
