use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use rushai_config::{Config, ConfigError, Loader};

fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

fn loader(cwd: &Path, user_dir: Option<&Path>) -> Loader {
    Loader {
        cwd: cwd.to_path_buf(),
        user_config_dir: user_dir.map(Path::to_path_buf),
        env: BTreeMap::new(),
    }
}

#[test]
fn precedence_table() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("repo");
    let user = tmp.path().join("user");
    fs::create_dir_all(root.join(".git")).unwrap();

    write(
        &user.join("rushai.json"),
        r#"{"theme":"user","model":"anthropic/claude"}"#,
    );
    write(
        &root.join("rushai.json"),
        r#"{"theme":"root-plain","yolo":true}"#,
    );
    write(&root.join(".rushai.json"), r#"{"theme":"root-dot"}"#);
    write(&root.join("sub/rushai.json"), r#"{"theme":"sub"}"#);

    // Nearest directory wins; unset fields fall through to lower layers.
    let config = loader(&root.join("sub"), Some(&user)).load().unwrap();
    assert_eq!(config.theme.as_deref(), Some("sub"));
    assert_eq!(config.model.as_deref(), Some("anthropic/claude"));
    assert!(config.yolo);

    // Within a directory, the dotfile wins over the plain file.
    let config = loader(&root, Some(&user)).load().unwrap();
    assert_eq!(config.theme.as_deref(), Some("root-dot"));

    // Without a user config the project layers still resolve.
    let config = loader(&root.join("sub"), None).load().unwrap();
    assert_eq!(config.theme.as_deref(), Some("sub"));
    assert_eq!(config.model, None);
}

#[test]
fn no_files_yields_default() {
    let tmp = tempfile::tempdir().unwrap();
    let config = loader(tmp.path(), None).load().unwrap();
    assert_eq!(config, Config::default());
}

#[test]
fn env_automap_overrides_files() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        &tmp.path().join("rushai.json"),
        r#"{"providers":{"anthropic":{"api_key":"from-file"}}}"#,
    );
    let mut l = loader(tmp.path(), None);
    l.env.insert(
        "RUSHAI_PROVIDERS__ANTHROPIC__API_KEY".into(),
        "from-env".into(),
    );
    l.env.insert("RUSHAI_YOLO".into(), "true".into());
    let config = l.load().unwrap();
    assert_eq!(
        config.providers["anthropic"].api_key.as_deref(),
        Some("from-env")
    );
    assert!(config.yolo);
}

#[test]
fn strings_expand_env_references() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        &tmp.path().join("rushai.json"),
        r#"{"providers":{"openai":{"api_key":"${OPENAI_KEY}"}}}"#,
    );
    let mut l = loader(tmp.path(), None);
    l.env.insert("OPENAI_KEY".into(), "sk-test".into());
    let config = l.load().unwrap();
    assert_eq!(
        config.providers["openai"].api_key.as_deref(),
        Some("sk-test")
    );
}

#[test]
fn missing_env_reference_fails() {
    let tmp = tempfile::tempdir().unwrap();
    write(&tmp.path().join("rushai.json"), r#"{"theme":"${NOPE}"}"#);
    let err = loader(tmp.path(), None).load().unwrap_err();
    assert!(matches!(err, ConfigError::MissingEnvVar(name) if name == "NOPE"));
}

#[test]
fn unknown_field_fails_with_path() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        &tmp.path().join("rushai.json"),
        r#"{"providers":{"anthropic":{"api_kee":"x"}}}"#,
    );
    let err = loader(tmp.path(), None).load().unwrap_err();
    let text = err.to_string();
    assert!(text.contains("api_kee"), "unhelpful error: {text}");
    assert!(text.contains("providers.anthropic"), "no path in: {text}");
}

#[test]
fn invalid_json_names_the_file() {
    let tmp = tempfile::tempdir().unwrap();
    write(&tmp.path().join("rushai.json"), "{not json");
    let err = loader(tmp.path(), None).load().unwrap_err();
    assert!(err.to_string().contains("rushai.json"));
}

#[test]
fn reserved_env_vars_are_not_config_fields() {
    let tmp = tempfile::tempdir().unwrap();
    let mut l = loader(tmp.path(), None);
    l.env.insert("RUSHAI_LOG".into(), "debug".into());
    l.env.insert("RUSHAI_DATA_DIR".into(), "/tmp/x".into());
    assert!(l.load().is_ok());
}
