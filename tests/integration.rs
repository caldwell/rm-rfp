// Copyright © 2024 David Caldwell <david@porkrind.org>

use std::{fs::{create_dir_all, read_dir},
          io::ErrorKind,
          path::{Path, PathBuf},
          process::{Command, Stdio}};

use anyhow::{anyhow, Context, Result};
use tempfile::{tempdir, TempDir};

fn make_test_tree(count: usize) -> Result<TempDir> {
    let dir = tempdir().context("tempdir()")?;
    let mut path = dir.path().to_owned();
    for c in "abcdefghijklmnopqrstuvwxyz".chars().take(count).map(|c| PathBuf::from(c.to_string())) {
        path.push(&c);
        for f in "abcdefghijklmnopqrstuvwxyz".chars().take(count).map(|c| PathBuf::from(format!("{c}{c}"))) {
            create_dir_all(&path).context("create_dir_all")?;
            let file = path.join(&f);
            std::fs::write(&file, file.strip_prefix(dir.path()).context("strip_prefix")?.as_os_str().as_encoded_bytes()).with_context(|| format!("write {file:?}"))?;
        }
    }
    Ok(dir)
}

fn find(path: &Path) -> Vec<PathBuf> {
    let mut paths = vec![];
    _find(path.to_owned(), &mut paths).expect("_find");
    paths.into_iter().map(|p| p.strip_prefix(&path).unwrap().to_owned()).collect()
}

fn _find(path: PathBuf, paths: &mut Vec<PathBuf>) -> Result<()> {
    let meta = match (&path).symlink_metadata() {
        Err(err) => {
            if err.kind() == ErrorKind::NotFound {
                return Ok(());
            }
            Err(anyhow!("stat: {err}"))?
        },
        Ok(m) => m,
    };
    if meta.is_dir() {
        let ctx = |e| anyhow!("read_dir: {e}");
        let mut entry = vec![];
        for f in read_dir(&path).map_err(ctx)? {
            entry.push(f.map_err(ctx)?.path())
        }
        entry.sort();
        let empty = entry.len() == 0;
        for f in entry.into_iter() {
            _find(f, paths)?;
        }
        if empty {
            paths.push(path);
        }
    } else {
        paths.push(path);
    }
    Ok(())
}

fn paths(paths: Vec<&str>) -> Vec<PathBuf> {
    paths.into_iter().map(|s| s.into()).collect()
}

fn run(flags: Vec<&'static str>, args: Vec<&Path>, stdin: &str) -> Result<(String, String)> {
    let exe = PathBuf::from(env!("CARGO_BIN_EXE_rm-rfp"));
    let mut child = Command::new(exe).args(flags)
                                     .args(args)
                                     .stdin(Stdio::piped())
                                     .stdout(Stdio::piped())
                                     .stderr(Stdio::piped())
                                     .spawn()
                                     .expect("Couldn't run {exe:?}");
    std::thread::spawn({
        let stdin_buf = stdin.to_owned();
        let mut stdin = child.stdin.take().ok_or(anyhow!("Failed to open stdin"))?;
        move || {
            use std::io::Write;
            stdin.write_all(stdin_buf.as_bytes())
        }
    });

    let output = child.wait_with_output()?;
    Ok((String::from_utf8(output.stdout)?, String::from_utf8(output.stderr)?))
}

#[test]
fn test_dry_run() {
    let dir = make_test_tree(26).expect("make_test_tree");
    let orig = find(&dir.path());
    let (out, err) = run(vec!["--dry-run"], vec![dir.path()], "").expect("rmp failed");
    let after = find(&dir.path());
    drop(dir);
    assert_eq!(after, orig);
    assert_eq!(out.as_str(), "");
    assert_eq!(err.as_str(), "");
}

#[test]
fn test_delete() {
    let dir = make_test_tree(26).expect("make_test_tree");
    let (out, err) = run(vec![], vec![dir.path()], "").expect("rmp failed");
    let after = find(&dir.path());
    drop(dir);
    assert_eq!(after, Vec::<PathBuf>::new());
    assert_eq!(out.as_str(), "");
    assert_eq!(err.as_str(), "");
}

#[test]
fn test_interactive_ynq() {
    let dir = make_test_tree(3).expect("make_test_tree");
    let inp = "nynyyyynnyyq".chars().map(|c| format!("{c}\n")).collect::<String>();
    let (out, err) = run(vec!["-i"], vec![dir.path()], &inp).expect("rmp failed");
    let after = find(&dir.path());
    drop(dir);
    if after != Vec::<PathBuf>::new() {
        print!("{}", out.lines().zip(inp.lines()).map(|(o, i)| format!("{o}{i}\n")).collect::<String>());
    }
    assert_eq!(after, paths(vec!["a/aa", "a/b/bb", "a/b/cc"]));
    assert_ne!(out.as_str(), "");
    assert_eq!(err.as_str(), "");
}

#[test]
fn test_interactive_a() {
    let dir = make_test_tree(26).expect("make_test_tree");
    let inp = "nnnyna".chars().map(|c| format!("{c}\n")).collect::<String>();
    let (out, err) = run(vec!["-i"], vec![dir.path()], &inp).expect("rmp failed");
    let after = find(&dir.path());
    drop(dir);
    if after != Vec::<PathBuf>::new() {
        print!("{}", out.lines().zip(inp.lines()).map(|(o, i)| format!("{o}{i}\n")).collect::<String>());
    }
    assert_eq!(after, paths(vec!["a/aa", "a/b/aa", "a/b/bb", "a/b/c/bb"]));
    assert_ne!(out.as_str(), "");
    assert_eq!(err.as_str(), "");
}

#[test]
fn test_interactive_d() {
    let dir = make_test_tree(7).expect("make_test_tree");
    let inp = "nnnndddq".chars().map(|c| format!("{c}\n")).collect::<String>();
    let (out, err) = run(vec!["-i"], vec![dir.path()], &inp).expect("rmp failed");
    let after = find(&dir.path());
    drop(dir);
    if after != Vec::<PathBuf>::new() {
        print!("{}", out.lines().zip(inp.lines()).map(|(o, i)| format!("{o}{i}\n")).collect::<String>());
    }
    assert_eq!(after, paths(vec!["a/aa", "a/b/aa", "a/b/bb", "a/b/c/aa"]));
    assert_ne!(out.as_str(), "");
    assert_eq!(err.as_str(), "");
}

#[test]
fn test_interactive_s() {
    let dir = make_test_tree(5).expect("make_test_tree");
    let inp = "yyyysysna".chars().map(|c| format!("{c}\n")).collect::<String>();
    let (out, err) = run(vec!["-i"], vec![dir.path()], &inp).expect("rmp failed");
    let after = find(&dir.path());
    drop(dir);
    if after != Vec::<PathBuf>::new() {
        print!("{}", out.lines().zip(inp.lines()).map(|(o, i)| format!("{o}{i}\n")).collect::<String>());
    }
    assert_eq!(after,
               paths(vec!["a/b/c/bb", "a/b/c/cc", "a/b/c/d/aa", "a/b/c/d/bb", "a/b/c/d/cc", "a/b/c/d/dd", "a/b/c/d/e/aa",
                          "a/b/c/d/e/bb", "a/b/c/d/e/cc", "a/b/c/d/e/dd", "a/b/c/d/e/ee", "a/b/c/d/ee", "a/b/c/dd", "a/b/c/ee",
                          "a/b/dd", "a/b/ee", "a/bb"]));
    assert_ne!(out.as_str(), "");
    assert_eq!(err.as_str(), "");
}
