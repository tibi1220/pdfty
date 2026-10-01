mod app;
mod cache;
mod image;
mod ipc;
mod reload;
mod render;
mod resolution;
mod synctex;
mod terminal;
mod text;

use std::{env, ffi::OsString, path::PathBuf, process::Command};

const USAGE: &str = "usage: pdfty [file.pdf ...] [--synctex]\n       pdfty <file.pdf> --get-socket\n       pdfty --list | -l";

/// Parsed CLI modes. Socket lookup and listing never enter the terminal UI.
enum Action {
    View { pdfs: Vec<PathBuf>, synctex: bool },
    GetSocket(PathBuf),
    List,
    Help,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("pdfty: {error}");
        std::process::exit(1);
    }
}

fn parse_args(args: impl IntoIterator<Item = OsString>) -> Result<Action, String> {
    let mut pdfs = Vec::new();
    let mut synctex = false;
    let mut get_socket = false;
    let mut list = false;
    let mut help = false;
    for arg in args {
        match arg.to_str() {
            Some("--synctex") => synctex = true,
            Some("--get-socket") => get_socket = true,
            Some("--list" | "-l") => list = true,
            Some("--help" | "-h") => help = true,
            Some(option) if option.starts_with('-') => {
                return Err(format!("unknown option: {option}\n{USAGE}"))
            }
            _ => pdfs.push(PathBuf::from(arg)),
        }
    }
    if help && !list && !get_socket && !synctex && pdfs.is_empty() {
        return Ok(Action::Help);
    }
    if list && pdfs.is_empty() && !get_socket && !synctex && !help {
        return Ok(Action::List);
    }
    if list || help {
        return Err(USAGE.into());
    }
    if get_socket {
        if pdfs.len() != 1 {
            return Err(USAGE.into());
        }
        Ok(Action::GetSocket(pdfs.remove(0)))
    } else {
        Ok(Action::View {
            synctex: synctex || pdfs.is_empty(),
            pdfs,
        })
    }
}

fn canonical_pdf(path: PathBuf) -> Result<PathBuf, String> {
    // The canonical path is shared by the socket hash and IPC identity check.
    // Resolving it here makes relative paths and symlinks refer to one viewer.
    let pdf = std::fs::canonicalize(path).map_err(|e| e.to_string())?;
    if pdf.extension().and_then(|x| x.to_str()) != Some("pdf") {
        return Err("expected a .pdf file".into());
    }
    Ok(pdf)
}

fn run() -> Result<(), String> {
    match parse_args(env::args_os().skip(1))? {
        Action::Help => println!("{USAGE}"),
        Action::List => {
            for (socket, pdf) in ipc::active_sockets()? {
                if let Some(pdf) = pdf {
                    println!("{}\t{}", socket.display(), pdf.display());
                } else {
                    println!("{}", socket.display());
                }
            }
        }
        Action::GetSocket(pdf) => println!("{}", ipc::socket_path(&canonical_pdf(pdf)?).display()),
        Action::View { pdfs, synctex } => {
            let pdfs = pdfs
                .into_iter()
                .map(canonical_pdf)
                .collect::<Result<Vec<_>, _>>()?;
            let mut required = vec!["mutool", "pdfinfo", "kitten"];
            if synctex {
                required.extend(["synctex", "nvr"]);
            }
            for tool in required {
                if Command::new("which")
                    .arg(tool)
                    .output()
                    .map(|o| !o.status.success())
                    .unwrap_or(true)
                {
                    return Err(format!("missing command: {tool}"));
                }
            }
            // A blank viewer enables IPC so Neovim can add its first PDF.
            app::run(pdfs, synctex)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{parse_args, Action};
    use std::ffi::OsString;

    fn parse(args: &[&str]) -> Action {
        parse_args(args.iter().map(OsString::from)).unwrap()
    }

    #[test]
    fn cli_modes_are_distinct() {
        assert!(matches!(parse(&["-l"]), Action::List));
        assert!(matches!(parse(&["--list"]), Action::List));
        assert!(matches!(
            parse(&["--get-socket", "sample/build/main.pdf"]),
            Action::GetSocket(_)
        ));
        assert!(matches!(
            parse(&["sample/build/main.pdf", "--get-socket"]),
            Action::GetSocket(_)
        ));
        assert!(matches!(
            parse(&["sample/build/main.pdf"]),
            Action::View { synctex: false, .. }
        ));
        assert!(matches!(
            parse(&["--synctex", "sample/build/main.pdf"]),
            Action::View { synctex: true, .. }
        ));
        assert!(matches!(parse(&[]), Action::View { synctex: true, pdfs } if pdfs.is_empty()));
        assert!(
            matches!(parse(&["a.pdf", "b.pdf", "--synctex"]), Action::View { synctex: true, pdfs } if pdfs.len() == 2)
        );
        assert!(matches!(
            parse(&["--synctex", "--get-socket", "sample/build/main.pdf"]),
            Action::GetSocket(_)
        ));
        assert!(parse_args(["--list".into(), "sample/build/main.pdf".into()]).is_err());
        assert!(parse_args(["--get-socket".into(), "a.pdf".into(), "b.pdf".into()]).is_err());
    }
}
