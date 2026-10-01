//! Focus the terminal belonging to the Neovim server used for inverse search.
//! Query the editor environment instead of assuming the viewer's neighboring
//! pane is the editor. tmux can select a different session/window/pane; Kitty
//! remote control handles a distinct terminal window when necessary.

use std::{env, process::Command};

#[derive(serde::Deserialize)]
struct EditorTerminal {
    tmux: String,
    pane: String,
    kitty: String,
    listen: String,
}

/// Focus only the terminal owned by the server that received the source jump.
pub(super) fn focus_nvim(server: &str, record_call: &mut impl FnMut(String)) -> Result<(), String> {
    // Query the server that received the source jump, so focus follows the
    // paired editor even when several Neovim instances are running.
    let expression = "json_encode({'tmux': $TMUX, 'pane': $TMUX_PANE, 'kitty': $KITTY_WINDOW_ID, 'listen': $KITTY_LISTEN_ON})";
    record_call(format!(
        "nvr --nostart --servername {server:?} --remote-expr {expression:?}"
    ));
    let output = Command::new("nvr")
        .args([
            "--nostart",
            "--servername",
            server,
            "--remote-expr",
            expression,
        ])
        .output()
        .map_err(|error| error.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().into());
    }
    let editor: EditorTerminal = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("read Neovim terminal: {error}"))?;
    let mut focused = false;
    if !editor.tmux.is_empty() && !editor.pane.is_empty() {
        let socket = tmux_socket(&editor.tmux).ok_or("invalid Neovim tmux address")?;
        record_call(format!(
            "tmux -S {socket:?} switch-client -t {:?}",
            editor.pane
        ));
        run_focus(Command::new("tmux").args(["-S", socket, "switch-client", "-t", &editor.pane]))?;
        focused = true;
    }
    if !editor.kitty.is_empty()
        && (!focused || env::var("KITTY_WINDOW_ID").ok().as_deref() != Some(editor.kitty.as_str()))
    {
        let id = editor
            .kitty
            .parse::<u64>()
            .map_err(|_| "invalid Neovim Kitty window ID")?;
        let mut command = Command::new("kitten");
        command.arg("@");
        if !editor.listen.is_empty() {
            command.args(["--to", &editor.listen]);
        }
        let target = format!("id:{id}");
        command.args(["focus-window", "--match", &target]);
        record_call(format!("kitten @ focus-window --match {target}"));
        run_focus(&mut command)?;
        focused = true;
    }
    if focused {
        Ok(())
    } else {
        Err("Neovim has no tmux pane or Kitty window ID".into())
    }
}

/// Read the socket field from tmux's environment value without truncating comma-containing paths.
pub(super) fn tmux_socket(address: &str) -> Option<&str> {
    // TMUX is socket,pid,session; the socket path itself may contain commas.
    address
        .rsplitn(3, ',')
        .nth(2)
        .filter(|socket| !socket.is_empty())
}

/// Capture errors so a failed focus change can be reported in the viewer footer.
fn run_focus(command: &mut Command) -> Result<(), String> {
    let output = command.output().map_err(|error| error.to_string())?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "{}: {}",
            command.get_program().to_string_lossy(),
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}
