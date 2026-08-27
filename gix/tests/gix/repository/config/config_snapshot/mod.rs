use gix::config::tree::{Branch, Core, Key, Pack, gitoxide};

use crate::{named_repo, repo_rw, repo_rw_opts};

fn options_with_includes() -> gix::open::Options {
    let mut permissions = gix::open::Permissions::isolated();
    permissions.config.includes = true;
    gix::open::Options::isolated().permissions(permissions)
}

#[cfg(feature = "credentials")]
mod credential_helpers;

#[test]
fn commit_auto_rollback() -> crate::Result {
    let mut repo = named_repo("make_basic_repo.sh")?;
    let default_abbrev = repo.head_id()?.to_string()[..7].to_owned();
    let short_abbrev = repo.head_id()?.to_string()[..4].to_owned();
    assert_eq!(repo.head_id()?.shorten()?.to_string(), default_abbrev);

    {
        let mut config = repo.config_snapshot_mut();
        config.set_raw_value(Core::ABBREV, "4")?;
        let repo = config.commit_auto_rollback()?;
        assert_eq!(repo.head_id()?.shorten()?.to_string(), short_abbrev);
    }

    assert_eq!(repo.head_id()?.shorten()?.to_string(), default_abbrev);

    let repo = {
        let mut config = repo.config_snapshot_mut();
        config.set_raw_value(Core::ABBREV, "4")?;
        let mut repo = config.commit_auto_rollback()?;
        assert_eq!(repo.head_id()?.shorten()?.to_string(), short_abbrev);
        // access to the mutable repo underneath
        repo.object_cache_size_if_unset(16 * 1024);
        repo.rollback()?
    };
    assert_eq!(repo.head_id()?.shorten()?.to_string(), default_abbrev);

    Ok(())
}

mod trusted_path {
    use crate::util::named_repo;

    #[test]
    fn optional_is_respected() -> crate::Result {
        let mut repo = named_repo("make_basic_repo.sh")?;
        repo.config_snapshot_mut().set_raw_value("my.path", "does-not-exist")?;

        let actual = repo.config_snapshot().trusted_path("my.path")?.expect("is set");
        assert_eq!(
            actual,
            std::path::PathBuf::from("does-not-exist"),
            "the path isn't evaluated by default, and may not exist"
        );

        repo.config_snapshot_mut()
            .set_raw_value("my.path", ":(optional)does-not-exist")?;
        let actual = repo.config_snapshot().trusted_path("my.path")?;
        assert_eq!(actual, None, "non-existing paths aren't returned to the caller");
        Ok(())
    }
}

#[test]
fn snapshot_mut_commit_and_forget() -> crate::Result {
    let mut repo = named_repo("make_basic_repo.sh")?;
    let repo = {
        let mut repo = repo.config_snapshot_mut();
        repo.set_value(&Core::ABBREV, "4")?;
        repo.commit()?
    };
    assert_eq!(repo.config_snapshot().integer("core.abbrev").expect("set"), 4);
    {
        let mut repo = repo.config_snapshot_mut();
        repo.set_raw_value(Core::ABBREV, "8")?;
        repo.forget();
    }
    assert_eq!(repo.config_snapshot().integer("core.abbrev"), Some(4));
    Ok(())
}

#[test]
fn committing_loose_compression_requires_reopening_the_object_store() -> crate::Result {
    use gix::objs::Write;

    fn loose_object_size(repo: &gix::Repository, id: gix::ObjectId) -> std::io::Result<u64> {
        let hex = id.to_string();
        std::fs::metadata(repo.git_dir().join("objects").join(&hex[..2]).join(&hex[2..])).map(|meta| meta.len())
    }

    let (mut repo, _tmp) = repo_rw("make_basic_repo.sh")?;
    let mut data = vec![b'a'; 128 * 1024];
    let compressed = repo.objects.write_buf(gix::objs::Kind::Blob, &data)?;
    let compressed_size = loose_object_size(&repo, compressed)?;

    let mut config = repo.config_snapshot_mut();
    config.set_value(&Core::LOOSE_COMPRESSION, "0")?;
    config.commit()?;

    data[0] = b'b';
    let still_compressed = repo.objects.write_buf(gix::objs::Kind::Blob, &data)?;
    let still_compressed_size = loose_object_size(&repo, still_compressed)?;

    let git_dir = repo.git_dir().to_owned();
    let options = repo
        .open_options()
        .clone()
        .config_overrides(["core.looseCompression=0"]);
    repo = gix::open_opts(git_dir, options)?;

    data[1] = b'b';
    let uncompressed = repo.write_blob(&data)?;
    let uncompressed_size = loose_object_size(&repo, uncompressed.detach())?;
    assert!(
        uncompressed_size > compressed_size * 10 && uncompressed_size > still_compressed_size * 10,
        "the override should take effect after reopening the object store: {compressed_size}, {still_compressed_size} vs {uncompressed_size}"
    );
    Ok(())
}

#[test]
fn compression_levels() -> crate::Result {
    use gix::zlib::Compression;

    let mut repo = named_repo("make_basic_repo.sh")?;
    assert_eq!(repo.loose_compression(), Compression::BEST_SPEED);
    assert_eq!(repo.pack_compression()?, Compression::DEFAULT);

    let mut config = repo.config_snapshot_mut();
    config.set_value(&Core::COMPRESSION, "4")?;
    config.commit()?;
    assert_eq!(repo.loose_compression(), Compression::new(4).expect("valid level"));
    assert_eq!(repo.pack_compression()?, Compression::new(4).expect("valid level"));

    let mut config = repo.config_snapshot_mut();
    config.set_value(&Core::LOOSE_COMPRESSION, "2")?;
    config.set_value(&Pack::COMPRESSION, "8")?;
    config.commit()?;
    assert_eq!(repo.loose_compression(), Compression::new(2).expect("valid level"));
    assert_eq!(repo.pack_compression()?, Compression::new(8).expect("valid level"));

    Ok(())
}

#[test]
fn values_are_set_in_memory_only() {
    let mut repo = named_repo("make_config_repo.sh").unwrap();
    let repo_clone = repo.clone();
    let key = "hallo.welt";
    let key_subsection = "branch.main.merge";
    assert_eq!(repo.config_snapshot().boolean(key), None, "no value there just yet");
    assert_eq!(repo.config_snapshot().string(key_subsection), None);

    {
        let mut config = repo.config_snapshot_mut();
        config.set_raw_value("hallo.welt", "true").unwrap();
        config
            .set_subsection_value(&Branch::MERGE, "main", "refs/heads/foo")
            .unwrap();
    }

    assert_eq!(
        repo.config_snapshot().boolean(key),
        Some(true),
        "value was set and applied"
    );
    assert_eq!(
        repo.config_snapshot()
            .string(key_subsection)
            .expect("value was just set"),
        "refs/heads/foo"
    );

    assert_eq!(
        repo_clone.config_snapshot().boolean(key),
        None,
        "values are not written back automatically nor are they shared between clones"
    );
    assert_eq!(repo_clone.config_snapshot().string(key_subsection), None);
}

#[test]
fn set_value_in_subsection() {
    let mut repo = named_repo("make_config_repo.sh").unwrap();
    {
        let mut config = repo.config_snapshot_mut();
        config
            .set_value(&gitoxide::Credentials::TERMINAL_PROMPT, "yes")
            .unwrap();
        assert_eq!(
            config
                .string(&*gitoxide::Credentials::TERMINAL_PROMPT.logical_name())
                .expect("just set"),
            "yes"
        );
    }
}

#[test]
fn apply_cli_overrides() -> crate::Result {
    let mut repo = named_repo("make_config_repo.sh").unwrap();
    repo.config_snapshot_mut().append_config(
        [
            "a.b=c",
            "remote.origin.url = url",
            "implicit.bool-true",
            "implicit.bool-false = ",
        ],
        gix_config::Source::Cli,
    )?;

    let config = repo.config_snapshot();
    assert_eq!(config.string("a.b").expect("present"), "c");
    assert_eq!(config.string("remote.origin.url").expect("present"), "url");
    assert_eq!(
        config.string("implicit.bool-true"),
        None,
        "no keysep is interpreted as 'not present' as we don't make up values"
    );
    assert_eq!(
        config.string("implicit.bool-false").expect("present"),
        "",
        "empty values are fine"
    );
    assert_eq!(
        config.boolean("implicit.bool-false"),
        Some(false),
        "empty values are boolean true"
    );
    assert_eq!(
        config.boolean("implicit.bool-true"),
        Some(true),
        "values without key-sep are true"
    );

    Ok(())
}

#[test]
fn reload_reloads_on_disk_changes() -> crate::Result {
    let (mut repo, _tmp) = repo_rw("make_config_repo.sh")?;
    assert_eq!(repo.config_snapshot().integer("core.abbrev"), None);
    let original_index = repo.index_path();
    let changed_index = repo.git_dir().join("changed-index");

    let config_path = repo.git_dir().join("config");
    let mut config = gix_config::File::from_path_no_includes(config_path.clone(), gix_config::Source::Local)?;
    config.set_raw_value("core.abbrev", "4")?;
    config.set_raw_value("gitoxide.core.indexFile", gix_path::into_bstr(&changed_index).as_ref())?;
    std::fs::write(config_path, config.to_bstring())?;

    assert_eq!(repo.config_snapshot().integer("core.abbrev"), None);
    assert_eq!(repo.index_path(), original_index, "repository locations remain cached");

    repo.reload()?;

    assert_eq!(repo.config_snapshot().integer("core.abbrev"), Some(4));
    assert_eq!(
        repo.index_path(),
        changed_index,
        "reload reapplies repository locations"
    );
    Ok(())
}

#[test]
fn reload_discards_in_memory_only_changes() -> crate::Result {
    let mut repo = named_repo("make_config_repo.sh")?;

    repo.config_snapshot_mut().set_raw_value(Core::ABBREV, "4")?;
    assert_eq!(repo.config_snapshot().integer("core.abbrev"), Some(4));

    repo.reload()?;
    assert_eq!(repo.config_snapshot().integer("core.abbrev"), None);
    Ok(())
}

#[test]
fn reload_rebuilds_includes_even_when_the_file_was_empty_or_missing() -> crate::Result {
    let (mut repo, _tmp) = repo_rw_opts("make_config_repo.sh", options_with_includes())?;
    let included_path = repo.git_dir().parent().expect("worktree repository").join("a.config");

    std::fs::write(&included_path, b"# no sections yet\n")?;
    repo.reload()?;
    assert_eq!(
        repo.config_snapshot()
            .string("a.local-override")
            .expect("root value remains"),
        "base"
    );

    std::fs::write(&included_path, b"[fresh]\nvalue = populated\n")?;
    repo.reload()?;
    assert_eq!(
        repo.config_snapshot().string("fresh.value").expect("new include value"),
        "populated"
    );

    std::fs::remove_file(&included_path)?;
    repo.reload()?;
    assert!(
        repo.config_snapshot().string("fresh.value").is_none(),
        "a missing include contributes no values"
    );
    Ok(())
}

#[test]
#[cfg(feature = "index")]
fn reload_preserves_the_reduced_trust_allocation_limit() -> crate::Result {
    let fixture = gix_testtools::scripted_fixture_writable("make_config_repo.sh")?;
    let mut repo = gix::open_opts(
        fixture.path(),
        crate::util::restricted()
            .with(gix_sec::Trust::Reduced)
            .config_overrides(["gitoxide.objects.allocLimitIfReducedTrust=1"]),
    )?;
    assert!(repo.open_index().is_err(), "the opening fallback limits allocations");

    repo.reload()?;
    assert!(
        repo.open_index().is_err(),
        "reopening reapplies the reduced-trust allocation limit"
    );
    Ok(())
}

#[test]
#[serial_test::serial]
fn reload_reapplies_per_file_safe_directory_trust() -> crate::Result {
    let fixture = gix_testtools::scripted_fixture_writable("make_config_repo.sh")?;
    let included_path = fixture.path().join("a.config");
    let mut included = gix_config::File::from_path_no_includes(included_path.clone(), gix_config::Source::Local)?;
    included.set_raw_value(Core::SSH_COMMAND, "trusted-ssh")?;
    std::fs::write(&included_path, included.to_bstring())?;

    let global_path = fixture.path().join("global.config");
    let mut global = gix_config::File::new(gix_config::file::Metadata::from(gix_config::Source::User));
    global.set_raw_value(
        "safe.directory",
        gix_path::into_bstr(&std::fs::canonicalize(&included_path)?).as_ref(),
    )?;
    std::fs::write(&global_path, global.to_bstring())?;
    let _env = gix_testtools::Env::new().set("GIT_CONFIG_GLOBAL", global_path.display().to_string());
    let mut permissions = gix::open::Permissions::isolated();
    permissions.config.user = true;
    permissions.config.includes = true;
    permissions.env.git_prefix = gix_sec::Permission::Allow;
    let mut repo = gix::open_opts(
        fixture.path(),
        gix::open::Options::isolated()
            .permissions(permissions)
            .with(gix_sec::Trust::Reduced),
    )?;
    assert_eq!(
        repo.config_snapshot().trusted_program(Core::SSH_COMMAND),
        Some("trusted-ssh".into())
    );

    repo.reload()?;
    assert_eq!(
        repo.config_snapshot().trusted_program(Core::SSH_COMMAND),
        Some("trusted-ssh".into()),
        "reopening repeats per-file trust promotion"
    );
    Ok(())
}

#[test]
fn reload_resolves_onbranch_includes_from_unnamespaced_head() -> crate::Result {
    use std::io::Write;

    let (mut repo, _tmp) = repo_rw_opts("make_config_repo.sh", options_with_includes())?;
    let current_branch = repo
        .head_name()?
        .expect("the fixture has a symbolic HEAD")
        .shorten()
        .to_string();
    let config_path = repo.git_dir().join("config");
    let worktree = repo.git_dir().parent().expect("worktree repository");
    std::fs::write(worktree.join("current-branch.config"), b"[refresh]\nbranch = current\n")?;
    std::fs::write(worktree.join("other-branch.config"), b"[refresh]\nbranch = other\n")?;
    let mut disk = gix_config::File::from_path_no_includes(config_path.clone(), gix_config::Source::Local)?;
    disk.set_raw_value("gitoxide.core.refsNamespace", "config-refresh")?;
    std::fs::write(&config_path, disk.to_bstring())?;
    std::fs::OpenOptions::new().append(true).open(&config_path)?.write_all(
        format!(
            "\n[includeIf \"onbranch:{current_branch}\"]\n  path = ../current-branch.config\n\
             [includeIf \"onbranch:config-refresh-other\"]\n  path = ../other-branch.config\n"
        )
        .as_bytes(),
    )?;
    repo.reload()?;
    assert!(repo.namespace().is_some(), "the reopened repository uses a namespace");
    assert_eq!(
        repo.config_snapshot()
            .string("refresh.branch")
            .expect("current include"),
        "current"
    );

    std::fs::write(repo.git_dir().join("HEAD"), b"ref: refs/heads/config-refresh-other\n")?;
    repo.reload()?;
    assert_eq!(
        repo.config_snapshot().string("refresh.branch").expect("new include"),
        "other",
        "HEAD conditions are resolved without the configured reference namespace"
    );
    Ok(())
}

#[test]
#[cfg(all(feature = "sha1", feature = "sha256"))]
fn reload_rebuilds_object_stores_for_a_new_object_format() -> crate::Result {
    let (mut repo, _tmp) = repo_rw("make_config_repo.sh")?;
    let config_path = repo.git_dir().join("config");
    let mut disk = gix_config::File::from_path_no_includes(config_path.clone(), gix_config::Source::Local)?;
    disk.set_raw_value("core.repositoryFormatVersion", "1")?;
    disk.set_raw_value("extensions.objectFormat", "sha256")?;
    std::fs::write(config_path, disk.to_bstring())?;

    repo.reload()?;
    assert_eq!(repo.object_hash(), gix::hash::Kind::Sha256);
    assert_eq!(repo.write_blob(b"sha256")?.kind(), gix::hash::Kind::Sha256);
    Ok(())
}

#[test]
fn reload_keeps_opening_overrides_and_discards_runtime_edits() -> crate::Result {
    let options = options_with_includes().config_overrides([
        "user.name=gitoxide",
        "user.email=gitoxide@localhost",
        "refresh.open=from-options",
    ]);
    let (mut repo, _tmp) = repo_rw_opts("make_config_repo.sh", options)?;
    repo.config_snapshot_mut()
        .append_config(["refresh.runtime=from-api"], gix_config::Source::Api)?;
    assert_eq!(
        repo.config_snapshot().string("refresh.runtime").expect("runtime value"),
        "from-api"
    );

    repo.reload()?;
    assert_eq!(
        repo.config_snapshot().string("refresh.open").expect("opening override"),
        "from-options"
    );
    assert!(
        repo.config_snapshot().string("refresh.runtime").is_none(),
        "reload discards transient API sections"
    );
    Ok(())
}

mod file_mut {
    use super::options_with_includes;
    use crate::{named_repo, repo_rw_opts};
    use std::io::Write;

    #[test]
    fn locks_and_edits_one_physical_file_until_explicit_reload() -> crate::Result {
        let (mut repo, _tmp) = repo_rw_opts("make_config_repo.sh", options_with_includes())?;
        let root_path = repo.git_dir().join("config");
        let target_path = repo.git_dir().parent().expect("worktree repository").join("a.config");
        std::fs::write(
            &target_path,
            b"# leading comment\nleading = value\n[a]\n  local-override = included\n\n[committer]\n  name = committer\n  email = committer@email\n# trailing comment\n",
        )?;
        std::fs::OpenOptions::new()
            .append(true)
            .open(&root_path)?
            .write_all(b"\n[include]\n  path = ../a.config\n")?;
        repo.reload()?;
        let root_before = std::fs::read(&root_path)?;

        repo.config_snapshot_mut().set_raw_value("memory.value", "transient")?;
        let mut file = repo.config_file_mut(&target_path)?;
        match repo.config_file_mut(&target_path) {
            Err(gix::config::file_mut::Error::AcquireLock(_)) => {}
            Err(err) => panic!("lock contention must be reported precisely: {err:?}"),
            Ok(_) => panic!("a second transaction must not acquire the same lock"),
        }
        file.set_raw_value("a.local-override", "written")?;
        file.commit()?;

        assert_eq!(std::fs::read(&root_path)?, root_before, "the root file is untouched");
        let written = std::fs::read_to_string(&target_path)?;
        assert!(
            written.starts_with("# leading comment\nleading = value\n"),
            "frontmatter is preserved: {written:?}"
        );
        assert!(written.ends_with("# trailing comment\n"), "postmatter is preserved");
        assert_eq!(written.matches("[a]").count(), 1, "one physical section is written");
        assert_eq!(
            repo.config_snapshot()
                .string("a.local-override")
                .expect("cached include value"),
            "included",
            "committing a file doesn't refresh the repository"
        );
        assert_eq!(
            repo.config_snapshot().string("memory.value").expect("in-memory value"),
            "transient"
        );

        repo.reload()?;
        assert_eq!(
            repo.config_snapshot()
                .string("a.local-override")
                .expect("reloaded include value"),
            "written"
        );
        assert!(
            repo.config_snapshot().string("memory.value").is_none(),
            "reload discards in-memory configuration"
        );
        Ok(())
    }

    #[test]
    fn honors_core_config_lock_timeout() -> crate::Result {
        let (mut repo, _tmp) = repo_rw_opts("make_config_repo.sh", gix::open::Options::isolated())?;
        let config_path = repo.git_dir().join("config");
        let mut lock_path = config_path.as_os_str().to_owned();
        lock_path.push(".lock");
        let lock_path = std::path::PathBuf::from(lock_path);
        std::fs::write(&lock_path, b"held")?;
        let release_path = lock_path.clone();
        let release = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(50));
            std::fs::remove_file(release_path)
        });

        let file = repo.config_file_mut(&config_path);
        release.join().expect("lock-release thread does not panic")?;
        drop(file?);

        repo.config_snapshot_mut()
            .set_raw_value(gix::config::tree::Core::CONFIG_LOCK_TIMEOUT, "0")?;
        std::fs::write(&lock_path, b"held")?;
        let err = match repo.config_file_mut(config_path) {
            Ok(_) => panic!("a zero timeout must not wait for the lock"),
            Err(err) => err,
        };
        std::fs::remove_file(lock_path)?;
        assert!(matches!(
            err,
            gix::config::file_mut::Error::AcquireLock(gix::lock::acquire::Error::PermanentlyLocked {
                mode: gix::lock::acquire::Fail::Immediately,
                ..
            })
        ));
        Ok(())
    }

    #[test]
    fn preserves_permissions() -> crate::Result {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let (repo, _tmp) = repo_rw_opts("make_config_repo.sh", options_with_includes())?;
            let target_path = repo.git_dir().parent().expect("worktree repository").join("a.config");
            std::fs::set_permissions(&target_path, std::fs::Permissions::from_mode(0o600))?;
            let mut file = repo.config_file_mut(&target_path)?;
            file.set_raw_value("a.local-override", "written")?;
            file.commit()?;
            assert_eq!(
                std::fs::metadata(target_path)?.permissions().mode() & 0o777,
                0o600,
                "writing preserves file permissions"
            );
        }
        #[cfg(windows)]
        {
            let (repo, _tmp) = repo_rw_opts("make_config_repo.sh", options_with_includes())?;
            let target_path = repo.git_dir().parent().expect("worktree repository").join("a.config");
            let mut permissions = std::fs::metadata(&target_path)?.permissions();
            permissions.set_readonly(true);
            std::fs::set_permissions(&target_path, permissions)?;

            let mut file = repo.config_file_mut(&target_path)?;
            file.set_raw_value("a.local-override", "written")?;
            file.commit()?;

            let mut permissions = std::fs::metadata(&target_path)?.permissions();
            let is_readonly = permissions.readonly();
            #[expect(clippy::permissions_set_readonly_false, reason = "this test only runs on Windows")]
            permissions.set_readonly(false);
            std::fs::set_permissions(&target_path, permissions)?;
            assert!(is_readonly, "writing preserves read-only file permissions");
        }
        Ok(())
    }

    #[test]
    #[cfg(unix)]
    fn follows_symlinked_configuration_files() -> crate::Result {
        use std::os::unix::fs::symlink;

        let repo = named_repo("make_basic_repo.sh")?;
        let dir = gix_testtools::tempfile::tempdir()?;
        let target = dir.path().join("target.config");
        let link = dir.path().join("link.config");
        std::fs::write(&target, b"[core]\n  abbrev = 7\n")?;
        symlink("target.config", &link)?;

        let mut file = repo.config_file_mut(&link)?;
        match repo.config_file_mut(&target) {
            Err(gix::config::file_mut::Error::AcquireLock(_)) => {}
            Err(err) => panic!("the target must report lock contention: {err:?}"),
            Ok(_) => panic!("the symlink and its target must use the same lock"),
        }
        file.set_raw_value("core.abbrev", "8")?;
        file.commit()?;

        assert!(link.symlink_metadata()?.file_type().is_symlink());
        let config = gix_config::File::from_path_no_includes(target, gix_config::Source::Local)?;
        assert_eq!(config.string("core.abbrev").expect("updated value"), "8");
        Ok(())
    }

    #[test]
    fn supports_empty_and_missing_files() -> crate::Result {
        let repo = named_repo("make_basic_repo.sh")?;
        let dir = gix_testtools::tempfile::tempdir()?;
        let empty = dir.path().join("empty.config");
        std::fs::write(&empty, b"# retained\n")?;

        let mut file = repo.config_file_mut(&empty)?;
        file.set_raw_value("fresh.value", "first")?;
        file.commit()?;
        assert!(
            std::fs::read_to_string(&empty)?.starts_with("# retained\n"),
            "comment-only frontmatter is retained"
        );

        let missing = dir.path().join("missing.config");
        let mut file = repo.config_file_mut(&missing)?;
        file.set_raw_value("fresh.value", "created")?;
        file.commit()?;
        let config = gix_config::File::from_path_no_includes(missing, gix_config::Source::Local)?;
        assert_eq!(config.string("fresh.value").expect("created value"), "created");
        Ok(())
    }

    #[test]
    #[cfg(unix)]
    fn applies_shared_repository_permissions_to_new_files() -> crate::Result {
        use std::os::unix::fs::PermissionsExt;

        let mut repo = named_repo("make_basic_repo.sh")?;
        repo.config_snapshot_mut()
            .set_raw_value(gix::config::tree::Core::SHARED_REPOSITORY, "group")?;
        let dir = gix_testtools::tempfile::tempdir()?;
        let path = dir.path().join("missing.config");
        let mut file = repo.config_file_mut(&path)?;
        file.set_raw_value("fresh.value", "created")?;
        file.commit()?;

        assert_eq!(
            path.metadata()?.permissions().mode() & 0o777,
            (0o666 & !gix_testtools::umask()) | 0o660,
            "new configuration files honor core.sharedRepository after applying the umask"
        );
        Ok(())
    }

    #[test]
    fn semantic_validation_happens_on_reload() -> crate::Result {
        let (mut repo, _tmp) = repo_rw_opts("make_config_repo.sh", options_with_includes().strict_config(true))?;
        let config_path = repo.git_dir().join("config");
        let mut file = repo.config_file_mut(&config_path)?;
        file.set_raw_value("core.repositoryFormatVersion", "2")?;
        file.commit()?;

        let disk = gix_config::File::from_path_no_includes(config_path, gix_config::Source::Local)?;
        assert_eq!(disk.integer("core.repositoryFormatVersion")?, Some(2));
        let err = match repo.reload() {
            Ok(_) => panic!("the persisted repository format must be rejected while reopening"),
            Err(err) => err,
        };
        assert!(
            matches!(
                err,
                gix::open::Error::Config(gix::config::Error::UnsupportedRepositoryFormatVersion { version: 2 })
            ),
            "reload reports the semantic error: {err:?}"
        );
        assert_eq!(
            repo.config_snapshot().integer("core.repositoryFormatVersion"),
            Some(0),
            "a failed reload leaves the live repository unchanged"
        );
        Ok(())
    }

    #[test]
    fn include_changes_take_effect_only_after_reload() -> crate::Result {
        let (mut repo, _tmp) = repo_rw_opts("make_config_repo.sh", options_with_includes())?;
        let replacement_path = repo
            .git_dir()
            .parent()
            .expect("worktree repository")
            .join("replacement.config");
        std::fs::write(&replacement_path, b"[replacement]\nvalue = visible\n")?;

        let mut file = repo.config_file_mut(repo.git_dir().join("config"))?;
        file.set_raw_value("include.path", "../replacement.config")?;
        file.commit()?;
        assert!(
            repo.config_snapshot().string("replacement.value").is_none(),
            "commit leaves the cached include graph untouched"
        );

        repo.reload()?;
        assert_eq!(
            repo.config_snapshot()
                .string("replacement.value")
                .expect("replacement include"),
            "visible"
        );
        Ok(())
    }

    #[test]
    #[cfg(unix)]
    fn paths_with_parent_components_retain_symlink_semantics() -> crate::Result {
        use std::os::unix::fs::symlink;

        let repo = named_repo("make_basic_repo.sh")?;
        let dir = gix_testtools::tempfile::tempdir()?;
        let logical = dir.path().join("logical");
        let physical = dir.path().join("physical");
        std::fs::create_dir(&logical)?;
        std::fs::create_dir_all(physical.join("child"))?;
        symlink(physical.join("child"), logical.join("link"))?;
        std::fs::write(logical.join("config"), b"[target]\nvalue = lexical\n")?;
        std::fs::write(physical.join("config"), b"[target]\nvalue = physical\n")?;

        let mut file = repo.config_file_mut(logical.join("link/../config"))?;
        file.set_raw_value("target.value", "written")?;
        file.commit()?;

        let lexical = gix_config::File::from_path_no_includes(logical.join("config"), gix_config::Source::Local)?;
        let physical = gix_config::File::from_path_no_includes(physical.join("config"), gix_config::Source::Local)?;
        assert_eq!(lexical.string("target.value").expect("lexical value"), "lexical");
        assert_eq!(physical.string("target.value").expect("physical value"), "written");
        Ok(())
    }
}
