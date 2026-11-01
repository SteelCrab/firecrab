use super::*;

pub(super) fn preferred(account: &Account) -> Option<&'static str> {
    match (account.provider.as_str(), account.authentication.as_str()) {
        ("openai", "api_key") => Some("codex"),
        ("anthropic", "api_key") => Some("claude_code"),
        _ => None,
    }
}
pub(super) fn agent(s: &mut Setup) {
    if s.pending.is_some() || s.agent_changed || s.spec.agent.is_some() {
        return;
    }
    let mut eligible = s
        .accounts
        .iter()
        .filter_map(|a| preferred(a).map(|agent| (agent, a)));
    if let Some((agent, account)) = eligible.next()
        && eligible.next().is_none()
    {
        s.spec.agent = Some(Agent {
            agent: agent.into(),
            connection_id: account.id.clone(),
        });
    }
}
pub(super) fn account(s: &Setup, agent: &str) -> String {
    if let Some(current) = &s.spec.agent
        && s.accounts
            .iter()
            .any(|a| a.id == current.connection_id && compatible(agent, a))
    {
        return current.connection_id.clone();
    }
    let mut choices = s.accounts.iter().filter(|a| compatible(agent, a));
    match choices.next() {
        Some(first) if choices.next().is_none() => first.id.clone(),
        _ => String::new(),
    }
}
pub(super) fn repository(s: &mut Setup) {
    if s.pending.is_some() {
        return;
    }
    if !s.name_changed {
        let name = s
            .spec
            .github
            .repository
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || "._-".contains(*c))
            .take(64)
            .collect::<String>();
        s.spec.name = if name.bytes().any(|c| c.is_ascii_alphanumeric()) {
            name
        } else {
            "workspace".into()
        };
    }
    if !s.runtime_changed {
        let language = s
            .repos
            .iter()
            .find(|r| r.owner == s.spec.github.owner && r.repository == s.spec.github.repository)
            .and_then(|r| r.language.as_deref());
        s.spec.runtime = match language {
            Some("Python") => "python",
            Some("Rust") => "rust",
            _ => "node_js",
        }
        .into();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn setup() -> Setup {
        Setup{id:"11111111-1111-4111-8111-111111111111".into(),spec:serde_json::from_value(json!({"name":"","github":{"account":"octocat","owner":"team","repository":"web-app","branch":"main"},"runtime":"node_js","resources":{"cpu":2,"ram":4096,"diskGb":16},"agent":null})).unwrap(),github:"octocat".into(),accounts:vec![],repos:vec![Choice{owner:"team".into(),repository:"web-app".into(),default_branch:"main".into(),language:Some("Python".into())}],branches:vec!["main".into()],repo_next:None,branch_next:None,metadata_epoch:0,branch_epoch:0,loading:false,repo_loading:false,branch_loading:false,busy:false,pending:None,error:String::new(),repo_error:String::new(),ai_error:false,name_changed:false,runtime_changed:false,agent_changed:false}
    }
    fn account(id: &str, provider: &str, auth: &str) -> Account {
        Account {
            id: id.into(),
            name: "Shared account".into(),
            provider: provider.into(),
            authentication: auth.into(),
        }
    }
    #[test]
    fn repository_defaults_follow_metadata_and_preserve_manual_choices() {
        let mut s = setup();
        repository(&mut s);
        assert_eq!(s.spec.name, "web-app");
        assert_eq!(s.spec.runtime, "python");
        s.name_changed = true;
        s.runtime_changed = true;
        s.spec.name = "custom".into();
        s.spec.runtime = "rust".into();
        repository(&mut s);
        assert_eq!(s.spec.name, "custom");
        assert_eq!(s.spec.runtime, "rust");
    }
    #[test]
    fn one_runnable_account_is_reused_without_substituting_existing_assignment() {
        let mut s = setup();
        s.accounts = vec![
            account(&s.id, "openai", "api_key"),
            account("22222222-2222-4222-8222-222222222222", "openai", "chatgpt"),
        ];
        agent(&mut s);
        assert_eq!(s.spec.agent.as_ref().unwrap().agent, "codex");
        let original = s.spec.agent.clone();
        s.accounts.clear();
        s.accounts.push(account(
            "33333333-3333-4333-8333-333333333333",
            "anthropic",
            "api_key",
        ));
        agent(&mut s);
        assert!(s.spec.agent == original);
    }
    #[test]
    fn ambiguous_accounts_and_explicit_no_agent_never_pick_a_billing_account() {
        let mut s = setup();
        s.accounts = vec![
            account(&s.id, "openai", "api_key"),
            account(
                "22222222-2222-4222-8222-222222222222",
                "anthropic",
                "api_key",
            ),
        ];
        agent(&mut s);
        assert!(s.spec.agent.is_none());
        repository(&mut s);
        assert!(!can_save(&s));
        s.agent_changed = true;
        s.accounts.pop();
        agent(&mut s);
        assert!(s.spec.agent.is_none());
        assert!(can_save(&s));
    }
    #[test]
    fn failed_account_metadata_cannot_silently_create_without_ai() {
        let mut s = setup();
        repository(&mut s);
        s.ai_error = true;
        assert!(!can_save(&s));
        s.agent_changed = true;
        assert!(can_save(&s));
    }
    #[test]
    fn switching_agent_preserves_the_chosen_account_or_requires_a_choice() {
        let mut s = setup();
        s.accounts = vec![
            account(&s.id, "openai", "api_key"),
            account(
                "22222222-2222-4222-8222-222222222222",
                "anthropic",
                "api_key",
            ),
        ];
        s.spec.agent = Some(Agent {
            agent: "claude_code".into(),
            connection_id: s.accounts[1].id.clone(),
        });
        assert_eq!(super::account(&s, "opencode"), s.accounts[1].id);
        s.spec.agent = None;
        assert!(super::account(&s, "opencode").is_empty());
    }
}
