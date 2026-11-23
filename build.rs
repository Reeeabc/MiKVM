use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=assets/app.manifest");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let manifest_path = manifest_dir.join("assets").join("app.manifest");
    if !manifest_path.exists() {
        return;
    }

    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let rc_path = out_dir.join("app.rc");
    let manifest_escaped = manifest_path.to_string_lossy().replace('\\', "/");
    let rc_content = format!("1 24 \"{}\"\n", manifest_escaped);
    if fs::write(&rc_path, rc_content).is_err() {
        return;
    }

    let target_env = env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if target_env == "gnu" {
        let obj_path = out_dir.join("app_res.o");
        let status = Command::new("windres")
            .args([
                "-i",
                rc_path.to_str().unwrap(),
                "-o",
                obj_path.to_str().unwrap(),
                "-O",
                "coff",
            ])
            .status();
        if let Ok(s) = status {
            if s.success() {
                println!("cargo:rustc-link-arg={}", obj_path.display());
            }
        }
    } else if target_env == "msvc" {
        let res_path = out_dir.join("app.res");
        let status = Command::new("rc")
            .args(["/fo", res_path.to_str().unwrap(), rc_path.to_str().unwrap()])
            .status();
        if let Ok(s) = status {
            if s.success() {
                println!("cargo:rustc-link-arg={}", res_path.display());
            }
        }
    }
}
