//! Opt-in QA against explicitly owned resources. Not an automated coverage input.
use super::*;
#[test]
#[ignore = "Requires the explicitly owned qa-github-auth VM; never authenticates an account or launches a model"]
fn real_vm_reuses_repository_without_resetting_files() {
    let marker: Value =
        serde_json::from_slice(&fs::read("/private/tmp/firecrab-github-auth-qa.json").unwrap())
            .unwrap();
    assert_eq!(marker["createdBy"], "github-auth-QA");
    assert_eq!(marker["deleted"], false);
    let id: Uuid = marker["id"].as_str().unwrap().parse().unwrap();
    let client = reqwest::blocking::Client::new();
    let vm: Value = client
        .get(format!("http://127.0.0.1:31100/api/vms/{id}"))
        .send()
        .unwrap()
        .json()
        .unwrap();
    assert_eq!(vm["name"], marker["name"]);
    assert!(vm["name"].as_str().unwrap().starts_with("qa-github-auth-"));
    let home =
        PathBuf::from(std::env::var("FIRECRAB_GITHUB_QA_HOME").expect("Private QA managed home"));
    let runtime = Runtime::for_vm(&home, id, true).unwrap();
    let configured = runtime
        .setup_project(Setup {
            repository: "octocat/Hello-World".into(),
            branch: String::new(),
        })
        .unwrap();
    assert_eq!(configured["path"], WORKSPACE);
    let guest = runtime.guest.as_ref().unwrap();
    guest
        .shell(
            "printf QA_KEEP_EDIT > /workspace/firecrab/project/.firecrab-qa-edit",
            Duration::from_secs(5),
        )
        .unwrap();
    runtime
        .setup_project(Setup {
            repository: "octocat/Hello-World".into(),
            branch: String::new(),
        })
        .unwrap();
    assert_eq!(
        guest
            .shell(
                "cat /workspace/firecrab/project/.firecrab-qa-edit",
                Duration::from_secs(5)
            )
            .unwrap(),
        "QA_KEEP_EDIT"
    );
    assert!(
        runtime
            .setup_project(Setup {
                repository: "octocat/other".into(),
                branch: String::new()
            })
            .is_err()
    );
    fs::remove_file(runtime.project_record()).unwrap();
    assert_eq!(runtime.vm_project().unwrap()["state"], "ready");
    println!(
        "QA VM: repository cloned once, local edit preserved, guest binding recovered; no account authorization or model execution"
    );
}
