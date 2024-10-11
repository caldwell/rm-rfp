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
use indicatif::{HumanBytes, MultiProgress, ProgressBar, ProgressStyle};

static TOTAL: OnceLock<RwLock<Stats>> = OnceLock::new();

fn main() -> Result<()> {
    let target: PathBuf = std::env::args().nth(1).ok_or(anyhow!("Usage: rmp <file-or-directory>"))?.into();

    let (to_delete_tx, to_delete_rx) = sync_channel(1_000_000);

    let progress = ProgressBar::new_spinner().with_style(ProgressStyle::with_template("Counting files{spinner:3}  {len}").unwrap()
                                                                                                                         .tick_strings(&[".", "..", "...", ""]));
    let total = Arc::new(RwLock::new(Stats::default()));

    TOTAL.get_or_init(|| RwLock::new(Stats::default()));

    let finder = thread::spawn({
        let progress = progress.clone();
        let total = total.clone();
        move || -> Result<()> {
            let stats = find(target, &to_delete_tx)?;
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
                path_spinner.set_message((*path.to_string_lossy()).to_owned());
                path_spinner.set_prefix("rm");
                done.bytes += size;
                done.files += 1;
            },
            Ok(ToDelete::Dir(path)) => {
                //remove_dir(&path)?;
                sleep(Duration::from_micros(80));
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

fn find(path: PathBuf, tx: &SyncSender<ToDelete>) -> Result<Stats> {
    let mut stats = Stats::default();
    let meta = (&path).symlink_metadata().map_err(|e| anyhow!("stat: {e}"))?;
    if meta.is_dir() {
        let ctx = |e| anyhow!("read_dir: {e}");
        for f in read_dir(&path).map_err(ctx)? {
            let dirent = f.map_err(ctx)?;
            match find(dirent.path(), tx) {
                Ok(s) => stats += s,
                Err(err) => tx.send(ToDelete::Err { path: dirent.path(), err })?,
            }
        }
        {
            let mut tot = TOTAL.get().unwrap().write().unwrap();
            tot.dirs += 1;
            stats.dirs += 1;
        }
        tx.send(ToDelete::Dir(path))?;
    } else { // symlinks are more or less just files
        let bytes = meta.len();
        tx.send(ToDelete::File { path, size: bytes })?;
        stats += Stats { bytes, files: 1, dirs: 0 };
        {
            let mut tot = TOTAL.get().unwrap().write().unwrap();
            tot.files += 1;
            tot.bytes += bytes;
        }
    }
    Ok(stats)
}
