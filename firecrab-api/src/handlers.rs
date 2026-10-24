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
pub mod micromanager;
pub mod microregistry;
pub mod network;
pub mod oci;
pub mod pools;
pub mod registries;
pub mod schedules;
pub mod secrets;
pub mod security_groups;
pub mod shells;
pub mod stack_applies;
pub mod stacks;
pub mod storage;
pub mod update;
pub mod vm_execution;
pub mod vms;

pub mod cloudflare;
pub mod pipe;
pub mod pipe_ai;
/// Provider authentication endpoints shared by supported Pipe services.
pub mod pipe_auth;
pub mod pipe_databases;
pub mod pipe_storage;
