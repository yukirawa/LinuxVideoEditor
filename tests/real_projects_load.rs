//! Read-only guard for real on-disk projects: loading them must never break.
//!
//! The paths are user data and may be absent on another machine, so each case skips when the
//! file is missing — but when it is there, a load failure is a hard error.

use std::path::Path;

fn load_or_skip(path: &str) {
    let path = Path::new(path);
    if !path.exists() {
        eprintln!("skipping missing project file: {}", path.display());
        return;
    }
    yuki_video_editor::project_store::load(path)
        .unwrap_or_else(|error| panic!("failed to load {}: {error}", path.display()));
}

#[test]
fn real_user_projects_still_load() {
    load_or_skip("/home/yukirawa/プロジェクト/ゲームのゆきらわ/outoshefu.yukivproj");
    load_or_skip("/home/yukirawa/プロジェクト/ゲームのゆきらわ/AI_Test.yukivproj");
    load_or_skip(
        "/home/yukirawa/プロジェクト/ゆきらわ/北&東パス/函館本線全線乗車編/無題のプロジェクト.yukivproj",
    );
}
