//! GitHub Copilot CLI doesn't speak OSC 7501, so it never showed up as an
//! agent. Its hooks do run at every turn; `hooks/herdr.json` in Copilot's
//! folder has each one report the matching status to the terminal itself.
//! Only inside a core terminal (`HC_TERMINAL_ID`); elsewhere they stay quiet.
//! Known gaps: nothing fires when a turn is interrupted (it stays working
//! until the next prompt), or right after a permission is granted (it stays
//! blocked until that tool ends).

use serde_json::{Value, json};

/// Copilot's hook → the 7501 state it reports, as `sh` that sets `s`
/// (or exits when there is nothing to say).
const EVENTS: &[(&str, &str)] = &[
    ("sessionStart", "s=idle"),
    ("userPromptSubmitted", "s=working"),
    // Back from a permission prompt or a question (both are tools).
    ("postToolUse", "s=working"),
    ("postToolUseFailure", "s=working"),
    (
        "notification",
        "case $in in *'\"permission_prompt\"'*) s='blocked:kind=permission' ;; *'\"elicitation_dialog\"'*) s='blocked:kind=question' ;; *) exit 0 ;; esac",
    ),
    // Copilot retries recoverable errors on its own.
    ("errorOccurred", "case $in in *'\"recoverable\":false'*|*'\"recoverable\": false'*) s=error ;; *) exit 0 ;; esac"),
    ("agentStop", "s=done"),
    ("sessionEnd", "s=clear"),
];

fn command(pick: &str) -> String {
    format!(
        "in=$(cat); [ -n \"$HC_TERMINAL_ID\" ] || exit 0; {pick}; printf '\\033]7501;state=%s:app=copilot\\033\\\\' \"$s\" 2>/dev/null >/dev/tty; exit 0"
    )
}

fn hooks() -> Value {
    let hooks: serde_json::Map<String, Value> = EVENTS
        .iter()
        .map(|(event, pick)| ((*event).to_owned(), json!([{ "type": "command", "bash": command(pick), "timeoutSec": 5 }])))
        .collect();
    json!({ "version": 1, "hooks": hooks })
}

/// Copilot's folder: `COPILOT_HOME`, else `~/.copilot`.
fn folder() -> Option<std::path::PathBuf> {
    std::env::var_os("COPILOT_HOME").filter(|v| !v.is_empty()).map(Into::into).or_else(|| std::env::home_dir().map(|h| h.join(".copilot")))
}

/// Writes the hooks when Copilot is set up here (its folder exists); rewritten when they change.
pub fn install_hooks() {
    // Windows has no /dev/tty for a hook to report through.
    if cfg!(windows) {
        return;
    }
    if let Some(dir) = folder() {
        install(&dir);
    }
}

fn install(dir: &std::path::Path) {
    if !dir.is_dir() {
        return;
    }
    let file = dir.join("hooks").join("herdr.json");
    let body = serde_json::to_string_pretty(&hooks()).expect("hooks json") + "\n";
    if std::fs::read_to_string(&file).ok().as_deref() != Some(body.as_str()) {
        let _ = std::fs::create_dir_all(dir.join("hooks"));
        let _ = crate::persist::write_atomic(&file, body);
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::io::Write;
    use std::process::{Command, Stdio};

    /// What the hook for `event` writes to the terminal, given Copilot's input.
    fn report(event: &str, input: &str, in_core: bool) -> String {
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let out = std::env::temp_dir().join(format!("herdr-copilot-{}-{n}", std::process::id()));
        std::fs::write(&out, "").unwrap();
        let hooks = super::hooks();
        let bash = hooks["hooks"][event][0]["bash"].as_str().unwrap().replace("/dev/tty", &out.display().to_string());
        let mut sh = Command::new("sh");
        sh.args(["-c", &bash]).stdin(Stdio::piped()).env_remove("HC_TERMINAL_ID");
        if in_core {
            sh.env("HC_TERMINAL_ID", "t1");
        }
        let mut child = sh.spawn().unwrap();
        child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
        assert!(child.wait().unwrap().success(), "{event} exits 0");
        let written = std::fs::read_to_string(&out).unwrap();
        let _ = std::fs::remove_file(&out);
        written
    }

    #[test]
    fn copilot_hooks_report_its_turns_as_7501_in_core_terminals_only() {
        let st = |s: &str| format!("\x1b]7501;state={s}:app=copilot\x1b\\");
        assert_eq!(report("userPromptSubmitted", r#"{"prompt":"hi"}"#, true), st("working"));
        assert_eq!(report("agentStop", "{}", true), st("done"));
        assert_eq!(report("sessionEnd", "{}", true), st("clear"));
        // Copilot's own input, as it sends it.
        let asked = r#"{"message":"Run command: touch x","title":"Permission needed","hook_event_name":"Notification","notification_type":"permission_prompt"}"#;
        assert_eq!(report("notification", asked, true), st("blocked:kind=permission"));
        let question = r#"{"message":"Information requested","notification_type":"elicitation_dialog"}"#;
        assert_eq!(report("notification", question, true), st("blocked:kind=question"));
        assert_eq!(report("notification", r#"{"notification_type":"agent_idle"}"#, true), "");
        assert_eq!(report("errorOccurred", r#"{"error":"x","recoverable":true}"#, true), "");
        assert_eq!(report("errorOccurred", r#"{"error":"x","recoverable":false}"#, true), st("error"));
        assert_eq!(report("userPromptSubmitted", "{}", false), "", "outside a core terminal it stays quiet");
    }

    #[test]
    fn copilot_hooks_install_only_where_copilot_is_set_up() {
        let dir = std::env::temp_dir().join(format!("herdr-copilot-home-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        super::install(&dir);
        assert!(!dir.exists(), "no Copilot folder: nothing written");
        std::fs::create_dir_all(&dir).unwrap();
        super::install(&dir);
        let file: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(dir.join("hooks/herdr.json")).unwrap()).unwrap();
        assert_eq!(file, super::hooks());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
