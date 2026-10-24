//! Production mount entry point: the `a4aa-dsh mount` binary reads the DSH
//! credential reference and installs the ACP profile patch. The fixture
//! credentials carry placeholder values; no secret is read or printed.

use std::process::Command;

const CREDENTIALS: &str = include_str!("fixtures/dsh-command-code-mount.yaml");

fn dsh_home() -> tempfile::TempDir {
    let home = tempfile::tempdir().expect("temp dsh home");
    std::fs::write(home.path().join(".credentials.yaml"), CREDENTIALS).unwrap();
    home
}

fn mount(home: &std::path::Path, extra: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_a4aa-dsh"))
        .arg("mount")
        .arg("--home")
        .arg(home)
        .args([
            "--model",
            "gpt-5.4=GPT-5.4",
            "--model",
            "gpt-5.4-mini=GPT-5.4 Mini",
        ])
        .args(extra)
        .output()
        .expect("run a4aa-dsh mount")
}

#[test]
fn mount_binary_installs_the_patch_and_is_idempotent() {
    let home = dsh_home();
    let first = mount(home.path(), &[]);
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let stdout = String::from_utf8_lossy(&first.stdout);
    assert!(stdout.contains("mounted"), "{stdout}");
    let patch = home.path().join("profiles/acp/cordis.patch.yml");
    let installed = std::fs::read_to_string(&patch).expect("installed patch");
    assert!(installed.contains("@deepseek-ai/dsh-llm-pi-ai"));
    assert!(installed.contains("apiKeyEnv: COMMAND_CODE_API_KEY"));
    assert!(installed.contains("id: gpt-5.4"));
    assert!(installed.contains("id: gpt-5.4-mini"));
    assert!(!installed.contains("fixture"));

    let second = mount(home.path(), &[]);
    assert!(second.status.success());
    assert!(
        String::from_utf8_lossy(&second.stdout).contains("already mounted"),
        "{}",
        String::from_utf8_lossy(&second.stdout)
    );
}

#[test]
fn mount_binary_refuses_an_existing_different_patch_without_replace() {
    let home = dsh_home();
    let patch = home.path().join("profiles/acp/cordis.patch.yml");
    std::fs::create_dir_all(patch.parent().unwrap()).unwrap();
    std::fs::write(&patch, "- id: other-entry\n").unwrap();
    let refused = mount(home.path(), &[]);
    assert!(!refused.status.success());
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("--replace"),
        "{}",
        String::from_utf8_lossy(&refused.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(&patch).unwrap(),
        "- id: other-entry\n"
    );
    let replaced = mount(home.path(), &["--replace"]);
    assert!(
        replaced.status.success(),
        "{}",
        String::from_utf8_lossy(&replaced.stderr)
    );
    assert!(std::fs::read_to_string(&patch)
        .unwrap()
        .contains("@deepseek-ai/dsh-llm-pi-ai"));
}

#[test]
fn mount_binary_refuses_a_credentials_document_without_the_ref() {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(
        home.path().join(".credentials.yaml"),
        include_str!("fixtures/dsh-command-code-absent.yaml"),
    )
    .unwrap();
    let refused = mount(home.path(), &[]);
    assert!(!refused.status.success());
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("COMMAND_CODE_API_KEY"),
        "{}",
        String::from_utf8_lossy(&refused.stderr)
    );
    assert!(!home.path().join("profiles/acp/cordis.patch.yml").exists());
}
