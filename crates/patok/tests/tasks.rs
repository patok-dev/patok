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

#[test]
fn tasks_move_prints_the_renumbered_ids_and_exits_zero() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("TASKS.md"),
        "## P1\n- [ ] T1.1: a\n- [ ] T1.2: b\n## P2\n- [ ] T2.1: c\n- [ ] T2.2: d\n",
    )
    .unwrap();
    let out = run_tasks(dir.path(), &["tasks", "move", "T2.1", "up"]);
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "Moved task T2.1 up.\nT1.1 a\nT1.2 c\nT1.3 b\nT2.1 d\n"
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("TASKS.md")).unwrap(),
        "## P1\n- [ ] T1.1: a\n- [ ] T1.2: c\n- [ ] T1.3: b\n## P2\n- [ ] T2.1: d\n"
    );
}

#[test]
fn tasks_move_excludes_completed_tasks_from_the_output() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("TASKS.md"),
        "- [x] T1.1: done\n- [ ] T1.2: a\n- [ ] T1.3: b\n",
    )
    .unwrap();
    let out = run_tasks(dir.path(), &["tasks", "move", "T1.3", "up"]);
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "Moved task T1.3 up.\nT1.2 b\nT1.3 a\n"
    );
}

#[test]
fn tasks_move_fails_without_a_neighbour() {
    let dir = tempfile::tempdir().unwrap();
    let text = "- [ ] T1.1: a\n- [ ] T1.2: b\n";
    std::fs::write(dir.path().join("TASKS.md"), text).unwrap();
    let out = run_tasks(dir.path(), &["tasks", "move", "T1.2", "down"]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.starts_with("patok: "), "stderr: {stderr}");
    assert!(
        stderr.contains("has no task below it to swap with"),
        "stderr: {stderr}"
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("TASKS.md")).unwrap(),
        text
    );
}

const TWO_GROUPS: &str =
    "## P1\n- [ ] T1.1: a\n- [ ] T1.2: b\n## P2\n- [ ] T2.1: c\n- [ ] T2.2: d\n";

#[test]
fn tasks_indent_prints_the_renumbered_ids_and_exits_zero() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("TASKS.md"), TWO_GROUPS).unwrap();
    let out = run_tasks(dir.path(), &["tasks", "indent", "T2.1"]);
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "Indented task T2.1.\nT1.1 a\nT1.2 b\nT1.3 c\nT2.1 d\n"
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("TASKS.md")).unwrap(),
        "## P1\n- [ ] T1.1: a\n- [ ] T1.2: b\n- [ ] T1.3: c\n## P2\n- [ ] T2.1: d\n"
    );
}

#[test]
fn tasks_indent_fails_without_a_previous_group() {
    let dir = tempfile::tempdir().unwrap();
    let text = "- [ ] T1.1: a\n- [ ] T1.2: b\n";
    std::fs::write(dir.path().join("TASKS.md"), text).unwrap();
    let out = run_tasks(dir.path(), &["tasks", "indent", "T1.2"]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.starts_with("patok: "), "stderr: {stderr}");
    assert!(
        stderr.contains("has no previous group to join"),
        "stderr: {stderr}"
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("TASKS.md")).unwrap(),
        text
    );
}

#[test]
fn tasks_outdent_prints_the_renumbered_ids_and_exits_zero() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("TASKS.md"), TWO_GROUPS).unwrap();
    let out = run_tasks(dir.path(), &["tasks", "outdent", "T1.2"]);
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "Outdented task T1.2.\nT1.1 a\nT2.1 b\nT3.1 c\nT3.2 d\n"
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("TASKS.md")).unwrap(),
        "## P1\n- [ ] T1.1: a\n- [ ] T2.1: b\n## P2\n- [ ] T3.1: c\n- [ ] T3.2: d\n"
    );
}

#[test]
fn tasks_outdent_excludes_completed_tasks_from_the_output() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("TASKS.md"),
        "- [x] T1.1: done\n- [ ] T1.2: a\n- [ ] T1.3: b\n",
    )
    .unwrap();
    let out = run_tasks(dir.path(), &["tasks", "outdent", "T1.3"]);
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "Outdented task T1.3.\nT1.2 a\nT2.1 b\n"
    );
}

#[test]
fn tasks_outdent_fails_on_the_first_task_of_a_group() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("TASKS.md"), TWO_GROUPS).unwrap();
    let out = run_tasks(dir.path(), &["tasks", "outdent", "T1.1"]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.starts_with("patok: "), "stderr: {stderr}");
    assert!(stderr.contains("nothing to split off"), "stderr: {stderr}");
    assert_eq!(
        std::fs::read_to_string(dir.path().join("TASKS.md")).unwrap(),
        TWO_GROUPS
    );
}

#[test]
fn tasks_move_rejects_an_invalid_direction() {
    let dir = tempfile::tempdir().unwrap();
    let text = "- [ ] T1.1: a\n- [ ] T1.2: b\n";
    std::fs::write(dir.path().join("TASKS.md"), text).unwrap();
    let out = run_tasks(dir.path(), &["tasks", "move", "T1.1", "sideways"]);
    assert!(!out.status.success());
    assert_eq!(
        std::fs::read_to_string(dir.path().join("TASKS.md")).unwrap(),
        text
    );
}
