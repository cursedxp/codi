//! Integration tests for engine + config using a fake-goose stub.
//!
//! These tests run entirely offline: no real model, no Goose binary needed.
//! A small shell script acts as `goose`, capturing the env vars that codi sets
//! so we can assert the correct provider/model routing.

use std::io::Write;
use std::path::PathBuf;

use codi_core::config::{Config, RoutingConfig, RoutingMode};
use codi_core::engine::{run_session, SessionMode};

/// Write a fake `goose` shell script that echoes the relevant env vars and
/// exits 0. Returns the path to the script.
fn fake_goose(dir: &std::path::Path) -> PathBuf {
    let path = dir.join("goose");
    let mut f = std::fs::File::create(&path).unwrap();
    writeln!(
        f,
        "#!/bin/sh\necho \"fake-goose: GOOSE_MODEL=$GOOSE_MODEL GOOSE_OPENAI_HOST=$GOOSE_OPENAI_HOST\"\nexit 0"
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        f.set_permissions(std::fs::Permissions::from_mode(0o755))
            .unwrap();
    }
    path
}

fn cfg_with_goose_bin(bin: &PathBuf, mode: RoutingMode) -> Config {
    let mut cfg = Config::default();
    cfg.goose_bin = Some(bin.to_str().unwrap().to_string());
    cfg.routing = RoutingConfig { mode };
    cfg.safety.confirm_commands = false;
    cfg.safety.confirm_writes = false;
    cfg
}

#[test]
fn engine_launches_fake_goose_local_only() {
    let dir = tempfile::tempdir().unwrap();
    let goose = fake_goose(dir.path());

    let cfg = cfg_with_goose_bin(&goose, RoutingMode::LocalOnly);
    let code = run_session(
        &cfg,
        "add a hello function",
        SessionMode::OneShot("add a hello function".to_string()),
        None,
        dir.path(),
        "",
    )
    .unwrap();

    assert_eq!(code, 0, "fake-goose should exit 0");
}

#[test]
fn config_roundtrip_in_temp_dir() {
    let dir = tempfile::tempdir().unwrap();
    let goose = fake_goose(dir.path());

    // Write a codi.toml
    std::fs::write(
        dir.path().join("codi.toml"),
        r#"
[model.local]
base_url = "http://127.0.0.1:19999/v1"
model    = "test-model"
api_key  = ""

[routing]
mode = "local-only"

[commands]
test = "echo ok"
"#,
    )
    .unwrap();

    let cfg = Config::load(dir.path()).unwrap();
    assert_eq!(cfg.model.local.model, "test-model");
    assert_eq!(cfg.routing.mode, RoutingMode::LocalOnly);
    assert_eq!(cfg.commands.test.as_deref(), Some("echo ok"));

    let mut cfg = cfg;
    cfg.goose_bin = Some(goose.to_str().unwrap().to_string());

    let code = run_session(
        &cfg,
        "do something",
        SessionMode::OneShot("do something".to_string()),
        None,
        dir.path(),
        "",
    )
    .unwrap();
    assert_eq!(code, 0);
}

#[test]
fn engine_fails_gracefully_when_goose_not_found() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = Config::default();
    cfg.goose_bin = Some("/nonexistent/path/goose".to_string());

    let result = run_session(
        &cfg,
        "test",
        SessionMode::OneShot("test".to_string()),
        None,
        dir.path(),
        "",
    );
    assert!(result.is_err(), "should error when goose binary not found");
}
