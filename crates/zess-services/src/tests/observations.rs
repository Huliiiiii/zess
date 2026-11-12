use zess_git::{
    SelectedPath,
    ignore::{IgnoreDestination, IgnoreRequest},
};

use super::*;

#[test]
fn linked_worktree_previews_follow_local_ignore_rules() {
    let fixture = Fixture::new();
    let repository = fixture.0.join("repository");
    let linked = fixture.0.join("linked");
    fs::create_dir(&repository).unwrap();
    git(&repository, &["init", "--quiet"]);
    git(
        &repository,
        &["commit", "--quiet", "--allow-empty", "-m", "initial"],
    );
    git(
        &repository,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            "linked",
            linked.to_str().unwrap(),
        ],
    );
    let directory = linked.join("nested");
    fs::create_dir(&directory).unwrap();
    fs::write(directory.join("local.txt"), "generated").unwrap();
    fs::write(directory.join("visible.txt"), "visible").unwrap();
    let mut services = NativeServices::new(directory).unwrap();
    let args = json!({"path":""});
    let mut preview = observe(&mut services, "fs.preview", &args);
    assert_eq!(
        names(&services.call("fs.preview", args.clone())),
        ["local.txt", "visible.txt"]
    );

    let mut ignore = services
        .start_ignore(IgnoreRequest {
            destination: IgnoreDestination::Local,
            paths: vec![SelectedPath {
                path: "local.txt".into(),
                directory: false,
            }],
        })
        .unwrap();
    let deadline = Instant::now() + TIMEOUT;
    loop {
        if let Some(result) = ignore.poll() {
            result.as_ref().unwrap();
            break;
        }
        assert!(Instant::now() < deadline, "local exclude update timed out");
        thread::sleep(Duration::from_millis(1));
    }

    wait_for_change(&mut services, &mut preview);
    assert_eq!(
        names(&services.call("fs.preview", args.clone())),
        ["visible.txt"]
    );
    let listing = services.call("fs.list", args);
    assert_eq!(names(&listing), ["local.txt", "visible.txt"]);
    assert_eq!(listing["entries"][0]["ignored"], true);
}

#[test]
fn external_ignore_rules_refresh_status_and_directory_previews() {
    const CONFIG_FIXTURE: &str = "ZESS_TEST_GLOBAL_IGNORE_DIRECTORY";

    let Some(directory) = std::env::var_os(CONFIG_FIXTURE).map(PathBuf::from) else {
        let fixture = Fixture::new();
        let config = fixture.0.join("custom.conf");
        let rules = fixture.0.join("external rules");
        fs::write(&rules, "").unwrap();
        git(
            &fixture.0,
            &[
                "config",
                "--file",
                config.to_str().unwrap(),
                "core.excludesFile",
                rules.to_str().unwrap(),
            ],
        );
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "tests::observations::external_ignore_rules_refresh_status_and_directory_previews",
                "--nocapture",
            ])
            .env(CONFIG_FIXTURE, &fixture.0)
            .env("GIT_CONFIG_GLOBAL", &config)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success() && stdout.contains("1 passed"),
            "{stdout}\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    };

    let repository = directory.join("repository");
    let nested = repository.join("nested");
    fs::create_dir_all(&nested).unwrap();
    git(&repository, &["init", "--quiet"]);
    fs::write(
        repository.join(".git/info/exclude"),
        "!/nested/local-override.txt\n",
    )
    .unwrap();
    fs::write(nested.join(".gitignore"), "!/directory-override.txt\n").unwrap();
    for file in [
        "ignored.txt",
        "local-override.txt",
        "directory-override.txt",
    ] {
        fs::write(nested.join(file), "contents").unwrap();
    }
    let mut services = NativeServices::new(nested).unwrap();
    let mut preview = observe(&mut services, "fs.preview", &json!({"path":""}));
    let mut status = observe(&mut services, "git.status", &json!({}));
    assert_eq!(
        names(&services.call("fs.preview", json!({"path":""}))),
        [
            ".gitignore",
            "directory-override.txt",
            "ignored.txt",
            "local-override.txt"
        ]
    );

    fs::write(
        directory.join("external rules"),
        b"/nested/ignored.txt\n/nested/local-override.txt\n/nested/directory-override.txt\n# Non-UTF-8 comment: \xff\n",
    )
    .unwrap();
    wait_for_change(&mut services, &mut preview);
    wait_for_change(&mut services, &mut status);
    let expected = [".gitignore", "directory-override.txt", "local-override.txt"];
    assert_eq!(
        names(&services.call("fs.preview", json!({"path":""}))),
        expected
    );
    let result = services.call("git.status", json!({}));
    let paths = result["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["path"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(paths, expected);
}

#[test]
fn git_history_refreshes_on_commits_without_refreshing_on_index_only_changes() {
    let fixture = Fixture::new();
    git(&fixture.0, &["init", "--quiet"]);
    fs::write(fixture.0.join("note.txt"), "tracked").unwrap();
    git(&fixture.0, &["add", "note.txt"]);
    git(&fixture.0, &["commit", "--quiet", "-m", "initial"]);
    let mut services = fixture.services();
    let mut log = observe(&mut services, "git.log", &json!({}));
    let mut status = observe(&mut services, "git.status", &json!({}));

    git(&fixture.0, &["update-index", "--force-remove", "note.txt"]);
    wait_for_change(&mut services, &mut status);
    let deadline = Instant::now() + Duration::from_millis(150);
    while Instant::now() < deadline {
        services.poll_changes().unwrap();
        assert!(!log.changed());
        thread::sleep(Duration::from_millis(5));
    }

    git(&fixture.0, &["commit", "--quiet", "-m", "remove note"]);
    wait_for_change(&mut services, &mut log);
    let result = services.call("git.log", json!({}));
    assert_eq!(result["commits"].as_array().unwrap().len(), 2, "{result}");
}

#[cfg(unix)]
#[test]
fn a_live_preview_observation_recovers_from_a_broken_symbolic_link() {
    let fixture = Fixture::new();
    let path = fixture.0.join("note.txt");
    let target = fixture.0.join("target.txt");
    fs::write(&target, "original").unwrap();
    std::os::unix::fs::symlink("target.txt", &path).unwrap();
    let mut services = fixture.services();
    let args = json!({"path":"note.txt"});
    let mut preview = observe(&mut services, "fs.preview", &args);
    assert_eq!(
        services.call("fs.preview", args.clone())["text"],
        "original"
    );

    fs::remove_file(&target).unwrap();
    wait_for_change(&mut services, &mut preview);
    let failed = start(&mut services, "fs.preview", args.clone());
    let deadline = Instant::now() + TIMEOUT;
    let error = loop {
        if let Err(error) = services.poll_changes() {
            break error.to_string();
        }
        assert!(Instant::now() < deadline, "broken link was not reported");
        thread::sleep(Duration::from_millis(1));
    };
    assert_eq!(error, "broken symbolic link in workspace path");

    let replacement = fixture.0.join("replacement.txt");
    fs::write(&replacement, "recovered").unwrap();
    fs::rename(&replacement, &path).unwrap();
    wait_for_change(&mut services, &mut preview);
    assert_eq!(services.call("fs.preview", args)["text"], "recovered");
    match services.poll_query(failed).unwrap() {
        crate::QueryPoll::Ready(Err(failure)) => assert_eq!(failure, error),
        _ => panic!("the original preview did not keep its broken-link error"),
    }
}
