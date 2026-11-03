use super::super::http_fixture::Fixture;
use super::*;

fn registry(fixture: &Fixture) -> Registry {
    Registry {
        base: fixture.url.clone(),
    }
}

const RESULT: &str = r#"{"query":"playwright","count":2,"skills":[
 {"id":"microsoft/playwright-cli/playwright-cli","source":"microsoft/playwright-cli","skillId":"playwright-cli","name":"playwright-cli","installs":206087},
 {"id":"anthropics/skills/webapp-testing","source":"anthropics/skills","skillId":"webapp-testing","name":"webapp-testing","installs":196046}
]}"#;

#[test]
fn search_returns_registry_skills_with_install_counts() {
    let fixture = Fixture::start();
    fixture.set("/api/search?q=playwright&limit=50", 200, RESULT);
    let value = registry(&fixture).search("playwright").unwrap();
    assert_eq!(value["query"], "playwright");
    assert_eq!(value["count"], 2);
    assert_eq!(
        value["skills"][0],
        json!({"name":"playwright-cli","skill":"playwright-cli","source":"microsoft/playwright-cli","installs":206087})
    );
    assert_eq!(value["skills"][1]["source"], "anthropics/skills");
}

#[test]
fn search_encodes_the_query() {
    let fixture = Fixture::start();
    fixture.set(
        "/api/search?q=react+native&limit=50",
        200,
        r#"{"skills":[]}"#,
    );
    let value = registry(&fixture).search("  react native ").unwrap();
    assert_eq!(value["count"], 0);
    assert_eq!(fixture.calls(), ["/api/search?q=react+native&limit=50"]);
}

#[test]
fn search_rejects_short_or_long_queries_without_a_request() {
    let fixture = Fixture::start();
    assert!(registry(&fixture).search(" a ").is_err());
    assert!(registry(&fixture).search(&"x".repeat(101)).is_err());
    assert!(fixture.calls().is_empty());
}

#[test]
fn search_drops_entries_that_cannot_be_installed_safely() {
    let fixture = Fixture::start();
    fixture.set(
        "/api/search?q=evil&limit=50",
        200,
        r#"{"skills":[
          {"source":"owner/repo; rm -rf /","skillId":"x","name":"x","installs":1},
          {"source":"https://example.com/x","skillId":"x","name":"x","installs":1},
          {"source":"owner/repo","skillId":"-rf","name":"-rf","installs":1},
          {"source":"owner/repo","skillId":"ok-skill","name":"ok\u001b[31mskill","installs":3}
        ]}"#,
    );
    let value = registry(&fixture).search("evil").unwrap();
    assert_eq!(value["count"], 1);
    assert_eq!(value["skills"][0]["skill"], "ok-skill");
    assert_eq!(value["skills"][0]["name"], "ok[31mskill");
}

#[test]
fn registry_failure_has_a_clear_error() {
    let fixture = Fixture::start();
    fixture.set("/api/search?q=react&limit=50", 503, "{}");
    let error = registry(&fixture).search("react").err().unwrap();
    assert!(
        error
            .to_string()
            .contains("skills.sh search failed with HTTP 503"),
        "{error}"
    );
}

#[test]
fn source_and_skill_validation() {
    assert!(valid_source("vercel-labs/agent-skills"));
    assert!(valid_source("a.b_c/d-e.f"));
    for bad in [
        "",
        "owner",
        "owner/",
        "/repo",
        "-x/repo",
        "a/b/c",
        "a/b c",
        "a/b;c",
        "https://x/y",
    ] {
        assert!(!valid_source(bad), "{bad}");
    }
    assert!(valid_skill("web-design-guidelines"));
    assert!(valid_skill("Convex Best Practices"));
    for bad in ["", "-rf", " lead", "a\nb", "a;b", "a$b", &"x".repeat(129)] {
        assert!(!valid_skill(bad), "{bad:?}");
    }
}
