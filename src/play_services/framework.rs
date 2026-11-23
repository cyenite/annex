//! The framework half of the stand-in: the image's own services.jar with
//! `android/framework/AnnexSignatures.smali` added and called from every return of
//! `ComputerEngine.generatePackageInfo`, so client libraries see Google's certificate on the
//! stand-in's packages. Built on first use and cached per image.

use std::fs;
use std::path::{Path, PathBuf};
use std::process;

use anyhow::{Context, Result, bail};

use super::{APKTOOL, keep, root, run_java, smali};
use crate::docker;
use crate::instance::AndroidVersion;

pub const TARGET: &str = "/system/framework/services.jar";
pub const PRECOMPILED: [&str; 3] = [
    "/system/framework/oat/x86_64/services.odex",
    "/system/framework/oat/x86_64/services.vdex",
    "/system/framework/oat/x86_64/services.art",
];
pub const HELPER: &str = include_str!("../../android/framework/AnnexSignatures.smali");
// Bump when the helper or the hook changes, so instances get a freshly patched framework.
const PATCH_VERSION: &str = "2";
const ENGINE: &str = "com/android/server/pm/ComputerEngine.smali";
const HELPER_CLASS: &str = "com/android/server/pm/AnnexSignatures.smali";
const HOOKED_METHOD: &str = " generatePackageInfo(Lcom/android/server/pm/pkg/PackageStateInternal;";
const PACKAGE_INFO: &str = ")Landroid/content/pm/PackageInfo;";
const APPLY: &str = "Lcom/android/server/pm/AnnexSignatures;->apply(Landroid/content/pm/PackageInfo;Ljava/lang/Object;)V";

/// The patched services.jar for the image `android` currently names. The cache is keyed by image
/// id, not tag: a jar patched from another build would crash-loop system_server.
pub fn patched(android: AndroidVersion) -> Result<PathBuf> {
    let image = android.image();
    let id = docker::run(&["image", "inspect", "--format", "{{.Id}}", image])?;
    let id = id.strip_prefix("sha256:").unwrap_or(&id);
    let cached = root()
        .join("framework")
        .join(PATCH_VERSION)
        .join(id)
        .join("services.jar");
    if cached.is_file() {
        return Ok(cached);
    }
    println!(
        "preparing Android {}'s framework for the Google services stand-in (once per image, about a minute)",
        android.label()
    );
    let work = root().join(format!(".work-framework-{}", process::id()));
    fs::create_dir_all(&work)?;
    let built = build(image, &work, &cached);
    let _ = fs::remove_dir_all(&work);
    built.with_context(|| format!("could not patch Android {}'s framework", android.label()))?;
    Ok(cached)
}

fn build(image: &str, work: &Path, cached: &Path) -> Result<()> {
    let stock = work.join("services.jar");
    let extractor = format!("annex-framework-{}", process::id());
    docker::run(&["create", "--name", &extractor, image])?;
    let source = format!("{extractor}:{TARGET}");
    let copied = docker::run(&["cp", &source, &stock.to_string_lossy()]);
    docker::run(&["rm", "--force", &extractor])?;
    copied?;
    let apktool = APKTOOL.inside();
    let volumes = [(work, "/work")];
    run_java(
        &[&APKTOOL],
        &volumes,
        &[
            "java",
            "-jar",
            &apktool,
            "d",
            "-f",
            "-o",
            "/work/services",
            "/work/services.jar",
        ],
    )?;
    patch(&work.join("services"))?;
    run_java(
        &[&APKTOOL],
        &volumes,
        &[
            "java",
            "-jar",
            &apktool,
            "b",
            "-o",
            "/work/patched.jar",
            "/work/services",
        ],
    )?;
    let patched = work.join("patched.jar");
    if fs::metadata(&patched)?.len() < fs::metadata(&stock)?.len() / 2 {
        bail!("the rebuilt framework is far smaller than the original");
    }
    keep(&patched, cached)
}

fn patch(decoded: &Path) -> Result<()> {
    let folders = smali::folders(decoded)?;
    let engine = smali::class(&folders, ENGINE)?;
    fs::write(&engine, hook(&fs::read_to_string(&engine)?)?)?;
    smali::add_class(&folders, HELPER_CLASS, HELPER)
}

/// Passes every PackageInfo that generatePackageInfo returns, with its PackageStateInternal, to
/// AnnexSignatures.apply. The parameter is saved in a new register at entry, because compiled
/// code may reuse parameter registers, and every return goes through the call, so the hook needs
/// no knowledge of a particular build's labels or register numbers.
fn hook(source: &str) -> Result<String> {
    let lines: Vec<&str> = source.lines().collect();
    let method = smali::method(
        &lines,
        "generatePackageInfo(PackageStateInternal, ...)",
        |line| {
            line.contains(HOOKED_METHOD)
                && line.ends_with(PACKAGE_INFO)
                && !line.contains(" static ")
        },
    )?;
    if method.locals < 2 {
        bail!("generatePackageInfo has fewer than two registers to call through");
    }
    let entry = smali::first_instruction(&lines, &method)?;
    let saved = format!("v{}", method.locals);
    let mut hooked = Vec::with_capacity(lines.len() + 16);
    let mut returns = 0;
    for (i, line) in lines.iter().enumerate() {
        if i == method.locals_line {
            hooked.push(format!("    .locals {}", method.locals + 1));
            continue;
        }
        if i == entry {
            hooked.push(format!("    move-object/from16 {saved}, p1"));
            hooked.push(String::new());
        }
        let returned = line.trim().strip_prefix("return-object ");
        if let (true, Some(register)) = (method.start < i && i < method.end, returned) {
            hooked.push(format!("    move-object/from16 v0, {register}"));
            hooked.push(String::new());
            hooked.push(format!("    move-object/from16 v1, {saved}"));
            hooked.push(String::new());
            hooked.push(format!("    invoke-static {{v0, v1}}, {APPLY}"));
            hooked.push(String::new());
            hooked.push("    return-object v0".to_string());
            returns += 1;
        } else {
            hooked.push(line.to_string());
        }
    }
    if returns == 0 {
        bail!("generatePackageInfo has no return-object");
    }
    Ok(smali::finish(hooked, source))
}

#[cfg(test)]
mod tests {
    use super::*;

    const COMPUTER_ENGINE: &str = "\
.class public Lcom/android/server/pm/ComputerEngine;

.method public final generatePackageInfo(Lcom/android/server/pm/pkg/PackageStateInternal;JI)Landroid/content/pm/PackageInfo;
    .locals 17

    move-object/from16 v13, p1

    const/4 v14, 0x0

    if-nez v13, :cond_1

    return-object v14

    :cond_1
    invoke-static {v13}, Lcom/example/Build;->info(Ljava/lang/Object;)Landroid/content/pm/PackageInfo;

    move-result-object v2

    return-object v2
.end method

.method public final getPackageInfo(Ljava/lang/String;JI)Landroid/content/pm/PackageInfo;
    .locals 1

    const/4 v0, 0x0

    return-object v0
.end method
";

    #[test]
    fn hooks_every_return_of_generate_package_info_only() {
        let hooked = hook(COMPUTER_ENGINE).unwrap();
        assert!(hooked.contains("    .locals 18\n\n    move-object/from16 v17, p1\n"));
        assert_eq!(hooked.matches(APPLY).count(), 2);
        assert!(hooked.contains(
            "    :cond_1\n    invoke-static {v13}, Lcom/example/Build;->info(Ljava/lang/Object;)Landroid/content/pm/PackageInfo;\n\n    move-result-object v2\n\n    move-object/from16 v0, v2\n\n    move-object/from16 v1, v17\n"
        ));
        assert!(hooked.contains("    move-object/from16 v0, v14\n"));
        assert!(!hooked.contains("return-object v14"));
        assert!(!hooked.contains("return-object v2"));
        let untouched =
            "    .locals 1\n\n    const/4 v0, 0x0\n\n    return-object v0\n.end method\n";
        assert!(hooked.ends_with(untouched));
    }

    #[test]
    fn refuses_a_framework_it_does_not_recognise() {
        let renamed = COMPUTER_ENGINE.replace("generatePackageInfo", "buildPackageInfo");
        assert!(hook(&renamed).is_err());
        let first = COMPUTER_ENGINE.find(".method").unwrap();
        let twice = format!("{COMPUTER_ENGINE}{}", &COMPUTER_ENGINE[first..]);
        assert!(hook(&twice).is_err());
        let registers = COMPUTER_ENGINE.replace(".locals 17", ".registers 20");
        assert!(hook(&registers).is_err());
        let narrow = COMPUTER_ENGINE.replace(".locals 17", ".locals 1");
        assert!(hook(&narrow).is_err());
    }

    #[test]
    fn the_helper_is_the_class_the_hook_calls() {
        assert!(HELPER.starts_with(".class public final Lcom/android/server/pm/AnnexSignatures;"));
        assert!(HELPER.contains(
            ".method public static apply(Landroid/content/pm/PackageInfo;Ljava/lang/Object;)V"
        ));
    }
}
