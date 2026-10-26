// Feature shaders (shader pipeline plan, slice 4): each <source>/shaders/<name>.hlsl becomes the
// package file shaders/<name>.cso. The compiler is the pinned dxc of fm_render (its hashes come
// from crates/fm_render/shaders/manifest.json, so built-in and feature shaders use one compiler),
// with shader contract v1 (fm_shader.hlsli, from this build) on the include path. Pixel shaders
// only (ps_6_0, entry main); .hlsli files are an author's own includes and are not compiled.
use sha2::Digest;
use std::path::{Path, PathBuf};

const BIN: &str = r"C:\Program Files (x86)\Windows Kits\10\bin\10.0.26100.0\x64";
const MANIFEST: &str = include_str!("../../../crates/fm_render/shaders/manifest.json");
const CONTRACT: &str = include_str!("../../../crates/fm_render/shaders/fm_shader.hlsli");

/// The compiler, after its files match the pinned hashes.
fn compiler() -> Result<PathBuf, String> {
    let m: serde_json::Value = serde_json::from_str(MANIFEST).map_err(|e| e.to_string())?;
    let hashes = m["compiler"]["hashes"].as_object().ok_or("fm_render manifest.json has no compiler hashes")?;
    for (file, want) in hashes {
        let path = Path::new(BIN).join(file);
        let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e} (install Windows SDK 10.0.26100.0)", path.display()))?;
        let got: String = sha2::Sha256::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect();
        if Some(got.as_str()) != want.as_str() { return Err(format!("{} is not the pinned compiler ({got})", path.display())); }
    }
    Ok(Path::new(BIN).join("dxc.exe"))
}

/// The compiled shaders of `dir`/shaders in name order: none when the folder does not exist.
/// A bad name or a compile error stops the pack with dxc's message.
pub fn compile(dir: &Path) -> Result<Vec<(String, Vec<u8>)>, String> {
    let src = dir.join("shaders");
    if !src.is_dir() { return Ok(Vec::new()); }
    let mut names = Vec::new();
    for entry in std::fs::read_dir(&src).map_err(|e| format!("{}: {e}", src.display()))? {
        let path = entry.map_err(|e| e.to_string())?.path();
        let file = path.file_name().and_then(|f| f.to_str()).unwrap_or_default().to_string();
        if file.ends_with(".hlsli") { continue; }
        let name = file.strip_suffix(".hlsl").filter(|n| fm_feature_sdk::shader_name_valid(n));
        names.push(name.map(String::from).ok_or_else(|| format!("{}: only <name>.hlsl (a-z, 0-9, _ or -) and .hlsli files", path.display()))?);
    }
    names.sort();
    if names.is_empty() { return Err(format!("{} holds no .hlsl shader", src.display())); }
    let dxc = compiler()?;
    let work = std::env::temp_dir().join(format!("fm-pack-shaders-{}", std::process::id()));
    std::fs::create_dir_all(&work).map_err(|e| format!("{}: {e}", work.display()))?;
    let result = (|| {
        std::fs::write(work.join("fm_shader.hlsli"), CONTRACT).map_err(|e| e.to_string())?;
        names.iter().map(|name| {
            let out = work.join(format!("{name}.cso"));
            let run = std::process::Command::new(&dxc)
                .args(["-nologo", "-T", "ps_6_0", "-E", "main", "-O3", "-Qstrip_debug", "-Qstrip_reflect", "-I"])
                .arg(&work).arg("-Fo").arg(&out).arg(src.join(format!("{name}.hlsl")))
                .output().map_err(|e| format!("{}: {e}", dxc.display()))?;
            if !run.status.success() {
                return Err(format!("shaders/{name}.hlsl does not compile:\n{}{}", String::from_utf8_lossy(&run.stdout), String::from_utf8_lossy(&run.stderr)));
            }
            let bytes = std::fs::read(&out).map_err(|e| format!("{}: {e}", out.display()))?;
            Ok((name.clone(), bytes))
        }).collect()
    })();
    let _ = std::fs::remove_dir_all(&work);
    result
}

/// Developer hot reload: compiles `dir`/shaders and writes each <name>.cso into `out`. Each file
/// goes to a temporary name first and then replaces the old one, so a host never reads half.
pub fn write(dir: &Path, out: &Path) -> Result<(), String> {
    let built = compile(dir)?;
    if built.is_empty() { return Err(format!("{} has no shaders/ folder", dir.display())); }
    for (name, bytes) in built {
        let (tmp, file) = (out.join(format!("{name}.cso.tmp")), out.join(format!("{name}.cso")));
        std::fs::write(&tmp, &bytes).map_err(|e| format!("{}: {e}", tmp.display()))?;
        std::fs::rename(&tmp, &file).map_err(|e| format!("{}: {e}", file.display()))?;
        println!("{} ({} bytes)", file.display(), bytes.len());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    const GOOD: &str = "#include \"fm_shader.hlsli\"\n#include \"common.hlsli\"\n[RootSignature(FM_RS)] float4 main(FmPixel i) : SV_Target { return half_of(fm_color.Sample(fm_point, i.uv)); }\n";

    // A bad file name stops before the compiler, so this runs on every system.
    #[test]
    fn a_bad_shader_name_stops_before_the_compiler() {
        let dir = std::env::temp_dir().join(format!("fm-pack-shader-name-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("shaders")).unwrap();
        std::fs::write(dir.join("shaders").join("Bad Name.hlsl"), GOOD).unwrap();
        assert!(super::compile(&dir).unwrap_err().contains("only <name>.hlsl"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    // A contract shader compiles to a DXBC container; a broken one stops with dxc's message. The
    // pinned compiler is the Windows SDK's dxc.exe, so this runs on Windows only.
    #[cfg(windows)]
    #[test]
    fn feature_shaders_compile_with_the_pinned_compiler() {
        let dir = std::env::temp_dir().join(format!("fm-pack-shader-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("shaders")).unwrap();
        let good = GOOD;
        std::fs::write(dir.join("shaders").join("half.hlsl"), good).unwrap();
        std::fs::write(dir.join("shaders").join("common.hlsli"), "float4 half_of(float4 c) { return c * fm_params[0].x; }\n").unwrap();
        let built = super::compile(&dir).unwrap();
        assert_eq!(built.len(), 1);
        assert_eq!((built[0].0.as_str(), &built[0].1[..4]), ("half", b"DXBC".as_slice()));
        std::fs::write(dir.join("shaders").join("broken.hlsl"), "float4 main() : SV_Target { return nothing; }\n").unwrap();
        let e = super::compile(&dir).unwrap_err();
        assert!(e.contains("shaders/broken.hlsl does not compile") && e.contains("nothing"), "{e}");
        std::fs::remove_file(dir.join("shaders").join("broken.hlsl")).unwrap();
        std::fs::write(dir.join("shaders").join("Bad Name.hlsl"), good).unwrap();
        assert!(super::compile(&dir).unwrap_err().contains("only <name>.hlsl"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
