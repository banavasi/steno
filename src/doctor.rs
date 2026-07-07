use anyhow::Result;
use cpal::traits::{DeviceTrait, HostTrait};
use serde::Serialize;
use std::process::Command;

#[derive(Serialize)]
struct Check {
    name: &'static str,
    ok: bool,
    detail: String,
}

fn cmd_version(bin: &str, args: &[&str]) -> Option<String> {
    Command::new(bin).args(args).output().ok().and_then(|o| {
        o.status.success().then(|| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .next()
                .unwrap_or("")
                .trim()
                .to_string()
        })
    })
}

pub fn run(json: bool) -> Result<()> {
    let mut checks = Vec::new();

    let data = crate::session::data_dir();
    let writable = std::fs::create_dir_all(data.join("sessions")).is_ok();
    checks.push(Check {
        name: "data-dir",
        ok: writable,
        detail: data.display().to_string(),
    });

    let mic = cpal::default_host()
        .default_input_device()
        .and_then(|d| d.description().ok())
        .map(|d| d.name().to_string());
    checks.push(Check {
        name: "mic",
        ok: mic.is_some(),
        detail: mic.unwrap_or_else(|| "no default input device".into()),
    });

    let gcli = cmd_version("gcli", &["--version"]);
    checks.push(Check {
        name: "gcli",
        ok: gcli.is_some(),
        detail: gcli.unwrap_or_else(|| "not on PATH (calendar picker disabled)".into()),
    });

    let mut cal_details = Vec::new();
    let mut cal_ok = false;
    for profile in ["personal", "asu", "oneorigin"] {
        match crate::calendar::list_today(profile) {
            Ok(evs) => {
                cal_ok = true; // at least one working profile
                cal_details.push(format!("{profile}: {} event(s) today", evs.len()));
            }
            Err(e) => {
                let e = e.to_string();
                cal_details.push(format!("{profile}: {}", e.lines().next().unwrap_or("error")));
            }
        }
    }
    checks.push(Check {
        name: "calendar",
        ok: cal_ok,
        detail: cal_details.join(" · "),
    });

    let claude = cmd_version("claude", &["--version"]);
    checks.push(Check {
        name: "claude",
        ok: claude.is_some(),
        detail: claude
            .map(|v| format!("{v} (chat pane + summary, on your subscription)"))
            .unwrap_or_else(|| "claude CLI missing (chat + summary panes disabled)".into()),
    });

    let model_dir = crate::stt::nemotron::model_dir();
    let model_ok = crate::stt::nemotron::model_present(&model_dir);
    checks.push(Check {
        name: "stt-model",
        ok: model_ok,
        detail: if model_ok {
            model_dir.display().to_string()
        } else {
            "missing — just run `mentor`; it offers the download on first start".into()
        },
    });

    let loopback = Command::new("parec").arg("--version").output().is_ok();
    checks.push(Check {
        name: "loopback",
        ok: loopback,
        detail: if loopback {
            let hp = crate::loopback::default_output_is_headphones();
            format!(
                "parec via @DEFAULT_MONITOR@ ({})",
                match hp {
                    Some(true) => "headphones ✓",
                    Some(false) => "⚠ speakers — Me/Them labels degrade without headphones",
                    None => "output port unknown",
                }
            )
        } else {
            "parec missing (pulseaudio-utils) — mic-only mode".into()
        },
    });

    let all_ok = checks.iter().all(|c| c.ok);
    if json {
        println!(
            "{}",
            serde_json::json!({ "ok": all_ok, "checks": checks })
        );
    } else {
        for c in &checks {
            println!("{} {:10} {}", if c.ok { "✓" } else { "✗" }, c.name, c.detail);
        }
    }
    std::process::exit(if all_ok { 0 } else { 1 });
}
