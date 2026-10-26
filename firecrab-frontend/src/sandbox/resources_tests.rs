use super::*;

fn environment_editor_input() -> Editor {
    Editor {
        kind: Kind::Environment,
        id: "environment".into(),
        project: "project".into(),
        spec: json!({
            "version": 1,
            "subnetCidr": "172.29.80.0/24",
            "roles": [role(json!({"name": "pinned-image"}), 0)],
            "allow": [],
            "retention": {"ttlSeconds": null}
        }),
        choices: vec![],
        notes: BTreeMap::new(),
        loading: false,
        busy: false,
        uncertain: false,
        error: String::new(),
    }
}

#[test]
fn malformed_environment_input_renders_the_current_language_before_submission() {
    for locale in ["en", "ko"] {
        let a = App {
            locale: locale.into(),
            ..App::default()
        };
        for (key, value, en, ko) in [
            (
                "0-services",
                "health",
                "Services use name:port, one per line",
                "서비스는 이름:포트 형식으로 한 줄에 하나씩 입력한다",
            ),
            (
                "0-services",
                "health:0",
                "Service ports must be between 1 and 65535",
                "서비스 포트는 1부터 65535 사이여야 한다",
            ),
            (
                "0-services",
                "health:65536",
                "Service ports must be between 1 and 65535",
                "서비스 포트는 1부터 65535 사이여야 한다",
            ),
            (
                "sb-resource-allow",
                "debug backend",
                "Allowed connections use: source destination port",
                "허용할 연결은 출발역할 도착역할 포트 형식으로 입력한다",
            ),
            (
                "sb-resource-allow",
                "debug backend 0",
                "Allowed connection ports must be between 1 and 65535",
                "허용할 연결의 포트는 1부터 65535 사이여야 한다",
            ),
            (
                "sb-resource-allow",
                "debug backend invalid",
                "Allowed connection ports must be between 1 and 65535",
                "허용할 연결의 포트는 1부터 65535 사이여야 한다",
            ),
            (
                "sb-resource-ttl",
                "later",
                "Lifetime must be a positive number of seconds",
                "보존 시간은 양의 정수(초)로 입력한다",
            ),
        ] {
            let mut e = environment_editor_input();
            e.notes.insert(key.into(), value.into());
            let expected = if locale == "ko" { ko } else { en };
            e.error = environment_spec(&a, &e).unwrap_err();
            assert_eq!(e.error, expected, "{locale}: {key}={value}");
            let mut html = String::new();
            editor(&a, &e, &mut html);
            assert!(html.contains("role=\"alert\""));
            assert!(html.contains(expected));
            assert!(!e.busy && !e.uncertain);
        }
    }
}

#[test]
fn environment_language_changes_do_not_change_the_submitted_spec() {
    let mut e = environment_editor_input();
    e.notes
        .insert("0-services".into(), "health:8080\nmetrics:9090".into());
    e.notes.insert("0-dependsOn".into(), "db cache".into());
    e.notes.insert(
        "sb-resource-allow".into(),
        "debug backend 8080\nbackend db 5432".into(),
    );
    e.notes.insert("sb-resource-ttl".into(), "900".into());
    let mut a = App {
        locale: "en".into(),
        ..App::default()
    };
    let en = environment_spec(&a, &e).unwrap();
    a.locale = "ko".into();
    assert_eq!(environment_spec(&a, &e).unwrap(), en);
    assert_eq!(en["roles"][0]["dependsOn"], json!(["db", "cache"]));
    assert_eq!(
        en["roles"][0]["services"],
        json!([
            {"name": "health", "port": 8080, "readiness": true},
            {"name": "metrics", "port": 9090, "readiness": true}
        ])
    );
    assert_eq!(
        en["allow"],
        json!([
            {"from": "debug", "to": "backend", "port": 8080},
            {"from": "backend", "to": "db", "port": 5432}
        ])
    );
    assert_eq!(en["retention"]["ttlSeconds"], 900);
}
