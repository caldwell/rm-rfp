// Copyright © 2024 David Caldwell <david@porkrind.org>

use std::{fs::{read_dir, remove_dir, remove_file, Metadata},
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
        let multi = multi.clone();
        move || -> Result<()> {
            let interactive = Interactive::new(args.flag_interactive, move |f| multi.suspend(|| f()));
            let mut finder = Find::new(&to_delete_tx, interactive);
            for path in paths {
                finder.find(path).map_err(|(path, err)| anyhow!("{path:?};{err}"))?;
                finder.interactive.reset_state();
            }
            TOTAL.done.store(true, Ordering::Relaxed);
            progress.set_length(TOTAL.files.load(Ordering::Relaxed));
            progress.set_style(ProgressStyle::with_template("{elapsed_precise} {wide_bar:.on_cyan/on_17} {eta_precise}").unwrap()
                                                                                                                        .progress_chars("   "));
            Ok(())
        }
    });

    let mut done = Stats::default();
    loop {
        match to_delete_rx.recv() {
            Ok(ToDelete::File { size, path }) => {
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

impl ToDelete {
    fn path(self) -> PathBuf {
        match self {
            ToDelete::File { path, .. } | ToDelete::Dir(path) | ToDelete::Err { path, .. } => path
        }
    }
}

struct Find<'a> {
    tx: &'a SyncSender<ToDelete>,
    interactive: Interactive,
}

type FindResult<T> = std::result::Result<T, (PathBuf, anyhow::Error)>;

impl<'a> Find<'a> {
    fn new(tx: &'a SyncSender<ToDelete>, interactive: Interactive) -> Find<'a> {
        Find { tx, interactive }
    }

    fn find(&mut self, path: PathBuf) -> FindResult<bool> {
        let meta = (&path).symlink_metadata().map_err(|e| (path.clone(), anyhow!("stat: {e}")))?;
        fn channel_closed(e: std::sync::mpsc::SendError<ToDelete>) -> (PathBuf, anyhow::Error) {
            (e.0.path(), anyhow!("finder tx channel was closed"))
        }

        if self.interactive.ask(&path, &meta, true)? == Directive::Skip { return Ok(true) }

        if meta.is_dir() {
            let mut skipped_any = false;
            for dirent in Self::readdir_sorted(&path, &meta)? {
                match self.find(dirent?) {
                    Err((path, err)) => self.tx.send(ToDelete::Err { path, err }).map_err(channel_closed)?,
                    Ok(true) => skipped_any = true,
                    Ok(false) => {},
                }
            }

            if skipped_any { return Ok(true) } // Directory is not empty so don't bother asking or trying to delete it.
            if self.interactive.ask(&path, &meta, true)? == Directive::Skip { return Ok(true) }

            TOTAL.dirs.fetch_add(1, Ordering::Relaxed);
            self.tx.send(ToDelete::Dir(path)).map_err(channel_closed)?;
        } else { // symlinks are more or less just files
            let bytes = meta.len();
            self.tx.send(ToDelete::File { path, size: bytes }).map_err(channel_closed)?;
            TOTAL.files.fetch_add(1, Ordering::Relaxed);
            TOTAL.bytes.fetch_add(bytes, Ordering::Relaxed);
        }
        Ok(false)
    }

    fn readdir_sorted<'p>(path: &'p Path, meta: &Metadata) -> FindResult<Box<dyn Iterator<Item=FindResult<PathBuf>> + 'p>> {
        let ctx = |e| (path.to_owned(), anyhow!("read_dir: {e}"));

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

            return Ok(Box::new(dirents.into_iter().map(|ent| Ok(ent))));
        } else {
            return Ok(Box::new(read_dir(&path).map_err(ctx)?
                                              .map(|res_de| res_de.map(|de| de.path())
                                                                  .map_err(|e| (path.to_owned(), anyhow!(e))))))
        }
    }
}

#[derive(PartialEq, Eq, Debug)]
pub enum Response {
    Delete,
    Skip,
    DeleteFromNowOn,
    Quit,
    DeleteThisDir(PathBuf),
    SkipThisDir(PathBuf),
}

#[derive(PartialEq, Eq, Debug)]
pub enum Directive {
    Delete,
    Skip,
}

pub struct Interactive {
    enable: bool,
    state: Option<Response>,
    ask_ctx: AskerContext,
}

type Asker<'a> = &'a (dyn Fn() -> Result<Response> + 'a);
type AskerContext = Box<dyn Fn(Asker<'_>) -> Result<Response>>;

impl Interactive {
    pub fn new<F>(enable: bool, ask_ctx: F) -> Interactive
    where F: Fn(Asker) -> Result<Response> + 'static,
    {
        Interactive {
            enable,
            ask_ctx: Box::new(ask_ctx),
            state: None,
        }
    }

    /// Called between args to reset the state of DeleteThisDir or SkipThisDir back to None. The only other
    /// states that it could be are DeleteFromNowOn and Quit. Neither of these will be reset.
    ///
    /// The reasoning is that the args are separate and it might be confusing for "everything in this dir" to
    /// also mean some argument down the line that we haven't processed yet.
    pub fn reset_state(&mut self) {
        match self.state {
            Some(Response::Quit) |
            Some(Response::DeleteFromNowOn) => {},
            _ => { self.state = None },
        }
    }

    pub fn ask(&mut self, path: &Path, meta: &Metadata, traverse: bool) -> FindResult<Directive> {
        if self.enable {
            match &self.state {
                Some(Response::DeleteFromNowOn)                                    => { return Ok(Directive::Delete) },
                Some(Response::DeleteThisDir(ref skip)) if is_same_dir(skip, path) => { return Ok(Directive::Delete) },
                Some(Response::SkipThisDir(ref skip))   if is_same_dir(skip, path) => { return Ok(Directive::Skip) },
                Some(Response::Quit)                                               => { return Ok(Directive::Skip) },
                _ => {
                    match (self.ask_ctx)(&|| self.ask_user(&path, &meta, traverse)).map_err(|e| (path.to_owned(), anyhow!(e)))? {
                        Response::Delete             => { return Ok(Directive::Delete) },
                        Response::Skip               => { return Ok(Directive::Skip) },
                        d@Response::DeleteFromNowOn |
                        d@Response::DeleteThisDir(_) =>  { self.state = Some(d); return Ok(Directive::Delete) },
                        d@Response::Quit            |
                        d@Response::SkipThisDir(_)   =>  { self.state = Some(d); return Ok(Directive::Skip) },
                    }
                }
            }
        }
        Ok(Directive::Delete)
    }


  fn ask_user(&self, path: &Path, meta: &Metadata, traverse: bool) -> Result<Response> {
    let (path, prompt) = match (meta.is_dir(), traverse) {
        (false, _) => {
            #[cfg(unix)]
            use std::os::unix::fs::FileTypeExt;
            let ft = meta.file_type();
            (path,
             if ft.is_file() && meta.len() == 0 {
                 format!("remove empty file {path:?}")
             } else if ft.is_file() {
                 format!("remove file {path:?} [{}]", HumanBytes(meta.len()))
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
        (true, true) => (path, format!("descend into directory {path:?}")),
        (true, false) => (path, format!("remove directory {path:?}")),
    };
    loop {
        print!("{}? (y/N/a/q/d/s/?) ", prompt);
        _ = std::io::stdout().flush();
        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;
        if !std::io::stdout().is_terminal() { print!("\n") } // hack to make tests easier
        match input.to_lowercase().trim() {
            "y" => return Ok(Response::Delete),
            ""  | /* default */
            "n" => return Ok(Response::Skip),
            "a" => return Ok(Response::DeleteFromNowOn),
            "q" => return Ok(Response::Quit),
            "d" => return Ok(Response::DeleteThisDir(path.to_owned())),
            "s" => return Ok(Response::SkipThisDir(path.to_owned())),
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
