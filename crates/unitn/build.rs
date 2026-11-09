fn main() {
    slint_build::compile("ui/app.slint").expect("failed to compile the Slint UI");
    windows_icon();
}

/// Puts the app icon into unitn.exe, so Explorer, the taskbar and the
/// installer's shortcuts (Start menu, desktop) show it. Uses the resource
/// compiler (rc.exe) of the Windows SDK; no extra crates needed.
fn windows_icon() {
    use std::path::PathBuf;
    println!("cargo:rerun-if-changed=icons/icon.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows")
        || std::env::var("CARGO_CFG_TARGET_ENV").as_deref() != Ok("msvc")
    {
        return;
    }
    let Some(rc) = find_rc() else {
        println!("cargo:warning=rc.exe not found: unitn.exe gets no icon");
        return;
    };
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let ico = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("icons").join("icon.ico");
    let script = out.join("unitn.rc");
    let res = out.join("unitn.res");
    let ico = ico.to_string_lossy().replace('\\', "\\\\");
    std::fs::write(&script, format!("1 ICON \"{ico}\"\n")).unwrap();
    let status = std::process::Command::new(&rc)
        .arg("/nologo")
        .arg("/fo")
        .arg(&res)
        .arg(&script)
        .status();
    match status {
        Ok(s) if s.success() => {
            println!("cargo:rustc-link-arg-bins={}", res.display());
        }
        other => println!("cargo:warning=rc.exe failed ({other:?}): unitn.exe gets no icon"),
    }
}

/// rc.exe from PATH, or the newest one in the Windows 10/11 SDK.
fn find_rc() -> Option<std::path::PathBuf> {
    use std::path::PathBuf;
    if let Ok(path) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path) {
            let rc = dir.join("rc.exe");
            if rc.is_file() {
                return Some(rc);
            }
        }
    }
    let arch = match std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() {
        Ok("aarch64") => "arm64",
        Ok("x86") => "x86",
        _ => "x64",
    };
    let roots = [
        std::env::var("WindowsSdkDir").ok().map(PathBuf::from),
        Some(PathBuf::from(r"C:\Program Files (x86)\Windows Kits\10")),
        Some(PathBuf::from(r"C:\Program Files\Windows Kits\10")),
    ];
    for root in roots.into_iter().flatten() {
        let Ok(entries) = std::fs::read_dir(root.join("bin")) else { continue };
        let mut versions: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
        versions.sort();
        for version in versions.iter().rev() {
            // Use the host's rc.exe (x64 on the build machines); fall back to the target arch.
            for host in ["x64", arch, "x86"] {
                let rc = version.join(host).join("rc.exe");
                if rc.is_file() {
                    return Some(rc);
                }
            }
        }
    }
    None
}
