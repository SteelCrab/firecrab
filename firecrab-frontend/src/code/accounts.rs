//! Shared public account choices. Eligibility does not prove agent authentication.
use super::*;
use crate::model::escape;

#[derive(Deserialize)]
pub(super) struct Account {
    pub id: String,
    pub name: String,
    pub provider: String,
    pub authentication: String,
}
pub(super) fn compatible(agent: &str, account: &Account) -> bool {
    matches!(
        (
            agent,
            account.provider.as_str(),
            account.authentication.as_str()
        ),
        ("codex", "openai", "api_key" | "chatgpt")
            | ("claude_code" | "opencode", "anthropic", "api_key")
            | ("opencode", "openai", "api_key")
    ) && uuid(&account.id)
}
pub(super) fn parse(value: Value) -> Option<Vec<Account>> {
    let accounts: Vec<Account> = serde_json::from_value(value).ok()?;
    let mut ids = std::collections::BTreeSet::new();
    accounts
        .iter()
        .all(|a| uuid(&a.id) && ids.insert(&a.id))
        .then_some(accounts)
}
pub(super) fn option(value: &str, label: &str, selected: &str) -> String {
    format!(
        "<option value=\"{}\" {}>{}</option>",
        escape(value),
        if value == selected { "selected" } else { "" },
        escape(label)
    )
}
pub(super) fn choices(app: &App, accounts: &[Account], binding: &Agent) -> String {
    let mut out = option(
        "",
        tr(app, "Select a compatible account", "호환 계정 선택"),
        &binding.connection_id,
    );
    for a in accounts.iter().filter(|a| compatible(&binding.agent, a)) {
        let method = match (a.provider.as_str(), a.authentication.as_str()) {
            (_, "chatgpt") => "ChatGPT",
            ("anthropic", _) => "Anthropic API key",
            _ => "OpenAI API key",
        };
        out.push_str(&option(
            &a.id,
            &format!("{} · {method}", a.name),
            &binding.connection_id,
        ));
    }
    out
}
