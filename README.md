rm-rfp
======

Like `rm -rf` but with progress.

`rm-rfp` is a simple utility for deleting large filesystem hierarchies. Deleting
lots of files and directories can take a long time and using `rm -rfv` can slow
things down (since it prints _every_ file it encounters), and fill up your
terminal history. `rm-rfp` tries to fix that by showing what it is currently
deleting and some stats on just a couple terminal lines. It doesn't update these
lines for every file it deletes to minimize the terminal slowing down the
deletion progress. If possible† it will also show a progress bar and an ETA.


† A progress bar is only shown if it can count all the files and directories to
be deleted before it finishes deleting them. In practice this depends on the
filesystem and underlying storage medium (HDD, SSD, Network share, etc). If
deleting the files is faster than finding them then it will never get ahead and
be able to show the progress bar (or it will appear in the last few milliseconds,
which isn't super helpful).

Usage
-----

      rm-rfp [options] <path>...

    Options:

      -h, --help          Show this screen.
      -n, --dry-run       Don't delete anything, but go through the motions as if it were.
      -i, --interactive   Prompt before deleting each file.
      --no-preserve-root  Don't fail if '/' is given as an argument.

### Options:

#### `--dry-run`

Go through all the motions but don't actually delete anything. This is mostly
useful for debugging `rm-rfp` itself, but can give you a sense of what the
program does and how its progress stats work.

#### `--interactive`

This is very similar to `rm -i` but adds a few more prompts (rather than just 'Yes' or 'No'):

    y - Yes, delete it
    n - No, don't delete it
    a - Delete this and everything else (without any further prompts)
    q - Quit without deleting this nor anything else
    d - Delete this and the rest of its directory without further prompts
    s - Don't delete this or anything else in its directory, but continue asking about other items
    ? - Show help

The goal is to give a little more power when checking if a delete command is
correct. Once you've interactively confirmed enough things to believe you got
the command line arguments correct you can use `a` to effectively turn off
interactive mode and delete everything else. Similarly `q` is there if it looks
like it wants to delete more than you intended and you'd just like to quit. `d`
and `s` let you delete or skip everything in a specific directory (and all of
its children) but you'd still like to be prompted for other directories.

#### `--no-preserve-root`

This is the same safety measure that POSIX specifies for `rm`.

    rm-rfp /

This command will fail unless the `--no-preserve-root` option is set.

This option _also_ acts like the `--preserve-root=all` parameter in GNU
coreutils `rm`. This means that by default `rm-rfp` will fail if one of the
directories given on the command line is the root of a mounted filesystem. For
instance, if you have an external hard drive on macOS mounted at /Volumes/hd
then this command will fail:

    rm -rfp /Volumes/hd

Again, adding `--no-preserve-root` will bypass this check.

Building From Source
--------------------

`rm-rfp` is written in Rust so compiling it should just be a matter of:

    # Debug build:
    cargo build

    # Optimized build:
    cargo build --release

License
-------

Copyright © 2024 David Caldwell <david_rm-rfp@porkrind.org>

*TLDR: [GPLv3](/LICENSE.md). You can redistribute the binary (or a
modified version) as long as you ship the source code used to build it
alongside.*

This program is free software: you can redistribute it and/or modify
it under the terms of the GNU General Public License as published by
the Free Software Foundation, either version 3 of the License, or
(at your option) any later version.

This program is distributed in the hope that it will be useful,
but WITHOUT ANY WARRANTY; without even the implied warranty of
MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
GNU General Public License for more details.

You should have received a copy of the GNU General Public License
along with this program.  If not, see <https://www.gnu.org/licenses/>.
