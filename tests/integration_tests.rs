use std::fs;
use std::process::Command;

#[test]
fn test_limon_cli_help_and_version() {
    let output = Command::new(env!("CARGO_BIN_EXE_limon"))
        .arg("--help")
        .output()
        .expect("failed to run limon --help");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("limon"));
    assert!(stdout.contains("process"));
    assert!(!stdout.contains("watch"));

    let output_v = Command::new(env!("CARGO_BIN_EXE_limon"))
        .arg("--version")
        .output()
        .expect("failed to run limon --version");
    assert!(output_v.status.success());
    let stdout_v = String::from_utf8_lossy(&output_v.stdout);
    let expected_version = format!("limon {}", env!("CARGO_PKG_VERSION"));
    assert!(stdout_v.contains(&expected_version));

    let output_watch = Command::new(env!("CARGO_BIN_EXE_limon"))
        .arg("watch")
        .output()
        .expect("failed to run limon watch");
    assert!(!output_watch.status.success(), "watch subcommand must be rejected");
}

#[test]
fn test_limon_process_without_luaurc() {
    let temp_dir = std::env::temp_dir().join(format!("limon_test_no_luaurc_{}", std::process::id()));
    let _ = fs::remove_dir_all(&temp_dir);
    fs::create_dir_all(&temp_dir).unwrap();

    // limon.toml defines aliases directly
    let limon_toml = r#"
[aliases]
pkg = "@game/ReplicatedStorage/Packages"
utils = "@game/ReplicatedStorage/Utils"
"#;
    fs::write(temp_dir.join("limon.toml"), limon_toml).unwrap();

    // No .luaurc file created anywhere!

    fs::create_dir_all(temp_dir.join("src/sub")).unwrap();
    let main_code = r#"
local Foo = require("@pkg/Foo")
local Bar = require("@utils/Bar")
local Baz = require("@game/ReplicatedStorage/Baz")
local Unknown = require("@unknown/Lib")

return { Foo = Foo, Bar = Bar, Baz = Baz, Unknown = Unknown }
"#;
    fs::write(temp_dir.join("src/main.luau"), main_code).unwrap();

    let sub_code = r#"
local Sub = require("@pkg/Sub")
return Sub
"#;
    fs::write(temp_dir.join("src/sub/sub.luau"), sub_code).unwrap();

    // Process project with --output
    let output_dir = temp_dir.join("dist");
    let status = Command::new(env!("CARGO_BIN_EXE_limon"))
        .arg("process")
        .arg(&temp_dir)
        .arg("--output")
        .arg(&output_dir)
        .status()
        .expect("failed to run limon process");
    assert!(status.success());

    // Verify dist contents - aliases in limon.toml should be expanded without needing .luaurc
    let dist_main = fs::read_to_string(output_dir.join("src/main.luau")).unwrap();
    assert!(dist_main.contains(r#"require("@game/ReplicatedStorage/Packages/Foo")"#));
    assert!(dist_main.contains(r#"require("@game/ReplicatedStorage/Utils/Bar")"#));
    assert!(dist_main.contains(r#"require("@game/ReplicatedStorage/Baz")"#)); // @game untouched
    assert!(dist_main.contains(r#"require("@unknown/Lib")"#)); // undefined alias untouched

    let dist_sub = fs::read_to_string(output_dir.join("src/sub/sub.luau")).unwrap();
    assert!(dist_sub.contains(r#"require("@game/ReplicatedStorage/Packages/Sub")"#));

    // Second run: verify file modification times do not change when files are already up-to-date
    let mtime_before = fs::metadata(output_dir.join("src/main.luau")).unwrap().modified().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(50));
    
    let status2 = Command::new(env!("CARGO_BIN_EXE_limon"))
        .arg("process")
        .arg(&temp_dir)
        .arg("--output")
        .arg(&output_dir)
        .status()
        .expect("failed second run");
    assert!(status2.success());

    let mtime_after = fs::metadata(output_dir.join("src/main.luau")).unwrap().modified().unwrap();
    assert_eq!(mtime_before, mtime_after, "mtime must NOT change if file content is already up-to-date!");

    // Cleanup
    let _ = fs::remove_dir_all(&temp_dir);
}

#[test]
fn test_limon_process_env_and_aliases_preserve_lines() {
    let temp_dir = std::env::temp_dir().join(format!("limon_test_env_lines_{}", std::process::id()));
    let _ = fs::remove_dir_all(&temp_dir);
    fs::create_dir_all(temp_dir.join("src")).unwrap();

    let limon_toml = r#"
[aliases]
pkg = "@game/ReplicatedStorage/Packages"

[env]
cast = true
dead_code_elimination = true
preserve_lines = true
"#;
    fs::write(temp_dir.join("limon.toml"), limon_toml).unwrap();

    let main_code = "\
local isDev = _G.DEV
local port = _G.PORT
local appName = _G.APP_NAME
if _G.DEV then
    local DevTool = require(\"@pkg/Dev\")
    DevTool.start()
else
    local ProdMod = require(\"@pkg/Prod\")
    ProdMod.start(port)
end
local footer = \"line_11_anchor\"
";
    fs::write(temp_dir.join("src/main.luau"), main_code).unwrap();

    let output_dir = temp_dir.join("dist");
    let status = Command::new(env!("CARGO_BIN_EXE_limon"))
        .arg("process")
        .arg(&temp_dir)
        .arg("--output")
        .arg(&output_dir)
        .env("DEV", "false")
        .env("PORT", "8080")
        .env("APP_NAME", "limon-app")
        .status()
        .expect("failed to run limon process");
    assert!(status.success());

    let out = fs::read_to_string(output_dir.join("src/main.luau")).unwrap();
    assert!(out.contains("local isDev = false"));
    assert!(out.contains("local port = 8080"));
    assert!(out.contains(r#"local appName = "limon-app""#));
    assert!(!out.contains("DevTool"));
    assert!(!out.contains("@pkg/Dev"));
    assert!(out.contains(r#"local ProdMod = require("@game/ReplicatedStorage/Packages/Prod")"#));
    assert!(out.contains("ProdMod.start(port)"));

    let prod_line = out
        .lines()
        .position(|l| l.contains("local ProdMod"))
        .unwrap()
        + 1;
    assert_eq!(prod_line, 8);
    let anchor_line = out
        .lines()
        .position(|l| l.contains("line_11_anchor"))
        .unwrap()
        + 1;
    assert_eq!(anchor_line, 11);

    let _ = fs::remove_dir_all(&temp_dir);
}

#[test]
fn test_limon_process_env_compact_cast_false_and_types() {
    let temp_dir = std::env::temp_dir().join(format!("limon_test_env_compact_{}", std::process::id()));
    let _ = fs::remove_dir_all(&temp_dir);
    fs::create_dir_all(temp_dir.join("src")).unwrap();

    let limon_toml = r#"
[env]
cast = false
dead_code_elimination = true
preserve_lines = false

[env.types]
DEV = "bool"
PORT = "number"
"#;
    fs::write(temp_dir.join("limon.toml"), limon_toml).unwrap();

    let main_code = "\
local raw = _G.RAW_VAL
local port = _G.PORT
if _G.DEV then
    print(\"dev\")
else
    print(\"prod\")
end
";
    fs::write(temp_dir.join("src/main.luau"), main_code).unwrap();

    let output_dir = temp_dir.join("dist");
    let status = Command::new(env!("CARGO_BIN_EXE_limon"))
        .arg("process")
        .arg(&temp_dir)
        .arg("--output")
        .arg(&output_dir)
        .env("RAW_VAL", "true")
        .env("DEV", "true")
        .env("PORT", "9000")
        .status()
        .expect("failed to run limon process");
    assert!(status.success());

    let out = fs::read_to_string(output_dir.join("src/main.luau")).unwrap();
    assert_eq!(
        out,
        "local raw = \"true\"\nlocal port = 9000\n    print(\"dev\")\n"
    );

    let _ = fs::remove_dir_all(&temp_dir);
}

#[test]
fn test_limon_process_env_dce_disabled() {
    let temp_dir = std::env::temp_dir().join(format!("limon_test_env_nodce_{}", std::process::id()));
    let _ = fs::remove_dir_all(&temp_dir);
    fs::create_dir_all(temp_dir.join("src")).unwrap();

    let limon_toml = r#"
[env]
cast = true
dead_code_elimination = false
"#;
    fs::write(temp_dir.join("limon.toml"), limon_toml).unwrap();

    let main_code = "\
if _G.DEV then
    print(\"dev\")
else
    print(\"prod\")
end
";
    fs::write(temp_dir.join("src/main.luau"), main_code).unwrap();

    let output_dir = temp_dir.join("dist");
    let status = Command::new(env!("CARGO_BIN_EXE_limon"))
        .arg("process")
        .arg(&temp_dir)
        .arg("--output")
        .arg(&output_dir)
        .env("DEV", "false")
        .status()
        .expect("failed to run limon process");
    assert!(status.success());

    let out = fs::read_to_string(output_dir.join("src/main.luau")).unwrap();
    assert!(out.contains("if false then"));
    assert!(out.contains("print(\"dev\")"));
    assert!(out.contains("print(\"prod\")"));

    let _ = fs::remove_dir_all(&temp_dir);
}

#[test]
fn test_limon_process_env_invalid_type_fails() {
    let temp_dir = std::env::temp_dir().join(format!("limon_test_env_badcast_{}", std::process::id()));
    let _ = fs::remove_dir_all(&temp_dir);
    fs::create_dir_all(temp_dir.join("src")).unwrap();

    let limon_toml = r#"
[env]
[env.types]
PORT = "number"
"#;
    fs::write(temp_dir.join("limon.toml"), limon_toml).unwrap();
    fs::write(temp_dir.join("src/main.luau"), "local p = _G.PORT\n").unwrap();

    let output_dir = temp_dir.join("dist");
    let output = Command::new(env!("CARGO_BIN_EXE_limon"))
        .arg("process")
        .arg(&temp_dir)
        .arg("--output")
        .arg(&output_dir)
        .env("PORT", "abc")
        .output()
        .expect("failed to run limon process");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("PORT"));

    let _ = fs::remove_dir_all(&temp_dir);
}

#[test]
fn test_limon_process_clean_removes_orphans_and_preserves_mtimes() {
    let temp_dir = std::env::temp_dir().join(format!("limon_test_clean_{}", std::process::id()));
    let _ = fs::remove_dir_all(&temp_dir);
    fs::create_dir_all(temp_dir.join("src/sub")).unwrap();

    fs::write(
        temp_dir.join("limon.toml"),
        "[aliases]\npkg = \"@game/Packages\"\n",
    )
    .unwrap();
    fs::write(
        temp_dir.join("src/keep.luau"),
        "local P = require(\"@pkg/P\")\nreturn P\n",
    )
    .unwrap();
    fs::write(
        temp_dir.join("src/sub/stale.luau"),
        "return true\n",
    )
    .unwrap();

    let src_dir = temp_dir.join("src");
    let output_dir = temp_dir.join("dist");
    let status = Command::new(env!("CARGO_BIN_EXE_limon"))
        .arg("process")
        .arg(&src_dir)
        .arg("--config")
        .arg(temp_dir.join("limon.toml"))
        .arg("--output")
        .arg(&output_dir)
        .arg("--clean")
        .status()
        .expect("failed first clean run");
    assert!(status.success());
    assert!(output_dir.join("keep.luau").exists());
    assert!(output_dir.join("sub/stale.luau").exists());

    let mtime_before = fs::metadata(output_dir.join("keep.luau"))
        .unwrap()
        .modified()
        .unwrap();

    // Remove source file + empty subdir, wait briefly, then re-run with --clean
    fs::remove_file(src_dir.join("sub/stale.luau")).unwrap();
    fs::remove_dir(src_dir.join("sub")).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(50));

    let output2 = Command::new(env!("CARGO_BIN_EXE_limon"))
        .arg("process")
        .arg(&src_dir)
        .arg("--config")
        .arg(temp_dir.join("limon.toml"))
        .arg("--output")
        .arg(&output_dir)
        .arg("--clean")
        .output()
        .expect("failed second clean run");
    assert!(output2.status.success());
    let stdout2 = String::from_utf8_lossy(&output2.stdout);
    assert!(stdout2.contains("Removed:"));

    assert!(!output_dir.join("sub/stale.luau").exists());
    assert!(!output_dir.join("sub").exists());
    assert!(output_dir.join("keep.luau").exists());

    let mtime_after = fs::metadata(output_dir.join("keep.luau"))
        .unwrap()
        .modified()
        .unwrap();
    assert_eq!(
        mtime_before, mtime_after,
        "keep.luau mtime must not change during clean"
    );

    let _ = fs::remove_dir_all(&temp_dir);
}

#[test]
fn test_limon_process_clean_refuses_ancestor_output() {
    let temp_dir = std::env::temp_dir().join(format!("limon_test_clean_safe_{}", std::process::id()));
    let _ = fs::remove_dir_all(&temp_dir);
    fs::create_dir_all(temp_dir.join("src")).unwrap();

    fs::write(temp_dir.join("limon.toml"), "").unwrap();
    fs::write(temp_dir.join("src/main.luau"), "return 1\n").unwrap();
    fs::write(temp_dir.join("keep_me.txt"), "do not delete\n").unwrap();

    // Output is parent (ancestor) of src
    let output_ancestor = Command::new(env!("CARGO_BIN_EXE_limon"))
        .arg("process")
        .arg(temp_dir.join("src"))
        .arg("--config")
        .arg(temp_dir.join("limon.toml"))
        .arg("--output")
        .arg(&temp_dir)
        .arg("--clean")
        .output()
        .expect("failed ancestor clean test");
    assert!(!output_ancestor.status.success());
    let stderr = String::from_utf8_lossy(&output_ancestor.stderr);
    assert!(stderr.contains("Refusing to clean"));
    assert!(temp_dir.join("keep_me.txt").exists());
    assert!(temp_dir.join("src/main.luau").exists());

    // Output is same directory as src
    let output_same = Command::new(env!("CARGO_BIN_EXE_limon"))
        .arg("process")
        .arg(temp_dir.join("src"))
        .arg("--config")
        .arg(temp_dir.join("limon.toml"))
        .arg("--output")
        .arg(temp_dir.join("src"))
        .arg("--clean")
        .output()
        .expect("failed same-dir clean test");
    assert!(!output_same.status.success());
    assert!(temp_dir.join("src/main.luau").exists());

    let _ = fs::remove_dir_all(&temp_dir);
}

#[test]
fn test_limon_process_required_env_fails_when_missing() {
    let temp_dir = std::env::temp_dir().join(format!("limon_test_required_{}", std::process::id()));
    let _ = fs::remove_dir_all(&temp_dir);
    fs::create_dir_all(temp_dir.join("src")).unwrap();

    fs::write(
        temp_dir.join("limon.toml"),
        "[env]\nrequired = [\"API_URL\", \"SECRET\"]\n",
    )
    .unwrap();
    fs::write(
        temp_dir.join("src/main.luau"),
        "local url = _G.API_URL\nlocal s = _G.SECRET\n",
    )
    .unwrap();

    let output_dir = temp_dir.join("dist");
    let fail_out = Command::new(env!("CARGO_BIN_EXE_limon"))
        .arg("process")
        .arg(&temp_dir)
        .arg("--output")
        .arg(&output_dir)
        .env_remove("API_URL")
        .env_remove("SECRET")
        .output()
        .expect("failed running required env test");
    assert!(!fail_out.status.success());
    let stderr = String::from_utf8_lossy(&fail_out.stderr);
    assert!(stderr.contains("API_URL"));
    assert!(stderr.contains("SECRET"));

    let ok_out = Command::new(env!("CARGO_BIN_EXE_limon"))
        .arg("process")
        .arg(&temp_dir)
        .arg("--output")
        .arg(&output_dir)
        .env("API_URL", "https://api.example.com")
        .env("SECRET", "s3cr3t")
        .status()
        .expect("failed running required env success test");
    assert!(ok_out.success());

    let out = fs::read_to_string(output_dir.join("src/main.luau")).unwrap();
    assert!(out.contains(r#"local url = "https://api.example.com""#));
    assert!(out.contains(r#"local s = "s3cr3t""#));

    let _ = fs::remove_dir_all(&temp_dir);
}

#[test]
fn test_limon_process_strict_aliases() {
    let temp_dir = std::env::temp_dir().join(format!("limon_test_strict_{}", std::process::id()));
    let _ = fs::remove_dir_all(&temp_dir);
    fs::create_dir_all(temp_dir.join("src")).unwrap();

    fs::write(
        temp_dir.join("limon.toml"),
        "[aliases]\npkg = \"@game/ReplicatedStorage/Packages\"\n\n[env]\n",
    )
    .unwrap();
    fs::write(
        temp_dir.join("src/bad.luau"),
        "local A = require(\"@pkg/Ok\")\nlocal B = require(\"@typo/Foo\")\n",
    )
    .unwrap();

    let output_dir = temp_dir.join("dist");
    let fail_cli = Command::new(env!("CARGO_BIN_EXE_limon"))
        .arg("process")
        .arg(&temp_dir)
        .arg("--output")
        .arg(&output_dir)
        .arg("--strict")
        .output()
        .expect("failed running strict cli test");
    assert!(!fail_cli.status.success());
    let stderr = String::from_utf8_lossy(&fail_cli.stderr);
    assert!(stderr.contains("@typo"));
    assert!(stderr.contains("line 2"));
    assert!(!output_dir.join("src/bad.luau").exists());

    // Now test strict = true in limon.toml with @game, @self, and dead-branch require
    fs::write(
        temp_dir.join("limon.toml"),
        "strict = true\n\n[aliases]\npkg = \"@game/ReplicatedStorage/Packages\"\n\n[env]\n",
    )
    .unwrap();
    fs::write(
        temp_dir.join("src/bad.luau"),
        "\
local A = require(\"@pkg/Ok\")
local B = require(\"@game/ReplicatedStorage/Shared\")
local C = require(\"@self/Helper\")
if _G.DEV then
    local D = require(\"@dev_only/Inspector\")
end
",
    )
    .unwrap();

    let ok_toml = Command::new(env!("CARGO_BIN_EXE_limon"))
        .arg("process")
        .arg(&temp_dir)
        .arg("--output")
        .arg(&output_dir)
        .env("DEV", "false")
        .status()
        .expect("failed running strict toml test");
    assert!(ok_toml.success());
    let out = fs::read_to_string(output_dir.join("src/bad.luau")).unwrap();
    assert!(out.contains(r#"require("@game/ReplicatedStorage/Packages/Ok")"#));
    assert!(out.contains(r#"require("@self/Helper")"#));
    assert!(!out.contains("dev_only"));

    let _ = fs::remove_dir_all(&temp_dir);
}

#[test]
fn test_limon_process_parallel_many_files() {
    let temp_dir = std::env::temp_dir().join(format!("limon_test_parallel_{}", std::process::id()));
    let _ = fs::remove_dir_all(&temp_dir);
    fs::create_dir_all(temp_dir.join("src/nested/deep")).unwrap();

    fs::write(
        temp_dir.join("limon.toml"),
        "[aliases]\npkg = \"@game/Packages\"\n\n[env]\n",
    )
    .unwrap();

    for i in 0..24 {
        let dir = match i % 3 {
            0 => temp_dir.join("src"),
            1 => temp_dir.join("src/nested"),
            _ => temp_dir.join("src/nested/deep"),
        };
        let content = format!(
            "local Mod = require(\"@pkg/Mod{0}\")\nlocal flag = _G.DEV\nreturn {{ Mod = Mod, id = {0}, flag = flag }}\n",
            i
        );
        fs::write(dir.join(format!("file_{}.luau", i)), content).unwrap();
    }

    let output_dir = temp_dir.join("dist");
    let status = Command::new(env!("CARGO_BIN_EXE_limon"))
        .arg("process")
        .arg(temp_dir.join("src"))
        .arg("--config")
        .arg(temp_dir.join("limon.toml"))
        .arg("--output")
        .arg(&output_dir)
        .env("DEV", "true")
        .status()
        .expect("failed parallel run");
    assert!(status.success());

    for i in 0..24 {
        let rel = match i % 3 {
            0 => format!("file_{}.luau", i),
            1 => format!("nested/file_{}.luau", i),
            _ => format!("nested/deep/file_{}.luau", i),
        };
        let out = fs::read_to_string(output_dir.join(&rel)).unwrap();
        assert!(out.contains(&format!(r#"require("@game/Packages/Mod{}")"#, i)));
        assert!(out.contains("local flag = true"));
    }

    let _ = fs::remove_dir_all(&temp_dir);
}
