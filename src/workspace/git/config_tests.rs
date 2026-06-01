use super::config::*;
use crate::workspace::git::{
    discovery::git_worktree_info,
    status::git_status_fingerprint,
    test_support::{temp_test_dir, write_fake_tracked_repo},
};

#[test]
fn git_status_fingerprint_honors_remote_fetch_refspec() {
    let root = temp_test_dir("custom-fetch-refspec");
    write_fake_tracked_repo(&root);
    std::fs::remove_dir_all(root.join(".git/refs/remotes/origin")).unwrap();
    std::fs::create_dir_all(root.join(".git/refs/remotes/upstream")).unwrap();
    std::fs::write(
        root.join(".git/refs/remotes/upstream/main"),
        "2222222222222222222222222222222222222222\n",
    )
    .unwrap();
    std::fs::write(
            root.join(".git/config"),
            "[remote \"origin\"]\n\tfetch = +refs/heads/*:refs/remotes/upstream/*\n[branch \"main\"]\n\tremote = origin\n\tmerge = refs/heads/main\n",
        )
        .unwrap();

    let fingerprint = git_status_fingerprint(&root).unwrap();

    let upstream = fingerprint.upstream.unwrap();
    assert_eq!(upstream.full_ref, "refs/remotes/upstream/main");

    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn git_status_fingerprint_reads_included_config() {
    let root = temp_test_dir("included-config");
    write_fake_tracked_repo(&root);
    std::fs::remove_dir_all(root.join(".git/refs/remotes/origin")).unwrap();
    std::fs::create_dir_all(root.join(".git/refs/remotes/included")).unwrap();
    std::fs::write(
        root.join(".git/refs/remotes/included/main"),
        "2222222222222222222222222222222222222222\n",
    )
    .unwrap();
    std::fs::write(
        root.join(".git/config"),
        "[include]\n\tpath = included.cfg\n",
    )
    .unwrap();
    std::fs::write(
            root.join(".git/included.cfg"),
            "[remote \"included\"]\n\tfetch = +refs/heads/*:refs/remotes/included/*\n[branch \"main\"]\n\tremote = included\n\tmerge = refs/heads/main\n",
        )
        .unwrap();

    let fingerprint = git_status_fingerprint(&root).unwrap();

    let upstream = fingerprint.upstream.unwrap();
    assert_eq!(upstream.remote, "included");
    assert_eq!(upstream.full_ref, "refs/remotes/included/main");

    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn git_status_branch_config_reads_user_config_before_repo_config() {
    let root = temp_test_dir("user-config");
    write_fake_tracked_repo(&root);
    let user_config = root.join("user.gitconfig");
    std::fs::write(root.join(".git/config"), "").unwrap();
    std::fs::write(
            &user_config,
            "[remote \"global\"]\n\tfetch = +refs/heads/*:refs/remotes/global/*\n[branch \"main\"]\n\tremote = global\n\tmerge = refs/heads/main\n",
        )
        .unwrap();

    let info = git_worktree_info(&root).unwrap();
    let config = read_branch_config_with_user_paths(&info, "main", vec![user_config]).unwrap();

    assert_eq!(config.remote, "global");
    assert_eq!(
        upstream_full_ref(&config).as_deref(),
        Some("refs/remotes/global/main")
    );

    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn git_status_branch_config_repo_config_overrides_user_config() {
    let root = temp_test_dir("repo-overrides-user-config");
    write_fake_tracked_repo(&root);
    let user_config = root.join("user.gitconfig");
    std::fs::write(
            &user_config,
            "[remote \"global\"]\n\tfetch = +refs/heads/*:refs/remotes/global/*\n[branch \"main\"]\n\tremote = global\n\tmerge = refs/heads/main\n",
        )
        .unwrap();

    let info = git_worktree_info(&root).unwrap();
    let config = read_branch_config_with_user_paths(&info, "main", vec![user_config]).unwrap();

    assert_eq!(config.remote, "origin");
    assert_eq!(
        upstream_full_ref(&config).as_deref(),
        Some("refs/remotes/origin/main")
    );

    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn git_status_fingerprint_applies_repeated_includes_in_order() {
    let root = temp_test_dir("repeated-include");
    write_fake_tracked_repo(&root);
    std::fs::remove_dir_all(root.join(".git/refs/remotes/origin")).unwrap();
    std::fs::create_dir_all(root.join(".git/refs/remotes/included")).unwrap();
    std::fs::write(
        root.join(".git/refs/remotes/included/main"),
        "2222222222222222222222222222222222222222\n",
    )
    .unwrap();
    std::fs::write(
            root.join(".git/config"),
            "[include]\n\tpath = included.cfg\n[branch \"main\"]\n\tremote = middle\n[include]\n\tpath = included.cfg\n",
        )
        .unwrap();
    std::fs::write(
            root.join(".git/included.cfg"),
            "[remote \"included\"]\n\tfetch = +refs/heads/*:refs/remotes/included/*\n[branch \"main\"]\n\tremote = included\n\tmerge = refs/heads/main\n",
        )
        .unwrap();

    let fingerprint = git_status_fingerprint(&root).unwrap();

    let upstream = fingerprint.upstream.unwrap();
    assert_eq!(upstream.remote, "included");
    assert_eq!(upstream.full_ref, "refs/remotes/included/main");

    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn git_status_fingerprint_reads_matching_include_if_config() {
    let root = temp_test_dir("include-if-config");
    write_fake_tracked_repo(&root);
    std::fs::remove_dir_all(root.join(".git/refs/remotes/origin")).unwrap();
    std::fs::create_dir_all(root.join(".git/refs/remotes/included")).unwrap();
    std::fs::write(
        root.join(".git/refs/remotes/included/main"),
        "2222222222222222222222222222222222222222\n",
    )
    .unwrap();
    std::fs::write(
        root.join(".git/config"),
        format!(
            "[includeIf \"gitdir:{}\"]\n\tpath = included.cfg\n",
            root.join(".git").display()
        ),
    )
    .unwrap();
    std::fs::write(
            root.join(".git/included.cfg"),
            "[remote \"included\"]\n\tfetch = +refs/heads/*:refs/remotes/included/*\n[branch \"main\"]\n\tremote = included\n\tmerge = refs/heads/main\n",
        )
        .unwrap();

    let fingerprint = git_status_fingerprint(&root).unwrap();

    let upstream = fingerprint.upstream.unwrap();
    assert_eq!(upstream.remote, "included");
    assert_eq!(upstream.full_ref, "refs/remotes/included/main");

    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn git_status_fingerprint_matches_gitdir_include_if_directory_pattern() {
    let base = temp_test_dir("include-if-dir");
    let root = base.join("work/repo");
    std::fs::create_dir_all(&root).unwrap();
    write_fake_tracked_repo(&root);
    std::fs::remove_dir_all(root.join(".git/refs/remotes/origin")).unwrap();
    std::fs::create_dir_all(root.join(".git/refs/remotes/included")).unwrap();
    std::fs::write(
        root.join(".git/refs/remotes/included/main"),
        "2222222222222222222222222222222222222222\n",
    )
    .unwrap();
    std::fs::write(
        root.join(".git/config"),
        format!(
            "[includeIf \"gitdir:{}/\"]\n\tpath = included.cfg\n",
            base.join("work").display()
        ),
    )
    .unwrap();
    std::fs::write(
            root.join(".git/included.cfg"),
            "[remote \"included\"]\n\tfetch = +refs/heads/*:refs/remotes/included/*\n[branch \"main\"]\n\tremote = included\n\tmerge = refs/heads/main\n",
        )
        .unwrap();

    let fingerprint = git_status_fingerprint(&root).unwrap();

    let upstream = fingerprint.upstream.unwrap();
    assert_eq!(upstream.remote, "included");
    assert_eq!(upstream.full_ref, "refs/remotes/included/main");

    std::fs::remove_dir_all(base).unwrap();
}

#[test]
fn git_status_fingerprint_reads_case_insensitive_config_keys() {
    let root = temp_test_dir("case-insensitive-config");
    write_fake_tracked_repo(&root);
    std::fs::write(
            root.join(".git/config"),
            "[Remote \"origin\"] # remote section\n\tFetch = +refs/heads/*:refs/remotes/origin/*\n[Branch \"main\"] ; branch section\n\tRemote = origin\n\tMerge = refs/heads/main\n",
        )
        .unwrap();

    let fingerprint = git_status_fingerprint(&root).unwrap();

    let upstream = fingerprint.upstream.unwrap();
    assert_eq!(upstream.remote, "origin");
    assert_eq!(upstream.full_ref, "refs/remotes/origin/main");

    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn git_status_fingerprint_keeps_refspecs_for_later_remote_override() {
    let root = temp_test_dir("worktree-remote-override");
    write_fake_tracked_repo(&root);
    std::fs::remove_dir_all(root.join(".git/refs/remotes/origin")).unwrap();
    std::fs::create_dir_all(root.join(".git/refs/remotes/fork")).unwrap();
    std::fs::write(
        root.join(".git/refs/remotes/fork/main"),
        "2222222222222222222222222222222222222222\n",
    )
    .unwrap();
    std::fs::write(
            root.join(".git/config"),
            "[extensions]\n\tworktreeConfig = true\n[remote \"fork\"]\n\tfetch = +refs/heads/*:refs/remotes/fork/*\n[branch \"main\"]\n\tremote = origin\n\tmerge = refs/heads/main\n",
        )
        .unwrap();
    std::fs::write(
        root.join(".git/config.worktree"),
        "[branch \"main\"]\n\tremote = fork\n",
    )
    .unwrap();

    let fingerprint = git_status_fingerprint(&root).unwrap();

    let upstream = fingerprint.upstream.unwrap();
    assert_eq!(upstream.remote, "fork");
    assert_eq!(upstream.full_ref, "refs/remotes/fork/main");

    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn git_status_fingerprint_ignores_worktree_config_when_extension_disabled() {
    let root = temp_test_dir("worktree-config-disabled");
    write_fake_tracked_repo(&root);
    std::fs::create_dir_all(root.join(".git/refs/remotes/fork")).unwrap();
    std::fs::write(
        root.join(".git/refs/remotes/fork/main"),
        "3333333333333333333333333333333333333333\n",
    )
    .unwrap();
    std::fs::write(
            root.join(".git/config"),
            "[remote \"fork\"]\n\tfetch = +refs/heads/*:refs/remotes/fork/*\n[branch \"main\"]\n\tremote = origin\n\tmerge = refs/heads/main\n",
        )
        .unwrap();
    std::fs::write(
        root.join(".git/config.worktree"),
        "[branch \"main\"]\n\tremote = fork\n",
    )
    .unwrap();

    let fingerprint = git_status_fingerprint(&root).unwrap();

    let upstream = fingerprint.upstream.unwrap();
    assert_eq!(upstream.remote, "origin");
    assert_eq!(upstream.full_ref, "refs/remotes/origin/main");

    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn git_status_fingerprint_accepts_git_boolean_worktree_config() {
    let root = temp_test_dir("worktree-config-boolean");
    write_fake_tracked_repo(&root);
    std::fs::create_dir_all(root.join(".git/refs/remotes/fork")).unwrap();
    std::fs::write(
        root.join(".git/refs/remotes/fork/main"),
        "3333333333333333333333333333333333333333\n",
    )
    .unwrap();
    std::fs::write(
            root.join(".git/config"),
            "[extensions]\n\tworktreeConfig\n[remote \"fork\"]\n\tfetch = +refs/heads/*:refs/remotes/fork/*\n[branch \"main\"]\n\tremote = origin\n\tmerge = refs/heads/main\n",
        )
        .unwrap();
    std::fs::write(
        root.join(".git/config.worktree"),
        "[branch \"main\"]\n\tremote = fork\n",
    )
    .unwrap();

    let fingerprint = git_status_fingerprint(&root).unwrap();

    let upstream = fingerprint.upstream.unwrap();
    assert_eq!(upstream.remote, "fork");
    assert_eq!(upstream.full_ref, "refs/remotes/fork/main");

    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn git_status_fingerprint_uses_last_worktree_config_boolean() {
    let root = temp_test_dir("worktree-config-duplicate-boolean");
    write_fake_tracked_repo(&root);
    std::fs::create_dir_all(root.join(".git/refs/remotes/fork")).unwrap();
    std::fs::write(
        root.join(".git/refs/remotes/fork/main"),
        "3333333333333333333333333333333333333333\n",
    )
    .unwrap();
    std::fs::write(
            root.join(".git/config"),
            "[extensions]\n\tworktreeConfig = false\n\tworktreeConfig = true\n[remote \"fork\"]\n\tfetch = +refs/heads/*:refs/remotes/fork/*\n[branch \"main\"]\n\tremote = origin\n\tmerge = refs/heads/main\n",
        )
        .unwrap();
    std::fs::write(
        root.join(".git/config.worktree"),
        "[branch \"main\"]\n\tremote = fork\n",
    )
    .unwrap();

    let fingerprint = git_status_fingerprint(&root).unwrap();

    let upstream = fingerprint.upstream.unwrap();
    assert_eq!(upstream.remote, "fork");
    assert_eq!(upstream.full_ref, "refs/remotes/fork/main");

    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn git_status_fingerprint_ignores_included_worktree_config_extension() {
    let root = temp_test_dir("worktree-config-included-extension");
    write_fake_tracked_repo(&root);
    std::fs::create_dir_all(root.join(".git/refs/remotes/fork")).unwrap();
    std::fs::write(
        root.join(".git/refs/remotes/fork/main"),
        "3333333333333333333333333333333333333333\n",
    )
    .unwrap();
    std::fs::write(
            root.join(".git/config"),
            "[include]\n\tpath = extension.cfg\n[remote \"fork\"]\n\tfetch = +refs/heads/*:refs/remotes/fork/*\n[branch \"main\"]\n\tremote = origin\n\tmerge = refs/heads/main\n",
        )
        .unwrap();
    std::fs::write(
        root.join(".git/extension.cfg"),
        "[extensions]\n\tworktreeConfig = true\n",
    )
    .unwrap();
    std::fs::write(
        root.join(".git/config.worktree"),
        "[branch \"main\"]\n\tremote = fork\n",
    )
    .unwrap();

    let fingerprint = git_status_fingerprint(&root).unwrap();

    let upstream = fingerprint.upstream.unwrap();
    assert_eq!(upstream.remote, "origin");
    assert_eq!(upstream.full_ref, "refs/remotes/origin/main");

    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn git_status_fingerprint_reads_onbranch_include_if_config() {
    let root = temp_test_dir("include-if-onbranch");
    write_fake_tracked_repo(&root);
    std::fs::remove_dir_all(root.join(".git/refs/remotes/origin")).unwrap();
    std::fs::create_dir_all(root.join(".git/refs/remotes/included")).unwrap();
    std::fs::write(
        root.join(".git/refs/remotes/included/main"),
        "2222222222222222222222222222222222222222\n",
    )
    .unwrap();
    std::fs::write(
        root.join(".git/config"),
        "[includeIf \"onbranch:main\"]\n\tpath = included.cfg\n",
    )
    .unwrap();
    std::fs::write(
            root.join(".git/included.cfg"),
            "[remote \"included\"]\n\tfetch = +refs/heads/*:refs/remotes/included/*\n[branch \"main\"]\n\tremote = included\n\tmerge = refs/heads/main\n",
        )
        .unwrap();

    let fingerprint = git_status_fingerprint(&root).unwrap();

    let upstream = fingerprint.upstream.unwrap();
    assert_eq!(upstream.remote, "included");
    assert_eq!(upstream.full_ref, "refs/remotes/included/main");

    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn git_status_fingerprint_reads_hasconfig_include_if_config() {
    let root = temp_test_dir("include-if-hasconfig");
    write_fake_tracked_repo(&root);
    std::fs::remove_dir_all(root.join(".git/refs/remotes/origin")).unwrap();
    std::fs::create_dir_all(root.join(".git/refs/remotes/included")).unwrap();
    std::fs::write(
        root.join(".git/refs/remotes/included/main"),
        "2222222222222222222222222222222222222222\n",
    )
    .unwrap();
    std::fs::write(
            root.join(".git/config"),
            "[remote \"fork\"]\n\turl = https://example.test/fork.git\n[includeIf \"hasconfig:remote.*.url:*fork.git\"]\n\tpath = included.cfg\n",
        )
        .unwrap();
    std::fs::write(
            root.join(".git/included.cfg"),
            "[remote \"included\"]\n\tfetch = +refs/heads/*:refs/remotes/included/*\n[branch \"main\"]\n\tremote = included\n\tmerge = refs/heads/main\n",
        )
        .unwrap();

    let fingerprint = git_status_fingerprint(&root).unwrap();

    let upstream = fingerprint.upstream.unwrap();
    assert_eq!(upstream.remote, "included");
    assert_eq!(upstream.full_ref, "refs/remotes/included/main");

    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn git_status_fingerprint_matches_user_hasconfig_against_repo_remote_url() {
    let root = temp_test_dir("include-if-hasconfig-user-repo-url");
    let user_config = root.join("user.gitconfig");
    write_fake_tracked_repo(&root);
    std::fs::remove_dir_all(root.join(".git/refs/remotes/origin")).unwrap();
    std::fs::create_dir_all(root.join(".git/refs/remotes/included")).unwrap();
    std::fs::write(
        root.join(".git/refs/remotes/included/main"),
        "2222222222222222222222222222222222222222\n",
    )
    .unwrap();
    std::fs::write(
        root.join(".git/config"),
        "[remote \"fork\"]\n\turl = https://example.test/fork.git\n",
    )
    .unwrap();
    std::fs::write(
        &user_config,
        "[includeIf \"hasconfig:remote.*.url:*fork.git\"]\n\tpath = user-included.cfg\n",
    )
    .unwrap();
    std::fs::write(
            root.join("user-included.cfg"),
            "[remote \"included\"]\n\tfetch = +refs/heads/*:refs/remotes/included/*\n[branch \"main\"]\n\tremote = included\n\tmerge = refs/heads/main\n",
        )
        .unwrap();

    let info = git_worktree_info(&root).unwrap();
    let config = read_branch_config_with_user_paths(&info, "main", vec![user_config]).unwrap();

    assert_eq!(config.remote, "included");
    assert_eq!(config.merge_ref, "refs/heads/main");

    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn git_status_fingerprint_skips_hasconfig_include_that_defines_remote_url() {
    let root = temp_test_dir("include-if-hasconfig-rejects-remote-url");
    let user_config = root.join("user.gitconfig");
    write_fake_tracked_repo(&root);
    std::fs::create_dir_all(root.join(".git/refs/remotes/included")).unwrap();
    std::fs::write(
        root.join(".git/refs/remotes/included/main"),
        "3333333333333333333333333333333333333333\n",
    )
    .unwrap();
    std::fs::write(
            root.join(".git/config"),
            "[remote \"fork\"]\n\turl = https://example.test/fork.git\n[branch \"main\"]\n\tremote = origin\n\tmerge = refs/heads/main\n",
        )
        .unwrap();
    std::fs::write(
        &user_config,
        "[includeIf \"hasconfig:remote.*.url:*fork.git\"]\n\tpath = user-included.cfg\n",
    )
    .unwrap();
    std::fs::write(
            root.join("user-included.cfg"),
            "[remote \"included\"]\n\turl = https://example.test/included.git\n\tfetch = +refs/heads/*:refs/remotes/included/*\n[branch \"main\"]\n\tremote = included\n\tmerge = refs/heads/main\n",
        )
        .unwrap();

    let info = git_worktree_info(&root).unwrap();
    let config = read_branch_config_with_user_paths(&info, "main", vec![user_config]).unwrap();

    assert_eq!(config.remote, "origin");
    assert_eq!(config.merge_ref, "refs/heads/main");

    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn git_status_fingerprint_skips_hasconfig_include_chain_that_defines_remote_url() {
    let root = temp_test_dir("include-if-hasconfig-rejects-nested-remote-url");
    let user_config = root.join("user.gitconfig");
    write_fake_tracked_repo(&root);
    std::fs::create_dir_all(root.join(".git/refs/remotes/included")).unwrap();
    std::fs::write(
        root.join(".git/refs/remotes/included/main"),
        "3333333333333333333333333333333333333333\n",
    )
    .unwrap();
    std::fs::write(
            root.join(".git/config"),
            "[remote \"fork\"]\n\turl = https://example.test/fork.git\n[branch \"main\"]\n\tremote = origin\n\tmerge = refs/heads/main\n",
        )
        .unwrap();
    std::fs::write(
        &user_config,
        "[includeIf \"hasconfig:remote.*.url:*fork.git\"]\n\tpath = user-included.cfg\n",
    )
    .unwrap();
    std::fs::write(
            root.join("user-included.cfg"),
            "[include]\n\tpath = nested-remote.cfg\n[branch \"main\"]\n\tremote = included\n\tmerge = refs/heads/main\n",
        )
        .unwrap();
    std::fs::write(
            root.join("nested-remote.cfg"),
            "[remote \"included\"]\n\turl = https://example.test/included.git\n\tfetch = +refs/heads/*:refs/remotes/included/*\n",
        )
        .unwrap();

    let info = git_worktree_info(&root).unwrap();
    let config = read_branch_config_with_user_paths(&info, "main", vec![user_config]).unwrap();

    assert_eq!(config.remote, "origin");
    assert_eq!(config.merge_ref, "refs/heads/main");

    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn git_status_fingerprint_ignores_worktree_urls_for_hasconfig() {
    let root = temp_test_dir("include-if-hasconfig-worktree-url");
    write_fake_tracked_repo(&root);
    std::fs::write(
            root.join(".git/config"),
            "[extensions]\n\tworktreeConfig = true\n[branch \"main\"]\n\tremote = origin\n\tmerge = refs/heads/main\n",
        )
        .unwrap();
    std::fs::write(
            root.join(".git/config.worktree"),
            "[remote \"fork\"]\n\turl = https://example.test/fork.git\n[includeIf \"hasconfig:remote.*.url:*fork.git\"]\n\tpath = included.cfg\n",
        )
        .unwrap();
    std::fs::write(
        root.join(".git/included.cfg"),
        "[branch \"main\"]\n\tremote = included\n",
    )
    .unwrap();

    let fingerprint = git_status_fingerprint(&root).unwrap();

    let upstream = fingerprint.upstream.unwrap();
    assert_eq!(upstream.remote, "origin");
    assert_eq!(upstream.full_ref, "refs/remotes/origin/main");

    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn git_status_fingerprint_stops_recursive_include_cycles() {
    let root = temp_test_dir("include-cycle");
    write_fake_tracked_repo(&root);
    std::fs::remove_dir_all(root.join(".git/refs/remotes/origin")).unwrap();
    std::fs::create_dir_all(root.join(".git/refs/remotes/included")).unwrap();
    std::fs::write(
        root.join(".git/refs/remotes/included/main"),
        "2222222222222222222222222222222222222222\n",
    )
    .unwrap();
    std::fs::write(root.join(".git/config"), "[include]\n\tpath = a.cfg\n").unwrap();
    std::fs::write(root.join(".git/a.cfg"), "[include]\n\tpath = b.cfg\n").unwrap();
    std::fs::write(
            root.join(".git/b.cfg"),
            "[include]\n\tpath = a.cfg\n[remote \"included\"]\n\tfetch = +refs/heads/*:refs/remotes/included/*\n[branch \"main\"]\n\tremote = included\n\tmerge = refs/heads/main\n",
        )
        .unwrap();

    let fingerprint = git_status_fingerprint(&root).unwrap();

    let upstream = fingerprint.upstream.unwrap();
    assert_eq!(upstream.remote, "included");
    assert_eq!(upstream.full_ref, "refs/remotes/included/main");

    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn git_status_fingerprint_reads_linked_worktree_config() {
    let base = temp_test_dir("linked-worktree-config");
    let common_dir = base.join("repo/.git");
    let worktree = base.join("linked");
    let git_dir = common_dir.join("worktrees/linked");
    std::fs::create_dir_all(common_dir.join("refs/heads")).unwrap();
    std::fs::create_dir_all(common_dir.join("refs/remotes/fork")).unwrap();
    std::fs::create_dir_all(&git_dir).unwrap();
    std::fs::create_dir_all(&worktree).unwrap();
    std::fs::write(
        worktree.join(".git"),
        format!("gitdir: {}\n", git_dir.display()),
    )
    .unwrap();
    std::fs::write(git_dir.join("commondir"), "../..\n").unwrap();
    std::fs::write(git_dir.join("HEAD"), "ref: refs/heads/main\n").unwrap();
    std::fs::write(
        common_dir.join("refs/heads/main"),
        "1111111111111111111111111111111111111111\n",
    )
    .unwrap();
    std::fs::write(
        common_dir.join("refs/remotes/fork/main"),
        "2222222222222222222222222222222222222222\n",
    )
    .unwrap();
    std::fs::write(
            common_dir.join("config"),
            "[extensions]\n\tworktreeConfig = TRUE\n[remote \"fork\"]\n\tfetch = +refs/heads/*:refs/remotes/fork/*\n[branch \"main\"]\n\tremote = origin\n\tmerge = refs/heads/main\n",
        )
        .unwrap();
    std::fs::write(
        git_dir.join("config.worktree"),
        "[branch \"main\"]\n\tremote = fork\n",
    )
    .unwrap();

    let fingerprint = git_status_fingerprint(&worktree).unwrap();

    let upstream = fingerprint.upstream.unwrap();
    assert_eq!(upstream.remote, "fork");
    assert_eq!(upstream.full_ref, "refs/remotes/fork/main");

    std::fs::remove_dir_all(base).unwrap();
}

#[test]
fn git_status_fingerprint_ignores_inline_fetch_refspec_comment() {
    let root = temp_test_dir("commented-fetch-refspec");
    write_fake_tracked_repo(&root);
    std::fs::remove_dir_all(root.join(".git/refs/remotes/origin")).unwrap();
    std::fs::create_dir_all(root.join(".git/refs/remotes/upstream")).unwrap();
    std::fs::write(
        root.join(".git/refs/remotes/upstream/main"),
        "2222222222222222222222222222222222222222\n",
    )
    .unwrap();
    std::fs::write(
            root.join(".git/config"),
            "[remote \"origin\"]\n\tfetch = +refs/heads/*:refs/remotes/upstream/* # custom map\n[branch \"main\"]\n\tremote = origin\n\tmerge = refs/heads/main\n",
        )
        .unwrap();

    let fingerprint = git_status_fingerprint(&root).unwrap();

    let upstream = fingerprint.upstream.unwrap();
    assert_eq!(upstream.full_ref, "refs/remotes/upstream/main");
    assert_eq!(
        upstream.oid.as_deref(),
        Some("2222222222222222222222222222222222222222")
    );

    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn git_status_fingerprint_clears_upstream_for_unmapped_refspec() {
    let root = temp_test_dir("unmapped-fetch-refspec");
    write_fake_tracked_repo(&root);
    std::fs::write(
            root.join(".git/config"),
            "[remote \"origin\"]\n\tfetch = +refs/pull/*:refs/remotes/origin/pr/*\n[branch \"main\"]\n\tremote = origin\n\tmerge = refs/heads/main\n",
        )
        .unwrap();

    let fingerprint = git_status_fingerprint(&root).unwrap();

    assert_eq!(fingerprint.upstream, None);

    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn git_status_fingerprint_honors_negative_fetch_refspec() {
    let root = temp_test_dir("negative-fetch-refspec");
    write_fake_tracked_repo(&root);
    std::fs::write(
            root.join(".git/config"),
            "[remote \"origin\"]\n\tfetch = +refs/heads/*:refs/remotes/origin/*\n\tfetch = ^refs/heads/main\n[branch \"main\"]\n\tremote = origin\n\tmerge = refs/heads/main\n",
        )
        .unwrap();

    let fingerprint = git_status_fingerprint(&root).unwrap();

    let upstream = fingerprint.upstream.unwrap();
    assert_eq!(upstream.full_ref, "refs/remotes/origin/main");
    assert_eq!(
        upstream.oid.as_deref(),
        Some("2222222222222222222222222222222222222222")
    );

    std::fs::remove_dir_all(root).unwrap();
}
