use assert_cmd::Command;
use predicates::str::contains;

#[test]
fn login_requires_app_key() {
    let mut command = Command::cargo_bin("baidupan-cli").expect("binary");
    command
        .arg("login")
        .env_remove("BAIDUPAN_APP_KEY")
        .env_remove("BAIDUPAN_APP_SECRET")
        .assert()
        .failure()
        .stderr(contains("missing environment variable BAIDUPAN_APP_KEY"));
}

#[test]
fn whoami_requires_login() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let mut command = Command::cargo_bin("baidupan-cli").expect("binary");
    command
        .arg("whoami")
        .env("HOME", temp_dir.path())
        .env("XDG_CONFIG_HOME", temp_dir.path())
        .assert()
        .failure()
        .stderr(contains("token file does not exist"));
}
