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

fn run_tasks(dir: &std::path::Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_patok"))
        .args([&["-d", dir.to_str().unwrap()], args].concat())
        .output()
        .unwrap()
}

#[test]
fn tasks_list_prints_added_tasks_one_per_line_id_first() {
    let dir = tempfile::tempdir().unwrap();
    for text in ["first task", "second task", "third task"] {
        let out = run_tasks(dir.path(), &["tasks", "add", text]);
        assert!(out.status.success());
    }
    let out = run_tasks(dir.path(), &["tasks", "list"]);
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        stdout,
        "T1.1 first task\nT2.1 second task\nT3.1 third task\n"
    );
    for id in ["T1.1", "T2.1", "T3.1"] {
        assert_eq!(stdout.lines().filter(|l| l.starts_with(id)).count(), 1);
    }
}

#[test]
fn tasks_list_includes_done_tasks_and_ignores_other_lines() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("TASKS.md"),
        "# Tasks\nSome prose.\n- [ ] T1.1: a\n- [x] T1.2: b\n",
    )
    .unwrap();
    let out = run_tasks(dir.path(), &["tasks", "list"]);
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(stdout, "T1.1 a\nT1.2 b\n");
}

#[test]
fn tasks_list_shows_a_newly_added_task_immediately() {
    let dir = tempfile::tempdir().unwrap();
    let out = run_tasks(dir.path(), &["tasks", "add", "first"]);
    assert!(out.status.success());
    let first = run_tasks(dir.path(), &["tasks", "list"]);
    assert!(first.status.success());
    let first_stdout = String::from_utf8_lossy(&first.stdout);
    assert!(first_stdout.contains("T1.1 first"));
    assert!(!first_stdout.contains("T2.1"));
    let out = run_tasks(dir.path(), &["tasks", "add", "second"]);
    assert!(out.status.success());
    let second = run_tasks(dir.path(), &["tasks", "list"]);
    assert!(second.status.success());
    let second_stdout = String::from_utf8_lossy(&second.stdout);
    assert!(second_stdout.contains("T1.1 first"));
    assert!(second_stdout.contains("T2.1 second"));
}

#[test]
fn tasks_list_succeeds_with_empty_output_on_an_empty_store() {
    let dir = tempfile::tempdir().unwrap();
    let out = run_tasks(dir.path(), &["tasks", "list"]);
    assert!(out.status.success());
    assert!(out.stdout.is_empty());
    assert!(!dir.path().join("TASKS.md").exists());
}

#[test]
fn tasks_list_fails_when_the_store_cannot_be_read() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("TASKS.md")).unwrap();
    let out = run_tasks(dir.path(), &["tasks", "list"]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.starts_with("patok: "), "stderr: {stderr}");
    assert!(stderr.contains("cannot read"), "stderr: {stderr}");
}
