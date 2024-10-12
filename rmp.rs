// Copyright © 2024 David Caldwell <david@porkrind.org>

use std::{fs::{read_dir, remove_dir, remove_file},
          panic,
          path::PathBuf,
          sync::OnceLock,
          sync::{mpsc::{sync_channel, SyncSender},
                 Arc, RwLock},
          thread::{self, sleep},
          time::Duration};

use anyhow::{anyhow, Error, Result};
use docopt::Docopt;
use indicatif::{HumanBytes, MultiProgress, ProgressBar, ProgressStyle};
use serde::Deserialize;

static TOTAL: OnceLock<RwLock<Stats>> = OnceLock::new();

fn usage() -> String {
    format!(r#"
Usage:
  rmp --help
  rmp [options] <path>...

Options:
  -h, --help         Show this screen.
  -n, --dry-run      Don't delete anything, but go through the motions as if it were.
"#)
}

#[derive(Debug, Deserialize)]
struct Args {
    flag_dry_run:     bool,
    arg_path:         Vec<PathBuf>,
}

fn main() -> Result<()> {
    let args: Args = Docopt::new(usage())
        .and_then(|d| d.deserialize())
        .unwrap_or_else(|e| e.exit());

    let (to_delete_tx, to_delete_rx) = sync_channel(1_000_000);

    let progress = ProgressBar::new_spinner().with_style(ProgressStyle::with_template("Counting files{spinner:3}  {len}").unwrap()
                                                                                                                         .tick_strings(&[".", "..", "...", ""]));
    let total = Arc::new(RwLock::new(Stats::default()));

    TOTAL.get_or_init(|| RwLock::new(Stats::default()));

    let finder = thread::spawn({
        let progress = progress.clone();
        let total = total.clone();
        let paths = args.arg_path.clone();
        move || -> Result<()> {
            let mut stats = Stats::default();
            for path in paths {
                stats += find(path, &to_delete_tx).map_err(|(path, err)| anyhow!("{path:?};{err}"))?
            }
            *total.write().unwrap() = stats;
            progress.set_length(stats.files);
            progress.set_style(ProgressStyle::with_template("{elapsed_precise} {wide_bar:.on_cyan/on_17} {eta_precise}").unwrap()
                                                                                                                        .progress_chars("   "));
            Ok(())
        }
    });

    let path_spinner = ProgressBar::new_spinner().with_style(ProgressStyle::with_template("{spinner} {prefix:<5.245} {msg:!.214}").unwrap());
    path_spinner.enable_steady_tick(Duration::from_secs_f32(1.0 / 10.0));
    let totals = ProgressBar::new_spinner().with_style(ProgressStyle::with_template("{msg}, elapsed: {elapsed_precise}")?);
    let multi = MultiProgress::new();
    multi.add(progress.clone());
    multi.add(path_spinner.clone());
    multi.add(totals.clone());

    let mut done = Stats::default();
    loop {
        match to_delete_rx.recv() {
            Ok(ToDelete::File { size, path }) => {
                //remove_file(&path)?;
                sleep(Duration::from_micros(1000));
                if args.flag_dry_run {
                    sleep(Duration::from_micros(1000));
                } else {
                    remove_file(&path)?;
                }
                path_spinner.set_message((*path.to_string_lossy()).to_owned());
                path_spinner.set_prefix("rm");
                done.bytes += size;
                done.files += 1;
            },
            Ok(ToDelete::Dir(path)) => {
                if args.flag_dry_run {
                    sleep(Duration::from_micros(80));
                } else {
                    remove_dir(&path)?;
                }
                path_spinner.set_message((*path.to_string_lossy()).to_owned());
                path_spinner.set_prefix("rmdir");
                done.dirs += 1;
            },
            Ok(ToDelete::Err { path, err }) => {
                _ = multi.println(format!("{path:?}: {err}"));
            },
            Err(_) => {  /* read-on-close-channel, ie: done. We'll get the real status from the join so just ignore this. */
                break
            },
        }
        match *(total.read().unwrap()) {
            Stats { bytes, files, dirs } if bytes != 0 || files != 0 || dirs != 0 => {
                totals.set_message(format!("Total: freed: {}/{}, directories removed: {}/{}, files removed: {}/{}",
                                           HumanBytes(done.bytes), HumanBytes(bytes),
                                           done.dirs, dirs,
                                           done.files, files));
            },
            _ => {
                totals.set_message(format!("Total: freed: {}, directories removed: {}, files removed: {}",
                                           HumanBytes(done.bytes), done.dirs, done.files));
            },
        }
        progress.set_position(done.files);
        progress.set_length(TOTAL.get().unwrap().read().unwrap().files);
    }

    totals.finish();
    path_spinner.finish_and_clear();
    progress.finish_and_clear();

    match finder.join() {
        Ok(res) => res?,
        Err(e) => panic::resume_unwind(e),
    }
    Ok(())
}

#[derive(Default, Clone, Copy)]
struct Stats {
    bytes: u64,
    files: u64,
    dirs:  u64,
}

impl std::ops::AddAssign for Stats {
    fn add_assign(&mut self, rhs: Self) {
        self.bytes += rhs.bytes;
        self.files += rhs.files;
        self.dirs  += rhs.dirs;
    }
}

enum ToDelete {
    File { size: u64, path: PathBuf },
    Dir(PathBuf),
    Err { path: PathBuf, err: Error },
}

fn find(path: PathBuf, tx: &SyncSender<ToDelete>) -> std::result::Result<Stats, (PathBuf, anyhow::Error)> {
    let mut stats = Stats::default();
    let meta = (&path).symlink_metadata().map_err(|e| (path.clone(), anyhow!("stat: {e}")))?;
    let channel_closed = |e: std::sync::mpsc::SendError<ToDelete>|
        (match e.0 {
            ToDelete::File { path, .. } |
            ToDelete::Dir(path) |
            ToDelete::Err { path, .. } => path,
        }, anyhow!("finder tx channel was closed"));

    if meta.is_dir() {
        let ctx = |e| (path.clone(), anyhow!("read_dir: {e}"));

        // Sort the entries so the user can tell how far we've gotten even if the progress bar isn't
        // going. However, don't waste time and memory sorting directories that are massive. If you've ever
        // done "ls" in a directory and had it take multiple seconds before printing anything you know what
        // we're trying to avoid here. The cutoff point is somewhat arbitrary. We want it high enough that
        // most things get sorted but low enough that the time and memory spent reading the entries and
        // sorting is negligible.
        //
        // If the we're unix we can get the number of directory entries quickly from the nlink stat field. If
        // we're not, then don't bother sorting.
        #[cfg(unix)] use std::os::unix::fs::MetadataExt;
        #[cfg(unix)] let nlink = meta.nlink() as usize;
        #[cfg(not(unix))] let nlink = 5000_usize;
        if nlink < 5000 {
            let mut dirents = Vec::with_capacity(nlink); // oversized by 2 (., ..) but who cares.
            for f in read_dir(&path).map_err(ctx)? {
                dirents.push(f.map_err(ctx)?.path());
            }
            dirents.sort();
            for f in dirents.into_iter() {
                match find(f, tx) {
                    Ok(s) => stats += s,
                    Err((path, err)) => tx.send(ToDelete::Err { path, err }).map_err(channel_closed)?,
                }
            }
        } else {
            for f in read_dir(&path).map_err(ctx)? {
                let dirent = f.map_err(ctx)?;
                match find(dirent.path(), tx) {
                    Ok(s) => stats += s,
                    Err((path, err)) => tx.send(ToDelete::Err { path, err }).map_err(channel_closed)?,
                }
            }
        }
        {
            let mut tot = TOTAL.get().unwrap().write().unwrap();
            tot.dirs += 1;
            stats.dirs += 1;
        }
        tx.send(ToDelete::Dir(path)).map_err(channel_closed)?;
    } else { // symlinks are more or less just files
        let bytes = meta.len();
        tx.send(ToDelete::File { path, size: bytes }).map_err(channel_closed)?;
        stats += Stats { bytes, files: 1, dirs: 0 };
        {
            let mut tot = TOTAL.get().unwrap().write().unwrap();
            tot.files += 1;
            tot.bytes += bytes;
        }
    }
    Ok(stats)
}
