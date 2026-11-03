//! Skill discovery through the skills.sh registry used by the Vercel Skills CLI.
//! Nothing here knows about MicroVMs, Herdr or agent sessions.
#[cfg(test)]
pub(crate) mod http_fixture;
pub mod registry;
