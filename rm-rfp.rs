// Copyright © 2024 David Caldwell <david@porkrind.org>

use std::{fs::{read_dir, remove_dir, remove_file},
          io::{IsTerminal, Write},
          panic,
          path::{Path, PathBuf},
          sync::{atomic::{AtomicBool, AtomicU64, Ordering},
                 mpsc::{sync_channel, SyncSender}},
          thread::{self, sleep},
          time::Duration};

use anyhow::{anyhow, Error, Result};
use docopt::Docopt;
use indicatif::{HumanBytes, MultiProgress, ProgressBar, ProgressStyle};
use serde::Deserialize;

static TOTAL: AtomicStats = AtomicStats { bytes: AtomicU64::new(0),
                                          files: AtomicU64::new(0),
                                          dirs : AtomicU64::new(0),
                                          done : AtomicBool::new(false) };

fn usage() -> String {
    format!(r#"
Usage:
  rm-rfp --help
  rm-rfp [options] <path>...

Options:
  -h, --help          Show this screen.
  -n, --dry-run       Don't delete anything, but go through the motions as if it were.
  -i, --interactive   Prompt before deleting each file.
"#)
}

#[derive(Debug, Deserialize)]
struct Args {
    flag_dry_run:     bool,
    flag_interactive: bool,
    arg_path:         Vec<PathBuf>,
}

fn main() -> Result<()> {
    let args: Args = Docopt::new(usage())
        .and_then(|d| d.deserialize())
        .unwrap_or_else(|e| e.exit());

    let (to_delete_tx, to_delete_rx) = sync_channel(1_000_000);

    let progress = ProgressBar::new_spinner().with_style(ProgressStyle::with_template("Counting files{spinner:3}  {len}").unwrap()
                                                                                                                         .tick_strings(&[".", "..", "...", ""]));
    let path_spinner = ProgressBar::new_spinner().with_style(ProgressStyle::with_template("{spinner} {prefix:<5.245} {msg:!.214}").unwrap());
    path_spinner.enable_steady_tick(Duration::from_secs_f32(1.0 / 10.0));
    let totals = ProgressBar::new_spinner().with_style(ProgressStyle::with_template("{msg}, elapsed: {elapsed_precise}")?);
    let multi = MultiProgress::new();
    multi.add(progress.clone());
    multi.add(path_spinner.clone());
    multi.add(totals.clone());

    let finder = thread::spawn({
        let progress = progress.clone();
        let paths = args.arg_path.clone();
        move || -> Result<()> {
            for path in paths {
                find(path, &to_delete_tx).map_err(|(path, err)| anyhow!("{path:?};{err}"))?
            }
            TOTAL.done.store(true, Ordering::Relaxed);
            progress.set_length(TOTAL.files.load(Ordering::Relaxed));
            progress.set_style(ProgressStyle::with_template("{elapsed_precise} {wide_bar:.on_cyan/on_17} {eta_precise}").unwrap()
                                                                                                                        .progress_chars("   "));
            Ok(())
        }
    });

    let mut done = Stats::default();
    let mut interactive_state = None;
    loop {
        match to_delete_rx.recv() {
            | Ok(ref f @ ToDelete::File { .. })
            | Ok(ref f @ ToDelete::Dir(_)) => {
                match (args.flag_interactive, &interactive_state, f) {
                    (false, _, _) => { /* delete */ },
                    (true, Some(Directive::DeleteFromNowOn), _) => { /* delete */ },
                    | (true, Some(Directive::SkipThisDir(ref skip)), ToDelete::File { ref path, .. })
                    | (true, Some(Directive::SkipThisDir(ref skip)), ToDelete::Dir(ref path))
                        if is_same_dir(skip, path) => { continue },
                    | (true, Some(Directive::DeleteThisDir(ref skip)), ToDelete::File { ref path, .. })
                    | (true, Some(Directive::DeleteThisDir(ref skip)), ToDelete::Dir(ref path))
                        if is_same_dir(skip, path) => { /* delete */ },
                    (_, _, _) => {
                        match multi.suspend(|| ask(&f))? {
                            Directive::Delete => {},
                            Directive::Skip => continue,
                            Directive::Quit => break,
                            d@Directive::DeleteFromNowOn |
                            d@Directive::DeleteThisDir(_) =>  { interactive_state = Some(d) },
                            d@Directive::SkipThisDir(_) =>  { interactive_state = Some(d); continue },
                        }
                    }
                }
                match f {
                    ToDelete::File { size, path } => {
                        if args.flag_dry_run {
                            sleep(Duration::from_micros(1000));
                        } else {
                            if let Err(e) = remove_file(&path) {
                                _ = multi.println(format!("Couldn't remove {path:?}: {e}"));
                            }
                        }
                        path_spinner.set_message((*path.to_string_lossy()).to_owned());
                        path_spinner.set_prefix("rm");
                        done.bytes += size;
                        done.files += 1;
                    },
                    ToDelete::Dir(path) => {
                        if args.flag_dry_run {
                            sleep(Duration::from_micros(80));
                        } else {
                            if let Err(e) = remove_dir(&path) {
                                _ = multi.println(format!("Couldn't remove directory {path:?}: {e}"));
                            }
                        }
                        path_spinner.set_message((*path.to_string_lossy()).to_owned());
                        path_spinner.set_prefix("rmdir");
                        done.dirs += 1;
                    },
                    ToDelete::Err { .. } => unreachable!(),
                }
            },
            Ok(ToDelete::Err { path, err }) => {
                _ = multi.println(format!("{path:?}: {err}"));
            },
            Err(_) => {  /* read-on-close-channel, ie: done. We'll get the real status from the join so just ignore this. */
                break
            },
        }
        match (TOTAL.done.load(Ordering::Relaxed),
               TOTAL.bytes.load(Ordering::Relaxed),
               TOTAL.files.load(Ordering::Relaxed),
               TOTAL.dirs.load(Ordering::Relaxed)) {
            (true, bytes, files, dirs) => {
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
        progress.set_length(TOTAL.files.load(Ordering::Relaxed));
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


struct AtomicStats {
    bytes: AtomicU64,
    files: AtomicU64,
    dirs:  AtomicU64,
    done:  AtomicBool,
}

#[derive(Default, Clone, Copy)]
struct Stats {
    bytes: u64,
    files: u64,
    dirs:  u64,
}


enum ToDelete {
    File { size: u64, path: PathBuf },
    Dir(PathBuf),
    Err { path: PathBuf, err: Error },
}

fn find(path: PathBuf, tx: &SyncSender<ToDelete>) -> std::result::Result<(), (PathBuf, anyhow::Error)> {
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
                if let Err((path,err)) = find(f, tx) {
                    tx.send(ToDelete::Err { path, err }).map_err(channel_closed)?;
                }
            }
        } else {
            for f in read_dir(&path).map_err(ctx)? {
                let dirent = f.map_err(ctx)?;
                if let Err((path, err)) = find(dirent.path(), tx) {
                    tx.send(ToDelete::Err { path, err }).map_err(channel_closed)?;
                }
            }
        }
        TOTAL.dirs.fetch_add(1, Ordering::Relaxed);
        tx.send(ToDelete::Dir(path)).map_err(channel_closed)?;
    } else { // symlinks are more or less just files
        let bytes = meta.len();
        tx.send(ToDelete::File { path, size: bytes }).map_err(channel_closed)?;
        TOTAL.files.fetch_add(1, Ordering::Relaxed);
        TOTAL.bytes.fetch_add(bytes, Ordering::Relaxed);
    }
    Ok(())
}

enum Directive {
    Delete,
    Skip,
    DeleteFromNowOn,
    Quit,
    DeleteThisDir(PathBuf),
    SkipThisDir(PathBuf),
}

fn ask(item: &ToDelete) -> Result<Directive> {
    let (path, prompt) = match item {
        ToDelete::File { size, path } => {
            #[cfg(unix)]
            use std::os::unix::fs::FileTypeExt;
            let ft = path.symlink_metadata().map_err(|e| anyhow!("interactive: stat {path:?}: {e}"))?.file_type();
            (path,
             if ft.is_file() && *size == 0 {
                 format!("remove empty file {path:?}")
             } else if ft.is_file() {
                 format!("remove file {path:?} [{}]", HumanBytes(*size))
             } else if ft.is_symlink() {
                 format!("remove symbolic link {path:?}")
             } else {
                 #[cfg(unix)]
                 if ft.is_fifo() {
                     format!("remove fifo {path:?}")
                 } else if ft.is_socket() {
                     format!("remove socket {path:?}")
                 } else if ft.is_char_device() {
                     format!("remove character device {path:?}")
                 } else if ft.is_block_device() {
                     format!("remove block device {path:?}")
                 } else {
                     format!("remove unknown file {path:?}") // can't happen?
                 }

                 #[cfg(not(unix))]
                 format!("remove {path:?}")
             })
        }
        ToDelete::Dir(path) => (path, format!("remove dir {}", path.to_string_lossy())),
        ToDelete::Err { .. } => unreachable!(),
    };
    loop {
        print!("{}? (y/N/a/q/d/s/?) ", prompt);
        _ = std::io::stdout().flush();
        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;
        if !std::io::stdout().is_terminal() { print!("\n") } // hack to make tests easier
        match input.to_lowercase().trim() {
            "y" => return Ok(Directive::Delete),
            ""  | /* default */
            "n" => return Ok(Directive::Skip),
            "a" => return Ok(Directive::DeleteFromNowOn),
            "q" => return Ok(Directive::Quit),
            "d" => return Ok(Directive::DeleteThisDir(path.to_owned())),
            "s" => return Ok(Directive::SkipThisDir(path.to_owned())),
            "?" => println!("y - Yes, delete it\n\
                             n - No, don't delete it\n\
                             a - Delete this and everything else (without any further prompts)\n\
                             q - Quit without deleting this nor anything else\n\
                             d - Delete this and the rest of its directory without further prompts\n\
                             s - Don't delete this or anything else in its directory, but continue asking about other items\n\
                             ? - Show help"),
            _ => println!("Bad input. Enter \"?\" for help"),
        }
    }
}

fn is_same_dir(p1: &Path, p2: &Path) -> bool {
    match (p1.parent(), p2) {
        (Some(dir), path) => {
            for p in path.ancestors() {
                if p == dir { return true }
            }
            false
        },
        (None, _) => false,
    }
}
