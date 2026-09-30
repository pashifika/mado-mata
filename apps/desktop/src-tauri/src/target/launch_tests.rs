use super::*;
use crate::target::TargetLocation;
use crate::target::launch::LaunchRecipe;
use crate::target::tests::{MetadataFixture, configuration, declaration};
use mado_runtime_comparison::model::Limits;

fn control() -> Control {
    Control::new(&Limits {
        duration_ms: 30_000,
        readiness_ms: 1_000,
        wait_ms: 1_000,
        cleanup_ms: 1_000,
        containment_ms: 2_000,
        queue_capacity: 1,
        handles: 1,
        log_records: 1,
        log_bytes: 1_024,
        vm_bytes: 1_024,
        max_actions: 1,
        snapshot_files: 1,
        snapshot_bytes: 1_024,
    })
}

fn bundle_configuration(fixture: &MetadataFixture) -> TargetConfiguration {
    let mut selected = configuration(fixture.bundle(false).to_str().unwrap());
    selected.game.kind = "bundle".into();
    selected
}

fn add_direct_launcher(fixture: &MetadataFixture, selected: &mut TargetConfiguration) -> PathBuf {
    let launcher = fixture.executable("launcher-bin/Launcher");
    selected.launcher = Some(TargetLocation {
        kind: "executable".into(),
        path: launcher.to_str().unwrap().into(),
    });
    launcher
}

#[test]
fn bundle_recipient_is_outer_bundle_and_explicit_directory_is_refused() {
    let fixture = MetadataFixture::new();
    let game = bundle_configuration(&fixture);
    let launcher = fixture.bundle(true);
    for separate in [false, true] {
        let mut selected = game.clone();
        if separate {
            selected.launcher = Some(TargetLocation {
                kind: "bundle".into(),
                path: launcher.to_str().unwrap().into(),
            });
        }
        let resolution = selected.resolve(&declaration()).unwrap();
        let recipe = LaunchRecipe::capture(&selected, &declaration(), &resolution).unwrap();
        let expected = if separate {
            resolution.launcher.as_ref().unwrap()
        } else {
            &resolution.game
        };
        assert_eq!(
            recipe.native.recipient,
            Recipient::Bundle(PathBuf::from(&expected.path))
        );
        assert_ne!(Path::new(&expected.path), Path::new(&expected.executable));

        selected.working_directory = Some(fixture.0.to_str().unwrap().into());
        let resolution = selected.resolve(&declaration()).unwrap();
        let fault = LaunchRecipe::capture(&selected, &declaration(), &resolution).unwrap_err();
        assert_eq!(fault.category, "TargetConfiguration");
        assert_eq!(fault.context["field"], "working_directory");
    }
}

#[test]
fn direct_launcher_owns_recipient_and_explicit_or_parent_directory() {
    let fixture = MetadataFixture::new();
    let mut selected = bundle_configuration(&fixture);
    let launcher = add_direct_launcher(&fixture, &mut selected);
    let directory = fixture.0.join("explicit-directory");
    fs::create_dir(&directory).unwrap();
    for explicit in [false, true] {
        selected.working_directory = explicit.then(|| directory.to_str().unwrap().into());
        let resolution = selected.resolve(&declaration()).unwrap();
        let recipe = LaunchRecipe::capture(&selected, &declaration(), &resolution).unwrap();
        let executable = fs::canonicalize(&launcher).unwrap();
        let expected_directory = if explicit {
            fs::canonicalize(&directory).unwrap()
        } else {
            executable.parent().unwrap().to_owned()
        };
        assert_eq!(
            recipe.native.recipient,
            Recipient::Executable {
                path: executable,
                directory: expected_directory,
            }
        );
        assert_eq!(recipe.native.resolution.game, resolution.game);
        recipe.native.revalidate(&control()).unwrap();
    }
}

#[test]
fn recipe_revalidation_refuses_replaced_game_launcher_or_directory() {
    for changed_field in ["game", "launcher", "working_directory"] {
        let fixture = MetadataFixture::new();
        let mut selected = bundle_configuration(&fixture);
        let launcher = add_direct_launcher(&fixture, &mut selected);
        let directory = fixture.0.join("cwd");
        fs::create_dir(&directory).unwrap();
        selected.working_directory = Some(directory.to_str().unwrap().into());
        let resolution = selected.resolve(&declaration()).unwrap();
        let recipe = LaunchRecipe::capture(&selected, &declaration(), &resolution).unwrap();
        recipe.native.revalidate(&control()).unwrap();
        match changed_field {
            "game" => fs::write(&resolution.game.executable, b"replacement game metadata").unwrap(),
            "launcher" => fs::write(&launcher, b"replacement launcher metadata").unwrap(),
            "working_directory" => {
                fs::rename(&directory, fixture.0.join("retained-old-cwd")).unwrap();
                fs::create_dir(&directory).unwrap();
            }
            _ => unreachable!(),
        }
        let fault = recipe.prepare(&control()).err().unwrap();
        assert_eq!(
            fault.category, "NativeLaunchRecipeChanged",
            "{changed_field}"
        );
    }
}

#[test]
fn stale_saved_launcher_resolution_refuses_even_when_game_still_matches() {
    let fixture = MetadataFixture::new();
    let mut selected = bundle_configuration(&fixture);
    add_direct_launcher(&fixture, &mut selected);
    let resolution = selected.resolve(&declaration()).unwrap();
    selected.launcher.as_mut().unwrap().path = fixture
        .executable("other-launcher")
        .to_str()
        .unwrap()
        .into();
    let fault = LaunchRecipe::capture(&selected, &declaration(), &resolution).unwrap_err();
    assert_eq!(fault.category, "NativeLaunchRecipeChanged");
}

#[test]
fn cancelled_prelaunch_revalidation_preserves_cancellation_without_os_submission() {
    let fixture = MetadataFixture::new();
    let selected = bundle_configuration(&fixture);
    let resolution = selected.resolve(&declaration()).unwrap();
    let recipe = LaunchRecipe::capture(&selected, &declaration(), &resolution).unwrap();
    let control = control();
    control.cancel();
    assert_eq!(
        recipe.prepare(&control).err().unwrap().category,
        "Cancelled"
    );
}
