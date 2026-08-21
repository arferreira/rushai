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

#[test]
fn exec_streams_a_fake_script() {
    let tmp = tempfile::tempdir().unwrap();
    let script = tmp.path().join("script.json");
    fs::write(
        &script,
        r#"[
          {"kind":"event","type":"text_delta","data":"hello "},
          {"kind":"event","type":"text_delta","data":"world"},
          {"kind":"event","type":"usage","data":{"input":3,"output":2,"cache_read":0,"cache_write":0}},
          {"kind":"event","type":"done","data":{"stop":"end_turn"}}
        ]"#,
    )
    .unwrap();
    let assert = rush()
        .env("RUSHAI_DATA_DIR", tmp.path())
        .args(["exec", "-p", "hi"])
        .arg("--fake-provider")
        .arg(&script)
        .assert()
        .success();
    let out = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(out.contains("hello world"), "{out}");
    let err = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    assert!(err.contains("3 in, 2 out"), "{err}");
}

#[test]
fn exec_fault_script_fails() {
    let tmp = tempfile::tempdir().unwrap();
    let script = tmp.path().join("script.json");
    fs::write(
        &script,
        r#"[
          {"kind":"event","type":"text_delta","data":"partial"},
          {"kind":"fault","message":"connection reset"}
        ]"#,
    )
    .unwrap();
    rush()
        .env("RUSHAI_DATA_DIR", tmp.path())
        .args(["exec", "-p", "hi"])
        .arg("--fake-provider")
        .arg(&script)
        .assert()
        .failure()
        .stderr(predicates::str::contains("connection reset"));
}

#[test]
fn exec_without_key_says_how_to_set_one() {
    let tmp = tempfile::tempdir().unwrap();
    rush()
        .current_dir(tmp.path())
        .env("RUSHAI_DATA_DIR", tmp.path().join("data"))
        .env("XDG_CONFIG_HOME", tmp.path().join("xdg"))
        .env_remove("ANTHROPIC_API_KEY")
        .args(["exec", "-p", "hi"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("ANTHROPIC_API_KEY"));
}
