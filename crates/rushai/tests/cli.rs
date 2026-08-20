use std::fs;

use assert_cmd::Command;
use rushai_core::store::Store;

fn rush() -> Command {
    Command::cargo_bin("rush").unwrap()
}

#[test]
fn config_resolves_and_redacts_api_keys() {
    let tmp = tempfile::tempdir().unwrap();
    fs::write(
        tmp.path().join("rushai.json"),
        r#"{"theme":"dark","providers":{"anthropic":{"api_key":"sk-secret"}}}"#,
    )
    .unwrap();
    let assert = rush()
        .current_dir(tmp.path())
        .env("XDG_CONFIG_HOME", tmp.path().join("xdg"))
        .arg("config")
        .assert()
        .success();
    let out = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(out.contains(r#""theme": "dark""#), "{out}");
    assert!(out.contains("[redacted]"), "{out}");
    assert!(!out.contains("sk-secret"), "api key leaked: {out}");
}

#[test]
fn config_schema_prints_schema() {
    let assert = rush().args(["config", "schema"]).assert().success();
    let out = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let schema: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert!(schema["properties"]["providers"].is_object(), "{out}");
}

#[test]
fn sessions_reads_the_store() {
    let tmp = tempfile::tempdir().unwrap();

    let empty = rush()
        .env("RUSHAI_DATA_DIR", tmp.path())
        .arg("sessions")
        .assert()
        .success();
    assert_eq!(
        String::from_utf8(empty.get_output().stdout.clone()).unwrap(),
        "no sessions\n"
    );

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let store = Store::open(tmp.path().join("rushai.db")).unwrap();
        store
            .create_session("hello rushai".into(), None)
            .await
            .unwrap();
    });

    let listed = rush()
        .env("RUSHAI_DATA_DIR", tmp.path())
        .arg("sessions")
        .assert()
        .success();
    let out = String::from_utf8(listed.get_output().stdout.clone()).unwrap();
    assert!(out.contains("hello rushai"), "{out}");
}
