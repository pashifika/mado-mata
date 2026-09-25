use super::super::Settings;
use super::super::fixtures::Directory;
use super::*;
use crate::configuration::publish_no_replace;

#[test]
fn missing_only_publication_refuses_destination_appearance() {
    let directory = Directory::new();
    let path = directory.0.join("settings.json");
    let bytes = encode(&Settings::default(), MAX_SETTINGS_BYTES).unwrap();
    let fault = write_atomic(&path, &bytes, |from, to| {
        fs::write(to, b"other writer")?;
        publish_no_replace(from, to)
    })
    .unwrap_err();
    assert_eq!(fault.context["kind"], "AlreadyExists");
    assert_eq!(fs::read(&path).unwrap(), b"other writer");
    assert!(!path.with_extension("pending").exists());
}
