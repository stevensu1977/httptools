//! Configuration checks use --dry-run, so they require no network or credentials.
use std::{
    path::PathBuf,
    process::{Command, Output},
};

struct Fixture {
    directory: PathBuf,
}

impl Fixture {
    fn new(dotenv: &str) -> Self {
        let directory =
            std::env::temp_dir().join(format!("batchbench-config-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        std::fs::write(directory.join(".env"), dotenv).unwrap();
        Self { directory }
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_batchbench"));
        command
            .current_dir(&self.directory)
            .env_remove("ACCESS_TOKEN")
            .env_remove("ENDPOINT")
            .env_remove("METHOD")
            .args(["--dry-run", "--batch-size", "1"]);
        command
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

fn assert_preview(output: Output) {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let body: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(body["batch"].as_array().unwrap().len(), 1);
}

#[test]
fn defaults_and_supported_methods_work_without_environment_configuration() {
    let fixture = Fixture::new("");
    assert_preview(fixture.command().output().unwrap());
    for method in ["GET", "post", "PUT", "pAtCh", "DELETE", "HEAD", "OPTIONS"] {
        assert_preview(
            fixture
                .command()
                .args(["--method", method])
                .output()
                .unwrap(),
        );
    }
}

#[test]
fn dotenv_endpoint_and_method_are_validated_before_any_network_activity() {
    let valid = Fixture::new("ENDPOINT=http://127.0.0.1:1/batch\nMETHOD=patch\n");
    assert_preview(valid.command().output().unwrap());

    let bad_endpoint = Fixture::new("ENDPOINT=not-a-url\nMETHOD=POST\n");
    assert_eq!(
        bad_endpoint.command().output().unwrap().status.code(),
        Some(2)
    );

    let bad_method = Fixture::new("ENDPOINT=http://127.0.0.1:1/batch\nMETHOD=NOT_A_METHOD\n");
    let output = bad_method.command().output().unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("--method"));
}

#[test]
fn exported_variables_override_dotenv_values() {
    let fixture = Fixture::new("ENDPOINT=bad-file-url\nMETHOD=bad-file-method\n");
    assert_preview(
        fixture
            .command()
            .env("ENDPOINT", "https://example.com/from-shell")
            .env("METHOD", "PUT")
            .output()
            .unwrap(),
    );
    // Overriding only one field must leave the other invalid .env value in force.
    assert_eq!(
        fixture
            .command()
            .env("ENDPOINT", "https://example.com")
            .output()
            .unwrap()
            .status
            .code(),
        Some(2)
    );
    assert_eq!(
        fixture
            .command()
            .env("METHOD", "POST")
            .output()
            .unwrap()
            .status
            .code(),
        Some(2)
    );
}

#[test]
fn endpoint_and_url_flags_override_environment_values() {
    let fixture = Fixture::new("ENDPOINT=bad-file-url\nMETHOD=bad-file-method\n");
    for endpoint_flag in ["--endpoint", "--url"] {
        assert_preview(
            fixture
                .command()
                .env("ENDPOINT", "bad-shell-url")
                .env("METHOD", "bad-shell-method")
                .args([
                    endpoint_flag,
                    "https://example.com/from-cli",
                    "--method",
                    "pAtCh",
                ])
                .output()
                .unwrap(),
        );
    }
}

#[test]
fn invalid_flags_do_not_fall_back_to_valid_environment_values() {
    let fixture = Fixture::new("ENDPOINT=https://example.com\nMETHOD=POST\n");
    assert_eq!(
        fixture
            .command()
            .args(["--endpoint", "invalid-url"])
            .output()
            .unwrap()
            .status
            .code(),
        Some(2)
    );
    assert_eq!(
        fixture
            .command()
            .args(["--method", "NOT_A_METHOD"])
            .output()
            .unwrap()
            .status
            .code(),
        Some(2)
    );
    assert_eq!(
        fixture
            .command()
            .env("METHOD", "NOT_A_METHOD")
            .output()
            .unwrap()
            .status
            .code(),
        Some(2)
    );
    assert_eq!(
        fixture
            .command()
            .env("ENDPOINT", "invalid-url")
            .output()
            .unwrap()
            .status
            .code(),
        Some(2)
    );
}
