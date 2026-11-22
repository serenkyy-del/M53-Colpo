use std::{env, fs, path::PathBuf, process::Command};
fn main() {
    // GPUI already embeds the Windows manifest in the standalone build.
    if env::var_os("CARGO_FEATURE_STANDALONE").is_some() {
        return;
    }
    println!("cargo:rerun-if-changed=windows.manifest");
    println!("cargo:rerun-if-changed=../../assets/branding/m53colpo.ico");
    let target = env::var("TARGET").unwrap();
    if !target.contains("windows") {
        return;
    }
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap()).join("windows.manifest");
    let icon = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap())
        .join("../../assets/branding/m53colpo.ico");
    let out = PathBuf::from(env::var("OUT_DIR").unwrap());
    let rc = out.join("application.rc");
    let icon_resource = format!(
        "1 ICON \"{}\"\n",
        icon.display().to_string().replace('\\', "\\\\")
    );
    if target.ends_with("msvc") {
        // gpui-pre's default windows-manifest feature already embeds resource #1,
        // including PerMonitorV2, Windows 10 compatibility and asInvoker privileges.
        // Asking MSVC to embed another manifest with the same ID causes CVT1100.
        // The custom manifest below is used only by the MinGW cross-build.
        fs::write(&rc, icon_resource).unwrap();
        let resource = out.join("application.res");
        assert!(
            Command::new(resource_compiler())
                .arg("/nologo")
                .arg("/fo")
                .arg(&resource)
                .arg(&rc)
                .status()
                .expect("Windows SDK resource compiler required")
                .success()
        );
        for binary in ["m53-gpui-editor", "M53ColpoLauncher"] {
            println!("cargo:rustc-link-arg-bin={binary}={}", resource.display());
        }
    } else {
        let obj = out.join("manifest.o");
        fs::write(
            &rc,
            format!(
                "{icon_resource}1 24 \"{}\"\n",
                manifest.display().to_string().replace('\\', "\\\\")
            ),
        )
        .unwrap();
        let windres = env::var("WINDRES").unwrap_or_else(|_| {
            if cfg!(windows) {
                "windres".into()
            } else {
                "x86_64-w64-mingw32-windres".into()
            }
        });
        assert!(
            Command::new(windres)
                .arg(&rc)
                .args(["-O", "coff", "-o"])
                .arg(&obj)
                .status()
                .expect("Windows resource compiler required")
                .success()
        );
        println!("cargo:rustc-link-arg-bin=m53-gpui-editor={}", obj.display());
        println!(
            "cargo:rustc-link-arg-bin=M53ColpoLauncher={}",
            obj.display()
        );
    }
}

fn resource_compiler() -> PathBuf {
    if let Some(path) = env::var_os("RC") {
        return path.into();
    }
    if let Some(root) = env::var_os("WindowsSdkVerBinPath") {
        let path = PathBuf::from(root).join("x64/rc.exe");
        if path.is_file() {
            return path;
        }
    }
    if let Some(root) = env::var_os("ProgramFiles(x86)") {
        let root = PathBuf::from(root).join("Windows Kits/10/bin");
        let mut candidates: Vec<_> = fs::read_dir(root)
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.path().join("x64/rc.exe"))
            .filter(|p| p.is_file())
            .collect();
        candidates.sort();
        if let Some(path) = candidates.pop() {
            return path;
        }
    }
    "rc.exe".into()
}
