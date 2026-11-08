//! Islet, the package manager for Pelago. Stage 0, card I-1: the store.

mod blake3;
mod store;

use std::path::Path;
use std::process::ExitCode;
use store::{Store, Verdict};

const USAGE: &str = "islet 0.0.1, the package manager for Pelago

usage:
  islet store put <dir>            copy a folder into the store; prints its hash
  islet store path <hash>          print where a stored tree lives
  islet store info                 where the store is, how many trees, how many bytes
  islet store verify [<hash>]      recompute the hash of one tree, or of every tree
  islet store link <hash> <dest>   link a stored tree at <dest> (symlink; junction on Windows)
  islet hash <dir>                 print a folder's hash without storing it

The store is the folder named by ISLET_STORE if set; otherwise `.islet` at the drive root on Windows
and `~/.islet` elsewhere.";

fn run(args: &[String]) -> Result<(), String> {
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    let open = |near: &str| Store::for_project(Path::new(near)).map_err(|e| e.to_string());
    match a.as_slice() {
        ["hash", dir] => {
            println!(
                "{}",
                store::tree_hash(Path::new(dir)).map_err(|e| e.to_string())?
            );
            Ok(())
        }
        ["store", "put", dir] => {
            let h = open(".")?.put(Path::new(dir)).map_err(|e| e.to_string())?;
            println!("{h}");
            Ok(())
        }
        ["store", "path", hash] => {
            let s = open(".")?;
            if !s.contains(hash) {
                return Err(format!("not in the store: {hash}"));
            }
            println!("{}", s.path_of(hash).map_err(|e| e.to_string())?.display());
            Ok(())
        }
        ["store", "info"] => {
            let i = open(".")?.info().map_err(|e| e.to_string())?;
            println!(
                "store  {}\ntrees  {}\nbytes  {}",
                i.root.display(),
                i.trees,
                i.bytes
            );
            Ok(())
        }
        ["store", "verify"] => {
            let s = open(".")?;
            let all = s.list().map_err(|e| e.to_string())?;
            let mut bad = 0;
            for h in &all {
                match s.verify(h).map_err(|e| e.to_string())? {
                    Verdict::Ok => {}
                    Verdict::Mismatch { actual } => {
                        bad += 1;
                        println!("CORRUPT {h} (now hashes to {actual})");
                    }
                }
            }
            println!("{} trees checked, {bad} corrupt", all.len());
            if bad == 0 {
                Ok(())
            } else {
                Err("store verification failed".into())
            }
        }
        ["store", "verify", hash] => match open(".")?.verify(hash).map_err(|e| e.to_string())? {
            Verdict::Ok => {
                println!("ok {hash}");
                Ok(())
            }
            Verdict::Mismatch { actual } => Err(format!("CORRUPT {hash} (now hashes to {actual})")),
        },
        ["store", "link", hash, dest] => {
            open(".")?
                .link(hash, Path::new(dest))
                .map_err(|e| e.to_string())?;
            println!("linked {dest}");
            Ok(())
        }
        _ => Err(USAGE.to_string()),
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(m) => {
            eprintln!("{m}");
            ExitCode::from(if m == USAGE { 2 } else { 1 })
        }
    }
}
