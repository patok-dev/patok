use std::process::Command;

#[test]
fn tasks_add_prints_the_task_and_generated_id_and_exits_zero() {
    let dir = tempfile::tempdir().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_patok"))
        .args([
            "-d",
            dir.path().to_str().unwrap(),
            "tasks",
            "add",
            "sample task text",
        ])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    // A fresh store: the generated id is T1.1, and both it and the task text
    // appear in the confirmation.
    assert!(stdout.contains("T1.1"), "stdout: {stdout}");
    assert!(stdout.contains("sample task text"), "stdout: {stdout}");
    assert_eq!(
        std::fs::read_to_string(dir.path().join("TASKS.md")).unwrap(),
        "- [ ] T1.1: sample task text\n"
    );
}

#[test]
fn tasks_add_numbers_after_existing_tasks() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("TASKS.md"), "# T\n- [ ] T2.1: existing\n").unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_patok"))
        .args(["-d", dir.path().to_str().unwrap(), "tasks", "add", "new"])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert_eq!(
        std::fs::read_to_string(dir.path().join("TASKS.md")).unwrap(),
        "# T\n- [ ] T2.1: existing\n- [ ] T3.1: new\n"
    );
}

#[test]
fn tasks_add_fails_on_an_invalid_store_path() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("missing");
    let out = Command::new(env!("CARGO_BIN_EXE_patok"))
        .args(["-d", missing.to_str().unwrap(), "tasks", "add", "x"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.starts_with("patok: "), "stderr: {stderr}");
    assert!(!stderr.trim().is_empty());
    assert!(!missing.join("TASKS.md").exists());
}
