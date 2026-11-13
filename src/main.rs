mod app;
mod ipc;
mod kitty;
mod layout;
mod nvim;
mod render;
mod synctex;
mod terminal;
mod text;
mod watch;

use std::{ffi::OsString, path::PathBuf};

const USAGE: &str = "usage: pdfty [file.pdf ...] [--synctex] [--no-debug-lines]
       pdfty <file.pdf> --get-socket
       pdfty --list | -l
       pdfty --has-source <file.pdf> <source.tex>

Environment: PDFTY_TRANSFER=file|direct selects how pixels reach the terminal
(default: file locally, direct over SSH).";

/// Parsed CLI modes. Only `View` enters the terminal UI.
enum Action {
    View { pdfs: Vec<PathBuf>, synctex: bool, debug_lines: bool },
    GetSocket(PathBuf),
    List,
    HasSource(PathBuf, PathBuf),
    Help,
}

fn main() {
    match run() {
        Ok(code) => std::process::exit(code),
        Err(error) => {
            eprintln!("pdfty: {error}");
            std::process::exit(2);
        }
    }
}

fn parse_args(args: impl IntoIterator<Item = OsString>) -> Result<Action, String> {
    let mut paths = Vec::new();
    let (mut synctex, mut get_socket, mut list, mut help, mut has_source) = (false, false, false, false, false);
    let mut debug_lines = true;
    for arg in args {
        match arg.to_str() {
            Some("--synctex") => synctex = true,
            Some("--get-socket") => get_socket = true,
            Some("--list" | "-l") => list = true,
            Some("--help" | "-h") => help = true,
            Some("--has-source") => has_source = true,
            Some("--debug-lines") => debug_lines = true,
            Some("--no-debug-lines") => debug_lines = false,
            Some(option) if option.starts_with('-') => return Err(format!("unknown option: {option}\n{USAGE}")),
            _ => paths.push(PathBuf::from(arg)),
        }
    }
    let modes = [get_socket, list, help, has_source].iter().filter(|&&m| m).count();
    if modes > 1 {
        return Err(USAGE.into());
    }
    if help {
        return if paths.is_empty() && !synctex { Ok(Action::Help) } else { Err(USAGE.into()) };
    }
    if list {
        return if paths.is_empty() && !synctex { Ok(Action::List) } else { Err(USAGE.into()) };
    }
    if has_source {
        return match <[PathBuf; 2]>::try_from(paths) {
            Ok([pdf, source]) => Ok(Action::HasSource(pdf, source)),
            Err(_) => Err(USAGE.into()),
        };
    }
    if get_socket {
        return match <[PathBuf; 1]>::try_from(paths) {
            Ok([pdf]) => Ok(Action::GetSocket(pdf)),
            Err(_) => Err(USAGE.into()),
        };
    }
    // A blank viewer enables IPC so Neovim can add its first PDF.
    Ok(Action::View { synctex: synctex || paths.is_empty(), pdfs: paths, debug_lines })
}

/// The canonical path is shared by the socket hash and the IPC identity check,
/// so relative paths and symlinks refer to one viewer.
fn canonical_pdf(path: PathBuf) -> Result<PathBuf, String> {
    let pdf = std::fs::canonicalize(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    if !pdf.extension().is_some_and(|x| x.eq_ignore_ascii_case("pdf")) {
        return Err(format!("{}: expected a .pdf file", path.display()));
    }
    Ok(pdf)
}

fn run() -> Result<i32, String> {
    match parse_args(std::env::args_os().skip(1))? {
        Action::Help => println!("{USAGE}"),
        Action::List => {
            for (socket, pdf) in ipc::active_sockets()? {
                match pdf {
                    Some(pdf) => println!("{}\t{}", socket.display(), pdf.display()),
                    None => println!("{}", socket.display()),
                }
            }
        }
        Action::GetSocket(pdf) => println!("{}", ipc::socket_path(&canonical_pdf(pdf)?).display()),
        Action::HasSource(pdf, source) => {
            let found = synctex::Synctex::load(&pdf).is_ok_and(|s| s.has_source(&source));
            return Ok(if found { 0 } else { 1 });
        }
        Action::View { pdfs, synctex, debug_lines } => {
            let mut unique = Vec::new();
            for pdf in pdfs {
                let pdf = canonical_pdf(pdf)?;
                if !unique.contains(&pdf) {
                    unique.push(pdf);
                }
            }
            app::run(unique, synctex, debug_lines)?;
        }
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::{parse_args, Action};
    use std::ffi::OsString;

    fn parse(args: &[&str]) -> Result<Action, String> {
        parse_args(args.iter().map(OsString::from))
    }

    #[test]
    fn cli_modes_are_distinct() {
        assert!(matches!(parse(&["-l"]), Ok(Action::List)));
        assert!(matches!(parse(&["--list"]), Ok(Action::List)));
        assert!(matches!(parse(&["--get-socket", "a.pdf"]), Ok(Action::GetSocket(_))));
        assert!(matches!(parse(&["a.pdf", "--get-socket"]), Ok(Action::GetSocket(_))));
        assert!(matches!(parse(&["a.pdf"]), Ok(Action::View { synctex: false, .. })));
        assert!(matches!(parse(&["--synctex", "a.pdf"]), Ok(Action::View { synctex: true, .. })));
        assert!(matches!(parse(&[]), Ok(Action::View { synctex: true, pdfs, .. }) if pdfs.is_empty()));
        assert!(matches!(parse(&["a.pdf", "b.pdf", "--synctex"]), Ok(Action::View { synctex: true, pdfs, .. }) if pdfs.len() == 2));
        assert!(matches!(parse(&["--has-source", "a.pdf", "a.tex"]), Ok(Action::HasSource(..))));
        assert!(parse(&["--has-source", "a.pdf"]).is_err());
        assert!(parse(&["--list", "a.pdf"]).is_err());
        assert!(parse(&["--get-socket", "a.pdf", "b.pdf"]).is_err());
        assert!(parse(&["--list", "--get-socket", "a.pdf"]).is_err());
        assert!(parse(&["--bogus"]).is_err());
        assert!(matches!(parse(&["a.pdf"]), Ok(Action::View { debug_lines: true, .. })));
        assert!(matches!(parse(&["--no-debug-lines", "a.pdf"]), Ok(Action::View { debug_lines: false, .. })));
    }
}
