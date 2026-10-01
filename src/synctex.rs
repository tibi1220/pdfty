//! SyncTeX point/source conversion and source jumps through nvr.
//! Focus is an optional final step, performed only after a successful jump.

mod focus;

use std::{fs, path::Path, process::Command};

/// Forward-search result in PDF page coordinates (points, origin at top left).
pub struct Position {
    pub page: u32,
    pub x: f64,
    pub y: f64,
}
/// Inverse-search target to open through the VimTeX Neovim server.
pub struct Source {
    pub file: String,
    pub line: u32,
}

fn field<'a>(text: &'a str, name: &str) -> Option<&'a str> {
    text.lines()
        .find_map(|line| line.strip_prefix(name).map(str::trim))
}

pub fn forward(pdf: &Path, source: &Path, line: u32, column: u32) -> Result<Position, String> {
    // `synctex view` resolves the source location using the SyncTeX database
    // beside the PDF, regardless of the build output directory's name.
    let input = format!("{line}:{column}:{}", source.display());
    let output = Command::new("synctex")
        .args(["view", "-i", &input, "-o"])
        .arg(pdf)
        .output()
        .map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(&output.stdout);
    if !output.status.success() || !text.contains("SyncTeX result begin") {
        return Err(format!(
            "forward SyncTeX: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(Position {
        page: field(&text, "Page:")
            .ok_or("missing SyncTeX page")?
            .parse()
            .map_err(|_| "bad SyncTeX page")?,
        x: field(&text, "x:")
            .ok_or("missing SyncTeX x")?
            .parse()
            .map_err(|_| "bad SyncTeX x")?,
        y: field(&text, "y:")
            .ok_or("missing SyncTeX y")?
            .parse()
            .map_err(|_| "bad SyncTeX y")?,
    })
}

pub fn inverse(pdf: &Path, page: u32, x: f64, y: f64) -> Result<Source, String> {
    // x/y must already be converted from terminal cells to PDF points.
    let input = format!("{page}:{x:.2}:{y:.2}:{}", pdf.display());
    let output = Command::new("synctex")
        .args(["edit", "-o", &input])
        .output()
        .map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(&output.stdout);
    if !output.status.success() || !text.contains("SyncTeX result begin") {
        return Err(format!(
            "inverse SyncTeX: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(Source {
        file: field(&text, "Input:")
            .ok_or("missing SyncTeX source")?
            .into(),
        line: field(&text, "Line:")
            .ok_or("missing SyncTeX line")?
            .parse()
            .map_err(|_| "bad SyncTeX line")?,
    })
}

/// Open the resolved source in the registered editor, using the VimTeX server file
/// only as a fallback. Refocusing is a final step after a successful source jump.
pub fn open_nvim(
    source: &Source,
    registered_server: Option<&str>,
    refocus: bool,
    mut record_call: impl FnMut(String),
) -> Result<(), String> {
    let server = match registered_server.filter(|server| !server.trim().is_empty()) {
        Some(server) => server.to_owned(),
        None => match fs::read_to_string("/tmp/vimtexserver.txt") {
            Ok(server) => server,
            Err(error) => {
                record_call("nvr --servername </tmp/vimtexserver.txt unavailable>".into());
                return Err(format!("read /tmp/vimtexserver.txt: {error}"));
            }
        },
    };
    let server = server.trim();
    if server.is_empty() {
        record_call("nvr --servername </tmp/vimtexserver.txt empty>".into());
        return Err("/tmp/vimtexserver.txt is empty".into());
    }
    record_call(format!(
        "nvr --nostart --servername {server:?} --remote-solent {:?} -c {}",
        source.file, source.line
    ));
    // Pass every argument directly, so source paths with spaces or shell
    // metacharacters remain a single filename. `-c N` jumps to line N.
    // --nostart keeps a dead server from silently launching another Neovim.
    let output = Command::new("nvr")
        .args([
            "--nostart",
            "--servername",
            server,
            "--remote-solent",
            &source.file,
            "-c",
        ])
        .arg(source.line.to_string())
        .output()
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().into());
    }
    if refocus {
        focus::focus_nvim(server, &mut record_call)
            .map_err(|error| format!("source opened; focus: {error}"))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::focus::tmux_socket;
    use super::{forward, inverse};
    use std::{fs, path::Path};

    #[test]
    fn editor_tmux_address_preserves_socket_paths_with_commas() {
        assert_eq!(
            tmux_socket("/tmp/tmux-501/default,123,0"),
            Some("/tmp/tmux-501/default")
        );
        assert_eq!(
            tmux_socket("/tmp/a,b/socket,123,0"),
            Some("/tmp/a,b/socket")
        );
        assert_eq!(tmux_socket("invalid"), None);
    }

    #[test]
    fn sample_forward_and_inverse_round_trip() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let source = root.join("sample/main.tex");
        let pdf = root.join("sample/build/main.pdf");
        let synctex = root.join("sample/build/main.synctex.gz");
        assert!(pdf.is_file() && synctex.is_file(), "build the sample with `cd sample && latexmk -lualatex -synctex=1 -outdir=build main.tex`");
        let tex = fs::read_to_string(&source).unwrap();
        let anchors = ["Lorem ipsum dolor sit amet", "\\section{Some tikz picture}"];
        for anchor in anchors {
            let line = tex
                .lines()
                .position(|text| text.starts_with(anchor))
                .expect("sample anchor is missing") as u32
                + 1;
            let position = forward(&pdf, &source, line, 1).unwrap();
            assert!((1..=crate::render::page_count(&pdf).unwrap()).contains(&position.page));
            assert!(position.x.is_finite() && position.y.is_finite());
            if anchor == "Lorem ipsum dolor sit amet" {
                let lines = crate::text::extract(&pdf, position.page).unwrap();
                let paragraph = crate::text::paragraph_at(&lines, position.x, position.y)
                    .expect("forward SyncTeX target should find its paragraph");
                assert!(paragraph.h > 20.0);
            }
            let result = inverse(&pdf, position.page, position.x, position.y).unwrap();
            assert_eq!(
                fs::canonicalize(result.file).unwrap(),
                fs::canonicalize(&source).unwrap()
            );
            assert_eq!(result.line, line, "wrong source line for {anchor}");
        }
    }
}
