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
    assert!(stdout.contains("watch"));

    let output_v = Command::new(env!("CARGO_BIN_EXE_limon"))
        .arg("--version")
        .output()
        .expect("failed to run limon --version");
    assert!(output_v.status.success());
    let stdout_v = String::from_utf8_lossy(&output_v.stdout);
    assert!(stdout_v.contains("limon 0.1.0"));
}

#[test]
fn test_limon_process_end_to_end() {
    let temp_dir = std::env::temp_dir().join(format!("limon_test_{}", std::process::id()));
    let _ = fs::remove_dir_all(&temp_dir);
    fs::create_dir_all(&temp_dir).unwrap();

    // Setup project structure:
    // temp_dir/
    //   limon.toml
    //   .luaurc (root: pkg, utils)
    //   src/
    //     main.luau (uses @pkg/Foo, @utils/Bar, @game/ReplicatedStorage/Baz)
    //     sub/
    //       sub.luau (uses @pkg/Sub)
    //   no_luaurc/
    //     orphan.luau (uses @pkg/Orphan - should NOT be touched because no .luaurc)
    //   assets/
    //     data.json

    let limon_toml = r#"
[aliases]
pkg = "@game/ReplicatedStorage/Packages"
utils = "@game/ReplicatedStorage/Utils"
"#;
    fs::write(temp_dir.join("limon.toml"), limon_toml).unwrap();

    let root_luaurc = r#"
{
    // Comments in luaurc
    "aliases": {
        "pkg": "src/Packages",
        "utils": "src/Utils"
    }
}
"#;
    fs::write(temp_dir.join(".luaurc"), root_luaurc).unwrap();

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

    // An isolated folder outside the scope of any .luaurc
    let isolated_dir = std::env::temp_dir().join(format!("limon_isolated_{}", std::process::id()));
    let _ = fs::remove_dir_all(&isolated_dir);
    fs::create_dir_all(&isolated_dir).unwrap();

    let orphan_code = r#"
local Orphan = require("@pkg/Orphan")
return Orphan
"#;
    fs::write(isolated_dir.join("orphan.luau"), orphan_code).unwrap();

    // 1. Process project with --output
    let output_dir = temp_dir.join("dist");
    let status = Command::new(env!("CARGO_BIN_EXE_limon"))
        .arg("process")
        .arg(&temp_dir)
        .arg("--output")
        .arg(&output_dir)
        .status()
        .expect("failed to run limon process");
    assert!(status.success());

    // Verify dist contents
    let dist_main = fs::read_to_string(output_dir.join("src/main.luau")).unwrap();
    assert!(dist_main.contains(r#"require("@game/ReplicatedStorage/Packages/Foo")"#));
    assert!(dist_main.contains(r#"require("@game/ReplicatedStorage/Utils/Bar")"#));
    assert!(dist_main.contains(r#"require("@game/ReplicatedStorage/Baz")"#));
    assert!(dist_main.contains(r#"require("@unknown/Lib")"#));

    let dist_sub = fs::read_to_string(output_dir.join("src/sub/sub.luau")).unwrap();
    assert!(dist_sub.contains(r#"require("@game/ReplicatedStorage/Packages/Sub")"#));

    // 2. Process isolated file without .luaurc -> verify it is untouched
    let isolated_out = isolated_dir.join("out");
    let status2 = Command::new(env!("CARGO_BIN_EXE_limon"))
        .arg("process")
        .arg(&isolated_dir)
        .arg("--output")
        .arg(&isolated_out)
        .status()
        .expect("failed to run limon on isolated dir");
    assert!(status2.success());

    let out_orphan = fs::read_to_string(isolated_out.join("orphan.luau")).unwrap();
    // Must NOT be touched!
    assert!(out_orphan.contains(r#"require("@pkg/Orphan")"#));

    // Cleanup
    let _ = fs::remove_dir_all(&temp_dir);
    let _ = fs::remove_dir_all(&isolated_dir);
}
