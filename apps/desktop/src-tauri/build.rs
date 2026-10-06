fn main() {
    #[cfg(feature = "desktop")]
    {
        let mut attributes = tauri_build::Attributes::new();
        if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
            let icon = windows_icon();
            attributes = attributes
                .windows_attributes(tauri_build::WindowsAttributes::new().window_icon_path(icon));
        }
        tauri_build::try_build(attributes).expect("desktop build resources");
    }
}

#[cfg(feature = "desktop")]
fn windows_icon() -> std::path::PathBuf {
    use std::io::Write;

    // ICO accepts a PNG payload unchanged; keep one source image for both shells.
    let png = include_bytes!("icons/icon.png");
    let width = u32::from_be_bytes(png[16..20].try_into().expect("PNG width"));
    let height = u32::from_be_bytes(png[20..24].try_into().expect("PNG height"));
    assert!(
        (1..=256).contains(&width) && (1..=256).contains(&height),
        "icon dimensions"
    );
    let mut header = [
        0_u8, 0, 1, 0, 1, 0, 0, 0, 0, 0, 1, 0, 32, 0, 0, 0, 0, 0, 22, 0, 0, 0,
    ];
    header[6] = if width == 256 { 0 } else { width as u8 };
    header[7] = if height == 256 { 0 } else { height as u8 };
    header[14..18].copy_from_slice(&u32::try_from(png.len()).expect("icon size").to_le_bytes());
    let icon = std::path::PathBuf::from(std::env::var_os("OUT_DIR").expect("Cargo OUT_DIR"))
        .join("mado-mata.ico");
    let mut file = std::fs::File::create(&icon).expect("create Windows icon");
    file.write_all(&header).expect("write ICO directory");
    file.write_all(png).expect("write ICO image");
    println!("cargo:rerun-if-changed=icons/icon.png");
    icon
}
